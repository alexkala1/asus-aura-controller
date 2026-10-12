#!/usr/bin/env python3
"""Local HTTP API server for ASUS Aura Lighting Studio.
Provides static asset serving and live hardware endpoints calling asus-aura-cli.
"""
import http.server
import json
import os
import shlex
import socketserver
import subprocess
import sys
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
WEB_DIR = ROOT_DIR / "web"
CLI_BIN = ROOT_DIR / "target" / "release" / "asus-aura-cli"
PORT = int(os.environ.get("AURA_PORT", 7890))

class AuraRequestHandler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(WEB_DIR), **kwargs)

    def log_message(self, format, *args):
        sys.stderr.write(f"[aura-server] {format % args}\n")

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
                # Fallback to dry-run status if sudo fails
                r_dry = subprocess.run([str(CLI_BIN), "--dry-run", "--json", "status"], capture_output=True, text=True)
                data = json.loads(r_dry.stdout) if r_dry.returncode == 0 else {}
                self.send_json_response(200, {"ok": False, "mode": "sim", "error": r.stderr.strip(), "data": data})
        except Exception as e:
            self.send_json_response(500, {"ok": False, "error": str(e)})

    def handle_post_apply(self):
        try:
            content_length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(content_length).decode("utf-8")
            req = json.loads(body)
        except Exception as e:
            self.send_json_response(400, {"ok": False, "error": f"Invalid JSON: {e}"})
            return

        target = req.get("target", "all")  # all, gpu, mobo
        is_dry_run = bool(req.get("dry_run", False))
        is_off = bool(req.get("off", False))
        color = req.get("color", "").lstrip("#").strip()
        mode = req.get("mode", "static").strip()
        speed = req.get("speed", 0)

        cmd = []
        if not is_dry_run:
            cmd.extend(["sudo", "-n"])
        cmd.extend([str(CLI_BIN)])
        if is_dry_run:
            cmd.append("--dry-run")
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
        print(f"[aura-server] Executing: {cmd_display}", flush=True)

        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            if r.returncode == 0:
                self.send_json_response(200, {
                    "ok": True,
                    "command": cmd_display,
                    "output": r.stdout.strip(),
                    "dry_run": is_dry_run
                })
            else:
                self.send_json_response(400, {
                    "ok": False,
                    "command": cmd_display,
                    "error": r.stderr.strip() or r.stdout.strip(),
                    "dry_run": is_dry_run
                })
        except Exception as e:
            self.send_json_response(500, {"ok": False, "command": cmd_display, "error": str(e)})

    def send_json_response(self, code, data):
        payload = json.dumps(data).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()
        self.wfile.write(payload)

    def do_OPTIONS(self):
        self.send_response(200)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()


def run_server():
    socketserver.TCPServer.allow_reuse_address = True
    with socketserver.TCPServer(("127.0.0.1", PORT), AuraRequestHandler) as httpd:
        print(f"[aura-server] Serving on http://127.0.0.1:{PORT} (Press Ctrl+C to stop)", flush=True)
        try:
            httpd.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    run_server()
