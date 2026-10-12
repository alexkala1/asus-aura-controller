//! MCP server (`asus-aura-cli mcp`): newline-delimited JSON-RPC 2.0 over stdio.
//!
//! stdout carries protocol messages only; every diagnostic goes to stderr.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::gpu::{GpuController, GpuSnapshot, I2cBus};
use crate::mobo::{MoboController, MoboSnapshot, PortIo};
use crate::protocol::{parse_hex_color, GpuMode, MoboMode};
use crate::{gpu_controller, mobo_controller};

const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const PULSE_SECONDS: u64 = 3;
/// Safety net: an agent that dies while "working" must not leave the LEDs lit forever.
const HOLD_SECONDS: u64 = 900;
const MAX_HOLD_SECONDS: u64 = 3600;
const MAX_SPEED: u8 = 4;
const MOBO_ZONES: u8 = 5;

type Gpu = GpuController<Box<dyn I2cBus>>;
type Mobo = MoboController<Box<dyn PortIo>>;
type RpcError = (i32, String);
type Rgb = (u8, u8, u8);

// ---------------------------------------------------------------- effects

/// (accepted names, GPU equivalent, motherboard mode). The first name is canonical.
const EFFECTS: [(&[&str], Option<GpuMode>, MoboMode); 13] = [
    (&["static"], Some(GpuMode::Static), MoboMode::Static),
    (
        &["breathing"],
        Some(GpuMode::Breathing),
        MoboMode::Breathing,
    ),
    (
        &["strobing", "flashing"],
        Some(GpuMode::Flashing),
        MoboMode::Flashing,
    ),
    (
        &["color-cycle", "spectrum"],
        Some(GpuMode::Spectrum),
        MoboMode::Spectrum,
    ),
    (&["rainbow"], None, MoboMode::Rainbow),
    (&["rainbow-breathing"], None, MoboMode::RainbowBreathing),
    (&["comet", "chase-fade"], None, MoboMode::ChaseFade),
    (
        &["flash-and-dash", "spectrum-chase-fade"],
        None,
        MoboMode::SpectrumChaseFade,
    ),
    (&["chase"], None, MoboMode::Chase),
    (&["spectrum-chase"], None, MoboMode::SpectrumChase),
    (&["wave"], None, MoboMode::Wave),
    (
        &["glowing-yoyo", "rainbow-pulse"],
        None,
        MoboMode::RainbowPulse,
    ),
    (
        &["starry-night", "random-flicker"],
        None,
        MoboMode::RandomFlicker,
    ),
];

fn find_effect(name: &str) -> Option<(Option<GpuMode>, MoboMode, &'static str)> {
    let wanted = name.to_ascii_lowercase().replace('_', "-");
    EFFECTS
        .iter()
        .find(|(names, ..)| names.contains(&wanted.as_str()))
        .map(|(names, gpu, mobo)| (*gpu, *mobo, names[0]))
}

// ---------------------------------------------------------------- plans

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    All,
    Gpu,
    Mobo,
}

impl Target {
    fn gpu(self) -> bool {
        self != Self::Mobo
    }
    fn mobo(self) -> bool {
        self != Self::Gpu
    }
}

#[derive(Debug, PartialEq, Eq)]
struct GpuOp {
    mode: GpuMode,
    color: Option<Rgb>,
}

#[derive(Debug, PartialEq, Eq)]
enum MoboOp {
    Off,
    Static(Rgb),
    Zone(u8, Rgb),
    Effect {
        mode: MoboMode,
        speed: u8,
        color: Option<Rgb>,
    },
}

/// What a tool call will write, decided before any device is opened.
#[derive(Debug, Default, PartialEq, Eq)]
struct Plan {
    gpu: Option<GpuOp>,
    mobo: Option<MoboOp>,
    notes: Vec<String>,
}

fn hex((r, g, b): Rgb) -> String {
    format!("{r:02x}{g:02x}{b:02x}")
}

fn plan_color(target: Target, zone: Option<u8>, color: Rgb) -> Result<Plan, String> {
    if zone.is_some() && target != Target::Mobo {
        return Err("`zone` requires target \"mobo\"".to_owned());
    }
    Ok(Plan {
        gpu: target.gpu().then_some(GpuOp {
            mode: GpuMode::Static,
            color: Some(color),
        }),
        mobo: target.mobo().then_some(match zone {
            Some(zone) => MoboOp::Zone(zone, color),
            None => MoboOp::Static(color),
        }),
        notes: Vec::new(),
    })
}

fn plan_effect(
    target: Target,
    mode: &str,
    color: Option<Rgb>,
    speed: Option<u8>,
) -> Result<Plan, String> {
    let (gpu_mode, mobo_mode, name) =
        find_effect(mode).ok_or_else(|| format!("Unknown effect '{mode}'"))?;
    let mut plan = Plan::default();
    if target.gpu() {
        let fallback = gpu_mode.is_none();
        if fallback {
            plan.notes.push(format!(
                "GPU has no '{name}' effect; it shows static color instead (GPU effects: static, breathing, strobing, color-cycle)."
            ));
        }
        if speed.is_some() {
            plan.notes.push(
                "GPU effects have no speed control; speed applies to the motherboard only."
                    .to_owned(),
            );
        }
        plan.gpu = Some(GpuOp {
            mode: gpu_mode.unwrap_or(GpuMode::Static),
            color,
        });
    }
    if target.mobo() {
        plan.mobo = Some(match (mobo_mode, color) {
            (MoboMode::Static, Some(color)) => MoboOp::Static(color),
            _ => MoboOp::Effect {
                mode: mobo_mode,
                speed: speed.unwrap_or(0),
                color,
            },
        });
    }
    Ok(plan)
}

fn plan_off(target: Target) -> Plan {
    Plan {
        gpu: target.gpu().then_some(GpuOp {
            mode: GpuMode::Off,
            color: None,
        }),
        mobo: target.mobo().then_some(MoboOp::Off),
        notes: Vec::new(),
    }
}

fn describe_gpu(op: &GpuOp) -> Value {
    json!({"mode": op.mode.name(), "color": op.color.map(hex)})
}

fn describe_mobo(op: &MoboOp) -> Value {
    match op {
        MoboOp::Off => json!({"mode": "off"}),
        MoboOp::Static(c) => json!({"mode": "static", "color": hex(*c)}),
        MoboOp::Zone(zone, c) => json!({"mode": "static", "zone": zone, "color": hex(*c)}),
        MoboOp::Effect { mode, speed, color } => {
            json!({"mode": mode.name().replace('_', "-"), "speed": speed, "color": color.map(hex)})
        }
    }
}

fn apply_gpu(gpu: &mut Gpu, op: &GpuOp) -> Result<(), String> {
    match op.color {
        Some((r, g, b)) => gpu.set_color(r, g, b, op.mode),
        None => gpu.set_mode(op.mode),
    }
}

fn apply_mobo(mobo: &mut Mobo, op: &MoboOp) -> Result<(), String> {
    match *op {
        MoboOp::Off => mobo.set_mode(MoboMode::Off, 0, 0),
        MoboOp::Static((r, g, b)) => mobo.set_color_static(r, g, b),
        MoboOp::Zone(zone, (r, g, b)) => mobo.set_zone_color(zone, r, g, b),
        MoboOp::Effect { mode, speed, color } => mobo.set_effect(mode, speed, color),
    }
}

// ---------------------------------------------------------------- agent states

const AMBER: Rgb = (0xff, 0xb0, 0x00);
const CYAN: Rgb = (0x00, 0xe5, 0xff);
const GREEN: Rgb = (0x00, 0xe6, 0x76);
const RED: Rgb = (0xff, 0x17, 0x44);

struct Pattern {
    name: &'static str,
    plan: Plan,
    default_seconds: u64,
}

/// `None` means "idle", which restores the saved baseline instead of drawing.
fn agent_pattern(state: &str) -> Option<Pattern> {
    let (name, color, gpu_mode, mobo_mode, speed, default_seconds) = match state {
        "working" => (
            "amber_breathe",
            AMBER,
            GpuMode::Breathing,
            MoboMode::Breathing,
            2,
            HOLD_SECONDS,
        ),
        // The GPU has no wave effect: it holds static cyan while the board waves.
        "reviewing" => (
            "cyan_wave",
            CYAN,
            GpuMode::Static,
            MoboMode::Wave,
            2,
            HOLD_SECONDS,
        ),
        "success" => (
            "green_pulse",
            GREEN,
            GpuMode::Breathing,
            MoboMode::Breathing,
            0,
            PULSE_SECONDS,
        ),
        "failure" => (
            "red_strobe",
            RED,
            GpuMode::Flashing,
            MoboMode::Flashing,
            0,
            PULSE_SECONDS,
        ),
        _ => return None,
    };
    Some(Pattern {
        name,
        plan: Plan {
            gpu: Some(GpuOp {
                mode: gpu_mode,
                color: Some(color),
            }),
            mobo: Some(MoboOp::Effect {
                mode: mobo_mode,
                speed,
                color: Some(color),
            }),
            notes: Vec::new(),
        },
        default_seconds,
    })
}

struct Baseline {
    gpu: Option<GpuSnapshot>,
    mobo: Option<MoboSnapshot>,
}

struct AgentHold {
    state: String,
    baseline: Baseline,
    deadline: Instant,
}

// ---------------------------------------------------------------- arguments

fn arg_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

fn arg_uint(
    args: &Map<String, Value>,
    key: &str,
    min: u64,
    max: u64,
) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(format!("`{key}` must be an integer {min}..{max}")),
        },
    }
}

fn arg_target(args: &Map<String, Value>, default: Target) -> Result<Target, String> {
    match arg_str(args, "target")? {
        None => Ok(default),
        Some("all") => Ok(Target::All),
        Some("gpu") => Ok(Target::Gpu),
        Some("mobo") => Ok(Target::Mobo),
        Some(other) => Err(format!("`target` must be all, gpu or mobo (got '{other}')")),
    }
}

fn arg_color(args: &Map<String, Value>) -> Result<Option<Rgb>, String> {
    arg_str(args, "color")?.map(parse_hex_color).transpose()
}

fn reject_unknown(args: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match args.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!(
            "Unknown argument `{key}` (allowed: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------- tool catalog

fn tool_catalog() -> Value {
    let target = |extra: &str| json!({"type": "string", "enum": ["all", "gpu", "mobo"], "description": format!("Which device. {extra}")});
    let color = json!({"type": "string", "pattern": "^#?[0-9a-fA-F]{6}$", "description": "RGB hex such as 00ffcc or #ff0055"});
    let effect_names: Vec<&str> = EFFECTS.iter().map(|(names, ..)| names[0]).collect();
    json!([
        {
            "name": "aura_get_status",
            "description": "Read GPU and motherboard lighting state (mode, colors, firmware) and any active agent-state hold. Read-only.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "aura_set_color",
            "description": "Set a static RGB color. Motherboard writes color every onboard LED unless `zone` is given.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": target("Default all. Required to be mobo when `zone` is set."),
                    "color": color,
                    "zone": {"type": "integer", "minimum": 0, "maximum": 4, "description": "Motherboard zone: 0-3 Back I/O, 4 RGB header. Implies target mobo."}
                },
                "required": ["color"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        },
        {
            "name": "aura_set_effect",
            "description": "Apply a lighting effect. The GPU supports static, breathing, strobing and color-cycle only; other effects fall back to static GPU color and the response says so.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": target("Default all."),
                    "mode": {"type": "string", "enum": effect_names},
                    "color": color,
                    "speed": {"type": "integer", "minimum": 0, "maximum": 4, "description": "Motherboard only; 0 is fastest, 4 slowest. Default 0."}
                },
                "required": ["mode"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        },
        {
            "name": "aura_turn_off",
            "description": "Turn LEDs off.",
            "inputSchema": {"type": "object", "properties": {"target": target("Default all.")}, "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        },
        {
            "name": "aura_agent_state",
            "description": "Show an agent state on the case LEDs: working = amber breathe, reviewing = cyan wave (GPU holds static cyan), success = green pulse, failure = red strobe, idle = restore the lighting that was set before. success/failure revert on their own after 3s; working/reviewing after 15 min unless `duration_seconds` is given.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "state": {"type": "string", "enum": ["working", "reviewing", "success", "failure", "idle"]},
                    "duration_seconds": {"type": "integer", "minimum": 1, "maximum": MAX_HOLD_SECONDS, "description": "Seconds before the baseline is restored automatically."}
                },
                "required": ["state"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}
        }
    ])
}

fn topology() -> Value {
    json!({
        "gpu": {
            "device": "ASUS ROG Strix GeForce GTX 1060 6GB",
            "bus": crate::protocol::GPU_I2C_DEV,
            "address": format!("0x{:02x}", crate::protocol::GPU_I2C_ADDR),
            "identity_check": "registers 0x20/0x21 must read 0x1589 before any write",
            "effects": ["static", "breathing", "strobing", "color-cycle", "off"],
            "speed_control": false
        },
        "motherboard": {
            "device": "ASUS ROG STRIX Z270E GAMING",
            "super_io": "Nuvoton NCT6793D",
            "smbus_port": format!("0x{:04x}", crate::protocol::MOBO_PORT_BASE),
            "slave_address": format!("0x{:02x}", crate::protocol::MOBO_SLAVE_ADDR),
            "identity_check": format!("firmware must start with {}", crate::protocol::MOBO_EXPECTED_NAME),
            "access": "/dev/port (root or CAP_SYS_RAWIO)",
            "zones": [
                {"zone": 0, "channel": "Back I/O"}, {"zone": 1, "channel": "Back I/O"},
                {"zone": 2, "channel": "Back I/O"}, {"zone": 3, "channel": "Back I/O"},
                {"zone": 4, "channel": "RGB Header 1"}
            ],
            "zone_note": "Default mapping; aura://state/current reports the channels read from firmware.",
            "effects": EFFECTS.iter().map(|(names, ..)| names[0]).collect::<Vec<_>>(),
            "speed": "0 (fastest) to 4 (slowest)"
        },
        "agent_states": ["working", "reviewing", "success", "failure", "idle"]
    })
}

// ---------------------------------------------------------------- server

pub struct Server {
    dry_run: bool,
    hold: Option<AgentHold>,
}

fn rpc_error(id: &Value, code: i32, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn tool_result(body: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&body).unwrap_or_default();
    json!({"content": [{"type": "text", "text": text}], "structuredContent": body, "isError": is_error})
}

impl Server {
    pub fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            hold: None,
        }
    }

    /// One inbound line to at most one outbound message (notifications get none).
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(e) => return Some(rpc_error(&Value::Null, -32700, format!("Parse error: {e}"))),
        };
        let Some(object) = message.as_object() else {
            return Some(rpc_error(
                &Value::Null,
                -32600,
                "Expected a single JSON-RPC request object (batches are not supported)",
            ));
        };
        let id = object.get("id");
        if !matches!(
            id,
            None | Some(Value::Null | Value::String(_) | Value::Number(_))
        ) {
            return Some(rpc_error(
                &Value::Null,
                -32600,
                "`id` must be a string, number or null",
            ));
        }
        let reply_id = id.cloned().unwrap_or(Value::Null);
        let method = match object.get("method").and_then(Value::as_str) {
            Some(method) if object.get("jsonrpc") == Some(&json!("2.0")) => method,
            _ => {
                return Some(rpc_error(
                    &reply_id,
                    -32600,
                    "Invalid request: need jsonrpc \"2.0\" and a string `method`",
                ))
            }
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let outcome = self.dispatch(method, &params);
        id?; // a request without `id` is a notification: never reply
        Some(match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": reply_id, "result": result}),
            Err((code, message)) => rpc_error(&reply_id, code, message),
        })
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                let asked = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let version = SUPPORTED_VERSIONS
                    .iter()
                    .find(|v| **v == asked)
                    .unwrap_or(&SUPPORTED_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}, "resources": {"subscribe": false, "listChanged": false}},
                    "serverInfo": {"name": "asus-aura", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Controls ASUS Aura RGB on this host. Prefer aura_agent_state for status signalling; it restores the user's lighting on idle."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_catalog()})),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({"resources": [
                {"uri": "aura://hardware/topology", "name": "Hardware topology", "description": "Buses, addresses, zones and supported effects", "mimeType": "application/json"},
                {"uri": "aura://state/current", "name": "Current lighting state", "description": "Live device state and active agent hold", "mimeType": "application/json"}
            ]})),
            "resources/read" => {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or((-32602, "Missing `uri`".to_owned()))?;
                let body = match uri {
                    "aura://hardware/topology" => topology(),
                    "aura://state/current" => self.status(),
                    _ => return Err((-32002, format!("Resource not found: {uri}"))),
                };
                Ok(
                    json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": serde_json::to_string_pretty(&body).unwrap_or_default()}]}),
                )
            }
            _ => Err((-32601, format!("Method not found: {method}"))),
        }
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "Missing tool `name`".to_owned()))?;
        let empty = Map::new();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(map)) => map,
            Some(_) => return Err((-32602, "`arguments` must be an object".to_owned())),
        };
        if ![
            "aura_get_status",
            "aura_set_color",
            "aura_set_effect",
            "aura_turn_off",
            "aura_agent_state",
        ]
        .contains(&name)
        {
            return Err((-32602, format!("Unknown tool: {name}")));
        }
        let outcome = match name {
            "aura_get_status" => reject_unknown(args, &[]).map(|()| (self.status(), false)),
            "aura_set_color" => self.set_color(args),
            "aura_set_effect" => self.set_effect(args),
            "aura_turn_off" => self.turn_off(args),
            _ => self.agent_state(args),
        };
        Ok(match outcome {
            Ok((body, is_error)) => tool_result(body, is_error),
            Err(message) => tool_result(json!({"success": false, "error": message}), true),
        })
    }

    // ---- tools

    fn status(&self) -> Value {
        let gpu = gpu_controller(self.dry_run).and_then(|mut c| c.status());
        let mobo = mobo_controller(self.dry_run).and_then(|mut c| c.status());
        fn side<T: serde::Serialize>(result: Result<T, String>, key: &str) -> (String, Value) {
            match result {
                Ok(status) => (
                    key.to_owned(),
                    serde_json::to_value(status).unwrap_or(Value::Null),
                ),
                Err(error) => (key.to_owned(), json!({"error": error})),
            }
        }
        let connected = gpu.is_ok() || mobo.is_ok();
        let (gk, gv) = side(gpu, "gpu");
        let (mk, mv) = side(mobo, "motherboard");
        let now = Instant::now();
        let hold = self.hold.as_ref().map(|h| {
            json!({
                "state": h.state,
                "reverts_in_seconds": h.deadline.saturating_duration_since(now).as_secs_f32().ceil()
            })
        });
        json!({"connected": connected, "dry_run": self.dry_run, gk: gv, mk: mv, "agent_state": hold})
    }

    fn set_color(&mut self, args: &Map<String, Value>) -> Result<(Value, bool), String> {
        reject_unknown(args, &["target", "color", "zone"])?;
        let color = arg_color(args)?.ok_or("`color` is required")?;
        let zone = arg_uint(args, "zone", 0, u64::from(MOBO_ZONES - 1))?.map(|z| z as u8);
        let default = if zone.is_some() {
            Target::Mobo
        } else {
            Target::All
        };
        let plan = plan_color(arg_target(args, default)?, zone, color)?;
        self.apply_user_plan(&plan)
    }

    fn set_effect(&mut self, args: &Map<String, Value>) -> Result<(Value, bool), String> {
        reject_unknown(args, &["target", "mode", "color", "speed"])?;
        let mode = arg_str(args, "mode")?.ok_or("`mode` is required")?;
        let speed = arg_uint(args, "speed", 0, u64::from(MAX_SPEED))?.map(|s| s as u8);
        let plan = plan_effect(
            arg_target(args, Target::All)?,
            mode,
            arg_color(args)?,
            speed,
        )?;
        self.apply_user_plan(&plan)
    }

    fn turn_off(&mut self, args: &Map<String, Value>) -> Result<(Value, bool), String> {
        reject_unknown(args, &["target"])?;
        let plan = plan_off(arg_target(args, Target::All)?);
        self.apply_user_plan(&plan)
    }

    /// Explicit lighting changes supersede any agent-state hold (nothing left to restore).
    fn apply_user_plan(&mut self, plan: &Plan) -> Result<(Value, bool), String> {
        // Open every requested device first so a missing one aborts before any write.
        let mut gpu = plan
            .gpu
            .as_ref()
            .map(|_| gpu_controller(self.dry_run))
            .transpose()?;
        let mut mobo = plan
            .mobo
            .as_ref()
            .map(|_| mobo_controller(self.dry_run))
            .transpose()?;
        let (body, ok) = run_plan(plan, gpu.as_mut(), mobo.as_mut());
        if ok {
            self.hold = None;
        }
        Ok((body, !ok))
    }

    fn agent_state(&mut self, args: &Map<String, Value>) -> Result<(Value, bool), String> {
        reject_unknown(args, &["state", "duration_seconds"])?;
        let state = arg_str(args, "state")?.ok_or("`state` is required")?;
        let seconds = arg_uint(args, "duration_seconds", 1, MAX_HOLD_SECONDS)?;
        if state == "idle" {
            let restored = self.restore_baseline()?;
            return Ok((
                json!({"state_applied": "idle", "pattern": "baseline", "baseline_restored": restored}),
                false,
            ));
        }
        let pattern = agent_pattern(state).ok_or_else(|| {
            format!("Unknown state '{state}' (working, reviewing, success, failure, idle)")
        })?;
        let mut warnings = Vec::new();
        let mut gpu = gpu_controller(self.dry_run)
            .map_err(|e| warnings.push(format!("gpu unavailable: {e}")))
            .ok();
        let mut mobo = mobo_controller(self.dry_run)
            .map_err(|e| warnings.push(format!("motherboard unavailable: {e}")))
            .ok();
        if gpu.is_none() && mobo.is_none() {
            return Err(warnings.join("; "));
        }
        // Keep the earliest baseline across chained states (working -> success -> idle).
        let baseline = match self.hold.take() {
            Some(hold) => hold.baseline,
            None => Baseline {
                gpu: gpu.as_mut().and_then(|c| {
                    c.snapshot()
                        .map_err(|e| warnings.push(format!("gpu baseline not saved: {e}")))
                        .ok()
                }),
                mobo: mobo.as_mut().and_then(|c| {
                    c.snapshot()
                        .map_err(|e| warnings.push(format!("motherboard baseline not saved: {e}")))
                        .ok()
                }),
            },
        };
        let hold_for = Duration::from_secs(seconds.unwrap_or(pattern.default_seconds));
        self.hold = Some(AgentHold {
            state: state.to_owned(),
            baseline,
            deadline: Instant::now() + hold_for,
        });
        let (mut body, ok) = run_plan(&pattern.plan, gpu.as_mut(), mobo.as_mut());
        body["state_applied"] = json!(state);
        body["pattern"] = json!(pattern.name);
        body["reverts_in_seconds"] = json!(hold_for.as_secs());
        if !warnings.is_empty() {
            body["warnings"] = json!(warnings);
        }
        Ok((body, !ok))
    }

    /// Returns whether anything was restored; errors from the hardware are surfaced.
    fn restore_baseline(&mut self) -> Result<bool, String> {
        let Some(hold) = self.hold.take() else {
            return Ok(false);
        };
        let mut errors = Vec::new();
        if let Some(snapshot) = &hold.baseline.gpu {
            if let Err(e) = gpu_controller(self.dry_run).and_then(|mut c| c.restore(snapshot)) {
                errors.push(format!("gpu: {e}"));
            }
        }
        if let Some(snapshot) = &hold.baseline.mobo {
            if let Err(e) = mobo_controller(self.dry_run).and_then(|mut c| c.restore(snapshot)) {
                errors.push(format!("motherboard: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(true)
        } else {
            Err(format!("Baseline restore failed ({})", errors.join("; ")))
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.hold.as_ref().map(|h| h.deadline)
    }

    /// Timer path: revert once the hold has expired.
    pub fn expire_if_due(&mut self, now: Instant) {
        if self.hold.as_ref().is_some_and(|h| h.deadline <= now) {
            if let Err(e) = self.restore_baseline() {
                eprintln!("asus-aura mcp: {e}");
            }
        }
    }
}

/// Apply each side independently and report exactly what happened per device.
fn run_plan(plan: &Plan, gpu: Option<&mut Gpu>, mobo: Option<&mut Mobo>) -> (Value, bool) {
    let mut applied = Map::new();
    let mut errors = Map::new();
    if let (Some(op), Some(gpu)) = (&plan.gpu, gpu) {
        match apply_gpu(gpu, op) {
            Ok(()) => drop(applied.insert("gpu".to_owned(), describe_gpu(op))),
            Err(e) => drop(errors.insert("gpu".to_owned(), json!(e))),
        }
    }
    if let (Some(op), Some(mobo)) = (&plan.mobo, mobo) {
        match apply_mobo(mobo, op) {
            Ok(()) => drop(applied.insert("motherboard".to_owned(), describe_mobo(op))),
            Err(e) => drop(errors.insert("motherboard".to_owned(), json!(e))),
        }
    }
    let ok = errors.is_empty();
    let mut body = json!({"success": ok, "applied": applied, "notes": plan.notes});
    if !ok {
        body["errors"] = Value::Object(errors);
    }
    (body, ok)
}

// ---------------------------------------------------------------- stdio loop

pub fn serve(dry_run: bool) -> Result<(), String> {
    crate::protocol::route_dry_run_log_to_stderr();
    let mut server = Server::new(dry_run);
    let (tx, rx) = mpsc::channel::<String>();
    // Blocking reads live on their own thread so the loop can fire revert timers.
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut out = io::stdout().lock();
    loop {
        let line = match server.next_deadline() {
            Some(deadline) => {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(line) => Some(line),
                    Err(RecvTimeoutError::Timeout) => {
                        server.expire_if_due(Instant::now());
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => None,
                }
            }
            None => rx.recv().ok(),
        };
        let Some(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line) {
            writeln!(out, "{reply}")
                .and_then(|()| out.flush())
                .map_err(|e| e.to_string())?;
        }
    }
    // Client went away: leave the user's lighting as it was.
    if let Err(e) = server.restore_baseline() {
        eprintln!("asus-aura mcp: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &mut Server, line: Value) -> Value {
        server.handle_line(&line.to_string()).expect("a reply")
    }

    fn tool(server: &mut Server, name: &str, arguments: Value) -> Value {
        let reply = call(
            server,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}}),
        );
        reply["result"].clone()
    }

    #[test]
    fn plans_map_friendly_names_and_flag_gpu_fallbacks() {
        let plan = plan_effect(Target::All, "Color_Cycle", Some((1, 2, 3)), Some(2)).unwrap();
        assert_eq!(
            plan.gpu,
            Some(GpuOp {
                mode: GpuMode::Spectrum,
                color: Some((1, 2, 3))
            })
        );
        assert_eq!(
            plan.mobo,
            Some(MoboOp::Effect {
                mode: MoboMode::Spectrum,
                speed: 2,
                color: Some((1, 2, 3))
            })
        );
        assert_eq!(plan.notes.len(), 1); // speed ignored on GPU

        let wave = plan_effect(Target::All, "wave", None, None).unwrap();
        assert_eq!(
            wave.gpu,
            Some(GpuOp {
                mode: GpuMode::Static,
                color: None
            })
        );
        assert!(wave.notes[0].contains("no 'wave' effect"));

        assert!(plan_effect(Target::Gpu, "disco", None, None).is_err());
        assert_eq!(
            plan_effect(Target::Mobo, "static", Some((9, 9, 9)), None)
                .unwrap()
                .mobo,
            Some(MoboOp::Static((9, 9, 9)))
        );
    }

    #[test]
    fn zone_colors_are_motherboard_only() {
        assert!(plan_color(Target::All, Some(1), (0, 0, 0)).is_err());
        assert_eq!(
            plan_color(Target::Mobo, Some(4), (1, 1, 1)).unwrap().mobo,
            Some(MoboOp::Zone(4, (1, 1, 1)))
        );
        assert_eq!(plan_off(Target::Gpu).mobo, None);
    }

    #[test]
    fn agent_patterns_match_the_spec() {
        for (state, color, mobo_mode, gpu_mode, seconds) in [
            (
                "working",
                AMBER,
                MoboMode::Breathing,
                GpuMode::Breathing,
                HOLD_SECONDS,
            ),
            (
                "reviewing",
                CYAN,
                MoboMode::Wave,
                GpuMode::Static,
                HOLD_SECONDS,
            ),
            (
                "success",
                GREEN,
                MoboMode::Breathing,
                GpuMode::Breathing,
                PULSE_SECONDS,
            ),
            (
                "failure",
                RED,
                MoboMode::Flashing,
                GpuMode::Flashing,
                PULSE_SECONDS,
            ),
        ] {
            let pattern = agent_pattern(state).unwrap();
            assert_eq!(
                pattern.plan.gpu,
                Some(GpuOp {
                    mode: gpu_mode,
                    color: Some(color)
                }),
                "{state}"
            );
            assert!(
                matches!(pattern.plan.mobo, Some(MoboOp::Effect { mode, color: Some(c), .. }) if mode == mobo_mode && c == color),
                "{state}"
            );
            assert_eq!(pattern.default_seconds, seconds);
        }
        assert!(agent_pattern("idle").is_none());
    }

    #[test]
    fn protocol_handshake_and_errors() {
        let mut server = Server::new(true);
        let init = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}),
        );
        assert_eq!(init["id"], "a");
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        let newer = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 2, "method": "initialize", "params": {"protocolVersion": "2099-01-01"}}),
        );
        assert_eq!(newer["result"]["protocolVersion"], SUPPORTED_VERSIONS[0]);

        assert!(server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
        assert_eq!(
            server.handle_line("{nope").unwrap()["error"]["code"],
            -32700
        );
        assert_eq!(server.handle_line("[]").unwrap()["error"]["code"], -32600);
        assert_eq!(
            server.handle_line(r#"{"id":1,"method":"ping"}"#).unwrap()["error"]["code"],
            -32600
        );
        assert_eq!(
            call(
                &mut server,
                json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})
            )["error"]["code"],
            -32601
        );
        let unknown = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "aura_nope"}}),
        );
        assert_eq!(unknown["error"]["code"], -32602);
        let tools = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 5, "method": "tools/list"}),
        );
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn invalid_arguments_are_tool_errors_that_name_the_problem() {
        let mut server = Server::new(true);
        for (name, args, needle) in [
            (
                "aura_set_color",
                json!({"color": "zzz"}),
                "6 hexadecimal digits",
            ),
            ("aura_set_color", json!({}), "`color` is required"),
            (
                "aura_set_color",
                json!({"color": "ffffff", "zone": 5}),
                "0..4",
            ),
            (
                "aura_set_color",
                json!({"color": "ffffff", "zone": 1, "target": "gpu"}),
                "requires target",
            ),
            (
                "aura_set_effect",
                json!({"mode": "wave", "speed": 9}),
                "0..4",
            ),
            (
                "aura_set_effect",
                json!({"mode": "disco"}),
                "Unknown effect",
            ),
            (
                "aura_turn_off",
                json!({"target": "fans"}),
                "all, gpu or mobo",
            ),
            ("aura_turn_off", json!({"colour": "x"}), "Unknown argument"),
            (
                "aura_agent_state",
                json!({"state": "panic"}),
                "Unknown state",
            ),
        ] {
            let result = tool(&mut server, name, args);
            assert_eq!(result["isError"], true, "{name}");
            assert!(
                result["structuredContent"]["error"]
                    .as_str()
                    .unwrap()
                    .contains(needle),
                "{name}: {result}"
            );
        }
    }

    #[test]
    fn dry_run_tools_report_what_was_applied() {
        let mut server = Server::new(true);
        let result = tool(
            &mut server,
            "aura_set_effect",
            json!({"mode": "wave", "color": "00ffcc", "speed": 3}),
        );
        assert_eq!(result["isError"], false);
        let body = &result["structuredContent"];
        assert_eq!(body["applied"]["gpu"]["mode"], "static");
        assert_eq!(
            body["applied"]["motherboard"],
            json!({"mode": "wave", "speed": 3, "color": "00ffcc"})
        );
        assert!(body["notes"][0]
            .as_str()
            .unwrap()
            .contains("static color instead"));

        let status = tool(&mut server, "aura_get_status", json!({}));
        assert_eq!(status["structuredContent"]["connected"], true);
        assert_eq!(status["structuredContent"]["motherboard"]["led_count"], 5);
    }

    #[test]
    fn agent_hold_reverts_on_idle_timeout_and_manual_override() {
        let mut server = Server::new(true);
        let working = tool(&mut server, "aura_agent_state", json!({"state": "working"}));
        assert_eq!(working["structuredContent"]["pattern"], "amber_breathe");
        assert_eq!(server.hold.as_ref().unwrap().state, "working");

        // A later state keeps the first baseline.
        tool(
            &mut server,
            "aura_agent_state",
            json!({"state": "success", "duration_seconds": 1}),
        );
        assert_eq!(server.hold.as_ref().unwrap().state, "success");
        assert!(server.hold.as_ref().unwrap().baseline.gpu.is_some());
        let status = tool(&mut server, "aura_get_status", json!({}));
        assert_eq!(
            status["structuredContent"]["agent_state"]["state"],
            "success"
        );

        server.expire_if_due(Instant::now()); // not due yet
        assert!(server.hold.is_some());
        server.expire_if_due(Instant::now() + Duration::from_secs(2));
        assert!(server.hold.is_none());

        tool(&mut server, "aura_agent_state", json!({"state": "failure"}));
        let idle = tool(&mut server, "aura_agent_state", json!({"state": "idle"}));
        assert_eq!(idle["structuredContent"]["baseline_restored"], true);
        let again = tool(&mut server, "aura_agent_state", json!({"state": "idle"}));
        assert_eq!(again["structuredContent"]["baseline_restored"], false);

        tool(&mut server, "aura_agent_state", json!({"state": "working"}));
        tool(&mut server, "aura_set_color", json!({"color": "112233"}));
        assert!(
            server.hold.is_none(),
            "an explicit change supersedes the hold"
        );
    }

    #[test]
    fn resources_list_and_read() {
        let mut server = Server::new(true);
        let list = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
        );
        assert_eq!(list["result"]["resources"].as_array().unwrap().len(), 2);
        for uri in ["aura://hardware/topology", "aura://state/current"] {
            let read = call(
                &mut server,
                json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": uri}}),
            );
            let text = read["result"]["contents"][0]["text"].as_str().unwrap();
            serde_json::from_str::<Value>(text).expect("JSON body");
        }
        let missing = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "aura://nope"}}),
        );
        assert_eq!(missing["error"]["code"], -32002);
    }
}
