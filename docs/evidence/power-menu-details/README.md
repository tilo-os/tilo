# Complete session descriptions and shared tooltip wrapping

Source baseline: `ae67fc35d70b479b9a24a8c3a1426e289cbc6a12`.
Verified implementation: `2e9349c8d64f0252bf4ad6cdc43cd449c52c3dc9`.

The 200% text baseline ellipsizes the power menu's secondary descriptions.
The shared tooltip also measures a long hint as one line and lets glyphs escape
its capped box. The production-path tooltip regression fails on the baseline
because wrapped caption height plus padding exceeds the painted box.

The fix uses the existing TooltipHost and shared caption/material painter for
keyboard and pointer reading. Measurement and painting wrap at the same available
width. Wide outputs place hints beside the panel, aligned to the selected row;
compact outputs use a clamped, inert hint. Clicking a hint twice performs no action.
Pointer leave, Esc, reopen and resize clear or reposition it. Accessible row targets
invoke the existing session intents; closing the menu matches keyboard activation.
No OS session action is executed by these evidence fixtures.

## Capture and validation

The production PowerMenu and Tooltip painters generated 12 before and 12 after
captures: dark, light, high contrast, 200% text, 150% UI plus 130% text, and a narrow
320×480 output. Eight inspected representatives are saved to Library. Their original
native IDs, byte counts, dimensions, source commits and SHA-256 hashes are in
[artifacts.json](artifacts.json). Browser screenshot links are provided in the PR body.
No PNG or other capture binary is committed.

All work ran remotely with Vulkan and the Mesa lavapipe CPU ICD. The capture fixture
is reproducible with `TILO_POWER_DETAIL_SHOTS` and the
`power_menu_detail_capture_matrix_is_written` lib-test filter. It renders production
surfaces offscreen; it is not a live VM or physical-device screenshot.

Validation passed: nine power-menu cases (including actual LayerCx pointer routing,
keyboard/cancel/repeated press/resize and semantic availability), six shared-tooltip
cases, `cargo check --locked -p shell --tests`, and UI/shell all-target Clippy with
warnings denied. Staged pre-commit and normal pre-push checks passed. The affected
plan selects 97 of 273 packages (`full: false`), from the shared UI component's
reverse dependencies and contracts.

Messages #2881 and Dictation #2974 share overlay_host/mod.rs but their inspected
UI hunks do not touch the Session intent close line. Displays #3095 has no file
overlap. Files #3009 broker/transfer/undo paths are untouched. Overview #3076 remains
an independent ready PR waiting for its separately owned lock-recovery fixture repair.
