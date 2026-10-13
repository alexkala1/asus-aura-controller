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
// The GPU's only autonomous effect is its Spectrum color cycle: rainbow-style effects run it.
const gpuSpectrumModes = new Set(["spectrum", "rainbow", "rainbow-breathing", "wave", "rainbow-pulse"]);
// Effects that draw from a configured color; all others generate their own colors.
const colorModes = new Set(["static", "breathing", "flashing", "chase-fade", "chase"]);
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
const nativeMode = channel => channel.nativeMode || modes.find(([key]) => key === channel.mode)[2];
function effectiveMode(channel) {
  const mode = nativeMode(channel);
  return channel.id !== "gpu" ? mode : gpuSpectrumModes.has(mode) ? "spectrum" : gpuModes.has(mode) ? mode : "static";
}
const configurableColor = channel => channel.mode !== "off" && colorModes.has(effectiveMode(channel));
const autonomousColor = channel => channel.mode !== "off" && !configurableColor(channel);
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
      // Static needs an explicit staged color even when the first status was autonomous.
      if (key === "mode" && effectiveMode(target) === "static" && !target.color) {
        target.color = "#ffffff";
        if (target.id === "mobo") zoneColors.fill(target.color);
      }
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
    channel.card.querySelector(".color-picker").disabled = !connected || busy || !configurableColor(channel)
      || (sync.checked && !colorModes.has(nativeMode(channel)));
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
    const autonomous = autonomousColor(channel);
    card.style.setProperty("--led", autonomous ? "var(--color-accent)" : channel.color || "var(--led-off)");
    card.style.setProperty("--duration", `${3 + channel.speed * 1.5}s`);
    card.dataset.mode = connected ? channel.mode : "off";
    card.dataset.colorSource = autonomous ? "autonomous" : "configured";
    if (channel.color) card.querySelector(".color-picker").value = channel.color;
    card.querySelector(".hex-value").textContent = channel.mode === "off" ? "Off" : autonomous ? "Auto" : channel.color ? channel.color.toUpperCase() : "—";
    card.querySelector(".mode-picker").value = channel.mode;
    card.querySelector(".speed-slider").value = channel.speed;
    card.querySelector(".speed-value").textContent = channel.id === "gpu" ? "Fixed" : channel.speed;
  }
  const led = channel => channel.mode === "off" ? "var(--led-off)" : autonomousColor(channel) ? "var(--color-accent)" : channel.color || "var(--led-off)";
  $(".gpu-hardware").style.setProperty("--gpu-led", led(channels[0]));
  const motherboard = channels[1];
  document.querySelectorAll(".zone-led").forEach((element, index) => {
    const color = zoneColors[index];
    const autonomous = autonomousColor(motherboard);
    element.style.setProperty("--zone-led", led({ ...motherboard, color }));
    element.dataset.color = autonomous ? "" : color || "";
    element.dataset.colorSource = autonomous ? "autonomous" : "configured";
    element.setAttribute("role", "img");
    element.setAttribute("aria-label", `${index < 4 ? "Back I/O" : "RGB Header"} zone ${index}: ${motherboard.mode === "off" ? "off" : autonomous ? "automatic effect colors" : !color ? "unverified" : color.toUpperCase()}`);
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
  const mapped = nativeMode(active);
  const off = active.mode === "off";
  // Mode name sent to the CLI. `all` takes the native name and maps each device itself;
  // a GPU-only command must name a mode the GPU has.
  const mode = target !== "gpu" ? mapped
    : gpuSpectrumModes.has(mapped) ? "spectrum"
    : gpuModes.has(mapped) ? mapped : "static";
  // Autonomous effects (rainbow, wave, ...) never get a color flag.
  const needsColor = target === "gpu" ? ["static", "breathing", "flashing"].includes(mode) : colorModes.has(mapped);
  return { target, mode, mapped, color: needsColor ? active.color?.slice(1) : undefined, speed: active.speed, off };
}

function commandNote({ target, mode, mapped }) {
  const gpuRuns = gpuSpectrumModes.has(mapped) ? "its Spectrum color cycle"
    : gpuModes.has(mapped) ? null : "a static color";
  const effect = active.mode;
  if (target === "gpu") {
    if (gpuSpectrumModes.has(mapped) && mapped !== "spectrum") return `GPU runs its Spectrum color cycle for ${effect}.`;
    if (!gpuModes.has(mapped)) return `GPU uses static color for ${effect}; that effect is motherboard-only.`;
    return "Applies to the GTX 1060. GPU effect speed is fixed by hardware.";
  }
  if (target === "mobo") return colorModes.has(mapped)
    ? "Applies to all motherboard LEDs. Static color and off ignore speed."
    : "Applies to all motherboard LEDs. This effect generates its own colors, so no color is sent.";
  const own = colorModes.has(mapped) ? "" : " The effect generates its own colors, so no color is sent.";
  return gpuRuns
    ? `Applies to both devices. The GPU runs ${gpuRuns}.${own}`
    : `Applies to both devices.${own}`;
}

function renderCommand() {
  if (!connected) {
    $("#command-preview").textContent = "Waiting for verified hardware…";
    return;
  }
  const chosen = selection();
  const { target, mode, color, speed, off } = chosen;
  const args = ["sudo", "luminactl", target];
  if (off) args.push("--off");
  else {
    if (color) args.push("--color", color);
    args.push("--mode", mode);
    if (target === "mobo") args.push("--speed", speed);
  }
  $("#command-preview").textContent = args.join(" ");
  $("#command-target").textContent = sync.checked ? "Both devices" : active.name;
  $("#command-note").textContent = commandNote(chosen);
}

function notify(message, state) {
  $("#status-toast").textContent = message;
  $("#status-toast").dataset.state = state;
}

async function api(path, options = {}) {
  const response = await fetch(path, { ...options, cache: "no-store", signal: AbortSignal.timeout(12000) });
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
    // Legacy daemons have no metadata: infer whether their RGB buffers are active.
    const hasConfiguredColor = (status, channel) => status.color_configurable ?? configurableColor(channel);
    if (!dirty.gpu) {
      Object.assign(channels[0], { mode: gpuMode, nativeMode: gpu.mode.replaceAll("_", "-") });
      if (gpu.color_source !== "autonomous" && hasConfiguredColor(gpu, channels[0])) channels[0].color = `#${gpu.color_hex}`;
    }
    if (!dirty.mobo) {
      Object.assign(channels[1], { mode: moboMode, nativeMode: mobo.mode.replaceAll("_", "-"), speed: mobo.speed });
      if (mobo.color_source !== "autonomous" && hasConfiguredColor(mobo, channels[1])) {
        zoneColors = mobo.zones.map(zone => `#${zone.hex}`);
        channels[1].color = zoneColors[0];
      }
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
  const { mapped, ...payload } = selection(); // `mapped` is display-only
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
