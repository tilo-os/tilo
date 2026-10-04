# App Capture Fidelity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every reviewed app screenshot honor its requested viewport and text scale before UI fixes are accepted.

**Architecture:** Share screenshot theme resolution between the standard launcher and custom handlers. Thread that Theme into each existing render pipeline, set the renderer scale, and preserve fixture setup. This is the first independently reviewable subproject of the all-app UI balance effort.

**Tech Stack:** Rust, Tilo ui, Vello, Taffy, headless FrameHarness.

**Spec:** `docs/superpowers/specs/2026-09-15-app-ui-balance.md`

## Global Constraints

- Preserve primary actions, keyboard access, state and explicit manual dimensions.
- Keep legacy renderer-relative font APIs compatible; theme-resolved fonts must scale once.
- No dependency upgrades.
- Do not fix application composition in this capture-only subproject.

### Task 1: Unify custom screenshot theme and measurement setup

**Files:**
- Modify `ui/src/launch.rs`: expose existing `shot_theme` as public with documented CLI precedence over compatibility environment scale, supporting existing named themes and motion flags.
- Modify screenshot handlers in `apps/{camera,chat,code_editor,contacts,disks,slides,sheets,typst_editor,video-player,video_editor,word-processor,file-explorer,maps,monitor,imager,music,terminal}/src/main.rs` and `apps/store/src/{main,shots}.rs`.
- Tests: launcher unit tests and existing app screenshot/argument tests in the files above; add only focused regression cases that verify the behavior.

**Interfaces:**
- Produces `pub fn shot_theme(args: &LaunchArgs) -> Theme` in `ui::launch`.
- Custom render functions accept a resolved `Theme` or `&Theme`; do not mutate global environment to transmit scale.
- CLI `args.text_scale` wins over `TILO_SHOTS_TEXT_SCALE`; absent both means1. Existing validated/clamped Theme behavior remains authoritative.
- Files honors standard `args.viewport`, retaining `--size` as an explicit compatibility override when provided.

- [ ] Inspect the existing custom fixture setup and renderer-aware layout entry point before changing it. Preserve named palette selection, app-specific defaults, motion options, setup-after-sync and all screenshot scenarios.
- [ ] Add a regression test around pure scale selection using these cases (implement a small pure helper if necessary to avoid process-global environment mutation):

```rust
// CLI value, environment value, expected scale
[(Some(2.0), Some("1.25"), 2.0),
 (None, Some("1.25"), 1.25),
 (None, None, 1.0),
 (None, Some("invalid"), 1.0)]
```

Run the focused launcher tests before implementation and record expected failure.
- [ ] Publish the existing theme builder and consistently configure custom renderers from its result:

```rust
let theme = ui::launch::shot_theme(args);
let mut text_renderer = ui::text_render::TextRenderer::new();
text_renderer.set_text_scale(theme.text_scale);
```

For FrameHarness use `FrameHarness::new_with_theme`. For manual Taffy pipelines, route through the existing renderer-aware solve rather than adding a second measuring algorithm; keep fixture state and final settling pass. Thread the theme through internal render function arguments where needed. Preserve Imager's standalone scenario route by passing its parsed scale into the same theme/renderer setup.
- [ ] Correct Files viewport precedence and add a regression case: `--viewport 480x900` produces `(480,900)`; explicit `--size 640x480` retains its existing override semantics.
- [ ] Run focused launcher/app argument tests, `cargo check --no-default-features` for all changed app packages, then the relevant UI library tests once. Set `CARGO_BUILD_JOBS=4`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`. Avoid workspace-wide formatting or unrelated files.
- [ ] Build changed app binaries and provide fresh paired1×/2× captures at the same800×900 viewport for at least Camera, Sheets, Maps, Files and Music, verifying actual typography changes and dimensions. Put evidence in this plan's SDD workspace; include command, exit result and binary hash. Do not claim the newly exposed large-text layout defects are fixed.
- [ ] Self-review every listed handler, commit only scoped files, report tests and remaining visual defects to the controller. Controller will independently inspect all newly truthful app captures before the app fix packages.
