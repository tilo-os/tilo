# Task 3 — Session recovery

## Implementation

Normal browser workspaces acquire an exclusive app-root session lock before creating browser content or opening the UI document. Both desktop entrypoints use the same recovery factory. `--app-root /absolute/path` (or `QOS_BROWSER_APP_ROOT`) isolates Chromium, history, settings, bookmarks, extensions, filter cache, UI document, and session files. Existing default storage paths remain unchanged.

Version 1 snapshots contain ordered URL/pin records, active index, and clean-exit marker. Bounds are 32 tabs, 8192 bytes per URL and 300000 serialized bytes. Only HTTP(S) without userinfo and about:blank persist; unsupported current URLs become blank placeholders. Invalid versions, bounds, unsafe URLs and non-prefix pinned ordering are rejected. Private mode explicitly opens no session store and creates no session files; private-window UI remains Task 5.

Snapshots use a mode-0600 exclusive temporary file, file fsync, atomic rename and directory fsync. Pre-rename failures preserve the previous snapshot. An OS file lock prevents a second live process from writing the session or UI document and is automatically released after process death. A stale temporary file is removed only after lock acquisition.

Clean restart automatically restores the active page and defers each background URL until first selection. Crash restart presents Restore / Start fresh and performs no saved-page navigation before the choice. Meaningful tab/order/pin/active/navigation changes persist; address drafts do not. Normal workspace drop records clean exit, while panic unwinding and SIGKILL leave the dirty marker. Closing a tab removes its record; quitting retains open tabs.

## Changed production paths

- New: `apps/browser/src/session.rs`, `apps/browser/src/storage.rs`.
- Workspace integration: `apps/browser/src/browser_tabs.rs`, `browser_widget.rs`, `main.rs`, `lib.rs`.
- Isolated storage routing: `apps/browser/src/history.rs`, `settings.rs`, `library.rs`, `extensions.rs`, and only the `cache_dir` hunk of `blocker.rs`.
- Explicit native profile constructor and URL-tagged completion decode/queue handling: `ui/src/webview/chromium/mod.rs`; matching event variant/tests in `ui/src/webview/mod.rs`, `ui/src/webview/chromium/tests.rs`, and `apps/browser/src/live_probe.rs`.

## Verification

- Disk roundtrip, atomic replacement failure, corrupt/unsafe/oversized snapshots, serialized-byte budget, pinned ordering, private exclusion and live lock tests were developed with failing tests first.
- Workspace tests cover active/order/pin restore, address-draft exclusion, closed-tab removal and actual canonical Start fresh button dispatch.
- Browser suite before final label/event follow-up: 156 passed, 0 failed, 1 ignored (`/tmp/browser-session-final-unit.log`). Debug executable built successfully (`/tmp/browser-session-live-build.log`).
- Deferred tab label regression first failed with about:blank instead of saved URL (`/tmp/browser-session-label-red.log`). Final browser suite: **158 passed, 0 failed, 1 ignored** (`/tmp/browser-session-final-all.log`). Binary tests: **2 passed** (`/tmp/browser-session-bin-tests.log`). Focused Chromium tests: **24 passed** (`/tmp/browser-session-chromium-final.log`). Final debug build passed in1.52s (`/tmp/browser-session-final-build.log`). Scoped `git diff --check` passed.

## Process evidence

Worker fixture: `target/browser-session-worker/`, HTTP PID10798 port65177, app bundle `Browser Session Worker.app`, isolated root `profile`. App PID12705 started from a clean three-tab snapshot (Alpha pinned, Beta-next active, Gamma last). Request log showed only Beta-next initially, then Alpha/Gamma on selection; switching back to Beta-next did not reload it. A second process exited1 with the live-lock error and byte-identical snapshot (`second-instance.json`). Actual AX window close exited PID12705 and wrote clean_exit=true while retaining all three records (`profile/verified-clean.json`). No CUA call was made after terminal close.

Independent parent acceptance used its separate port64808 fixture and `target/browser-complete-ux/session-acceptance` roots. Parent reported: PID13739 active-only restore, deferred Gamma loads once and retains counter1 on switching; second instance fails without snapshot change; closing Gamma removes it; unsent address draft excluded; window close marks clean. PID24211 restart retained Alpha pinned plus Beta lazy, with Gamma and draft absent. Parent SIGKILL of verified PID24211 preserved dirty state. PID26380 showed Restore/Start fresh without saved-page requests; Restore loaded only Alpha and retained both tabs. Separate crash profile PID29490 Start fresh produced one unpinned blank tab without old requests. Parent closed all session acceptance apps through their window close control. Original user browser and the unrelated parent QA profile were not used for these tests.

## Limits

Snapshots restore URLs/order/pins/active selection, not DOM state, navigation stacks or form contents. Background native endpoints exist at blank pages, but saved URLs are not fetched until activation. Session fsync runs synchronously only when the snapshot changes. Directory-sync errors after rename are reported but cannot roll back an already atomic replacement. Private UI is not implemented in this task; the persistence policy is provided for Task 5.


### Final live follow-up and additional fixtures

Worker clean restart PID44784 retained all three tabs and loaded only Beta-next. SIGKILL of that verified owned PID left its dirty snapshot intact (`profile/verified-before-crash.json`). Restart PID45607 showed the recovery prompt with no new page request (`requests-before-restore.json`). Choosing Restore loaded only Beta-next and retained all three records/pin/order. Normal AX close wrote clean=true (`profile/verified-after-crash-restore.json`).

Final build PID57582 at new `final-history` root showed saved Alpha/Gamma URLs as deferred AX labels and the Beta-next URL in history, with no inherited about:blank title. The app was closed normally. Load completion now preserves its source URL, stale blank completions cannot finish a newer restored page, and new navigation clears the preceding document title. Late titles may leave a history row displaying its URL, which remains an accurate destination.

Malformed process fixtures passed for version999 (PID61232), 33tabs (PID61285), and javascript URL (PID61325). Each rejected the snapshot, persisted one safe local fallback URL and fetched that local fixture. Results: `target/browser-session-worker/invalid-process-results.json`. Each process was terminated by its owned subprocess handle after assertions (exit-15); these termination tests intentionally leave dirty markers. The initial harness mistakenly used shot-only `--url`, producing the normal default homepage; it was corrected to the supported positional URL before the three successful fixture runs. No unsafe saved-page request occurred.

All worker browser app processes have exited. The task-only HTTP fixture can remain available for review; it does not access user data. Test artifacts live under ignored `target/browser-session-worker` and temporary logs under `/tmp`, while this report is committed.
