# Dev Crew Goal: Build Linux ASUS Aura RGB Controller

Goal: Build standalone Linux CLI utility `aura-ctl` to control ASUS GTX 1060 and ROG STRIX Z270E motherboard LEDs.
Context: Reverse engineered ASUS Aura 1.07.84 binaries (REA dossier in REA_ANALYSIS.md). GTX 1060 is on `/dev/i2c-0` slave `0x29` (magic `0x1589`). Z270E Aura controller is ENE `LED-0116` at NCT6793D Super I/O port base `0x02c0` slave `0x4e`. Specs in CODEX_BUILDER_BRIEF.md and CLAUDE_ARCHITECT_BRIEF.md.
Constraints: Python 3 standard library only (no external dependencies, no pip requirements, no Wine, no daemons); require root only for hardware I/O; provide `--dry-run`; enforce hardware magic signature validation before any write.
Done when: `python3 -m unittest discover tests` passes; `sudo ./aura-ctl status` reads live hardware IDs (`0x1589` and `LED-0116`); `./aura-ctl --dry-run all --color ff0055` displays planned hardware transactions without error.
Out of scope: Windows GUI, systemd background daemon, DRAM RGB remapping.
Assumptions: User runs CLI via sudo when communicating with hardware `/dev/port` and `/dev/i2c-0`.
