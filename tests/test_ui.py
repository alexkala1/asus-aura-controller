"""Live dashboard contract tests. Intercept the daemon, never access hardware."""

import copy
import importlib.util
import io
import json
import mimetypes
from pathlib import Path
from types import SimpleNamespace
import xml.etree.ElementTree as ET

import pytest
from playwright.sync_api import expect

WEB = Path(__file__).resolve().parents[1] / "web"
URL = "http://127.0.0.1:7890"
STATUS = {
    "gpu": {"device": "ASUS ROG Strix GeForce GTX 1060 6GB", "bus": "/dev/i2c-0", "address": "0x29", "magic_verified": True, "mode": "static", "color_hex": "ff0055", "rgb": [255, 0, 85]},
    "motherboard": {"device": "ASUS ROG STRIX Z270E GAMING Motherboard", "super_io": "Nuvoton NCT6793D", "smbus_port": "0x02c0", "slave_address": "0x4e", "firmware": "LED-0116", "led_count": 5, "mode": "static", "speed": 0, "direction": "forward", "zones": [
        {"led_index": i, "channel": "0x85" if i < 4 else "0x86", "channel_name": "Back I/O" if i < 4 else "RGB Header 1", "hex": "00e5ff", "rgb": [0, 229, 255]} for i in range(5)
    ]},
}


def set_color(page, selector, color):
    page.locator(selector).evaluate(
        "(el, color) => { el.value = color; el.dispatchEvent(new Event('input', {bubbles:true})); }", color
    )


def apply_status(state, payload):
    """Model CLI device-wide semantics inside the test fixture only."""
    if payload["target"] in ("all", "gpu"):
        gpu = state["data"]["gpu"]
        gpu["mode"] = "off" if payload["off"] else payload["mode"] if payload["mode"] in ("static", "breathing", "flashing", "spectrum") else "static"
        if not payload["off"]:
            gpu["color_hex"] = payload["color"]
    if payload["target"] in ("all", "mobo"):
        mobo = state["data"]["motherboard"]
        mobo["mode"] = "off" if payload["off"] else payload["mode"]
        mobo["speed"] = payload["speed"] if payload["target"] == "mobo" else 0
        if not payload["off"] and payload["mode"] == "static":
            for zone in mobo["zones"]:
                zone["hex"] = payload["color"]


@pytest.fixture
def daemon(page):
    state = {"data": copy.deepcopy(STATUS), "writes": [], "hold_status": False, "hold_apply": False,
             "pending_status": [], "pending_apply": [], "status_error": None, "apply_error": None,
             "console_errors": [], "page_errors": []}
    page.on("console", lambda msg: state["console_errors"].append(msg.text) if msg.type == "error" else None)
    page.on("pageerror", lambda error: state["page_errors"].append(str(error)))

    def handle(route):
        path = route.request.url.removeprefix(URL).split("?")[0]
        if path == "/api/status":
            if state["hold_status"]:
                state["pending_status"].append(route)
            elif state["status_error"]:
                route.fulfill(json={"ok": False, "error": state["status_error"]})
            else:
                route.fulfill(json={"ok": True, "mode": "live", "data": state["data"]})
        elif path == "/api/apply":
            payload = route.request.post_data_json
            state["writes"].append(payload)
            if state["hold_apply"]:
                state["pending_apply"].append(route)
            elif state["apply_error"]:
                route.fulfill(json={"ok": False, "error": state["apply_error"]})
            else:
                apply_status(state, payload)
                route.fulfill(json={"ok": True, "output": "Applied"})
        else:
            asset = WEB / (path.lstrip("/") or "index.html")
            if asset.is_file() and asset.resolve().is_relative_to(WEB):
                route.fulfill(body=asset.read_bytes(), content_type=mimetypes.guess_type(asset)[0] or "application/octet-stream")
            else:
                route.fulfill(status=404, body="Not found")

    page.route(f"{URL}/**", handle)
    return state


@pytest.mark.parametrize("width", [320, 375, 414, 768, 1440])
def test_live_dashboard(page, daemon, width):
    page.set_viewport_size({"width": width, "height": 1000})
    page.goto(URL, wait_until="networkidle")
    expect(page.locator("#live-status")).to_have_text("● LIVE")
    expect(page.locator("#daemon-status")).to_have_text("Daemon :7890 · 2 devices detected")
    expect(page.locator(".brand svg")).to_be_visible()
    expect(page.locator(".brand-wordmark")).to_contain_text("LUMINA")
    expect(page.locator("link[rel=icon]")).to_have_attribute("href", "favicon.svg")
    expect(page.locator("#mode-live, #mode-sim")).to_have_count(0)
    expect(page.locator("h1 em")).to_have_count(0)
    assert page.locator("h1").evaluate("el => getComputedStyle(el).fontStyle") == "normal"
    expect(page.get_by_role("heading", name="NVIDIA GTX 1060 6GB")).to_be_visible()
    expect(page.get_by_role("heading", name="ASUS ROG STRIX Z270E GAMING")).to_be_visible()
    expect(page.locator("#firmware")).to_have_text("LED-0116")
    expect(page.locator(".channel-card")).to_have_count(2)
    expect(page.locator("#mobo .color-picker")).to_have_count(1)
    expect(page.locator("#mobo .mode-picker")).to_have_count(1)
    expect(page.locator("#mobo .speed-slider")).to_have_count(1)
    expect(page.locator(".zone-led")).to_have_count(5)
    expect(page.locator(".cpu-socket, .ram-slots, .pci-slot")).to_have_count(0)
    expect(page.locator("#gpu-speed")).to_be_disabled()
    expect(page.locator("#gpu-color")).to_have_value("#ff0055")
    expect(page.locator("#mobo-color")).to_have_value("#00e5ff")

    set_color(page, "#gpu-color", "#00e5ff")
    expect(page.locator("#gpu .hex-value")).to_have_text("#00E5FF")
    assert "rgb(0, 229, 255)" in page.locator("#gpu .preview-wordmark").evaluate("el => getComputedStyle(el).textShadow")
    page.locator("#gpu-mode").select_option("breathing")
    expect(page.locator("#command-preview")).to_have_text("sudo luminactl gpu --color 00e5ff --mode breathing")
    assert daemon["writes"] == []  # Staging never writes hardware.

    set_color(page, "#mobo-color", "#112233")
    for zone in range(5):
        expect(page.locator(f'.zone-led[data-zone="{zone}"]')).to_have_attribute("data-color", "#112233")
    page.locator("#mobo-mode").select_option("strobing")
    page.locator("#mobo-speed").focus()
    page.keyboard.press("End")
    expect(page.locator("#command-preview")).to_contain_text("--mode flashing --speed 4")
    expect(page.locator(".board-map")).to_have_attribute("data-mode", "strobing")
    page.locator("#sync-all").check()
    set_color(page, "#mobo-color", "#ff0055")
    page.locator("#mobo-mode").select_option("rainbow")
    for channel in ["gpu", "mobo"]:
        expect(page.locator(f"#{channel}-color")).to_have_value("#ff0055")
        expect(page.locator(f"#{channel}-mode")).to_have_value("rainbow")
    expect(page.locator("#command-preview")).to_have_text("sudo luminactl all --color ff0055 --mode rainbow")
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_have_text("✓ Applied to hardware.")
    expect(page.locator("#btn-apply")).to_be_enabled()
    assert daemon["writes"] == [{"target": "all", "mode": "rainbow", "color": "ff0055", "speed": 4, "off": False}]

    page.locator("#gpu-mode").select_option("off")
    expect(page.locator("#command-preview")).to_have_text("sudo luminactl all --off")
    page.locator("#btn-apply").click()
    expect(page.locator(".channel-card[data-mode=off]")).to_have_count(2)
    expect(page.locator(".board-map")).to_have_attribute("data-mode", "off")
    page.locator("#sync-all").uncheck()
    page.locator("#gpu-mode").select_option("wave")
    expect(page.locator("#command-note")).to_contain_text("GPU uses static color")

    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth")
    # overflow-x:clip is a floor, not a way to conceal oversized controls.
    oversized = page.locator("input, select, button, .brand, .device-panel, pre").evaluate_all(
        "els => els.filter(el => { const r=el.getBoundingClientRect(); return r.left < -1 || r.right > innerWidth+1; }).map(el => el.id || el.className)"
    )
    assert oversized == []
    if width in (1440, 375):
        page.locator("#gpu-mode").select_option("static")
        page.locator("#sync-all").check()
        set_color(page, "#gpu-color", "#00e5ff")
        filename = "screenshot.png" if width == 1440 else "screenshot-mobile.png"
        page.screenshot(path=str(WEB / filename), full_page=True, animations="disabled")
    assert daemon["console_errors"] == []
    assert daemon["page_errors"] == []


def test_loading_error_success_and_keyboard_states(page, daemon):
    daemon["hold_status"] = True
    page.goto(URL, wait_until="domcontentloaded")
    expect(page.locator("#live-status")).to_have_text("● CONNECTING")
    expect(page.locator("#gpu-color")).to_be_disabled()
    expect(page.locator("#gpu-color")).to_have_attribute("data-state", "loading")
    daemon["hold_status"] = False
    page.wait_for_function("document.querySelector('#btn-refresh').disabled")
    assert daemon["pending_status"]
    daemon["pending_status"].pop().fulfill(json={"ok": True, "mode": "live", "data": daemon["data"]})
    expect(page.locator("#btn-apply")).to_be_enabled()
    page.locator("#gpu-mode").focus()
    outline = page.locator("#gpu-mode").evaluate("el => ({width:getComputedStyle(el).outlineWidth,style:getComputedStyle(el).outlineStyle})")
    assert outline == {"width": "2px", "style": "solid"}
    page.keyboard.press("ArrowDown")
    expect(page.locator("#gpu-mode")).to_have_attribute("data-state", "default")

    daemon["hold_apply"] = True
    page.locator("#btn-apply").click()
    expect(page.locator("#btn-apply")).to_have_text("Applying…")
    expect(page.locator("#btn-apply")).to_be_disabled()
    expect(page.locator("#gpu-mode")).to_be_disabled()
    expect(page.locator("#sync-all")).to_be_disabled()
    expect(page.locator("#gpu-mode")).to_have_attribute("data-state", "loading")
    assert daemon["pending_apply"]
    daemon["pending_apply"].pop().fulfill(json={"ok": False, "error": "Permission denied; run daemon with configured sudo access."})
    expect(page.locator("#status-toast")).to_contain_text("Apply failed: Permission denied")
    expect(page.locator("#gpu-mode")).to_have_attribute("aria-invalid", "true")
    expect(page.locator("#gpu-mode")).to_have_attribute("data-state", "error")
    expect(page.locator("#btn-apply")).to_be_enabled()
    daemon["hold_apply"] = False
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_have_text("✓ Applied to hardware.")
    expect(page.locator("#gpu-mode")).to_have_attribute("data-state", "success")
    assert daemon["page_errors"] == []


@pytest.mark.parametrize("failure", ["unavailable", "signature", "firmware"])
def test_disconnect_and_unverified_hardware_never_enable_apply(page, daemon, failure):
    if failure == "unavailable":
        daemon["status_error"] = "Daemon cannot read hardware. Check sudo access."
    elif failure == "signature":
        daemon["data"]["gpu"]["magic_verified"] = False
    else:
        daemon["data"]["motherboard"]["firmware"] = "OTHER-DEVICE"
    page.goto(URL, wait_until="networkidle")
    expect(page.locator("#live-status")).to_have_text("● OFFLINE")
    expect(page.locator("#btn-apply")).to_be_disabled()
    expect(page.locator("#gpu-color")).to_be_disabled()
    assert daemon["writes"] == []
    daemon["status_error"] = None
    daemon["data"] = copy.deepcopy(STATUS)
    page.locator("#btn-refresh").click()
    expect(page.locator("#live-status")).to_have_text("● LIVE")
    expect(page.locator("#btn-apply")).to_be_enabled()


def test_file_open_is_offline_and_reduced_motion_is_respected(page):
    page.emulate_media(reduced_motion="reduce")
    page.goto((WEB / "index.html").as_uri(), wait_until="networkidle")
    expect(page.locator("#live-status")).to_have_text("● OFFLINE")
    expect(page.locator("#connection-help")).to_contain_text("http://127.0.0.1:7890")
    expect(page.locator("#btn-apply")).to_be_disabled()
    assert page.locator("#gpu .led-preview").evaluate("el => getComputedStyle(el).animationName") == "none"


@pytest.mark.parametrize("native", ["rainbow_breathing", "chase", "spectrum_chase"])
def test_preserves_native_hardware_modes(page, daemon, native):
    daemon["data"]["motherboard"]["mode"] = native
    page.goto(URL, wait_until="networkidle")
    expect(page.locator("#live-status")).to_have_text("● LIVE")
    page.locator("#mobo-color").focus()
    expect(page.locator("#command-preview")).to_contain_text(f"--mode {native.replace('_', '-')}")


def test_refresh_preserves_staged_edits_and_disconnect_locks_writes(page, daemon):
    page.goto(URL, wait_until="networkidle")
    set_color(page, "#gpu-color", "#112233")
    page.locator("#btn-refresh").click()
    expect(page.locator("#btn-refresh")).to_be_enabled()
    expect(page.locator("#gpu-color")).to_have_value("#112233")
    daemon["status_error"] = "Hardware disconnected"
    page.locator("#btn-refresh").click()
    expect(page.locator("#live-status")).to_have_text("● OFFLINE")
    expect(page.locator("#btn-apply")).to_be_disabled()
    assert daemon["writes"] == []


@pytest.mark.parametrize("first, second", [("mobo", "gpu"), ("gpu", "mobo")])
def test_applying_one_device_keeps_the_other_devices_staged_edit(page, daemon, first, second):
    page.goto(URL, wait_until="networkidle")
    expect(page.locator("#sync-all")).not_to_be_checked()
    staged = {"gpu": "#112233", "mobo": "#abcdef"}
    # The device edited last is the active one, so it is the one Apply sends first.
    for device in (second, first):
        set_color(page, f"#{device}-color", staged[device])
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_contain_text("still staged")
    assert [(w["target"], "#" + w["color"]) for w in daemon["writes"]] == [(first, staged[first])]
    # The refresh that follows the apply must not overwrite the other staged device.
    expect(page.locator(f"#{second}-color")).to_have_value(staged[second])
    page.locator("#btn-refresh").click()
    expect(page.locator("#btn-refresh")).to_be_enabled()
    expect(page.locator(f"#{second}-color")).to_have_value(staged[second])
    # The remaining staged payload is what the next Apply sends.
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_have_text("✓ Applied to hardware.")
    assert [(w["target"], "#" + w["color"]) for w in daemon["writes"]] == [
        (first, staged[first]), (second, staged[second])]
    expect(page.locator(f"#{first}-color")).to_have_value(staged[first])


def test_failed_apply_keeps_both_devices_staged_through_refresh(page, daemon):
    page.goto(URL, wait_until="networkidle")
    set_color(page, "#gpu-color", "#112233")
    set_color(page, "#mobo-color", "#abcdef")
    daemon["apply_error"] = "device busy"
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_contain_text("Apply failed")
    page.locator("#btn-refresh").click()
    expect(page.locator("#btn-refresh")).to_be_enabled()
    expect(page.locator("#gpu-color")).to_have_value("#112233")
    expect(page.locator("#mobo-color")).to_have_value("#abcdef")
    daemon["apply_error"] = None
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_contain_text("still staged")
    assert [w["target"] for w in daemon["writes"]] == ["mobo", "mobo"]
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_have_text("✓ Applied to hardware.")
    assert [w["target"] for w in daemon["writes"]] == ["mobo", "mobo", "gpu"]


def test_linked_edits_stage_both_devices_and_apply_clears_both(page, daemon):
    page.goto(URL, wait_until="networkidle")
    page.locator("#sync-all").check()
    set_color(page, "#gpu-color", "#445566")
    page.locator("#btn-apply").click()
    expect(page.locator("#status-toast")).to_have_text("✓ Applied to hardware.")
    assert [w["target"] for w in daemon["writes"]] == ["all"]


def test_copy_feedback_and_hover_active_states(page, daemon):
    page.goto(URL, wait_until="networkidle")
    page.evaluate("Object.defineProperty(navigator, 'clipboard', {value: {writeText: async text => {window.copiedText=text;}}})")
    page.locator("#btn-copy").click()
    expect(page.locator("#status-toast")).to_have_text("✓ Command copied.")
    assert page.evaluate("window.copiedText") == page.locator("#command-preview").inner_text()
    page.evaluate("Object.defineProperty(navigator.clipboard, 'writeText', {value:async () => {throw Error('denied');}})")
    page.locator("#btn-copy").click()
    expect(page.locator("#status-toast")).to_contain_text("Copy unavailable")
    button = page.locator("#btn-apply")
    base = button.evaluate("el => getComputedStyle(el).backgroundColor")
    button.hover()
    assert button.evaluate("el => getComputedStyle(el).backgroundColor") != base
    page.mouse.down()
    assert button.evaluate("el => getComputedStyle(el).boxShadow") != "none"
    page.mouse.move(0, 0)
    page.mouse.up()


def test_connected_controls_remain_neutral_and_selection_has_no_border(page, daemon):
    page.goto(URL, wait_until="networkidle")
    expect(page.locator("#mobo-color")).to_have_attribute("data-state", "success")
    assert page.locator("#mobo .controls").evaluate("el => getComputedStyle(el).boxShadow") == "none"
    fields = page.locator("#mobo-color, #mobo-mode, #btn-apply")
    original = fields.evaluate_all("els => els.map(el => getComputedStyle(el).borderColor)")
    page.locator("#sync-all").check()
    assert fields.evaluate_all("els => els.map(el => getComputedStyle(el).borderColor)") == original
    for card in ["#gpu", "#mobo"]:
        assert page.locator(card).evaluate("el => getComputedStyle(el).borderWidth") == "0px"
    assert page.locator(".sync-panel").evaluate("el => getComputedStyle(el).borderLeftWidth") == "0px"
    assert page.locator("#mobo-speed").evaluate("el => getComputedStyle(el).accentColor") != page.locator("#live-status").evaluate("el => getComputedStyle(el).color")
    expect(page.locator("#live-status")).to_have_text("● LIVE")


def test_board_strip_reads_each_zone_and_unified_color_writes_boardwide(page, daemon):
    daemon["data"]["motherboard"]["zones"][4]["hex"] = "112233"
    page.goto(URL, wait_until="networkidle")
    expect(page.locator('.zone-led[data-zone="0"]')).to_have_attribute("data-color", "#00e5ff")
    expect(page.locator('.zone-led[data-zone="4"]')).to_have_attribute("data-color", "#112233")
    set_color(page, "#mobo-color", "#334455")
    page.locator("#btn-apply").click()
    expect(page.locator("#btn-apply")).to_be_enabled()
    assert daemon["writes"][-1]["target"] == "mobo"
    assert daemon["writes"][-1]["color"] == "334455"
    for zone in range(5):
        expect(page.locator(f'.zone-led[data-zone="{zone}"]')).to_have_attribute("data-color", "#334455")


def test_vector_assets_and_locked_tokens():
    ns = {"svg": "http://www.w3.org/2000/svg"}
    for filename in ("lumina-logo.svg", "favicon.svg"):
        root = ET.parse(WEB / filename).getroot()
        assert root.attrib["viewBox"] == "0 0 64 64"
        assert [stop.attrib["stop-color"] for stop in root.findall(".//svg:stop", ns)] == ["#ff0055", "#00e5ff"]
        assert len(root.findall(".//svg:path", ns)) >= 3
    css = (WEB / "style.css").read_text()
    tokens, rules = css.split("}\n", 1)
    for name in ("--bg-canvas", "--bg-card", "--border-subtle", "--color-accent", "--text-primary", "--text-secondary"):
        assert name in tokens
    assert "oklch(" not in rules  # Color literals belong only in :root.
    assert "#ff" not in rules
    app = (WEB / "app.js").read_text()
    assert "dry_run" not in app and "--dry-run" not in app


def test_daemon_status_failure_has_no_synthetic_fallback(monkeypatch):
    spec = importlib.util.spec_from_file_location("lumina_web_server", WEB / "serve.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    calls = []
    responses = []
    def failed_read(cmd, **kwargs):
        calls.append(cmd)
        return SimpleNamespace(returncode=1, stdout="", stderr="Permission denied")
    monkeypatch.setattr(module.subprocess, "run", failed_read)
    handler = object.__new__(module.LuminaRequestHandler)
    handler.send_json_response = lambda code, body: responses.append((code, body))
    handler.handle_get_status()
    assert len(calls) == 1
    assert calls[0][:2] == ["sudo", "-n"]
    assert responses == [(503, {"ok": False, "error": "Permission denied"})]
    handler.headers = {"Content-Length": "16"}
    body = json.dumps({"dry_run": True}).encode()
    handler.headers["Content-Length"] = str(len(body))
    handler.rfile = io.BytesIO(body)
    handler.handle_post_apply()
    assert len(calls) == 1
    assert responses[-1][0] == 400
