//! Drives `asus-aura-cli --dry-run mcp` over real stdio, like an MCP client would.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

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
