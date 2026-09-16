# Task 11 shared ToggleGroup prerequisite

Controller authorized this exact shared-source repair after the API Tester true 2× capture showed resolved 24 px tab text painting at 48 px.

- Production scope: `ui/src/widget/toggle_group.rs` retained draw only. Normalize the already-resolved theme font with `TextRenderer::renderer_input_style`; pass that identical style to measurement and drawing. No sizing, padding, selection, action or event changes. The immediate paint API still accepts its existing renderer-relative style.
- RED: `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib retained_labels_apply_resolved_text_scale_exactly_once` — 0 passed, 1 failed: `2x text must paint once at resolved size 24, got 48`.
- GREEN / affected existing tests: `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::toggle_group::tests` — 13 passed, 0 failed. This includes the new actual glyph-run font-size check at 1× and 2×, plus existing pointer, keyboard, accessibility, action and paint tests.
- Existing macOS linker warning: `__eh_frame section too large ...`; no test failures.
- Self-review: helper is the accepted shared normalization API; no reciprocal app font compensation. The 1× glyph size is unchanged. Broader widget scaling was not changed or claimed.
- Exact command output is in `logs/toggle-{red,green}.log`.
