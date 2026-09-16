# Task 3 capability checkpoint

User changed the active objective to basic browser UX before this scope was complete. No goal completion claim. No push or merge.

## Implemented

- Engine-neutral download/permission decisions, bounded native callback stores (32 per kind), 60-second deny/cancel expiry, explicit Rust prompt controls.
- Download progress/completion/failure/cancellation views; each approved destination is reserved with `mkdtemp`, suggested filenames cannot traverse, repeated downloads cannot overwrite prior files, nothing auto-opens.
- Desktop toolbar developer-tools command.
- Clear-on-exit is sent to the native helper; actual CEF profile data is removed after `CefShutdown`. BrowserWidget clears saved history during drop rather than next startup. Existing dedicated persistent profile is retained normally.
- Explicit manifest-scoped MV3 grants. Changing the manifest invalidates approval. Only approved absolute paths are sent to CEF `--load-extension`; activation/revocation requires restart. UI says “Load requested at startup”, not “active”. Permissions review includes API permissions, host permissions, optional permissions, and content-script matches.
- Locally installed extension action pages can be opened in the engine. This is an extension page surface, not a claim of full Chrome toolbar compatibility or Web Store installation.

## Direct evidence (macOS CEF 152 / Chromium 152.0.7977.83)

- Download/permission fixtures first failed because no prompts existed; after implementation: safe duplicate download exact bytes, declined download cancellation/no saved file, explicit geolocation denial, and actual profile deletion before next launch pass.
- MV3 fixture passes service-worker message handling, `chrome.storage.local` counter, content script execution, action page rendering/storage, and protection from ordinary page script injection on `chrome-extension:`. Its runtime extension ID matches the same SHA-256 absolute-path derivation used by the Rust action surface.
- Existing native keyboard/text, physical Retina resize, internal-document exclusion and script lifecycle fixtures pass (4 tests).
- Rust transport capability tests were red for missing variants, then 16 Chromium tests passed. Extension approval test was red for missing grant functions, then passed within browser suite.
- Initial full browser suite: 101 passed, 1 compact-toolbar regression caused by added DevTools button. Fixed by reserving it for wide toolbar. Final rerun: all 102 browser library tests passed (0 failed); build and tests exited 0.

Commands:

```
target/chromium-tools/bin/cmake --build target/chromium-cmake --parallel 8
python3 apps/browser/chromium/tests/capabilities.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'
python3 apps/browser/chromium/tests/extensions.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host' Extensions.test_mv3_worker_storage_content_and_action
python3 apps/browser/chromium/tests/native_regressions.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'
cargo test -p ui --lib webview::chromium --profile thin-app
cargo test -p browser --lib --profile thin-app
```

## Remaining / suspended when user changed objective

- Per-site blocking controls not implemented; global controls remain.
- Media/PiP probe experiment timed out with multiple CDP observers. All experiment changes were removed; Chromium still truthfully returns media unavailable. Audio/video/clipboard end-to-end qualification remains (existing text/key tests pass).
- New native capabilities have not been rerun on Linux. Parent owns Linux qualification and packaging.
- Existing explicit `clear_data` only clears cookies/HTTP cache/auth; full origin storage deletion is implemented at exit, not claimed for the in-session command.
- Permissions expiry/callback capacity are enforced but no timed 60-second fixture was added. Full active-download cancellation (after receiving partial bytes), permission grant with real capture devices, app-level history-drop persistence and developer-tools visual opening need additional qualification.
- Extension loading is proven only for the included MV3 fixture. Per-package load errors are not yet exposed as a manager status. UI extension activation controls and prompts have not received a native screenshot review.
