# Task 5 report — Intrinsic card and section minimum propagation

Status: **DONE_WITH_CONCERNS**. Card, ContentCard, and CardGrid now contribute their automatic block-axis minimum to intrinsic Grid rows. The Store catalogue and Settings Region dependency regressions pass, and fresh captures show the previously overlapping sections in order.

## Scope and implementation

- Changed `Card`, `ContentCard`, and `CardGrid` from a zero minimum height to an automatic minimum height. Each source site explains that `flex_shrink` does not protect a Grid item during Grid sizing.
- Kept the zero minimum width that allows text and cards to wrap in narrow layouts.
- Preserved explicit CardGrid row heights, button dimensions, flexible internal tracks, surface caps, padding, gaps, and application composition.
- Added measured framework regressions for eight automatic cards plus a trailing action, a legacy Card with six form rows, direct narrow and nested wide section grids, 1×/2× text scale, and wide→narrow→wide relayout equivalence.
- Did not change application source or capture routing.

The task started from `5ca1d1d83`. Unrelated Chromium work advanced the shared branch through `19f65aab7`, `736da9dc7`, and `5673a5e67` while this task was in progress; those commits are outside this task's diff.

## RED evidence

Before changing production code:

- `cargo test -p store full_narrow_catalogue_keeps_discover_section_below_ready_cards -- --nocapture` — failed: Ready-card descendants ended at y=863 while Discover began at y=576.
- `cargo test -p settings --test it region_preview_follows_every_format_control -- --nocapture` — failed: the final field ended at y=450 while Preview began at y=436.
- `cargo test -p ui widget::card_composition::tests::automatic_content_cards_expand_a_short_scroll_stack --lib` — failed: the first automatic card extended to y=188 while the next began at y=37.
- `cargo test -p ui widget::card_composition::tests::automatic_legacy_card_expands_in_a_short_intrinsic_grid --lib` — failed: Preview began at y=340 before the legacy Card ended at y=355.
- `cargo test -p ui widget::card_composition::tests::nested_card_grids_propagate_content_minimum_to_following_sections --lib` — failed after matching the Store's direct narrow topology: Discover began before the Ready cards ended.

The first small nested fixture did not reproduce the contract and was replaced before implementation, as required by the brief.

## GREEN and compatibility evidence

Focused verification after the three automatic minimum-height changes:

- `cargo test -p ui widget::card_composition::tests --lib` — 10 passed.
- `cargo test -p ui widget::card::tests --lib` — 13 passed.
- `cargo test -p ui widget::column::tests --lib` — 15 passed.
- `cargo test -p ui widget::scrollable::tests --lib` — 34 passed.
- `cargo test -p store full_narrow_catalogue_keeps_discover_section_below_ready_cards -- --nocapture` — 1 passed.
- `cargo test -p settings --test it region_preview_follows_every_format_control -- --nocapture` — 1 passed.

Existing compatibility coverage remains green:

- `content_cards_keep_explicit_fixed_rows` keeps `CardGrid::with_card_height(180)` at exactly 180px with oversized prose.
- `actions_keep_manual_sizes_and_secondary_then_primary_order` keeps authored button dimensions and action order.
- CardGrid's legacy fixed-row tests keep explicit grid rows unchanged.
- The new relayout test verifies 1× and 2× text, including wide→narrow→wide geometry matching a fresh wide layout.
- The short-stack test verifies that content owns the scroll extent while the viewport stays fixed, the bottom action becomes reachable, pointer activation fires once, keyboard focus traversal reaches it, and semantic activation fires once.

## Broad UI suite

`CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui` was run once after focused tests:

- UI library: 3,070 passed.
- UI integration tests: 294 passed, 3 failed.

The three failures and their exact observed mismatches were:

1. `bound_component_dispatch::legacy_bound_mutable_admission_edits_only_its_record_and_refuses_missing_or_busy` — actual `[("bound-B", true), ("", true)]`, expected `[("bound-B", true)]`.
2. `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` — `shell/src/shell/frame.rs` reported `literal_color=1` and `white_or_black=2`, both with baseline 0.
3. `typography_visual_regression::multilingual_gallery_matches_scale_matrix` — all six snapshots were taller than their baselines; examples include 1160×1073 versus 1160×824, 1450×1341 versus 1450×1030, and 1740×1788 versus 1740×1415.

To establish provenance, an isolated worktree at the then-current shared HEAD was populated with the current unrelated working changes while omitting only this task's `card.rs` and `card_composition.rs` edits. Each failing test was rerun individually there and produced the same mismatch. These three failures are therefore baseline-confirmed and are not caused by this task's Card changes.

## Fresh visual evidence

Store and Settings were freshly built from the corrected shared source with:

`CARGO_TARGET_DIR=.superpowers/app-ui-2026-09-15/task5-after-20260916/target CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked -p store -p settings`

The build passed. Binary SHA-256 values were:

- Store: `6c27510f98d50a00d9310097eb64342647ff3bb6a17d38051a723ab6f52bb901`
- Settings: `080b133a34100f0d099cbc71dbaa4e13a12d90b384d4f55aebf259dc04daca86`

Each app used its own isolated `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` beneath `.superpowers/app-ui-2026-09-15/task5-after-20260916/profiles/`. All capture commands exited 0. Evidence is under `.superpowers/app-ui-2026-09-15/task5-after-20260916/captures/`:

- Store `wide.png` — 1200×800; the three Ready cards end before the three Discover cards.
- Store `narrow.png` — 480×900; Terminal, Files, and VLC cards are disjoint and Discover follows below the fold.
- Store `short.png` — 800×480; card content remains contained and the viewport owns scrolling.
- Store `large-text.png` — 800×900 at 2× text; cards expand and remain disjoint, with lower content available by scrolling.
- Settings `panel-region_formats.png` — 1200×760 on the truthful Region route; all six controls remain inside Formats and Preview begins below it.
- Settings `panel-region_formats-narrow.png` — 440×820 on the truthful narrow Region route; Preview clears the final control and Shared follows below.
- Settings `panel-region-formats-large-text.png` — 1200×760 at 2× text on the Region route; Formats expands and Preview remains below it.
- Settings `short.png` — 800×480 on the ordinary responsive route; External Accounts is disjoint and scrollable. This is not presented as Region evidence.

All eight PNGs were inspected after capture and their dimensions were verified with `file`.

## Self-review and remaining concerns

- The production change is limited to the diagnosed Taffy minimum-height contribution at three component boundaries. No pixel minimums or application workarounds were introduced.
- The tests preserve the min-width-zero and fixed-size contracts while exercising both direct and nested Grid composition.
- `FrameHarness` does not reproduce the application's host-level Enter-key fallback. The regression proves pointer activation, keyboard focus reachability, and one semantic activation; it does not directly synthesize Enter for the final action. Existing Button host behavior is unchanged.
- The three broad-suite failures remain open in their existing owners, with isolated baseline reproduction recorded above.
- No push or merge was performed.
