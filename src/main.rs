mod diag;
mod gpu;
mod hardware_lock;
mod mcp;
mod mobo;
mod protocol;

use clap::{Parser, Subcommand, ValueEnum};
use gpu::{DryRunI2c, GpuController, GpuStatus, I2cBus, LinuxI2c};
use mobo::{DevPort, DryRunPort, MoboController, MoboStatus, PortIo};
use protocol::{parse_hex_color, GpuMode, MoboMode, GPU_I2C_DEV};
use std::io::{self, Write};

#[derive(Debug, Parser)]
#[command(
    name = "luminactl",
    about = "Pure Linux RGB lighting controller — reverse-engineered for ASUS Aura Sync hardware"
)]
pub struct Cli {
    #[arg(long, global = true)]
    dry_run: bool,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve lighting tools to coding agents over MCP (JSON-RPC on stdio).
    Mcp,
    Status,
    Gpu {
        #[arg(long)]
        color: Option<String>,
        #[arg(long, default_value = "static", value_parser = parse_gpu_mode)]
        mode: GpuMode,
        #[arg(long)]
        off: bool,
    },
    Mobo {
        #[arg(long)]
        color: Option<String>,
        #[arg(long, default_value = "static", value_parser = parse_mobo_mode)]
        mode: MoboMode,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=4))]
        speed: u8,
        #[arg(long)]
        off: bool,
    },
    All {
        /// Required for static mode; optional for effects (autonomous ones ignore it).
        #[arg(long)]
        color: Option<String>,
        #[arg(long, default_value = "static", value_parser = parse_all_mode)]
        mode: String,
        #[arg(long)]
        off: bool,
    },
}

/// GPU modes by name, plus the cross-device effect names that map onto the GPU's
/// built-in color cycle: its only autonomous effect.
fn parse_gpu_mode(name: &str) -> Result<GpuMode, String> {
    let normalized = name.replace('_', "-");
    match normalized.as_str() {
        "rainbow" | "color-cycle" => Ok(GpuMode::Spectrum),
        _ => GpuMode::from_str(&normalized, false),
    }
}

/// Resolve an `all --mode` name to the GPU and motherboard modes it selects.
fn resolve_all_mode(mode: &str) -> (GpuMode, MoboMode) {
    mcp::find_effect(mode)
        .map(|(gpu, mobo, _)| (gpu.unwrap_or(GpuMode::Static), mobo))
        .unwrap_or_else(|| {
            (
                GpuMode::from_str(mode, false).unwrap_or(GpuMode::Static),
                parse_mobo_mode(mode).unwrap_or(MoboMode::Static),
            )
        })
}

fn parse_mobo_mode(name: &str) -> Result<MoboMode, String> {
    MoboMode::from_str(&name.replace('_', "-"), false)
}

fn parse_all_mode(name: &str) -> Result<String, String> {
    let normalized = name.replace('_', "-");
    if mcp::find_effect(&normalized).is_some()
        || GpuMode::from_str(&normalized, false).is_ok()
        || parse_mobo_mode(&normalized).is_ok()
    {
        Ok(normalized)
    } else {
        Err(format!("invalid mode '{name}'"))
    }
}

// Trait objects select the real or dry-run transport without duplicating dispatch.
impl I2cBus for Box<dyn I2cBus> {
    fn read_reg(&mut self, reg: u8) -> io::Result<u8> {
        (**self).read_reg(reg)
    }
    fn write_reg(&mut self, reg: u8, val: u8) -> io::Result<()> {
        (**self).write_reg(reg, val)
    }
}

impl PortIo for Box<dyn PortIo> {
    fn inb(&mut self, port: u16) -> io::Result<u8> {
        (**self).inb(port)
    }
    fn outb(&mut self, port: u16, val: u8) -> io::Result<()> {
        (**self).outb(port, val)
    }
}

fn gpu_controller(dry_run: bool) -> Result<GpuController<Box<dyn I2cBus>>, String> {
    let bus: Box<dyn I2cBus> = if dry_run {
        Box::new(DryRunI2c)
    } else {
        Box::new(LinuxI2c::open(GPU_I2C_DEV).map_err(|e| e.to_string())?)
    };
    Ok(GpuController::new(bus, GPU_I2C_DEV))
}

fn mobo_controller(dry_run: bool) -> Result<MoboController<Box<dyn PortIo>>, String> {
    let port: Box<dyn PortIo> = if dry_run {
        Box::new(DryRunPort)
    } else {
        Box::new(DevPort::open().map_err(|e| e.to_string())?)
    };
    Ok(MoboController::new(port, dry_run))
}

fn human_status(gpu: &GpuStatus, mobo: &MoboStatus) -> String {
    let mut text = format!(
        "==================================================\n  Lumina Hardware Status (Live)\n==================================================\n[GPU]  {}\n       Bus: {} @ {} (Magic 0x1589 Verified)\n       Mode: {}, Color: #{}\n--------------------------------------------------\n[MOBO] {}\n       Super I/O Port: {} @ {} ({} Verified)\n       Mode: {}, Speed: {}, Direction: {}\n",
        gpu.device, gpu.bus, gpu.address, gpu.mode, gpu.color_hex, mobo.device,
        mobo.smbus_port, mobo.slave_address, mobo.firmware, mobo.mode, mobo.speed, mobo.direction
    );
    for zone in &mobo.zones {
        text.push_str(&format!(
            "       - LED {} ({}): #{}\n",
            zone.led_index, zone.channel_name, zone.hex
        ));
    }
    text.push_str("==================================================");
    text
}

pub fn run(cli: Cli) -> Result<String, String> {
    let dry = cli.dry_run;
    match cli.command {
        Command::Mcp => Err("`mcp` runs as a server; start it from main".to_owned()),
        Command::Status => {
            let gpu = gpu_controller(dry)?.status()?;
            let mobo = mobo_controller(dry)?.status()?;
            if cli.json {
                serde_json::to_string_pretty(&serde_json::json!({"gpu": gpu, "motherboard": mobo}))
                    .map_err(|e| e.to_string())
            } else {
                Ok(human_status(&gpu, &mobo))
            }
        }
        Command::Gpu { color, mode, off } => {
            if off {
                gpu_controller(dry)?.set_mode(GpuMode::Off)?;
                Ok("[GPU] LEDs turned off.".to_owned())
            } else if let Some(color) = color {
                let (r, g, b) = parse_hex_color(&color)?;
                gpu_controller(dry)?.set_color(r, g, b, mode)?;
                Ok(format!(
                    "[GPU] Color set to #{} (Mode: {}).",
                    color.trim_start_matches('#'),
                    mode.name()
                ))
            } else {
                gpu_controller(dry)?.set_mode(mode)?;
                Ok(format!("[GPU] Mode set to {}.", mode.name()))
            }
        }
        Command::Mobo {
            color,
            mode,
            speed,
            off,
        } => {
            if off {
                mobo_controller(dry)?.set_mode(MoboMode::Off, 0, 0)?;
                return Ok("[MOBO] LEDs turned off.".to_owned());
            }
            let rgb = color.as_deref().map(parse_hex_color).transpose()?;
            let hex = color.as_deref().unwrap_or_default().trim_start_matches('#');
            match rgb {
                Some((r, g, b)) if mode == MoboMode::Static => {
                    mobo_controller(dry)?.set_color_static(r, g, b)?;
                    Ok(format!("[MOBO] Static color set to #{hex}."))
                }
                Some(rgb) if mode.uses_color() => {
                    // Color-driven effects take their color from the effect buffer.
                    mobo_controller(dry)?.set_effect(mode, speed, Some(rgb))?;
                    Ok(format!(
                        "[MOBO] Mode set to {} with color #{hex} (Speed: {speed}).",
                        mode.name()
                    ))
                }
                _ => {
                    if rgb.is_some() {
                        eprintln!("warning: --color is ignored for effect modes that generate their own colors");
                    }
                    mobo_controller(dry)?.set_mode(mode, speed, 0)?;
                    Ok(format!(
                        "[MOBO] Mode set to {} (Speed: {speed}).",
                        mode.name()
                    ))
                }
            }
        }
        Command::All { color, mode, off } => {
            // Validate before opening either device or changing either controller.
            let rgb = color.as_deref().map(parse_hex_color).transpose()?;
            let (gpu_mode, mobo_mode) = resolve_all_mode(&mode);
            if !off && rgb.is_none() && mobo_mode == MoboMode::Static {
                return Err("--color is required for static mode".to_owned());
            }
            let mut gpu = gpu_controller(dry)?;
            let mut mobo = mobo_controller(dry)?;
            if off {
                gpu.set_mode(GpuMode::Off)?;
                mobo.set_mode(MoboMode::Off, 0, 0)?;
                return Ok("[ALL] GPU and Motherboard LEDs turned off.".to_owned());
            }
            let hex = color.as_deref().unwrap_or_default().trim_start_matches('#');
            match rgb {
                Some((r, g, b)) => gpu.set_color(r, g, b, gpu_mode)?,
                None => gpu.set_mode(gpu_mode)?,
            }
            match (rgb, mobo_mode) {
                (Some((r, g, b)), MoboMode::Static) => mobo.set_color_static(r, g, b)?,
                (Some(rgb), m) if m.uses_color() => mobo.set_effect(m, 0, Some(rgb))?,
                (_, m) => mobo.set_effect(m, 0, None)?, // autonomous, or keep stored colors
            }
            Ok(match rgb {
                Some(_) if mobo_mode == MoboMode::Static || mobo_mode.uses_color() => {
                    format!("[ALL] Synchronized GPU and Motherboard to #{hex} (Mode: {mode}).")
                }
                Some(_) => format!(
                    "[ALL] GPU set to #{hex} (Mode: {}); motherboard effect {} generates its own colors.",
                    gpu_mode.name(),
                    mobo_mode.name()
                ),
                None => format!(
                    "[ALL] GPU mode {}; motherboard mode {} (colors unchanged).",
                    gpu_mode.name(),
                    mobo_mode.name()
                ),
            })
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Mcp) {
        if let Err(message) = mcp::serve(cli.dry_run) {
            let _ = writeln!(io::stderr().lock(), "error: {message}");
            std::process::exit(1);
        }
        return;
    }
    let result = run(cli)
        .and_then(|text| writeln!(io::stdout().lock(), "{text}").map_err(|e| e.to_string()));
    if let Err(message) = result {
        let _ = writeln!(io::stderr().lock(), "error: {message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn accepts_commands_flags_and_mode_spellings() {
        for args in [
            vec!["luminactl", "--dry-run", "--json", "status"],
            vec!["luminactl", "status", "--json", "--dry-run"],
            vec![
                "luminactl",
                "gpu",
                "--color",
                "#FF0055",
                "--mode",
                "breathing",
            ],
            vec![
                "luminactl",
                "mobo",
                "--mode",
                "rainbow_breathing",
                "--speed",
                "4",
            ],
            vec!["luminactl", "mobo", "--mode", "rainbow-breathing"],
            vec!["luminactl", "all", "--off"],
            vec!["luminactl", "all", "--color", "112233", "--mode", "rainbow"],
            vec!["luminactl", "--dry-run", "mcp"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn color_is_required_only_for_static_all() {
        // Static needs a color; this is reported before any device is opened.
        let cli = Cli::try_parse_from(["luminactl", "--dry-run", "all"]).unwrap();
        assert_eq!(run(cli).unwrap_err(), "--color is required for static mode");
        let cli =
            Cli::try_parse_from(["luminactl", "--dry-run", "all", "--mode", "static"]).unwrap();
        assert!(run(cli).is_err());
        // Effects (autonomous or color-driven) and off need none.
        for mode in [
            "rainbow",
            "rainbow-breathing",
            "spectrum",
            "wave",
            "rainbow-pulse",
            "random-flicker",
            "starry-night",
            "breathing",
            "flashing",
            "off",
        ] {
            let cli =
                Cli::try_parse_from(["luminactl", "--dry-run", "all", "--mode", mode]).unwrap();
            assert!(run(cli).is_ok(), "{mode}");
        }
        let cli = Cli::try_parse_from(["luminactl", "--dry-run", "all", "--off"]).unwrap();
        assert!(run(cli).is_ok());
    }

    #[test]
    fn gpu_accepts_rainbow_and_color_cycle_as_spectrum() {
        for mode in ["rainbow", "color-cycle", "color_cycle", "spectrum"] {
            let cli =
                Cli::try_parse_from(["luminactl", "--dry-run", "gpu", "--mode", mode]).unwrap();
            assert_eq!(run(cli).unwrap(), "[GPU] Mode set to spectrum.", "{mode}");
        }
    }

    #[test]
    fn rejects_invalid_arguments() {
        for args in [
            vec!["luminactl"],
            vec!["luminactl", "mobo", "--speed", "5"],
            vec!["luminactl", "mobo", "--speed", "-1"],
            vec!["luminactl", "gpu", "--mode", "wave"],
            vec!["luminactl", "mobo", "--mode", "invalid"],
            vec!["luminactl", "all", "--color", "112233", "--mode", "invalid"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn bad_color_is_reported_before_device_access() {
        for cmd in ["gpu", "mobo", "all"] {
            let cli = Cli::try_parse_from(["luminactl", cmd, "--color", "zzz"]).unwrap();
            assert!(run(cli).unwrap_err().contains("6 hexadecimal digits"));
        }
    }

    #[test]
    fn json_status_is_clean_and_human_status_has_zones() {
        let cli = Cli::try_parse_from(["luminactl", "--dry-run", "status", "--json"]).unwrap();
        let data: serde_json::Value = serde_json::from_str(&run(cli).unwrap()).unwrap();
        assert_eq!(data["gpu"]["magic_verified"], true);
        assert_eq!(data["motherboard"]["led_count"], 5);
        let cli = Cli::try_parse_from(["luminactl", "--dry-run", "status"]).unwrap();
        let text = run(cli).unwrap();
        assert!(text.contains("Lumina Hardware Status (Live)"));
        assert!(text.contains("LED 4 (RGB Header 1): #000000"));
    }

    #[test]
    fn dispatches_dry_run_actions_and_off_precedence() {
        for (args, expected) in [
            (
                vec!["gpu", "--color", "ff0055"],
                "[GPU] Color set to #ff0055 (Mode: static).",
            ),
            (
                vec!["gpu", "--mode", "breathing"],
                "[GPU] Mode set to breathing.",
            ),
            (
                vec!["gpu", "--off", "--color", "zzz"],
                "[GPU] LEDs turned off.",
            ),
            (
                vec!["mobo", "--color", "112233"],
                "[MOBO] Static color set to #112233.",
            ),
            (
                vec!["mobo", "--mode", "wave", "--speed", "4"],
                "[MOBO] Mode set to wave (Speed: 4).",
            ),
            (vec!["mobo", "--off"], "[MOBO] LEDs turned off."),
            (
                vec!["all", "--off"],
                "[ALL] GPU and Motherboard LEDs turned off.",
            ),
            (
                vec!["all", "--color", "112233", "--mode", "rainbow"],
                "[ALL] GPU set to #112233 (Mode: spectrum); motherboard effect rainbow generates its own colors.",
            ),
            (
                vec!["all", "--color", "112233", "--mode", "rainbow_breathing"],
                "[ALL] GPU set to #112233 (Mode: spectrum); motherboard effect rainbow_breathing generates its own colors.",
            ),
        ] {
            let cli = Cli::try_parse_from(["luminactl", "--dry-run"].into_iter().chain(args)).unwrap();
            assert_eq!(run(cli).unwrap(), expected);
        }
    }
}
