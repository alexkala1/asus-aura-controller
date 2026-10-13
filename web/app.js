"use strict";

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
  { id: "gpu", name: "GTX 1060 logo", tag: "GPU", color: null, mode: "static", speed: 0 },
  { id: "mobo", name: "Motherboard lighting", tag: "5 LEDs", color: null, mode: "static", speed: 0 },
];
let zoneColors = Array(5).fill(null);
const $ = selector => document.querySelector(selector);
let active = channels[0];
let connected = false;
let busy = false;
// Staged (unapplied) edits are tracked per device so applying or refreshing one device
// never discards the other's pending changes.
const dirty = { gpu: false, mobo: false };
const deviceKey = channel => (channel.id === "gpu" ? "gpu" : "mobo");
let refreshing = false;
const sync = $("#sync-all");
const apply = $("#btn-apply");
const copy = $("#btn-copy");
const refresh = $("#btn-refresh");

for (const channel of channels) {
  const card = $("#channel-template").content.firstElementChild.cloneNode(true);
  card.id = channel.id;
  card.dataset.channel = channel.id;
  card.querySelector("h3").textContent = channel.name;
  card.querySelector(".channel-id").textContent = channel.tag;
  card.querySelector(".preview-wordmark").textContent = "GTX 1060";
  if (channel.id === "mobo") card.querySelector(".led-preview").remove();
  for (const [selector, suffix, label] of [[".color-picker", "color", "color"], [".mode-picker", "mode", "effect"], [".speed-slider", "speed", "speed"]]) {
    const input = card.querySelector(selector);
    input.id = `${channel.id}-${suffix}`;
    input.setAttribute("aria-label", `${channel.name} ${label}`);
    input.setAttribute("aria-describedby", "connection-help command-note");
    input.closest("label").htmlFor = input.id;
    input.disabled = true;
  }
  for (const [value, label] of modes) card.querySelector(".mode-picker").add(new Option(label, value));
  channel.card = card;
  const update = (key, value) => {
    if (!connected || busy) return;
    active = channel;
    channel[key] = value;
    // The CLI writes motherboard settings device-wide, never per zone.
    const linked = sync.checked ? channels : [channel];
    for (const target of linked) {
      target[key] = value;
      if (key === "color" && target.id === "mobo") zoneColors.fill(value);
      if (key === "mode") target.nativeMode = null;
    }
    for (const target of linked) dirty[deviceKey(target)] = true;
    setControlState("default");
    notify("Changes staged. Apply to update hardware.", "default");
    render();
  };
  card.querySelector(".color-picker").addEventListener("input", event => update("color", event.target.value));
  card.querySelector(".mode-picker").addEventListener("change", event => update("mode", event.target.value));
  card.querySelector(".speed-slider").addEventListener("input", event => update("speed", Number(event.target.value)));
  card.addEventListener("focusin", () => { active = channel; renderCommand(); markActive(); });
  $(channel.id === "gpu" ? "#gpu-controls" : "#mobo-controls").append(card);
}

function markActive() {
  for (const channel of channels) channel.card.classList.toggle("selected", connected && (sync.checked || channel === active));
}

function setControlState(state) {
  for (const element of document.querySelectorAll("input, select, button, .controls")) {
    element.dataset.state = state;
    element.setAttribute("aria-busy", String(state === "loading"));
    if (element.matches("input, select")) element.setAttribute("aria-invalid", String(state === "error"));
  }
}

function lockControls() {
  for (const channel of channels) {
    for (const input of channel.card.querySelectorAll("input, select")) input.disabled = !connected || busy;
    if (channel.id === "gpu") channel.card.querySelector(".speed-slider").disabled = true;
  }
  sync.disabled = !connected || busy;
  apply.disabled = !connected || busy || refreshing;
  copy.disabled = !connected || busy;
  refresh.disabled = busy || refreshing;
}

function render() {
  for (const channel of channels) {
    const card = channel.card;
    card.style.setProperty("--led", channel.color || "var(--led-off)");
    card.style.setProperty("--duration", `${3 + channel.speed * 1.5}s`);
    card.dataset.mode = channel.color ? channel.mode : "off";
    if (channel.color) card.querySelector(".color-picker").value = channel.color;
    card.querySelector(".hex-value").textContent = channel.color ? channel.color.toUpperCase() : "—";
    card.querySelector(".mode-picker").value = channel.mode;
    card.querySelector(".speed-slider").value = channel.speed;
    card.querySelector(".speed-value").textContent = channel.id === "gpu" ? "Fixed" : channel.speed;
  }
  const led = channel => channel.mode === "off" || !channel.color ? "var(--led-off)" : channel.color;
  $(".gpu-hardware").style.setProperty("--gpu-led", led(channels[0]));
  const motherboard = channels[1];
  document.querySelectorAll(".zone-led").forEach((element, index) => {
    const color = zoneColors[index];
    element.style.setProperty("--zone-led", motherboard.mode === "off" || !color ? "var(--led-off)" : color);
    element.dataset.color = color || "";
    element.setAttribute("role", "img");
    element.setAttribute("aria-label", `${index < 4 ? "Back I/O" : "RGB Header"} zone ${index}: ${!color ? "unverified" : motherboard.mode === "off" ? "off" : color.toUpperCase()}`);
  });
  $(".board-map").dataset.mode = motherboard.mode;
  $(".board-map").style.setProperty("--duration", `${3 + motherboard.speed * 1.5}s`);
  $("#sync-state").textContent = sync.checked ? "Both devices linked" : "Control each device independently";
  markActive();
  renderCommand();
  lockControls();
}

function selection() {
  const target = sync.checked ? "all" : active.id === "gpu" ? "gpu" : "mobo";
  const mapped = active.nativeMode || modes.find(([key]) => key === active.mode)[2];
  const mode = target === "gpu" && !gpuModes.has(mapped) ? "static" : mapped;
  return { target, mode, color: active.color?.slice(1), speed: active.speed, off: active.mode === "off" };
}

function renderCommand() {
  if (!active.color) {
    $("#command-preview").textContent = "Waiting for verified hardware…";
    return;
  }
  const { target, mode, color, speed, off } = selection();
  const args = ["sudo", "luminactl", target];
  if (off) args.push("--off");
  else {
    args.push("--color", color, "--mode", mode);
    if (target === "mobo") args.push("--speed", speed);
  }
  $("#command-preview").textContent = args.join(" ");
  $("#command-target").textContent = sync.checked ? "Both devices" : active.name;
  $("#command-note").textContent = target === "gpu" && mode !== modes.find(([key]) => key === active.mode)[2]
    ? `GPU uses static color for ${active.mode}; that effect is motherboard-only.`
    : target === "mobo" ? "Applies to all motherboard LEDs. Static color and off ignore speed."
    : target === "all" ? "Applies to both devices. Motherboard-only effects use static color on the GPU."
    : "Applies to the GTX 1060. GPU effect speed is fixed by hardware.";
}

function notify(message, state) {
  $("#status-toast").textContent = message;
  $("#status-toast").dataset.state = state;
}

async function api(path, options = {}) {
  const response = await fetch(path, { ...options, signal: AbortSignal.timeout(12000) });
  const data = await response.json();
  if (!response.ok || !data.ok) throw new Error(data.error || `Daemon request failed (${response.status}).`);
  return data;
}

function hardwareData(data) {
  const gpu = data?.gpu;
  const mobo = data?.motherboard;
  const validHex = value => typeof value === "string" && /^[0-9a-f]{6}$/i.test(value);
  if (gpu?.magic_verified !== true || gpu.bus !== "/dev/i2c-0" || gpu.address !== "0x29"
      || !validHex(gpu.color_hex) || !mobo?.firmware?.startsWith("LED-0116")
      || mobo.smbus_port !== "0x02c0" || mobo.slave_address !== "0x4e"
      || mobo.led_count !== 5 || !Array.isArray(mobo.zones) || mobo.zones.length !== 5
      || !mobo.zones.every((zone, i) => zone.led_index === i && validHex(zone.hex))) {
    throw new Error("Hardware identity or LED data could not be verified. Check the daemon and device connections.");
  }
  const aliases = { "rainbow-breathing": "breathing", chase: "comet", "spectrum-chase": "flash-and-dash" };
  const displayMode = value => {
    const native = typeof value === "string" ? value.replaceAll("_", "-") : "";
    return modes.find(([, , name]) => name === native)?.[0] || aliases[native];
  };
  if (!displayMode(gpu.mode) || !displayMode(mobo.mode)) throw new Error("Unsupported hardware effect reported. Refresh after selecting a supported CLI mode.");
  if (!Number.isInteger(mobo.speed) || mobo.speed < 0 || mobo.speed > 4) throw new Error("Hardware effect speed is outside the supported range.");
  return { gpu, mobo, gpuMode: displayMode(gpu.mode), moboMode: displayMode(mobo.mode) };
}

async function refreshStatus() {
  if (busy || refreshing) return;
  refreshing = true;
  refresh.textContent = "Checking devices…";
  refresh.dataset.state = "loading";
  if (!connected) setControlState("loading");
  lockControls();
  try {
    if (location.protocol === "file:") throw new Error("Open http://127.0.0.1:7890 after starting python3 web/serve.py.");
    const result = await api("/api/status");
    if (result.mode !== "live") throw new Error("Daemon did not return live hardware status.");
    const { gpu, mobo, gpuMode, moboMode } = hardwareData(result.data);
    if (!dirty.gpu) {
      Object.assign(channels[0], { color: `#${gpu.color_hex}`, mode: gpuMode, nativeMode: gpu.mode.replaceAll("_", "-") });
    }
    if (!dirty.mobo) {
      zoneColors = mobo.zones.map(zone => `#${zone.hex}`);
      Object.assign(channels[1], { color: zoneColors[0], mode: moboMode, nativeMode: mobo.mode.replaceAll("_", "-"), speed: mobo.speed });
    }
    connected = true;
    $(".connection").dataset.state = "success";
    $("#live-status").textContent = "● LIVE";
    $("#daemon-status").textContent = `Daemon :${location.port || "7890"} · 2 devices detected`;
    $("#connection-help").textContent = "GPU and motherboard verified. Changes are staged until you apply them.";
    $("#gpu-state").textContent = "Verified";
    $("#mobo-state").textContent = "Verified";
    $("#firmware").textContent = mobo.firmware;
    setControlState("success");
  } catch (error) {
    connected = false;
    $(".connection").dataset.state = "error";
    $("#live-status").textContent = "● OFFLINE";
    $("#daemon-status").textContent = `Daemon :${location.port || "7890"} · hardware unverified`;
    $("#connection-help").textContent = error.message;
    $("#gpu-state").textContent = "Unverified";
    $("#mobo-state").textContent = "Unverified";
    $("#firmware").textContent = "unverified";
    setControlState("error");
  } finally {
    refreshing = false;
    refresh.textContent = "Refresh devices";
    render();
  }
}

apply.addEventListener("click", async () => {
  if (!connected || busy || refreshing) return;
  const payload = selection();
  busy = true;
  setControlState("loading");
  lockControls();
  apply.textContent = "Applying…";
  notify("Writing lighting settings to hardware…", "loading");
  let succeeded = false;
  try {
    await api("/api/apply", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
    const applied = payload.target === "all" ? channels : [channels[payload.target === "gpu" ? 0 : 1]];
    for (const channel of applied) dirty[deviceKey(channel)] = false;
    succeeded = true;
    const pending = channels.find(channel => dirty[deviceKey(channel)]);
    if (pending) {
      // Point the next Apply at the device that is still staged.
      active = pending;
      notify(`✓ Applied. ${pending.name} changes are still staged.`, "success");
    } else {
      notify("✓ Applied to hardware.", "success");
    }
    setControlState("success");
  } catch (error) {
    notify(`Apply failed: ${error.message} Settings remain staged; retry when ready.`, "error");
    setControlState("error");
  } finally {
    busy = false;
    apply.textContent = "Apply to hardware";
    lockControls();
  }
  if (succeeded) await refreshStatus();
});

copy.addEventListener("click", async () => {
  copy.dataset.state = "loading";
  try {
    await navigator.clipboard.writeText($("#command-preview").textContent);
    copy.dataset.state = "success";
    notify("✓ Command copied.", "success");
  } catch {
    copy.dataset.state = "error";
    notify("Copy unavailable. Select the command text and copy it manually.", "error");
  }
});
sync.addEventListener("change", () => {
  if (sync.checked) {
    for (const channel of channels) Object.assign(channel, { color: active.color, mode: active.mode, nativeMode: active.nativeMode, speed: active.speed });
    zoneColors.fill(active.color);
    dirty.gpu = dirty.mobo = true; // linking stages the mirrored settings on both devices
  }
  setControlState("default");
  render();
});
refresh.addEventListener("click", refreshStatus);
setControlState("loading");
render();
refreshStatus();
setInterval(refreshStatus, 10000);
