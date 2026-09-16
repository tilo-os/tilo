# Offscreen frame-rate comparison

A local 7040px-tall static fixture removes network and site-content variability from the scroll phase. Both configurations receive identical 60 wheel inputs at 16 ms intervals, reversing after 30 steps. Three runs use the same viewport and blocking snapshot. The only native change is the offscreen frame-rate cap; its value is now included in ready metadata.

| Native cap | Delivered FPS, three runs | Median frame intervals |
|---|---|---|
|30 FPS|30.28 /30.04 /29.91|33.42 /33.30 /33.54ms|
|60 FPS|58.92 /58.88 /60.05|16.86 /16.98 /16.66ms|

This supports raising the default cap to 60 FPS. Rates are received CPU-frame intervals, not full Rust/Vello presentation. More frequent frames can increase active rendering/copying work; inactive tabs still suppress painting. No GPU transport changes. The 60.05 FPS sample reflects timing jitter over a short interval, not a higher configured cap.

Raw timestamps and configuration are retained in scroll-cap-30.json and scroll-cap-60.json. The experiment used a copy of benchmark-pages.mjs with its input delay changed from 33 to 16 ms; the committed harness now uses 16 ms and records it explicitly. Earlier three-site results retain their original 33 ms input cadence and 30 FPS cap.

Native incremental build: 3.07 s wall time; CTest protocol 1/1 passed. This is not a clean Chromium source build: the project links the pinned prebuilt CEF SDK. Eight benchmark tests pass after the cadence change. Full UI smoothness remains a separate live acceptance check.
