#!/usr/bin/env python3
"""
ASUS Aura RGB Controller for Linux
Supports:
  - GPU: ASUS ROG Strix GeForce GTX 1060 (via /dev/i2c-0 at 0x29)
  - Motherboard: ASUS ROG STRIX Z270E GAMING (via NCT6793D Super I/O port 0x02c0 at 0x4e)

Pure Python 3 standard library, no external packages required.
"""

import sys
import os
import time
import argparse
import json
from typing import Dict, Any, Tuple, Optional, List

# Matches the Rust CLI: one exclusive advisory flock per device node, held while open,
# so Python and Rust processes never interleave register transactions.
DEVICE_LOCK_TIMEOUT = 5.0


def lock_device(fd: int, path: str, timeout: Optional[float] = None,
                sleep=time.sleep, monotonic=time.monotonic) -> None:
    import errno
    import fcntl
    deadline = monotonic() + (DEVICE_LOCK_TIMEOUT if timeout is None else timeout)
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return
        except OSError as exc:
            if exc.errno not in (errno.EAGAIN, errno.EACCES, errno.EWOULDBLOCK):
                raise
            if monotonic() >= deadline:
                raise TimeoutError(
                    f"timed out waiting for exclusive access to {path} "
                    "(another luminactl process is using it)")
            sleep(0.025)


# --- Constants: GPU ---
GPU_I2C_DEV = "/dev/i2c-0"
GPU_I2C_ADDR = 0x29
GPU_MAGIC_HIGH = 0x15
GPU_MAGIC_LOW = 0x89

GPU_REG_RED = 0x04
GPU_REG_GREEN = 0x05
GPU_REG_BLUE = 0x06
GPU_REG_MODE = 0x07
GPU_REG_SYNC = 0x0C
GPU_REG_APPLY = 0x0E
GPU_REG_MAGIC_H = 0x20
GPU_REG_MAGIC_L = 0x21

GPU_MODES = {
    "off": 0,
    "static": 1,
    "breathing": 2,
    "flashing": 3,
    "spectrum": 4,
}
GPU_MODE_NAMES = {v: k for k, v in GPU_MODES.items()}

# --- Constants: Motherboard (ENE on NCT6793D) ---
MOBO_PORT_BASE = 0x02c0
MOBO_SLAVE_ADDR = 0x4e
MOBO_EXPECTED_NAME = "LED-0116"

MOBO_REG_NAME = 0x1000
MOBO_REG_CONFIG = 0x1C00
MOBO_REG_DIRECT = 0x8020
MOBO_REG_MODE = 0x8021
MOBO_REG_SPEED = 0x8022
MOBO_REG_DIRECTION = 0x8023
MOBO_REG_APPLY = 0x80A0
MOBO_REG_COLORS_DIRECT = 0x8000
MOBO_REG_COLORS_EFFECT = 0x8010

MOBO_MODES = {
    "off": 0,
    "static": 1,
    "breathing": 2,
    "flashing": 3,
    "spectrum": 4,
    "rainbow": 5,
    "rainbow_breathing": 6,
    "chase_fade": 7,
    "spectrum_chase_fade": 8,
    "chase": 9,
    "spectrum_chase": 10,
    "wave": 11,
    "rainbow_pulse": 12,
    "random_flicker": 13,
}
MOBO_MODE_NAMES = {v: k for k, v in MOBO_MODES.items()}

# One vocabulary for every subcommand, matching the Rust CLI: friendly aliases map onto
# the native names, and `all` resolves one name to a (GPU mode, motherboard mode) pair.
GPU_ALIASES = {"rainbow": "spectrum", "color-cycle": "spectrum", "strobing": "flashing"}
MOBO_ALIASES = {
    "color-cycle": "spectrum", "strobing": "flashing", "comet": "chase_fade",
    "flash-and-dash": "spectrum_chase_fade", "glowing-yoyo": "rainbow_pulse",
    "starry-night": "random_flicker",
}
# Rainbow-style effects run the GPU's only autonomous effect: its Spectrum color cycle.
GPU_SPECTRUM_FOR = {"rainbow", "rainbow_breathing", "wave", "rainbow_pulse"}
# Motherboard effects that draw from a configured color (REA mode table).
MOBO_COLOR_MODES = {"static", "breathing", "flashing", "chase_fade", "chase"}


def resolve_gpu_mode(name: str) -> str:
    key = name.strip().lower().replace("_", "-")
    native = GPU_ALIASES.get(key, key.replace("-", "_"))
    if native not in GPU_MODES:
        raise ValueError(f"invalid GPU mode '{name}' (choose from {', '.join(sorted(GPU_MODES))}, rainbow, color-cycle)")
    return native


def resolve_mobo_mode(name: str) -> str:
    key = name.strip().lower().replace("_", "-")
    native = MOBO_ALIASES.get(key, key.replace("-", "_"))
    if native not in MOBO_MODES:
        raise ValueError(f"invalid motherboard mode '{name}'")
    return native


def resolve_all_mode(name: str) -> Tuple[str, str]:
    """(GPU mode name, motherboard mode name) for `all --mode`; unknown names are errors."""
    try:
        mobo = resolve_mobo_mode(name)
    except ValueError:
        mobo = None
    try:
        gpu = resolve_gpu_mode(name)
    except ValueError:
        gpu = None
    if mobo is None and gpu is None:
        raise ValueError(f"invalid mode '{name}'")
    mobo = mobo or "static"
    if mobo in GPU_SPECTRUM_FOR:
        gpu = "spectrum"
    elif gpu is None:
        gpu = "static"  # no GPU equivalent: fall back to static color
    return gpu, mobo

MOBO_CHANNELS = {
    0x84: "Audio",
    0x85: "Back I/O",
    0x86: "RGB Header 1",
    0x87: "RGB Header 2",
}

# Linux i2c ioctls
I2C_SLAVE = 0x0703
I2C_SMBUS = 0x0720
I2C_SMBUS_READ = 1
I2C_SMBUS_WRITE = 0
I2C_SMBUS_BYTE_DATA = 2


def parse_hex_color(hex_str: str) -> Tuple[int, int, int]:
    clean = hex_str.lstrip("#")
    if len(clean) != 6:
        raise ValueError(f"Color '{hex_str}' must be a 6-character hex string (e.g. ff00aa)")
    return int(clean[0:2], 16), int(clean[2:4], 16), int(clean[4:6], 16)


class GpuAuraController:
    """Controls the ASUS ROG Strix GTX 1060 Aura controller over /dev/i2c-0."""

    def __init__(self, dev_path: str = GPU_I2C_DEV, dry_run: bool = False):
        self.dev_path = dev_path
        self.dry_run = dry_run
        self.fd: Optional[int] = None

    def open(self):
        if self.dry_run or self.fd is not None:
            return  # idempotent: an owned descriptor (and its lock) is never replaced
        if not os.path.exists(self.dev_path):
            raise FileNotFoundError(f"I2C device '{self.dev_path}' not found. Ensure i2c-dev module is loaded.")
        import fcntl
        try:
            fd = os.open(self.dev_path, os.O_RDWR)
        except PermissionError:
            raise PermissionError(f"Permission denied accessing '{self.dev_path}'. Run with sudo or add user to group 'i2c'.")
        try:
            lock_device(fd, self.dev_path)
            fcntl.ioctl(fd, I2C_SLAVE, GPU_I2C_ADDR)
        except PermissionError:
            os.close(fd)
            raise PermissionError(f"Permission denied accessing '{self.dev_path}'. Run with sudo or add user to group 'i2c'.")
        except BaseException:
            os.close(fd)
            raise
        self.fd = fd  # published only once the lock and slave address are in place

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def read_byte_data(self, reg: int) -> int:
        if self.dry_run:
            if reg == GPU_REG_MAGIC_H:
                return GPU_MAGIC_HIGH
            if reg == GPU_REG_MAGIC_L:
                return GPU_MAGIC_LOW
            return 0x00

        import fcntl, struct, ctypes
        # SMBus read byte data via ioctl(I2C_SMBUS)
        class i2c_smbus_ioctl_data(ctypes.Structure):
            _fields_ = [
                ('read_write', ctypes.c_uint8),
                ('command', ctypes.c_uint8),
                ('size', ctypes.c_uint32),
                ('data', ctypes.c_void_p)
            ]
        data_buf = ctypes.create_string_buffer(34)
        msg = i2c_smbus_ioctl_data(
            read_write=I2C_SMBUS_READ,
            command=reg,
            size=I2C_SMBUS_BYTE_DATA,
            data=ctypes.cast(data_buf, ctypes.c_void_p)
        )
        fcntl.ioctl(self.fd, I2C_SMBUS, msg)
        return data_buf.raw[0]

    def write_byte_data(self, reg: int, val: int):
        if self.dry_run:
            print(f"[DRY-RUN] GPU Write: Reg 0x{reg:02x} <= 0x{val:02x}")
            return

        import fcntl, ctypes
        class i2c_smbus_ioctl_data(ctypes.Structure):
            _fields_ = [
                ('read_write', ctypes.c_uint8),
                ('command', ctypes.c_uint8),
                ('size', ctypes.c_uint32),
                ('data', ctypes.c_void_p)
            ]
        data_buf = ctypes.create_string_buffer(bytes([val]), 34)
        msg = i2c_smbus_ioctl_data(
            read_write=I2C_SMBUS_WRITE,
            command=reg,
            size=I2C_SMBUS_BYTE_DATA,
            data=ctypes.cast(data_buf, ctypes.c_void_p)
        )
        fcntl.ioctl(self.fd, I2C_SMBUS, msg)

    def verify(self) -> bool:
        """Safety gate: Verify the Aura magic signature 0x1589 before executing any commands."""
        h = self.read_byte_data(GPU_REG_MAGIC_H)
        l = self.read_byte_data(GPU_REG_MAGIC_L)
        return (h == GPU_MAGIC_HIGH) and (l == GPU_MAGIC_LOW)

    def get_status(self) -> Dict[str, Any]:
        self.open()
        try:
            if not self.verify():
                raise RuntimeError("GPU Aura signature mismatch (expected 0x1589). Safety abort.")
            r = self.read_byte_data(GPU_REG_RED)
            g = self.read_byte_data(GPU_REG_GREEN)
            b = self.read_byte_data(GPU_REG_BLUE)
            mode_val = self.read_byte_data(GPU_REG_MODE)
            return {
                "device": "ASUS ROG Strix GeForce GTX 1060 6GB",
                "bus": self.dev_path,
                "address": f"0x{GPU_I2C_ADDR:02x}",
                "magic_verified": True,
                "mode": GPU_MODE_NAMES.get(mode_val, f"unknown({mode_val})"),
                "color_hex": f"{r:02x}{g:02x}{b:02x}",
                "rgb": [r, g, b]
            }
        finally:
            self.close()

    def set_color(self, red: int, green: int, blue: int, mode: int = 1):
        self.open()
        try:
            if not self.verify():
                raise RuntimeError("GPU Aura signature mismatch (expected 0x1589). Safety abort.")
            self.write_byte_data(GPU_REG_RED, red)
            self.write_byte_data(GPU_REG_GREEN, green)
            self.write_byte_data(GPU_REG_BLUE, blue)
            self.write_byte_data(GPU_REG_MODE, mode)
            self.write_byte_data(GPU_REG_APPLY, 0x01)
        finally:
            self.close()

    def set_mode(self, mode: int):
        self.open()
        try:
            if not self.verify():
                raise RuntimeError("GPU Aura signature mismatch (expected 0x1589). Safety abort.")
            self.write_byte_data(GPU_REG_MODE, mode)
            self.write_byte_data(GPU_REG_APPLY, 0x01)
        finally:
            self.close()


class MoboAuraController:
    """Controls the ASUS ROG STRIX Z270E GAMING Aura controller via NCT6793D Super I/O SMBus."""

    def __init__(self, base_port: int = MOBO_PORT_BASE, dry_run: bool = False,
                 port_path: str = "/dev/port"):
        self.base_port = base_port
        self.dry_run = dry_run
        self.port_path = port_path
        self.port_file: Optional[Any] = None

    def open(self):
        if self.dry_run or self.port_file is not None:
            return  # idempotent: an owned file (and its lock) is never replaced
        try:
            port_file = open(self.port_path, "r+b", buffering=0)
        except PermissionError:
            raise PermissionError("Accessing /dev/port requires root privileges. Please run with sudo.")
        except FileNotFoundError:
            raise FileNotFoundError("/dev/port not found on this Linux system.")
        try:
            lock_device(port_file.fileno(), self.port_path)
        except BaseException:
            port_file.close()
            raise
        self.port_file = port_file

    def close(self):
        if self.port_file:
            self.port_file.close()
            self.port_file = None

    def _smbus_write_word(self, cmd: int, word: int):
        p = self.port_file
        b = self.base_port
        # Soft reset
        p.seek(b + 0x4); p.write(b'\x40')
        # Address + Write (0x4e << 1 | 0)
        p.seek(b + 0x5); p.write(bytes([(MOBO_SLAVE_ADDR << 1) | 0]))
        # Command register index
        p.seek(b + 0x3); p.write(bytes([cmd]))
        # Low byte then high byte
        p.seek(b + 0x0); p.write(bytes([word & 0xff]))
        p.seek(b + 0x0); p.write(bytes([(word >> 8) & 0xff]))
        # Command: WRITE_WORD (0x09)
        p.seek(b + 0x2); p.write(b'\x09')
        # Start (0x80)
        p.seek(b + 0x4); p.write(b'\x80')

        # Wait for completion
        for _ in range(150):
            p.seek(b + 0xe)
            if (p.read(1)[0] & 0x04) == 0:
                break
            time.sleep(0.001)

    def _smbus_write_byte(self, cmd: int, val: int):
        p = self.port_file
        b = self.base_port
        p.seek(b + 0x4); p.write(b'\x40')
        p.seek(b + 0x5); p.write(bytes([(MOBO_SLAVE_ADDR << 1) | 0]))
        p.seek(b + 0x3); p.write(bytes([cmd]))
        p.seek(b + 0x0); p.write(bytes([val]))
        p.seek(b + 0x2); p.write(b'\x08') # WRITE_BYTE
        p.seek(b + 0x4); p.write(b'\x80')
        for _ in range(150):
            p.seek(b + 0xe)
            if (p.read(1)[0] & 0x04) == 0:
                break
            time.sleep(0.001)

    def _smbus_read_byte(self, cmd: int) -> int:
        p = self.port_file
        b = self.base_port
        p.seek(b + 0x4); p.write(b'\x40')
        p.seek(b + 0x5); p.write(bytes([(MOBO_SLAVE_ADDR << 1) | 1]))
        p.seek(b + 0x3); p.write(bytes([cmd]))
        p.seek(b + 0x2); p.write(b'\x00') # READ_BYTE
        p.seek(b + 0x4); p.write(b'\x80')
        for _ in range(150):
            p.seek(b + 0xe)
            if (p.read(1)[0] & 0x04) == 0:
                break
            time.sleep(0.001)
        p.seek(b + 0x0)
        return p.read(1)[0]

    def ene_read_reg(self, reg: int) -> int:
        if self.dry_run:
            return 0x00
        swapped = ((reg << 8) & 0xff00) | ((reg >> 8) & 0x00ff)
        self._smbus_write_word(0x00, swapped)
        return self._smbus_read_byte(0x81)

    def ene_write_reg(self, reg: int, val: int):
        if self.dry_run:
            print(f"[DRY-RUN] Mobo Write: Reg 0x{reg:04x} <= 0x{val:02x}")
            return
        swapped = ((reg << 8) & 0xff00) | ((reg >> 8) & 0x00ff)
        self._smbus_write_word(0x00, swapped)
        self._smbus_write_byte(0x01, val)

    def verify(self) -> str:
        """Safety gate: Verify the ENE firmware identifier 'LED-0116' before executing any writes."""
        if self.dry_run:
            return MOBO_EXPECTED_NAME
        name_bytes = bytearray()
        for i in range(16):
            b = self.ene_read_reg(MOBO_REG_NAME + i)
            if b == 0:
                break
            name_bytes.append(b)
        name_str = name_bytes.decode('latin1', errors='ignore').strip()
        if not name_str.startswith("LED-0116"):
            raise RuntimeError(f"Motherboard Aura firmware identifier mismatch: got '{name_str}', expected 'LED-0116'. Safety abort.")
        return name_str

    def get_status(self) -> Dict[str, Any]:
        self.open()
        try:
            fw_name = self.verify()
            led_count = self.ene_read_reg(MOBO_REG_CONFIG + 0x02) if not self.dry_run else 5
            if led_count > 5:
                raise RuntimeError(f"Motherboard reports {led_count} LEDs but the color buffers hold 5. Safety abort.")
            mode_val = self.ene_read_reg(MOBO_REG_MODE) if not self.dry_run else 5
            speed_val = self.ene_read_reg(MOBO_REG_SPEED) if not self.dry_run else 0
            dir_val = self.ene_read_reg(MOBO_REG_DIRECTION) if not self.dry_run else 0
            direct = (self.ene_read_reg(MOBO_REG_DIRECT) != 0) if not self.dry_run else False
            # Report the configured color of the buffer the board is actually using.
            color_base = MOBO_REG_COLORS_DIRECT if direct else MOBO_REG_COLORS_EFFECT

            # Read channels
            zones = []
            for i in range(led_count):
                ch = self.ene_read_reg(MOBO_REG_CONFIG + 0x13 + i) if not self.dry_run else (0x85 if i < 4 else 0x86)
                ch_name = MOBO_CHANNELS.get(ch, f"Unknown(0x{ch:02x})")
                r = self.ene_read_reg(color_base + (i * 3)) if not self.dry_run else 0
                b = self.ene_read_reg(color_base + (i * 3) + 1) if not self.dry_run else 0
                g = self.ene_read_reg(color_base + (i * 3) + 2) if not self.dry_run else 0
                zones.append({
                    "led_index": i,
                    "channel": f"0x{ch:02x}",
                    "channel_name": ch_name,
                    "rgb": [r, g, b],
                    "hex": f"{r:02x}{g:02x}{b:02x}"
                })

            return {
                "device": "ASUS ROG STRIX Z270E GAMING Motherboard",
                "super_io": "Nuvoton NCT6793D",
                "smbus_port": f"0x{self.base_port:04x}",
                "slave_address": f"0x{MOBO_SLAVE_ADDR:02x}",
                "firmware": fw_name,
                "led_count": led_count,
                "mode": MOBO_MODE_NAMES.get(mode_val, f"unknown({mode_val})"),
                "speed": speed_val,
                "direction": "forward" if dir_val == 0 else "reverse",
                "color_source": "direct" if direct else "effect",
                "zones": zones
            }
        finally:
            self.close()

    def set_color_static(self, red: int, green: int, blue: int):
        """Set all 5 onboard LEDs to static color (Note: ENE hardware uses Red-Blue-Green byte order)."""
        self.open()
        try:
            self.verify()
            # Direct mode enable
            self.ene_write_reg(MOBO_REG_DIRECT, 0x01)
            # Write 5 LEDs (Order: R, B, G)
            for i in range(5):
                self.ene_write_reg(MOBO_REG_COLORS_DIRECT + (i * 3) + 0, red)
                self.ene_write_reg(MOBO_REG_COLORS_DIRECT + (i * 3) + 1, blue)
                self.ene_write_reg(MOBO_REG_COLORS_DIRECT + (i * 3) + 2, green)

            # Mode 1 (Static)
            self.ene_write_reg(MOBO_REG_MODE, 1)
            self.ene_write_reg(MOBO_REG_SPEED, 0)
            self.ene_write_reg(MOBO_REG_DIRECTION, 0)
            self.ene_write_reg(MOBO_REG_APPLY, 0x01)
        finally:
            self.close()

    def set_mode(self, mode: int, speed: int = 0, direction: int = 0):
        """Mode-only transition. Anything but static leaves direct mode first: a set direct
        flag keeps the board on the static direct buffer. Static keeps the current buffer."""
        self.open()
        try:
            self.verify()
            if mode != MOBO_MODES["static"]:
                self.ene_write_reg(MOBO_REG_DIRECT, 0x00)
            self._write_mode(mode, speed, direction)
        finally:
            self.close()

    def set_effect(self, mode: int, speed: int = 0, color: Optional[Tuple[int, int, int]] = None,
                   direction: int = 0):
        """Animated effect; `color` fills the effect color buffer (red, blue, green order)."""
        self.open()
        try:
            self.verify()
            self.ene_write_reg(MOBO_REG_DIRECT, 0x00)
            if color is not None:
                red, green, blue = color
                for i in range(5):
                    self.ene_write_reg(MOBO_REG_COLORS_EFFECT + (i * 3) + 0, red)
                    self.ene_write_reg(MOBO_REG_COLORS_EFFECT + (i * 3) + 1, blue)
                    self.ene_write_reg(MOBO_REG_COLORS_EFFECT + (i * 3) + 2, green)
            self._write_mode(mode, speed, direction)
        finally:
            self.close()

    def _write_mode(self, mode: int, speed: int, direction: int):
        self.ene_write_reg(MOBO_REG_MODE, mode)
        self.ene_write_reg(MOBO_REG_SPEED, speed)
        self.ene_write_reg(MOBO_REG_DIRECTION, direction)
        self.ene_write_reg(MOBO_REG_APPLY, 0x01)


def _mode_arg(resolver):
    """argparse `type=` wrapper turning a resolver's ValueError into a usage error."""
    def parse(value):
        try:
            resolver(value)
        except ValueError as e:
            raise argparse.ArgumentTypeError(str(e))
        return value
    return parse


def main():
    parser = argparse.ArgumentParser(description="ASUS Aura Sync Hardware RGB Controller for Linux")
    parser.add_argument("--dry-run", action="store_true", help="Print planned register transactions without writing")
    parser.add_argument("--json", action="store_true", help="Output results in JSON format")

    subparsers = parser.add_subparsers(dest="command", required=True)

    # Subcommand: status
    p_status = subparsers.add_parser("status", help="Read and print status of GPU and Motherboard LEDs")
    p_status.add_argument("--json", action="store_true", help="Output results in JSON format")

    # Subcommand: gpu
    p_gpu = subparsers.add_parser("gpu", help="Control GPU LEDs")
    p_gpu.add_argument("--color", type=str, help="Hex color code (e.g. ff0055 or 00ffcc)")
    p_gpu.add_argument("--mode", type=_mode_arg(resolve_gpu_mode), default="static", help="Lighting mode (off, static, breathing, flashing, spectrum; rainbow and color-cycle mean spectrum)")
    p_gpu.add_argument("--off", action="store_true", help="Turn GPU LEDs off")

    # Subcommand: mobo
    p_mobo = subparsers.add_parser("mobo", help="Control Motherboard LEDs")
    p_mobo.add_argument("--color", type=str, help="Hex color code (e.g. ff0055 or 00ffcc)")
    p_mobo.add_argument("--mode", type=_mode_arg(resolve_mobo_mode), default="static", help="Lighting mode (native names or aliases such as comet, starry-night)")
    p_mobo.add_argument("--speed", type=int, choices=[0, 1, 2, 3, 4], default=0, help="Effect speed (0=fastest, 4=slowest)")
    p_mobo.add_argument("--off", action="store_true", help="Turn Motherboard LEDs off")

    # Subcommand: all
    p_all = subparsers.add_parser("all", help="Control both GPU and Motherboard LEDs synchronously")
    p_all.add_argument("--color", type=str, help="Hex color code; required for static mode, optional for effects")
    p_all.add_argument("--mode", type=_mode_arg(resolve_all_mode), default="static", help="Lighting mode (static, breathing, rainbow, wave, ...)")
    p_all.add_argument("--off", action="store_true", help="Turn all LEDs off")

    args = parser.parse_args()

    gpu = GpuAuraController(dry_run=args.dry_run)
    mobo = MoboAuraController(dry_run=args.dry_run)

    if args.command == "status":
        gpu_stat = gpu.get_status()
        mobo_stat = mobo.get_status()
        combined = {"gpu": gpu_stat, "motherboard": mobo_stat}
        if getattr(args, 'json', False):
            print(json.dumps(combined, indent=2))
        else:
            print("==================================================")
            print("  ASUS Aura Hardware Status (Live)")
            print("==================================================")
            print(f"[GPU]  {gpu_stat['device']}")
            print(f"       Bus: {gpu_stat['bus']} @ {gpu_stat['address']} (Magic 0x1589 Verified)")
            print(f"       Mode: {gpu_stat['mode']}, Color: #{gpu_stat['color_hex']}")
            print("--------------------------------------------------")
            print(f"[MOBO] {mobo_stat['device']}")
            print(f"       Super I/O Port: {mobo_stat['smbus_port']} @ {mobo_stat['slave_address']} ({mobo_stat['firmware']} Verified)")
            print(f"       Mode: {mobo_stat['mode']}, Speed: {mobo_stat['speed']}, Direction: {mobo_stat['direction']}")
            for z in mobo_stat['zones']:
                print(f"       - LED {z['led_index']} ({z['channel_name']}): #{z['hex']}")
            print("==================================================")

    elif args.command == "gpu":
        if args.off:
            gpu.set_mode(GPU_MODES["off"])
            print("[GPU] LEDs turned off.")
        else:
            mode_name = resolve_gpu_mode(args.mode)
            mode_code = GPU_MODES[mode_name]
            if args.color:
                r, g, b = parse_hex_color(args.color)
                gpu.set_color(r, g, b, mode=mode_code)
                if mode_name == "spectrum":
                    print("[GPU] Spectrum generates its own colors; --color ignored.")
                else:
                    print(f"[GPU] Color set to #{args.color.lstrip('#')} (Mode: {mode_name}).")
            else:
                gpu.set_mode(mode_code)
                print(f"[GPU] Mode set to {mode_name}.")

    elif args.command == "mobo":
        if args.off:
            mobo.set_mode(MOBO_MODES["off"])
            print("[MOBO] LEDs turned off.")
        else:
            mode_name = resolve_mobo_mode(args.mode)
            mode_code = MOBO_MODES[mode_name]
            rgb = parse_hex_color(args.color) if args.color else None
            if rgb and mode_name == "static":
                mobo.set_color_static(*rgb)
                print(f"[MOBO] Static color set to #{args.color.lstrip('#')}.")
            elif rgb and mode_name in MOBO_COLOR_MODES:
                mobo.set_effect(mode_code, speed=args.speed, color=rgb)
                print(f"[MOBO] Mode set to {mode_name} with color #{args.color.lstrip('#')} (Speed: {args.speed}).")
            else:
                if rgb:
                    print("warning: --color is ignored for effect modes that generate their own colors", file=sys.stderr)
                mobo.set_mode(mode_code, speed=args.speed)
                print(f"[MOBO] Mode set to {mode_name} (Speed: {args.speed}).")

    elif args.command == "all":
        if args.off:
            gpu.set_mode(GPU_MODES["off"])
            mobo.set_mode(MOBO_MODES["off"])
            print("[ALL] GPU and Motherboard LEDs turned off.")
        else:
            rgb = parse_hex_color(args.color) if args.color else None
            gpu_name, mobo_name = resolve_all_mode(args.mode)
            if rgb is None and mobo_name == "static":
                parser.error("--color is required for static mode")
            hex_color = (args.color or "").lstrip("#")
            if rgb:
                gpu.set_color(*rgb, mode=GPU_MODES[gpu_name])
            else:
                gpu.set_mode(GPU_MODES[gpu_name])
            if rgb and mobo_name == "static":
                mobo.set_color_static(*rgb)
            elif rgb and mobo_name in MOBO_COLOR_MODES:
                mobo.set_effect(MOBO_MODES[mobo_name], color=rgb)
            else:
                mobo.set_effect(MOBO_MODES[mobo_name])
            if rgb and (mobo_name == "static" or mobo_name in MOBO_COLOR_MODES):
                print(f"[ALL] Synchronized GPU and Motherboard to #{hex_color} (Mode: {args.mode}).")
            elif rgb:
                print(f"[ALL] GPU mode {gpu_name}; motherboard mode {mobo_name} (hardware-generated colors; --color ignored).")
            else:
                print(f"[ALL] GPU mode {gpu_name}; motherboard mode {mobo_name} (colors unchanged).")


if __name__ == "__main__":
    main()
