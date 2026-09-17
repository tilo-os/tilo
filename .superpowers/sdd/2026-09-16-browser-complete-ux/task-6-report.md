# Task 6 — background downloads implementation report

## Scope and ownership

Implementation worker: `/root/browser_download_lifetime` on `codex/macos-shell`, base `23dc5bc1a`.

Owned production/test paths:

- `ui/src/webview/{mod.rs,test_engine.rs,chromium/mod.rs,chromium/tests.rs}`
- owned hunks in shared `ui/src/{widget/mod.rs,app.rs}`
- `apps/browser/src/{capabilities.rs,browser_widget.rs,browser_tabs.rs}`
- `apps/browser/chromium/src/{capabilities.h,host.cc,download_slots.h}`
- `apps/browser/chromium/tests/{protocol_test.cc,download_lifetime.py}`

The workspace contains extensive unrelated tracked/staged/untracked edits. Only owned paths/hunks will be staged. No GUI was launched by this worker.

## Implementation checkpoint

- Runtime-wide download records now retain bounded URL/retry metadata and sparse terminal updates.
- Native admission uses a 32-ID pending/active/early union; visible tabs detach only for approved transfers and physical owners close after terminal settlement.
- `tab_detached` retires the Rust route before publication; `closed` remains physical destruction evidence.
- Native runtime events expose admitted/detached/accepting state; runtime disconnect fails progress records.
- Retry uses a prior terminal ID and `CefBrowserHost::StartDownload`, with retry eligibility derived conservatively from `CanDownload` request-method provenance.
- Browser chrome exposes aggregate status and the downloads library retains open/reveal/retry actions.
- Primary window close uses a default-allow Widget hook. Browser roots offer Keep downloading or Cancel downloads and quit; cancellation waits for admitted=0, detached=0, and terminal records.

## TDD and verification evidence so far

1. `cargo test -p ui ordinary_window_close_defaults_to_immediate_exit --no-default-features`
   - RED session `94035`: production `WindowCloseAdmission`/hook definitions absent.
   - GREEN session `47985`: 1 passed, 0 failed; linker emitted the existing compact-unwind size warning.
2. `cargo test -p browser browser_tabs::tests::window_close_requires_an_explicit_choice_and_waits_for_native_owner_cleanup --no-default-features`
   - RED: missing exit panel/state behavior.
   - GREEN: initially 1 passed; after exercising the real button/action-broker path, session `915cb9`, exit 0, 1 passed, 0 failed.
3. `cargo test -p browser capabilities::tests --no-default-features`
   - First run: 3 passed, 1 failed because the assertion searched only the root AX node instead of the terminal status child.
   - Corrected run: exit 0 in 22.60s, 4 passed, 0 failed.
4. `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-protocol-test --parallel 2 && target/chromium/qos-protocol-test`
   - exit 0; protocol tests passed, including bounded slot duplicate/admit/settle behavior.
5. `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-chromium-host --parallel 4`
   - first compile found a C++ method/type name collision (`Retry`); renamed record type.
   - next run exit 0; host built and signed.
6. `python3 apps/browser/chromium/tests/download_lifetime.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'`
   - old-host RED session `13950`: exit 1 after 92.98s waiting for `tab_detached`; old behavior emitted `close_blocked`.
   - first rebuilt-host run session `72815`: exit 1 after 127.04s; timed out before the first `download_prompt`, then graceful shutdown timed out after 35s. Root cause was not established and the failure is retained as an intermittent/unknown limitation, potentially overlapping the separately tracked native startup/navigation defect.
   - isolated diagnostic run crossed admission correctly: tab 2 ready; runtime admitted 0→1; prompt ID 1 arrived; GET progress was `retryable: true`; stderr contained only the known macOS signature-validation warning.
   - exact rerun exec chunk `a0cb3d`: exit 0 in 2.83s, 1 passed.
7. `cargo test -p ui webview::chromium::tests --no-default-features`
   - first covering run: 31 passed, 1 failed, 2 ignored. Failure was `detached_acknowledgment_releases_visible_route_before_physical_close`: the fixture printed `tab_detached` before route 2 was created, so it could not test production ordering.
   - focused corrected fixture command `cargo test -p ui detached_acknowledgment_releases_visible_route_before_physical_close --no-default-features`, exec session `86959`: exit 0, 1 passed. The helper now waits for an explicit post-route marker before publishing detach.
8. Native early/provenance seam:
   - RED: `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-protocol-test --parallel 2`, chunk `c41ad7`, exit 2 because `Stage`, `Promote`, `EarlyForOwner`, and `DownloadProvenance` were intentionally absent.
   - GREEN: the same build plus `target/chromium/qos-protocol-test`, chunk `f0d1ad`, exit 0; `protocol tests passed`. Coverage includes repeated early ID admission, promotion, owner-specific early cleanup, capacity recovery, same-URL GET/POST ambiguity, unknown provenance, consumed provenance, and blob rejection.
9. Expanded real CEF fixture:
   - `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-chromium-host --parallel 4 && PYTHONUNBUFFERED=1 python3 apps/browser/chromium/tests/download_lifetime.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'`
   - exec session `6515`, chunks `676800`/`569cfd`, exit 0. Host rebuilt and signed; 3 tests passed in 56.21s.
   - Real coverage: two transfers retain one detached physical owner until both terminal; cancellation metadata remains retryable for GET; the other transfer completes with exact bytes; retry creates a fresh ID, requires consent, completes exact bytes, and leaves active-tab URI unchanged; closing an unaccepted prompt cancels/cleans it; POST cancellation is non-retryable and retry sends no GET; refused destination fails truthfully and returns runtime to admitted=0/detached=0.
   - After changing the POST case to use the window-exit `download_cancel_all` command and assert `accepting=false`, reran the same Python fixture without rebuilding unchanged production code: chunk `466929`, exit 0, 3 passed in 6.64s.
10. `cargo test -p browser --no-default-features`
   - exec session `80956`, chunks `7cbfbe`/`82983f`, exit 0 after 38.46s build.
   - browser lib: 185 passed, 0 failed, 3 ignored; main: 3 passed; private launch integration: 1 passed; doc tests: 0 failures.
11. Final `cargo test -p ui webview::chromium::tests --no-default-features` attempts:
   - chunk `dd5723`: blocked before Task 6 tests by a concurrent untracked history file passing `String` to `Text::new(&str)`; its owner repaired this.
   - chunk `855e65`: blocked before Task 6 tests by unrelated concurrent `ui/src/widget/nav_shell.rs:1558`, where `ActionSpec::immediate(..., &())` no longer satisfies the `Action` bound.
   - after both owners repaired their unrelated sources, chunk `839338` exited 0: 32 passed, 0 failed, 2 ignored (34 selected; child-process fixtures also passed). The linker emitted only the existing compact-unwind size warning.

## Remaining verification

- Build the optimized thin browser artifact once for parent-owned live GUI acceptance.
- Review/stage only owned hunks, record commit(s), and report parent-owned live GUI limitations honestly.
