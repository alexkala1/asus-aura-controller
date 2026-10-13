#!/usr/bin/env python3
"""Local HTTP API server for Lumina Lighting Studio.
Provides static asset serving and live hardware endpoints calling luminactl.
"""
import http.server
import json
import os
import re
import shlex
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
WEB_DIR = ROOT_DIR / "web"
# sudo rules must point at a root-owned binary (see SECURITY.md), so allow an override.
CLI_BIN = Path(
    os.environ.get("LUMINA_CLI_BIN")
    or os.environ.get("AURA_CLI_BIN")  # legacy name
    or ROOT_DIR / "target" / "release" / "luminactl"
)
PORT = int(os.environ.get("LUMINA_PORT") or os.environ.get("AURA_PORT") or 7890)

MAX_BODY_BYTES = 16 * 1024      # an apply request is a few hundred bytes
IO_TIMEOUT = 10.0               # per socket read; also bounds the whole body read
CONNECTION_DEADLINE = 30.0      # hard cap on one connection, however slowly it drips
MAX_ACTIVE_HANDLERS = 16        # further connections get an immediate 503


class RequestError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


class LuminaRequestHandler(http.server.SimpleHTTPRequestHandler):
    timeout = IO_TIMEOUT  # StreamRequestHandler applies this to the connection socket

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(WEB_DIR), **kwargs)

    def handle(self):
        timer = threading.Timer(CONNECTION_DEADLINE, self._abort_connection)
        timer.daemon = True
        timer.start()
        try:
            super().handle()
        finally:
            timer.cancel()

    def _abort_connection(self):
        try:
            self.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass

    def read_json_body(self):
        """Validate framing before reading, then read exactly Content-Length bytes."""
        headers = self.headers
        if headers.get("Transfer-Encoding") is not None:
            raise RequestError(400, "Transfer-Encoding is not supported; send Content-Length.")
        get_all = getattr(headers, "get_all", None)
        values = get_all("Content-Length") if get_all else [headers.get("Content-Length")]
        values = [v for v in (values or []) if v is not None]
        if not values:
            raise RequestError(411, "Content-Length is required.")
        if len({v.strip() for v in values}) != 1:
            raise RequestError(400, "Conflicting Content-Length headers.")
        raw = values[0].strip()
        if not re.fullmatch(r"[0-9]{1,9}", raw):
            raise RequestError(400, "Invalid Content-Length.")
        length = int(raw)
        if length > MAX_BODY_BYTES:
            raise RequestError(413, f"Request body exceeds {MAX_BODY_BYTES} bytes.")
        deadline = time.monotonic() + IO_TIMEOUT
        chunks, remaining = [], length
        while remaining:
            if time.monotonic() >= deadline:
                raise RequestError(408, "Timed out reading the request body.")
            try:
                chunk = self.rfile.read(min(remaining, 4096))
            except (socket.timeout, TimeoutError):
                raise RequestError(408, "Timed out reading the request body.")
            if not chunk:
                raise RequestError(400, "Request body ended before Content-Length bytes.")
            chunks.append(chunk)
            remaining -= len(chunk)
        try:
            return json.loads(b"".join(chunks).decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as e:
            raise RequestError(400, f"Invalid JSON: {e}")

    def log_message(self, format, *args):
        sys.stderr.write(f"[lumina-server] {format % args}\n")

    def do_GET(self):
        if self.path == "/api/status":
            self.handle_get_status()
        else:
            super().do_GET()

    def do_POST(self):
        if self.path == "/api/apply":
            self.handle_post_apply()
        else:
            self.send_error(404, "Endpoint not found")

    def handle_get_status(self):
        cmd = ["sudo", "-n", str(CLI_BIN), "--json", "status"]
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=5)
            if r.returncode == 0:
                data = json.loads(r.stdout)
                self.send_json_response(200, {"ok": True, "mode": "live", "data": data})
            else:
                self.send_json_response(503, {"ok": False, "error": r.stderr.strip() or "Hardware status unavailable."})
        except Exception as e:
            self.send_json_response(500, {"ok": False, "error": str(e)})

    def handle_post_apply(self):
        origin = self.headers.get("Origin")
        if origin and origin not in (f"http://127.0.0.1:{PORT}", f"http://localhost:{PORT}"):
            self.send_json_response(403, {"ok": False, "error": "Hardware commands require the local daemon origin."})
            return
        try:
            req = self.read_json_body()
        except RequestError as e:
            # The body may be unread, so the connection cannot be reused.
            self.close_connection = True
            self.send_json_response(e.code, {"ok": False, "error": e.message})
            return

        if not isinstance(req, dict) or "dry_run" in req:
            self.send_json_response(400, {"ok": False, "error": "Only live hardware commands are supported."})
            return
        target = req.get("target", "all")
        is_off = req.get("off", False)
        color = req.get("color", "")
        mode = req.get("mode", "static")
        speed = req.get("speed", 0)

        if target not in ("gpu", "mobo", "all"):
            self.send_json_response(400, {"ok": False, "error": "Unknown hardware target."})
            return
        if not isinstance(is_off, bool) or not isinstance(color, str) or not isinstance(mode, str) or type(speed) is not int or not 0 <= speed <= 4:
            self.send_json_response(400, {"ok": False, "error": "Invalid lighting settings. Speed must be 0–4."})
            return
        color = color.removeprefix("#")
        if not is_off and (len(color) != 6 or any(c not in "0123456789abcdefABCDEF" for c in color)):
            self.send_json_response(400, {"ok": False, "error": "Color must contain six hexadecimal digits."})
            return
        cmd = ["sudo", "-n", str(CLI_BIN)]
        cmd.append(target)

        if is_off:
            cmd.append("--off")
        else:
            if color:
                cmd.extend(["--color", color])
            if mode:
                cmd.extend(["--mode", mode])
            if target == "mobo" and speed is not None:
                cmd.extend(["--speed", str(speed)])

        cmd_display = " ".join(shlex.quote(c) for c in cmd)
        print(f"[lumina-server] Executing: {cmd_display}", flush=True)

        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            if r.returncode == 0:
                self.send_json_response(200, {
                    "ok": True,
                    "command": cmd_display,
                    "output": r.stdout.strip()
                })
            else:
                self.send_json_response(400, {
                    "ok": False,
                    "command": cmd_display,
                    "error": r.stderr.strip() or r.stdout.strip()
                })
        except Exception as e:
            self.send_json_response(500, {"ok": False, "command": cmd_display, "error": str(e)})

    def send_json_response(self, code, data):
        payload = json.dumps(data).encode("utf-8")
        try:
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError, socket.timeout):
            self.close_connection = True  # the client left; nothing more to say

    def do_OPTIONS(self):
        self.send_response(200)
        self.end_headers()


class BoundedThreadingServer(http.server.ThreadingHTTPServer):
    """One thread per connection with a hard cap, so a stalled client never blocks others."""

    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, *args, max_active=MAX_ACTIVE_HANDLERS, **kwargs):
        self._slots = threading.BoundedSemaphore(max_active)
        super().__init__(*args, **kwargs)

    def process_request(self, request, client_address):
        if not self._slots.acquire(blocking=False):
            try:
                request.sendall(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n"
                                b"Content-Length: 0\r\n\r\n")
            except OSError:
                pass
            self.shutdown_request(request)
            return
        super().process_request(request, client_address)

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self._slots.release()


# Legacy name kept for importers.
AuraRequestHandler = LuminaRequestHandler


def run_server():
    with BoundedThreadingServer(("127.0.0.1", PORT), LuminaRequestHandler) as httpd:
        print(f"[lumina-server] Serving on http://127.0.0.1:{PORT} (Press Ctrl+C to stop)", flush=True)
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    run_server()
