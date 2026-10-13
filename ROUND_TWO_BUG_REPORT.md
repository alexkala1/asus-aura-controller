# Round Two Adversarial Bug Report

**Bug hunter:** Codex. **Resolver:** Claude.

**Audited revision:** `22864c4b622d09eeadabed91b8cf41c650ab35d4`.

**Scope:** all Rust hardware drivers, device locking, CLI and MCP lifecycle;
the Python reference implementation; Web Studio daemon/frontend; unit and
integration coverage. This audit changed no implementation or test files and
accessed no real hardware. This report is the only repository addition.

**Results:** 12 findings: **2 High, 9 Medium, 1 Low**. High means a concrete
path to unintended hardware register writes; Medium means incorrect device
state, lost user changes, broken lifecycle guarantees, or service denial;
Low means a test infrastructure failure that weakens future verification.

## Verification and limits

- `cargo test --all`: **56 passed** (41 unit, 9 CLI, 6 MCP stdio).
- `cargo build`: passed; dry-run reproductions used the current debug binary.
- `pytest tests/test_ui.py`: **19 passed** in a temporary copy of web/tests,
  preserving repository screenshots.
- Additional evidence: fake-register Rust controller harness, mocked Python
  device files/ioctl, an isolated HTTP server with subprocesses forbidden,
  browser staging reproduction, and bounded dry-run MCP subprocess probes.
- Register values and software control flow were verified. Physical LED
  behavior and damage were not tested. Findings distinguish those claims.
- Existing green tests miss the scenarios below; some encode the bad behavior.

## RT2-01 — High — Direct color writes overrun motherboard register buffers

**Targets:** `src/mobo.rs:238–240`, `280–292`, `313–326`, `352–397`;
register boundaries in `src/protocol.rs:38–43`.

**Failure scenario:** `led_count()` permits up to sixteen LEDs. The direct
buffer begins at `0x8000`, the effect buffer at `0x8010`, and controls at
`0x8020`. Only five complete RGB triplets fit in either color buffer. Six
reported LEDs cause direct writes to overlap the effect buffer; eleven reach
the direct-enable control. Larger counts reach mode/speed/direction. Zone
writes and snapshot/restore share the same unsafe assumptions. Identity
verification does not prevent a bad count from corrupting adjacent registers.

**Evidence:** A sandbox harness calling actual controller methods with a fake
`PortIo` reported eleven LEDs. `set_color_static(0x11, 0x22, 0x33)` succeeded
but left `0x8020 = 0x22` and effect-buffer `0x8010 = 0x33`. This proves register
overlap, without claiming physically demonstrated hardware damage.

**Claude fix specification:**

1. Define direct/effect capacities from their register boundaries, both five.
2. For the sole supported board, reject nonzero firmware counts above five
   before any ENE register mutation. Retain zero→five only as an explicitly
   documented compatibility policy.
3. Use the validated count for status, static writes, zone validation, and
   snapshots. Validate both snapshot vector lengths before any restore write;
   manually constructed snapshots must not bypass the limit.
4. Replace the unsafe sixteen-LED expectation in
   `src/mobo.rs:498–514`, rather than merely adding another test.

**Required regressions:** counts 6, 11, 16, and 255 fail with no ENE mutations;
zones ≥5 fail; counts 1–5 keep color writes within their own buffers; oversized
restore snapshots fail before mutation.

## RT2-02 — High — Executable Python hardware paths bypass device locking

**Targets:** `aura_ctl.py:105–115`, `230–243`; locking contract in
`src/hardware_lock.rs:6–33`, `src/gpu.rs:60–68`, `src/mobo.rs:34–41`.

**Failure scenario:** Rust operations serialize with an exclusive device
`flock`, but the executable Python reference opens the same device nodes
without locking. Python and Rust CLI/MCP operations can therefore interleave.
For the motherboard, one operation's register-address selection can be
followed by another operation's data transaction, causing unintended writes.
This is a hardware-integrity problem, not a demonstrated privilege escalation.

**Evidence:** Hold `LOCK_EX | LOCK_NB` on a temporary device substitute. With
GPU `ioctl` mocked, Python `open()` still reaches `I2C_SLAVE`; motherboard
`open()` also succeeds. Register corruption is inferred from the shared
transaction framing; the locking bypass itself was demonstrated.

**Claude fix specification:** Add one Python device-lock helper using
`fcntl.flock(fd, LOCK_EX | LOCK_NB)`, a monotonic five-second deadline, and
bounded retry sleeps. Acquire immediately after open and before ioctl/port
access; retain the descriptor/lock until close. Close newly opened descriptors
on every lock or ioctl failure. Dry-run must not open devices or create lock
files. If Python is intentionally reference-only, the acceptable alternative
is to disable its live hardware execution explicitly.

**Required regressions:** cross-process temporary-file contention prevents
Python entry into ioctl/port operations; releasing the Rust-compatible lock
allows progress; lock timeout and ioctl failure close descriptors; dry-run
remains independent of device files.

## RT2-03 — Medium — Broken MCP stdout bypasses baseline restoration

**Targets:** `src/mcp.rs:937–950`; process error exit in `src/main.rs:225–228`.

**Failure scenario:** Establish an agent hold, close the client's stdout read
end, then send another request. The response write/flush error propagates via
`?` before the common restoration block. Main exits and leaves the agent
pattern applied instead of restoring the saved lighting. EOF/signal cleanup
tests do not cover this equally normal client-disconnect path.

**Evidence:** Dry-run working hold, followed by closed stdout and `ping`,
exited with status 1 and `Broken pipe (os error 32)`. Stderr contained neither
the baseline GPU-off write nor `clean shutdown`.

**Claude fix specification:** Make every serve-loop exit, including output
write/flush and input failures, enter one common cleanup path. Preserve the
original transport error, attempt baseline restoration with the existing
bounded retry policy, then return the original error. Cleanup must not depend
on successfully writing another JSON-RPC response or waiting on a blocked
output writer. Surface restoration failure separately on a usable diagnostic
channel; do not turn transport failure into success.

**Required regressions:** hold→close stdout→request causes baseline restore
before nonzero exit; injected flush failure behaves identically; ordinary
EOF/SIGTERM/SIGINT restoration remains intact.

## RT2-04 — Medium — MCP response backpressure stops watchdog and shutdown

**Targets:** `src/mcp.rs:915–940`; signal flag at `840–847`.

**Failure scenario:** The same thread handles output, hold deadlines, and the
shutdown flag. A client that keeps sending requests but stops reading replies
fills the stdout pipe. Blocking `writeln!` prevents hold expiry and SIGTERM
processing indefinitely. A one-second failure/flashing hold can remain active
past its deadline. Bounded input buffering does not bound output blocking.

**Evidence:** Start a one-second dry-run failure hold, drain its first reply,
send 5,000 pings without reading replies, wait 1.8 seconds, then send SIGTERM.
The server remained alive 0.6 seconds later and required SIGKILL. Stderr was
discarded in this probe, isolating stdout backpressure from logging.

**Claude fix specification:** Keep lifecycle/timer control independent of
blocking client output. Use bounded response buffering and a writer whose
blocking cannot stall the owner of hold state, or pollable/nonblocking writes
with explicit deadlines. On sustained output backpressure, terminate the
session through RT2-03's cleanup path. Bound memory and shutdown waits; do not
join a blocked writer indefinitely. Audit synchronous diagnostics as well so
an unread stderr pipe cannot become the replacement lifecycle blocker.

**Required regressions:** saturate stdout during an active hold; watchdog
restoration still executes and SIGTERM exits within a documented bound.
Output saturation must not produce unbounded queues or orphan threads.

## RT2-05 — Medium — Selective/partial manual changes mishandle agent baselines

**Targets:** `src/mcp.rs:673–690`, `776–796`, `815–835`.

**Failure scenario A:** A working hold covers GPU and motherboard. A successful
GPU-only manual color call clears the entire hold. The motherboard remains in
the agent pattern permanently because its baseline is discarded too.

**Failure scenario B:** An all-device manual call succeeds on GPU but fails on
motherboard. Because aggregate `ok` is false, both baselines remain. The timer
later overwrites the successful GPU manual change with its old baseline.

**Evidence:** Scenario A was demonstrated over dry-run stdio: working→GPU-only
color→idle returned `baseline_restored: false`, with no motherboard baseline
mode write. Scenario B is confirmed by the independent per-device result map
and aggregate hold-clearing branch; it needs an injected-device regression.

**Claude fix specification:** Track held baselines per device. A successful
explicit change relinquishes only that device's saved baseline; untouched
devices retain their restoration deadlines. In a partial result, relinquish
only the successfully changed devices, keep failed-device recovery state,
and report the partial outcome. Do not drop unrelated baselines or restore
over successful explicit changes. Track restore progress per device so a
successful restore is not replayed while another device retries.

**Required regressions:** two-device hold→GPU-only override preserves GPU
manual settings and restores motherboard; reverse target order; one-sided
manual failure does not roll back the successful side; one-sided restoration
failure does not repeatedly rewrite a side already restored.

## RT2-06 — Medium — Agent patterns are applied without a recoverable baseline

**Targets:** `src/mcp.rs:708–747`, `776–796`.

**Failure scenario:** A device opens, but snapshot reads transiently fail.
The code records a warning and stores `None` for its baseline while leaving
the controller eligible for `run_plan`. If subsequent writes succeed, an
agent pattern is applied with nothing to restore. `write_baseline()` skips
the missing snapshot and can report restoration success. Chained states also
reuse an old incomplete baseline while newly available devices can be changed
for the first time without being added to it.

**Evidence:** Source-confirmed data flow: snapshot `.ok()` removes the
recovery state, but does not remove the corresponding `Some(controller)`
passed to `run_plan`. No hardware-failure injection was needed to identify
the path; current factories make that missing test difficult to express.

**Claude fix specification:** Before a temporary pattern changes a device,
require a valid saved baseline for that device. Exclude a device whose
snapshot fails and report why; fail the call if no recoverable device remains.
On chained states, snapshot any newly participating device before its first
mutation while preserving existing baselines. Restoration responses must
distinguish devices actually restored from devices never safely enrolled.
Introduce injectable controller factories/transports for these tests.

**Required regressions:** snapshot-read failure followed by otherwise
successful writes produces no mutation on that device; the healthy device
can still participate with explicit warnings; later device recovery captures
a baseline before mutation; empty baselines cannot claim a restored device.

## RT2-07 — Medium — Mode-only transitions leave direct mode enabled

**Targets:** `src/mobo.rs:307–309`; callers `src/main.rs:166–177`,
`205–208`, `215–216`, and `src/mcp.rs:224–225`.

**Failure scenario:** Static/zone writes enable `MOBO_REG_DIRECT = 1`.
Subsequent mode-only rainbow, breathing, or off calls do not clear it, unlike
`set_effect`. The CLI reports the requested mode while leaving a contradictory
controller selection. Physical failure to display the effect is inferred
from direct-enable semantics, rather than measured on hardware.

**Evidence:** A fake-register harness using actual methods performed static
color→rainbow via `set_mode`; the direct flag remained 1.

**Claude fix specification:** Centralize mode-transition policy. Animated
effects and Off must disable direct operation before mode/apply writes.
Specify static-without-color behavior explicitly and preserve its intended
buffer selection. Ensure CLI and MCP mode/off entrypoints use that policy.

**Required regressions:** static→rainbow, zone-static→breathing, and
static→off finish with direct zero; direct-disable precedes apply. Replace
isolated-call expectations that omit this required state transition.

## RT2-08 — Medium — Combined CLI effects do not apply their requested color

**Targets:** `src/main.rs:195–212`; coverage gaps in
`tests/cli_dry_run.rs:112–118`, `152–160`.

**Failure scenario:** `all --color 112233 --mode breathing` sets GPU RGB,
but changes only the motherboard mode registers. Stored effect colors stay
unchanged. The success message nevertheless claims both devices were
synchronized to `112233`. RT2-07's direct flag is a related, separate omission.

**Evidence:** The dry-run command emits GPU RGB writes and motherboard
`0x8021`, `0x8022`, `0x8023`, `0x80a0` writes, followed by the synchronized
color claim. No effect-buffer color writes occur.

**Claude fix specification:** Preserve static handling via
`set_color_static`. For color-driven nonstatic motherboard effects, call
`set_effect(mobo_mode, 0, Some(rgb))` so the effect buffer and selection are
updated. For autonomous spectrum/rainbow effects, describe generated colors
accurately instead of promising the supplied RGB on both devices. Coordinate
this dispatch change with RT2-07's centralized transition policy.

**Required regressions:** complete register sequences for combined breathing
and flashing, including direct zero and effect-buffer RBG bytes; accurate
messages for autonomous colors; transitions from an existing static state.

## RT2-09 — Medium — Motherboard status reports inactive-buffer colors

**Targets:** `src/mobo.rs:243–261`; effect writes at `336–349`.

**Failure scenario:** `set_effect` writes the effect buffer and disables
direct mode; `status` always reads the direct buffer. MCP/API/UI therefore
report the old static color alongside the newly applied effect.

**Evidence:** Actual methods on a fake bus performed static `112233`, then
breathing `aabbcc`. Effect bytes were `aa,cc,bb` (RBG), but status RGB remained
`[17,34,51]` (`112233`).

**Claude fix specification:** Read the direct-enable register with mode and
select the active configured-color buffer: direct when enabled, effect when
disabled. Apply RT2-01's independently validated capacities. Document that
effect RGB means configured color, not an instantaneous animation sample.
If both buffers are exposed, name them explicitly and update clients to use
the active one rather than silently retaining stale `zones` semantics.

**Required regressions:** colored breathing reports its effect color;
effect→static reports direct colors; inactive-buffer differences never leak
into active-color status; RBG conversion remains correct.

## RT2-10 — Medium — One incomplete HTTP request freezes the Web daemon

**Targets:** `web/serve.py:56–62`, `130–135`.

**Failure scenario:** The single-threaded TCP server reads an unchecked
`Content-Length` with no connection timeout. An unauthenticated local client
can retain a partially delivered body indefinitely, blocking status/apply and
static assets for every other client. Negative lengths reach `read(-1)`;
large delivered bodies can exhaust memory. Loopback-only binding limits the
attacker to local access but does not remove the service-denial scenario.

**Evidence:** An isolated actual handler received a POST declaring 99,999,999
bytes but sending only `{`. A second favicon request timed out until the
first connection closed; it then returned 200. Hardware subprocess calls
were forbidden and the count remained zero.

**Claude fix specification:** Reject invalid/negative/conflicting lengths and
unsupported transfer encodings before reading. Limit JSON bodies to 16 KiB
and return 413 above the limit. Set finite header/body read timeouts, detect
premature EOF, and return a controlled error or close the connection. Use a
concurrent server such as `ThreadingHTTPServer` with bounded active handlers
and finite timeouts; threading alone is insufficient. Retain device locking
when API subprocesses overlap.

**Required regressions:** real socket tests with mocked subprocesses cover
oversized, negative, conflicting and truncated lengths, incomplete headers,
and another client making progress while one stalls. The direct `BytesIO`
handler test at `tests/test_ui.py:315–337` cannot establish these guarantees.

## RT2-11 — Medium — Single-device apply discards another device's staged edits

**Targets:** `web/app.js:19–22`, `46–60`, `195–199`, `226–249`.

**Failure scenario:** One global `dirty` flag covers both channels. Applying
one device clears it, after which refresh overwrites both devices from
hardware. An unapplied edit on the other device disappears silently.

**Evidence:** In a browser with synchronization disabled, stage GPU `112233`
and motherboard `abcdef`; apply motherboard. The sole write targets the
motherboard, but the GPU control reverts to hardware `ff0055`.

**Claude fix specification:** Track staged/dirty state per device. Mark the
edited device, or both for synchronized edits. On successful apply, clear
only targeted devices. Refresh each clean device independently and preserve
all dirty-device staging, including native effects and motherboard zone
preview colors. Failed apply retains targeted staging. Define synchronization
transitions explicitly rather than using an unrelated global reset.

**Required regressions:** stage both independently, apply each in both orders,
and prove the remaining staged payload survives and is subsequently sent;
repeat with failed apply and automatic/manual refresh. Existing refresh
coverage at `tests/test_ui.py:237–248` never clears the global flag between
two pending device edits.

## RT2-12 — Low — MCP test client can deadlock instead of reporting failures

**Targets:** `tests/mcp_stdio.rs:15–23`, `40–44`, `61–65`.

**Failure scenario:** The harness pipes stderr without continuously draining
it. Dry-run logs fill that pipe and block the server before the next reply.
Response reads and child shutdown waits have no deadlines, so a longer
session or a protocol regression can hang the test suite indefinitely.

**Evidence:** Recreating the harness pipe arrangement stalled after 272
sequential `aura_turn_off` requests, with 65,280 bytes of unread stderr.
These are environment-specific thresholds, not constants to encode in tests.

**Claude fix specification:** Continuously drain stderr into a bounded
diagnostic buffer. Bound response waits and shutdown; on deadline, kill and
reap the child and include captured diagnostics. Ensure panic/drop paths also
reap children without waiting indefinitely. Preserve logs required by signal
restoration assertions while avoiding undrained pipes.

**Required regressions:** 1,000 write-producing tool calls complete; a
deliberately silent child produces a bounded test failure and is reaped.

## Resolver order and completion criteria

1. Fix RT2-01 and RT2-02 before live hardware testing.
2. Make the test client bounded (RT2-12), then implement lifecycle/output and
   per-device recovery fixes (RT2-03 through RT2-06).
3. Resolve driver transitions, combined effects, and truthful status together
   (RT2-07 through RT2-09), retaining separate regression assertions.
4. Fix HTTP framing/concurrency and staged UI state (RT2-10, RT2-11).
5. Run formatting, strict Clippy including all targets, full Rust tests,
   Python reference tests, and UI tests. Exercise new contention and stalled
   I/O tests without real hardware; attach exact pass/fail evidence.

Rust `hardware_lock.rs` itself showed no demonstrated locking defect: it
acquires before transactions, retries with a deadline, and releases with the
file descriptor. No additional high-confidence GPU identity/SMBus ABI defect,
command injection, XSS, or browser-origin bypass was established. These are
audit limits, not claims of exhaustive safety. Existing authorization for
trusted local processes and unsupported hardware restrictions were not
misclassified as new vulnerabilities.
