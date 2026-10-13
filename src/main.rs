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
#[command(about = "ASUS Aura Sync Hardware RGB Controller for Linux")]
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
        #[arg(long, value_enum, default_value = "static")]
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
        #[arg(long, required_unless_present = "off")]
        color: Option<String>,
        #[arg(long, default_value = "static", value_parser = parse_all_mode)]
        mode: String,
        #[arg(long)]
        off: bool,
    },
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
        "==================================================\n  ASUS Aura Hardware Status (Live)\n==================================================\n[GPU]  {}\n       Bus: {} @ {} (Magic 0x1589 Verified)\n       Mode: {}, Color: #{}\n--------------------------------------------------\n[MOBO] {}\n       Super I/O Port: {} @ {} ({} Verified)\n       Mode: {}, Speed: {}, Direction: {}\n",
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
            if color.is_some() && mode != MoboMode::Static && !off {
                eprintln!("warning: --color is ignored for effect modes; motherboard effects keep their stored colors");
            }
            if off {
                mobo_controller(dry)?.set_mode(MoboMode::Off, 0, 0)?;
                Ok("[MOBO] LEDs turned off.".to_owned())
            } else if let Some(color) = color.filter(|_| mode == MoboMode::Static) {
                let (r, g, b) = parse_hex_color(&color)?;
                mobo_controller(dry)?.set_color_static(r, g, b)?;
                Ok(format!(
                    "[MOBO] Static color set to #{}.",
                    color.trim_start_matches('#')
                ))
            } else {
                mobo_controller(dry)?.set_mode(mode, speed, 0)?;
                Ok(format!(
                    "[MOBO] Mode set to {} (Speed: {speed}).",
                    mode.name()
                ))
            }
        }
        Command::All { color, mode, off } => {
            // Validate before opening either device or changing either controller.
            let rgb = if off {
                None
            } else {
                Some(parse_hex_color(
                    color.as_deref().ok_or("--color is required unless --off")?,
                )?)
            };
            let mut gpu = gpu_controller(dry)?;
            let mut mobo = mobo_controller(dry)?;
            if let Some((r, g, b)) = rgb {
                let (gpu_mode, mobo_mode) = mcp::find_effect(&mode)
                    .map(|(gpu, mobo, _)| (gpu.unwrap_or(GpuMode::Static), mobo))
                    .unwrap_or_else(|| {
                        (
                            GpuMode::from_str(&mode, false).unwrap_or(GpuMode::Static),
                            parse_mobo_mode(&mode).unwrap_or(MoboMode::Static),
                        )
                    });
                gpu.set_color(r, g, b, gpu_mode)?;
                let hex = color.as_deref().unwrap_or_default().trim_start_matches('#');
                if mobo_mode == MoboMode::Static {
                    mobo.set_color_static(r, g, b)?;
                } else if mobo_mode.uses_color() {
                    // Writes the effect color buffer and leaves direct mode.
                    mobo.set_effect(mobo_mode, 0, Some((r, g, b)))?;
                } else {
                    mobo.set_effect(mobo_mode, 0, None)?;
                    return Ok(format!(
                        "[ALL] GPU set to #{hex} (Mode: {}); motherboard effect {} generates its own colors.",
                        gpu_mode.name(),
                        mobo_mode.name()
                    ));
                }
                Ok(format!(
                    "[ALL] Synchronized GPU and Motherboard to #{hex} (Mode: {mode})."
                ))
            } else {
                gpu.set_mode(GpuMode::Off)?;
                mobo.set_mode(MoboMode::Off, 0, 0)?;
                Ok("[ALL] GPU and Motherboard LEDs turned off.".to_owned())
            }
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
            vec!["aura", "--dry-run", "--json", "status"],
            vec!["aura", "status", "--json", "--dry-run"],
            vec!["aura", "gpu", "--color", "#FF0055", "--mode", "breathing"],
            vec![
                "aura",
                "mobo",
                "--mode",
                "rainbow_breathing",
                "--speed",
                "4",
            ],
            vec!["aura", "mobo", "--mode", "rainbow-breathing"],
            vec!["aura", "all", "--off"],
            vec!["aura", "all", "--color", "112233", "--mode", "rainbow"],
            vec!["aura", "--dry-run", "mcp"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn rejects_invalid_arguments() {
        for args in [
            vec!["aura"],
            vec!["aura", "all"],
            vec!["aura", "mobo", "--speed", "5"],
            vec!["aura", "mobo", "--speed", "-1"],
            vec!["aura", "gpu", "--mode", "rainbow"],
            vec!["aura", "mobo", "--mode", "invalid"],
            vec!["aura", "all", "--color", "112233", "--mode", "invalid"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn bad_color_is_reported_before_device_access() {
        for cmd in ["gpu", "mobo", "all"] {
            let cli = Cli::try_parse_from(["aura", cmd, "--color", "zzz"]).unwrap();
            assert!(run(cli).unwrap_err().contains("6 hexadecimal digits"));
        }
    }

    #[test]
    fn json_status_is_clean_and_human_status_has_zones() {
        let cli = Cli::try_parse_from(["aura", "--dry-run", "status", "--json"]).unwrap();
        let data: serde_json::Value = serde_json::from_str(&run(cli).unwrap()).unwrap();
        assert_eq!(data["gpu"]["magic_verified"], true);
        assert_eq!(data["motherboard"]["led_count"], 5);
        let cli = Cli::try_parse_from(["aura", "--dry-run", "status"]).unwrap();
        let text = run(cli).unwrap();
        assert!(text.contains("ASUS Aura Hardware Status (Live)"));
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
                "[ALL] GPU set to #112233 (Mode: static); motherboard effect rainbow generates its own colors.",
            ),
            (
                vec!["all", "--color", "112233", "--mode", "rainbow_breathing"],
                "[ALL] GPU set to #112233 (Mode: static); motherboard effect rainbow_breathing generates its own colors.",
            ),
        ] {
            let cli = Cli::try_parse_from(["aura", "--dry-run"].into_iter().chain(args)).unwrap();
            assert_eq!(run(cli).unwrap(), expected);
        }
    }
}
