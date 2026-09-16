# Browser daily-use audit — 16 September 2026

## Environment and method

macOS ARM64, pinned CEF 152, built-in blocking enabled. Tested a separate Browser UX app with its own Chromium profile and browsing history paused. The original running browser was left untouched. Local fixtures contain generated text and files only; the file-selection check does not upload data to an external service. Public YouTube playback was checked separately.

The audit used actual native UI actions, screenshots/accessibility observations, native CEF fixtures, and Rust regression tests. Several defects passed initial unit tests and were found through live use; their fixes are included in the reported checkpoints. This is a development acceptance exercise, not a claim of complete Chrome feature parity.

## Verified interactions

| Interaction | Live evidence |
|---|---|
| Multiple tabs | Create, immediate URL typing, switch, close/reopen; accessible New/Close actions; retained page text and paused-history state. |
| Back history | Right-click Back opens real session entries; selecting an entry navigates while preserving Forward. The 450 ms hold threshold has focused tests. |
| Loading and Stop | A 30-second local response shows tab loading, progress, and Stop; Stop returns the toolbar to Reload and removes progress. |
| Find | Immediate typing retains the complete query; actual page matches give a count; Next/Previous update the ordinal; navigation closes stale results. |
| Page menus | Right-click a link opens a menu; Open link in new tab loads the correct destination. Oversized menu URLs are rejected without interrupting other tabs. |
| Clipboard | Select All visibly selects a page input; Copy and Paste duplicate its complete text. |
| File selection | Native picker selects the generated test file; the page receives and displays its filename; Tab advances to the next editable field. |
| Downloads | Save completes a generated 26-byte file; Reveal selects it in Finder; Open shows its exact contents in TextEdit. |
| Bookmarks | Cmd+D adds a bookmark; Library Remove deletes the generated bookmark without the broken confirmation palette. |
| Media | One public YouTube video advances from 0:05 to 0:25 with changing decoded images; keyboard pause/resume works. Fullscreen fills the page area while the browser toolbar remains visible. |
| Compact layout | At 800 px wide, optional toolbar controls are hidden to keep the address readable. Unstyled pages use a readable white background. |
| Idle behavior | One debug session included no-frame intervals of 66.6 and 34.4 seconds, ruling out continuous repaint throughout that session. This is not a power benchmark. |

## Final integration

The optimized self-contained Browser Preview bundle passed code-signature verification and launched its embedded helper without a helper-path override. Live checks on the final reviewed code confirmed complete immediate new-tab URL entry, retained page proportions after opening/closing Find, real match counts, loading/Stop transitions, and public Wikipedia navigation.

A generated 2.48 MB download remained visible in the source tab after a close attempt, with an explicit finish/cancel notice. Another tab showed its progress and Cancel action; cancelling there allowed the source tab to close. Completed-download records also remained available after their source tab closed in the preceding integration check. Active downloads must finish or be cancelled before closing their source tab: CEF does not continue them after destroying that browser instance.

Final regressions: 141 browser tests passed (1 ignored), 48 WebView/protocol tests passed, 8 benchmark tests passed. Native fixtures verified variable-height Retina repaint and download close admission/completion/cancellation. A 40-cycle close/create regression verifies tab capacity is released. Scoped review of the final repairs (`c11e5c7d0`, followed by `ca5b92f36`) has no remaining P1/P2 findings.

The live audit spans multiple sessions and builds; it is not a single uninterrupted five-minute benchmark of the final binary.

Native zoom tests verify actual page viewport changes and shared-origin synchronization across tabs. Live zoom shortcut testing through the automation tool was inconclusive on this keyboard layout; no full UI latency claim is made.

## Performance and scope

The [three-site benchmark](2026-09-16-browser-pages/README.md) records navigation timing, scroll response and CEF CPU frame delivery with raw samples and runtime/filter hashes. It measures Wikipedia Main Page, Hacker News and scrollable YouTube search results. It does not measure video performance or end-to-end Rust/Vello presentation latency.

An intermittent cookie-store initialization delay remains unexplained. Tabs are bounded to 32 live instances. The browser uses CPU frame transport rather than shared GPU textures. These limitations are separate from the everyday-control fixes.
