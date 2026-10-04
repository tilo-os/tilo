# Complete Browser UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Implement all ten requirements in the complete-browser UX specification.

**Architecture:** Extend the existing Rust browser and asynchronous CEF protocol in sequential, reviewable subsystems. Keep browser-only visual behavior isolated from shared toolkit consumers. Persist normal state atomically and separate ephemeral/private state at the native profile boundary.

**Tech Stack:** Rust, Vello/wgpu/winit/Taffy, C++ CEF 152, macOS Keychain, Python native fixtures, Node benchmark harness.

**Spec:** docs/superpowers/specs/2026-09-16-browser-complete-ux-design.md

## Global Constraints

- Implement the full ten-requirement scope; no reduced success definition.
- Keep sandbox, TLS verification and encrypted storage enabled.
- Tests use isolated profiles and synthetic data; leave the user's original browser/profile untouched.
- Keep data and protocol messages bounded. Validate origin-sensitive actions at execution time.
- One implementation worker at a time for shared browser/native files; parent owns integration evidence and packaging.

## Task 1: Symmetric navigation menus

Files: apps/browser/src/back_hold.rs, browser_widget.rs; existing ui/src/webview protocol and native navigation_history.h only if required.

Consumes existing NavigationHistory command/event and NavigateToEntry. Produces direction-aware menu state and the same accessible/keyboard interaction on both buttons.

- [ ] Add failing regressions: with entries A,B,C and current B, Back shows A and Forward shows C; the opposite direction/current entry is excluded. A held Forward opens once and release does not navigate. A stale reply after direction/page change is ignored.
- [ ] Run focused browser tests and record intended failures.
- [ ] Generalize gesture/menu state to carry direction, retaining real entry indices and nearest-first ordering. Route pointer, keyboard and AX activation through the same behavior.
- [ ] Run `cargo test -p browser --lib`, inspect menu rendering, commit only owned files, write report.

## Task 2: Browser tabs and media identity

Files: browser_tabs.rs; new browser_tab_strip.rs; browser_widget.rs; ui/src/webview/{mod.rs,chromium/mod.rs}; native host.cc and protocol headers for favicon/audio events.

Consumes independent tab endpoints and close admission. Produces pinned state and tab descriptors needed by recovery; media events are bounded and tied to tab identity.

- [ ] Add failing tests for pin partition/order, duplicate URL, close-others protection, mute commands and overflow keeping selected tab visible.
- [ ] Implement browser-specific horizontal strip with spec controls, favicon loading with validation/fallback, audio state and all menu actions.
- [ ] Verify native favicon/audio updates on fixture pages, AX actions, drag/close/new-tab hit targets at 800px and 32 tabs; regression-test shared TabBar consumers unchanged.
- [ ] Run browser/protocol tests once, commit owned changes and report.

## Task 3: Session recovery

Files: new apps/browser/src/session.rs; browser_tabs.rs, main.rs, lib.rs.

Consumes Task 2 tab URL/order/pin state. Produces versioned bounded session snapshot and dirty/clean shutdown marker; private sessions are excluded by explicit policy.

- [ ] Add failing disk roundtrip/atomic replacement/corrupt-data tests using temp directories, plus active/order/pin restoration and unsafe/private exclusion.
- [ ] Implement durable snapshot after meaningful tab/navigation changes, clean-exit marker and crash recovery UI. Restore active tab first and load background pages on activation.
- [ ] Verify two process launches and a forced fixture crash using an isolated profile; check no duplicate or lost tabs.
- [ ] Run focused then browser tests, commit/report.

## Task 4: Per-site blocking and site information

Files: new site_controls.rs; settings.rs, native_blocker.rs, browser_widget.rs; native request_filter.h, capabilities.h, host.cc; WebView protocol.

Consumes committed origin and engine callbacks. Produces bounded site status, per-site exceptions, request counts, filter update status, certificate/permission information and origin-scoped data-clear commands.

- [ ] Add failing origin canonicalization/exception persistence/count reset/stale action tests. Native fixture must prove toggling one origin affects network and cosmetics while another origin remains blocked.
- [ ] Add shield/site-info popovers and actual native information; bind permission changes and data deletion to validated displayed origin.
- [ ] Verify filter update success/failure/retry, TLS info, permission revocation and site-specific cookie/storage clearing with isolated fixtures.
- [ ] Run relevant browser/native/protocol tests, commit/report.

## Task 5: Private windows

Files: main.rs, browser_tabs.rs, browser_widget.rs, session.rs, history.rs, native engine launch/profile code.

Consumes normal recovery policy; produces explicit browsing-mode policy shared by history, recovery, settings and credential flows.

- [ ] Add failing tests proving private tabs cannot write normal history/session and normal/private cookies are isolated.
- [ ] Implement New Private Window, ephemeral native context/runtime, private chrome indicator and close cleanup. Keep normal history pause independent.
- [ ] Launch normal and private fixture processes, set different cookies/local storage, reopen private and prove absence; inspect private profile cleanup.
- [ ] Run scoped tests, commit/report.

## Task 6: Background downloads

Files: capabilities.rs, browser_tabs.rs, browser_widget.rs; native capabilities.h, host.cc; download_lifetime.py.

Consumes shared download store and native close admission. Produces transfer-owner lifecycle independent from visible tabs, download toolbar status, retry and explicit window-exit choices.

- [ ] Extend failing slow-download fixture: close visible origin tab during transfer, receive complete exact bytes, close hidden owner afterwards. Repeat for cancellation/failure.
- [ ] Implement native ownership separation and bounded cleanup; expose count/progress/completion and retry in chrome. Never retain hidden Rust widgets as a shortcut.
- [ ] Verify close/reopen tabs, complete/cancel/retry and window-close choices live; confirm terminal owner cleanup and no route-capacity leaks.
- [ ] Run browser/native tests, commit/report.

## Task 7: Native fullscreen

Files: browser_widget.rs, browser_tabs.rs; native host callbacks; ui host fullscreen interface where necessary.

Consumes content fullscreen intent; produces window fullscreen state and chrome visibility restoration.

- [ ] Add failing state tests for enter/exit, Escape, tab switch and renderer loss restoring chrome.
- [ ] Connect page fullscreen to native window fullscreen; hide chrome and show temporary exit hint. Keep window state authoritative.
- [ ] Test generated video/page fullscreen in actual window, Escape and tab switches; record screenshots and native state.
- [ ] Run focused regressions, commit/report.

## Task 8: Credentials and basic autofill

Files: new credentials.rs and autofill.rs; browser_widget.rs, settings/library integration; native document/frame bridge and macOS Keychain integration.

Consumes browsing-mode and committed secure-origin policy. Produces explicit save/update/fill/delete operations, keychain-backed secrets and editable non-secret autofill profile.

- [ ] Add failing tests for exact-origin matching, insecure/cross-frame rejection, stale document rejection, private no-save, update/delete and secret-free serialized metadata/logs.
- [ ] Implement asynchronous Keychain adapter and explicit page form capture/fill consent UX; no automatic secret disclosure. Add basic name/address profile editor and user-triggered fill.
- [ ] Verify synthetic login forms on two distinct origins, private mode, multi-account selection and deletion using a test-specific Keychain service. Remove generated test entries afterwards.
- [ ] Run browser/native tests, inspect persisted files for synthetic secret leakage, commit/report.

## Task 9: Startup and end-to-end performance

Files: scripts/browser benchmark tooling, new native-window measurement harness, native initialization and shared profiling hooks only where proven necessary.

Consumes actual runtime telemetry; produces reproducible startup diagnosis/fix and raw input-to-present timing artifacts for Wikipedia, Hacker News and YouTube search.

- [ ] Run repeated isolated cold/warm startup measurements with cookie initialization timing, preserving failures and process/environment data.
- [ ] Diagnose the delayed stage from logs and implement a regression-backed fix without weakening encryption/security.
- [ ] Measure OS-delivered input through actual UI presentation feedback with explicit timestamp domains; report latency distributions, missed frames and idle cadence separately from CEF frames.
- [ ] Re-run after the fix with comparable workload; publish reproducible commands/raw data and remaining limitations. Commit/report.

## Task 10: Full integration and delivery

Files: audits, packaging artifacts and fixes identified by final integration.

- [ ] Review each requirement against authoritative code, test and live evidence; missing/inconclusive items remain open.
- [ ] Run focused regression suites and native fixtures for the final integrated code. Exercise a realistic multi-page browsing session with tabs, recovery, private mode, blocking exceptions, downloads, fullscreen and synthetic autofill.
- [ ] Conduct scoped final code review; repair verified findings, rebuild optimized self-contained preview and verify signatures/embedded helper.
- [ ] Publish measured results and exact limitations, clean only generated fixture data. Mark goal complete only after every requirement is evidenced.
