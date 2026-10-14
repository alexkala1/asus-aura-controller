# Rainbow, optional colors, and Web Studio repair

Date: 2026-10-14. Repository: `/home/alexkala/repos/luminactl`
(same checkout as `asus-aura-controller`).

## Changes and verification

- GPU spectrum updates use mode register `0x07 = 4`; removed the unsupported
  routine `0x0E = 1` write. No new `0x0C` writes were added.
- Spectrum/rainbow do not require a color. Provided GPU spectrum or combined
  rainbow colors are explicitly ignored without overwriting inactive stored
  RGB. Static/breathing/flashing keep their configured-color behavior.
- Status now includes `color_source` and `color_configurable`. Raw RGB fields
  remain compatible, but human output and Web Studio do not equate autonomous
  mode's stored zero bytes with a black/off animation.
- Web Studio displays **Auto**, disables irrelevant color pickers, preserves
  last configured colors, and applies rainbow without a `color` payload.
- Assets, API responses, and error responses have
  `Cache-Control: no-cache, no-store, must-revalidate`. Conditional asset
  requests return fresh `200`, including future `If-Modified-Since` probes.
- `cargo test --all`: **94 passed** (60 unit, 17 CLI, 17 MCP) on the final tree.
- `pytest tests/`: **103 passed**, using a temporary exact source/test copy to
  preserve the checkout's existing modified screenshots.
- One intermediate parallel Rust run timed out in the closed-stdout MCP test.
  The focused test and subsequent full suite passed; the timeout's underlying
  cause was not established. Concurrent Round Four edits/tests were preserved.
- Formatting, strict all-target Clippy, release build, and changed-file
  whitespace checks pass. Desktop/mobile Chromium verification had no
  console/page errors and no horizontal overflow.

## Vendor evidence

REA recognized the native target, but deep providers were unavailable
(Ghidra unconfigured, Hopper absent, IDA unconfigured). GNU objdump and local
PE parsing provided the following static evidence; no vendor executable ran.
Local ledger IDs LVE-01 through LVE-06 are analyst-assigned, not REA-issued IDs.
The detailed ledger is `/tmp/lumina-vendor-evidence.md`.

| Requested artifact under `upstream_binaries/extracted` | SHA-256 |
| --- | --- |
| `vga_binaries/LED_DLL_forMB_x64.dll` | `fbb5a6064c209059ee20d4c49c71d9f8d64f5ab1d0cea6adad04bae43ea7b099` |
| `mb_binaries/Aac3572MbHal_x64.exe` | `afe4583197683b04fe947f5179a3be74534d68d3fe56ef015032fa6c8fface3f` |
| `vga_binaries/AacVgaHal_x64.dll` | `c1732b2beba78f05a7df800332e2863cdaf856596d9c6297759789946a8facb8` |
| `ene_extracted/HAL_DLL_X64` | `506b9e40fd3539cd6e9a152e0378056e054c2ba014872b561ec80eb51e56f5af` |

### GPU: observed dispatch, not speculative register naming

`LED_DLL_forMB_x64.dll` export `fnSetLEDEFFECT` at `0x18000bff0` delegates to
`0x18000a580`. It packs flag `request+0x30 = 1` and mode `request+0x34`, then
calls the pointer resolved as `Set_LED_ExternalFan_Info_byCardIndex`.

The adjacent dependency is `VGA_Extra_x64.dll` (SHA-256
`852eae263a0ebdea7d30fb9c14516337b8be10118128c9821a500fed23c9865b`). Its
ordinary, non-`0xce` address mode branch at `0x180004d7b–0x180004d9b` writes
only `0x07 = requested mode`. Spectrum therefore selects `0x07 = 4` on that
branch. Actual GPU device binding is supported by its Linux address `0x29`
and successful live mode readback, rather than a recorded Windows execution.

The other requests are distinct:

- `fnSetLEDFrame`, wrapper `0x18000c5e0`, uses request flag `+0x40`;
  dispatcher `0x180004b06–0x180004b4e` writes register `0x0C`.
- `fnSetLEDBurn`, wrapper `0x18000c980`, uses request flag `+0x38`;
  ordinary branch `0x180004eab–0x180004eba` writes register `0x0E`.
- `fnApplySetting` at `0x18000bfe0` simply returns true.

This establishes separation from the mode request. It does not establish
what burn does to persistence/EEPROM, nor every higher-level Windows call.
The Linux normal mode path avoids those unrelated writes.

`AacVgaHal_x64.dll` resolves `fnSetLEDEFFECT` through `GetProcAddress` at
`0x1800160a2–0x1800160b6`; it is a delegate, not an independent register path.

### Motherboard: actual evidence boundary

The current Linux driver clears direct operation (`0x8020 = 0`), writes mode
`0x8021 = 5` for rainbow or `4` for spectrum, writes speed at `0x8022` and
direction at `0x8023`, then writes `0x80A0 = 1`. That control sequence remains
unchanged by this repair and is covered by mock-register tests. Live rainbow
readback indicates mode 5, direct operation disabled, speed 0 and forward
direction. It does not prove the exact vendor-required ordering.

The supplied ENE `HAL_DLL_X64` identifies itself as **external-drive RGB HAL
for ASMedia**, not LED-0116 motherboard support. Its function beginning
`0x180008490` writes a control block at `0x8020` and applies via `0x802f = 1`.
That external-drive sequence was deliberately not transplanted to the board.

`Aac3572MbHal_x64.exe` contains `Ec3572`/`BaseEc3572` RTTI. No LED-0116 binding
or exact requested register sequence was recovered. The larger nested
`Hal/AacMBSetup.exe` was statically extracted into CAB/MSI payloads; its x64
HAL is byte-for-byte the same Aac3572 binary, not an alternate older ENE HAL.
Bounded literal searches cannot exclude computed/table-driven access.

Live rainbow returns zero bytes from the configured effect color buffer
starting at `0x8010`. A configured-color buffer is not established as a live
frame sample; zero alone does not establish off/black or a failed autonomous
animation. The status/UI now make that distinction explicit. Exact LED-0116
vendor write order and internal frame-generation behavior remain unverified
from these supplied artifacts. A matching HAL or passive transaction capture
would be needed to resolve that remaining vendor question.

## Deployment and live results

Built with `cargo build --release` and installed root-owned mode 0755 to
`/usr/local/bin/luminactl`. Release and installed binary SHA-256 match:

`6ce950103ec62a017c244157a31c5155cca4dc3dad83cfc643b6461601fb498c`

Restarted `/usr/bin/python3 web/serve.py` as transient user unit
`luminactl-web.service`, with `LUMINA_CLI_BIN=/usr/local/bin/luminactl`.
It is active at `http://127.0.0.1:7890`.

| Requested live command | Result |
| --- | --- |
| `sudo luminactl mobo --mode rainbow` | Exit 0; motherboard mode rainbow, speed 0 |
| `sudo luminactl gpu --mode spectrum` | Exit 0; GPU mode spectrum |
| `sudo luminactl all --mode rainbow` | Exit 0; GPU spectrum + motherboard rainbow; no color required |

Subsequent live JSON status reports GPU magic verified and motherboard
firmware `LED-0116`, five LEDs, `color_source=autonomous`, and
`color_configurable=false` on both devices. Initial motherboard stored zones
were `000000`; after the optional-color test below they contain `112233`.
In either case the UI presents **Auto**, not a claimed live animation sample.

Additional authorized color/mode transition checks also passed:

- GPU static `--color 112233`: exit 0; RGB and static mode read back correctly
  without a burn write. Switching to spectrum then reads mode 4 correctly.
- Motherboard breathing `--color 112233`: exit 0; effect RGB reads `112233`
  with `color_source=effect`.
- Returning both devices to `all --mode rainbow`: exit 0; GPU spectrum and
  motherboard rainbow are retained, with stored colors correctly classified
  as inactive/autonomous rather than displayed as the animation's color.

Actual Web Studio Apply was also tested through headless Chromium:

```json
{"target":"all","mode":"rainbow","speed":0,"off":false}
```

There was no color field. The daemon reported success; both live modes were
retained. Screenshots: `/tmp/luminactl-rainbow-desktop.png` (1440px) and
`/tmp/luminactl-rainbow-mobile.png` (375px), inspected after capture.

These are command/readback and browser verification results. Physical light
output was not visually observed by this agent; no claim of measured emitted
colors or Windows runtime transactions is made.
