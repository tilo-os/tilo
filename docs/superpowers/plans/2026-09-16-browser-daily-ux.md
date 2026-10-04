# Daily Browser UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development task-by-task.

**Goal:** Deliver usable multi-tab everyday browsing and measured three-site load/scroll performance.
**Architecture:** Shared CEF runtime with per-tab routing; Rust workspace owns retained per-tab widgets and common tab chrome.
**Tech Stack:** Rust, C++, pinned CEF152, Vello/wgpu, existing toolkit widgets; Node benchmark harness.
**Spec:** docs/superpowers/specs/2026-09-16-browser-daily-ux-design.md

## Global Constraints

Preserve unrelated edits and prior UX/blocker behavior. Keep sandbox/TLS enabled. Real retained tabs share one profile. Bounded protocol. No GPU transport rewrite.

### Task 1: Real multi-tab runtime and workspace

Files: apps/browser/chromium/src/host.cc and focused new tab-runtime module; ui/src/webview/chromium/{mod.rs,protocol.rs,tests.rs}; ui/src/webview/mod.rs; apps/browser/src/{main.rs,lib.rs,browser_widget.rs}; new apps/browser/src/browser_tabs.rs and native tab fixtures. Reuse ui/src/widget/tabs.rs interfaces; small toolkit extensions only if needed.

Interfaces: ChromiumEngine::new retains existing single-engine API while a shared runtime can create per-tab endpoints; explicit tab identity in new protocol traffic, legacy default tab remains supported for existing fixtures. Workspace stores retained BrowserWidget per tab and forwards Widget/AppWidget behavior to active tab. Native popup/new-tab events allocate routes correctly.

- [ ] Write failing tests for two live tabs, routed events/frames, close isolation, shared storage, hidden/visible lifecycle and endpoint disconnect.
- [ ] Implement shared native context/client registry and Rust transport ownership/routing with limits and deterministic cleanup. Preserve legacy single-tab fixture behavior.
- [ ] Add tab bar/create/switch/close/reopen/reorder and shortcuts; retain page state, independent histories/scroll/loading, shared storage/settings.
- [ ] Route target=_blank and middle/platform-modifier click to new tabs; expose loading per tab and page. Test real fixture pages.
- [ ] Run affected protocol/browser/native tests; build actual app, report/commit and scoped review.

### Task 2: Everyday interaction gaps

Files: focused new browser UX modules plus browser_widget.rs/main.rs/native handlers as needed; tests alongside flows.

- [ ] Exercise the spec checklist against Task1 app and record concrete gaps.
- [ ] Test and implement find next/previous/count, zoom/reset, missing context menus/shortcuts, downloads opening/reveal and error/retry as needed.
- [ ] Verify existing clipboard/upload/bookmarks/history/permission flows; fix actual failures.
- [ ] Test layout/accessibility/reduced motion/idle behavior; report/commit and scoped review.

### Task 3: Three-site benchmark (parent independent ownership)

Files: scripts/browser/benchmark-pages.mjs, tests and benchmark documentation/results.

- [ ] Test metric aggregation, invalid/no-scroll samples and bounded protocol parsing.
- [ ] Implement isolated-profile real CEF navigation/scroll workload with optional loopback CDP measurement, exact configuration and raw output.
- [ ] Run Wikipedia/Hacker News/YouTube, inspect evidence, report invalid workloads honestly and choose documented usable public workload if required.
- [ ] Save results and summary, explicitly distinguish engine delivery and full UI presentation; review harness.

### Task 4: Final live five-minute audit

- [ ] Use actual browser for tabs/links/history/find/zoom/bookmarks/downloads and real pages; record/fix concrete gaps.
- [ ] Final scoped review across this goal's changes; verify affected fixes, package runnable app without disrupting existing browser session.
- [ ] Complete only when required work is verified; report remaining limitations explicitly.
