//! Drives `asus-aura-cli --dry-run mcp` over real stdio, like an MCP client would.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_asus-aura-cli"))
            .args(["--dry-run", "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mcp server");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Every stdout line must be one JSON-RPC message; dry-run traces would break this.
    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {line:?}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let reply = self.read();
        assert_eq!(reply["id"], id);
        reply
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))["result"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

#[test]
fn full_session_keeps_stdout_clean_json_rpc() {
    let mut client = Client::start();
    let init = client.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "asus-aura");
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let tools = client.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "aura_get_status",
            "aura_set_color",
            "aura_set_effect",
            "aura_turn_off",
            "aura_agent_state"
        ]
    );

    // These all write registers, whose dry-run traces must go to stderr.
    let color = client.tool(
        "aura_set_color",
        json!({"target": "mobo", "zone": 4, "color": "#ff0055"}),
    );
    assert_eq!(color["isError"], false);
    assert_eq!(
        color["structuredContent"]["applied"]["motherboard"]["zone"],
        4
    );
    let off = client.tool("aura_turn_off", json!({}));
    assert_eq!(off["structuredContent"]["applied"]["gpu"]["mode"], "off");

    let status = client.tool("aura_get_status", json!({}));
    assert_eq!(status["structuredContent"]["gpu"]["magic_verified"], true);

    let topology = client.request("resources/read", json!({"uri": "aura://hardware/topology"}));
    assert!(topology["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("NCT6793D"));
}

#[test]
fn agent_state_reverts_by_itself() {
    let mut client = Client::start();
    let failure = client.tool(
        "aura_agent_state",
        json!({"state": "failure", "duration_seconds": 1}),
    );
    assert_eq!(failure["structuredContent"]["pattern"], "red_strobe");
    let held = client.tool("aura_get_status", json!({}));
    assert_eq!(held["structuredContent"]["agent_state"]["state"], "failure");

    // The server's own timer fires with no client traffic in between.
    std::thread::sleep(Duration::from_millis(1400));
    let after = client.tool("aura_get_status", json!({}));
    assert_eq!(after["structuredContent"]["agent_state"], Value::Null);
}

#[test]
fn malformed_input_gets_an_error_and_the_server_keeps_running() {
    let mut client = Client::start();
    client
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"not json\n\n")
        .unwrap();
    assert_eq!(client.read()["error"]["code"], -32700);
    assert_eq!(client.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn oversized_message_is_discarded_then_ping_succeeds() {
    let mut client = Client::start();
    client
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&vec![b'x'; 1024 * 1024 + 1])
        .unwrap();
    // The error arrives even before the oversized stream ends.
    let error = client.read();
    assert_eq!(error["error"]["code"], -32600);
    assert_eq!(error["error"]["message"], "message too large");
    assert_eq!(error["id"], Value::Null);
    client
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"discard this too\n")
        .unwrap();
    assert_eq!(client.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn signals_restore_baseline_and_exit_cleanly() {
    for signal in ["-TERM", "-INT"] {
        let mut client = Client::start();
        assert_eq!(
            client.tool("aura_agent_state", json!({"state": "working"}))["isError"],
            false
        );
        assert!(Command::new("kill")
            .args([signal, &client.child.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = client.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                client.child.kill().unwrap();
                panic!("server did not shut down promptly");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success());
        let mut stderr = String::new();
        client
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(stderr.contains("shutdown signal received"));
        assert!(stderr.contains("clean shutdown"));
        // Dry-run baseline is GPU off and motherboard rainbow.
        assert!(stderr.contains("GPU Write: Reg 0x07 <= 0x00"));
        assert!(stderr.contains("Mobo Write: Reg 0x8021 <= 0x05"));
    }
}

#[test]
fn static_without_color_only_writes_motherboard_mode_registers() {
    let mut client = Client::start();
    let result = client.tool(
        "aura_set_effect",
        json!({"target": "mobo", "mode": "static"}),
    );
    assert_eq!(result["isError"], false);
    client.stdin.take();
    assert!(client.child.wait().unwrap().success());
    let mut stderr = String::new();
    client
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let writes: Vec<_> = stderr
        .lines()
        .filter(|line| line.contains("Mobo Write:"))
        .collect();
    assert_eq!(
        writes,
        [
            "[DRY-RUN] Mobo Write: Reg 0x8021 <= 0x01",
            "[DRY-RUN] Mobo Write: Reg 0x8022 <= 0x00",
            "[DRY-RUN] Mobo Write: Reg 0x8023 <= 0x00",
            "[DRY-RUN] Mobo Write: Reg 0x80a0 <= 0x01",
        ]
    );
}
