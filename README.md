# ASUS Aura CLI

Standalone Rust CLI for the ASUS ROG Strix GeForce GTX 1060 and ROG STRIX Z270E GAMING motherboard on Linux.

Build and test:

```sh
cargo build --release
cargo test
```

Preview commands without opening hardware devices:

```sh
./target/release/asus-aura-cli --dry-run status
./target/release/asus-aura-cli --dry-run --json status
./target/release/asus-aura-cli --dry-run gpu --color ff0055 --mode static
./target/release/asus-aura-cli --dry-run mobo --color 112233
./target/release/asus-aura-cli --dry-run mobo --mode rainbow-breathing --speed 2
./target/release/asus-aura-cli --dry-run all --color 00ffcc --mode breathing
./target/release/asus-aura-cli --dry-run all --off
```

`--off` takes precedence over color and mode. GPU and motherboard commands default to static mode; motherboard speed accepts 0–4 (0 is fastest). `all` requires `--color` unless `--off` is present. Modes without a GPU equivalent use static GPU color; non-static motherboard modes set the effect without changing its color buffer. Dry-run status uses canned values, not live readings.

For live access, omit `--dry-run`. GPU access uses `/dev/i2c-0` and requires the `i2c-dev` module and suitable permissions. The group-access rule in [udev/99-asus-aura.rules](udev/99-asus-aura.rules) allows members of the `i2c` group to access I2C devices. Motherboard access uses `/dev/port` and requires root; use `sudo` for motherboard or combined commands. The udev rule does not grant `/dev/port` access.

Every controller operation checks its device identity before changing lighting: GPU magic must be `0x1589`, and motherboard firmware must start with `LED-0116`. Mismatches abort with an error. Motherboard ENE buffers store **RBG** bytes (red, blue, green); the driver packs writes and converts status back to RGB automatically.

## MCP server (`asus-aura-cli mcp`)

Lets coding agents (Claude, Codex, Agy, Cursor) read and drive the lighting over MCP: newline-delimited JSON-RPC 2.0 on stdio, protocol versions 2025-06-18, 2025-03-26 and 2024-11-05. stdout carries protocol messages only; logs and `--dry-run` register traces go to stderr.

| Tool | Arguments | Notes |
| --- | --- | --- |
| `aura_get_status` | none | Device state, firmware, active agent hold. A device that cannot be opened reports `{"error": ...}` instead of failing the call. |
| `aura_set_color` | `color`, `target` (all/gpu/mobo), `zone` (0–4, motherboard only) | |
| `aura_set_effect` | `mode`, `color?`, `target?`, `speed?` (0–4, 0 fastest, motherboard only) | The GPU supports static, breathing, strobing, color-cycle. Other modes fall back to static GPU color and the response `notes` say so. |
| `aura_turn_off` | `target?` | |
| `aura_agent_state` | `state` (working/reviewing/success/failure/idle), `duration_seconds?` | working = amber breathe, reviewing = cyan wave (GPU holds static cyan), success = green pulse, failure = red strobe. The lighting in place beforehand is saved and restored on `idle`, after 3 s for success/failure, or after 15 min for working/reviewing (watchdog for a dead agent). An explicit set/off call cancels the hold. |

Resources: `aura://hardware/topology` (static) and `aura://state/current` (live read).

```sh
./target/release/asus-aura-cli --dry-run mcp          # try it without touching hardware
cargo test --test mcp_stdio                           # JSON-RPC session tests over real stdio
```

Client config snippets are in [mcp/](mcp/): copy `claude_desktop_config.json` into Claude Desktop's config, and `antigravity-aura.json` into `~/.gemini/antigravity-cli/mcp/`. The motherboard needs `/dev/port` (root), so the live config runs the binary through `sudo -n`. Limit that to this exact command instead of the whole binary:

```sh
echo "alexkala ALL=(root) NOPASSWD: /home/alexkala/repos/asus-aura-controller/target/release/asus-aura-cli mcp" | sudo tee /etc/sudoers.d/99-aura-mcp
```

Any process that can start the server can then change your lighting as root-level hardware writes, so only register it with agents you trust. Without root the GPU still works (i2c group) and `aura_agent_state` degrades to the GPU, reporting the motherboard under `warnings`.

Hardware status: the new motherboard writes (per-zone color, effect-color buffer at `0x8010`, snapshot/restore) are covered by mock-register tests only and have not been run on the Z270E. Check them with the dry-run server and one live `aura_set_effect` call before relying on agent states. The effect buffer holds 5 LEDs, so the driver never writes past `0x801F`.
