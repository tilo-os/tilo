# Task 1 report: symmetric navigation menus

## Result

Back and Forward now share the same click, 450 ms hold, right-click, keyboard-context-menu, and accessible menu-entry behavior. History requests carry a direction through the Rust/JSON/CEF boundary. CEF returns only entries on that side of the current entry, nearest first, while retaining its real DevTools entry IDs.

Browser menu replies are accepted only when request ID, direction, and the captured document revision still match. URI changes cancel pending requests and close an open history menu. Legacy native event fixtures that omit `direction` continue to decode as Back.

## Files

- `apps/browser/src/back_hold.rs`
- `apps/browser/src/browser_widget.rs`
- `apps/browser/chromium/src/host.cc`
- `apps/browser/chromium/src/navigation_history.h`
- `apps/browser/chromium/tests/native_regressions.py`
- `ui/src/webview/mod.rs`
- `ui/src/webview/chromium/mod.rs`
- `ui/src/webview/chromium/tests.rs`
- `ui/src/webview/test_engine.rs`

## Commit

- `3b1f1243f feat(browser): add symmetric navigation history menus`

## TDD evidence

RED:

- `cargo test -p browser --lib hold_forward_opens_once_and_release_does_not_navigate -- --nocapture`
  - Failed at the Forward press assertion because the existing handler only owned Back.
- `cargo test -p browser --lib stale_history_reply_after_direction_or_page_change_is_ignored --no-run`
  - Failed to compile on the intentionally missing `NavigationDirection`, direction event field, and direction-aware request method.

Focused GREEN:

- `cargo test -p browser --lib current_entry_splits_nearest_back_and_forward_history_and_forward_hold_opens_once -- --nocapture`
  - `1 passed; 0 failed`
- `cargo test -p browser --lib stale_history_reply_after_direction_or_page_change_is_ignored -- --nocapture`
  - `1 passed; 0 failed`
- `cargo test -p browser --lib forward_history_keyboard_open_and_accessible_activation_use_session_entry -- --nocapture`
  - `1 passed; 0 failed`
- `cargo test -p browser --lib hold_back_requests_real_session_history_and_release_does_not_navigate -- --exact --nocapture`
  - `1 passed; 0 failed`

## Final verification

- `cargo test -p browser --lib`
  - `144 passed; 0 failed; 1 ignored`
- `cargo test -p ui session_history_payloads_are_bounded_and_keep_snapshot_identity -- --nocapture`
  - `1 passed; 0 failed; 3179 filtered out` in the UI library; integration target selected zero tests.
  - Existing linker warning: `__eh_frame section too large ... performance of exception handling might be affected`.
- `python3 scripts/browser/build-host.py --platform macosarm64 --jobs 6`
  - Exit 0; CEF host and helpers built.
  - Existing macOS warning: `allowedFileTypes` is deprecated in `main_mac.mm`.
- `target/chromium/qos-protocol-test`
  - `protocol tests passed`
- `python3 apps/browser/chromium/tests/native_regressions.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host' NativeRegressions.test_session_history_is_directional_and_uses_real_entry_ids`
  - `Ran 1 test ... OK`
  - Exercises A → B → C, Back to B, Back menu beginning with A, Forward menu containing only C, and navigation to C by the returned real entry ID.
- `git diff --check -- <nine owned source/test paths>`
  - Exit 0.

## Limitations and observations

- One native fixture retry timed out during `setUp` while waiting 20 seconds for the initial local HTTP page's `loaded` event. It did not reach the navigation-history behavior and emitted no host log in that failure path. The immediate rerun passed in 1.966 seconds. This is retained as startup-flake evidence for the separately tracked startup diagnostic.
- The native fixture initially exposed that CEF keeps `about:blank` older than fixture page A. The assertion now checks nearest-first A plus directional exclusion, rather than incorrectly requiring A to be the entire Back list.
- This task did not perform a separate pixel-level menu inspection; layout, menu rows, input routing, and accessibility are exercised through the browser widget tests, and the parent task owns the final live/visual integration pass.
