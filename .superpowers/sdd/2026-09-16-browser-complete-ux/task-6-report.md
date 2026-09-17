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

- Parent-owned live GUI acceptance remains: toolbar visuals/actions, real OS close choices, private-window saved-file/profile cleanup, and open/reveal. This worker did not launch the GUI, as coordinated.
- The initial rebuilt-host native run's unexplained prompt/shutdown timeout remains recorded above; the exact fixture then passed twice, including all expanded cases. The separate Task 9/10 startup/rendering defect was not edited or hidden.

## Optimized candidate and owned commits

- Source commit: `4a23e862ce8a96742d7c4a0e821c0de94eb30f22` (`Implement background download ownership and exit flow`). It contains only Task 6 paths and the Task 6 hunks from shared `ui/src/app.rs` and `ui/src/widget/mod.rs`.
- `cargo build -p browser --profile thin-app` — session `5014`, chunks `4dfb59`/`5426b3`/`90f03f`/`68b638`/`74814a`/`b63960`, exit 0 after 3m48s. It initially waited for the shared Cargo lock, then compiled `ui`, `qos-app`, and `browser` without warnings/errors in the returned build output.
- Candidate: `target/thin-app/browser`, 27,316,768 bytes, SHA-256 `6ff412a0d6584a8b58c2ec347f44022c4a3c13ba052231e8827761a9f695b1c8`.
- Review/stage only owned hunks, record commit(s), and report parent-owned live GUI limitations honestly.

## Review fix round 1

Review verdict `Needs fixes` identified four lifecycle gaps. The corrective implementation is separate from source commit `4a23e862c`:

- Same-owner/same-URL request provenance is now a bounded FIFO. Every request in an overlapping group remains ambiguous until consumed, so the reproduced GET/POST/GET ordering cannot make the POST or later overlapping GET replayable. A subsequent isolated GET remains retryable. Overflow disables retry provenance conservatively for the runtime.
- A busy visible owner now reserves one of 32 retained-owner slots before native admission. Detachment converts that reservation into a physical-owner reservation, which remains until `OnBeforeClose`; terminal settlement alone cannot free capacity. This preserves 32 visible clients and up to 32 admitted transfers without cancelling accepted work.
- Owner activity covers early callbacks and prompted/approved transfers. A posted idle callback re-reads that activity at execution, so a new early admission invalidates a stale close decision. The deterministic seam delays physical destruction and the idle callback, then verifies capacity recovery and stale-task rejection.
- Cancel-and-quit now requires a native `download_runtime` acknowledgment with `accepting=false`, in addition to zero admitted/detached owners and terminal records. `CancelAll` publishes a final acknowledgment even when there was no active transfer. The action-broker regression leaves the dialog open through completion, selects Cancel against an accepting/idle snapshot, injects a pre-barrier admission, and waits through terminal and physical cleanup.
- The real POST case now cancels one POST while the runtime is still accepting, attempts retry, and proves no prompt and no GET. Cancel-all acknowledgment is exercised afterward with a separate active GET.

### TDD and focused verification

1. Native reservation/provenance RED:
   - `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-protocol-test --parallel 2`
   - chunk `a6107a`, exit 2: the new regression could not compile because production `DownloadOwnerReservations` did not exist.
2. Native reservation/provenance GREEN:
   - `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-protocol-test --parallel 2 && target/chromium/qos-protocol-test`
   - chunk `824421`, exit 0: `protocol tests passed` for outstanding GET/POST/GET ambiguity, delayed physical-close capacity, replacement visible-owner admission, and stale idle-task revalidation.
   - Final rebuild after transition-readability edits: `target/chromium-tools/bin/cmake --build target/chromium-build --target qos-protocol-test qos-chromium-host --parallel 4 && target/chromium/qos-protocol-test`, session `26687`, exit 0; host linked/signed and `protocol tests passed`.
3. Rust exit-barrier RED attempts:
   - `cargo test -p browser browser_tabs::tests::window_close_requires_an_explicit_choice_and_waits_for_native_owner_cleanup --no-default-features`
   - sessions `62d97d` and `21529f` were blocked before the Task 6 test by unrelated concurrent compile errors in `ui/src/record.rs` and then `ui/src/widget/modal_dialog.rs`. Their owners repaired both shared files; Task 6 did not edit them.
4. Rust exit-barrier GREEN:
   - same focused command, session `81224`, exit 0: 1 passed, 0 failed (187 filtered).
   - `cargo test -p browser capabilities::tests --no-default-features`, session `78506`, exit 0: 4 passed, 0 failed (184 filtered).
   - `cargo check -p browser --no-default-features`, session `44773`, exit 0.
5. Source/fixture checks:
   - `python3 -m py_compile apps/browser/chromium/tests/download_lifetime.py`, exit 0.
   - `git diff --check -- apps/browser/chromium/src/capabilities.h apps/browser/chromium/src/download_slots.h apps/browser/chromium/src/host.cc apps/browser/chromium/tests/download_lifetime.py apps/browser/chromium/tests/protocol_test.cc apps/browser/src/browser_tabs.rs apps/browser/src/capabilities.rs`, exit 0.
6. Real CEF fixture limitation:
   - First fix-round run (session `77843`) and final correctly addressed run `PYTHONUNBUFFERED=1 python3 apps/browser/chromium/tests/download_lifetime.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'` (session `29886`) both exited 1 after about 378 seconds. All three cases stopped before `download_prompt`; the latest event was navigation progress `0.1` after an `about:blank` load, and cleanup then timed out after 35 seconds. Stderr contained only the known macOS signature-validation warning. This reproduces the separately tracked Task 9/10 first-navigation/shutdown defect and does not reach the changed download transitions. The deterministic native protocol suite and native host build are green; no live CEF acceptance is claimed for this fix round.
   - A prior invocation in session `27785` used the obsolete executable basename `Browser Engine` and exited immediately with `FileNotFoundError`; the actual bundle executable is `qos-chromium-host`, used by session `29886` above.

No GUI was launched in this fix round. Parent-owned live GUI acceptance remains pending.
