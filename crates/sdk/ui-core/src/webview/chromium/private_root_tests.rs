// A private window's host runs in a disposable root this process owns and
// removes only after the host exited, so its own engine's supervisor
// finishes the cleanup before the engine is dropped. The helper costs more
// than a case of another engine test, so it has a module of its own.

#![allow(
    clippy::unwrap_used,
    reason = "test fixtures: a failure preparing the fixture root or the helper script is the case failing"
)]

use super::*;

#[test]
#[cfg(unix)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn private_engines_own_and_remove_their_disposable_roots() {
    // phase: private_helper_gets_incognito_flag_and_owned_root_is_removed_after_exit
    {
        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("private-helper");
        let arguments = fixture.path().join("arguments");
        let exited = fixture.path().join("exited");
        let mut bytes = Vec::new();
        protocol::write_json(
            &mut bytes,
            &json!({"event":"ready","cef_version":"152","chromium_version":"152"}),
        )
        .unwrap();
        let encoded: String = bytes.iter().map(|byte| format!("\\{:03o}", byte)).collect();
        crate::testing::write_executable(
            &host,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{encoded}'\ncat >/dev/null\nprintf done > '{}'\n",
                arguments.display(),
                exited.display()
            ),
        );
        let root = private_root();

        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
        // The engine is handed back while its host starts.
        let launched = tilo_test_env::wait_for("the helper's arguments", || {
            std::fs::read_to_string(&arguments)
                .ok()
                .filter(|text| text.contains("--profile="))
        });
        let has_private = launched.lines().any(|arg| arg == "--private");
        let has_profile = launched
            .lines()
            .any(|arg| arg == format!("--profile={}/chromium", root.display()));
        assert!(root.exists(), "root must outlive the running helper");
        drop(engine);
        assert!(has_private, "launched arguments: {launched:?}");
        assert!(has_profile, "launched arguments: {launched:?}");
        assert!(
            exited.exists(),
            "engine drop must wait for the helper to exit"
        );
        assert!(
            !root.exists(),
            "engine drop must wait for complete owned-root cleanup"
        );
    }
    // phase: private_startup_failures_remove_owned_roots_after_reaping
    {
        let missing_root = private_root();
        assert!(
            ChromiumEngine::with_private_host(Path::new("/missing/tilo-host"), &missing_root)
                .is_err()
        );
        assert!(!missing_root.exists());

        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("not-ready");
        crate::testing::write_executable(&host, "#!/bin/sh\nexit 7\n");
        // A host that exits before it is ready fails the page once it is
        // reaped; the window never waited for it.
        let readiness_root = private_root();
        let engine = ChromiumEngine::with_private_host(&host, &readiness_root).unwrap();
        wait_until_removed(&readiness_root);
        assert!(!readiness_root.exists());
        assert!(engine.shared.failed.load(Ordering::Acquire));
        assert!(
            engine
                .drain_events()
                .iter()
                .any(|event| matches!(event, WebViewEvent::LoadFailed { .. }))
        );
    }
    // phase: private_packaged_helper_discovery_failure_removes_root
    {
        let fixture = tempfile::tempdir().unwrap();
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webview::chromium::tests::private_packaged_helper_discovery_failure_child",
                "--ignored",
            ])
            .env_remove(runtime::HOST_OVERRIDE_ENV)
            .env(runtime::RUNTIME_DIR_ENV, fixture.path())
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(!root.exists());
    }
    // phase: private_helper_crash_removes_root_while_engine_handle_remains
    {
        let (_fixture, host) = helper_fixture("exit 9");
        let root = private_root();
        let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
        wait_until_removed(&root);
        assert!(
            !root.exists(),
            "supervisor must clean after the crashed helper exits"
        );
        assert!(engine.shared.failed.load(Ordering::Acquire));
    }
    // phase: private_process_exit_waits_for_helper_and_root_cleanup
    {
        // Keep the protocol output open while stdin is consumed. Redirecting
        // stdout alone sends EOF to the production frame reader, which can
        // reap this supposedly-live helper before the child checks its root.
        let (_fixture, host) = helper_fixture("exec 3>&1; exec cat >/dev/null");
        let root = private_root();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webview::chromium::tests::private_process_lifecycle_child",
                "--ignored",
            ])
            .env("TILO_TEST_PRIVATE_HOST", &host)
            .env("TILO_TEST_PRIVATE_ROOT", &root)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(!root.exists(), "root must be absent when the child exits");
    }
}

#[cfg(unix)]
fn wait_until_removed(root: &Path) {
    tilo_test_env::wait_until("the profile root to be removed", || !root.exists());
}

#[test]
#[ignore = "child entry point for isolated packaged-helper discovery"]
#[cfg(unix)]
fn private_packaged_helper_discovery_failure_child() {
    let root = PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").unwrap());
    let result = ChromiumEngine::new_private(&root);
    assert!(
        result.is_err(),
        "isolated discovery unexpectedly found a helper"
    );
    assert!(
        !root.exists(),
        "discovery failure must release the allocated private root"
    );
}

#[test]
#[ignore = "child entry point for the real process-lifecycle regression"]
#[cfg(unix)]
fn private_process_lifecycle_child() {
    let host = std::env::var_os("TILO_TEST_PRIVATE_HOST").unwrap();
    let root = PathBuf::from(std::env::var_os("TILO_TEST_PRIVATE_ROOT").unwrap());
    let engine = ChromiumEngine::with_private_host(Path::new(&host), &root).unwrap();
    assert!(root.exists());
    drop(engine);
}

#[test]
#[cfg(unix)]
fn owned_navigation_cancel_refusal_wakes_without_holding_command_order() {
    // Closing the command queue hangs up helper stdin. Keep stdout owned by
    // the supervised helper until cancellation fails it; otherwise an unrelated
    // EOF can win the first-error latch before the refusal under test.
    let (_fixture, host) = helper_fixture("exec 3>&1; cat >/dev/null; exec sleep 30");
    // This route joins the supervisor on drop, so a fast test process cannot
    // exit before its deliberately live helper has been killed and reaped.
    let root = private_root();
    let engine = ChromiumEngine::with_private_host(&host, &root).unwrap();
    let enabled = Arc::new(AtomicBool::new(false));
    let reentered = Arc::new(AtomicBool::new(false));
    let weak = Arc::downgrade(&engine);
    let ready = enabled.clone();
    let observed = reentered.clone();
    engine.set_waker(Box::new(move || {
        if ready.swap(false, Ordering::AcqRel) {
            weak.upgrade()
                .unwrap()
                .send(WebViewCommand::CancelScopedNavigation { request_id: 2 });
            observed.store(true, Ordering::Release);
        }
    }));
    engine.tx.close();
    enabled.store(true, Ordering::Release);
    let cancelling = engine.clone();
    let (finished, received) = sync_channel(1);
    let worker = std::thread::spawn(move || {
        cancelling.send(WebViewCommand::CancelScopedNavigation { request_id: 1 });
        finished.send(()).unwrap();
    });
    tilo_test_env::recv(
        "cancel refusal must allow its waker to reenter the command path",
        &received,
    );
    worker.join().unwrap();
    assert!(reentered.load(Ordering::Acquire));
    assert!(engine.drain_events().iter().any(|event| matches!(event, WebViewEvent::LoadFailed { error, .. } if error.contains("cancellation was not queued"))));
    drop(engine);
    assert!(
        !root.exists(),
        "cancellation failure must reap its owned helper"
    );
}
