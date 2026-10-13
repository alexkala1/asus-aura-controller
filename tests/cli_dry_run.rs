use std::process::{Command, Output};

fn dry_run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_luminactl"))
        .arg("--dry-run")
        .args(args)
        .output()
        .expect("run the built CLI")
}

fn successful_stdout(args: &[&str]) -> String {
    let output = dry_run(args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "unexpected stderr for {args:?}");
    String::from_utf8(output.stdout).expect("UTF-8 stdout")
}

#[test]
fn status_json_reports_verified_devices_and_five_zones() {
    let stdout = successful_stdout(&["status", "--json"]);
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("pure JSON stdout");
    let gpu = &data["gpu"];
    for key in [
        "device",
        "bus",
        "address",
        "magic_verified",
        "mode",
        "color_hex",
        "rgb",
    ] {
        assert!(gpu.get(key).is_some(), "missing GPU key {key}");
    }
    assert_eq!(gpu["magic_verified"], true);
    assert_eq!(gpu["address"], "0x29");
    let mobo = &data["motherboard"];
    for key in [
        "device",
        "super_io",
        "smbus_port",
        "slave_address",
        "firmware",
        "led_count",
        "mode",
        "speed",
        "direction",
        "zones",
    ] {
        assert!(mobo.get(key).is_some(), "missing motherboard key {key}");
    }
    assert!(mobo["firmware"].as_str().unwrap().starts_with("LED-0116"));
    assert_eq!(mobo["led_count"], 5);
    let zones = mobo["zones"].as_array().expect("zones array");
    assert_eq!(zones.len(), 5);
    for (index, zone) in zones.iter().enumerate() {
        assert_eq!(zone["led_index"], index);
        assert_eq!(zone["channel"], if index < 4 { "0x85" } else { "0x86" });
        assert_eq!(
            zone["channel_name"],
            if index < 4 {
                "Back I/O"
            } else {
                "RGB Header 1"
            }
        );
        assert_eq!(zone["rgb"], serde_json::json!([0, 0, 0]));
        assert_eq!(zone["hex"], "000000");
    }
}

#[test]
fn gpu_color_writes_rgb_mode_then_apply() {
    let stdout = successful_stdout(&["gpu", "--color", "00ffcc"]);
    let writes: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("[DRY-RUN] GPU Write:"))
        .collect();
    assert_eq!(
        writes,
        [
            "[DRY-RUN] GPU Write: Reg 0x04 <= 0x00",
            "[DRY-RUN] GPU Write: Reg 0x05 <= 0xff",
            "[DRY-RUN] GPU Write: Reg 0x06 <= 0xcc",
            "[DRY-RUN] GPU Write: Reg 0x07 <= 0x01",
            "[DRY-RUN] GPU Write: Reg 0x0e <= 0x01",
        ]
    );
}

#[test]
fn mobo_color_writes_rbg_bytes() {
    let stdout = successful_stdout(&["mobo", "--color", "112233"]);
    let writes: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("[DRY-RUN] Mobo Write:"))
        .collect();
    assert_eq!(
        &writes[..4],
        [
            "[DRY-RUN] Mobo Write: Reg 0x8020 <= 0x01",
            "[DRY-RUN] Mobo Write: Reg 0x8000 <= 0x11",
            "[DRY-RUN] Mobo Write: Reg 0x8001 <= 0x33",
            "[DRY-RUN] Mobo Write: Reg 0x8002 <= 0x22",
        ]
    );
}

#[test]
fn all_off_needs_no_color_and_turns_both_devices_off() {
    let stdout = successful_stdout(&["all", "--off"]);
    assert!(stdout.contains("[ALL] GPU and Motherboard LEDs turned off."));
    assert!(stdout.contains("[DRY-RUN] GPU Write: Reg 0x07 <= 0x00"));
    assert!(stdout.contains("[DRY-RUN] Mobo Write: Reg 0x8021 <= 0x00"));
}

#[test]
fn invalid_color_fails_without_writes() {
    for command in ["gpu", "mobo", "all"] {
        let output = dry_run(&[command, "--color", "zzz"]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).starts_with("error:"));
    }
}

#[test]
fn invalid_speed_is_rejected_before_dispatch() {
    let output = dry_run(&["mobo", "--speed", "9"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("error:"));
    assert!(stderr.contains("--speed"));
}

#[test]
fn invalid_all_mode_is_a_clap_error_before_device_access() {
    let output = Command::new(env!("CARGO_BIN_EXE_luminactl"))
        .args(["all", "--color", "112233", "--mode", "invalid"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid mode"));
}

#[test]
fn ignored_motherboard_color_warns_once_and_preserves_stdout() {
    let output = dry_run(&["mobo", "--color", "112233", "--mode", "wave"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr).unwrap(),
        "warning: --color is ignored for effect modes; motherboard effects keep their stored colors\n");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        successful_stdout(&["mobo", "--mode", "wave"])
    );
}

#[test]
fn dry_run_needs_no_writable_runtime_directory() {
    let dir = std::env::temp_dir().join(format!("aura-dry-environment-{}", std::process::id()));
    assert!(!dir.exists());
    for args in [
        vec!["status"],
        vec!["gpu", "--off"],
        vec!["mobo", "--off"],
        vec!["all", "--off"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_luminactl"))
            .env("XDG_RUNTIME_DIR", &dir)
            .env("TMPDIR", &dir)
            .arg("--dry-run")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!dir.exists());
    }
}

fn mobo_writes(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix("[DRY-RUN] Mobo Write: Reg "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn all_color_effects_write_the_effect_buffer_and_leave_direct_mode() {
    for (mode, code) in [("breathing", "0x02"), ("flashing", "0x03")] {
        let stdout = successful_stdout(&["all", "--color", "112233", "--mode", mode]);
        let mut expected = vec!["0x8020 <= 0x00".to_owned()];
        for led in 0..5u16 {
            for (offset, byte) in ["0x11", "0x33", "0x22"].iter().enumerate() {
                expected.push(format!(
                    "0x{:04x} <= {byte}",
                    0x8010 + 3 * led + offset as u16
                ));
            }
        }
        expected.extend(
            [
                "0x8021 <= ",
                "0x8022 <= 0x00",
                "0x8023 <= 0x00",
                "0x80a0 <= 0x01",
            ]
            .map(|s| {
                if s.ends_with("<= ") {
                    format!("{s}{code}")
                } else {
                    s.to_owned()
                }
            }),
        );
        assert_eq!(mobo_writes(&stdout), expected, "{mode}");
        assert!(stdout.contains("Synchronized GPU and Motherboard to #112233"));
    }
}

#[test]
fn all_autonomous_effects_leave_direct_mode_and_do_not_claim_the_color() {
    let stdout = successful_stdout(&["all", "--color", "112233", "--mode", "rainbow"]);
    assert_eq!(
        mobo_writes(&stdout),
        [
            "0x8020 <= 0x00",
            "0x8021 <= 0x05",
            "0x8022 <= 0x00",
            "0x8023 <= 0x00",
            "0x80a0 <= 0x01"
        ]
    );
    assert!(stdout.contains("generates its own colors"));
    assert!(!stdout.contains("Synchronized"));
}
