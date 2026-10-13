# Contributing

Thanks for helping. This project writes to real hardware registers, so the bar for changes that touch device access is higher than usual: every write path needs an identity check and a test that does not need the hardware.

## Development setup

Needs Linux, a recent stable Rust toolchain (`rustup`; developed on 1.98) and Python 3 for the helper tests.

```sh
git clone https://github.com/alexkala/asus-aura-controller && cd asus-aura-controller
rustup component add rustfmt clippy
cargo build
```

Run these before every push (CI runs the same):

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test                                  # unit tests, CLI dry-run tests, MCP stdio tests
python3 -m unittest tests.test_aura         # Python reference implementation
```

The web UI tests use Python 3.12 and Playwright Chromium, with the same pinned dependencies as CI:

```sh
uv venv --python 3.12
uv pip install -r requirements-ui.txt
uv run --no-project python -m playwright install chromium
uv run --no-project pytest tests/test_ui.py
```

On Ubuntu, add `--with-deps` to the Playwright install command to install its system dependencies. The tests regenerate `web/screenshot*.png`; commit those only when the UI changed.

To run the checks automatically, save this as `.git/hooks/pre-commit` and `chmod +x` it:

```sh
#!/bin/sh
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

## Testing hardware safely

1. **Default to `--dry-run`.** `asus-aura-cli --dry-run <command>` runs the real command path with a fake transport and prints each register write to stdout (to stderr under `mcp`). Use it for every change first. `--dry-run mcp` works the same way for the MCP server.
2. **Unit-test against mock registers.** `src/gpu.rs` and `src/mobo.rs` have mock transports that record every register write in order; assert exact write sequences there rather than "it looked right on my LEDs".
3. **Only then go live,** on supported hardware, with this order: `status` (read-only), one static color on one device, then modes. Keep a way to turn the lights off (`asus-aura-cli all --off`) within reach.
4. **Never weaken an identity check** (GPU magic, motherboard firmware prefix) to get a test or a board working. Add new identities next to the existing ones instead.
5. Do not run other RGB tools against the same buses at the same time.

## Adding support for a new board or GPU

Nothing besides the two tested devices is supported, and boards that merely share a Super I/O chip (NCT6791D / NCT6793D / NCT6796D / NCT6798D) are not known to work. Support is added by evidence, in this order:

1. **Open an issue first** with the hardware report. Redact serial numbers, UUIDs and MAC addresses before posting:
   ```sh
   sudo dmidecode -t baseboard | grep -Ev 'Serial|Asset'      # board name and version
   lspci -nn | grep -i -E 'vga|nvidia'                       # GPU and PCI IDs
   i2cdetect -l                                              # which bus is which adapter
   grep . /sys/class/hwmon/hwmon*/name                       # Super I/O driver, e.g. nct6793
   ```
2. **Read-only probes only.** Use `i2cdetect -y -r <bus>` (read mode) and `i2cget` on the suspected address to read the device's identity registers. Do not write to an unidentified address; on some buses that can corrupt DIMM SPD or other EEPROMs. Record the identity values and the exact tool and kernel versions.
3. **Register dump.** Share a read-only dump of the registers the driver would use (identity, mode, speed, color buffers) in the issue: raw bytes, with the byte order noted (ENE buffers are red, blue, green).
4. **Implement the identity gate first.** New constants live in `src/protocol.rs`; the controller must refuse to write unless the identity matches. Make bus, port base and slave address explicit rather than guessing.
5. **Mock-register tests** reproducing your dump: the identity pass, the identity mismatch (asserting that no writes happen), and the exact write sequence for static color and one effect.
6. **Hardware verification** in the PR description: board and BIOS version, kernel, the commands you ran in the order from "Testing hardware safely", what you observed, and how you restored the original lighting. Include the `--dry-run` output next to the live one.
7. Update the support table in `README.md`, marking the device as tested by you and the date.

## Pull requests

- Keep changes small and focused; one device or one behavior per PR.
- Match the surrounding code style; `cargo fmt` is the formatter.
- New behavior needs a test. Anything that changes a register write needs a mock-register test that shows the new write order.
- Describe what you ran and on what hardware. "Dry-run only" is fine and expected for most changes; say so.
- Be kind in reviews. Security issues go to the process in [SECURITY.md](SECURITY.md), not a public issue.
