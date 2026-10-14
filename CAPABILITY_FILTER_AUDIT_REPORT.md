# Hardware-capability filtering audit

Verdict: **REVISE**. Two Medium findings and one Low finding remain despite passing checks.

Scope: uncommitted changes in `src/gpu.rs`, `src/mobo.rs`, `web/serve.py`, `web/app.js`, and their regression tests. Audit performed 2026-10-14. Implementation and existing tests were not modified. All additional reproductions used mocked browser/API traffic or `--dry-run`; no live hardware was accessed.

## CAP-01 — Medium: Sync All copies a device-specific native alias into the shared effect

**Target:** `web/app.js:350–355`, `383–387`, `478–489`; request construction at `258–269`.

**Failure:** GPU status `spectrum` now displays as Rainbow. Enabling Sync All checks the displayed key against the intersection, but copies the GPU's preserved native `spectrum` to both channels. Both pickers show Rainbow while Apply sends `all --mode spectrum`. The motherboard runs Spectrum Cycle, mode 4, rather than Rainbow, mode 5. This is introduced by the new GPU display mapping. Similar preserved native aliases must not silently override the shared picker selection.

**Reproduction evidence:** Chromium loaded the existing `tests/test_ui.py` mocked daemon fixture with GPU status `mode="spectrum"`. Initial GPU picker: `rainbow`. After checking Sync All: GPU and motherboard pickers both `rainbow`; preview: `sudo luminactl all --mode spectrum`. Apply emitted:

```json
{"target":"all","mode":"spectrum","speed":0,"off":false}
```

`target/debug/luminactl --dry-run all --mode spectrum` confirmed `GPU Reg 0x07 <= 0x04` and `Mobo Reg 0x8021 <= 0x04`. The REA mode table assigns motherboard Rainbow to 5.

**Claude fix specification:** When enabling Sync All, stage the selected common Studio effect and clear each channel's device-specific `nativeMode` override. Resolve the shared effect through the existing Studio-to-CLI mapping; Rainbow must send `rainbow` so dispatch selects GPU 4 and motherboard 5. Preserve native readback aliases only for unlinked operations. Add regression coverage starting from GPU Spectrum readback, enabling Sync All without changing the picker, and asserting a Rainbow payload plus motherboard Rainbow readback. Also cover motherboard Rainbow Breathing displayed as Breathing before linking, ensuring the staged shared effect matches the displayed selection.

## CAP-02 — Medium: Refresh invalidates the intersection without invalidating Apply

**Target:** `web/app.js:153–171`, `375–398`, `478–489`; request construction at `258–269`.

**Failure:** Intersection enforcement only repairs state when Sync All is toggled. After a linked apply clears dirty flags, manual/periodic refresh can read a motherboard-only effect changed by another client. The intersection removes that option but `channel.mode` and its native override remain unchanged. The picker becomes blank, and focusing that card makes Apply send the forbidden effect to `all`; the daemon rejects it. The capability-filtered UI therefore still constructs commands outside its intersection.

**Reproduction evidence:** In the existing mocked Chromium fixture: link Static, apply successfully, replace motherboard status with `mode="wave", color_source="autonomous", color_configurable=false`, refresh, and focus the motherboard picker. Observed:

```text
picker value: ''
options: static, breathing, strobing, rainbow, off
preview: sudo luminactl all --mode wave
Apply payload: {target: all, mode: wave, speed: 0, off: false}
page errors: []
```

The new API validator rejects `all/wave` because the GPU cannot run it.

**Claude fix specification:** Reconcile linked state after every successful status/capability refresh. If clean device readbacks cannot represent the same common Studio effect, turn Sync All off, retain each device's actual state, and notify the user that the devices differ. Do not silently replace hardware state or stage a fallback on refresh. Add an independent guard before constructing/sending linked requests so an effect outside the current intersection cannot reach `/api/apply`. Add manual and timer refresh regressions after a successful linked apply, including external motherboard Wave and a capability-list change that excludes a staged selection; require a valid visible picker and no forbidden linked request.

## CAP-03 — Low: API alias validation accepts a spelling the GPU CLI rejects

**Target:** `web/serve.py:39–53`, `256–261`, `275–278`; CLI contract in `src/main.rs:67–75`.

**Failure:** The common alias table maps `rainbow-breathing` to Rainbow for GPU validation, but execution forwards the original spelling. GPU-only CLI parsing rejects that spelling. The supposedly validated request invokes sudo/subprocess and returns a raw CLI error instead of the capability rejection. The existing HTTP tests mock subprocess success, so they cannot detect this contract mismatch.

**Reproduction evidence:** `device_mode("gpu", "rainbow-breathing")` returned `"rainbow"`. A mocked handler received `{"target":"gpu","mode":"rainbow-breathing"}`; subprocess was intercepted and replaced with the current debug CLI using `--dry-run`. It reached execution with `gpu --mode rainbow-breathing`, exited 2, and the handler returned HTTP 400 with `invalid value 'rainbow-breathing' ... is not a GPU effect`. No sudo or hardware operation was executed by the harness.

**Claude fix specification:** Validate aliases against the actual target dispatch contract. Reject GPU-only `rainbow-breathing` before subprocess, or deliberately normalize it to the supported GPU command `spectrum` and document that alias. Keep motherboard native `rainbow-breathing`, `chase`, and `spectrum-chase` semantics intact; blanket canonicalization would collapse distinct motherboard mode IDs. Preserve valid `all` mappings separately. Add API-to-real-dry-run-CLI contract tests for accepted aliases on GPU, motherboard, and all targets, rather than assuming every accepted subprocess call succeeds.

## Verified behavior and evidence limits

- GPU capability serialization exposes Static, Breathing, Strobing, Rainbow (GPU Spectrum alias), and Off. This matches `REA_ANALYSIS.md:155–169`, including the explicit firmware-verification caveat for Off. Impossible GPU Wave, Comet, and Starry Night are excluded.
- Motherboard serialization exposes the eleven existing Studio effects. Their native mappings are consistent with the documented mode table at `REA_ANALYSIS.md:88–104`. This is a Studio vocabulary subset, not an enumeration of all fourteen native motherboard IDs; modes 6, 9, and 10 remain native CLI effects.
- Both status structs derive serialization and populate their lists. The dry-run status regression verifies the emitted metadata. `/api/status` supplies matching fallback lists for legacy binaries.
- The ordinary per-card picker population and initial Sync All intersection/fallback tests pass. Hardware register-writing code is unchanged by this diff.
- These checks establish software routing consistency; they do not independently verify emitted motherboard animations. The REA report explicitly limits vendor-sequence evidence at lines 134–153.

## Verification

| Check | Result |
|---|---|
| `cargo clippy --all-targets -- -D warnings` | PASS |
| `cargo test --all -- --test-threads=1` | PASS: 99 tests (62 unit, 20 CLI, 17 MCP) |
| `env PYTHONPATH=. .venv/bin/pytest tests/` | PASS: 115 tests, 41.70 seconds |
| Additional Chromium/API adversarial harnesses | Three findings reproduced |

Python checks used an exact temporary copy of `web/`, `tests/`, and `lumina_ctl.py` with the repository's existing virtual environment, preserving tracked screenshot artifacts. No implementation or existing test files were edited.
