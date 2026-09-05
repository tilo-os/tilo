# Retained shell damage: measurement protocol

Recorded before running the release measurements for this checkpoint.

- Source base: `3e03f98fd2b98ce9c86e1013126ae916bc3cbc81`, plus this checkpoint's retained-shell changes.
- Build: `cargo build -p ui -p shell --release --example ui-perf --example redraw_bench --locked`.
- Keep the repository's ordinary release profile and target configuration. No RUSTFLAGS or release-profile overrides.
- Shell: native 3840×2160, exactly 1,000 required action-triggered redraws, unchanged alternating input targets and ordinary animation policy.
- The action stream is the same across versions, but wall-clock animations can naturally sample different phases when rendering gets faster. This is an end-to-end action benchmark, not an identical pre-recorded scene replay. The separate pixel oracles compare the same candidate scene against full rendering.
- Time the complete harness path: target lookup, input dispatch, layout, scene recording, rendering, completion/readback, and full XRGB destination output.
- Keep every slow frame and full/recovery submission. Incomplete runs fail; the one-second fence deadline is unchanged.
- Each measurement process has a predeclared 15-minute wall-clock cap (then TERM, KILL after another 30 seconds) to bound a stuck driver submission. Hitting that cap is a failed/incomplete run, never a sample exclusion or acceptance pass.
- Acceptance limits remain mean ≤8,000 us and p99 ≤30,000 us. Diagnostic profiling cannot produce an acceptance pass.
- Perform one fresh unprofiled control with the preserved previous `3e03f98fd` shell binary, then three unprofiled candidate runs. Run one separate candidate diagnostic for phase and copy-payload accounting.
- Run three default candidate UI redraw benchmarks at native 4K/1,000 redraws, plus the existing 4K pixel preflight and one-million frozen/idle event check.
- Do not overlap performance measurements with builds, correctness tests, or other benchmarks.
- The host has reported Vulkan llvmpipe (CPU/software rendering), not a physical 2010 laptop. No KMS page flip or display scanout is timed.
- Texture-to-staging copy payload is an API-level byte count, not a measurement of PCIe traffic, physical GPU memory, or hidden driver copies. Full CPU XRGB destination writes remain intentional for scanout buffer-age/cursor safety.

Correctness and failed intermediate checks are retained in [validation.txt](validation.txt).
Executable hashes and build settings are in [artifacts.md](artifacts.md); full
commands and exit codes are in each named `.txt` log. See the
[performance report](../../ui-performance-benchmark.md#default-release-measurements-and-retained-failures)
for the result tables. Build and changed-line lint follow-ups are in
[build-and-lint-followup.txt](build-and-lint-followup.txt).
The final source-frozen tests, strict clippy, and formatting checks are in
[final-validation.txt](final-validation.txt).
Log text preserves commands, outcomes, and measurements; trailing whitespace
and extra blank lines at EOF are normalized for repository whitespace checks.

The original shell control and three candidate acceptance trials are incomplete
fence-timeout failures. All three completed UI redraw trials miss the 30 ms p99
gate. Later completed diagnostics/repeats do not erase those failures. The
million-event runs are frozen/idle events with zero redraws, not a million
rendered frames. Neither the 8 ms shell mean nor constant 60 FPS is established.
## Bounded post-drift follow-up

The original control and all three candidate acceptance attempts remain failed/incomplete. A later same-binary diagnostic completed all 1,000 frames but missed both latency targets. After the four separate-process debug cold-scene probes and one opt-in sparse-coarse diagnostic, run this fixed unprofiled sequence: candidate, previous 3e03 control, candidate, candidate. Keep ordinary defaults, the same 1,000-action native-4K workload, every failure, and the same 15-minute process cap/one-second fence deadline. These are additional observations, not replacements for the original failed trials. No builds/tests overlap.
