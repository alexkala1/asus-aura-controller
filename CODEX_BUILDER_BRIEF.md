# Codex Builder Brief: ASUS Aura Controller Implementation

## Goal
Implement a standalone Linux CLI utility (`aura-ctl` / `aura_controller.py`) in `/home/alexkala/repos/asus-aura-controller` to read and set the RGB LEDs of:
1. ASUS GeForce GTX 1060 (via `/dev/i2c-0` slave `0x29`)
2. ASUS ROG STRIX Z270E GAMING motherboard (via NCT6793D Super I/O SMBus at port `0x02c0` slave `0x4e`)

## Technical Specifications & Implementation Details

### Module 1: GPU Backend (`gpu_aura.py`)
- Standard Linux `ioctl` or `/dev/i2c-0` reads/writes using Python standard library (no external C dependencies required).
- Validation:
  - Check device exists (`/dev/i2c-0`).
  - Read registers `0x20` and `0x21`. Assert `0x15` and `0x89`.
- Color update:
  - Write `0x04`: Red
  - Write `0x05`: Green
  - Write `0x06`: Blue
  - Write `0x07`: Mode (default 1 for static)
  - Write `0x0E`: `0x01` (apply)

### Module 2: Motherboard Backend (`mobo_aura.py`)
- Super I/O port interface via `/dev/port` (requires root privileges) or `iopl`.
- Configuration:
  - Base port: `0x02c0`
  - Slave address: `0x4e`
  - Verification: Read 8 bytes starting at `0x1000`. Assert starts with `LED-0116`.
- Register access functions:
  - `ene_read_reg(reg)`:
    - Write word `((reg << 8) & 0xff00) | ((reg >> 8) & 0x00ff)` to cmd `0x00`
    - Read byte from cmd `0x81`
  - `ene_write_reg(reg, val)`:
    - Write word `((reg << 8) & 0xff00) | ((reg >> 8) & 0x00ff)` to cmd `0x00`
    - Write byte `val` to cmd `0x01`
- Color update:
  - Set direct mode: `ene_write_reg(0x8020, 0x01)`
  - Write RGB triplets into `0x8000` buffer
  - Apply changes: `ene_write_reg(0x80a0, 0x01)`

### Module 3: CLI Frontend (`aura-ctl`)
- Single entrypoint with subcommands:
  - `status`: Reads current colors and magic IDs, prints clean table/JSON.
  - `gpu`: Controls GPU (`--color RRGGBB`, `--mode static|breathing|flashing|cycle`, `--off`).
  - `mobo`: Controls Motherboard (`--color RRGGBB`, `--mode ...`, `--off`).
  - `all`: Syncs both GPU and Motherboard to the same color and effect simultaneously!
  - `--dry-run`: Prints planned I2C/SMBus transactions without writing.

### Module 4: Unit Test Suite (`tests/`)
- Mocks `/dev/i2c-0` and `/dev/port`.
- Tests magic signature verification, command encoding, hex color parsing, and error abort mechanisms.
