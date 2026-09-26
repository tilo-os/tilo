# Chromium Browser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Replace simulated/default browsing with a Chromium engine on macOS and Linux, with native blocking and verified navigation to tilo.so.

**Architecture:** Native CEF process communicates with Rust WebViewEngine over framed pipes. Native blocking uses a Rust static library linked into the helper; CEF distributions are independently cached.

**Tech Stack:** Rust, C++, Objective-C++, CEF, adblock-rust, existing Vello/wgpu UI.

**Spec:** docs/superpowers/specs/2026-09-16-chromium-browser-design.md

## Global Constraints

- Preserve unrelated worktree edits; commit only task-owned files or hunks.
- macOS and Linux are required. No silent test-engine fallback, sandbox disabling or certificate bypass.
- Keep Chrome extension support in scope; built-in blocking is independent.
- Full completion includes actual Rust browser navigation to https://tilo.so.
- Use pinned CEF SDK distributions; ordinary Rust builds must not compile Chromium.

## IPC contract shared by Tasks 1 and 2

Each packet is `u32 little-endian payload length`, then a one-byte kind, then
payload. Length includes kind. Kind 1 is UTF-8 JSON; kind 2 is a frame:
`u32 width, u32 height, width*height*4 BGRA bytes`. Maximum packet 64 MiB,
maximum dimension 8192, checked multiplication and exact lengths. JSON commands
have an `op` field; events an `event` field. Standard output is IPC only;
diagnostics go to stderr. stdin EOF closes the browser and exits.

Commands: `navigate {url}`, `back`, `forward`, `reload`, `stop`,
`resize {width,height,scale_factor}`, `focus {focused}`,
`mouse_move {x,y}`, `mouse_button {button,down,x,y}` (left/middle/right),
`scroll {x,y,dx,dy}`, `key {key,down,ctrl,shift,alt,meta}`,
`scripts {scripts:[string]}`, `filter_rules {rules:string,enabled:bool|null} (null preserves enabled state)`,
`clear_data`, `devtools`, `shutdown`.

Events: `ready {cef_version,chromium_version}`, `uri {url}`, `title {title}`,
`progress {progress}`, `loaded {url,status}`, `error {url,message}`,
`history {back,forward}`, `closed`. Future extensions are versioned; unknown
events are ignored and malformed packets terminate the connection.

### Task 1: Native CEF host and reproducible runtime build

**Files:** Create apps/browser/chromium/{CMakeLists.txt,src/*,resources/*,README.md}; create scripts/browser/{cef-lock.json,build-host.py}; native protocol tests under apps/browser/chromium/tests/.

**Interfaces:** Consumes the IPC contract above. Produces target/chromium/Browser Engine.app/Contents/MacOS/tilo-chromium-host on macOS, target/chromium/tilo-chromium-host on Linux. Host accepts --profile=<absolute directory>. Filtering C ABI will be supplied by Task 2; native runtime may be built initially without that linkage only for explicit development validation, never treated as complete blocking.

- [ ] Write and run protocol tests for truncated headers, oversized packets, frame bounds and valid messages before implementing the codec.
- [ ] Pin stable CEF archives with digest verification for macOS arm64/x64 and Linux arm64/x64; download atomically and fail closed on mismatch.
- [ ] Implement framed IO, CEF offscreen browser lifetime, events, input and navigation using CEF's documented platform entrypoints. stdin reader posts commands onto CEF UI thread; callbacks never print to stdout.
- [ ] Package framework/resources and sandboxed subprocesses using upstream CEF sample layouts. Implement CEF render-process scripts without exposing privileged bindings to websites.
- [ ] Build and run host against an HTTPS page; verify ready, load and nonempty frame. Record exact command and result. Commit owned files and report remaining capabilities honestly.

### Task 2: Rust IPC engine and native blocking

**Files:** Create ui/src/webview/chromium/{mod.rs,protocol.rs,tests.rs}; modify ui/src/webview/mod.rs; add crates/browser/chromium-blocker/{Cargo.toml,src/lib.rs}; modify workspace manifest by minimal hunk; extend native host with filtering bridge.

**Interfaces:** Produces ChromiumEngine::new() -> Result<Arc<Self>, String> implementing WebViewEngine. Runtime locator uses TILO_CHROMIUM_HOST override or packaged/repo target paths. C ABI filtering has opaque owned state and synchronous network decisions; never depends on UI IPC for each network request.

- [ ] Test IPC parser with malformed lengths, BGRA conversion, URI/title events and helper EOF. Verify failure before implementation.
- [ ] Implement bounded channels, a dedicated writer and event/frame reader, host lifetime cleanup, and visible failures. Maintain engine waker and latest-frame mailbox.
- [ ] Test native blocker against domain/type/third-party rules and exceptions; implement adblock-rust-backed opaque C ABI with defined string ownership and panic containment.
- [ ] Wire CEF resource interception to native matcher; supply built-in/cached rules before first web request. Add cosmetic resource queries and trusted document-start scripts.
- [ ] Run focused Rust/native tests, commit owned changes and report.

### Task 3: Browser integration and everyday capabilities

**Files:** Modify apps/browser/src/{browser_widget.rs,blocker.rs,main.rs,extensions.rs,settings.rs}; add focused integration modules/tests and update manifests. Extend native host as required.

**Interfaces:** Consumes ChromiumEngine, preserves BrowserWidget public constructors and explicit new_headless TestEngine behavior. Add engine-neutral commands/events for download, permissions, media/PiP and extensions without placing CEF dependencies in ui's default build.

- [ ] Add failing integration tests for live default selection, no simulated fallback, filter readiness and settings behavior.
- [ ] Select Chromium for live browsing; show startup error state when unavailable. Preserve deterministic shots.
- [ ] Implement downloads, permission decisions, clipboard/text input, developer tools, persistent profile/session and clear-on-exit behavior.
- [ ] Implement and validate MV3 service worker/storage/content-script/action surfaces; manage installation and permissions explicitly.
- [ ] Preserve existing history, bookmarks, search and Tilo actions; run browser regression tests and native fixture checks.

### Task 4: GPU transport, packaging and platform qualification

**Files:** Extend apps/browser/chromium, ui/src/webview/chromium, platform build scripts and packaging definitions.

- [ ] Add platform texture ownership/lifetime tests and bounded transport handling.
- [ ] Implement client-owned GPU copies/import with synchronization on macOS and Linux; retain identified CPU fallback.
- [ ] Package/sign local macOS bundle and Linux distribution without weakening sandboxing. Document runtime upgrade and cache invalidation.
- [ ] Run platform builds and fixture suites; report unavailable environment evidence as outstanding, never as passed.

### Task 5: End-to-end verification and review

**Files:** Add scripts/browser/verify-live.py and docs/superpowers/audits/2026-09-16-chromium-browser-verification.md.

- [ ] Launch the actual browser, navigate to https://tilo.so, inspect the rendered window, and record final URL/title/load result/frame evidence.
- [ ] Run native blocker and browser capability fixtures; measure build/startup/frame delivery/memory with environment recorded.
- [ ] Request independent task and final reviews, fix actionable findings, and rerun affected checks.
- [ ] Audit every spec requirement against direct evidence. Only complete the goal when no required work remains.
