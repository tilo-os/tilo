# Task 1 report: custom screenshot capture fidelity

## Result

Implemented the capture-fidelity prerequisite across all 18 listed custom screenshot handlers. Standard and custom handlers now resolve palette, text scale, and motion through `ui::launch::shot_theme`; manual Taffy capture paths configure one `TextRenderer` from that theme and use the shared renderer-aware `compute_widget_layout` solver for measurement. Existing fixture setup, post-sync scenario staging, structural settling, media warm-up, overlays, and Store renderer settling were retained.

Scoped implementation commit: `ef5f08db5`.

No application composition was changed. The new 2× captures deliberately expose existing large-text layout defects for the next work package.

## Implementation

- `ui/src/launch.rs`
  - Published `pub fn shot_theme(args: &LaunchArgs) -> Theme`.
  - Preserved named theme lookup through `Theme::named`.
  - Added compatibility `QOS_SHOTS_TEXT_SCALE` fallback with CLI precedence.
  - Preserved validated/clamped `Theme::with_text_scale` behavior and all three existing reduced-motion environment flags.
  - Added a pure regression table for `(CLI, environment, expected)` scale selection.
- `apps/camera/src/main.rs`, `apps/slides/src/main.rs`
  - Passed the shared resolved theme into `FrameHarness::new_with_theme`, whose constructor configures renderer scale.
- `apps/chat/src/main.rs`, `apps/code_editor/src/main.rs`, `apps/contacts/src/main.rs`, `apps/disks/src/main.rs`, `apps/sheets/src/main.rs`, `apps/typst_editor/src/main.rs`, `apps/video-player/src/main.rs`, `apps/video_editor/src/main.rs`, `apps/word-processor/src/main.rs`, `apps/maps/src/main.rs`, `apps/monitor/src/main.rs`, `apps/terminal/src/main.rs`
  - Threaded the resolved theme into the existing manual capture functions.
  - Set renderer scale from `theme.text_scale` before both measurement and draw.
  - Replaced raw `compute_layout_with_measure` calls with the existing admitted, renderer-aware `ui::widget::compute_widget_layout` solver.
  - Kept each app's sync/scenario order and app-specific theme override behavior.
- `apps/file-explorer/src/main.rs`
  - Routed screenshot mode through `LaunchSpec`, preserving all 17 scenarios, `--path`, and `--size`.
  - Added pure viewport precedence coverage: standard `--viewport` is honored and explicit `--size` wins.
  - Retained the second structural settling pass, priming draw, thumbnail warm-up, post-layout scenarios, and overlay draw.
- `apps/imager/src/main.rs`
  - Passed the standard handler's resolved theme into the common renderer.
  - Preserved standalone `--shots-dir` by converting its parsed scale into the same theme/renderer flow.
- `apps/music/src/main.rs`
  - Based capture theme selection on the shared resolver while preserving `--theme`, `QOS_MUSIC_THEME`, and app-scoped reduced motion.
  - Switched the common frame function to the renderer-aware solver; live-shot settling loops remain unchanged.
- `apps/store/src/main.rs`, `apps/store/src/shots.rs`
  - Added a resolved-theme entry point for the binary handler.
  - Preserved compatibility wrappers used by existing tests and the three CPU-renderer warm-up passes.

## TDD evidence

### Launcher scale precedence

RED:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui launch::tests::screenshot_text_scale_prefers_cli_then_compatibility_environment
error[E0425]: cannot find function `shot_text_scale` in this scope
```

GREEN:

```text
test launch::tests::screenshot_text_scale_prefers_cli_then_compatibility_environment ... ok
test result: ok. 1 passed; 0 failed
```

The table covers exactly:

```text
(Some(2.0), Some("1.25")) -> 2.0
(None, Some("1.25"))      -> 1.25
(None, None)              -> 1.0
(None, Some("invalid"))   -> 1.0
```

### Files viewport precedence

RED:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p file-explorer screenshot_viewport_honors_standard_value_unless_size_overrides_it
error[E0432]: unresolved import `super::shot_viewport`
```

GREEN:

```text
test actor_tests::screenshot_viewport_honors_standard_value_unless_size_overrides_it ... ok
test result: ok. 1 passed; 0 failed
```

Binary confirmation:

```text
--viewport 480x900                         -> 480×900
--viewport 480x900 --size 640x480          -> 640×480
```

## Verification commands and results

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check --no-default-features \
  -p camera -p chat -p code_editor -p contacts -p disks -p slides -p sheets \
  -p typst_editor -p video-player -p video_editor -p word-processor \
  -p file-explorer -p maps -p monitor -p imager -p music -p terminal -p store
PASS (18 packages)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui launch::tests
PASS (23 passed, 0 failed)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test --no-default-features --bins \
  -p camera -p chat -p code_editor -p contacts -p disks -p slides -p sheets \
  -p typst_editor -p video-player -p video_editor -p word-processor \
  -p file-explorer -p maps -p monitor -p imager -p music -p terminal -p store
PASS (all binary tests; 9 total tests, 0 failed)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib
PASS (3064 passed, 0 failed, 2 ignored)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo build \
  -p camera -p sheets -p maps -p file-explorer -p music
PASS

cargo fmt -- <20 scoped Rust files>
PASS

git diff --check
PASS
```

The UI test linker emitted the existing macOS warning that `__eh_frame` exceeds the compact-unwind encoding limit; tests completed successfully.

## Capture evidence

All paired captures were produced by the freshly built binaries with the same `--viewport 800x900`. The 2× captures add `--text-scale 2`. `sips` reports 800×900 for every file, and visual inspection confirms larger typography rather than only different metadata.

Binary SHA-256:

| Binary | SHA-256 |
| --- | --- |
| `target/debug/camera` | `41e512f78e388cd08e726e904171da9b827981e01c494d735e2002e04613a156` |
| `target/debug/sheets` | `35514f99f12f9bc9755e1b3f197973cfbde6c405e2a804beeb07d69b6354d983` |
| `target/debug/maps` | `5d005d27743b3cf10863ec456f1235821000f61549ba2a3de79fcd3f1ceac1ed` |
| `target/debug/file-explorer` | `fabf159539af860ee8e855ec757f4e45d0bd5e4265e9e8805657062c6cbf645b` |
| `target/debug/music` | `39875110320eb3485e5f0c613c7c9bc6f764fd77b8f026e981962a327822042b` |

PNG SHA-256:

| App | 1× capture | 1× SHA-256 | 2× capture | 2× SHA-256 |
| --- | --- | --- | --- | --- |
| Camera | `captures/camera-1x.png` | `dc2caf63c8640f42d4084892152a0f880ed38e99401397f4d2ffaffdfacf2f90` | `captures/camera-2x.png` | `a698bb97dbdb3ce23d41a264b3e42dfb379d0ef0f1a55098cbd59d8ec3130720` |
| Sheets | `captures/sheets-1x.png` | `59919e0869e5eb8029bdf967b11ca5c7934d60cf48770642268648f964fc78ee` | `captures/sheets-2x.png` | `af0b57ff4281f397a279ac54821978ce8d8c334a6341e7d7c7b869b557a6ad0a` |
| Maps | `captures/maps-1x.png` | `e73c1c3f89acc19609c75d92ac677135b238d3a457366c21076572903ff7d594` | `captures/maps-2x.png` | `1d47197b8c2ede2241fd34767f62577bdd1d1a92eed21b7586b43fc77dcb8920` |
| Files | `captures/file-explorer-1x.png` | `6119eb08b8521f933b88af279e98905417df730b6c3c4fdd0d32531691b4bb5f` | `captures/file-explorer-2x.png` | `9646cd12031012870bad458e2aff914891a3eead327c29677239d7b5630fbc4b` |
| Music | `captures/music-1x.png` | `fea108898af8f462b1b8883d4b4f86b69525fb9036a2a38a9a1aed3cf52291e7` | `captures/music-2x.png` | `5cd21389c8600c1d2f380f50113bc028a2d9bff5a829e99cdbd2edbd70ebc51a` |

Paths are relative to `.superpowers/sdd/2026-09-15-app-capture-fidelity/`.

Files alias evidence:

| Capture | Dimensions | SHA-256 |
| --- | --- | --- |
| `captures/file-explorer-viewport-480x900.png` | 480×900 | `f0028b80ddeb781c04d172ea8857af95fc274a497e0a7c4e337c93c2393d55a7` |
| `captures/file-explorer-size-override-640x480.png` | 640×480 | `155f4067466d7a48d8b60f4c7ade010b54474b5b2a28d4ed28d18cd3b0a73714` |

## Self-review

- Reviewed every handler listed in the brief; none remain on the old unscaled manual screenshot measurement path.
- Confirmed the same renderer instance is used for measurement and drawing in each manual pipeline.
- Confirmed FrameHarness handles renderer scale for Camera and Slides.
- Confirmed CLI scale precedes environment scale and malformed environment input returns 1.0.
- Confirmed named themes and reduced-motion inputs remain available.
- Confirmed Files retains every pre-layout and post-layout scenario, its second structural pass, thumbnail warm-up, and overlay draw.
- Confirmed Code Editor retains its scenario-driven second layout; Store retains three offscreen renderer warm-up passes; Music retains live settling loops; Video Player/Editor retain decode warm-up; Camera retains width/height compatibility overrides; Word Processor retains the explicit 1000×700 history capture.
- `git diff --check` is clean. Unrelated dirty and untracked files remain outside the scoped commit.

## Concerns and follow-up defects

- Truthful 2× typography reveals severe overlap and clipping in Sheets, Maps, and Files at 800×900.
- Camera's 2× top toolbar and mode labels crowd their fixed regions.
- Music responds to 2× with a substantially different compact/list composition; typography is visibly scaled and the capture dimensions are correct, but controller review should decide whether that responsive result is intended.
- These are composition defects intentionally left for later work. This task only makes the capture oracle truthful.

## Round 1 review fix

Independent review found that Imager's standalone `--shots-dir` route used CLI scale or 1.0 and skipped the `QOS_SHOTS_TEXT_SCALE` compatibility fallback. The shared pure `ui::launch::shot_text_scale` resolver is now public, and the standalone route passes its parsed CLI value plus the environment value through the same CLI→environment→1 policy as `shot_theme`. Named-theme and motion resolution are unchanged.

Scoped fix commit: `4f0cc6b27` (`fix imager batch shot scale fallback`).

RED:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p imager --bin imager standalone_shot_scale_uses_environment_unless_cli_overrides_it
error[E0061]: this function takes 1 argument but 2 arguments were supplied
```

GREEN and covering verification:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p imager --bin imager standalone_shot_scale_uses_environment_unless_cli_overrides_it
PASS (1 passed, 0 failed)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p imager --bin imager
PASS (2 passed, 0 failed)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui launch::tests::screenshot_text_scale_prefers_cli_then_compatibility_environment
PASS (1 passed, 0 failed)

CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check --no-default-features -p imager
PASS

git diff --check
PASS
```

The focused UI test emitted the existing macOS compact-unwind linker warning and passed. Per review direction, the broad UI suite was not rerun.
