# Workspace history visual qualification

Source baseline: `dd869a0bbf`.

Offscreen captures only: no live desktop, compositor process or live model was
used. Updated captures and logs are untracked under `target/history-ux-fixed/`.
Scenario execution and actual image inspection are reported separately.

## Reproduction

Build once through the installed Cargo shim and use its resulting binary:

```sh
cargo build --locked -p shell --bin shots
for theme in light dark high-contrast; do
  for scenario in room_activity_catch_up_keyboard \
    room_activity_explore_detail_compare history_visual_qualification \
    room_activity_replay automatic_work_session_correction; do
    out="$PWD/target/history-ux-fixed/$theme/$scenario"
    mkdir -p "$out"/{1600x900,800x600}
    TILO_SHOTS_THEME="$theme" target/debug/shots \
      --mode offscreen --scenario "$scenario" \
      --sweep 1600x900,800x600 --out "$out" > "$out/updated-command.log" 2>&1
  done
done
cargo test --locked -p shell --bin shots \
  history_qualification_preserves_evidence_and_heads_at_both_sizes
cargo check --locked -p shell --tests
cargo clippy --locked -p shell --tests -- -D warnings
```

Use absolute, precreated output directories for atomic PNG publication.

## Fixed matrix results

All five scenarios completed successfully in light, dark and high-contrast,
at **both 1600×900 and 800×600** (30 scenario/viewport runs). Each directory
retains `updated-command.log`. The earlier captures under
`target/history-ux-shots/` are historical evidence, not the updated result.

| Scenario | All three themes, both sizes |
| --- | --- |
| Catch-up keyboard | Completes, including narrow work-session focus |
| Original-row exact comparison | Completes |
| History visual qualification | Completes, including scrolled summary and detail captures |
| Replay | Completes |
| Automatic work-session correction | Completes |

The focused history qualification test passed at both sizes, and
`cargo check --locked -p shell --tests` passed. Logs are
`target/history-shots-test.log` and `target/history-shots-check.log`.
The warning-denying shell test-target Clippy run passed; its log is
`target/history-shots-clippy.log`. The activity-pane tests also passed
(132 tests, `target/history-pane-tests.log`).

## What the qualification scenario proves

- Synthetic wording is admitted by the production narrative validator and
  explicitly labels its generator `shots fixture (not a live model)`.
- Summary focus uses the original citation's stable accessibility key. Paint
  must publish that same citation within the pane after scrolling; narrative
  identity, cited IDs and source document heads must remain unchanged.
- Expanded provenance is activated with accessibility focus and Enter, then
  paged through the shared scroll region. Citation activation also uses the
  canonical focus/Enter event route, not forced timeline selection. The exact
  inclusive evidence span and retained resource provenance are asserted.
- Details and comparison each have a separate PageDown-to-end capture. Selected
  IDs and heads remain unchanged. Comparison uses `history.compare` and must
  actually materialize. Its synthetic provider publishes action labels through
  the shared semantic sentence reader. Bookkeeping patches are excluded from
  the visible preview, while exact patches, source IDs and causal documents
  remain available to machine consumers. Unknown action labels get an explicit
  localized unavailable description, never an inferred routing name.
- Rename invokes the existing `history.groups.edit` action with an exact
  frontier and the existing grouping record. The same record must receive the
  new name, and shared room document heads must remain unchanged. Every
  qualification action also asserts unchanged shared source heads. This is a
  canonical semantic rename, not a hand-written text input.
- Replay seeks both endpoints through the shared accessible slider. Invalid
  seeks fail without adopting a replacement. Switching the fixture's current
  workspace makes the retained pin unavailable; its exact address remains
  pinned, its ordinal becomes absent, playback is stopped and Play is refused.
  The newly opened activity tile paints that missing-pin state.
- Resource identity and contributor filtering retain the financial-report
  context without writing history.

## Actual image inspection

The following updated PNGs were opened as images, not inferred from exit codes.
Paths are relative to the checkout:

- `target/history-ux-fixed/light/history_visual_qualification/800x600/02-validated-fixture-summary.png`
  — generated paragraph and original citation are both visibly reachable in the
  scrolled narrow pane.
- `target/history-ux-fixed/dark/history_visual_qualification/800x600/02b-expanded-summary-provenance.png`
  — paragraph, generator, locale, bounded-excerpt coverage, citation caveat and
  citation are visible together after expanding and paging.
- `target/history-ux-fixed/high-contrast/history_visual_qualification/1600x900/04b-comparison-scrolled-end.png`
  — historical resource names and published action labels are visible without
  receipt, invocation or document-key addresses.
- `target/history-ux-fixed/light/history_visual_qualification/800x600/04b-comparison-scrolled-end.png`
  — wrapped affected-resource text and published action descriptions fit
  separately; the bottom of the detail scroll region is visible.
- `target/history-ux-fixed/dark/history_visual_qualification/800x600/07-missing-retained-replay-pin.png`
  — the activity tile visibly reports the retained unavailable checkpoint and
  shows disabled Play, without painting a replacement checkpoint's metadata.

The matrix proves execution across all six theme/size combinations; these
representative inspections do not claim every updated PNG was individually
opened. Summary and details scroll through production controls rather than
moving their content or forcing selection for the camera.

## Limits

Stills prove paint, not a live model, screen-reader delivery or real-seat input.
CPU-only scenario tests use the production paint pass but do not establish pixel
readability. Semantic rename is qualified; typing into the shared rename editor
is not exercised by this scenario. Wide content that already fits need not move
when PageDown is pressed. Workspace switching in the missing-pin fixture changes
viewer-local workspace state; it does not delete historical evidence.
