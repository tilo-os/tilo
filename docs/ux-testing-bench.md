# Desktop UX testing bench

Start with a user goal, describe the visible route, set an interaction budget, and assert the resulting product state. Input dispatch alone never means success.

The latest measured closure is a 48-run repaired matrix.

## Goal-only agent trials and usability properties

Measured results from 12 live trials and a deterministic usability audit: all tasks completed, but only 7/12 met efficiency gates; the stronger tests expose failures despite the original 48-case matrix passing.

The [evidence method](ux-evidence-method.md) explains what each test can establish and what remains unproven. The new runner adds keyboard entry, feedback, cancellation, recovery, state preservation, exact document outcomes, and semantic target geometry. It can also ask an OpenAI vision model to complete goals using screenshots alone, with private independent success checks.

```sh
python3 scripts/ux-bench/usability_test.py
python3 scripts/ux-bench/usability.py --mode deterministic --out target/ux-bench/usability-check
python3 scripts/ux-bench/usability.py --mode all --out target/ux-bench/usability-trials --key-prompt
```

Build `ux-bench` first. `--key-prompt` reads the key without echoing it; alternatively use `OPENAI_API_KEY`. Never put a key into a scenario or commit it. Default agent campaign: three goals, baseline and compact with 200% text, two fresh attempts each, 24 decisions per attempt. `--model`, `--tokens`, `--total-tokens`, and timeout flags control bounded API usage. Only synthetic fixture screenshots, task text and the agent's own actions are sent; hidden state, selectors and solution routes stay private.

Each attempt keeps one real native widget-host session alive. `--interactive --scenario FILE --out NEW_DIRECTORY` exposes JSON-lines observe/click/key/type/wheel/finish operations. This is real application rendering and event dispatch in an isolated profile, not OS hardware input. Positive wheel dy moves toward content below. The supplied scenario config sets the display and fixture; its scripted route is ignored in interactive mode.

The HTML/JSON report separates independently verified completion from efficient completion, records planned measurement denominators, preserves screenshots and replay inputs, and labels API errors. Nonzero exit means a required check, efficiency gate, coverage or execution failed; it does not mean the evaluator itself crashed. Reports retain failures instead of retrying until success. Run `--mode replay --replay-source ATTEMPT_DIRECTORY --out NEW_DIRECTORY` to reproduce recorded inputs without an API call.

Semantic geometry is evidence about declared targets; unresolved overlaps need comparison with pixels. Clipped fragments and unsupported areas are counted explicitly. Missing semantics is a measurement/accessibility gap, not proof that no visual feedback exists.

## Run

On macOS, from the repository root:

```sh
cargo build --locked -p shell --bin ux-bench --no-default-features
python3 scripts/ux-bench/run-suite.py --out target/ux-bench/my-review
python3 scripts/ux-bench/run-suite.py --matrix --out target/ux-bench/my-matrix
```

Choose a new output directory for every run. Open its `index.html` to compare journeys; open each journey for screenshots, target highlights, measurements and input traces. Exit codes: 0 passes, 1 required UX checks fail or matrix coverage is incomplete, 2 executable/scenario errors in the native CLI. The suite distinguishes execution errors from UX failures and exits nonzero for either.

Run one scenario:

```sh
target/debug/ux-bench --scenario scripts/ux-bench/files-scroll.json --out target/ux-bench/scroll-review
```

The runner creates an isolated HOME, profile and 81 fixture documents inside the output directory. It constructs the native shell tree and uses its real widget input, layout and rendering paths. `fixture: "files"` opens Files before measurement; this tests browsing independently of the separate launch journey. Do not interpret that fixture as proof that users can launch Files.

## Measurements

Scenario JSON has a `name`, user `goal`, `viewport`, `budget` for clicks and keys, and ordered `steps`. Optional fields select fixture, theme, input scale and text scale. See `scripts/ux-bench/*.json` for executable examples and `crates/sdk/ui-core/src/ux_bench.rs` for the strict schema.

- `input` reuses the existing headless action model: named pointer clicks, coordinates, keys, typing, modifiers and semantic actions. `semantic:` selectors resolve real pointer targets. Direct semantic invocations are counted separately and cannot pass a visible interaction-budget journey.
- `wheel` injects line or physical-pixel input at a logical pointer position or the visible center of a unique semantic `target`, with gesture phase. Scale converts physical movement to logical distance.
- `wait_state` waits for an adapter state outcome with a bounded timeout.
- `checkpoint` captures pixels, the inspection tree, resource/action contracts and native workspace/tab state.
- Checks cover visibility, absence, spacing, alignment, overlap, target size, keyboard focus, explicit pixel contrast, target movement, image-region translation, stationary image regions, resources, semantic values/selection and native state.

The `focus` check names a widget path, a test id or a `semantic:` name and passes when that control is where the runner's keyboard focus is: the widget itself, or a control inside it that publishes `focused`. It is the width-class check values cannot stand in for, because shape decides which controls exist at which width and a journey that only reads values can pass while the keyboard can no longer reach the primary action. Its observed evidence names the focus owner, and the checkpoint's inspection file records the same tree. Use it beside `visible` and `target_size` on the same target: reachable, drawn and big enough to hit are three separate promises.

Every journey must end with an outcome assertion. Failures retain expected and observed values. A missing or ambiguous target fails measurements; missing geometry does not prove a control is absent. The Files scroll scenario checks both moving list pixels and a stationary workspace chip. Target-relative image regions use `{"target":"semantic:Files","inset":[12,32,12,32]}` (left/top/right/bottom logical-pixel insets). Both captures must have identical target bounds; the same screen region is sampled in both, so moving the whole container cannot pass. Blank stationary regions and unchanged scroll pixels fail.

Click counts include taps. Key counts include each typed character and explicit key activation; modifier changes are not separately counted. These are route costs, not estimates of task duration or user comprehension.

## Action-model evidence

Each native run exports the permission-filtered shell catalogue to `initial.actions.json`. Each checkpoint exports `<name>.resources.json` with resource actions and `<name>.inspection.json` with semantic/layout evidence. `run.json` records the executable hash, exact scenario, fixture location and workspace names verified after shutdown persistence. The runner checks product state after visible actions rather than treating an available action as a successful user journey.

## Matrix and limits

`--matrix` executes all eight original journeys across baseline, compact (800×700), light, high-contrast, Retina input scale and 200% text: 48 runs. Click routes use the actual semantic control bounds. Files scrolling uses its real viewport and a target-relative stationary workspace chip. Settings also scrolls its General body to the summary and back before renaming, within the original click/key budget.

Contrast remains two explicit rendered pixel pairs per variant at a minimum 4.5:1. `VARIANT_PIXEL_PROBES` in `run-suite.py` contains separately authored compact and large-text glyph interiors; other variants retain the original pairs. These probes deliberately sample the Home name and app shortcut hint, not icons or blank wallpaper. When typography/layout changes, inspect the rendered glyphs and re-author affected probes; do not lower the contrast threshold.

Screenshots are logical-resolution captures composited onto the native window background. Retina verifies input scaling, not physical-pixel raster quality. Contrast checks sample authored rendered pixel pairs; they do not certify every glyph or the full screen. Visibility checks the window, declared clipping and hidden state, but not occlusion by sibling overlays. A missing semantic target remains a coverage defect, not evidence that the pixels are absent.

The bench does not yet drive the macOS hardware event queue. Before accepting a scroll fix, run live checks with a wheel mouse and trackpad: pointer over inner/outer regions, nested boundaries, both directions, momentum/end/cancel, Retina and external displays, native three-finger workspace navigation, and keyboard alternatives. Review whether the route is understandable as well as whether its numeric budget passes.

## Bench verification

```sh
cargo test --locked -p tilo-ui --lib ux_bench --no-default-features
cargo test --locked -p tilo-ui --lib widget::scrollable::tests --no-default-features
cargo test --locked -p shell --bin shell --no-default-features
python3 scripts/ux-bench/test_suite.py
```

Negative controls verify that bad spacing, low contrast, excess clicks, clipped targets, static scroll regions, moving anchors, and missing evidence cannot produce a passing report. The suite is suitable for a macOS CI job after building the binary; any required failure makes it red. Keep original failing reports for comparison; never weaken checks to obtain green status.
