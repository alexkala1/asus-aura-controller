# Dev Crew Goal: Build Standalone Rust ASUS Aura Controller

Goal: Implement standalone Rust CLI utility `asus-aura-cli` to control ASUS GTX 1060 and ROG STRIX Z270E motherboard LEDs.
Context: Working reference in aura_ctl.py and REA specs in REA_ANALYSIS.md. GTX 1060 is on `/dev/i2c-0` slave `0x29` (magic `0x1589`). Z270E Aura controller is ENE `LED-0116` at NCT6793D Super I/O port base `0x02c0` slave `0x4e` with 5 LEDs (4 Back I/O, 1 RGB Header) and RBG byte order.
Constraints: Rust edition 2021; clap (derive), serde, serde_json; no external C libraries; enforce magic signature validation before writing; provide `--dry-run` and `--json`; match subcommands `status`, `gpu`, `mobo`, `all`.
Done when: `cargo test` passes; `cargo build --release` creates working executable `target/release/asus-aura-cli`; `./target/release/asus-aura-cli --dry-run status --json` outputs valid JSON matching aura_ctl.py schema.
Out of scope: Windows GUI, systemd background daemon, DRAM RGB remapping.
Assumptions: Binary runs via sudo or with `setcap cap_sys_rawio+ep target/release/asus-aura-cli`.
