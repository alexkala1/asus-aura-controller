# Round Four Bug Hunt and Feature-Gap Audit

**Auditor:** Claude. **Audited revision:** `fb42c8b` (master, clean tree apart from the two test-rewritten screenshots).
**Scope:** whole repository: `src/{protocol,mobo,gpu,mcp,main,hardware_lock,diag}.rs`, `lumina_ctl.py`,
`web/{serve.py,app.js,index.html}`, tests, CI.

**Method and safety.** Read-only audit: this report is the only file added. Every reproduction used
`--dry-run`, an isolated loopback server with `subprocess.run` mocked, intercepted browser routes, or
reading code. No live hardware device was opened. Findings marked *code-confirmed* were not reproduced
because reproducing them needs a real device node (stated per finding).

**Result: 17 findings: 0 Critical, 0 High, 6 Medium, 10 Low, 1 Informational.** No finding gives a path to
an unintended register write that the Round 2/3 guards do not already stop.

**Coordination note (Codex is working on hardware registers and cache).** To avoid collisions, each
finding lists its files. Findings that touch `src/mobo.rs`, `src/gpu.rs`, `src/protocol.rs` or the static
file/cache behaviour of `web/serve.py` (R4-03, R4-11, R4-12, R4-02's daemon side) should wait for or be
merged by that session. R4-01, R4-04 (CLI part), R4-05, R4-06, R4-07, R4-08, R4-14, R4-15, R4-16 and the
`web/app.js` parts do not touch those files.

## Summary

| ID | Severity | Area | One line |
| --- | --- | --- | --- |
| R4-01 | Medium | MCP | SIGHUP/SIGQUIT end the server without restoring the baseline |
| R4-02 | Medium | Web UI | Any unknown hardware mode takes the whole UI offline with no way back |
| R4-03 | Medium | Daemon | No `Host` validation: DNS rebinding reads hardware status |
| R4-04 | Medium | CLI/Web | Sync All speed is silently dropped (`all` has no `--speed`) |
| R4-05 | Medium | Python | Reference tool diverges: `all --off` fails, no `--color`-less effects, direct mode never cleared |
| R4-06 | Medium | CLI | Mode vocabulary differs between `mobo`, `gpu`, `all`, MCP and the UI |
| R4-07 | Low | MCP | JSON-RPC batches rejected although 2025-03-26 is advertised |
| R4-08 | Low | MCP | `"id": null` requests are answered |
| R4-09 | Low | Web UI | Non-JSON error bodies surface as "Unexpected end of JSON input" |
| R4-10 | Low | Daemon | `/api/status?x=1` and `HEAD` are 404; other methods return HTML 501 |
| R4-11 | Low | Daemon | No clickjacking/CSP/nosniff headers; static files have no cache policy |
| R4-12 | Low | Daemon | Default CLI path is a user-writable build artifact that cannot work with the documented sudoers rule |
| R4-13 | Low | Daemon | Timeout kills `sudo`, not the `luminactl` it started (code-confirmed) |
| R4-14 | Low | CLI | `all --mode off --color X` prints a misleading success message |
| R4-15 | Low | MCP | Device-lock waits block the single main loop (timers/signals delayed up to ~10 s) (code-confirmed) |
| R4-16 | Low | Build | `File::try_lock` needs Rust 1.89 but there is no `rust-version` |
| R4-17 | Low | Feature gap | No per-zone or direction control in the CLI/UI although the driver and MCP support zones |
| R4-18 | Info | Hardware | Shared SMBus host: coordination with kernel/ACPI users is unverified |

## Findings

### R4-01 — Medium — SIGHUP/SIGQUIT kill the MCP server without restoring lighting

**Files:** `src/mcp.rs` (`serve`, signal setup: only signals 2 and 15 are handled).
**Scenario:** the terminal or SSH session that owns the server closes (SIGHUP), or a supervisor sends SIGQUIT.
The default action terminates the process immediately; the agent pattern stays applied forever.
**Reproduction (dry-run):**
```python
# start `luminactl --dry-run mcp`, send lumina_agent_state working, then kill -HUP
# results of the probe: SIGHUP rc=-1 restored=False, SIGQUIT rc=-3 restored=False, SIGTERM rc=0 restored=True
```
**Fix:** install the same flag handler for SIGHUP (and SIGQUIT if a graceful path is wanted). Release builds use
`panic = "abort"`; add a panic hook that does a bounded best-effort restore (or document that a panic leaves the pattern).
**Test:** extend `signals_restore_baseline_and_exit_cleanly` with `-HUP`.

### R4-02 — Medium — One unknown hardware mode locks the whole Web UI offline

**Files:** `web/app.js` (`hardwareData`, `refreshStatus`), `src/gpu.rs` / `src/mobo.rs` status naming.
**Scenario:** another tool leaves a device in a state this project does not model (GPU mode code 5 "Direct" appears in
`REA_ANALYSIS.md`; the mobo can report codes above 13). Status reports `unknown(N)`, `hardwareData` throws, the page shows
`● OFFLINE`, Apply is disabled, and the user cannot even take control from the UI.
**Reproduction (intercepted browser routes):** status with `gpu.mode = "unknown(5)"` or `motherboard.mode = "unknown(14)"` gives
`live='● OFFLINE' help='Unsupported hardware effect reported…' apply_disabled=True`. A known `chase` mode stays LIVE.
**Fix:** only identity/LED-count failures should lock the UI. Show an "Unmanaged mode (N)" state, keep Apply enabled so a
selection replaces it, and keep the staged-edit logic. Add the GPU `5` direct mode to the enum/status naming if it is real.
**Test:** unknown-mode status keeps Apply enabled and a subsequent apply works.

### R4-03 — Medium — No `Host` validation: DNS rebinding reads hardware state

**Files:** `web/serve.py` (`do_GET`, `LuminaRequestHandler`).
**Scenario:** a web page on `evil.example` rebinds its name to `127.0.0.1`. The browser treats the daemon as same-origin, so it can
read `/api/status` (hardware identity, firmware, colors) and the static UI. POSTs are blocked by the `Origin` check, so this is
information disclosure only.
**Reproduction (loopback, mocked subprocess):** `GET /api/status` with `Host: evil.example:7890` and `Origin: http://evil.example:7890`
returns `200 application/json` with the status body.
**Fix:** reject requests whose `Host` is not in `{127.0.0.1:PORT, localhost:PORT, [::1]:PORT}` with 421/403 before routing, for GET and POST.
**Test:** socket test with a foreign `Host` header gets 4xx and executes no subprocess.

### R4-04 — Medium — Sync All speed is silently dropped

**Files:** `src/main.rs` (`Command::All` has no `--speed`), `web/serve.py` (forwards `--speed` only for `target == "mobo"`),
`web/app.js` (sends `speed` and keeps the slider enabled in sync mode), `tests/test_ui.py` (asserts `speed: 4` on an `all` write).
**Reproduction:** `luminactl --dry-run all --mode rainbow --speed 3` → `error: unexpected argument '--speed' found`. In the UI, link both
devices, drag speed to 4, Apply: the command is `sudo luminactl all --mode rainbow`; speed is never applied.
**Fix:** add `--speed 0..=4` to `all` (motherboard only, like the MCP tool) and forward it from the daemon for `all`; show it in the preview
when relevant.
**Test:** dry-run register assertion for `all --mode wave --speed 3` (`0x8022 <= 0x03`); UI preview contains `--speed`.

### R4-05 — Medium — Python reference diverges from the Rust CLI

**Files:** `lumina_ctl.py`, `tests/test_lumina.py`.
**Evidence (dry-run):**
- `all --off` and `all --mode rainbow` fail: `the following arguments are required: --color` (argparse `required=True`).
- `all --color 112233 --mode bogus` is accepted and silently becomes static (`dict.get` fallback).
- `gpu --mode rainbow` is rejected; `mobo --color … --mode breathing` ignores the color.
- `set_mode` never clears direct mode (the Rust RT2-07 fix), so a mode change after a static color may not show.
- `get_status` always reads the direct buffer (the Rust RT2-09 fix reports the active buffer), and has no per-zone/effect-color/snapshot support.
**Fix:** either bring the Python tool to parity for the shipped surface (optional `--color` except static, mode validation, direct-mode
clear, active-buffer status) or mark it reference-only/deprecated in the README and fail closed for the diverging commands.
**Test:** port the Rust `cli_dry_run` register-sequence assertions.

### R4-06 — Medium — Mode vocabulary is inconsistent across surfaces

**Files:** `src/main.rs` (`parse_gpu_mode`, `parse_mobo_mode`, `parse_all_mode`), `src/mcp.rs` (`EFFECTS`), `web/app.js` (`modes`).
**Reproduction:** `mobo --mode comet`, `mobo --mode starry-night`, `mobo --mode glowing-yoyo`, `mobo --mode color-cycle`, `gpu --mode strobing` all fail
with `invalid variant`, while `all --mode comet` works and the MCP/UI use those names. The UI only works because it sends native names.
**Support matrix found:**

| Mode | CLI `mobo` | CLI `gpu` | CLI `all` | MCP | UI selectable |
| --- | --- | --- | --- | --- | --- |
| friendly aliases (comet, starry-night, glowing-yoyo, color-cycle, strobing) | no | no (`rainbow`,`color-cycle` only) | yes | yes | yes |
| `chase`, `spectrum-chase`, `rainbow-breathing` | yes | no | yes | yes | no (read back as aliases) |
| GPU Direct (code 5) | n/a | no | no | no | no (R4-02) |

**Fix:** drive every subcommand from one table (the `EFFECTS` table plus native names) so any name valid anywhere is valid everywhere,
and list the UI's missing modes or document them as CLI-only.
**Test:** table-driven test that each name parses for each subcommand it applies to.

### R4-07 — Low — JSON-RPC batches are rejected although 2025-03-26 is advertised

**Files:** `src/mcp.rs` (`handle_line`, `SUPPORTED_VERSIONS`).
**Reproduction:** `initialize` with `2025-03-26`, then `[{"jsonrpc":"2.0","id":2,"method":"ping"},…]` →
`-32600 Expected a single JSON-RPC request object (batches are not supported)`. Batching was required in 2025-03-26 and removed in 2025-06-18.
**Fix:** support batches (reply with an array, drop notifications) or negotiate only 2025-06-18 and above.

### R4-08 — Low — `"id": null` is treated as a valid request

**Files:** `src/mcp.rs` (`handle_line`). `{"id":null,"method":"ping"}` returns `{"id":null,"result":{}}`. MCP forbids null ids; a reply with a null id
is indistinguishable from a parse-error reply. **Fix:** reject with -32600.

### R4-09 — Low — Non-JSON error bodies show raw parser errors in the UI

**Files:** `web/app.js` (`api`: `response.json()` runs before the status check).
**Reproduction:** status route answering `503` with an empty body (the daemon's connection-cap response) shows
`Failed to execute 'json' on 'Response': Unexpected end of JSON input`; an HTML 501 shows `Unexpected token '<'…`.
**Fix:** check `response.ok`/content type first and map 503 to "Daemon is busy, retry", others to the status text.

### R4-10 — Low — Route matching is exact

**Files:** `web/serve.py`. `GET /api/status?x=1`, `GET /api/status/` and `HEAD /api/status` return 404; `PUT`/`DELETE` return Python's HTML 501.
Harmless to the UI, but proxies and monitors fail. **Fix:** route on `urlsplit(path).path`, answer unsupported methods with a JSON 405 and `Allow`.

### R4-11 — Low — No browser hardening headers; static files have no cache policy

**Files:** `web/serve.py`. Static responses carry only `Last-Modified`. The UI can be framed by another origin (clickjacking the Apply button) and
there is no CSP or `X-Content-Type-Options`. After an upgrade a heuristically cached `app.js` can talk to a newer daemon API.
**Fix:** add `X-Frame-Options: DENY` / `frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, a CSP of `default-src 'self'`, and
`Cache-Control: no-cache` for static assets (touches the area Codex is changing).

### R4-12 — Low — Default binary path is incompatible with the documented sudoers rule

**Files:** `web/serve.py` (`CLI_BIN`), `README.md`, `SECURITY.md`.
`python3 web/serve.py` without `LUMINA_CLI_BIN` runs `sudo -n <repo>/target/release/luminactl`, which the documented rule
(`/usr/local/bin/luminactl`) does not allow, so Apply fails with a bare sudo error. The only way to "fix" it without reading the docs is a
sudoers rule for a user-writable file (privilege escalation).
**Fix:** default to `/usr/local/bin/luminactl` when it exists; otherwise return an actionable error naming `LUMINA_CLI_BIN` and never suggest the build path.

### R4-13 — Low — Timeout kills `sudo`, not the child (code-confirmed)

**Files:** `web/serve.py` (`subprocess.run(..., timeout=10)`). On timeout `run` kills only `sudo`; SIGKILL is not relayed, so `luminactl` can keep
running and holding the device lock. Not reproduced: needs a real privileged child. **Fix:** start in a new session and signal the process group with
SIGTERM first, or enforce the deadline inside `luminactl` (R4-15).

### R4-14 — Low — Misleading success message

**Files:** `src/main.rs` (`Command::All`). `all --mode off --color 112233` prints
`GPU set to #112233 (Mode: off); motherboard effect off generates its own colors.` Treat mode `off` as `--off`.

### R4-15 — Low — Lock waits stall the MCP main loop (code-confirmed)

**Files:** `src/hardware_lock.rs` (5 s wait per device), `src/mcp.rs`. A tool call, restore or status read waits up to 5 s for each device lock on the
only loop thread, so the hold watchdog and SIGTERM handling are delayed by up to ~10 s while another process holds a lock. Not reproduced:
`--dry-run` never takes the lock and real device nodes were off-limits. **Fix:** pass the shutdown flag into the lock poll and abort the wait on signal; consider a shorter wait inside MCP.

### R4-16 — Low — Missing `rust-version`

**Files:** `Cargo.toml`. `File::try_lock` (used in `src/hardware_lock.rs`) is stable from Rust 1.89; `std::io::pipe` (tests) from 1.87. Older toolchains fail with
an unstable-feature error instead of a clear message. **Fix:** `rust-version = "1.89"` and say so in README/CONTRIBUTING.

### R4-17 — Low (feature gap) — No per-zone or direction control outside MCP

**Files:** `src/main.rs`, `web/app.js`. The driver (`set_zone_color`, `write_mode(..., direction)`) and MCP (`zone`) support them; the CLI and UI do not, although the
UI draws five zone LEDs and the REA mode table lists direction for chase/wave/rainbow. **Fix:** `mobo --zone N --color` and `--direction forward|reverse`, then the UI.

### R4-18 — Informational — Shared SMBus host is unverified

The motherboard path drives the NCT6793D SMBus host through `/dev/port` while the kernel or ACPI firmware may use the same host. `flock` only coordinates
luminactl processes. Not reproducible without hardware; recommend documenting it next to "one tool at a time" and checking the kernel's SMBus ownership.

## Verified clean (no finding)

- **Argument bounds:** `--speed` outside 0..=4 is rejected (`mobo --speed 5`), invalid or short colors are rejected before any device access, MCP `zone` > 4 and
  speed > 4 are rejected, `all`/`mobo`/`gpu` `--off` takes precedence.
- **MCP framing and lifecycle:** malformed JSON, oversize lines, closed stdout, flush failure, backpressure, SIGTERM/SIGINT, EOF with large replies, legacy
  `aura_*`/`aura://` aliases and unknown methods/tools behave as specified; unknown-method and bad-params paths return -32601/-32602.
- **HTTP:** traversal (`/../`, `%2e%2e`) is 404; bad framing, oversized and truncated bodies, stalled clients and the connection cap behave as tested in Round 3;
  `Origin: null` POSTs are 403; JSON responses are `no-store`.
- **Web UI:** no `innerHTML`/`eval`; per-device staging survives refresh and partial apply; polling does not overlap (`refreshing` guard).
- **Locks:** acquisition order is always GPU then motherboard in Rust; Python never holds both.

## Suggested resolution order

1. R4-01, R4-04, R4-06 (small, independent of the files Codex owns).
2. R4-02, R4-09 (UI) together; R4-03, R4-10, R4-11, R4-12, R4-13 together once Codex's serve/cache changes land.
3. R4-05 (decide: port or deprecate), R4-07/R4-08, R4-14, R4-15, R4-16.
4. R4-17 and R4-18 as backlog.
