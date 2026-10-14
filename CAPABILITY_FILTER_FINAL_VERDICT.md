# Final capability filtering audit verdict

**APPROVE** — CAP-01, CAP-02, and CAP-03 are resolved within the reviewed scope. This verdict supersedes the earlier capability audit verdicts.

Verified the final CAP-01 change in `web/app.js:271–284`: linked requests resolve their CLI mode from the canonical Studio table on every request. Device-specific native readback overrides remain confined to unlinked requests. The resolved shared mode also determines color eligibility and the command preview.

Re-ran the independent Chromium reproduction harness against mocked daemon traffic:

1. Begin with GPU Spectrum readback and enable Sync All.
2. Apply Rainbow and wait for the automatic status refresh to finish.
3. Confirm both pickers and command preview remain Rainbow, with Sync All enabled.
4. Manually refresh and confirm the preview still uses Rainbow.
5. Apply again and wait for the second automatic refresh.

Both requests were `{target: all, mode: rainbow, speed: 0, off: false}`. Motherboard readback remained Rainbow; Sync All remained enabled; no page errors occurred. The harness exited successfully.

The repeated-Apply, Rainbow Breathing linking, external-effect refresh, and intersection guard regression tests all passed: **4 passed, 52 deselected in 3.40 seconds**. `git diff --check` also passed. The preceding audit verified CAP-02 and CAP-03 with passing clippy, 99 Rust tests, and 120 Python tests; those full-suite results were not rerun for this focused final check.

No implementation or existing tests were edited, and no live hardware was accessed.
