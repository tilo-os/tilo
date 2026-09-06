# Existing UI tail profile: predeclared diagnostic

Status: all four captures are complete and audited; see RESULTS.md. The unchanged plan below was recorded before execution. No acceptance claim or production change.

Four fixed native-4K, 1,000-action captures: default/PPT8 then PPT8/default, using the already validated and preserved Area-PPT8 v5 ordinary-release UI executable. No Rust/WGSL change or rebuild. Same animated input sequence, layout, damage, warm-up, timer boundaries, recovery and fences. All attempts and failures retained; no rerun-until-pass or outlier removal.

Enable only existing host render timing, frame breakdown, GPU timestamps and dispatch timestamps. Clear acceptance budgets, as this existing diagnostic explicitly rejects them. All prior experiment/cache controls cleared uniformly; PPT8 is the sole renderer variant. Normal cache location/default behavior restored per command (the earlier experiment used only per-process overrides). No user cache is cleared. No other jobs during captures. Timeout remains 15 minutes plus 30-second TERM grace.

Record requested/available/actual PPT8 selection, actual 3840×2160 extent, all 1,000 redraws, timestamp matching coverage/status, aggregate host/GPU stage distributions, the existing ten joined slow-render records, recovery and resource accounting. Missing or stale timestamp coverage must be explicit, not read as zero. The ten records are a render-time tail, not necessarily the total-frame-time p99 cohort. No per-frame coarse/fine arrays are currently printed; aggregate quantiles cannot establish which stage caused a specific total-frame outlier.

Every capture is diagnostic-only and cannot establish the performance goal. Do not equate GPU timestamp span with exclusive compute or host settle wait with GPU-copy time. Compare same-run host boundaries and matched GPU timestamps cautiously. Source and binary hashes must match before/after; raw output and exit codes are retained.
