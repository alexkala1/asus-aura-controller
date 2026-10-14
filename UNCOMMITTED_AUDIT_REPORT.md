# Audit of Claude's uncommitted zone and alias changes

## Verdict: REVISE

Three reproduced Web Studio state-management defects remain. CLI zone bounds,
register writes, and the reviewed alias mappings pass inspection. No
implementation or existing test edits were made; all browser reproductions
intercepted the API and accessed no live hardware.

## Findings

### AUD-01 — Medium — Static speed changes overwrite unrelated zone colors

**Location:** `web/app.js:82–94`, `203–207`.

A speed edit marks the motherboard generically dirty. `requests()` then
prepends a device-wide Static color command before staged zone requests,
although the user never requested a board-wide color change. Static speed has
no effect in the driver, but the accompanying wide color write affects every
LED.

**Browser/mock reproduction:** Initialize distinct zone colors
`000001,000002,000003,000004,000005`; select zone 1 and stage `abcdef`; adjust
the Static speed slider and Apply. Requests first set the entire board to
`000001`, then set zone 1 to `abcdef`. Final colors are
`000001,abcdef,000001,000001,000001`: untouched zones 2–4 lose their colors.

**Required fix:** Disable or ignore speed edits in Static. Track explicit
board-wide color intent separately from generic motherboard dirtiness.
Prepend a homogenizing color command only when the user intentionally stages
a board-wide color. A non-color edit must not create a wide RGB write.

**Regression:** Distinct initial zone colors → stage one zone → attempt a
Static speed edit → Apply; assert only the selected zone changes and no
unrequested board-wide color payload is sent.

### AUD-02 — Medium — Refresh silently discards pending zone edits

**Location:** `web/app.js:131–134`, `344–351`.

Zone staging does not mark the motherboard's editing mode as pending. A new
hardware status reporting an effect replaces its mode, clears `zoneDirty`,
and rendering also clears pending zones when zone editing becomes unavailable.
An automatic or manual refresh can therefore destroy unapplied user edits.

**Browser/mock reproduction:** Stage zone 1 as `abcdef`; change the mocked
hardware status to autonomous Rainbow; refresh. The queued zone command and
staged indicators disappear. The toast still says that zone 1 is staged and
should be applied, contradicting the lost state.

**Required fix:** Preserve pending zone colors and the intended Static editing
state across refresh. If external mode changes require conflict resolution,
retain the edits and expose a recoverable conflict instead of discarding
them. Remove staging mutations from `render()`. Clear staged zones only after
successful writes or an explicit user replacement/discard action.

**Regression:** Stage a zone, return an externally changed effect mode on
refresh/poll, and verify that the color survives in staging or a recoverable
conflict. No queued edit may silently disappear.

### AUD-03 — Low — Partial failure leaves preview and staged indicators stale

**Location:** `web/app.js:391–415`.

Successful requests remove their zones from `zoneDirty`, but the failure path
does not rerender the command preview and zone indicators. Internal retry
state is correct while the displayed/copied commands are obsolete.

**Browser/mock reproduction:** Stage zones 1 and 2. Return success for zone 1
and failure for zone 2. Retry sends only zone 2, but the displayed/copied
command still lists both zones.

**Required fix:** After processing requests, refresh the preview, selection,
zone hints and staged indicators while preserving the error notification and
control error state. Identify successful writes and remaining pending writes.

**Regression:** First zone succeeds, second fails; preview, Copy, and staged
indicators must show only the remaining zone. Retry must send only that zone.

## Verified bounds, aliases, and API boundary

- `src/main.rs:46–51`: `--zone` is parsed as a bounded integer 0–4, requires
  `--color`, and conflicts with `--off`.
- `src/main.rs:218–231`: non-Static zone combinations and invalid colors fail
  before hardware opens; valid input calls `set_zone_color(zone, r, g, b)`.
- `src/mobo.rs:349–362`: firmware LED count and zone index are checked before
  ENE mutation. All five zone dry-runs produced exactly the expected three
  selected RBG color writes and intentional control writes. Zone 4's color
  bytes end at `0x800e`, below the effect buffer at `0x8010`; no color-buffer
  overwrite of direct/mode registers was found.
- `src/main.rs:67–106`, `src/mcp.rs:72–83`: shared mappings resolve
  `comet→chase_fade(7)`, `strobing→flashing(3)`, and
  `starry-night/flicker→random_flicker(13)`. Case/underscore normalization
  passes CLI/Python probes. Unsupported GPU effects remain intentionally
  rejected by the GPU CLI; MCP's documented fallback remains separate.
- `web/serve.py:206–238`: zone types reject booleans, strings, floats and
  out-of-range integers; zone requests require motherboard target, Static,
  non-Off, and a valid color. The CLI runs as an argument list without shell
  interpolation; no introduced command-injection path was found.
- `web/index.html`: zone selection uses keyboard-operable native buttons;
  unavailable selections are guarded in JavaScript. Existing origin checks,
  bounded JSON framing and loopback-only serving remain intact.
- `web/serve.py:47–57`: assets/JSON/errors receive the no-cache/no-store
  policy and conditional asset responses avoid stale 304s. No new exploitable
  security-header regression was established in this diff.

## Requested validation

All commands ran in the actual repository checkout:

| Command | Result |
| --- | --- |
| `cargo clippy --all-targets -- -D warnings` | PASS, no warnings |
| `cargo test --all -- --test-threads=1` | PASS: 98 tests (62 unit, 19 CLI, 17 MCP) |
| `env PYTHONPATH=. .venv/bin/pytest tests/` | PASS: 108 tests |

The Python suite's generated screenshots were restored from pre-run copies,
preserving the existing modified screenshot bytes. Additional mocked browser
probes reproduced all three findings without editing the test suite. Passing
existing tests does not cover these zone/speed, external-refresh, and partial
failure scenarios.
