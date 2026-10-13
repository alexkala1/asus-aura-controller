"""Real-socket tests for web/serve.py. Subprocesses are mocked: no hardware is touched."""

import importlib.util
import json
import socket
import threading
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

WEB = Path(__file__).resolve().parents[1] / "web"


@pytest.fixture
def server_module(monkeypatch):
    spec = importlib.util.spec_from_file_location("lumina_web_server_sock", WEB / "serve.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.calls = []

    def fake_run(cmd, **kwargs):
        module.calls.append(cmd)
        return SimpleNamespace(returncode=0, stdout="ok", stderr="")

    monkeypatch.setattr(module.subprocess, "run", fake_run)
    return module


@pytest.fixture
def start(server_module):
    servers = []

    def _start(max_active=16):
        httpd = server_module.BoundedThreadingServer(
            ("127.0.0.1", 0), server_module.LuminaRequestHandler, max_active=max_active)
        threading.Thread(target=httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True).start()
        servers.append(httpd)
        return httpd.server_address[1]

    yield _start
    for httpd in servers:
        httpd.shutdown()
        httpd.server_close()


def exchange(port, payload: bytes, half_close=False, timeout=5.0) -> bytes:
    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as conn:
        conn.sendall(payload)
        if half_close:
            conn.shutdown(socket.SHUT_WR)
        chunks = []
        try:
            while chunk := conn.recv(4096):
                chunks.append(chunk)
        except socket.timeout:
            pass
        return b"".join(chunks)


def status_of(response: bytes) -> int:
    return int(response.split(b" ", 2)[1])


def post(headers: str, body: bytes = b"") -> bytes:
    return (f"POST /api/apply HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{headers}\r\n").encode() + body


VALID = json.dumps({"target": "gpu", "color": "ff0055", "mode": "static"}).encode()


def test_valid_apply_reaches_the_cli_exactly_once(start, server_module):
    port = start()
    response = exchange(port, post(f"Content-Length: {len(VALID)}\r\n", VALID))
    assert status_of(response) == 200
    assert len(server_module.calls) == 1
    assert server_module.calls[0][:2] == ["sudo", "-n"]


@pytest.mark.parametrize("headers, code", [
    ("Content-Length: 99999\r\n", 413),
    (f"Content-Length: {16 * 1024 + 1}\r\n", 413),
    ("Content-Length: -5\r\n", 400),
    ("Content-Length: abc\r\n", 400),
    ("Content-Length: 1e3\r\n", 400),
    ("Content-Length: 5\r\nContent-Length: 6\r\n", 400),
    ("Transfer-Encoding: chunked\r\n", 400),
    ("", 411),
])
def test_bad_framing_is_rejected_before_any_body_read_or_subprocess(start, server_module, headers, code):
    port = start()
    started = time.monotonic()
    response = exchange(port, post(headers))
    assert status_of(response) == code, response
    assert time.monotonic() - started < 2
    assert server_module.calls == []


def test_a_body_at_the_cap_is_accepted_up_to_json_validation(start, server_module):
    port = start()
    body = b" " * (16 * 1024 - 2) + b"{}"
    response = exchange(port, post(f"Content-Length: {len(body)}\r\n", body))
    assert status_of(response) == 400  # valid framing; the empty request is rejected later
    assert server_module.calls == []


def test_truncated_body_is_a_controlled_error(start, server_module):
    port = start()
    response = exchange(port, post("Content-Length: 100\r\n", b"{"), half_close=True)
    assert status_of(response) == 400
    assert b"ended before" in response
    assert server_module.calls == []


def test_a_stalled_body_does_not_block_other_clients(start, server_module):
    port = start()
    stalled = socket.create_connection(("127.0.0.1", port))
    stalled.sendall(post("Content-Length: 100\r\n", b"{"))
    time.sleep(0.2)
    started = time.monotonic()
    response = exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    assert status_of(response) == 200
    assert time.monotonic() - started < 2
    stalled.close()


def test_incomplete_headers_time_out_and_do_not_block_others(start, server_module, monkeypatch):
    monkeypatch.setattr(server_module.LuminaRequestHandler, "timeout", 0.4)
    port = start()
    stalled = socket.create_connection(("127.0.0.1", port))
    stalled.sendall(b"POST /api/apply HTTP/1.1\r\nHos")
    response = exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    assert status_of(response) == 200
    stalled.settimeout(4)
    assert stalled.recv(100) == b""  # the server gave up on the stalled connection
    stalled.close()


def test_a_stalled_body_times_out_with_408(start, server_module, monkeypatch):
    monkeypatch.setattr(server_module, "IO_TIMEOUT", 0.4)
    monkeypatch.setattr(server_module.LuminaRequestHandler, "timeout", 0.4)
    port = start()
    started = time.monotonic()
    response = exchange(port, post("Content-Length: 100\r\n", b"{"), timeout=4)
    assert status_of(response) == 408
    assert time.monotonic() - started < 3
    assert server_module.calls == []


def test_connection_cap_returns_503_instead_of_queueing(start, server_module):
    port = start(max_active=2)
    held = [socket.create_connection(("127.0.0.1", port)) for _ in range(2)]
    for conn in held:
        conn.sendall(b"POST /api/apply HTTP/1.1\r\nHos")
    time.sleep(0.3)
    response = exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\n\r\n")
    assert status_of(response) == 503
    for conn in held:
        conn.close()
    time.sleep(0.3)
    ok = exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    assert status_of(ok) == 200


def load_with_env(monkeypatch, **env):
    for name in ("LUMINA_CLI_BIN", "AURA_CLI_BIN", "LUMINA_PORT", "AURA_PORT"):
        monkeypatch.delenv(name, raising=False)
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    spec = importlib.util.spec_from_file_location("lumina_web_server_env", WEB / "serve.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_lumina_environment_names_win_and_legacy_names_still_work(monkeypatch):
    default = load_with_env(monkeypatch)
    assert default.CLI_BIN.name == "luminactl"
    assert default.PORT == 7890
    legacy = load_with_env(monkeypatch, AURA_CLI_BIN="/opt/old/bin", AURA_PORT="7001")
    assert str(legacy.CLI_BIN) == "/opt/old/bin" and legacy.PORT == 7001
    both = load_with_env(monkeypatch, LUMINA_CLI_BIN="/opt/new/bin", AURA_CLI_BIN="/opt/old/bin",
                         LUMINA_PORT="7002", AURA_PORT="7001")
    assert str(both.CLI_BIN) == "/opt/new/bin" and both.PORT == 7002


def test_legacy_handler_name_is_an_alias_and_logs_use_the_lumina_prefix(server_module, capsys):
    assert server_module.AuraRequestHandler is server_module.LuminaRequestHandler
    handler = object.__new__(server_module.LuminaRequestHandler)
    handler.log_message("%s", "hello")
    assert capsys.readouterr().err == "[lumina-server] hello\n"
