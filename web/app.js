"use strict";

const modes = [
  ["static", "Static", "static"], ["breathing", "Breathing", "breathing"],
  ["strobing", "Strobing", "flashing"], ["color-cycle", "Color cycle", "spectrum"],
  ["rainbow", "Rainbow", "rainbow"], ["comet", "Comet", "chase-fade"],
  ["flash-and-dash", "Flash and dash", "spectrum-chase-fade"], ["wave", "Wave", "wave"],
  ["glowing-yoyo", "Glowing yoyo", "rainbow-pulse"], ["starry-night", "Starry night", "random-flicker"],
  ["off", "Off", "off"],
];
// Effects each detected device reports (`supported_modes` in /api/status); the full list until known.
const supported = { gpu: modes.map(([key]) => key), mobo: modes.map(([key]) => key) };
// Sync All can only offer what every linked device runs.
const allowedModes = channel => (sync.checked ? supported.gpu.filter(key => supported.mobo.includes(key)) : supported[deviceKey(channel)]);
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
// null edits the whole board; a number edits that one LED (Static only).
let activeZone = null;
const zoneDirty = new Set(); // zones with a staged color the hardware does not have yet
// True only when the user chose a board-wide color (or Static). Other motherboard edits mark
// the device dirty without implying an RGB write that would overwrite individual zones.
let boardColorStaged = false;
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
// Speed reaches the hardware only for motherboard effects; Static, Off and the GPU ignore it.
const speedApplies = channel => channel.id !== "gpu" && channel.mode !== "static" && channel.mode !== "off";
const configurableColor = channel => channel.mode !== "off" && colorModes.has(effectiveMode(channel));
const autonomousColor = channel => channel.mode !== "off" && !configurableColor(channel);
let refreshing = false;
const sync = $("#sync-all");
const apply = $("#btn-apply");
const copy = $("#btn-copy");
const refresh = $("#btn-refresh");
const allZones = $("#btn-all-zones");
const discardZones = $("#btn-discard-zones");
const zoneButtons = [...document.querySelectorAll(".zone-btn")];
const zoneName = index => `${index < 4 ? "Back I/O" : "RGB Header"} zone ${index}`;
// Zones need a settled, unlinked board in Static; a pending apply only pauses edits.
const zonesAvailable = () => connected && !sync.checked && channels[1].mode === "static";
const zonesEditable = () => zonesAvailable() && !busy && !refreshing;

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
  channel.card = card;
  const update = (key, value) => {
    if (!connected || busy) return;
    active = channel;
    if (key === "color" && channel.id === "mobo" && activeZone !== null && zonesAvailable()) {
      zoneColors[activeZone] = value;
      zoneDirty.add(activeZone);
      setControlState("default");
      notify(`${zoneName(activeZone)} staged. Apply to update hardware.`, "default");
      render();
      return;
    }
    // The CLI writes motherboard settings device-wide, never per zone.
    const linked = sync.checked ? channels : [channel];
    // Static ignores speed in the driver: staging it would only mark the board dirty.
    if (key === "speed" && linked.every(target => !speedApplies(target))) return;
    channel[key] = value;
    let replacedZones = false;
    for (const target of linked) {
      target[key] = value;
      if (key === "color" && target.id === "mobo") {
        replacedZones ||= zoneDirty.size > 0;
        zoneColors.fill(value);
        zoneDirty.clear();
        boardColorStaged = true;
      }
      if (key === "mode") target.nativeMode = null;
      if (key === "mode" && target.id === "mobo") {
        activeZone = null;
        // An explicit effect choice replaces staged zones; Static keeps them (zones are Static-only).
        if (value !== "static") { replacedZones ||= zoneDirty.size > 0; zoneDirty.clear(); boardColorStaged = false; }
        else if (!zoneDirty.size) boardColorStaged = true;
      }
      // Static needs an explicit staged color even when the first status was autonomous.
      if (key === "mode" && effectiveMode(target) === "static" && !target.color) {
        target.color = "#ffffff";
        if (target.id === "mobo") zoneColors.fill(target.color);
      }
    }
    for (const target of linked) dirty[deviceKey(target)] = true;
    setControlState("default");
    notify(replacedZones ? "Staged zone colors were replaced. Apply to update hardware." : "Changes staged. Apply to update hardware.", "default");
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
    if (!speedApplies(channel)) channel.card.querySelector(".speed-slider").disabled = true;
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
    const zoned = channel.id === "mobo" && activeZone !== null;
    const shown = zoned ? zoneColors[activeZone] : channel.color;
    card.querySelector(".color-picker").setAttribute("aria-label", `${channel.name}${zoned ? ` ${zoneName(activeZone)}` : ""} color`);
    card.style.setProperty("--led", autonomous ? "var(--color-accent)" : shown || "var(--led-off)");
    card.style.setProperty("--duration", `${3 + channel.speed * 1.5}s`);
    card.dataset.mode = connected ? channel.mode : "off";
    card.dataset.colorSource = autonomous ? "autonomous" : "configured";
    if (shown) card.querySelector(".color-picker").value = shown;
    card.querySelector(".hex-value").textContent = channel.mode === "off" ? "Off" : autonomous ? "Auto" : shown ? shown.toUpperCase() : "—";
    const picker = card.querySelector(".mode-picker");
    const allowed = allowedModes(channel);
    if (picker.options.length !== allowed.length || allowed.some((key, i) => picker.options[i].value !== key)) {
      picker.replaceChildren(...allowed.map(key => new Option(modes.find(([value]) => value === key)[1], key)));
    }
    picker.value = channel.mode;
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
    const button = zoneButtons[index];
    button.setAttribute("aria-label", `${zoneName(index)}: ${motherboard.mode === "off" ? "off" : autonomous ? "automatic effect colors" : !color ? "unverified" : color.toUpperCase()}${zoneDirty.has(index) ? ", staged" : ""}`);
    button.setAttribute("aria-pressed", String(activeZone === index));
    button.setAttribute("aria-disabled", String(!zonesEditable()));
  });
  renderZoneHint();
  $(".board-map").dataset.mode = motherboard.mode;
  $(".board-map").style.setProperty("--duration", `${3 + motherboard.speed * 1.5}s`);
  $("#sync-state").textContent = sync.checked ? "Both devices linked" : "Control each device independently";
  markActive();
  renderCommand();
  lockControls();
}

function renderZoneHint() {
  allZones.hidden = activeZone === null;
  allZones.disabled = busy;
  discardZones.hidden = zoneDirty.size === 0;
  discardZones.disabled = busy;
  let hint = "";
  if (connected) {
    if (sync.checked) hint = "Zone colors are unavailable while Sync All is on.";
    else if (zoneDirty.size && channels[1].mode !== "static") hint = `The board is now running ${channels[1].mode}. Staged zone colors are kept; applying them switches the board to Static. Discard them to keep the effect.`;
    else if (channels[1].mode !== "static") hint = "Zone colors need the Static effect.";
    else if (activeZone === null) hint = "Editing all five LEDs. Select a zone to color it on its own.";
    else hint = `Editing ${zoneName(activeZone)}. The other LEDs keep their colors.${zoneDirty.has(activeZone) ? "" : " Pick a color to stage it."}`;
  }
  $("#zone-hint").textContent = hint;
}

function selectZone(index) {
  if (!zonesEditable()) return;
  activeZone = activeZone === index ? null : index;
  active = channels[1];
  render();
}
zoneButtons.forEach((button, index) => button.addEventListener("click", () => selectZone(index)));
allZones.addEventListener("click", () => { activeZone = null; render(); });
discardZones.addEventListener("click", () => {
  zoneDirty.clear();
  activeZone = null;
  notify("Staged zone colors discarded.", "default");
  // Show what the hardware really has again.
  refreshStatus();
});

// Staged zone colors, one request each: the CLI sets a single LED per command.
function zonePayloads() {
  return [...zoneDirty].sort().map(zone => ({ target: "mobo", zone, color: zoneColors[zone].slice(1), mode: "static" }));
}

// What Apply sends, in order: the staged device-wide change first, then any staged zones.
// A request may only name an effect its target hardware (or, linked, both devices) can run.
const effectAllowed = () => allowedModes(active).includes(active.mode);

function reconcileLinkedState() {
  if (!sync.checked) return;
  const common = allowedModes(active);
  const staged = dirty.gpu || dirty.mobo;
  const representable = channels.every(channel => common.includes(channel.mode)) && channels[0].mode === channels[1].mode;
  if (representable || (staged && channels.every(channel => common.includes(channel.mode)))) return;
  sync.checked = false;
  notify("Sync All turned off: the devices are running different effects. Each shows its own state.", "default");
}

function requests() {
  const wide = selection();
  const zones = active.id === "mobo" && !sync.checked ? zonePayloads() : [];
  if (!zones.length) return [wide];
  // A board-wide write precedes zones only when one was asked for: an effect/mode change, or an
  // explicit board color. Anything else would repaint LEDs the user never touched.
  const wideRequested = dirty.mobo && (boardColorStaged || channels[1].mode !== "static");
  return wideRequested ? [wide, ...zones] : zones;
}

function commandLine({ target, zone, mode, color, speed, off }) {
  const args = ["sudo", "luminactl", target];
  if (zone !== undefined) args.push("--zone", zone, "--color", color);
  else if (off) args.push("--off");
  else {
    if (color) args.push("--color", color);
    args.push("--mode", mode);
    if (target === "mobo") args.push("--speed", speed);
  }
  return args.join(" ");
}

function selection() {
  const target = sync.checked ? "all" : active.id === "gpu" ? "gpu" : "mobo";
  // Linked: the shared Studio effect decides; a device's readback spelling (GPU `spectrum`)
  // must not leak into the `all` request, which would run Spectrum Cycle on the board.
  const mapped = sync.checked ? modes.find(([key]) => key === active.mode)[2] : nativeMode(active);
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
  const list = requests();
  $("#command-preview").textContent = list.map(commandLine).join("\n");
  $("#command-target").textContent = sync.checked ? "Both devices" : active.name;
  $("#command-note").textContent = list.some(request => request.zone !== undefined)
    ? "Sets one motherboard LED per command; the other LEDs keep their colors."
    : commandNote(chosen);
}

function notify(message, state) {
  $("#status-toast").textContent = message;
  $("#status-toast").dataset.state = state;
}

const httpMessages = {
  404: "The daemon does not know that request. Restart python3 web/serve.py after updating.",
  405: "The daemon does not support that request.",
  408: "The daemon timed out reading the request. Try again.",
  413: "The request was too large for the daemon.",
  421: "The daemon refused the request host. Open http://127.0.0.1:7890 directly.",
  501: "The daemon does not support that request.",
  503: "The daemon is busy or the hardware is unavailable. Retry in a moment.",
};

// Turns anything the daemon (or a proxy in front of it) can answer into one readable
// message: never a raw JSON parser error.
async function api(path, options = {}) {
  let response;
  try {
    response = await fetch(path, { ...options, cache: "no-store", signal: AbortSignal.timeout(12000) });
  } catch (error) {
    if (error?.name === "TimeoutError" || error?.name === "AbortError") throw new Error("The daemon did not answer in time. Check that it is still running.");
    throw new Error("Cannot reach the daemon. Start it with python3 web/serve.py.");
  }
  let data = null;
  if ((response.headers.get("content-type") || "").includes("application/json")) {
    try { data = await response.json(); } catch { data = null; }
  }
  if (!response.ok || !data?.ok) {
    throw new Error(data?.error || httpMessages[response.status] || `Daemon request failed (${response.status}).`);
  }
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
  const displayMode = (value, device = "mobo") => {
    const native = typeof value === "string" ? value.replaceAll("_", "-") : "";
    // The GPU's Spectrum is its color cycle, listed as Rainbow.
    if (device === "gpu" && native === "spectrum") return "rainbow";
    return modes.find(([, , name]) => name === native)?.[0] || aliases[native];
  };
  if (!displayMode(gpu.mode, "gpu") || !displayMode(mobo.mode)) throw new Error("Unsupported hardware effect reported. Refresh after selecting a supported CLI mode.");
  if (!Number.isInteger(mobo.speed) || mobo.speed < 0 || mobo.speed > 4) throw new Error("Hardware effect speed is outside the supported range.");
  // Direct output overrides a stale effect code in the board's mode register.
  const moboMode = mobo.color_source === "direct" ? "static" : displayMode(mobo.mode);
  return { gpu, mobo, gpuMode: displayMode(gpu.mode, "gpu"), moboMode };
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
    for (const [key, status] of [["gpu", gpu], ["mobo", mobo]]) {
      const known = Array.isArray(status.supported_modes) ? status.supported_modes.filter(name => modes.some(([value]) => value === name)) : [];
      if (known.length) supported[key] = modes.map(([value]) => value).filter(value => known.includes(value));
    }
    const hasConfiguredColor = (status, channel) => status.color_configurable ?? configurableColor(channel);
    if (!dirty.gpu) {
      Object.assign(channels[0], { mode: gpuMode, nativeMode: gpu.mode.replaceAll("_", "-") });
      if (gpu.color_source !== "autonomous" && hasConfiguredColor(gpu, channels[0])) channels[0].color = `#${gpu.color_hex}`;
    }
    if (!dirty.mobo) {
      Object.assign(channels[1], { mode: moboMode, nativeMode: mobo.color_source === "direct" ? "static" : mobo.mode.replaceAll("_", "-"), speed: mobo.speed });
      // The board changed under us: drop the zone selection but keep staged colors so the
      // user can apply or discard them (see the zone hint).
      if (moboMode !== "static") activeZone = null;
      if (mobo.color_source !== "autonomous" && hasConfiguredColor(mobo, channels[1])) {
        const hardware = mobo.zones.map(zone => `#${zone.hex}`);
        channels[1].color = hardware[0];
        // Staged zone colors survive the periodic refresh until they are applied.
        for (const zone of zoneDirty) hardware[zone] = zoneColors[zone];
        zoneColors = hardware;
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
    reconcileLinkedState();
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
  if (!effectAllowed()) {
    notify("This device cannot run the selected effect. Pick a supported effect before applying.", "error");
    return;
  }
  const list = requests();
  busy = true;
  setControlState("loading");
  lockControls();
  apply.textContent = "Applying…";
  notify("Writing lighting settings to hardware…", "loading");
  let succeeded = false;
  try {
    for (const { mapped, ...payload } of list) { // `mapped` is display-only
      await api("/api/apply", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
      // Clear staging per request so a later failure leaves only the unsent ones staged.
      if (payload.zone !== undefined) zoneDirty.delete(payload.zone);
      else {
        for (const channel of payload.target === "all" ? channels : [channels[payload.target === "gpu" ? 0 : 1]]) dirty[deviceKey(channel)] = false;
        if (payload.target !== "gpu") boardColorStaged = false;
      }
    }
    succeeded = true;
    const pending = channels.find(channel => dirty[deviceKey(channel)]) || (zoneDirty.size ? channels[1] : null);
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
    // Redraw preview, selection and staged markers: after a partial failure only the unsent
    // zones may remain visible.
    render();
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
    if (!allowedModes(active).includes(active.mode)) {
      Object.assign(active, { mode: "static", nativeMode: null });
      if (!active.color) active.color = "#ffffff";
    }
    // The shared effect is the Studio effect on screen; device-specific readback spellings
    // (GPU `spectrum`, board `rainbow-breathing`) must not leak into the linked request.
    for (const channel of channels) Object.assign(channel, { color: active.color, mode: active.mode, nativeMode: null, speed: active.speed });
    zoneColors.fill(active.color);
    activeZone = null;
    zoneDirty.clear();
    boardColorStaged = true;
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
