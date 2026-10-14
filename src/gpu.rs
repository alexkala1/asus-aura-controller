//! GPU Aura transport and controller. Hardware access is isolated behind `I2cBus`.

use std::ffi::c_ulong;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;

use crate::protocol::{
    GpuMode, GPU_I2C_ADDR, GPU_MAGIC_HIGH, GPU_MAGIC_LOW, GPU_REG_BLUE, GPU_REG_GREEN,
    GPU_REG_MAGIC_H, GPU_REG_MAGIC_L, GPU_REG_MODE, GPU_REG_RED,
};

const I2C_SLAVE: c_ulong = 0x0703;
const I2C_SMBUS: c_ulong = 0x0720;
const I2C_SMBUS_WRITE: u8 = 0;
const I2C_SMBUS_READ: u8 = 1;
const I2C_SMBUS_BYTE_DATA: u32 = 2;
const SIGNATURE_MISMATCH: &str = "GPU Aura signature mismatch (expected 0x1589). Safety abort.";

extern "C" {
    fn ioctl(fd: i32, request: c_ulong, ...) -> i32;
}

#[repr(C)]
struct I2cSmbusIoctlData {
    read_write: u8,
    command: u8,
    size: u32,
    data: *mut u8,
}

// Linux's i2c_smbus_data union occupies 34 bytes and includes a u16 member.
#[repr(C, align(2))]
struct SmbusBuffer([u8; 34]);

pub trait I2cBus {
    fn read_reg(&mut self, reg: u8) -> io::Result<u8>;
    fn write_reg(&mut self, reg: u8, val: u8) -> io::Result<()>;
}

pub struct LinuxI2c {
    file: File,
    path: String,
}

fn device_error(path: &str, error: io::Error) -> io::Error {
    let message = match error.kind() {
        io::ErrorKind::PermissionDenied => format!(
            "Permission denied accessing '{path}'. Run with sudo or add user to group 'i2c'. ({error})"
        ),
        io::ErrorKind::NotFound => format!(
            "I2C device '{path}' not found. Ensure the i2c-dev module is loaded. ({error})"
        ),
        _ => format!("I2C device '{path}': {error}"),
    };
    io::Error::new(error.kind(), message)
}

impl LinuxI2c {
    pub fn open(path: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| device_error(path, error))?;
        crate::hardware_lock::acquire(&file, path)?;
        // SAFETY: file owns a live fd; I2C_SLAVE takes an integer address.
        let result = unsafe { ioctl(file.as_raw_fd(), I2C_SLAVE, GPU_I2C_ADDR as c_ulong) };
        if result < 0 {
            return Err(device_error(path, io::Error::last_os_error()));
        }
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }

    fn transfer(&mut self, reg: u8, read_write: u8, val: u8) -> io::Result<u8> {
        let mut buffer = SmbusBuffer([0; 34]);
        buffer.0[0] = val;
        let mut message = I2cSmbusIoctlData {
            read_write,
            command: reg,
            size: I2C_SMBUS_BYTE_DATA,
            data: buffer.0.as_mut_ptr(),
        };
        // SAFETY: message has the Linux C ABI layout. Both it and its aligned,
        // initialized union buffer remain live for this synchronous ioctl.
        let result = unsafe {
            ioctl(
                self.file.as_raw_fd(),
                I2C_SMBUS,
                &mut message as *mut I2cSmbusIoctlData,
            )
        };
        if result < 0 {
            return Err(device_error(&self.path, io::Error::last_os_error()));
        }
        Ok(buffer.0[0])
    }
}

impl I2cBus for LinuxI2c {
    fn read_reg(&mut self, reg: u8) -> io::Result<u8> {
        self.transfer(reg, I2C_SMBUS_READ, 0)
    }

    fn write_reg(&mut self, reg: u8, val: u8) -> io::Result<()> {
        self.transfer(reg, I2C_SMBUS_WRITE, val).map(|_| ())
    }
}

#[derive(Default)]
pub struct DryRunI2c;

impl I2cBus for DryRunI2c {
    fn read_reg(&mut self, reg: u8) -> io::Result<u8> {
        Ok(match reg {
            GPU_REG_MAGIC_H => GPU_MAGIC_HIGH,
            GPU_REG_MAGIC_L => GPU_MAGIC_LOW,
            _ => 0,
        })
    }

    fn write_reg(&mut self, reg: u8, val: u8) -> io::Result<()> {
        crate::protocol::dry_run_log(&format!(
            "[DRY-RUN] GPU Write: Reg 0x{reg:02x} <= 0x{val:02x}"
        ))
    }
}

#[derive(Debug, serde::Serialize)]
pub struct GpuStatus {
    pub device: String,
    pub bus: String,
    pub address: String,
    pub magic_verified: bool,
    pub mode: String,
    /// Spectrum generates colors internally; RGB registers are not animation samples.
    pub color_source: String,
    pub color_configurable: bool,
    pub color_hex: String,
    pub rgb: [u8; 3],
    /// Effects the GP106 registers can run, in the Studio's vocabulary.
    pub supported_modes: Vec<&'static str>,
}

/// The GPU has one zone and four hardware effects (registers 0x07 = 1..=4) plus off.
/// Spectrum is its color cycle, listed as `rainbow`; multi-zone effects do not exist here.
pub const GPU_SUPPORTED_MODES: [&str; 5] = ["static", "breathing", "strobing", "rainbow", "off"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuSnapshot {
    pub rgb: [u8; 3],
    pub mode: u8,
}

pub struct GpuController<B: I2cBus> {
    bus: B,
    bus_path: String,
}

impl<B: I2cBus> GpuController<B> {
    pub fn new(bus: B, bus_path: impl Into<String>) -> Self {
        Self {
            bus,
            bus_path: bus_path.into(),
        }
    }

    pub fn verify(&mut self) -> Result<bool, String> {
        let high = self
            .bus
            .read_reg(GPU_REG_MAGIC_H)
            .map_err(|e| e.to_string())?;
        let low = self
            .bus
            .read_reg(GPU_REG_MAGIC_L)
            .map_err(|e| e.to_string())?;
        Ok(high == GPU_MAGIC_HIGH && low == GPU_MAGIC_LOW)
    }

    fn require_signature(&mut self) -> Result<(), String> {
        if self.verify()? {
            Ok(())
        } else {
            Err(SIGNATURE_MISMATCH.to_owned())
        }
    }

    pub fn status(&mut self) -> Result<GpuStatus, String> {
        self.require_signature()?;
        let r = self.bus.read_reg(GPU_REG_RED).map_err(|e| e.to_string())?;
        let g = self
            .bus
            .read_reg(GPU_REG_GREEN)
            .map_err(|e| e.to_string())?;
        let b = self.bus.read_reg(GPU_REG_BLUE).map_err(|e| e.to_string())?;
        let code = self.bus.read_reg(GPU_REG_MODE).map_err(|e| e.to_string())?;
        Ok(GpuStatus {
            device: "ASUS ROG Strix GeForce GTX 1060 6GB".to_owned(),
            bus: self.bus_path.clone(),
            address: format!("0x{GPU_I2C_ADDR:02x}"),
            magic_verified: true,
            mode: GpuMode::from_code(code)
                .map(|mode| mode.name().to_owned())
                .unwrap_or_else(|| format!("unknown({code})")),
            color_source: if code == GpuMode::Spectrum.code() {
                "autonomous"
            } else {
                "configured"
            }
            .to_owned(),
            color_configurable: matches!(
                GpuMode::from_code(code),
                Some(GpuMode::Static | GpuMode::Breathing | GpuMode::Flashing)
            ),
            color_hex: format!("{r:02x}{g:02x}{b:02x}"),
            rgb: [r, g, b],
            supported_modes: GPU_SUPPORTED_MODES.to_vec(),
        })
    }

    pub fn set_color(&mut self, r: u8, g: u8, b: u8, mode: GpuMode) -> Result<(), String> {
        if mode == GpuMode::Spectrum {
            // The firmware generates this effect's colors; leave stored RGB intact.
            return self.set_mode(mode);
        }
        self.require_signature()?;
        for (reg, val) in [
            (GPU_REG_RED, r),
            (GPU_REG_GREEN, g),
            (GPU_REG_BLUE, b),
            (GPU_REG_MODE, mode.code()),
        ] {
            self.bus.write_reg(reg, val).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Raw register snapshot, so unknown mode codes still round-trip.
    pub fn snapshot(&mut self) -> Result<GpuSnapshot, String> {
        self.require_signature()?;
        let mut read = |reg| self.bus.read_reg(reg).map_err(|e| e.to_string());
        Ok(GpuSnapshot {
            rgb: [
                read(GPU_REG_RED)?,
                read(GPU_REG_GREEN)?,
                read(GPU_REG_BLUE)?,
            ],
            mode: read(GPU_REG_MODE)?,
        })
    }

    pub fn restore(&mut self, snapshot: &GpuSnapshot) -> Result<(), String> {
        self.require_signature()?;
        let [r, g, b] = snapshot.rgb;
        for (reg, val) in [
            (GPU_REG_RED, r),
            (GPU_REG_GREEN, g),
            (GPU_REG_BLUE, b),
            (GPU_REG_MODE, snapshot.mode),
        ] {
            self.bus.write_reg(reg, val).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn set_mode(&mut self, mode: GpuMode) -> Result<(), String> {
        self.require_signature()?;
        self.bus
            .write_reg(GPU_REG_MODE, mode.code())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn spectrum_status_does_not_claim_stored_rgb_is_the_live_color() {
        let mut bus = MockI2c::valid();
        bus.regs.insert(GPU_REG_MODE, GpuMode::Spectrum.code());
        let mut ctl = GpuController::new(bus, crate::protocol::GPU_I2C_DEV);
        let status = ctl.status().unwrap();
        assert_eq!(status.mode, "spectrum");
        assert_eq!(status.color_source, "autonomous");
        assert!(!status.color_configurable);
        assert_eq!(status.color_hex, "000000"); // Raw bytes remain compatible.
        ctl.set_color(0x11, 0x22, 0x33, GpuMode::Static).unwrap();
        let status = ctl.status().unwrap();
        assert_eq!(status.color_source, "configured");
        assert!(status.color_configurable);
        assert_eq!(status.color_hex, "112233");
    }

    #[derive(Default)]
    struct MockI2c {
        regs: HashMap<u8, u8>,
        writes: Vec<(u8, u8)>,
        reads: Vec<u8>,
        fail_read: Option<u8>,
        fail_write: Option<u8>,
    }

    impl MockI2c {
        fn valid() -> Self {
            Self {
                regs: HashMap::from([(0x20, 0x15), (0x21, 0x89)]),
                ..Self::default()
            }
        }
    }

    impl I2cBus for MockI2c {
        fn read_reg(&mut self, reg: u8) -> io::Result<u8> {
            self.reads.push(reg);
            if self.fail_read == Some(reg) {
                return Err(io::Error::other("read failed"));
            }
            Ok(*self.regs.get(&reg).unwrap_or(&0))
        }

        fn write_reg(&mut self, reg: u8, val: u8) -> io::Result<()> {
            if self.fail_write == Some(reg) {
                return Err(io::Error::other("write failed"));
            }
            self.writes.push((reg, val));
            self.regs.insert(reg, val);
            Ok(())
        }
    }

    #[test]
    fn signature_mismatch_prevents_all_operations() {
        for reg in [GPU_REG_MAGIC_H, GPU_REG_MAGIC_L] {
            let mut bus = MockI2c::valid();
            bus.regs.insert(reg, 0);
            let mut gpu = GpuController::new(bus, "/dev/i2c-0");
            assert!(!gpu.verify().unwrap());
            assert_eq!(
                gpu.set_color(1, 2, 3, GpuMode::Static).unwrap_err(),
                SIGNATURE_MISMATCH
            );
            assert_eq!(gpu.set_mode(GpuMode::Off).unwrap_err(), SIGNATURE_MISMATCH);
            assert_eq!(gpu.status().unwrap_err(), SIGNATURE_MISMATCH);
            assert!(gpu.bus.writes.is_empty());
            assert!(gpu.bus.reads.iter().all(|reg| [0x20, 0x21].contains(reg)));
        }
    }

    #[test]
    fn set_color_write_order() {
        let mut gpu = GpuController::new(MockI2c::valid(), "/dev/i2c-0");
        gpu.set_color(0x11, 0x22, 0x33, GpuMode::Breathing).unwrap();
        assert_eq!(gpu.bus.reads, [0x20, 0x21]);
        assert_eq!(gpu.bus.writes, [(4, 0x11), (5, 0x22), (6, 0x33), (7, 2)]);
    }

    #[test]
    fn set_mode_write_order_and_reverifies() {
        let mut gpu = GpuController::new(MockI2c::valid(), "/dev/i2c-0");
        gpu.set_mode(GpuMode::Flashing).unwrap();
        assert_eq!(gpu.bus.writes, [(7, 3)]);
        gpu.bus.regs.insert(0x21, 0);
        assert_eq!(gpu.set_mode(GpuMode::Off).unwrap_err(), SIGNATURE_MISMATCH);
        assert_eq!(gpu.bus.writes.len(), 1);
        assert_eq!(gpu.bus.reads, [0x20, 0x21, 0x20, 0x21]);
    }

    #[test]
    fn status_decodes_and_serializes() {
        let mut bus = MockI2c::valid();
        bus.regs.extend([(4, 0x01), (5, 0xAB), (6, 0xFF), (7, 4)]);
        let mut gpu = GpuController::new(bus, "/dev/i2c-test");
        let status = gpu.status().unwrap();
        assert_eq!(status.mode, "spectrum");
        assert_eq!(status.color_hex, "01abff");
        assert_eq!(status.rgb, [1, 0xab, 0xff]);
        assert_eq!(status.bus, "/dev/i2c-test");
        assert_eq!(status.address, "0x29");
        assert!(status.magic_verified);
        assert_eq!(status.device, "ASUS ROG Strix GeForce GTX 1060 6GB");
        assert_eq!(gpu.bus.reads, [0x20, 0x21, 4, 5, 6, 7]);
        assert!(gpu.bus.writes.is_empty());
        assert_eq!(
            serde_json::to_value(status).unwrap()["rgb"],
            serde_json::json!([1, 171, 255])
        );
    }

    #[test]
    fn unknown_mode_status() {
        let mut bus = MockI2c::valid();
        bus.regs.insert(7, 255);
        assert_eq!(
            GpuController::new(bus, "mock").status().unwrap().mode,
            "unknown(255)"
        );
    }

    #[test]
    fn transport_errors_stop_remaining_writes() {
        let mut bus = MockI2c::valid();
        bus.fail_read = Some(0x21);
        let mut gpu = GpuController::new(bus, "mock");
        assert_eq!(gpu.set_mode(GpuMode::Static).unwrap_err(), "read failed");
        assert!(gpu.bus.writes.is_empty());
        gpu.bus.fail_read = None;
        gpu.bus.fail_write = Some(5);
        assert_eq!(
            gpu.set_color(1, 2, 3, GpuMode::Static).unwrap_err(),
            "write failed"
        );
        assert_eq!(gpu.bus.writes, [(4, 1)]);
    }

    #[test]
    fn dry_run_reads_signature_only() {
        let mut bus = DryRunI2c;
        for reg in 0..=u8::MAX {
            let expected = match reg {
                0x20 => 0x15,
                0x21 => 0x89,
                _ => 0,
            };
            assert_eq!(bus.read_reg(reg).unwrap(), expected);
        }
        assert!(GpuController::new(bus, "dry-run").verify().unwrap());
    }

    #[test]
    fn friendly_device_errors_preserve_kind() {
        for (kind, expected) in [
            (io::ErrorKind::PermissionDenied, "sudo"),
            (io::ErrorKind::NotFound, "i2c-dev"),
        ] {
            let error = device_error("/dev/i2c-test", io::Error::from(kind));
            assert_eq!(error.kind(), kind);
            assert!(error.to_string().contains(expected));
            assert!(error.to_string().contains("/dev/i2c-test"));
        }
    }
}
