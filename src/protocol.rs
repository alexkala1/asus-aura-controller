//! Hardware-free constants and conversions for ASUS Aura controllers.

use std::sync::atomic::{AtomicBool, Ordering};

static DRY_RUN_LOG_TO_STDERR: AtomicBool = AtomicBool::new(false);

/// MCP owns stdout for JSON-RPC, so dry-run write traces must move to stderr.
pub fn route_dry_run_log_to_stderr() {
    crate::diag::init();
    DRY_RUN_LOG_TO_STDERR.store(true, Ordering::Relaxed);
}

pub fn dry_run_log(line: &str) -> std::io::Result<()> {
    use std::io::Write;
    if DRY_RUN_LOG_TO_STDERR.load(Ordering::Relaxed) {
        crate::diag::log(line);
        Ok(())
    } else {
        writeln!(std::io::stdout().lock(), "{line}")
    }
}

pub const GPU_I2C_DEV: &str = "/dev/i2c-0";
pub const GPU_I2C_ADDR: u8 = 0x29;
pub const GPU_MAGIC_HIGH: u8 = 0x15;
pub const GPU_MAGIC_LOW: u8 = 0x89;
pub const GPU_REG_RED: u8 = 0x04;
pub const GPU_REG_GREEN: u8 = 0x05;
pub const GPU_REG_BLUE: u8 = 0x06;
pub const GPU_REG_MODE: u8 = 0x07;
pub const GPU_REG_APPLY: u8 = 0x0E;
pub const GPU_REG_MAGIC_H: u8 = 0x20;
pub const GPU_REG_MAGIC_L: u8 = 0x21;

pub const MOBO_PORT_BASE: u16 = 0x02c0;
pub const MOBO_SLAVE_ADDR: u8 = 0x4e;
pub const MOBO_EXPECTED_NAME: &str = "LED-0116";
pub const MOBO_REG_NAME: u16 = 0x1000;
pub const MOBO_REG_CONFIG: u16 = 0x1C00;
pub const MOBO_REG_COLORS_DIRECT: u16 = 0x8000;
pub const MOBO_REG_COLORS_EFFECT: u16 = 0x8010;
pub const MOBO_REG_DIRECT: u16 = 0x8020;
pub const MOBO_REG_MODE: u16 = 0x8021;
pub const MOBO_REG_SPEED: u16 = 0x8022;
pub const MOBO_REG_DIRECTION: u16 = 0x8023;
pub const MOBO_REG_APPLY: u16 = 0x80A0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[repr(u8)]
pub enum GpuMode {
    Off = 0,
    Static = 1,
    Breathing = 2,
    Flashing = 3,
    Spectrum = 4,
}

impl GpuMode {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Off),
            1 => Some(Self::Static),
            2 => Some(Self::Breathing),
            3 => Some(Self::Flashing),
            4 => Some(Self::Spectrum),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Static => "static",
            Self::Breathing => "breathing",
            Self::Flashing => "flashing",
            Self::Spectrum => "spectrum",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
#[repr(u8)]
pub enum MoboMode {
    Off = 0,
    Static = 1,
    Breathing = 2,
    Flashing = 3,
    Spectrum = 4,
    Rainbow = 5,
    RainbowBreathing = 6,
    ChaseFade = 7,
    SpectrumChaseFade = 8,
    Chase = 9,
    SpectrumChase = 10,
    Wave = 11,
    RainbowPulse = 12,
    RandomFlicker = 13,
}

impl MoboMode {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Off),
            1 => Some(Self::Static),
            2 => Some(Self::Breathing),
            3 => Some(Self::Flashing),
            4 => Some(Self::Spectrum),
            5 => Some(Self::Rainbow),
            6 => Some(Self::RainbowBreathing),
            7 => Some(Self::ChaseFade),
            8 => Some(Self::SpectrumChaseFade),
            9 => Some(Self::Chase),
            10 => Some(Self::SpectrumChase),
            11 => Some(Self::Wave),
            12 => Some(Self::RainbowPulse),
            13 => Some(Self::RandomFlicker),
            _ => None,
        }
    }

    /// Whether the effect draws from the configured color buffer (REA mode table).
    pub fn uses_color(self) -> bool {
        matches!(
            self,
            Self::Static | Self::Breathing | Self::Flashing | Self::ChaseFade | Self::Chase
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Static => "static",
            Self::Breathing => "breathing",
            Self::Flashing => "flashing",
            Self::Spectrum => "spectrum",
            Self::Rainbow => "rainbow",
            Self::RainbowBreathing => "rainbow_breathing",
            Self::ChaseFade => "chase_fade",
            Self::SpectrumChaseFade => "spectrum_chase_fade",
            Self::Chase => "chase",
            Self::SpectrumChase => "spectrum_chase",
            Self::Wave => "wave",
            Self::RainbowPulse => "rainbow_pulse",
            Self::RandomFlicker => "random_flicker",
        }
    }
}

pub fn parse_hex_color(s: &str) -> Result<(u8, u8, u8), String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    // Validate bytes before slicing, so non-ASCII input cannot split a code point.
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "Color '{s}' must contain exactly 6 hexadecimal digits (e.g. ff00aa)"
        ));
    }
    let component =
        |start| u8::from_str_radix(&hex[start..start + 2], 16).map_err(|error| error.to_string());
    Ok((component(0)?, component(2)?, component(4)?))
}

/// ENE color buffers store red, blue, green rather than RGB.
pub fn pack_mobo_color(r: u8, g: u8, b: u8) -> [u8; 3] {
    [r, b, g]
}

pub fn channel_name(channel: u8) -> String {
    match channel {
        0x84 => "Audio".to_owned(),
        0x85 => "Back I/O".to_owned(),
        0x86 => "RGB Header 1".to_owned(),
        0x87 => "RGB Header 2".to_owned(),
        _ => format!("Unknown(0x{channel:02x})"),
    }
}

pub fn swap_reg16(reg: u16) -> u16 {
    reg.swap_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn color_parse_valid() {
        for color in ["ff00aa", "#ff00aa", "FF00AA", "#FF00AA"] {
            assert_eq!(parse_hex_color(color), Ok((0xff, 0x00, 0xaa)));
        }
        assert_eq!(parse_hex_color("112233"), Ok((0x11, 0x22, 0x33)));
    }

    #[test]
    fn color_parse_invalid() {
        for color in [
            "", "#", "12345", "1234567", "gg00aa", "##ff00aa", " 112233", "é1234", "123é4", "💡12",
            "#é1234",
        ] {
            assert!(parse_hex_color(color).is_err(), "accepted {color:?}");
        }
    }

    #[test]
    fn mobo_color_order() {
        assert_eq!(pack_mobo_color(0x11, 0x22, 0x33), [0x11, 0x33, 0x22]);
    }

    #[test]
    fn gpu_modes_round_trip() {
        let names = ["off", "static", "breathing", "flashing", "spectrum"];
        for (code, mode) in GpuMode::value_variants().iter().copied().enumerate() {
            assert_eq!(mode.code(), code as u8);
            assert_eq!(GpuMode::from_code(mode.code()), Some(mode));
            assert_eq!(mode.name(), names[code]);
            assert_eq!(GpuMode::from_str(mode.name(), false), Ok(mode));
        }
        for code in 5..=u8::MAX {
            assert_eq!(GpuMode::from_code(code), None);
        }
    }

    #[test]
    fn mobo_modes_round_trip() {
        let names = [
            "off",
            "static",
            "breathing",
            "flashing",
            "spectrum",
            "rainbow",
            "rainbow_breathing",
            "chase_fade",
            "spectrum_chase_fade",
            "chase",
            "spectrum_chase",
            "wave",
            "rainbow_pulse",
            "random_flicker",
        ];
        for (code, mode) in MoboMode::value_variants().iter().copied().enumerate() {
            assert_eq!(mode.code(), code as u8);
            assert_eq!(MoboMode::from_code(mode.code()), Some(mode));
            assert_eq!(mode.name(), names[code]);
            assert_eq!(
                MoboMode::from_str(&mode.name().replace('_', "-"), false),
                Ok(mode)
            );
        }
        for code in 14..=u8::MAX {
            assert_eq!(MoboMode::from_code(code), None);
        }
    }

    #[test]
    fn register_byte_swap() {
        assert_eq!(swap_reg16(0x1000), 0x0010);
        assert_eq!(swap_reg16(0x80A0), 0xA080);
        assert_eq!(swap_reg16(swap_reg16(0x1234)), 0x1234);
    }

    #[test]
    fn channel_names() {
        assert_eq!(channel_name(0x84), "Audio");
        assert_eq!(channel_name(0x85), "Back I/O");
        assert_eq!(channel_name(0x86), "RGB Header 1");
        assert_eq!(channel_name(0x87), "RGB Header 2");
        assert_eq!(channel_name(0x00), "Unknown(0x00)");
        assert_eq!(channel_name(0xff), "Unknown(0xff)");
    }
}
