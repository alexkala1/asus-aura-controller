//! Drives `asus-aura-cli --dry-run mcp` over real stdio, like an MCP client would.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const STDERR_KEEP: usize = 1 << 20;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Test client with bounded waits: stderr is drained continuously (so a chatty server
/// can never block on a full pipe), replies and shutdown have deadlines, and the child
/// is always killed and reaped.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    stderr: Arc<Mutex<Vec<u8>>>,
    drains: Vec<JoinHandle<()>>,
    next_id: u64,
}

impl Client {
    fn start() -> Self {
        Self::spawn(env!("CARGO_BIN_EXE_asus-aura-cli"), &["--dry-run", "mcp"])
    }

    fn spawn(program: &str, args: &[&str]) -> Self {
        Self::spawn_with_stdout(program, args, None)
    }

    /// Server whose stdout goes to a caller-supplied sink (a pipe the test controls,
    /// `/dev/full`, ...). No stdout reader runs, so `request`/`read` are unavailable.
    fn start_with_stdout(stdout: Stdio) -> Self {
        Self::spawn_with_stdout(
            env!("CARGO_BIN_EXE_asus-aura-cli"),
            &["--dry-run", "mcp"],
            Some(stdout),
        )
    }

    fn spawn_with_stdout(program: &str, args: &[&str], stdout_sink: Option<Stdio>) -> Self {
        let custom_stdout = stdout_sink.is_some();
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(stdout_sink.unwrap_or_else(Stdio::piped))
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mcp server");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        });
        let _ = custom_stdout;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&stderr);
        let drain = std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stderr_pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut buffer = sink.lock().unwrap();
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.len() > STDERR_KEEP {
                    let excess = buffer.len() - STDERR_KEEP;
                    buffer.drain(..excess);
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            stderr,
            drains: vec![reader, drain],
            next_id: 1,
        }
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned()
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn try_read(&mut self, timeout: Duration) -> Result<Value, String> {
        let line = self.lines.recv_timeout(timeout).map_err(|e| {
            format!(
                "no reply within {timeout:?} ({e}); stderr tail:\n{}",
                self.stderr_tail()
            )
        })?;
        serde_json::from_str(&line).map_err(|e| format!("stdout is not JSON ({e}): {line:?}"))
    }

    fn stderr_tail(&self) -> String {
        let text = self.stderr_text();
        let tail: Vec<&str> = text.lines().rev().take(20).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    /// Every stdout line must be one JSON-RPC message; dry-run traces would break this.
    fn read(&mut self) -> Value {
        match self.try_read(READ_TIMEOUT) {
            Ok(value) => value,
            Err(message) => {
                self.kill_and_reap();
                panic!("{message}");
            }
        }
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

    /// Wait for the child to exit; on deadline kill it and fail with its stderr tail.
    fn wait_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.join_drains();
                return status;
            }
            if Instant::now() >= deadline {
                let tail = self.stderr_tail();
                self.kill_and_reap();
                panic!("child did not exit within {timeout:?}; stderr tail:\n{tail}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Close stdin, wait for exit, return the status and complete (bounded) stderr.
    fn finish(&mut self) -> (ExitStatus, String) {
        self.stdin.take();
        let status = self.wait_exit(Duration::from_secs(5));
        (status, self.stderr_text())
    }

    fn join_drains(&mut self) {
        for handle in self.drains.drain(..) {
            let _ = handle.join();
        }
    }

    fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.join_drains();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                self.join_drains();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill_and_reap();
    }
}

#[test]
fn silent_child_gives_a_bounded_failure_and_is_reaped() {
    let mut client = Client::spawn("sleep", &["30"]);
    let pid = client.child.id();
    let started = Instant::now();
    let error = client.try_read(Duration::from_millis(200)).unwrap_err();
    assert!(error.contains("no reply within"));
    assert!(started.elapsed() < Duration::from_secs(5));
    client.kill_and_reap();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[test]
fn a_thousand_write_producing_calls_complete_with_stderr_drained() {
    let mut client = Client::start();
    for _ in 0..1000 {
        assert_eq!(client.tool("aura_turn_off", json!({}))["isError"], false);
    }
    let (status, stderr) = client.finish();
    assert!(status.success());
    assert!(stderr.contains("GPU Write"));
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
        let status = client.wait_exit(Duration::from_secs(3));
        assert!(status.success());
        let stderr = client.stderr_text();
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
    let (status, stderr) = client.finish();
    assert!(status.success());
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

fn tool_call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
}

/// Index of the first/last occurrence, to assert restore happens after the pattern.
fn assert_pattern_then_restore(stderr: &str, pattern_write: &str) {
    let pattern = stderr
        .find(pattern_write)
        .unwrap_or_else(|| panic!("no pattern write {pattern_write:?} in:\n{stderr}"));
    let restore = stderr
        .rfind("GPU Write: Reg 0x07 <= 0x00")
        .expect("baseline restore write");
    assert!(
        restore > pattern,
        "baseline was not restored after the pattern:\n{stderr}"
    );
}

#[test]
fn closed_stdout_restores_the_baseline_before_a_failing_exit() {
    let (reader, writer) = std::io::pipe().unwrap();
    let mut client = Client::start_with_stdout(Stdio::from(writer));
    let mut reader = BufReader::new(reader);
    client.send(tool_call(
        1,
        "aura_agent_state",
        json!({"state": "working"}),
    ));
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("amber_breathe"));
    drop(reader); // the client disconnects
    client.send(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
    let status = client.wait_exit(Duration::from_secs(5));
    assert_eq!(status.code(), Some(1));
    let stderr = client.stderr_text();
    assert!(stderr.contains("Broken pipe"), "{stderr}");
    assert!(!stderr.contains("clean shutdown"));
    assert_pattern_then_restore(&stderr, "GPU Write: Reg 0x07 <= 0x02");
}

#[test]
fn stdout_flush_failure_behaves_like_a_closed_pipe() {
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let mut client = Client::start_with_stdout(Stdio::from(full));
    client.send(tool_call(
        1,
        "aura_agent_state",
        json!({"state": "working"}),
    ));
    let status = client.wait_exit(Duration::from_secs(5));
    assert_eq!(status.code(), Some(1));
    let stderr = client.stderr_text();
    assert!(stderr.contains("No space left on device"), "{stderr}");
    assert_pattern_then_restore(&stderr, "GPU Write: Reg 0x07 <= 0x02");
}

#[test]
fn unread_replies_do_not_stall_the_watchdog_or_sigterm() {
    let (reader, writer) = std::io::pipe().unwrap();
    let mut client = Client::start_with_stdout(Stdio::from(writer));
    let mut reader = BufReader::new(reader);
    client.send(tool_call(
        1,
        "aura_agent_state",
        json!({"state": "failure", "duration_seconds": 1}),
    ));
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    // Large replies saturate the stdout pipe and block the writer thread, but stay
    // below the output queue bound, so the session itself must remain alive.
    for id in 2..42 {
        client.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"}));
    }
    std::thread::sleep(Duration::from_millis(1800));
    assert!(
        client.child.try_wait().unwrap().is_none(),
        "session ended early"
    );
    // The one-second failure hold expired and was restored although stdout is blocked.
    assert_pattern_then_restore(&client.stderr_text(), "GPU Write: Reg 0x07 <= 0x03");
    assert!(Command::new("kill")
        .args(["-TERM", &client.child.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert!(client.wait_exit(Duration::from_secs(3)).success());
    assert!(client.stderr_text().contains("shutdown signal received"));
    drop(reader);
}

#[test]
fn sustained_output_backpressure_ends_the_session_with_cleanup() {
    let (reader, writer) = std::io::pipe().unwrap();
    let mut client = Client::start_with_stdout(Stdio::from(writer));
    let mut reader = BufReader::new(reader);
    client.send(tool_call(
        1,
        "aura_agent_state",
        json!({"state": "failure", "duration_seconds": 30}),
    ));
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut stdin = client.stdin.take().unwrap();
    let flood = std::thread::spawn(move || {
        for id in 2..20_000u64 {
            if writeln!(
                stdin,
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/list"}}"#
            )
            .is_err()
            {
                break;
            }
        }
    });
    let status = client.wait_exit(Duration::from_secs(8));
    assert_eq!(status.code(), Some(1));
    let stderr = client.stderr_text();
    assert!(stderr.contains("not reading responses"), "{stderr}");
    assert_pattern_then_restore(&stderr, "GPU Write: Reg 0x07 <= 0x03");
    drop(reader);
    let _ = flood.join();
}
