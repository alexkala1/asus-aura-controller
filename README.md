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
