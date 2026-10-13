# Round Three Adversarial Bug Report

**Bug hunter:** Codex. **Resolver:** Claude. **Date:** 2026-10-14.

**Audited range:** `git diff 22864c4..90a61be`:
`1db517f` (Round Two fixes), `90a61be` (luminactl rename/compatibility).
Full audited HEAD: `90a61bebecc514f4ec5531263c9e09fbfec425f3`.

**Scope:** newly introduced MCP output/diagnostics/lifecycle, HTTP concurrency
and framing, legacy MCP aliases, per-device UI staging, motherboard register
bounds/snapshot validation, and Python/Rust device-lock interoperability.
No unrelated inherited findings are included.

**Summary:** **4 new findings: 0 High, 3 Medium, 1 Low.** Implementation and
existing tests were not modified. No live hardware devices were accessed.
All reproductions used `--dry-run`, temporary regular files with mocked
ioctl, intercepted browser routes, or isolated loopback servers with mocked
subprocesses. The two preexisting modified screenshot files were preserved.

## R3-01 — Medium — Detached MCP writer loses accepted replies on normal EOF

**Target:** `src/mcp.rs:1087–1107`, `1152–1155`, `1201–1210`.

**Introduced surface:** the new bounded response queue/background writer
accepts replies asynchronously, but has no drain/completion handshake or
retained join handle. `diag::flush` at shutdown flushes stderr diagnostics;
it does not flush or await MCP stdout.

**Failure scenario:** A client sends a valid request, closes only stdin, and
continues reading stdout. The main loop accepts and queues the reply, then
observes input EOF. After hardware cleanup it returns success while the
detached writer still owns queued or partially written output. Process exit
terminates that writer, leaving a missing or truncated JSON-RPC reply even
though the client is reading normally. A late writer error also cannot
reliably affect the returned success status.

**Concrete dry-run evidence:** Against the newly built
`target/debug/luminactl`, use a legal string request ID smaller than the
one-MiB inbound message limit:

```python
request = json.dumps({"jsonrpc": "2.0", "id": "x" * 524288, "method": "ping"}) + "\n"
result = subprocess.run(
    ["target/debug/luminactl", "--dry-run", "mcp"],
    input=request, capture_output=True, text=True, timeout=3,
)
json.loads(result.stdout)  # fails despite result.returncode == 0
```

`subprocess.run` closes input after sending and continuously drains output;
this is not an unread-output/backpressure probe. In **10/10 runs**, exit code
was **0**, but the response was incomplete: stdout contained between **0 and
163,840 bytes**, insufficient to hold the 524,288-byte ID. At a 65,536-byte
ID, **2/10** runs also truncated the JSON. Thirty runs each with 1, 10, and
40 small pings completed, explaining why current short-session tests miss
the race. Exact byte counts are scheduling-dependent.

**Claude fix specification:**

1. Give `Output` a retained worker completion signal/handle and an explicit
   bounded `finish` operation. Close the response sender before waiting so
   the worker can drain its queue and terminate.
2. Keep baseline restoration before output draining; an unread stdout must
   never delay the hardware cleanup or signal handling fixed in Round Two.
3. On normal EOF, wait for accepted replies to flush within a documented
   finite deadline. Return an output error on timeout or worker failure;
   do not report clean success with incomplete replies.
4. Observe worker errors after draining, including failures occurring after
   the main loop has stopped. Join only after confirmed completion; do not
   introduce an unconditional join that can block on an unread pipe.
5. Keep queue capacity and shutdown memory usage bounded. `diag::flush`
   remains separate from stdout completion.

**Required regressions:** one large valid-ID ping followed by stdin EOF must
return the entire matching JSON-RPC reply/newline or an explicit bounded
transport failure; multiple queued replies drain in order. Inject a slow
writable sink and a late write/flush failure. Retain tests proving saturated
stdout still permits baseline restoration and bounded SIGTERM/SIGINT exit.

## R3-02 — Medium — Repeated Python GPU open orphans an exclusive device lock

**Target:** `lumina_ctl.py:129–149`, especially `136–141`.

**Introduced surface:** the preexisting repeated-open descriptor ownership
problem now retains the newly added exclusive `flock`, converting it into
persistent device denial for both Python and Rust.

**Failure scenario:** Call `gpu.open()`, then call `gpu.open()` again, or a
public method such as `get_status()`/`set_color()` that opens internally.
The second `os.open` overwrites `self.fd`. Its distinct file description
then attempts to acquire the lock held by the now-orphaned first descriptor.
After the five-second timeout, cleanup closes only the second descriptor and
sets `self.fd = None`. `close()` can no longer release the first lock. Other
processes keep timing out until the original Python process exits.

**Concrete mock evidence:** Import the actual module, use a temporary regular
file as the device, mock GPU `fcntl.ioctl`, and shorten the contention timeout
for the probe. First open succeeds; second open raises `TimeoutError`.
After failure, `self.fd is None`, but `os.fstat(first_fd)` still succeeds.
An independent file opener cannot acquire `LOCK_EX | LOCK_NB` even after
`controller.close()`. No real device file or ioctl was used.

**Claude fix specification:** Make GPU `open()` idempotent: return before
allocating a descriptor when `self.fd is not None`. Hold a newly allocated
descriptor in a local variable, acquire its lock and configure the slave
address inside a cleanup-protected block, and assign `self.fd` only after
success. Close the local descriptor on every failure; never lose an existing
owned descriptor. Apply consistent idempotency/ownership semantics to the
motherboard open method as well.

**Required regressions:** `open(); open()` owns one descriptor and performs
one slave-selection ioctl; `open(); get_status()` does not self-timeout;
`close()` enables an independent process to acquire the tempfile lock.
Preserve lock-timeout/ioctl-failure descriptor cleanup and dry-run behavior.

## R3-03 — Medium — Buffered HTTP reads bypass the absolute body deadline

**Target:** `web/serve.py:79–93`; budgets at `28–29`.

**Introduced surface:** the new absolute body deadline is checked only before
each buffered `read`, while the connection has a separate, longer hard cap.

**Failure scenario:** `self.rfile.read(min(remaining, 4096))` may perform
multiple underlying socket reads internally. A client can deliver bytes often
enough to avoid the socket inactivity timeout while taking longer than the
absolute body budget. After the final buffered read completes, there is no
deadline check before JSON parsing and command execution. The production
30-second connection timer still provides a cap, but the promised 10-second
whole-body limit is not enforced. A valid apply can therefore execute after
the frontend's 12-second request timeout has already reported failure.

**Concrete socket/mock evidence:** Start the actual handler on an isolated
loopback port. Set body budget/socket inactivity timeout to **0.1 seconds**
and connection deadline to **2 seconds**. Send a valid roughly 54-byte apply
JSON body one byte every **0.01 seconds**. The response was **200 after
0.546 seconds**, with **one mocked CLI execution**, over five times the body
budget. No real subprocess or hardware operation ran.

**Claude fix specification:** Read with `read1()` or an equivalent primitive
that returns after one underlying socket read. Before every read, compute
the remaining absolute body budget and apply it as the socket timeout.
Recheck the deadline after every read, including the final chunk, and before
JSON validation/command dispatch. On expiry, return 408 or controlled
connection closure with zero command execution. Restore the response-write
timeout afterward. Retain the 16-KiB limit and existing framing rejection.

**Required regressions:** valid-body trickling where the inter-byte interval
is below the inactivity timeout but total duration exceeds the body budget
must fail within a small deadline tolerance and execute no command. Cover
short final-chunk and multi-chunk bodies. Existing tests at
`tests/test_serve.py:140–148` exercise complete stalls, not continued slow
progress inside a buffered read.

## R3-04 — Low — Worker-start failure permanently consumes HTTP capacity

**Target:** `web/serve.py:216–231`, particularly dispatch at `225`.

**Introduced surface:** the new semaphore admission limit assumes every
successful acquisition creates a worker that eventually releases the slot.

**Failure scenario:** `process_request` acquires a slot, then
`super().process_request` calls `threading.Thread.start`. If thread creation
raises under temporary resource pressure, there is no worker to execute the
release in `process_request_thread`'s `finally`. Capacity remains reduced
after the pressure ends. Repeated failures eventually cause permanent 503
responses until restart. The normal server error path closes the failed
socket but does not release this custom semaphore.

**Concrete fault-injection evidence:** Use an isolated server with
`max_active=1`; make `Thread.start()` raise
`RuntimeError("can't start new thread")` once. Close the failed request socket
through the normal error path and restore ordinary thread startup. The next
connection receives **503**, and a nonblocking semaphore acquisition still
returns **False**. No hardware subprocesses are involved.

**Claude fix specification:** Wrap the delegation to
`super().process_request` in an exception handler that releases the acquired
slot on worker-creation failure, then re-raises so the existing server error
path closes the connection. Keep the worker-finally release for successful
dispatch. Guarantee exactly one release per acquisition, including failed
startup; do not mask the original exception or double-release normal workers.

**Required regressions:** inject one and then several worker-start failures;
after normal creation resumes, all original capacity is available and valid
requests succeed. Separately verify handler exceptions still release once.
Existing capacity tests at `tests/test_serve.py:151–163` cover occupied
workers, not failure to start them.

## Verified surfaces and existing coverage

- **Rust baseline:** `cargo test --all` passes **76 tests**: 53 unit, 11 CLI,
  12 MCP stdio. `cargo build` passes.
- **Python/browser baseline:** `pytest tests/test_ui.py tests/test_serve.py
  tests/test_lumina.py -q` passes **52 tests** in a temporary copy, preserving
  the preexisting dirty screenshots. All 17 existing HTTP socket tests pass.
- **MCP restoration/backpressure:** current tests cover closed stdout,
  write/flush failure, saturated output, unread replies, watchdog expiry,
  SIGTERM/SIGINT, and repeated sessions. The bounded output channel and
  common cleanup substantially address Round Two; R3-01 concerns normal EOF
  output completion, not the previously fixed early-return restoration bug.
- **Diagnostics:** `src/diag.rs` uses nonblocking queue admission and a
  bounded 500-ms shutdown flush in its actual initialized MCP call path.
  No separate high-confidence introduced diagnostic-blocking defect was
  established. Bounded/drop-on-full diagnostics are not claimed lossless.
- **Legacy compatibility:** actual dry-run stdio probes exercise all five
  `aura_*` tools, all five canonical `lumina_*` tools, and both resource URIs
  under both schemes: **14 requests, 14 replies, zero RPC/tool errors**.
  Legacy resource responses preserve the requested `aura://` URI. Reviewed
  routes: `src/mcp.rs:610–639`, `654–684`.
- **Per-device staging:** intercepted Chromium requests confirm applying
  motherboard preserves staged GPU settings; manual refresh preserves them;
  the next Apply targets GPU. A delayed refresh arriving after a GPU edit
  preserves that edit while updating the clean motherboard. Sync→unsync→
  refresh preserves both staged values, with successive single-device Apply
  targets `gpu`, then `mobo`. No console/page errors were observed. Reviewed
  paths: `web/app.js:49–60`, `195–204`, `231–263`, `276–283`.
- **Motherboard bounds:** actual register mocks reject counts 6, 11, 16,
  and 255 before ENE mutation. Both oversized snapshot vectors are rejected
  before color writes. Valid direct/effect writes stay within five-LED
  capacities; no new buffer-overlap defect was found.
- **Cross-process locks:** a standalone Rust harness imports the actual
  `hardware_lock.rs` and locks only a tempfile. Python cannot acquire a
  Rust-held lock; Rust times out after **5.00 seconds** against a Python-held
  lock. Release permits later acquisition. Normal timeout/ioctl cleanup
  tests pass; R3-02 isolates the untested repeated-open ownership case.

## Claude resolver order

1. Fix R3-01 without reintroducing blocking cleanup; add delayed-writer EOF
   and late-failure regressions alongside the existing saturation tests.
2. Fix R3-02 descriptor ownership; run cross-process contention and repeated
   open/public-method tests on temporary files only.
3. Fix R3-03 absolute deadline enforcement and R3-04 admission unwind.
4. Run strict Clippy, formatting, the full Rust/Python/browser suites, and
   the new adversarial probes. Verify that implementation changes remain
   within this audited surface and that no live hardware is required.

This report distinguishes demonstrated software failures from physical
hardware behavior. Passing existing tests and clean scoped probes are not
claims of exhaustive safety outside `22864c4..90a61be`.
