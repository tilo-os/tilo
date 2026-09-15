# UX testing bench implementation plan

**Goal:** Measure visible desktop journeys, scrolling, spacing, and contrast with inspectable evidence.

**Architecture:** Extend the existing UI driver with a scenario runner, use the native shell tree in an isolated fixture profile, and produce JSON plus HTML reports. Action resources identify available operations; input steps measure the visible route. Explicit assertions determine success.

**Spec:** `docs/superpowers/audits/2026-09-15-desktop-ux-bench-review.md`.

**Authorization:** User requested implementation and review of results afterward. Work in the existing `codex/macos-shell` worktree; preserve unrelated changes. Execute inline.

## Tasks

- [x] Add `ui::ux_bench` scenario/check/report types and rejection tests for missing outcomes, invalid budgets, missing targets, spacing and contrast failures. Use strict JSON parsing and fail incomplete runs.
- [x] Execute saved input steps through `headless::Driver`; export checkpoint PNGs, inspection trees, available resource actions, interaction counters, and bounded outcome assertions. Keep direct invocation separate from pointer/keyboard cost.
- [x] Add physical-pixel wheel injection and configurable input scale to the driver. Reproduce nested-wheel and scale defects before fixing the shared Scrollable route.
- [x] Add a macOS `ux-bench` executable that constructs `compositor::macos::prepare` with isolated profile/files directories and runs the scenario. Add readable HTML output with failures and linked evidence.
- [x] Run real desktop scenarios for launch, Files, workspaces, and settings. Add explicit geometry/contrast probes and scroll outcomes based on observed targets. Preserve failures rather than widening budgets or dropping assertions.
- [x] Test malformed scenarios and deliberately failing measurements, run focused Rust tests and the built bench, inspect report screenshots, document commands and native-input coverage limits.

## Validation commands

`cargo test --locked -p ui --lib ux_bench --no-default-features`

`cargo test --locked -p shell --bin shell --no-default-features`

`cargo build --locked -p shell --bin ux-bench --no-default-features`

`target/debug/ux-bench --scenario scripts/ux-bench/desktop.json --out target/ux-bench/desktop`

Per-journey runs must report input failure and outcome failure distinctly, retain screenshots, and exit nonzero on any failed required check. Native hardware gestures cannot be certified by the headless run.

Results and limitations: `docs/superpowers/audits/2026-09-15-ux-bench-results.md`.
