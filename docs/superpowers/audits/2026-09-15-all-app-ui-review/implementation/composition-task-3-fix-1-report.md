# Task 3 fix round 1 report — Themes disclosure

Status: **DONE**. Both scoped Themes findings are fixed and committed. No Store, Settings, VPN, or shared UI production file was changed.

## Commits

- `f3e6e6f7f` — `fix(themes): contain narrow preview and expose selection`
- `711191161` — `fix(themes): align narrow view selector`
- The report commit is the commit containing this file.

No push or merge was performed.

## Implemented

- Replaced the two plain narrow-view buttons with the shared two-segment `ToggleGroup`. The selected segment now has a persistent visible accent fill, the group exposes its selected label, and both synthetic segment accessibility nodes expose checked state.
- Kept pointer, keyboard, and accessibility selection on the typed `qos.ui.segmented.select` view-operation route. Escape returns from Full preview to Customize and focuses the compound selector.
- Centralized the specimen's margins, title bar, section starts, chart bounds, and minimum chart height in `PreviewPane::SpecimenLayout`. The narrow pane now requests the preview-owned minimum height instead of the insufficient fixed 560px allocation.
- Included that measured preview height in the existing narrow scroll extent. At 480×480, maximum wheel scroll reaches the final chart row while both charts remain inside the preview card and pane.
- Preserved per-view scroll, the chosen view, unsaved edits, and document heads through view changes and wide/narrow breakpoint crossings. Hidden customizer/preview controls remain omitted from the active widget subtree.
- Added deterministic `customize` capture state and routed the custom-preview capture seed through the same selection synchronizer. Short custom/live preview captures settle at maximum scroll to show the contained tail.
- Applied the existing 16px narrow content gutter directly to the segmented selector's horizontal margin, aligning it with the customizer without adding another vertical wrapper or changing scroll height.

## RED/GREEN evidence

- The first containment regression failed to compile because `PreviewPane` had no owned card/tail geometry; the app duplicated a fixed 560px height. It passed after adding shared specimen geometry and the preview-owned intrinsic height.
- The initial accessibility assertions had no selected/checked state with the old `Row<Button>` composition. They pass with `ToggleGroup` for editable Customize, live Full preview, pointer switching, keyboard switching, Escape, and resize.
- Visual inspection caught a separate integration regression in the existing custom-preview shot seed: it displayed the preview while Customize remained selected. `custom_preview_capture_seed_selects_full_preview` failed with `("Customize", [true, false])`, then passed after the seed used `set_narrow_preview`.

## Verification

- `cargo fmt --package themes -- --check` — passed.
- `git diff --check -- apps/themes/src/app.rs apps/themes/src/preview.rs` — passed before the production commit.
- `cargo test -p themes narrow_ --lib` — 3 passed during focused GREEN verification.
- `cargo test -p themes custom_preview_capture_seed_selects_full_preview --lib` — 1 passed after the visual RED regression.
- `cargo test -p themes` — 52 passed; library, binary, and doc-test targets all green after the two functional fixes.
- `cargo test -p themes narrow_ --lib` — 3 passed again after the later horizontal-gutter-only follow-up, covering selector position, switch behavior, and preview tail geometry.
- `cargo clippy -p themes --lib --no-deps -- -D warnings -A clippy::struct_excessive_bools -A clippy::items_after_statements -A clippy::too_many_lines` — passed. The three allowances are existing warnings in the pre-existing `ThemesApp` structure/event method, not new findings.
- `cargo clippy -p themes --all-targets -- -D warnings` — could not reach Themes because concurrent, unrelated Chromium work currently emits 32 denied warnings in `ui/src/webview/chromium`; those files were left untouched.

## Fresh visual evidence

All captures used the repository `target` cache and a fresh task profile at `/tmp/qos-themes-task3-fix1.iFXwVW`; `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` all pointed inside that profile.

- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/customize-480x900.png`
- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/custom-preview-480x900.png`
- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/live-preview-480x900.png`
- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/customize-480x480.png`
- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/custom-preview-480x480.png`
- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/live-preview-480x480.png`
- `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/custom-preview-800x480.png`

The 480×900 captures show the correct selected fill for Customize and Full preview and show both chart surfaces inside the card. The 480×480 and 800×480 preview captures show the maximum-scroll tail with the chart row and bottom card margin fully visible. The short Customize capture shows its selected state and a usable scroll extent.

## Concerns

- No scoped correctness concern remains. Repository-wide strict Clippy is presently blocked by the unrelated concurrent Chromium lint failures described above.
- The task-specific capture profile remains under `/tmp` for controller inspection; it contains no real user preferences.
