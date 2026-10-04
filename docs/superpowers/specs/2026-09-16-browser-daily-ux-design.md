# Daily browser UX and performance

## User outcome

The user requests a usable everyday browser: hold Back history, visible loading when opening tabs, smoother interaction, real multi-tab support, and a repeatable Wikipedia/Hacker News/YouTube load/scroll benchmark. Acceptance is an actual five-minute browsing exercise that finds and fixes missing basic interactions. The user has explicitly requested implementation; continue without extra approval checkpoints.

## Architecture

Keep Rust/Vello browser chrome and pinned sandboxed CEF. Real tabs must retain separate live documents, history, scroll and loading state while sharing the same persistent Chromium profile/cookies. Use one helper with multiple CEF browsers/request contexts sharing global storage; do not spawn competing helpers on one locked profile or simulate tabs by replacing a single URL. Rust tab engines share one process transport with explicit tab identities and per-tab events/frames. Closing one tab does not close others. Hidden tabs suppress unnecessary painting while keeping their page state/audio alive. All routing/allocation remains bounded.

A browser workspace owns a visible tab bar and per-tab BrowserWidget state. Reuse toolkit tab/menu primitives where practical. Create/switch/close/reorder/reopen tabs; standard platform shortcuts; middle/Cmd-or-Ctrl-click links and target=_blank create tabs. Keep active tab address/title/history/find/zoom state separate. Each loading tab shows feedback in its tab and page controls. Last-tab behavior creates an empty new tab rather than terminating unexpectedly. Shared settings/bookmarks remain coherent; no silent loss of profile data.

## Everyday acceptance checklist

- Tab creation, switching, close/reopen, reordering, independent history/scroll, background page lifetime.
- Back/Forward, hold/right-click/keyboard history, reload/stop, address selection, search and suggestions, error/retry.
- Find in page with next/previous/count, zoom in/out/reset, common keyboard shortcuts and escape behavior.
- Link/page context menus, open in new/background tab, copy link/page text, file upload and ordinary downloads.
- Bookmark add/remove/library, history/library, downloads status/open/reveal where supported, site-permission decisions.
- Window resizing, readable compact layout, focus/accessibility, reduced motion, no perpetual idle redraw.
- Verify ordinary YouTube playback/fullscreen where the pinned runtime supports it; report actual unsupported media/DRM rather than claiming compatibility.

This checklist operationalizes the user's five-minute requirement. No claim of every feature of every browser; findings during the hands-on pass extend the repair list.

## Benchmark

Repeatable isolated-profile runs on English Wikipedia main page, Hacker News front page, and YouTube homepage. Record exact URLs/final URLs, titles/status, runtime/build/machine/viewport, blocking configuration, fresh-profile versus warm run conditions. Measure navigation timing and actual observed scroll response/frame pacing. Verify nonzero scroll movement; consent/interstitial/empty pages are invalid scroll workloads and must be reported, never converted to successful fast samples. Include raw samples, medians/p95 where sample sizes justify them and a readable summary. Distinguish CEF CPU frame delivery from full Rust UI presentation; no FPS or speed claims beyond measured scope. Keep sandbox/TLS enabled and debugging isolated to temporary test profiles/loopback.

## Constraints

Preserve unrelated concurrent changes, existing built-in blocking, reviewed bounded IPC, and prior UX commit adc78a4be. No GPU-sharing rewrite in this goal. Test changes and obtain scoped review before claiming completion.
