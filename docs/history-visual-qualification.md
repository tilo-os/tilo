# Workspace history visual qualification

Baseline: `a0182cc636`. Offscreen captures only; no display server, live desktop
capture, or live language-model inference was used. PNGs and full command logs
remain untracked under `target/history-ux-shots/`.

## Commands and results

All compilation used the installed Cargo shim and its remote-only executor.

```sh
TILO_SHOTS_THEME=light cargo run --locked -p shell --bin shots -- \
  --mode offscreen --scenario room_activity_explore_detail_compare \
  --sweep 1600x900,800x600 --out "$PWD/target/history-ux-shots/light"
```

This produced four PNGs. An earlier invocation exceeded its 200-second timeout
while remotely compiling shell. The subsequent relative-output-path invocation
failed in `offscreen.rs`'s atomic PNG publication with `ProvisionError` / ENOENT;
precreating directories and using an absolute output path succeeded. Mesa's EGL
warnings did not prevent the successful offscreen renders.

The following matrix ran, with each command's output retained in
`$out/command.log` (the light comparison log is `light-command.log`):

```sh
for theme in light dark high-contrast; do
  for scenario in room_activity_catch_up_keyboard \
    room_activity_explore_detail_compare room_activity_replay \
    automatic_work_session_correction; do
    out="$PWD/target/history-ux-shots/$theme/$scenario"
    mkdir -p "$out"/{1600x900,800x600}
    TILO_SHOTS_THEME="$theme" cargo run --locked -p shell --bin shots -- \
      --mode offscreen --scenario "$scenario" --sweep 1600x900,800x600 --out "$out"
  done
 done
```

The light comparison was reused from the first successful command rather than
repeated in the matrix. Replay and automatic-session correction each succeeded
at both sizes in all three themes. Catch-up produced four wide frames and two
small frames per theme, then **failed** with:

> scenario: the settled recap must render a focusable work session

The narrow frame after reordering/resizing has a roughly 320-pixel-wide activity
tile. Tasks, sources and resume content push the work session below the viewport;
only “More work sessions…” remains at the bottom. This is not a passing keyboard
qualification, and the assertion was not disabled.

The new scenario ran successfully at both sizes in all three themes:

```sh
for theme in light dark high-contrast; do
  out="$PWD/target/history-ux-shots/$theme/history_visual_qualification"
  mkdir -p "$out"/{1600x900,800x600}
  TILO_SHOTS_THEME="$theme" cargo run --locked -p shell --bin shots -- \
    --mode offscreen --scenario history_visual_qualification \
    --sweep 1600x900,800x600 --out "$out"
done
cargo test --locked -p shell --bin shots \
  history_qualification_preserves_evidence_and_heads_at_both_sizes
cargo check --locked -p shell --tests
cargo clippy --locked -p shell --bin shots -- -D warnings
```

The focused scenario test passed at both sizes, and the shell test-target check
passed, as did the shots binary's warning-denying Clippy check. These assertions
operate on the production pane before image capture:
validated source membership; the citation's exact inclusive work-session span;
resource context retained; actual comparison materialization; unchanged document
heads for reading, comparison and seeking; shared slider accessibility actions
at both endpoints; unavailable seek refusal without changing the selection.
The CPU-only test sink uses the same paint pass as the PNG sink.

## Paint observations

- Light, dark and high-contrast replay have readable controls and explicit
  historical-topology scope. The shared slider paints at the checkpoint endpoint.
- Wide generated-summary presentation shows the model identity, locale,
  evidence coverage, bounded-excerpt count and “citations are not fact checks”.
  Its generator is explicitly `shots fixture (not a live model)`; prose was
  injected through the production request validator only in the shots driver.
- At 800×600 the summary card extends beneath the viewport; its long attribution
  consumes the remaining height before the generated paragraph and citation.
  This capture does **not** qualify narrow summary readability or reachability.
- The new Details capture visibly paints the comparison and the focused Details
  control. However, provenance exposes `tilo.receipt` / invocation IDs, and
  “What changed” exposes task and receipt identifiers rather than readable
  resource names. That violates the interface guideline against implementation
  labels. This report does not claim the copy is polished.
- The legacy `room_activity_explore_detail_compare` keyboard sequence does not
  paint the promised comparison: its final frame selects the Ada author filter
  and stays in Timeline. Successful rendering alone is insufficient proof.
- The automatic-session seam captures show the boundary controls, but the
  baseline fixture opens a tile partially beyond the right display edge. The
  standalone correction captures are therefore not full-pane clipping proof.

## Representative images

1. `target/history-ux-shots/light/history_visual_qualification/1600x900/02-validated-fixture-summary.png`
2. `target/history-ux-shots/dark/history_visual_qualification/1600x900/04-exact-comparison.png`
3. `target/history-ux-shots/high-contrast/history_visual_qualification/800x600/05-accessible-slider-seek.png`

For the small summary defect, inspect
`target/history-ux-shots/light/history_visual_qualification/800x600/02-validated-fixture-summary.png`.
For the failed keyboard journey, inspect
`target/history-ux-shots/light/room_activity_catch_up_keyboard/800x600/02-window-reordered-and-resized.png`.

## Limits

Stills prove paint, not a live model, screen-reader delivery or a real-seat input
experience. The fixture test proves the controller/action behavior separately.
The existing fixture names work sessions and exercises their shared seam
correction; shared text-editor rename interaction is not qualified here.
A missing retained replay selection (as distinct from an invalid seek), managed
resource filters, and narrow summary scrolling remain unqualified. No production
pane changes were made to hide these gaps. Offscreen commands terminate without
spawning a compositor or a desktop runtime directory.
