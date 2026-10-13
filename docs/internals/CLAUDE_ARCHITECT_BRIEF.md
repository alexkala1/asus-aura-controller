# Claude Architect & Reviewer Brief: ASUS Aura Controller

## Role & Mandate
You are the architect and adversarial reviewer for the ASUS Aura Controller project (the repository root).
Your responsibilities:
1. Review implementation plans and code submitted by Codex.
2. Ensure hardware safety bounds: verify that register writes never touch undocumented memory spaces or invalid ports.
3. Validate acceptance criteria via automated non-destructive test scripts.
4. Enforce clean CLI UX, proper error reporting, and zero reliance on Windows emulation or external daemons.

## Hardware Architectural Contracts

### 1. GPU (GTX 1060 via `/dev/i2c-0`)
- Bus: Linux I2C adapter `/dev/i2c-0`
- Slave Address: `0x29`
- Safety Check Before Any Write:
  - Must read `0x20` and `0x21` and verify that `(val[0x20] << 8) | val[0x21] == 0x1589`.
  - If magic value does not match, abort immediately with error code.
- Supported Modes:
  - `0`: Off
  - `1`: Static (`red`, `green`, `blue`)
  - `2`: Breathing
  - `3`: Flashing
  - `4`: Spectrum Cycle

### 2. Motherboard (STRIX Z270E via NCT6793D Port 0x02c0)
- Super I/O: Logical Device `0x0B` at port `0x02c0`
- Slave Address: `0x4e`
- Safety Check Before Any Write:
  - Must read 8 bytes at register `0x1000` and verify that the string starts with `LED-0116`.
  - If device string does not match, abort immediately with error code.
- Zones supported:
  - Back I/O shroud (`0x85`)
  - RGB Headers (`0x86`, `0x87`)
  - Audio (`0x84`)

## Verification Acceptance Commands
Your review is PASS only if:
1. `python3 -m unittest discover tests` passes with 100% test coverage for packet framing and safety validators.
2. Non-destructive status probe runs cleanly:
   `sudo ./aura-ctl status --json`
   Outputs valid JSON containing GPU (`0x1589` verified) and Motherboard (`LED-0116` verified) state.
3. Dry-run mode works without requiring sudo or mutating hardware:
   `./aura-ctl gpu --color 00ffcc --dry-run`
   `./aura-ctl mobo --color ff00aa --dry-run`
