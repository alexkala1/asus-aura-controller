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
        _start.last = httpd
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


def trickle(port, headers, body, step, interval, read_timeout=5.0):
    """Send `body` in `step`-byte pieces, `interval` seconds apart; return (response, seconds)."""
    started = time.monotonic()
    with socket.create_connection(("127.0.0.1", port), timeout=read_timeout) as conn:
        conn.sendall(post(headers))
        try:
            for i in range(0, len(body), step):
                conn.sendall(body[i:i + step])
                time.sleep(interval)
        except OSError:
            pass  # the server already answered and closed
        chunks = []
        try:
            while chunk := conn.recv(4096):
                chunks.append(chunk)
        except socket.timeout:
            pass
    return b"".join(chunks), time.monotonic() - started


@pytest.mark.parametrize("pad, step, interval", [
    (0, 1, 0.02),        # tiny body, one byte at a time: short final chunk
    (6000, 100, 0.05),   # spans several 4 KiB reads, always inside the inactivity timeout
])
def test_slow_progress_cannot_outlive_the_absolute_body_deadline(
        start, server_module, monkeypatch, pad, step, interval):
    monkeypatch.setattr(server_module, "IO_TIMEOUT", 0.3)
    monkeypatch.setattr(server_module, "CONNECTION_DEADLINE", 10.0)
    monkeypatch.setattr(server_module.LuminaRequestHandler, "timeout", 0.3)
    port = start()
    body = VALID + b" " * pad
    response, elapsed = trickle(port, f"Content-Length: {len(body)}\r\n", body, step, interval)
    assert status_of(response) == 408, response
    assert elapsed < 1.5, f"deadline not enforced: {elapsed:.2f}s"
    assert server_module.calls == [], "a late body must never reach the CLI"


def test_a_body_that_completes_inside_the_budget_still_executes(start, server_module, monkeypatch):
    monkeypatch.setattr(server_module, "IO_TIMEOUT", 2.0)
    monkeypatch.setattr(server_module.LuminaRequestHandler, "timeout", 2.0)
    port = start()
    response, _ = trickle(port, f"Content-Length: {len(VALID)}\r\n", VALID, 20, 0.01)
    assert status_of(response) == 200
    assert len(server_module.calls) == 1


def free_slots(httpd, max_active):
    """Count admissible connections without relying on private semaphore state."""
    taken = 0
    while httpd._slots.acquire(blocking=False):
        taken += 1
    for _ in range(taken):
        httpd._slots.release()
    return taken


@pytest.mark.parametrize("failures", [1, 3])
def test_worker_start_failures_never_leak_admission_slots(start, server_module, monkeypatch, failures):
    port = start(max_active=2)
    httpd = start.last
    real_start = threading.Thread.start
    remaining = {"n": failures}

    def flaky_start(self):
        if remaining["n"] > 0 and self.name.startswith("Thread-") and "process_request" in repr(self._target):
            remaining["n"] -= 1
            raise RuntimeError("can't start new thread")
        return real_start(self)

    monkeypatch.setattr(threading.Thread, "start", flaky_start)
    for _ in range(failures):
        # The server's normal error path drops the connection without a response.
        assert exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\n\r\n") == b""
    assert remaining["n"] == 0
    monkeypatch.setattr(threading.Thread, "start", real_start)
    assert free_slots(httpd, 2) == 2, "capacity leaked"
    ok = exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    assert status_of(ok) == 200
    held = [socket.create_connection(("127.0.0.1", port)) for _ in range(2)]
    for conn in held:
        conn.sendall(b"POST /api/apply HTTP/1.1\r\nHos")
    time.sleep(0.2)
    assert status_of(exchange(port, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")) == 503
    for conn in held:
        conn.close()


def test_handler_exceptions_release_the_slot_exactly_once(start, server_module, monkeypatch, capfd):
    port = start(max_active=1)
    httpd = start.last

    def boom(self):
        raise ValueError("handler exploded")

    monkeypatch.setattr(server_module.LuminaRequestHandler, "handle", boom)
    for _ in range(3):
        exchange(port, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
    monkeypatch.undo()
    time.sleep(0.2)
    assert free_slots(httpd, 1) == 1
    assert "released too many times" not in capfd.readouterr().err
    assert status_of(exchange(port, b"GET /favicon.svg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")) == 200


def test_autonomous_modes_apply_without_a_color_but_static_still_needs_one(start, server_module):
    port = start()
    for mode in ("rainbow", "wave", "spectrum"):
        body = json.dumps({"target": "all", "mode": mode, "speed": 0, "off": False}).encode()
        response = exchange(port, post(f"Content-Length: {len(body)}\r\n", body))
        assert status_of(response) == 200, (mode, response)
        assert "--color" not in server_module.calls[-1]
        assert server_module.calls[-1][-2:] == ["--mode", mode]
    before = len(server_module.calls)
    for body in (json.dumps({"target": "all", "mode": "static"}).encode(),
                 json.dumps({"target": "gpu", "mode": "rainbow", "color": "zzz"}).encode()):
        response = exchange(port, post(f"Content-Length: {len(body)}\r\n", body))
        assert status_of(response) == 400
    assert len(server_module.calls) == before


def test_zone_apply_passes_zone_and_color_to_the_cli_and_rejects_bad_combinations(start, server_module):
    port = start()
    body = json.dumps({"target": "mobo", "zone": 3, "color": "#00FFCC", "mode": "static", "speed": 2}).encode()
    assert status_of(exchange(port, post(f"Content-Length: {len(body)}\r\n", body))) == 200
    # A zone command never carries --speed or --mode: it is one LED's static color.
    assert server_module.calls[-1][-5:] == ["mobo", "--zone", "3", "--color", "00FFCC"]
    assert "--speed" not in server_module.calls[-1]
    before = len(server_module.calls)
    for bad in (
        {"target": "mobo", "zone": 5, "color": "ff0000"},
        {"target": "mobo", "zone": -1, "color": "ff0000"},
        {"target": "mobo", "zone": True, "color": "ff0000"},
        {"target": "mobo", "zone": "1", "color": "ff0000"},
        {"target": "mobo", "zone": 1.0, "color": "ff0000"},
        {"target": "gpu", "zone": 1, "color": "ff0000"},
        {"target": "all", "zone": 1, "color": "ff0000"},
        {"target": "mobo", "zone": 1, "color": "ff0000", "off": True},
        {"target": "mobo", "zone": 1, "color": "ff0000", "mode": "breathing"},
        {"target": "mobo", "zone": 1},
        {"target": "mobo", "zone": 1, "color": "zzzzzz"},
    ):
        payload = json.dumps(bad).encode()
        assert status_of(exchange(port, post(f"Content-Length: {len(payload)}\r\n", payload))) == 400, bad
    assert len(server_module.calls) == before, "no rejected request may reach the CLI"


@pytest.mark.parametrize("method, path", [("GET", "/"), ("GET", "/app.js"), ("HEAD", "/style.css"),
                                          ("GET", "/not-found"), ("GET", "/api/status"), ("OPTIONS", "/api/apply")])
def test_every_daemon_response_disables_caching(start, method, path):
    port = start()
    response = exchange(port, f"{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".encode())
    headers = response.split(b"\r\n\r\n", 1)[0].lower()
    assert b"cache-control: no-cache, no-store, must-revalidate" in headers


def test_conditional_static_requests_receive_fresh_assets_not_304(start):
    port = start()
    response = exchange(port, b"GET /app.js HTTP/1.1\r\nHost: x\r\nIf-Modified-Since: Wed, 01 Jan 2100 00:00:00 GMT\r\nIf-None-Match: *\r\nConnection: close\r\n\r\n")
    assert status_of(response) == 200
    assert b'"use strict"' in response.split(b"\r\n\r\n", 1)[1]


def test_overload_response_disables_caching(start):
    port = start(max_active=1)
    held = socket.create_connection(("127.0.0.1", port))
    try:
        held.sendall(b"GET / HTTP/1.1\r\nHos")
        deadline = time.monotonic() + 2
        while start.last._slots.acquire(blocking=False):
            start.last._slots.release()
            assert time.monotonic() < deadline
            time.sleep(0.01)
        response = exchange(port, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        assert status_of(response) == 503
        assert b"Cache-Control: no-cache, no-store, must-revalidate\r\n" in response
    finally:
        held.close()


def test_routes_ignore_query_strings_and_head_is_a_cheap_probe(start, server_module, monkeypatch):
    def fake_status(cmd, **kwargs):
        server_module.calls.append(cmd)
        return SimpleNamespace(returncode=0, stdout=json.dumps({"gpu": {}, "motherboard": {}}), stderr="")

    monkeypatch.setattr(server_module.subprocess, "run", fake_status)
    port = start()
    for target in ("/api/status", "/api/status?x=1", "/api/status?x=1#frag"):
        response = exchange(port, f"GET {target} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".encode())
        assert status_of(response) == 200, (target, response)
    assert len(server_module.calls) == 3
    head = exchange(port, b"HEAD /api/status?x=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
    assert status_of(head) == 200
    assert head.endswith(b"\r\n\r\n"), "HEAD must not carry a body"
    assert b"application/json" in head
    assert len(server_module.calls) == 3, "HEAD must not run the CLI"
    body = json.dumps({"target": "gpu", "mode": "static", "color": "ff0055"}).encode()
    response = exchange(port, post(f"Content-Length: {len(body)}\r\n", body).replace(b"/api/apply", b"/api/apply?x=1"))
    assert status_of(response) == 200
    assert len(server_module.calls) == 4
    assert status_of(exchange(port, b"HEAD /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")) == 404


@pytest.mark.parametrize("method", ["PUT", "PATCH", "DELETE"])
def test_unsupported_methods_get_a_json_405_with_allow(start, server_module, method):
    port = start()
    response = exchange(port, f"{method} /api/apply HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".encode())
    assert status_of(response) == 405
    assert b"Allow: GET, HEAD, POST, OPTIONS" in response
    assert json.loads(response.split(b"\r\n\r\n", 1)[1])["ok"] is False
    assert server_module.calls == []
