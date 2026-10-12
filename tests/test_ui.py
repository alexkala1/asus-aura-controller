"""Hardware-free browser checks; open the dashboard directly from disk."""

from pathlib import Path

import pytest
from playwright.sync_api import expect


WEB = Path(__file__).resolve().parents[1] / "web"


def set_color(page, selector, color):
    # Native color dialogs cannot be automated: dispatch the picker's input event.
    page.locator(selector).evaluate(
        "(input, color) => { input.value = color; input.dispatchEvent(new Event('input', {bubbles: true})); }",
        color,
    )


@pytest.mark.parametrize("viewport", [{"width": 1440, "height": 1100}, {"width": 390, "height": 844}])
def test_lighting_dashboard(page, viewport):
    console_errors = []
    page_errors = []
    failed_requests = []
    page.on("console", lambda message: console_errors.append(message.text) if message.type == "error" else None)
    page.on("pageerror", lambda error: page_errors.append(str(error)))
    page.on("requestfailed", lambda request: failed_requests.append(request.url))
    page.set_viewport_size(viewport)
    page.goto((WEB / "index.html").as_uri(), wait_until="networkidle")

    expect(page.get_by_role("heading", name="ROG Strix GTX 1060")).to_be_visible()
    expect(page.get_by_role("heading", name="ROG STRIX Z270E", exact=True)).to_be_visible()
    expect(page.locator(".channel-card")).to_have_count(6)
    for zone in range(5):
        expect(page.locator(f"#zone-{zone}")).to_be_visible()
    expect(page.locator("#gpu .preview-wordmark")).to_have_text("GTX 1060")
    expect(page.locator("#gpu-mode option")).to_have_count(11)

    set_color(page, "#gpu-color", "#00ffcc")
    expect(page.locator("#gpu .hex-value")).to_have_text("#00FFCC")
    assert page.locator("#gpu").evaluate("el => el.style.getPropertyValue('--led')") == "#00ffcc"
    assert "rgb(0, 255, 204)" in page.locator("#gpu .preview-wordmark").evaluate("el => getComputedStyle(el).textShadow")
    expect(page.locator("#zone-0-color")).to_have_value("#ff835b")
    expect(page.locator("#command-preview")).to_contain_text("gpu --color 00ffcc --mode static")

    page.locator("#gpu-mode").select_option("breathing")
    expect(page.locator("#command-preview")).to_contain_text("--mode breathing")
    page.locator("#zone-0-mode").select_option("strobing")
    expect(page.locator("#command-preview")).to_contain_text("mobo --color ff835b --mode flashing")
    page.locator("#zone-0-speed").focus()
    page.keyboard.press("End")
    expect(page.locator("#command-preview")).to_contain_text("--speed 4")

    # Link the most recently edited channel, then change controls on different cards.
    page.locator("#sync-all").check()
    expect(page.locator("#sync-state")).to_have_text("All 6 channels linked")
    set_color(page, "#zone-4-color", "#a855ff")
    page.locator("#zone-4-mode").select_option("rainbow")
    page.locator("#zone-4-speed").focus()
    page.keyboard.press("Home")
    page.keyboard.press("ArrowRight")
    page.keyboard.press("ArrowRight")
    for channel in ["gpu", *[f"zone-{zone}" for zone in range(5)]]:
        expect(page.locator(f"#{channel}-color")).to_have_value("#a855ff")
        expect(page.locator(f"#{channel}-mode")).to_have_value("rainbow")
        expect(page.locator(f"#{channel}-speed")).to_have_value("2")
        assert page.locator(f"#{channel}").evaluate("el => el.style.getPropertyValue('--led')") == "#a855ff"
    expect(page.locator("#command-preview")).to_have_text("sudo asus-aura-cli all --color a855ff --mode rainbow")

    # Toggle to simulation mode and verify dry-run command
    page.locator("#mode-sim").click()
    expect(page.locator("#command-preview")).to_have_text("asus-aura-cli --dry-run all --color a855ff --mode rainbow")
    expect(page.locator("#btn-apply")).to_have_text("🧪 Test Dry-Run")

    # Toggle back to live hardware mode
    page.locator("#mode-live").click()
    expect(page.locator("#command-preview")).to_have_text("sudo asus-aura-cli all --color a855ff --mode rainbow")
    expect(page.locator("#btn-apply")).to_have_text("⚡ Apply to Hardware")

    page.locator("#gpu-mode").select_option("off")
    expect(page.locator("#command-preview")).to_have_text("sudo asus-aura-cli all --off")
    expect(page.locator(".channel-card[data-mode=off]")).to_have_count(6)
    page.locator("#sync-all").uncheck()
    set_color(page, "#gpu-color", "#ff835b")
    page.locator("#gpu-mode").select_option("wave")
    expect(page.locator("#command-preview")).to_contain_text("gpu --color ff835b --mode static")
    expect(page.locator("#command-note")).to_contain_text("GPU uses static color")
    expect(page.locator("#zone-4-color")).to_have_value("#a855ff")

    # Keyboard focus and narrow-screen reflow.
    page.locator("#animate").focus()
    page.keyboard.press("Space")
    expect(page.locator("body")).to_have_class("paused")
    assert page.locator("#animate").evaluate("el => document.activeElement === el")
    assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth")
    page.locator("#sync-all").check()
    page.locator("#gpu-mode").select_option("breathing")
    filename = "screenshot.png" if viewport["width"] == 1440 else "screenshot-mobile.png"
    page.screenshot(path=str(WEB / filename), full_page=True, animations="disabled")
    assert console_errors == []
    assert page_errors == []
    assert failed_requests == []


def test_reduced_motion(page):
    page.emulate_media(reduced_motion="reduce")
    page.goto((WEB / "index.html").as_uri(), wait_until="networkidle")
    expect(page.locator("#animate")).not_to_be_checked()
    page.locator("#gpu-mode").select_option("rainbow")
    assert page.locator("#gpu .led-preview").evaluate("el => getComputedStyle(el).animationName") == "none"
