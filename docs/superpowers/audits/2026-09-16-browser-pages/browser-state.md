# Browser implementation overview — 16 September 2026

## Technology

The browser uses a Rust application shell with Vello/wgpu rendering, winit window/input handling and Taffy layout. Web content runs in a separate C++ Chromium Embedded Framework helper. The SDK is pinned to CEF 152.0.6+g708dc14, Chromium 152.0.7977.83. This embeds Chromium; it does not build or maintain a complete Chromium source fork.

Built-in ad and tracker blocking uses adblock-rust 0.12.1 and filter resources. Native request filtering and document cosmetic/scriptlet integration are part of the browser, so an extension is not required. Sandbox and TLS validation remain enabled.

Tabs share one helper and profile, with independent CEF browser instances and explicit tab IDs. Inactive tabs retain their documents while suppressing unnecessary painting. The current limit is 32 live tabs; closed URLs can be reopened. Content reaches the Rust renderer as CPU image frames. GPU texture sharing is not implemented.

## Current state

Real tabs, back/forward history, loading/stop feedback, bookmarks/history/download libraries and browser settings are implemented. The daily-controls checkpoint adds find/count/next/previous, zoom, bookmark shortcuts, completed-download Open/Reveal and accessible New/Close tab actions. Unit and native tests pass for that checkpoint. Live checks now cover page context menus, a readable default page background, page focus and clipboard, native file selection, Find, bookmarks, download Open/Reveal and YouTube playback. Final optimized-preview checks passed for Find reflow, immediate tab typing, loading/Stop and download close/cancel behavior. Active downloads keep their source tab open until completion or cancellation; records are shared across tabs. See the [daily-use audit](../2026-09-16-browser-daily-ux.md). The code is a working development browser, not yet a fully qualified daily-driver release.

## Build time

Recorded incremental builds on this development machine:

- Optimized `thin-app` browser build: 131.25 s, including rebuilt rendering dependencies; final corrective rebuild: 1 min 59 s.
- Native CEF helper change: 3.07 s.
- Rust daily-controls checkpoint: 17.28 s.
- Earlier Rust tab-fix build: 64.53 s including build-lock waits.

These are incremental observations, not clean-build estimates. Concurrent development and shared Cargo locks affect wall time. No defensible clean build measurement is available in this report. The pinned prebuilt CEF SDK download/unpack is a separate setup cost.

## Performance

See README.md and results.json in this directory for all three-site timings and raw samples. All 9 latest pages loaded with HTTP 200 and actual scroll movement. The controlled local experiment delivered 59–60 CPU frames/s after changing the offscreen cap from 30 to 60, at the same input cadence.

The benchmark does not measure the Rust window's complete input-to-screen pipeline. YouTube uses scrollable search results, not video playback. Live-network load times vary, and an intermittent cookie-store startup delay remains unexplained; successful later runs do not prove it fixed. The daily-use audit records separate UI and limited media checks; no full-window latency or broad codec/DRM coverage is claimed.
