# Owned compositor direct-target GPU diagnostic

This opt-in native diagnostic compares three warmed solid-fill renders with actual
mounted Quick Settings/Overview pointer transitions at 2944×1840, using the
fixture's fixed clock, dark theme and reduced motion. Pointer events are ordinary
widget dispatch from an owned synthetic fixture, not physical input evidence. It uses the
production `VelloDrmBridge::paint_imported_until`, its existing exact submission
completion wait, and its final BGRA output blit. No production behavior changes.
The actual floating-material BackdropFrame is passed to both measured renderer
and dense oracle; it is never omitted to manufacture complete query coverage.
All scene/document state belongs to the test fixture; no desktop is contacted.

From the candidate checkout, run the configured native builder normally:

```sh
rcargo --no-fallback test \
  --locked -p shell --lib --profile thin-app --features direct-scene-diagnostic \
  --config 'profile.thin-app.package.shell.codegen-units=16' \
  --config 'profile.thin-app.package.tilo-ui.debug-assertions=true' \
  --config 'profile.thin-app.package.terminal.debug-assertions=true' \
  owned_qs_overview_and_solid_direct_target_diagnostic -- --nocapture
```

Shell uses sixteen codegen units to bound the optimized diagnostic build; this
explicit override differs from the default thin-app/deployed Shell profile.
The opt3 `thin-app` profile disables LTO and keeps Shell/Vello debug assertions
off. Two downstream fixture dependencies explicitly retain debug assertions:
existing Shell tests call toolkit/Terminal test APIs gated by that setting.
Consequently this is not a deployed-equivalent UI CPU benchmark. Preserve the
exact invocation, source commit/dirty diff, toolchain and `DIRECT-SCENE` rows.
The rows identify the actual adapter/backend/driver and distinguish software
renderers. No speed threshold qualifies the test as physical performance.

Every measured transaction reports actual render attempts, admitted damage,
CPU submission/completion/settlement, exact renderer generation/recording range,
and GPU recording envelope with its **nested** final-output interval. Never add
nested GPU intervals, or sum GPU time with overlapping CPU wait. The envelope
excludes pre-execution queue/relay waits and query readback copies. External
backdrop submissions make whole-frame GPU elapsed unavailable, while exact scoped
recording and nested output timings remain reported. Unsupported queries, missing
samples, stale/out-of-range samples or an empty bounded observation fail required
observation. External-work frames can expose a strictly partial recording range;
its measured count is explicit and never represents missing recordings. Without
external work, incomplete recording coverage still fails.

After each measured completion, a separate dense direct render and both pixel
readbacks verify output parity with the existing bounded independent-recording
rounding tolerance. Those oracle operations are outside reported timing. The
owned BGRA texture is not a real imported DMA buffer, DRM scanout or WSI surface:
producer import fences, physical page flips and KMS retirement are unmeasured.
Scene-dependent differences on this builder can motivate a hardware experiment;
they cannot establish the cause of Yoga's render duration or input latency.
