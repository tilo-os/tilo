# Task 5 implementation report

Status: implementation complete and locally verified; parent acceptance is partially complete and its remaining UI checks are blocked by the capture provider.

## Native private context TDD

- RED: `python3 apps/browser/chromium/tests/private_storage.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'` exited 1 at the reopened-private assertion because the pre-change helper retained `private-synthetic` storage.
- Rebuild: `python3 scripts/browser/build-host.py` exited 0 and rebuilt/signature-refreshed the CEF 152 host.
- First rebuilt sampled run used `target/browser-complete-ux/sampled-netlog-host-v2.py`; initial normal/private sample roots were `5c3a4ecd-6b9d-4391-82e0-d4b4c9690a03` and `8b3c06e4-a9df-4009-a398-9891348b30b1`. Both helpers exited 0; all recorded `/usr/bin/sample` calls exited 0 without timeout. The run reached normal reopen and exposed that the fixture cookie needed an explicit lifetime.
- Fixture correction: added `Max-Age=3600` so the normal-cookie reopen assertion tests a persistent cookie rather than a session cookie. Also tied title assertions to the requested query and made constructor failure/cleanup reap every synthetic helper while draining stderr to a temporary file.
- GREEN: `python3 apps/browser/chromium/tests/private_storage.py target/browser-complete-ux/sampled-netlog-host-v2.py` exited 0: `PASS: private context isolation, multi-tab sharing, and clean reopen`. Corrected-run wrapper roots: `b7cf7db3-3026-4c8f-bd3d-dce4bed4bcc6`, `0e73b080-1cf4-491a-bf32-37deac8abdc9`, `55f42c09-a2da-459d-a827-89b91788f851`, `99782ea8-f350-41e9-a35f-f1ca464ab236`.
- Sampling interpretation: the private sample had no `COOKIE_PERSISTENT_STORE_LOAD` interval, though its stack still included `SecItemCopyMatching`; this does not claim that private CEF avoids all OS Keychain initialization. Concurrent samples are retained for startup evidence, not latency comparison.
- Permission/context GREEN: `python3 apps/browser/chromium/tests/private_storage.py 'target/chromium/Browser Engine.app/Contents/MacOS/qos-chromium-host'` exited 0 with `PASS: private storage and permission isolation, multi-tab sharing, and clean reopen`. The host SHA-256 was `abbb69da8be8ee5492b39ce5bd8d63a5db74978cd98b012de2fae369140e242a`. This run used fake camera/microphone devices and proved normal `allowed` versus concurrent private `ask`, private grant/revoke without changing normal, private reopen `ask`, and normal reopen `allowed`. Output is retained at `target/browser-complete-ux/task5-private-storage-permissions.log` (SHA-256 `feb6cc00b73d34b91542ff211391282e77d335961ecf8f23b54b49c5138edab1`).

## Rust policy/root TDD

- RED: `cargo test -p browser browsing_mode::tests::private_mode_denies_every_personal_persistence_boundary --no-default-features` failed to compile on the absent policy gates.
- GREEN: the same command exited 0; 1 passed.
- RED: `cargo test -p browser storage::tests::private_roots_are_unique_owned_and_never_reuse_normal_overrides --no-default-features` failed to compile on absent owned-root allocation/validation.
- GREEN: the same command exited 0; 1 passed.
- RED: `cargo test -p ui private_helper_gets_incognito_flag_and_owned_root_is_removed_after_exit` failed on missing `with_private_host`; the first implementation then exposed a macOS `/var` versus `/private/var` canonical-path expectation in the test.
- GREEN: the same command exited 0; 1 passed, 3,195 filtered; helper exit marker existed before the complete owned root disappeared. Linker emitted the existing compact-unwind size warning.
- RED: `cargo test -p browser private_ --no-default-features` failed on absent `BrowserWidget::with_engine_mode` and `SiteControls::share_runtime_state`.
- GREEN: the same command exited 0; 7 library tests and 1 launch-policy test passed.
- RED: `cargo test -p browser allocation_sweeps_only_owned_roots_with_dead_processes --no-default-features` initially left the synthetic dead-owner root, then exposed the 256-entry production scan bound when run against the shared system temp directory.
- GREEN: after extracting the same bounded sweep over an injectable parent, the same command exited 0; 1 passed. The sweep requires a valid ownership marker plus dead owner PID and refuses cleanup while a recorded helper PID is live or untrusted.
- GREEN: `cargo test -p ui private_` exited 0; the three Chromium cases passed for normal helper exit, readiness/spawn failure, and native crash cleanup (the broad name filter also matched unrelated tests). Existing compact-unwind linker warning only.
- RED/GREEN: `cargo test -p ui unavailable_private_engine_owns_its_disposable_root` first failed to compile on the absent constructor, then exited 0 after the explicit WPE refusal engine took ownership of the disposable root.

## Launch parser and process lifecycle

- Live RED: parent acceptance clicked New Private Window on build `d9120eb9...`; the child exited with `--private is only meaningful with a capture or native-audit mode`. The flag was declared as capture-only rather than live.
- GREEN: `cargo test -p browser --test private_launch private_window_flag_is_valid_in_the_live_launch_parser` exited 0; 1 passed using the real Browser binary and standard parser.
- GREEN: optimized probe `target/thin-app/browser --private --shots-tabs target/browser-complete-ux/private-parser-probe.png` exited 0 and wrote a nonempty fixture.
- Live RED: closing private PID 69346/helper 69351 left owned root `5bee9f57-fa6e-45d0-b5de-45d5c28011e6`; parent retained and inspected marker permissions/PIDs before authorizing the next bounded sweep.
- RED: `cargo test -p ui private_helper_gets_incognito_flag_and_owned_root_is_removed_after_exit` failed immediately after engine drop: `engine drop must wait for the helper to exit`.
- GREEN: after retaining and joining the bounded supervisor thread from `Runtime::drop`, the same command exited 0; helper exit and full root cleanup were complete when engine drop returned.
- GREEN: `target/debug/deps/ui-c514f5321372d418 'webview::chromium::tests' --test-threads=1` exited 0: 30 passed, 1 intentionally ignored. Its parent process lifecycle case launched the ignored child entry point in a fresh OS process; that child exited 0 only after helper exit and root removal. The suite also covered readiness failure, missing helper, helper crash, and disposable WPE refusal cleanup.

## DPI regression and integration verification

- `cargo test -p browser --no-default-features` first found compact address width 97 after adding the toolbar action. Focused `cargo test -p browser browser_widget::tests::narrow_chrome_keeps_a_nonzero_page_viewport --no-default-features` passed after moving compact discovery into Settings while retaining Reload; the subsequent full Browser run passed 180 tests, 1 ignored, plus 3 main and 1 real-binary integration test.
- Parent live trace reproduced a private first-frame DPI failure (`rect=800x452`, `frame=800x444` on DPR 2). Unchanged native host discriminator passed physical resize/input (1920x1080 at DPR 2), narrowing the issue to app-side initial scale propagation.
- RED/GREEN: the pre-fix app seeded scale 1 before native mount and could retain a logical-size frame at DPR 2. `target/debug/deps/ui-c514f5321372d418 'widget::webview::tests::mounting_in_a_retina_window_sets_dpi_before_input' --exact --test-threads=1` exited 0 after native mount propagated the host scale and resent `Resize { width: 800, height: 600, scale_factor: 2.0 }` for a 400x300 view.
- Final chain: `cargo test -p ui webview::chromium::tests && cargo test -p browser --no-default-features && cargo check -p browser --no-default-features && cargo build -p browser --profile thin-app` exited 0. UI Chromium: 30 passed, 1 child-entry test ignored; Browser: 180 passed, 1 manual benchmark ignored; Browser main: 3 passed; real launch integration: 1 passed; check and optimized build succeeded. The linker emitted the existing compact-unwind size warning.
- Final real parser probe: `target/thin-app/browser --private --shots-tabs target/browser-complete-ux/private-parser-probe-final-20260916.png` exited 0, wrote a 13,918-byte PNG (SHA-256 `a64a626ec7e6401b31930ddd178fc60d7e02237594cca981c95a30140f0468dc`), and left no browser/helper process. The exact optimized binary is 26,710,016 bytes with SHA-256 `20cb26f900329dd24b81879d806cbffaa33ede25608f8fba0512b556153e46c4`.

## Acceptance boundaries

- Private mode is a separate Browser process. Every private window gets a unique 0700 synthetic root; its tabs share one private Chromium request context and runtime permission map. Normal extensions and credential/session/history/bookmark/settings stores are disabled in private mode. TLS and normal Chromium security remain enabled.
- The full toolbar exposes **New Private**; compact chrome retains Back, Reload/Stop, site protection, address/search, Library, and Settings, with the private-window action in Settings and the visible private banner in the window. Compact Forward remains the pre-existing hidden behavior and is recorded as an integration finding.
- Parent acceptance owns the live final-binary checks: physical-size DPR 2 frame/input, New Private Window child launch, visible banner/action, private storage and permission toggles, immediate post-close root removal before any later sweep, and an exact byte/file-set comparison proving the injected synthetic normal root is unchanged.
- Final-binary live partial GREEN: parent launched SHA-256 `20cb26f900329dd24b81879d806cbffaa33ede25608f8fba0512b556153e46c4` with an injected synthetic normal-root probe. Trace stabilized at `rect=800x452`, `frame=1600x904` before input, and a visible Seed click produced `private-synthetic` cookie/localStorage, proving full-size DPR and pointer alignment. ScreenCaptureKit then began returning `-3811`/`noWindowsAvailable` while the app/helper remained alive. Private close cleanup, compact action discovery, runtime exception controls, and the exact normal-root comparison therefore remain pending parent acceptance; this commit is a local checkpoint rather than a final acceptance claim.
