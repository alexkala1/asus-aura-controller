# Follow-up audit: AUD-01, AUD-02, AUD-03

## Verdict: APPROVE

Reviewed the current uncommitted `web/app.js` and `tests/test_ui.py` diff.
The three reported failure paths are addressed. No additional blocking
finding was identified in this focused review.

| Finding | Verified fix |
| --- | --- |
| AUD-01 | Static/Off/GPU speed controls are disabled; forced Static speed events are ignored. Explicit board-wide color intent is tracked separately from generic dirtiness before prepending a wide color request. The heterogeneous-zone regression verifies that untouched LEDs retain their colors. |
| AUD-02 | Rendering no longer clears zone staging. Refresh retains queued colors when hardware changes modes and presents a recovery/discard explanation. Applying retained zones explicitly switches to Static; user-selected replacement/discard paths are covered. |
| AUD-03 | Successful zone requests clear their own staging; the `finally` path rerenders preview, hints, selection, and markers after success or failure while retaining the error notification. Retry sends only remaining zone requests. |

The six new regressions cover ignored Static speed edits, explicit wide-color
ordering before zone writes, refresh after an external effect change, explicit
discard, intentional effect replacement, and partial-failure retry/preview.
Assertions check outgoing API payloads and visible state, rather than merely
accepting successful requests.

## Fresh validation

- `cargo clippy --all-targets -- -D warnings`: PASS.
- `cargo test --all -- --test-threads=1`: **98 passed**
  (62 unit, 19 CLI, 17 MCP).
- `PYTHONPATH=. pytest tests/` using the repository virtual environment:
  **114 passed**. Run against an exact temporary copy of current source/tests
  to preserve existing modified screenshots.
- `node --check web/app.js`: PASS.

No implementation/test edits or live hardware access occurred. Approval is
for the reviewed AUD-01–03 remediation, not a claim of exhaustive coverage of
unrelated hardware behavior or future changes.
