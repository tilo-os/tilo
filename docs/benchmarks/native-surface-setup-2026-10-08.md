# Native surface setup, 2026-10-08

Native window creation configured a swapchain before adjusting alpha and configuring it again inside the validation error scope. Build the surface and targets first, then configure once in that existing scope. The public configured-surface constructor retains its contract. Reused devices also retain their capability-query result rather than discarding it through `is_surface_supported` and querying again. Fresh adapter selection, explicit adapter preferences, format selection and device-loss callbacks remain on their existing paths.

## Measured result

An optimized diagnostic exercises the production SDK constructors, actual Wayland/Vulkan surface acquisition, a clear submission, GPU polling and validation. Baseline performs the native window's earlier configuration and checked configuration. Candidate performs only the checked configuration. An owned Weston 15.0.1 pixman/headless display ran in the remote build container with private profiles, caches and a short 0700 runtime directory. It was terminated and removed afterward. The adapter was Mesa llvmpipe/LLVM 22.1.8; every sample selected Vulkan/Rgba8Unorm and one device.

48 alternating processes gave 12 samples per variant and device condition:

| Condition | Baseline median | Candidate median | Interpretation |
|---|---:|---:|---|
| Reused device, native setup | 2.069 ms | 1.323 ms |36.1% lower observed median|
| Fresh device, native setup | 40.982 ms | 41.978 ms |No reliable improvement; overlapping 38–74 ms ranges|

Fresh-device means were 43.837 and 43.733 ms. No full-app or OS startup speedup is established by this fixture. All 48 submissions passed validation. Every sample recorded the expected checked configuration; the candidate omitted the earlier unchecked configuration. Both variants refused a deliberately nonexistent named adapter. Explicit SDK counters do not observe every upstream Vulkan query.

Raw events, artifact hashes, source blobs, host load and limits are in [the JSON receipt](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/benchmarks/native-surface-setup-2026-10-08.json). Baseline source is `cec43c3558437dabe21a82ca8a01eaa18a9740f5`. The measured probe used `std::time::Instant::now`; the submitted diagnostic uses the required `tilo_ui::time::now()` wrapper, which delegates to that same native clock. Receipt source blobs describe the measured artifact. Public probe artifacts remained remote; private QA profiles, identity data and VM receipts are excluded.

## Controls and limits

An earlier 16-process experiment selected the preferred Vulkan adapter without a surface, then retained its capability result. It did not improve fresh-device setup and is omitted from the final source. An initial compositor fixture failed before sampling because its Unix socket path exceeded 108 bytes; the failure was retained and a short private runtime directory fixed fixture setup. A test invocation with an unsupported wrapper flag was rejected before running tests. No assertion was weakened and no failing test was ignored.

This experiment uses software Vulkan on native remote compute, not a Tilo VM or physical display. It does not establish steady frame rate, input latency, matched app memory, idle CPU or full-machine boot gains. Existing VM display-path failures are not performance samples. Newly separated adapter-request, adapter-report, device-request and capability phases support the next functioning integrated-cohort trace.

## Reproduction

Build `startup-surface-probe` with the configured Cargo implementation:

```sh
${CARGO:-cargo} build --release --locked -p tilo-ui --example startup-surface-probe
```

Run baseline/candidate artifacts alternately against an owned headless Wayland display with private `XDG_RUNTIME_DIR`, `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and Mesa cache paths. Set `WGPU_BACKEND=vulkan`, `WINIT_UNIX_BACKEND=wayland` and an output path in `TILO_STARTUP_METRICS_PATH`. `--reuse-device` opens the device before window construction. Require successful actual acquisition/submission and validation, then repeat with a nonexistent `WGPU_ADAPTER_NAME` and require failure. Never point this diagnostic at the user's real desktop.
