# Capability filtering follow-up audit

Verdict: **REVISE**. CAP-02 and CAP-03 are addressed; CAP-01 remains reproducible after a successful Apply and status refresh.

Reviewed the updated uncommitted diff in `web/app.js`, `web/serve.py`, and regression tests on 2026-10-14. No implementation or existing tests were edited. Browser traffic was mocked, and CLI contract tests used `--dry-run`; no live hardware was accessed.

## CAP-01 — Medium — Still open: refresh restores the device-specific alias

**Target:** `web/app.js:271–282` (`selection`), `395–400` (status refresh), `496–504` (Sync All toggle).

The toggle now clears native overrides, so the first linked Rainbow request is correct. However, every successful Apply invokes `refreshStatus()`, which restores GPU `nativeMode="spectrum"` from hardware readback. Both displayed modes remain Rainbow, so `reconcileLinkedState()` keeps Sync All enabled and `effectAllowed()` passes. With the GPU active, the next linked request uses its native Spectrum alias again. It changes the motherboard from Rainbow (mode 5) to Spectrum Cycle (mode 4).

**Concrete reproduction:** Run Chromium against the existing mocked `tests/test_ui.py` daemon fixture, initialize GPU status as Spectrum, then:

1. Load Studio; leave the GPU active.
2. Enable Sync All and Apply.
3. Wait for both `busy` and `refreshing` to become false.
4. Observe both pickers still show Rainbow, while the command preview is `sudo luminactl all --mode spectrum`.
5. Apply again without changing a control.

Observed output:

```text
first request:  {target: all, mode: rainbow,  speed: 0, off: false}
Sync All after first refresh: true
pickers after first refresh: rainbow / rainbow
preview after first refresh: sudo luminactl all --mode spectrum
second request: {target: all, mode: spectrum, speed: 0, off: false}
motherboard status after second request: spectrum
Sync All after second refresh: false
```

**Required fix:** In linked request construction, always derive the CLI mode from the selected common Studio key, ignoring device-specific `nativeMode`. For example, choose the native mapping from `modes` when `sync.checked`, and use `nativeMode(active)` only for unlinked commands. Use that same resolved mode for payload, color eligibility, and preview. Readback overrides can remain available for unlinked operations. Clearing overrides only when the checkbox changes is insufficient.

**Required regression:** Extend the GPU Spectrum → linked Rainbow test to wait for the automatic refresh after Apply, assert the preview remains Rainbow, Apply a second time, and assert both payloads use `rainbow`, motherboard remains Rainbow, and Sync All stays enabled. Also exercise manual refresh before the second Apply.

## CAP-02 — Addressed

The refresh path now reconciles linked state after updating capabilities and clean device readbacks. An external motherboard Wave unlinks the devices, restores a visible Wave selection on the motherboard, and emits no automatic write. The Apply guard rejects an effect outside the active intersection before issuing a request. Both new regressions passed. This does not repair CAP-01 because its displayed Rainbow key is inside the intersection.

## CAP-03 — Addressed

GPU alias validation now uses an explicit CLI spelling map. GPU-only and linked `rainbow-breathing` are rejected before subprocess execution; motherboard native aliases retain their distinct CLI semantics. The new HTTP-to-real-CLI dry-run test verifies accepted spellings on all three targets and confirms rejected spellings never launch a subprocess. That test executed and passed; it was not skipped.

## Verification

| Check | Result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | PASS |
| `cargo test --all -- --test-threads=1` | PASS: 99 tests (62 unit, 20 CLI, 17 MCP) |
| `env PYTHONPATH=. .venv/bin/pytest tests/` | PASS: 120 tests, 44.62 seconds; no skips |
| `git diff --check` | PASS |
| Additional mocked Chromium repeated-Apply reproduction | CAP-01 still fails |

Python checks ran against exact temporary copies of the relevant source and tests using the repository virtual environment. The temporary debug-binary path linked to the current repository binary so the real-CLI contract test ran, while tracked screenshot artifacts remained untouched.
