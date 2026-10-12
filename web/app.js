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

function renderCommand() {
  const target = sync.checked ? "all" : active.id === "gpu" ? "gpu" : "mobo";
  const mappedMode = modes.find(([key]) => key === active.mode)[2];
  const nativeMode = target === "gpu" && !gpuModes.has(mappedMode) ? "static" : mappedMode;
  const args = ["asus-aura-cli", "--dry-run", target];
  if (active.mode === "off") args.push("--off");
  else {
    args.push("--color", active.color.slice(1), "--mode", nativeMode);
    if (target === "mobo") args.push("--speed", active.speed);
  }
  command.textContent = args.join(" ");
  document.querySelector("#command-target").textContent = sync.checked ? "All devices" : active.name;
  let note = "Commands are previews only; this page never accesses hardware.";
  if (target === "gpu" && nativeMode !== mappedMode) note = `GPU uses static color for ${active.mode}; that effect is available on the motherboard.`;
  else if (target === "all") note = "Sync All uses the CLI's default effect speed. GPU effects without an equivalent fall back to static color.";
  else if (target === "mobo") note = "This command affects all motherboard LEDs. Speed is ignored by static color and off commands.";
  document.querySelector("#command-note").textContent = note;
}

sync.addEventListener("change", () => { if (sync.checked) mirror(active); render(); });
const animate = document.querySelector("#animate");
if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) animate.checked = false;
animate.addEventListener("change", () => document.body.classList.toggle("paused", !animate.checked));
document.body.classList.toggle("paused", !animate.checked);
render();
