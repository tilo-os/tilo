# Validation protocol

Terra froze the repaired tests; review-freeze-2.sha256 preserves that source.
No Rust/WGSL validation command has run yet. Source remains frozen throughout
validation, compilation and runtime.

The prepared validation plan has 20 serial commands: formatting/diff, strict
production Clippy, strict vendor-test Clippy, changed-line/compiler diagnostics,
then the 16 commands in runtime-validation-plan-v2.json. The initial runtime
plan is preserved; v2 adds an explicit exact-name invocation of the ignored
native-4K shared-coarse P4/P8 proof before any command has executed. Its ignored
appearance in broad suites is not counted as a pass. Changed-line ranges and
the full 33-input source manifest are generated after freeze for each version.
Test outputs and failed attempts are preserved, never replaced by reruns.

Both default and opt-in fixtures must exercise actual selection. Shared-coarse
P4/P8 exact comparisons isolate fine output from independent coarse ordering.
The original independent native-4K retained/dense oracle runs separately in
D, B, P and BP. Existing XRGB staging/cursor/history and UI offscreen checks
remain required. An unavailable adapter or skipped required GPU fixture is
not a pass. All GPU work runs alone, with the ordinary recovery limits intact.

Only after reviewed source, all required tests/lint, unchanged hashes and an
ordinary release build may comparison-plan-v2.json execute. Before timing,
verify both copied binary hashes, all unused log paths, parser fixtures,
actual requested/available/selected-counter expectations, and no other jobs.

Any source fix invalidates the old freeze for the next validation/build.
Do not conflate a source version's preserved evidence with a newer source tree.
No default promotion or goal completion follows from diagnostics alone.
