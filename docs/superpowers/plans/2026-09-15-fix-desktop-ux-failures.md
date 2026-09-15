# Fix desktop UX failures implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: subagent-driven-development. Execute tasks sequentially with independent review.

**Goal:** Fix every observed failing desktop journey and remove the automated matrix coverage gaps without weakening outcomes.
**Architecture:** Correct native shell input/semantics and desktop navigation using existing widget/action interfaces; replace brittle benchmark probes with portable evidence where needed. Keep visible input costs and actual state outcomes.
**Tech Stack:** Rust UI/native macOS shell, headless action driver, Python matrix runner.
**Spec:** `docs/superpowers/audits/2026-09-15-ux-bench-results.md`, existing `scripts/ux-bench/*.json`, and user instruction “fix all failing parts”.

## Global Constraints

- Fix product behavior; never force success, widen budgets, lower contrast thresholds, remove required outcomes, or skip formerly failing cases to turn reports green.
- Keep interaction through the real shell/widget input and action model. Semantic exposure must describe actual visible controls and actual bounds, not benchmark-only targets.
- Preserve unrelated dirty and untracked work. Do not commit, reset, stage, push, merge, or delete another task's files. Review snapshot diffs capture this task's changes against its starting dirty tree.
- Retain isolated profiles, saved-state verification, permission-filtered actions and complete screenshot/inspection/resource evidence.
- Native hardware event delivery remains a stated manual acceptance limit; do not claim it tested by headless injection.
- No subagents from implementers or reviewers; the controller supplies all review seats.

## Task 1: Restore hosted Files input, semantics and document opening

**Files:** inspect `shell/src/compositor/macos.rs`, `shell/src/shell/overlay_host.rs`, hosted app host/cache code, `apps/file-explorer/src/`, `shell/src/shell/hosted_inputs.rs`; change the narrow responsible files. Tests in the corresponding host/app module or native shell tests. Bench scenarios remain unchanged in this task unless proven invalid; report evidence before changing any.
**Consumes:** existing native prepare session, headless driver, file fixture, Files tab rendering.
**Produces:** real Files rows with valid semantic bounds; pointer/key input reaches hosted app; scrolling moves list content with fixed app header; opening seeded document produces a real visible document outcome.

- [x] Reproduce `browse-file.json` and `files-scroll.json` against current binary (fresh output); inspect route and determine the root cause before editing. Files list currently renders but does not scroll and click/Enter fails to open Document-000.txt.
- [x] Add regression tests exercising the broken input/semantic boundary. Expected: pointer over Files content plus physical pixel wheel -60 changes content y by -60 logical pixels, unchanged app/header position, same logical distance at scale 2 with delta -120; file row is exposed with its actual rendered bounds; pointer selection plus Enter opens the selected seeded file. Include boundary/handled-state behavior at the layer changed.
- [x] Implement minimal shared/native hosting fixes without duplicating a second fake Files UI or bypassing providers. Keep keyboard focus and transforms consistent with drawing.
- [x] Run focused regressions, build `cargo build --locked -p shell --bin ux-bench --no-default-features`, then run both scenarios to fresh `target/ux-bench/fix-files-*` outputs. Preserve failures and investigate until task outcomes pass.
- [x] Self-review and write report with exact files, root causes, command results, screenshots, test evidence and remaining integration concerns. No commits because checkout contains overlapping uncommitted work.

## Task 2: Repair desktop discovery, launch, workspace navigation and contrast

**Files:** `shell/src/shell/overlay_host.rs`, `launcher.rs`, search providers, `overview.rs`, `workspace_settings*.rs`, native adapter as required; matching regression tests. Preserve Task 1 changes.
**Consumes:** actual hosted app outcomes and semantic bounds from Task 1; existing global search/action catalogue.
**Produces:** usable Open an app affordance, Files launch from palette, discoverable settings/overview, correct workspace return and readable shortcut hint.

- [x] Reproduce desktop, open-files, workspace-overview, workspace-settings and navigation-layout scenario failures; inspect screenshots and state rather than treating missing semantics as absent pixels.
- [x] Expose a visible functional “Open an app” control with at least 24 by 24 logical-pixel target, using existing shell launch action. Ensure entering Files through the existing Meta+k/type/Enter route selects the Files app and opens it within the existing 0-click/7-key budget. Add a state outcome if current Files visibility alone is too weak.
- [x] Expose actual overview workspace and creation controls and settings/name-input semantics with valid bounds. Fix create-workspace then previous-workspace navigation so active workspace returns Home within existing 3 clicks. Ensure rename still completes and persists Bench within 2 clicks/8 keys. Add relevant behavior regressions, including modal/overlay input ownership.
- [x] Fix shortcut hint contrast to meet 4.5:1 against its real background, retaining theme consistency; preserve navigation geometry checks. If text moves, explicitly re-author probe coordinates based on rendered glyph evidence, never select unrelated pixels to get a pass.
- [x] Build, run focused regression tests and all five scenarios to fresh outputs. Include checks for compact/text-scaled layouts where affected.
- [x] Write report with files, root causes, exact validation, evidence and remaining concerns. Do not commit or change unrelated work.

## Task 3: Close automated matrix gaps and verify all journeys

**Files:** `scripts/ux-bench/*.json`, `run-suite.py`, `test_suite.py`, `ui/src/ux_bench*.rs` / submodules only as necessary, documentation; product fixes only when a new actual matrix failure requires them.
**Consumes:** semantic targets and corrected behavior from Tasks 1-2.
**Produces:** all eight existing journey definitions exercised across six variants with truthful outcomes and no automated coverage gaps.

- [x] Replace coordinate-dependent inputs with the actual semantic targets exposed by product fixes. For scroll/image checks use target-relative sampled regions or deliberately authored variant probes; never compare irrelevant/blank pixels. Make compact, light, high-contrast, Retina input scale and 200% text routes valid. Contrast remains rendered-pixel evidence with minimum 4.5:1; introduce a targeted glyph-region probe if necessary, preserving negative controls and rejecting missing/blank/out-of-bounds evidence.
- [x] Ensure matrix no longer skips the formerly pinned cases; baseline and all five variants must execute all eight original goals. Existing budgets/outcomes remain binding. Use stronger real product state to disambiguate opening Files and selected document when necessary.
- [x] Test measurement/variant changes with negative controls; render and inspect representative screenshots so a new selector cannot certify invisible or wrong content. Maintain all existing rejection and evidence checks.
- [x] Run `cargo test --locked -p ui --lib ux_bench --no-default-features`, `cargo test --locked -p ui --lib widget::scrollable::tests --no-default-features`, `cargo test --locked -p shell --bin shell --no-default-features`, and `python3 scripts/ux-bench/test_suite.py`, plus focused tests introduced by Tasks 1-2. Build the final ux-bench binary and run `python3 scripts/ux-bench/run-suite.py --matrix --out target/ux-bench/fixed-matrix` (use a fresh suffixed directory if it exists). Require all runs pass, no execution errors or automated gaps. Investigate actual failures instead of suppressing them.
- [x] Run Clippy for touched targets; distinguish unrelated pre-existing diagnostics from introduced errors. Update docs with final measured outcomes, remaining native-hardware limit and reproducible commands. Keep original failing reports for comparison.
- [x] Self-review and write report with test logs, final report path, exact file changes and any concerns. No commits.
