# Desktop UX testing bench

Start with a user goal, describe the visible route, set an interaction budget, and assert the resulting product state. Input dispatch alone never means success.

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

Scenario JSON has a `name`, user `goal`, `viewport`, `budget` for clicks and keys, and ordered `steps`. Optional fields select fixture, theme, input scale and text scale. See `scripts/ux-bench/*.json` for executable examples and `ui/src/ux_bench.rs` for the strict schema.

- `input` reuses the existing headless action model: named pointer clicks, coordinates, keys, typing, modifiers and semantic actions. `semantic:` selectors resolve real pointer targets. Direct semantic invocations are counted separately and cannot pass a visible interaction-budget journey.
- `wheel` injects line or physical-pixel input at a logical pointer position, with gesture phase. Scale converts physical movement to logical distance.
- `wait_state` waits for an adapter state outcome with a bounded timeout.
- `checkpoint` captures pixels, the inspection tree, resource/action contracts and native workspace/tab state.
- Checks cover visibility, absence, spacing, alignment, overlap, target size, explicit pixel contrast, target movement, image-region translation, stationary image regions, resources, semantics and native state.

Every journey must end with an outcome assertion. Failures retain expected and observed values. A missing or ambiguous target fails measurements; missing geometry does not prove a control is absent. The Files scroll scenario checks both moving list pixels and a stationary header region.

Click counts include taps. Key counts include each typed character and explicit key activation; modifier changes are not separately counted. These are route costs, not estimates of task duration or user comprehension.

## Action-model evidence

Each native run exports the permission-filtered shell catalogue to `initial.actions.json`. Each checkpoint exports `<name>.resources.json` with resource actions and `<name>.inspection.json` with semantic/layout evidence. `run.json` records the executable hash, exact scenario, fixture location and workspace names verified after shutdown persistence. The runner checks product state after visible actions rather than treating an available action as a successful user journey.

## Matrix and limits

`--matrix` adds compact, light, high-contrast, Retina input scale and 200% text variants. Pixel contrast probes are pinned to their authored baseline. Coordinate/image-region journeys skip compact and large-text variants and report explicit coverage gaps; these routes need separately reviewed coordinates or better semantic targets. Name-based navigation geometry runs across all variants.

Screenshots are logical-resolution captures composited onto the native window background. Retina verifies input scaling, not physical-pixel raster quality. Contrast checks sample authored rendered pixel pairs; they do not certify every glyph or the full screen. Visibility checks the window, declared clipping and hidden state, but not occlusion by sibling overlays. Some custom-painted shell and hosted app controls currently lack inspection semantics; those failures are coverage defects, not evidence that the pixels are absent.

The bench does not yet drive the macOS hardware event queue. Before accepting a scroll fix, run live checks with a wheel mouse and trackpad: pointer over inner/outer regions, nested boundaries, both directions, momentum/end/cancel, Retina and external displays, native three-finger workspace navigation, and keyboard alternatives. Review whether the route is understandable as well as whether its numeric budget passes.

## Bench verification

```sh
cargo test --locked -p ui --lib ux_bench --no-default-features
cargo test --locked -p ui --lib widget::scrollable::tests --no-default-features
cargo test --locked -p shell --bin shell --no-default-features
python3 scripts/ux-bench/test_suite.py
```

Negative controls verify that bad spacing, low contrast, excess clicks, clipped targets, static scroll regions, moving anchors, and missing evidence cannot produce a passing report. The suite is suitable for a macOS CI job after building the binary; current desktop failures intentionally make it red. Keep these outcomes visible rather than weakening checks to obtain green status.
