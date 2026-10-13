//! ENE Aura registers transported over the NCT6793D SMBus controller.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::protocol::{
    channel_name, pack_mobo_color, swap_reg16, MoboMode, MOBO_EXPECTED_NAME, MOBO_PORT_BASE,
    MOBO_REG_APPLY, MOBO_REG_COLORS_DIRECT, MOBO_REG_COLORS_EFFECT, MOBO_REG_CONFIG,
    MOBO_REG_DIRECT, MOBO_REG_DIRECTION, MOBO_REG_MODE, MOBO_REG_NAME, MOBO_REG_SPEED,
    MOBO_SLAVE_ADDR,
};

pub trait PortIo {
    fn inb(&mut self, port: u16) -> io::Result<u8>;
    fn outb(&mut self, port: u16, val: u8) -> io::Result<()>;
}

pub struct DevPort {
    file: File,
}

fn port_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::PermissionDenied {
        io::Error::new(
            error.kind(),
            format!("Permission denied accessing /dev/port. Run with sudo. ({error})"),
        )
    } else {
        error
    }
}

impl DevPort {
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/port")
            .map_err(port_error)?;
        crate::hardware_lock::acquire(&file, "/dev/port")?;
        Ok(Self { file })
    }
}

impl PortIo for DevPort {
    fn inb(&mut self, port: u16) -> io::Result<u8> {
        self.file
            .seek(SeekFrom::Start(u64::from(port)))
            .map_err(port_error)?;
        let mut byte = [0];
        self.file.read_exact(&mut byte).map_err(port_error)?;
        Ok(byte[0])
    }

    fn outb(&mut self, port: u16, val: u8) -> io::Result<()> {
        self.file
            .seek(SeekFrom::Start(u64::from(port)))
            .map_err(port_error)?;
        self.file.write_all(&[val]).map_err(port_error)
    }
}

#[derive(Default)]
pub struct DryRunPort;

impl PortIo for DryRunPort {
    fn inb(&mut self, _port: u16) -> io::Result<u8> {
        Ok(0)
    }
    fn outb(&mut self, _port: u16, _val: u8) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
pub struct MoboZone {
    pub led_index: u8,
    pub channel: String,
    pub channel_name: String,
    pub rgb: [u8; 3],
    pub hex: String,
}

#[derive(Debug, serde::Serialize)]
pub struct MoboStatus {
    pub device: String,
    pub super_io: String,
    pub smbus_port: String,
    pub slave_address: String,
    pub firmware: String,
    pub led_count: u8,
    pub mode: String,
    pub speed: u8,
    pub direction: String,
    /// Which configured-color buffer `zones` was read from: "direct" when direct mode
    /// is enabled, otherwise "effect". Effect colors are the configured color, not an
    /// instantaneous sample of an animation.
    pub color_source: String,
    pub zones: Vec<MoboZone>,
}

/// Both color buffers (direct 0x8000..0x8010, effect 0x8010..0x8020) hold five RGB
/// triplets; the next register (0x8020) is the direct-mode flag.
const LED_CAPACITY: u8 = 5;

/// Raw register snapshot of everything the MCP agent-state patterns overwrite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoboSnapshot {
    pub direct: u8,
    pub mode: u8,
    pub speed: u8,
    pub direction: u8,
    /// Direct and effect buffers in raw ENE byte order (red, blue, green).
    pub direct_colors: Vec<[u8; 3]>,
    pub effect_colors: Vec<[u8; 3]>,
}

pub struct MoboController<P: PortIo> {
    port: P,
    dry_run: bool,
    base_port: u16,
}

impl<P: PortIo> MoboController<P> {
    pub fn new(port: P, dry_run: bool) -> Self {
        Self {
            port,
            dry_run,
            base_port: MOBO_PORT_BASE,
        }
    }

    fn poll_sleep(&self) {
        #[cfg(not(test))]
        if !self.dry_run {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn wait_ready(&mut self) -> io::Result<()> {
        for _ in 0..150 {
            if self.port.inb(self.base_port + 0xE)? & 0x04 == 0 {
                if self.port.inb(self.base_port + 9)? & 0x20 != 0 {
                    return Err(io::Error::other(
                        "Motherboard SMBus NO_ACK: device did not acknowledge.",
                    ));
                }
                return Ok(());
            }
            self.poll_sleep();
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Motherboard SMBus timeout after 150 polls.",
        ))
    }

    fn prepare(&mut self, cmd: u8, read: bool) -> io::Result<()> {
        self.port.outb(self.base_port + 4, 0x40)?;
        self.port
            .outb(self.base_port + 5, (MOBO_SLAVE_ADDR << 1) | u8::from(read))?;
        self.port.outb(self.base_port + 3, cmd)
    }

    fn start(&mut self, kind: u8) -> io::Result<()> {
        self.port.outb(self.base_port + 2, kind)?;
        self.port.outb(self.base_port + 4, 0x80)?;
        self.wait_ready()
    }

    fn smbus_write_word(&mut self, cmd: u8, word: u16) -> io::Result<()> {
        if self.dry_run {
            return Ok(());
        }
        self.prepare(cmd, false)?;
        self.port.outb(self.base_port, word as u8)?;
        self.port.outb(self.base_port, (word >> 8) as u8)?;
        self.start(0x09)
    }

    fn smbus_write_byte(&mut self, cmd: u8, val: u8) -> io::Result<()> {
        if self.dry_run {
            return Ok(());
        }
        self.prepare(cmd, false)?;
        self.port.outb(self.base_port, val)?;
        self.start(0x08)
    }

    fn smbus_read_byte(&mut self, cmd: u8) -> io::Result<u8> {
        if self.dry_run {
            return Ok(0);
        }
        self.prepare(cmd, true)?;
        self.start(0x00)?;
        self.port.inb(self.base_port)
    }

    fn ene_read_reg(&mut self, reg: u16) -> Result<u8, String> {
        if self.dry_run {
            return Ok(match reg {
                0x1000..=0x1007 => MOBO_EXPECTED_NAME.as_bytes()[usize::from(reg - MOBO_REG_NAME)],
                0x1C02 => 5,
                0x1C13..=0x1C16 => 0x85,
                0x1C17 => 0x86,
                MOBO_REG_MODE => 5,
                _ => 0,
            });
        }
        self.smbus_write_word(0x00, swap_reg16(reg))
            .map_err(|e| e.to_string())?;
        self.smbus_read_byte(0x81).map_err(|e| e.to_string())
    }

    fn ene_write_reg(&mut self, reg: u16, val: u8) -> Result<(), String> {
        if self.dry_run {
            return crate::protocol::dry_run_log(&format!(
                "[DRY-RUN] Mobo Write: Reg 0x{reg:04x} <= 0x{val:02x}"
            ))
            .map_err(|e| e.to_string());
        }
        self.smbus_write_word(0x00, swap_reg16(reg))
            .map_err(|e| e.to_string())?;
        self.smbus_write_byte(0x01, val).map_err(|e| e.to_string())
    }

    pub fn verify(&mut self) -> Result<String, String> {
        let mut name = String::new();
        for offset in 0..16 {
            let byte = self.ene_read_reg(MOBO_REG_NAME + offset)?;
            if byte == 0 {
                break;
            }
            name.push(char::from(byte)); // Preserve the Python latin1 decoding.
        }
        if !name.starts_with(MOBO_EXPECTED_NAME) {
            return Err(format!("Motherboard Aura firmware identifier mismatch: got '{name}', expected 'LED-0116'. Safety abort."));
        }
        Ok(name)
    }

    /// Firmware LED count. Zero keeps the historical default of five; anything above
    /// the buffer capacity would write into neighbouring registers, so it aborts
    /// before any register is changed.
    fn led_count(&mut self) -> Result<u8, String> {
        let count = self.ene_read_reg(MOBO_REG_CONFIG + 2)?;
        match count {
            0 => Ok(LED_CAPACITY),
            1..=LED_CAPACITY => Ok(count),
            _ => Err(format!(
                "Motherboard reports {count} LEDs but the color buffers hold {LED_CAPACITY}. Safety abort."
            )),
        }
    }

    pub fn status(&mut self) -> Result<MoboStatus, String> {
        let firmware = self.verify()?;
        let led_count = self.led_count()?;
        let mode = self.ene_read_reg(MOBO_REG_MODE)?;
        let speed = self.ene_read_reg(MOBO_REG_SPEED)?;
        let direction = self.ene_read_reg(MOBO_REG_DIRECTION)?;
        let direct = self.ene_read_reg(MOBO_REG_DIRECT)? != 0;
        let (color_base, color_source) = if direct {
            (MOBO_REG_COLORS_DIRECT, "direct")
        } else {
            (MOBO_REG_COLORS_EFFECT, "effect")
        };
        let mut zones = Vec::with_capacity(usize::from(led_count));
        for i in 0..led_count {
            let channel = self.ene_read_reg(MOBO_REG_CONFIG + 0x13 + u16::from(i))?;
            let reg = color_base + 3 * u16::from(i);
            let r = self.ene_read_reg(reg)?;
            let b = self.ene_read_reg(reg + 1)?;
            let g = self.ene_read_reg(reg + 2)?;
            zones.push(MoboZone {
                led_index: i,
                channel: format!("0x{channel:02x}"),
                channel_name: channel_name(channel),
                rgb: [r, g, b],
                hex: format!("{r:02x}{g:02x}{b:02x}"),
            });
        }
        Ok(MoboStatus {
            device: "ASUS ROG STRIX Z270E GAMING Motherboard".to_owned(),
            super_io: "Nuvoton NCT6793D".to_owned(),
            smbus_port: format!("0x{:04x}", self.base_port),
            slave_address: format!("0x{MOBO_SLAVE_ADDR:02x}"),
            firmware,
            led_count,
            mode: MoboMode::from_code(mode)
                .map(|m| m.name().to_owned())
                .unwrap_or_else(|| format!("unknown({mode})")),
            speed,
            direction: if direction == 0 { "forward" } else { "reverse" }.to_owned(),
            color_source: color_source.to_owned(),
            zones,
        })
    }

    pub fn set_color_static(&mut self, r: u8, g: u8, b: u8) -> Result<(), String> {
        self.verify()?;
        let count = self.led_count()?;
        self.ene_write_reg(MOBO_REG_DIRECT, 1)?;
        for i in 0..count {
            for (offset, byte) in pack_mobo_color(r, g, b).into_iter().enumerate() {
                self.ene_write_reg(
                    MOBO_REG_COLORS_DIRECT + 3 * u16::from(i) + offset as u16,
                    byte,
                )?;
            }
        }
        self.write_mode(MoboMode::Static, 0, 0)
    }

    fn write_mode(&mut self, mode: MoboMode, speed: u8, direction: u8) -> Result<(), String> {
        for (reg, val) in [
            (MOBO_REG_MODE, mode.code()),
            (MOBO_REG_SPEED, speed),
            (MOBO_REG_DIRECTION, direction),
            (MOBO_REG_APPLY, 1),
        ] {
            self.ene_write_reg(reg, val)?;
        }
        Ok(())
    }

    /// Mode-only transition. Animated modes and Off leave direct mode first, since a
    /// set direct flag keeps the board on the static direct buffer. Static keeps the
    /// current buffer selection: it is "whatever color is already configured".
    pub fn set_mode(&mut self, mode: MoboMode, speed: u8, direction: u8) -> Result<(), String> {
        self.verify()?;
        if mode != MoboMode::Static {
            self.ene_write_reg(MOBO_REG_DIRECT, 0)?;
        }
        self.write_mode(mode, speed, direction)
    }

    /// Static color on one zone; other zones keep their direct-buffer colors.
    pub fn set_zone_color(&mut self, zone: u8, r: u8, g: u8, b: u8) -> Result<(), String> {
        self.verify()?;
        let count = self.led_count()?;
        if zone >= count {
            return Err(format!("Zone {zone} out of range (0..{})", count - 1));
        }
        self.ene_write_reg(MOBO_REG_DIRECT, 1)?;
        for (offset, byte) in pack_mobo_color(r, g, b).into_iter().enumerate() {
            self.ene_write_reg(
                MOBO_REG_COLORS_DIRECT + 3 * u16::from(zone) + offset as u16,
                byte,
            )?;
        }
        self.write_mode(MoboMode::Static, 0, 0)
    }

    /// Animated effect, optionally with the colors that color-driven effects use.
    pub fn set_effect(
        &mut self,
        mode: MoboMode,
        speed: u8,
        color: Option<(u8, u8, u8)>,
    ) -> Result<(), String> {
        self.verify()?;
        let count = self.led_count()?;
        self.ene_write_reg(MOBO_REG_DIRECT, 0)?;
        if let Some((r, g, b)) = color {
            for i in 0..count {
                for (offset, byte) in pack_mobo_color(r, g, b).into_iter().enumerate() {
                    self.ene_write_reg(
                        MOBO_REG_COLORS_EFFECT + 3 * u16::from(i) + offset as u16,
                        byte,
                    )?;
                }
            }
        }
        self.write_mode(mode, speed, 0)
    }

    pub fn snapshot(&mut self) -> Result<MoboSnapshot, String> {
        self.verify()?;
        let count = self.led_count()?;
        let read_colors = |this: &mut Self, base: u16, leds: u8| -> Result<Vec<[u8; 3]>, String> {
            let mut colors = Vec::new();
            for i in 0..leds {
                let reg = base + 3 * u16::from(i);
                colors.push([
                    this.ene_read_reg(reg)?,
                    this.ene_read_reg(reg + 1)?,
                    this.ene_read_reg(reg + 2)?,
                ]);
            }
            Ok(colors)
        };
        let direct_colors = read_colors(self, MOBO_REG_COLORS_DIRECT, count)?;
        let effect_colors = read_colors(self, MOBO_REG_COLORS_EFFECT, count)?;
        Ok(MoboSnapshot {
            direct: self.ene_read_reg(MOBO_REG_DIRECT)?,
            mode: self.ene_read_reg(MOBO_REG_MODE)?,
            speed: self.ene_read_reg(MOBO_REG_SPEED)?,
            direction: self.ene_read_reg(MOBO_REG_DIRECTION)?,
            direct_colors,
            effect_colors,
        })
    }

    pub fn restore(&mut self, snapshot: &MoboSnapshot) -> Result<(), String> {
        self.verify()?;
        // A hand-built snapshot must not be able to write past either buffer.
        for (name, colors) in [
            ("direct", &snapshot.direct_colors),
            ("effect", &snapshot.effect_colors),
        ] {
            if colors.len() > usize::from(LED_CAPACITY) {
                return Err(format!(
                    "Snapshot {name} buffer has {} LEDs; the maximum is {LED_CAPACITY}.",
                    colors.len()
                ));
            }
        }
        for (base, colors) in [
            (MOBO_REG_COLORS_DIRECT, &snapshot.direct_colors),
            (MOBO_REG_COLORS_EFFECT, &snapshot.effect_colors),
        ] {
            for (i, rbg) in colors.iter().enumerate() {
                for (offset, byte) in rbg.iter().enumerate() {
                    self.ene_write_reg(base + 3 * i as u16 + offset as u16, *byte)?;
                }
            }
        }
        for (reg, val) in [
            (MOBO_REG_DIRECT, snapshot.direct),
            (MOBO_REG_MODE, snapshot.mode),
            (MOBO_REG_SPEED, snapshot.speed),
            (MOBO_REG_DIRECTION, snapshot.direction),
            (MOBO_REG_APPLY, 1),
        ] {
            self.ene_write_reg(reg, val)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MockPort {
        regs: HashMap<u16, u8>,
        ports: HashMap<u16, u8>,
        data: Vec<u8>,
        selected: u16,
        writes: Vec<(u16, u8)>,
        wire: Vec<(u16, u8)>,
        busy: bool,
        no_ack: bool,
        polls: usize,
        fail: bool,
    }

    impl MockPort {
        fn valid(count: u8) -> Self {
            let mut mock = Self::default();
            for (i, byte) in b"LED-0116".iter().enumerate() {
                mock.regs.insert(0x1000 + i as u16, *byte);
            }
            mock.regs.insert(0x1C02, count);
            mock
        }
    }

    impl PortIo for MockPort {
        fn inb(&mut self, port: u16) -> io::Result<u8> {
            if self.fail {
                return Err(io::Error::other("port failure"));
            }
            match port - MOBO_PORT_BASE {
                0xE => {
                    self.polls += 1;
                    Ok(if self.busy { 4 } else { 0 })
                }
                9 => Ok(if self.no_ack { 0x20 } else { 0 }),
                0 => Ok(*self.regs.get(&self.selected).unwrap_or(&0)),
                _ => Ok(0),
            }
        }

        fn outb(&mut self, port: u16, val: u8) -> io::Result<()> {
            if self.fail {
                return Err(io::Error::other("port failure"));
            }
            self.wire.push((port, val));
            self.ports.insert(port, val);
            match (port - MOBO_PORT_BASE, val) {
                (4, 0x40) => self.data.clear(),
                (0, _) => self.data.push(val),
                (4, 0x80) if !self.busy && !self.no_ack => {
                    let cmd = self.ports[&(MOBO_PORT_BASE + 3)];
                    let kind = self.ports[&(MOBO_PORT_BASE + 2)];
                    let address = self.ports[&(MOBO_PORT_BASE + 5)];
                    match (kind, cmd, address) {
                        (9, 0, 0x9C) => {
                            assert_eq!(self.data.len(), 2);
                            self.selected =
                                swap_reg16(u16::from_le_bytes([self.data[0], self.data[1]]));
                        }
                        (8, 1, 0x9C) => {
                            assert_eq!(self.data.len(), 1);
                            self.writes.push((self.selected, self.data[0]));
                            self.regs.insert(self.selected, self.data[0]);
                        }
                        (0, 0x81, 0x9D) => assert!(self.data.is_empty()),
                        _ => panic!("invalid SMBus command"),
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[test]
    fn rejects_identifier_without_ene_writes() {
        let mut mock = MockPort::valid(5);
        mock.regs.insert(0x1000, b'X');
        let mut ctl = MoboController::new(mock, false);
        let expected = "Motherboard Aura firmware identifier mismatch: got 'XED-0116', expected 'LED-0116'. Safety abort.";
        assert_eq!(ctl.set_color_static(1, 2, 3).unwrap_err(), expected);
        assert_eq!(ctl.set_mode(MoboMode::Off, 0, 0).unwrap_err(), expected);
        assert_eq!(ctl.status().unwrap_err(), expected);
        // Address-selection writes are necessary for reads; no ENE register is mutated.
        assert!(ctl.port.writes.is_empty());
    }

    #[test]
    fn static_order_and_led_count_bounds() {
        for (configured, count) in [(0, 5), (1, 1), (5, 5)] {
            let mut ctl = MoboController::new(MockPort::valid(configured), false);
            ctl.set_color_static(0x11, 0x22, 0x33).unwrap();
            let mut expected = vec![(0x8020, 1)];
            for i in 0..count {
                expected.extend([
                    (0x8000 + 3 * i, 0x11),
                    (0x8001 + 3 * i, 0x33),
                    (0x8002 + 3 * i, 0x22),
                ]);
            }
            expected.extend([(0x8021, 1), (0x8022, 0), (0x8023, 0), (0x80A0, 1)]);
            assert_eq!(ctl.port.writes, expected);
        }
    }

    #[test]
    fn oversized_led_counts_abort_before_any_mutation() {
        for configured in [6, 11, 16, 255] {
            let mut ctl = MoboController::new(MockPort::valid(configured), false);
            assert!(ctl
                .set_color_static(1, 2, 3)
                .unwrap_err()
                .contains("Safety abort"));
            assert!(ctl.set_zone_color(0, 1, 2, 3).is_err());
            assert!(ctl
                .set_effect(MoboMode::Breathing, 0, Some((1, 2, 3)))
                .is_err());
            assert!(ctl.status().is_err());
            assert!(ctl.snapshot().is_err());
            assert!(
                ctl.port.writes.is_empty(),
                "count {configured}: {:?}",
                ctl.port.writes
            );
        }
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        for zone in [5, 6, 255] {
            assert!(ctl.set_zone_color(zone, 1, 2, 3).is_err());
        }
        assert!(ctl.port.writes.is_empty());
    }

    #[test]
    fn color_writes_stay_inside_their_own_buffers() {
        for count in 1..=5u8 {
            let mut ctl = MoboController::new(MockPort::valid(count), false);
            ctl.set_color_static(1, 2, 3).unwrap();
            ctl.set_effect(MoboMode::Chase, 1, Some((4, 5, 6))).unwrap();
            for (reg, _) in &ctl.port.writes {
                let in_direct = (0x8000..0x8000 + 3 * u16::from(count)).contains(reg);
                let in_effect = (0x8010..0x8010 + 3 * u16::from(count)).contains(reg);
                let control = (0x8020..=0x80A0).contains(reg);
                assert!(
                    in_direct || in_effect || control,
                    "count {count}: stray write {reg:#x}"
                );
            }
        }
    }

    #[test]
    fn oversized_snapshots_are_rejected_before_any_write() {
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        let mut snapshot = ctl.snapshot().unwrap();
        for which in 0..2 {
            let mut bad = snapshot.clone();
            let colors = if which == 0 {
                &mut bad.direct_colors
            } else {
                &mut bad.effect_colors
            };
            colors.resize(6, [9, 9, 9]);
            assert!(ctl.restore(&bad).unwrap_err().contains("maximum is 5"));
        }
        assert!(ctl.port.writes.is_empty());
        snapshot.direct_colors.truncate(2);
        ctl.restore(&snapshot).unwrap();
    }

    #[test]
    fn animated_and_off_transitions_leave_direct_mode_before_applying() {
        for (setup_zone, mode, code) in [
            (false, MoboMode::Rainbow, 5),
            (true, MoboMode::Breathing, 2),
            (false, MoboMode::Off, 0),
        ] {
            let mut ctl = MoboController::new(MockPort::valid(5), false);
            if setup_zone {
                ctl.set_zone_color(1, 1, 2, 3).unwrap();
            } else {
                ctl.set_color_static(1, 2, 3).unwrap();
            }
            assert_eq!(ctl.port.regs[&0x8020], 1);
            ctl.port.writes.clear();
            ctl.set_mode(mode, 2, 0).unwrap();
            assert_eq!(ctl.port.regs[&0x8020], 0, "{mode:?}");
            assert_eq!(
                ctl.port.writes,
                [
                    (0x8020, 0),
                    (0x8021, code),
                    (0x8022, 2),
                    (0x8023, 0),
                    (0x80A0, 1)
                ]
            );
        }
        // Static without a color keeps the current buffer selection.
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.set_color_static(1, 2, 3).unwrap();
        ctl.port.writes.clear();
        ctl.set_mode(MoboMode::Static, 0, 0).unwrap();
        assert!(ctl.port.writes.iter().all(|(reg, _)| *reg != 0x8020));
        assert_eq!(ctl.port.regs[&0x8020], 1);
    }

    #[test]
    fn status_reports_the_active_color_buffer() {
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.set_color_static(0x11, 0x22, 0x33).unwrap();
        ctl.set_effect(MoboMode::Breathing, 0, Some((0xaa, 0xbb, 0xcc)))
            .unwrap();
        // Effect buffer bytes are red, blue, green; the inactive direct buffer differs.
        let status = ctl.status().unwrap();
        assert_eq!(status.color_source, "effect");
        assert_eq!(status.zones[0].hex, "aabbcc");
        assert_eq!(status.mode, "breathing");
        ctl.set_color_static(0x44, 0x55, 0x66).unwrap();
        let status = ctl.status().unwrap();
        assert_eq!(status.color_source, "direct");
        assert_eq!(status.zones[4].hex, "445566");
    }

    #[test]
    fn zone_color_writes_one_zone_and_rejects_out_of_range() {
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.set_zone_color(2, 0x11, 0x22, 0x33).unwrap();
        assert_eq!(
            ctl.port.writes,
            vec![
                (0x8020, 1),
                (0x8006, 0x11),
                (0x8007, 0x33),
                (0x8008, 0x22),
                (0x8021, 1),
                (0x8022, 0),
                (0x8023, 0),
                (0x80A0, 1)
            ]
        );
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        assert!(ctl
            .set_zone_color(5, 1, 2, 3)
            .unwrap_err()
            .contains("out of range"));
        assert!(ctl.port.writes.is_empty());
    }

    #[test]
    fn effect_leaves_direct_mode_and_never_overruns_the_effect_buffer() {
        // A full five-LED board fills 0x8010..0x801F and never touches 0x8020 again.
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.set_effect(MoboMode::Wave, 3, Some((0x11, 0x22, 0x33)))
            .unwrap();
        let writes = &ctl.port.writes;
        assert_eq!(writes[0], (0x8020, 0));
        assert_eq!(
            writes[1..4],
            [(0x8010, 0x11), (0x8011, 0x33), (0x8012, 0x22)]
        );
        assert_eq!(
            writes
                .iter()
                .filter(|(reg, _)| (0x8010..0x8020).contains(reg))
                .count(),
            15
        );
        assert_eq!(writes.iter().filter(|(reg, _)| *reg == 0x8020).count(), 1);
        assert_eq!(
            writes[16..],
            [(0x8021, 11), (0x8022, 3), (0x8023, 0), (0x80A0, 1)]
        );

        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.set_effect(MoboMode::Rainbow, 0, None).unwrap();
        assert_eq!(ctl.port.writes[0], (0x8020, 0));
        assert_eq!(
            ctl.port.writes.len(),
            5,
            "no color buffer writes without a color"
        );
    }

    #[test]
    fn snapshot_restore_round_trips_registers() {
        let mut port = MockPort::valid(5);
        for (reg, val) in [
            (0x8000, 1),
            (0x8001, 2),
            (0x8002, 3),
            (0x8010, 7),
            (0x8014, 9),
            (0x8020, 1),
            (0x8021, 11),
            (0x8022, 2),
            (0x8023, 1),
        ] {
            port.regs.insert(reg, val);
        }
        let mut ctl = MoboController::new(port, false);
        let snapshot = ctl.snapshot().unwrap();
        assert_eq!(
            (
                snapshot.direct,
                snapshot.mode,
                snapshot.speed,
                snapshot.direction
            ),
            (1, 11, 2, 1)
        );
        assert_eq!(snapshot.direct_colors.len(), 5);
        assert_eq!(snapshot.direct_colors[0], [1, 2, 3]);
        assert_eq!(snapshot.effect_colors[0], [7, 0, 0]);

        ctl.set_effect(MoboMode::Breathing, 0, Some((0xff, 0xb0, 0)))
            .unwrap();
        ctl.restore(&snapshot).unwrap();
        assert_eq!(ctl.snapshot().unwrap(), snapshot);
    }

    #[test]
    fn exact_read_framing() {
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        assert_eq!(ctl.ene_read_reg(0x1000).unwrap(), b'L');
        let b = MOBO_PORT_BASE;
        assert_eq!(
            ctl.port.wire,
            [
                (b + 4, 0x40),
                (b + 5, 0x9C),
                (b + 3, 0),
                (b, 0x10),
                (b, 0),
                (b + 2, 9),
                (b + 4, 0x80),
                (b + 4, 0x40),
                (b + 5, 0x9D),
                (b + 3, 0x81),
                (b + 2, 0),
                (b + 4, 0x80)
            ]
        );
        assert_eq!(swap_reg16(0x1000), 0x0010);
    }

    #[test]
    fn exact_write_framing() {
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.ene_write_reg(0x8021, 1).unwrap();
        let b = MOBO_PORT_BASE;
        assert_eq!(
            ctl.port.wire,
            [
                (b + 4, 0x40),
                (b + 5, 0x9C),
                (b + 3, 0),
                (b, 0x80),
                (b, 0x21),
                (b + 2, 9),
                (b + 4, 0x80),
                (b + 4, 0x40),
                (b + 5, 0x9C),
                (b + 3, 1),
                (b, 1),
                (b + 2, 8),
                (b + 4, 0x80)
            ]
        );
    }

    #[test]
    fn mode_order_and_reverification() {
        let mut ctl = MoboController::new(MockPort::valid(5), false);
        ctl.set_mode(MoboMode::Rainbow, 3, 1).unwrap();
        assert_eq!(
            ctl.port.writes,
            [
                (0x8020, 0),
                (0x8021, 5),
                (0x8022, 3),
                (0x8023, 1),
                (0x80A0, 1)
            ]
        );
        ctl.port.regs.insert(0x1000, 0);
        assert!(ctl.set_mode(MoboMode::Off, 0, 0).is_err());
        assert_eq!(ctl.port.writes.len(), 5);
    }

    #[test]
    fn status_unswaps_and_serializes() {
        let mut mock = MockPort::valid(1);
        mock.regs.extend([
            (0x8000, 0x11),
            (0x8001, 0x33),
            (0x8002, 0x22),
            (0x8020, 1),
            (0x1C13, 0x85),
            (0x8021, 255),
            (0x8022, 2),
            (0x8023, 1),
        ]);
        let mut ctl = MoboController::new(mock, false);
        let status = ctl.status().unwrap();
        assert_eq!(status.firmware, "LED-0116");
        assert_eq!(status.smbus_port, "0x02c0");
        assert_eq!(status.slave_address, "0x4e");
        assert_eq!(status.mode, "unknown(255)");
        assert_eq!(status.speed, 2);
        assert_eq!(status.direction, "reverse");
        assert_eq!(status.zones[0].rgb, [0x11, 0x22, 0x33]);
        assert_eq!(status.zones[0].hex, "112233");
        assert_eq!(status.zones[0].channel_name, "Back I/O");
        assert!(ctl.port.writes.is_empty());
        assert_eq!(
            serde_json::to_value(status).unwrap()["zones"][0]["rgb"],
            serde_json::json!([17, 34, 51])
        );
    }

    #[test]
    fn timeout_no_ack_and_io_errors_abort() {
        for kind in 0..3 {
            let mut mock = MockPort::valid(5);
            mock.busy = kind == 0;
            mock.no_ack = kind == 1;
            mock.fail = kind == 2;
            let mut ctl = MoboController::new(mock, false);
            let error = ctl.set_mode(MoboMode::Static, 0, 0).unwrap_err();
            assert!(error.contains(["timeout", "NO_ACK", "port failure"][kind]));
            assert!(ctl.port.writes.is_empty());
            if kind == 0 {
                assert_eq!(ctl.port.polls, 150);
            }
        }
    }

    #[test]
    fn dry_run_never_touches_port() {
        let mock = MockPort {
            fail: true,
            ..MockPort::default()
        };
        let mut ctl = MoboController::new(mock, true);
        let status = ctl.status().unwrap();
        assert_eq!(status.led_count, 5);
        assert_eq!(status.mode, "rainbow");
        assert_eq!(status.zones[4].channel, "0x86");
        assert_eq!(status.zones[0].channel, "0x85");
        assert_eq!(status.direction, "forward");
        ctl.set_color_static(0x11, 0x22, 0x33).unwrap();
        ctl.set_mode(MoboMode::Off, 0, 0).unwrap();
        assert!(ctl.port.wire.is_empty());
        assert_eq!(ctl.port.polls, 0);
    }
}
