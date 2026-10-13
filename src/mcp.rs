//! MCP server (`luminactl mcp`): newline-delimited JSON-RPC 2.0 over stdio.
//!
//! stdout carries protocol messages only; every diagnostic goes to stderr.

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
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
    (&["rainbow"], Some(GpuMode::Spectrum), MoboMode::Rainbow),
    (
        &["rainbow-breathing"],
        Some(GpuMode::Spectrum),
        MoboMode::RainbowBreathing,
    ),
    (&["comet", "chase-fade"], None, MoboMode::ChaseFade),
    (
        &["flash-and-dash", "spectrum-chase-fade"],
        None,
        MoboMode::SpectrumChaseFade,
    ),
    (&["chase"], None, MoboMode::Chase),
    (&["spectrum-chase"], None, MoboMode::SpectrumChase),
    (&["wave"], Some(GpuMode::Spectrum), MoboMode::Wave),
    (
        &["glowing-yoyo", "rainbow-pulse"],
        Some(GpuMode::Spectrum),
        MoboMode::RainbowPulse,
    ),
    (
        &["starry-night", "random-flicker"],
        None,
        MoboMode::RandomFlicker,
    ),
];

pub(crate) fn find_effect(name: &str) -> Option<(Option<GpuMode>, MoboMode, &'static str)> {
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
    Mode(MoboMode),
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
        match gpu_mode {
            None => plan.notes.push(format!(
                "GPU has no '{name}' effect; it shows static color instead (GPU effects: static, breathing, strobing, color-cycle)."
            )),
            Some(GpuMode::Spectrum) if name != "color-cycle" => plan.notes.push(format!(
                "GPU has no '{name}' effect; it runs its color cycle (spectrum) instead."
            )),
            Some(_) => {}
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
            (MoboMode::Static, None) => MoboOp::Mode(MoboMode::Static),
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
        MoboOp::Mode(mode) => json!({"mode": mode.name()}),
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
        MoboOp::Mode(mode) => mobo.set_mode(mode, 0, 0),
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

const MAX_RESTORE_ATTEMPTS: u8 = 3;
const RESTORE_RETRY: Duration = Duration::from_secs(5);

/// A saved baseline for one device. Devices are tracked separately so an explicit
/// change, a failed restore or a missing snapshot on one never affects the other.
struct DeviceHold<S> {
    snapshot: S,
    attempts: u8,
}

struct AgentHold {
    state: String,
    deadline: Instant,
    gpu: Option<DeviceHold<GpuSnapshot>>,
    mobo: Option<DeviceHold<MoboSnapshot>>,
}

impl AgentHold {
    fn is_empty(&self) -> bool {
        self.gpu.is_none() && self.mobo.is_none()
    }

    fn attempts(&self) -> u8 {
        let gpu = self.gpu.as_ref().map_or(0, |h| h.attempts);
        let mobo = self.mobo.as_ref().map_or(0, |h| h.attempts);
        gpu.max(mobo)
    }

    fn enrolled(&self) -> Vec<&'static str> {
        let mut devices = Vec::new();
        if self.gpu.is_some() {
            devices.push("gpu");
        }
        if self.mobo.is_some() {
            devices.push("motherboard");
        }
        devices
    }
}

/// Outcome of one restore pass, per device.
#[derive(Default)]
struct RestoreReport {
    restored: Vec<&'static str>,
    not_enrolled: Vec<&'static str>,
    failed: Vec<String>,
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
            "name": "lumina_get_status",
            "description": "Read GPU and motherboard lighting state (mode, colors, firmware) and any active agent-state hold. Read-only.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "lumina_set_color",
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
            "name": "lumina_set_effect",
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
            "name": "lumina_turn_off",
            "description": "Turn LEDs off.",
            "inputSchema": {"type": "object", "properties": {"target": target("Default all.")}, "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false}
        },
        {
            "name": "lumina_agent_state",
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
            "zone_note": "Default mapping; lumina://state/current reports the channels read from firmware.",
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
    /// Test-only fault injection: (operation, device) pairs that must fail.
    #[cfg(test)]
    faults: Vec<(&'static str, &'static str)>,
    #[cfg(test)]
    restore_calls: Vec<&'static str>,
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
            #[cfg(test)]
            faults: Vec::new(),
            #[cfg(test)]
            restore_calls: Vec::new(),
        }
    }

    #[cfg(test)]
    fn fault(&self, operation: &str, device: &str) -> bool {
        self.faults
            .iter()
            .any(|(o, d)| *o == operation && *d == device)
    }

    #[cfg(not(test))]
    fn fault(&self, _operation: &str, _device: &str) -> bool {
        false
    }

    fn open_gpu(&self) -> Result<Gpu, String> {
        if self.fault("open", "gpu") {
            return Err("injected open failure".to_owned());
        }
        gpu_controller(self.dry_run)
    }

    fn open_mobo(&self) -> Result<Mobo, String> {
        if self.fault("open", "motherboard") {
            return Err("injected open failure".to_owned());
        }
        mobo_controller(self.dry_run)
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
                    "serverInfo": {"name": "luminactl", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Controls Lumina RGB lighting on this host (reverse-engineered for ASUS Aura Sync hardware). Prefer lumina_agent_state for status signalling; it restores the user's lighting on idle."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_catalog()})),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({"resources": [
                {"uri": "lumina://hardware/topology", "name": "Hardware topology", "description": "Buses, addresses, zones and supported effects", "mimeType": "application/json"},
                {"uri": "lumina://state/current", "name": "Current lighting state", "description": "Live device state and active agent hold", "mimeType": "application/json"}
            ]})),
            "resources/read" => {
                let uri = params
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or((-32602, "Missing `uri`".to_owned()))?;
                // `aura://` is the legacy spelling of the same resources.
                let canonical = uri
                    .strip_prefix("aura://")
                    .map(|rest| format!("lumina://{rest}"));
                let body = match canonical.as_deref().unwrap_or(uri) {
                    "lumina://hardware/topology" => topology(),
                    "lumina://state/current" => self.status(),
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
        // Legacy `aura_*` names stay accepted as aliases of the canonical `lumina_*` tools.
        let canonical = name
            .strip_prefix("aura_")
            .map(|rest| format!("lumina_{rest}"));
        let name = canonical.as_deref().unwrap_or(name);
        if ![
            "lumina_get_status",
            "lumina_set_color",
            "lumina_set_effect",
            "lumina_turn_off",
            "lumina_agent_state",
        ]
        .contains(&name)
        {
            return Err((-32602, format!("Unknown tool: {name}")));
        }
        let outcome = match name {
            "lumina_get_status" => reject_unknown(args, &[]).map(|()| (self.status(), false)),
            "lumina_set_color" => self.set_color(args),
            "lumina_set_effect" => self.set_effect(args),
            "lumina_turn_off" => self.turn_off(args),
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
        let hold = self.hold_status();
        json!({"connected": connected, "dry_run": self.dry_run, gk: gv, mk: mv, "agent_state": hold})
    }

    fn hold_status(&self) -> Value {
        let now = Instant::now();
        self.hold
            .as_ref()
            .map(|h| {
                json!({
                    "state": if h.attempts() > 0 { "restoring" } else { &h.state },
                    "restore_attempts": h.attempts(),
                    "baseline_devices": h.enrolled(),
                    "reverts_in_seconds": h.deadline.saturating_duration_since(now).as_secs_f32().ceil()
                })
            })
            .unwrap_or(Value::Null)
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

    /// An explicit change relinquishes the saved baseline of exactly the devices it
    /// changed. Devices it did not touch, or failed to change, keep theirs.
    fn apply_user_plan(&mut self, plan: &Plan) -> Result<(Value, bool), String> {
        // Open every requested device first so a missing one aborts before any write.
        let mut gpu = plan.gpu.as_ref().map(|_| self.open_gpu()).transpose()?;
        let mut mobo = plan.mobo.as_ref().map(|_| self.open_mobo()).transpose()?;
        let (mut body, ok) = run_plan(plan, gpu.as_mut(), mobo.as_mut(), &|d| {
            self.fault("apply", d)
        });
        let applied = body["applied"].clone();
        let mut released = Vec::new();
        if let Some(hold) = self.hold.as_mut() {
            if applied.get("gpu").is_some() && hold.gpu.take().is_some() {
                released.push("gpu");
            }
            if applied.get("motherboard").is_some() && hold.mobo.take().is_some() {
                released.push("motherboard");
            }
            if hold.is_empty() {
                self.hold = None;
            }
        }
        if !released.is_empty() {
            body["agent_hold_released"] = json!(released);
        }
        Ok((body, !ok))
    }

    fn agent_state(&mut self, args: &Map<String, Value>) -> Result<(Value, bool), String> {
        reject_unknown(args, &["state", "duration_seconds"])?;
        let state = arg_str(args, "state")?.ok_or("`state` is required")?;
        let seconds = arg_uint(args, "duration_seconds", 1, MAX_HOLD_SECONDS)?;
        if state == "idle" {
            let report = self.restore_baseline();
            let ok = report.failed.is_empty();
            let mut body = json!({
                "state_applied": "idle",
                "pattern": "baseline",
                "baseline_restored": !report.restored.is_empty(),
                "restored_devices": report.restored,
                "not_enrolled": report.not_enrolled,
            });
            if !ok {
                body["success"] = json!(false);
                body["error"] = json!(format!(
                    "Baseline restore failed ({}); the remaining baselines are kept and retried",
                    report.failed.join("; ")
                ));
            }
            return Ok((body, !ok));
        }
        let pattern = agent_pattern(state).ok_or_else(|| {
            format!("Unknown state '{state}' (working, reviewing, success, failure, idle)")
        })?;
        let mut warnings = Vec::new();
        let mut gpu = self
            .open_gpu()
            .map_err(|e| warnings.push(format!("gpu unavailable: {e}")))
            .ok();
        let mut mobo = self
            .open_mobo()
            .map_err(|e| warnings.push(format!("motherboard unavailable: {e}")))
            .ok();
        let mut hold = self.hold.take().unwrap_or(AgentHold {
            state: state.to_owned(),
            deadline: Instant::now(),
            gpu: None,
            mobo: None,
        });
        // A device only joins the pattern once its baseline is safely saved; chained
        // states keep the earliest baseline and enrol newly available devices.
        if hold.gpu.is_none() {
            if let Some(controller) = gpu.as_mut() {
                let saved = if self.fault("snapshot", "gpu") {
                    Err("injected snapshot failure".to_owned())
                } else {
                    controller.snapshot()
                };
                match saved {
                    Ok(snapshot) => {
                        hold.gpu = Some(DeviceHold {
                            snapshot,
                            attempts: 0,
                        })
                    }
                    Err(e) => {
                        warnings.push(format!("gpu excluded: baseline not saved: {e}"));
                        gpu = None;
                    }
                }
            }
        }
        if hold.mobo.is_none() {
            if let Some(controller) = mobo.as_mut() {
                let saved = if self.fault("snapshot", "motherboard") {
                    Err("injected snapshot failure".to_owned())
                } else {
                    controller.snapshot()
                };
                match saved {
                    Ok(snapshot) => {
                        hold.mobo = Some(DeviceHold {
                            snapshot,
                            attempts: 0,
                        })
                    }
                    Err(e) => {
                        warnings.push(format!("motherboard excluded: baseline not saved: {e}"));
                        mobo = None;
                    }
                }
            }
        }
        if gpu.is_none() && mobo.is_none() {
            if !hold.is_empty() {
                self.hold = Some(hold);
            }
            return Err(format!(
                "No device with a recoverable baseline: {}",
                warnings.join("; ")
            ));
        }
        let hold_for = Duration::from_secs(seconds.unwrap_or(pattern.default_seconds));
        hold.state = state.to_owned();
        hold.deadline = Instant::now() + hold_for;
        if gpu.is_some() {
            if let Some(h) = hold.gpu.as_mut() {
                h.attempts = 0;
            }
        }
        if mobo.is_some() {
            if let Some(h) = hold.mobo.as_mut() {
                h.attempts = 0;
            }
        }
        let enrolled = hold.enrolled();
        self.hold = Some(hold);
        let (mut body, ok) = run_plan(&pattern.plan, gpu.as_mut(), mobo.as_mut(), &|d| {
            self.fault("apply", d)
        });
        body["state_applied"] = json!(state);
        body["pattern"] = json!(pattern.name);
        body["reverts_in_seconds"] = json!(hold_for.as_secs());
        body["baseline_devices"] = json!(enrolled);
        if !warnings.is_empty() {
            body["warnings"] = json!(warnings);
        }
        Ok((body, !ok))
    }

    /// Restore each enrolled device independently. A device that restores is released
    /// and never rewritten; one that fails keeps its baseline for a bounded retry.
    fn restore_baseline(&mut self) -> RestoreReport {
        let mut report = RestoreReport::default();
        let Some(mut hold) = self.hold.take() else {
            return report;
        };
        if hold.gpu.is_none() {
            report.not_enrolled.push("gpu");
        }
        if hold.mobo.is_none() {
            report.not_enrolled.push("motherboard");
        }
        if let Some(mut device) = hold.gpu.take() {
            #[cfg(test)]
            self.restore_calls.push("gpu");
            let result = if self.fault("restore", "gpu") {
                Err("injected restore failure".to_owned())
            } else {
                self.open_gpu()
                    .and_then(|mut c| c.restore(&device.snapshot))
            };
            match result {
                Ok(()) => report.restored.push("gpu"),
                Err(e) => {
                    device.attempts += 1;
                    report.failed.push(format!("gpu: {e}"));
                    if device.attempts < MAX_RESTORE_ATTEMPTS {
                        hold.gpu = Some(device);
                    } else {
                        crate::diag::log(&format!(
                            "luminactl mcp: giving up gpu baseline restore after {MAX_RESTORE_ATTEMPTS} attempts: {e}"
                        ));
                    }
                }
            }
        }
        if let Some(mut device) = hold.mobo.take() {
            #[cfg(test)]
            self.restore_calls.push("motherboard");
            let result = if self.fault("restore", "motherboard") {
                Err("injected restore failure".to_owned())
            } else {
                self.open_mobo()
                    .and_then(|mut c| c.restore(&device.snapshot))
            };
            match result {
                Ok(()) => report.restored.push("motherboard"),
                Err(e) => {
                    device.attempts += 1;
                    report.failed.push(format!("motherboard: {e}"));
                    if device.attempts < MAX_RESTORE_ATTEMPTS {
                        hold.mobo = Some(device);
                    } else {
                        crate::diag::log(&format!(
                            "luminactl mcp: giving up motherboard baseline restore after {MAX_RESTORE_ATTEMPTS} attempts: {e}"
                        ));
                    }
                }
            }
        }
        if !hold.is_empty() {
            hold.deadline = Instant::now() + RESTORE_RETRY;
            self.hold = Some(hold);
        }
        report
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.hold.as_ref().map(|h| h.deadline)
    }

    /// Timer path: revert once the hold has expired.
    pub fn expire_if_due(&mut self, now: Instant) {
        if self.hold.as_ref().is_some_and(|h| h.deadline <= now) {
            for failure in self.restore_baseline().failed {
                crate::diag::log(&format!(
                    "luminactl mcp: baseline restore failed: {failure}"
                ));
            }
        }
    }
}

/// Apply each side independently and report exactly what happened per device.
fn run_plan(
    plan: &Plan,
    gpu: Option<&mut Gpu>,
    mobo: Option<&mut Mobo>,
    inject_failure: &dyn Fn(&str) -> bool,
) -> (Value, bool) {
    let mut applied = Map::new();
    let mut errors = Map::new();
    if let (Some(op), Some(gpu)) = (&plan.gpu, gpu) {
        let result = if inject_failure("gpu") {
            Err("injected apply failure".to_owned())
        } else {
            apply_gpu(gpu, op)
        };
        match result {
            Ok(()) => drop(applied.insert("gpu".to_owned(), describe_gpu(op))),
            Err(e) => drop(errors.insert("gpu".to_owned(), json!(e))),
        }
    }
    if let (Some(op), Some(mobo)) = (&plan.mobo, mobo) {
        let result = if inject_failure("motherboard") {
            Err("injected apply failure".to_owned())
        } else {
            apply_mobo(mobo, op)
        };
        match result {
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

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn request_shutdown(_: i32) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

enum Input {
    Line(String),
    TooLarge,
}

fn read_input(mut reader: impl BufRead, tx: mpsc::SyncSender<Input>) -> io::Result<()> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if !oversized && !line.is_empty() {
                let _ = tx.send(Input::Line(String::from_utf8_lossy(&line).into_owned()));
            }
            return Ok(());
        }
        let end = buffer.iter().position(|b| *b == b'\n');
        let count = end.unwrap_or(buffer.len());
        if !oversized {
            if line.len() + count > MAX_MESSAGE_BYTES {
                oversized = true;
                line.clear();
                if tx.send(Input::TooLarge).is_err() {
                    return Ok(());
                }
            } else {
                line.extend_from_slice(&buffer[..count]);
            }
        }
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            if !oversized
                && tx
                    .send(Input::Line(String::from_utf8_lossy(&line).into_owned()))
                    .is_err()
            {
                return Ok(());
            }
            line.clear();
            oversized = false;
        }
    }
}

/// Replies go through a bounded queue to a writer thread, so a client that stops
/// reading can block only that thread: never hold timers, signals or restoration.
struct Output {
    tx: Option<mpsc::SyncSender<String>>,
    failure: Arc<Mutex<Option<String>>>,
    done: mpsc::Receiver<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}

const OUTPUT_QUEUE: usize = 64;
/// How long a normal end of input waits for accepted replies to reach stdout.
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

impl Output {
    fn spawn(mut sink: impl Write + Send + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel::<String>(OUTPUT_QUEUE);
        let (done_tx, done) = mpsc::channel();
        let failure = Arc::new(Mutex::new(None));
        let record = Arc::clone(&failure);
        let worker = std::thread::spawn(move || {
            for line in rx {
                if let Err(e) = writeln!(sink, "{line}").and_then(|()| sink.flush()) {
                    *record.lock().unwrap() = Some(format!("stdout: {e}"));
                    break; // dropping rx makes further sends fail immediately
                }
            }
            let _ = done_tx.send(());
        });
        Self {
            tx: Some(tx),
            failure,
            done,
            worker: Some(worker),
        }
    }

    fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }

    fn send(&self, reply: &Value) -> Result<(), String> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let Some(tx) = &self.tx else {
            return Err("stdout: output already closed".to_owned());
        };
        match tx.try_send(reply.to_string()) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                Err("client is not reading responses (output backlog full)".to_owned())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err(self
                .failure()
                .unwrap_or_else(|| "stdout: writer stopped".to_owned())),
        }
    }

    /// Close the queue and wait (bounded) for the worker to write out every accepted
    /// reply. The worker is joined only after it confirmed completion, so an unread
    /// pipe can delay this by at most `timeout`, never block it forever.
    fn finish(&mut self, timeout: Duration) -> Result<(), String> {
        self.tx = None; // lets the worker drain what is queued, then stop
        match self.done.recv_timeout(timeout) {
            Ok(()) => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(self.failure().unwrap_or_else(|| {
                    format!("stdout: replies were not delivered within {timeout:?}")
                }));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(self
                    .failure()
                    .unwrap_or_else(|| "stdout: writer thread died".to_owned()));
            }
        }
        self.failure().map_or(Ok(()), Err)
    }
}

/// Why the main loop stopped without a transport error.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Input closed: accepted replies are still owed to the client.
    InputClosed,
    /// SIGINT/SIGTERM: exit promptly, never wait on a possibly unread stdout.
    Signal,
}

/// Main loop. Returns how it ended on EOF or a shutdown signal and Err on a transport failure;
/// the caller runs the same cleanup either way.
fn run_loop(
    server: &mut Server,
    rx: &mpsc::Receiver<Input>,
    out: &Output,
    stop: &AtomicBool,
) -> Result<Ended, String> {
    loop {
        if stop.load(Ordering::Relaxed) {
            crate::diag::log("luminactl mcp: shutdown signal received");
            return Ok(Ended::Signal);
        }
        if let Some(error) = out.failure() {
            return Err(error);
        }
        server.expire_if_due(Instant::now());
        let wait = server
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(250))
            .min(Duration::from_millis(250));
        let input = match rx.recv_timeout(wait) {
            Ok(input) => input,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(Ended::InputClosed),
        };
        let reply = match input {
            Input::TooLarge => Some(rpc_error(&Value::Null, -32600, "message too large")),
            Input::Line(line) if line.trim().is_empty() => None,
            Input::Line(line) => server.handle_line(&line),
        };
        if let Some(reply) = reply {
            out.send(&reply)?;
        }
    }
}

/// Single exit path for every way a session can end: restore the saved lighting,
/// retrying failed devices a bounded number of times.
fn shutdown_cleanup(server: &mut Server) {
    for failure in server.restore_baseline().failed {
        crate::diag::log(&format!(
            "luminactl mcp: baseline restore failed: {failure}"
        ));
    }
    while server.hold.is_some() {
        std::thread::sleep(Duration::from_millis(250));
        server.expire_if_due(Instant::now());
    }
}

pub fn serve(dry_run: bool) -> Result<(), String> {
    // Linux signal handlers only set a lock-free flag; restoration stays on this thread.
    for signum in [2, 15] {
        if unsafe { signal(signum, request_shutdown) } == usize::MAX {
            return Err(format!(
                "cannot install signal handler: {}",
                io::Error::last_os_error()
            ));
        }
    }
    crate::protocol::route_dry_run_log_to_stderr();
    let mut server = Server::new(dry_run);
    let (tx, rx) = mpsc::sync_channel::<Input>(1);
    // Blocking reads live on their own thread so the loop can fire revert timers.
    std::thread::spawn(move || {
        if let Err(error) = read_input(io::stdin().lock(), tx) {
            crate::diag::log(&format!("luminactl mcp: input error: {error}"));
        }
    });
    let mut out = Output::spawn(io::stdout());
    let outcome = run_loop(&mut server, &rx, &out, &SHUTDOWN);
    // Hardware first: restoration never waits on the client's output.
    shutdown_cleanup(&mut server);
    // Only a normal end of input still owes replies. After a signal or a transport
    // failure there is nobody (or no time) to deliver them to.
    let outcome = match outcome {
        Ok(Ended::InputClosed) => out.finish(OUTPUT_DRAIN_TIMEOUT),
        Ok(Ended::Signal) => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = &outcome {
        crate::diag::log(&format!("luminactl mcp: ending session: {error}"));
    } else {
        crate::diag::log("luminactl mcp: clean shutdown");
    }
    crate::diag::flush(Duration::from_millis(500));
    outcome
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
        assert_eq!(
            plan_effect(Target::Mobo, "static", None, Some(4))
                .unwrap()
                .mobo,
            Some(MoboOp::Mode(MoboMode::Static))
        );
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

        // Effects without any GPU equivalent fall back to static color...
        let flicker = plan_effect(Target::All, "starry-night", None, None).unwrap();
        assert_eq!(
            flicker.gpu,
            Some(GpuOp {
                mode: GpuMode::Static,
                color: None
            })
        );
        assert!(flicker.notes[0].contains("no 'starry-night' effect"));
        // ...while rainbow-style effects use the GPU's color cycle, with no color needed.
        for name in ["rainbow", "rainbow-breathing", "wave", "rainbow-pulse"] {
            let plan = plan_effect(Target::All, name, None, None).unwrap();
            assert_eq!(
                plan.gpu,
                Some(GpuOp {
                    mode: GpuMode::Spectrum,
                    color: None
                }),
                "{name}"
            );
            assert!(plan.notes[0].contains("color cycle (spectrum)"), "{name}");
        }

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
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "lumina_nope"}}),
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
                "lumina_set_color",
                json!({"color": "zzz"}),
                "6 hexadecimal digits",
            ),
            ("lumina_set_color", json!({}), "`color` is required"),
            (
                "lumina_set_color",
                json!({"color": "ffffff", "zone": 5}),
                "0..4",
            ),
            (
                "lumina_set_color",
                json!({"color": "ffffff", "zone": 1, "target": "gpu"}),
                "requires target",
            ),
            (
                "lumina_set_effect",
                json!({"mode": "wave", "speed": 9}),
                "0..4",
            ),
            (
                "lumina_set_effect",
                json!({"mode": "disco"}),
                "Unknown effect",
            ),
            (
                "lumina_turn_off",
                json!({"target": "fans"}),
                "all, gpu or mobo",
            ),
            (
                "lumina_turn_off",
                json!({"colour": "x"}),
                "Unknown argument",
            ),
            (
                "lumina_agent_state",
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
            "lumina_set_effect",
            json!({"mode": "wave", "color": "00ffcc", "speed": 3}),
        );
        assert_eq!(result["isError"], false);
        let body = &result["structuredContent"];
        assert_eq!(body["applied"]["gpu"]["mode"], "spectrum");
        assert_eq!(
            body["applied"]["motherboard"],
            json!({"mode": "wave", "speed": 3, "color": "00ffcc"})
        );
        assert!(body["notes"][0]
            .as_str()
            .unwrap()
            .contains("color cycle (spectrum)"));

        let status = tool(&mut server, "lumina_get_status", json!({}));
        assert_eq!(status["structuredContent"]["connected"], true);
        assert_eq!(status["structuredContent"]["motherboard"]["led_count"], 5);
    }

    #[test]
    fn agent_hold_reverts_on_idle_timeout_and_manual_override() {
        let mut server = Server::new(true);
        let working = tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "working"}),
        );
        assert_eq!(working["structuredContent"]["pattern"], "amber_breathe");
        assert_eq!(server.hold.as_ref().unwrap().state, "working");

        // A later state keeps the first baseline.
        tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "success", "duration_seconds": 1}),
        );
        assert_eq!(server.hold.as_ref().unwrap().state, "success");
        assert!(server.hold.as_ref().unwrap().gpu.is_some());
        let status = tool(&mut server, "lumina_get_status", json!({}));
        assert_eq!(
            status["structuredContent"]["agent_state"]["state"],
            "success"
        );

        server.expire_if_due(Instant::now()); // not due yet
        assert!(server.hold.is_some());
        server.expire_if_due(Instant::now() + Duration::from_secs(2));
        assert!(server.hold.is_none());

        tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "failure"}),
        );
        let idle = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(idle["structuredContent"]["baseline_restored"], true);
        let again = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(again["structuredContent"]["baseline_restored"], false);

        tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "working"}),
        );
        tool(&mut server, "lumina_set_color", json!({"color": "112233"}));
        assert!(
            server.hold.is_none(),
            "an explicit change supersedes the hold"
        );
    }

    fn body(result: &Value) -> &Value {
        &result["structuredContent"]
    }

    fn working(server: &mut Server) {
        let result = tool(server, "lumina_agent_state", json!({"state": "working"}));
        assert_eq!(result["isError"], false, "{result}");
    }

    #[test]
    fn gpu_only_override_keeps_the_motherboard_baseline() {
        let mut server = Server::new(true);
        working(&mut server);
        let result = tool(
            &mut server,
            "lumina_set_color",
            json!({"target": "gpu", "color": "112233"}),
        );
        assert_eq!(body(&result)["agent_hold_released"], json!(["gpu"]));
        assert!(server.hold.as_ref().unwrap().gpu.is_none());
        assert!(server.hold.as_ref().unwrap().mobo.is_some());
        let idle = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(body(&idle)["restored_devices"], json!(["motherboard"]));
        assert_eq!(body(&idle)["not_enrolled"], json!(["gpu"]));
        assert_eq!(
            server.restore_calls,
            ["motherboard"],
            "GPU manual change is never rewritten"
        );
        assert!(server.hold.is_none());
    }

    #[test]
    fn motherboard_only_override_keeps_the_gpu_baseline() {
        let mut server = Server::new(true);
        working(&mut server);
        tool(
            &mut server,
            "lumina_set_color",
            json!({"target": "mobo", "color": "112233"}),
        );
        assert!(server.hold.as_ref().unwrap().gpu.is_some());
        assert!(server.hold.as_ref().unwrap().mobo.is_none());
        let idle = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(body(&idle)["restored_devices"], json!(["gpu"]));
        assert_eq!(server.restore_calls, ["gpu"]);
    }

    #[test]
    fn one_sided_manual_failure_does_not_roll_back_the_successful_side() {
        let mut server = Server::new(true);
        working(&mut server);
        server.faults.push(("apply", "motherboard"));
        let result = tool(&mut server, "lumina_set_color", json!({"color": "112233"}));
        assert_eq!(result["isError"], true);
        assert!(body(&result)["applied"]["gpu"].is_object());
        assert!(body(&result)["errors"]["motherboard"].is_string());
        assert!(
            server.hold.as_ref().unwrap().gpu.is_none(),
            "GPU change is final"
        );
        assert!(
            server.hold.as_ref().unwrap().mobo.is_some(),
            "failed side keeps its recovery state"
        );
        server.faults.clear();
        server.expire_if_due(server.next_deadline().unwrap());
        assert_eq!(server.restore_calls, ["motherboard"]);
        assert!(server.hold.is_none());
    }

    #[test]
    fn restore_failure_on_one_device_never_replays_the_other() {
        let mut server = Server::new(true);
        working(&mut server);
        server.faults.push(("restore", "motherboard"));
        let idle = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(idle["isError"], true);
        assert_eq!(body(&idle)["restored_devices"], json!(["gpu"]));
        let hold = server.hold.as_ref().unwrap();
        assert!(hold.gpu.is_none() && hold.mobo.as_ref().unwrap().attempts == 1);
        assert!(hold.deadline > Instant::now() + Duration::from_secs(4));
        assert_eq!(server.hold_status()["state"], "restoring");
        assert_eq!(
            server.hold_status()["baseline_devices"],
            json!(["motherboard"])
        );

        server.expire_if_due(Instant::now()); // retry not due yet
        assert_eq!(server.restore_calls, ["gpu", "motherboard"]);
        server.faults.clear();
        server.expire_if_due(server.next_deadline().unwrap());
        assert_eq!(server.restore_calls, ["gpu", "motherboard", "motherboard"]);
        assert!(server.hold.is_none());
    }

    #[test]
    fn restore_gives_up_per_device_after_three_attempts() {
        let mut server = Server::new(true);
        working(&mut server);
        server.faults.push(("restore", "gpu"));
        for attempt in 1..=3u8 {
            let report = server.restore_baseline();
            assert_eq!(report.failed.len(), 1);
            if attempt < 3 {
                assert_eq!(
                    server.hold.as_ref().unwrap().gpu.as_ref().unwrap().attempts,
                    attempt
                );
            }
        }
        assert!(server.hold.is_none());
    }

    #[test]
    fn device_without_a_saved_baseline_is_never_mutated() {
        let mut server = Server::new(true);
        server.faults.push(("snapshot", "gpu"));
        let result = tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "working"}),
        );
        assert_eq!(result["isError"], false);
        assert!(
            body(&result)["applied"].get("gpu").is_none(),
            "GPU must not be written"
        );
        assert!(body(&result)["applied"]["motherboard"].is_object());
        assert!(body(&result)["warnings"][0]
            .as_str()
            .unwrap()
            .contains("gpu excluded"));
        assert_eq!(body(&result)["baseline_devices"], json!(["motherboard"]));
        assert!(server.hold.as_ref().unwrap().gpu.is_none());

        // The device recovers: it is enrolled (baseline first) on the next state change.
        server.faults.clear();
        let next = tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "reviewing"}),
        );
        assert!(body(&next)["applied"]["gpu"].is_object());
        assert_eq!(
            body(&next)["baseline_devices"],
            json!(["gpu", "motherboard"])
        );
        let idle = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(
            body(&idle)["restored_devices"],
            json!(["gpu", "motherboard"])
        );
    }

    #[test]
    fn no_recoverable_device_fails_the_call_and_keeps_existing_holds() {
        let mut server = Server::new(true);
        server
            .faults
            .extend([("snapshot", "gpu"), ("snapshot", "motherboard")]);
        let result = tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "working"}),
        );
        assert_eq!(result["isError"], true);
        assert!(server.hold.is_none());
        let idle = tool(&mut server, "lumina_agent_state", json!({"state": "idle"}));
        assert_eq!(
            body(&idle)["baseline_restored"],
            false,
            "an empty baseline claims nothing"
        );
        assert_eq!(body(&idle)["restored_devices"], json!([]));

        server.faults.clear();
        working(&mut server);
        server
            .faults
            .extend([("open", "gpu"), ("open", "motherboard")]);
        let again = tool(
            &mut server,
            "lumina_agent_state",
            json!({"state": "success"}),
        );
        assert_eq!(again["isError"], true);
        assert!(
            server.hold.as_ref().unwrap().gpu.is_some(),
            "existing baselines survive"
        );
        assert_eq!(server.hold.as_ref().unwrap().state, "working");
    }

    struct TestSink {
        written: Arc<Mutex<Vec<u8>>>,
        delay: Duration,
        fail_on_flush: Option<usize>,
        flushes: usize,
        block: bool,
    }

    impl Write for TestSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.block {
                std::thread::sleep(Duration::from_secs(3600));
            }
            std::thread::sleep(self.delay);
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_on_flush == Some(self.flushes) {
                return Err(io::Error::other("late sink failure"));
            }
            Ok(())
        }
    }

    fn sink(
        delay_ms: u64,
        fail_on_flush: Option<usize>,
        block: bool,
    ) -> (TestSink, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = TestSink {
            written: Arc::clone(&written),
            delay: Duration::from_millis(delay_ms),
            fail_on_flush,
            flushes: 0,
            block,
        };
        (sink, written)
    }

    #[test]
    fn output_finish_drains_a_slow_sink_in_order() {
        let (slow, written) = sink(5, None, false);
        let mut out = Output::spawn(slow);
        for id in 0..40 {
            out.send(&json!({"id": id})).unwrap();
        }
        out.finish(Duration::from_secs(5)).unwrap();
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        let ids: Vec<u64> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["id"]
                    .as_u64()
                    .unwrap()
            })
            .collect();
        assert_eq!(ids, (0..40).collect::<Vec<_>>());
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn output_finish_reports_a_late_write_failure() {
        let (failing, _written) = sink(2, Some(3), false);
        let mut out = Output::spawn(failing);
        for id in 0..10 {
            let _ = out.send(&json!({"id": id})); // later sends may already fail
        }
        let error = out.finish(Duration::from_secs(5)).unwrap_err();
        assert!(error.contains("late sink failure"), "{error}");
    }

    #[test]
    fn output_finish_never_blocks_on_an_unreadable_sink() {
        let (stuck, _written) = sink(0, None, true);
        let mut out = Output::spawn(stuck);
        out.send(&json!({"id": 1})).unwrap();
        let started = Instant::now();
        let error = out.finish(Duration::from_millis(150)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(error.contains("not delivered"), "{error}");
    }

    #[test]
    fn legacy_aura_names_alias_the_lumina_tools_and_resources() {
        let mut server = Server::new(true);
        let legacy = tool(&mut server, "aura_get_status", json!({}));
        let canonical = tool(&mut server, "lumina_get_status", json!({}));
        assert_eq!(legacy["isError"], false);
        assert_eq!(body(&legacy), body(&canonical));
        let off = tool(&mut server, "aura_turn_off", json!({"target": "gpu"}));
        assert_eq!(body(&off)["applied"]["gpu"]["mode"], "off");
        let tools = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        );
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.iter().all(|n| n.starts_with("lumina_")), "{names:?}");
        let read = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": "aura://hardware/topology"}}),
        );
        assert_eq!(
            read["result"]["contents"][0]["uri"],
            "aura://hardware/topology"
        );
        let listed = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"}),
        );
        let uris: Vec<&str> = listed["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert_eq!(
            uris,
            ["lumina://hardware/topology", "lumina://state/current"]
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
        for uri in ["lumina://hardware/topology", "lumina://state/current"] {
            let read = call(
                &mut server,
                json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": uri}}),
            );
            let text = read["result"]["contents"][0]["text"].as_str().unwrap();
            serde_json::from_str::<Value>(text).expect("JSON body");
        }
        let missing = call(
            &mut server,
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "lumina://nope"}}),
        );
        assert_eq!(missing["error"]["code"], -32002);
    }
}
