# Browser implementation overview — 16 September 2026

## Technology

The browser uses a Rust application shell with Vello/wgpu rendering, winit window/input handling and Taffy layout. Web content runs in a separate C++ Chromium Embedded Framework helper. The SDK is pinned to CEF152.0.6+g708dc14, Chromium152.0.7977.83. This embeds Chromium; it does not build or maintain a complete Chromium source fork.

Built-in ad and tracker blocking uses adblock-rust0.12.1 and filter resources. Native request filtering and document cosmetic/scriptlet integration are part of the browser, so an extension is not required. Sandbox and TLS validation remain enabled.

Tabs share one helper and profile, with independent CEF browser instances and explicit tab IDs. Inactive tabs retain their documents while suppressing unnecessary painting. The current limit is32 live tabs; closed URLs can be reopened. Content reaches the Rust renderer as CPU image frames. GPU texture sharing is not implemented.

## Current state

Real tabs, back/forward history, loading/stop feedback, bookmarks/history/download libraries and browser settings are implemented. The daily-controls checkpoint adds find/count/next/previous, zoom, bookmark shortcuts, completed-download Open/Reveal and accessible New/Close tab actions. Unit and native tests pass for that checkpoint. Full live usability acceptance is still in progress: context menus, default page background, focus and find/zoom synchronization have concrete open repairs. The code is a working development browser, not yet a fully qualified daily-driver release.

## Build time

Recorded incremental builds on this development machine:

- Native CEF helper change:3.07s.
- Rust daily-controls checkpoint:17.28s.
- Earlier Rust tab-fix build:64.53s including build-lock waits.

These are incremental observations, not clean-build estimates. Concurrent development and shared Cargo locks affect wall time. No defensible clean build measurement is available in this report. The pinned prebuilt CEF SDK download/unpack is a separate setup cost.

## Performance

See README.md and results.json in this directory for all three-site timings and raw samples. All9 latest pages loaded withHTTP200 and actual scroll movement. The controlled local experiment delivered59–60 CPU frames/s after changing the offscreen cap from30 to60, at the same input cadence.

The benchmark does not measure the Rust window's complete input-to-screen pipeline. YouTube uses scrollable search results, not video playback. Live-network load times vary, and an intermittent cookie-store startup delay remains unexplained; successful later runs do not prove it fixed. Full UI smoothness and media acceptance remain separate audit work.
