"use strict";

// Display names map to the existing CLI's GPU/motherboard effect vocabulary.
const modes = [
  ["static", "Static", "static"], ["breathing", "Breathing", "breathing"],
  ["strobing", "Strobing", "flashing"], ["color-cycle", "Color cycle", "spectrum"],
  ["rainbow", "Rainbow", "rainbow"], ["comet", "Comet", "chase-fade"],
  ["flash-and-dash", "Flash and dash", "spectrum-chase-fade"], ["wave", "Wave", "wave"],
  ["glowing-yoyo", "Glowing yoyo", "rainbow-pulse"], ["starry-night", "Starry night", "random-flicker"],
  ["off", "Off", "off"],
];
const gpuModes = new Set(["static", "breathing", "flashing", "spectrum", "off"]);
const channels = [
  { id: "gpu", name: "GTX 1060 logo", tag: "GPU", color: "#ff835b", mode: "static", speed: 0 },
  ...Array.from({ length: 5 }, (_, zone) => ({
    id: `zone-${zone}`, name: zone < 4 ? `Back I/O · Zone ${zone}` : "RGB Header · Zone 4",
    tag: `0${zone}`, color: "#ff835b", mode: "static", speed: 0,
  })),
];
let active = channels[0];
const sync = document.querySelector("#sync-all");
const command = document.querySelector("#command-preview");
const template = document.querySelector("#channel-template");

for (const channel of channels) {
  const card = template.content.firstElementChild.cloneNode(true);
  card.id = channel.id;
  card.dataset.channel = channel.id;
  card.querySelector("h3").textContent = channel.name;
  card.querySelector(".channel-id").textContent = channel.tag;
  card.querySelector(".preview-wordmark").textContent = "GTX 1060";
  for (const [selector, suffix, label] of [
    [".color-picker", "color", "color"], [".mode-picker", "mode", "effect"], [".speed-slider", "speed", "speed"],
  ]) {
    const input = card.querySelector(selector);
    input.id = `${channel.id}-${suffix}`;
    input.setAttribute("aria-label", `${channel.name} ${label}`);
    input.closest("label").htmlFor = input.id;
  }
  for (const [value, label] of modes) {
    card.querySelector(".mode-picker").add(new Option(label, value));
  }
  channel.card = card;
  const update = (key, value) => {
    active = channel;
    channel[key] = value;
    if (sync.checked) mirror(channel);
    render();
  };
  card.querySelector(".color-picker").addEventListener("input", event => update("color", event.target.value));
  card.querySelector(".mode-picker").addEventListener("change", event => update("mode", event.target.value));
  card.querySelector(".speed-slider").addEventListener("input", event => update("speed", Number(event.target.value)));
  card.addEventListener("focusin", () => { active = channel; renderCommand(); markActive(); });
  document.querySelector(channel.id === "gpu" ? "#gpu-controls" : "#mobo-controls").append(card);
}

function mirror(source) {
  for (const channel of channels) {
    Object.assign(channel, { color: source.color, mode: source.mode, speed: source.speed });
  }
}

function markActive() {
  for (const channel of channels) channel.card.classList.toggle("selected", channel === active || sync.checked);
}

function render() {
  for (const channel of channels) {
    const card = channel.card;
    card.style.setProperty("--led", channel.color);
    card.style.setProperty("--duration", `${3 + channel.speed * 1.5}s`);
    card.dataset.mode = channel.mode;
    card.querySelector(".color-picker").value = channel.color;
    card.querySelector(".hex-value").textContent = channel.color.toUpperCase();
    card.querySelector(".mode-picker").value = channel.mode;
    card.querySelector(".speed-slider").value = channel.speed;
    card.querySelector(".speed-value").textContent = channel.speed;
  }
  document.querySelector(".gpu-hardware").style.setProperty("--gpu-led", channels[0].mode === "off" ? "#30353e" : channels[0].color);
  document.querySelectorAll(".io-strip span").forEach((led, index) => {
    led.style.setProperty("--zone-led", channels[index + 1].mode === "off" ? "#30353e" : channels[index + 1].color);
  });
  document.querySelector(".header-strip").style.setProperty("--zone-led", channels[5].mode === "off" ? "#30353e" : channels[5].color);
  document.querySelector("#sync-state").textContent = sync.checked ? "All 6 channels linked" : "Independent channels";
  markActive();
  renderCommand();
}

let isLiveMode = true;
const modeLiveBtn = document.querySelector("#mode-live");
const modeSimBtn = document.querySelector("#mode-sim");
const btnApply = document.querySelector("#btn-apply");
const btnCopy = document.querySelector("#btn-copy");
const toast = document.querySelector("#status-toast");

function setMode(live) {
  isLiveMode = live;
  if (modeLiveBtn && modeSimBtn) {
    modeLiveBtn.classList.toggle("active", live);
    modeLiveBtn.setAttribute("aria-checked", String(live));
    modeSimBtn.classList.toggle("active", !live);
    modeSimBtn.setAttribute("aria-checked", String(!live));
  }
  if (btnApply) {
    btnApply.textContent = live ? "⚡ Apply to Hardware" : "🧪 Test Dry-Run";
  }
  renderCommand();
}

if (modeLiveBtn) modeLiveBtn.addEventListener("click", () => setMode(true));
if (modeSimBtn) modeSimBtn.addEventListener("click", () => setMode(false));

function showToast(msg, type = "success") {
  if (!toast) return;
  toast.textContent = msg;
  toast.className = `toast ${type}`;
  toast.style.display = "inline-block";
  setTimeout(() => { toast.style.display = "none"; }, 4000);
}

function renderCommand() {
  const target = sync.checked ? "all" : active.id === "gpu" ? "gpu" : "mobo";
  const mappedMode = modes.find(([key]) => key === active.mode)[2];
  const nativeMode = target === "gpu" && !gpuModes.has(mappedMode) ? "static" : mappedMode;
  const args = isLiveMode ? ["sudo", "asus-aura-cli", target] : ["asus-aura-cli", "--dry-run", target];
  if (active.mode === "off") args.push("--off");
  else {
    args.push("--color", active.color.slice(1), "--mode", nativeMode);
    if (target === "mobo") args.push("--speed", active.speed);
  }
  command.textContent = args.join(" ");
  document.querySelector("#command-target").textContent = sync.checked ? "All devices" : active.name;
  let note = isLiveMode
    ? "Live Hardware Mode: Commands apply directly to your GTX 1060 and Z270E motherboard."
    : "Simulation Mode: Previews command safely without touching hardware.";
  if (target === "gpu" && nativeMode !== mappedMode) note += ` (GPU uses static color for ${active.mode}).`;
  document.querySelector("#command-note").textContent = note;
}

if (btnApply) {
  btnApply.addEventListener("click", async () => {
    const target = sync.checked ? "all" : active.id === "gpu" ? "gpu" : "mobo";
    const mappedMode = modes.find(([key]) => key === active.mode)[2];
    const nativeMode = target === "gpu" && !gpuModes.has(mappedMode) ? "static" : mappedMode;
    btnApply.disabled = true;
    btnApply.textContent = "Applying...";
    try {
      const resp = await fetch("/api/apply", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          target,
          dry_run: !isLiveMode,
          color: active.color.slice(1),
          mode: nativeMode,
          speed: active.speed,
          off: active.mode === "off"
        })
      });
      const data = await resp.json();
      if (data.ok) {
        showToast(isLiveMode ? "⚡ Applied to Hardware!" : "🧪 Dry-Run Passed!", "success");
      } else {
        showToast(`Error: ${data.error || "Failed to apply"}`, "error");
      }
    } catch (e) {
      showToast(`Server not responding: Run 'python3 web/serve.py'`, "error");
    } finally {
      btnApply.disabled = false;
      btnApply.textContent = isLiveMode ? "⚡ Apply to Hardware" : "🧪 Test Dry-Run";
    }
  });
}

if (btnCopy) {
  btnCopy.addEventListener("click", () => {
    navigator.clipboard.writeText(command.textContent).then(
      () => showToast("📋 Command copied to clipboard!", "success"),
      () => showToast("Failed to copy", "error")
    );
  });
}

// Initial fetch from live hardware status if API is available
async function initLiveStatus() {
  if (window.location.protocol === "file:") return;
  try {
    const resp = await fetch("/api/status");
    const json = await resp.json();
    if (json.ok && json.data) {
      const gpu = json.data.gpu;
      if (gpu && gpu.color_hex) {
        channels[0].color = "#" + gpu.color_hex;
        if (gpu.mode) channels[0].mode = gpu.mode;
      }
      const mobo = json.data.motherboard;
      if (mobo && mobo.zones) {
        mobo.zones.forEach((z, i) => {
          if (channels[i + 1] && z.hex) channels[i + 1].color = "#" + z.hex;
        });
      }
      render();
    }
  } catch (_) {
    // Static mode fallback
  }
}

sync.addEventListener("change", () => { if (sync.checked) mirror(active); render(); });
const animate = document.querySelector("#animate");
if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) animate.checked = false;
animate.addEventListener("change", () => document.body.classList.toggle("paused", !animate.checked));
document.body.classList.toggle("paused", !animate.checked);
render();
initLiveStatus();
