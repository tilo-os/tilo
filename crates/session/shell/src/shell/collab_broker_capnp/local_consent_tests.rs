use super::local_consent::{LocalSessionAuthority, LocalSessionLiveness};
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const NATIVE_CONSENT_CHILD_TEST: &str = "TILO_NATIVE_CONSENT_CHILD_TEST";
const NATIVE_CONSENT_CHILD_PROOF: &str = "TILO_NATIVE_CONSENT_CHILD_PROOF";
const NATIVE_CONSENT_FIXTURE_ROOT: &str = "TILO_NATIVE_CONSENT_FIXTURE_ROOT";
const MISLEADING_TILO_USER_ID: &str = "environment-owner-must-not-select-identity";

fn wait_for_native_consent_child(
    child: &mut std::process::Child,
    test_name: &str,
) -> std::process::ExitStatus {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match child.try_wait().expect("poll isolated native-consent test") {
            Some(status) => return status,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated native-consent test timed out: {test_name}");
            }
        }
    }
}

pub(crate) fn run_isolated_native_consent_test(test_name: &str, assertions: impl FnOnce()) {
    if let Some(marker) = std::env::var_os(NATIVE_CONSENT_CHILD_TEST) {
        assert_eq!(marker, test_name, "isolated child ran the wrong exact test");
        assertions();
        let proof = PathBuf::from(
            std::env::var_os(NATIVE_CONSENT_CHILD_PROOF).expect("isolated child proof path"),
        );
        std::fs::write(&proof, format!("{test_name}\nassertions-complete\n"))
            .expect("write isolated child proof");
    } else {
        let temp = tempfile::tempdir().expect("isolated native-consent root");
        let fixture_root = temp
            .path()
            .canonicalize()
            .expect("canonical isolated native-consent root");
        std::fs::set_permissions(&fixture_root, std::fs::Permissions::from_mode(0o700))
            .expect("secure native-consent broker fixture root");
        let environment = fixture_root.join("environment");
        let runtime = environment.join("runtime");
        std::fs::create_dir_all(&runtime).expect("create isolated runtime");
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
            .expect("secure isolated runtime");
        let proof = temp.path().join("assertions-complete");
        let mut child = Command::new(std::env::current_exe().expect("current test binary"));
        child
            .args(["--exact", test_name, "--nocapture"])
            .env(NATIVE_CONSENT_CHILD_TEST, test_name)
            .env(NATIVE_CONSENT_CHILD_PROOF, &proof)
            .env(NATIVE_CONSENT_FIXTURE_ROOT, &fixture_root)
            .env("HOME", environment.join("home"))
            .env("XDG_CONFIG_HOME", environment.join("config"))
            .env("XDG_DATA_HOME", environment.join("data"))
            .env("XDG_CACHE_HOME", environment.join("cache"))
            .env("XDG_STATE_HOME", environment.join("state"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("TILO_IDENTITY_ROOT", environment.join("identity"))
            .env("TILO_USER_ID", MISLEADING_TILO_USER_ID);
        let mut child = child.spawn().expect("spawn isolated native-consent test");
        let status = wait_for_native_consent_child(&mut child, test_name);
        assert!(
            status.success(),
            "isolated native-consent child failed: {status}"
        );
        assert_eq!(
            std::fs::read_to_string(&proof).expect("isolated child completion proof"),
            format!("{test_name}\nassertions-complete\n"),
            "the exact child test did not complete its assertions"
        );
    }
}

pub(crate) fn native_consent_fixture_root() -> PathBuf {
    PathBuf::from(std::env::var_os(NATIVE_CONSENT_FIXTURE_ROOT).expect("fixture root"))
}

pub(crate) fn wait_for_local_session_worker_teardown(lifecycle: &LocalSessionLifecycle) {
    let deadline = tilo_test_env::hang_deadline();
    while lifecycle.snapshot().is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        lifecycle.snapshot().is_none(),
        "weak lifecycle must stop upgrading after the signalled broker worker releases its state"
    );
}

#[test]
fn native_consent_authority_starts_inactive_and_publication_is_idempotent() {
    let authority = LocalSessionAuthority::new();
    let lifecycle = authority.lifecycle();
    let initial = lifecycle.snapshot().expect("broker owns authority");
    assert_eq!(initial.liveness, LocalSessionLiveness::Inactive);
    assert_eq!(initial.generation, 0);
    assert_eq!(initial.locked, None);
    assert_eq!(initial.onboarding_active, None);

    lifecycle.publish_native_state(false, false);
    let active = lifecycle.snapshot().expect("active authority");
    assert_eq!(active.liveness, LocalSessionLiveness::Active);
    assert_eq!(active.generation, 1);
    assert_eq!(active.locked, Some(false));
    assert_eq!(active.onboarding_active, Some(false));
    assert_eq!(
        authority.active_session().expect("live session").epoch,
        active.epoch
    );

    lifecycle.publish_native_state(false, false);
    assert_eq!(
        lifecycle.snapshot().expect("idempotent authority"),
        active,
        "re-publishing unchanged native state must not rotate authority"
    );
}

#[test]
fn native_consent_authority_revocation_and_reactivation_advance_generation() {
    let authority = LocalSessionAuthority::new();
    let lifecycle = authority.lifecycle();
    lifecycle.publish_native_state(false, false);
    let first = lifecycle.snapshot().expect("first active generation");

    lifecycle.set_locked(true);
    let locked = lifecycle.snapshot().expect("locked generation");
    assert_eq!(locked.liveness, LocalSessionLiveness::Inactive);
    assert!(locked.generation > first.generation);
    lifecycle.set_locked(true);
    assert_eq!(lifecycle.snapshot().expect("repeat lock"), locked);

    lifecycle.set_locked(false);
    let unlocked = lifecycle.snapshot().expect("fresh unlock generation");
    assert_eq!(unlocked.liveness, LocalSessionLiveness::Active);
    assert!(unlocked.generation > locked.generation);
    assert_eq!(unlocked.epoch, first.epoch);

    lifecycle.set_onboarding_active(true);
    let onboarding = lifecycle.snapshot().expect("onboarding revocation");
    assert_eq!(onboarding.liveness, LocalSessionLiveness::Inactive);
    assert!(onboarding.generation > unlocked.generation);
    lifecycle.set_onboarding_active(false);
    let completed = lifecycle.snapshot().expect("post-onboarding generation");
    assert_eq!(completed.liveness, LocalSessionLiveness::Active);
    assert!(completed.generation > onboarding.generation);
    assert_eq!(completed.epoch, first.epoch);
}

#[test]
fn native_consent_authority_close_is_terminal_and_epochs_are_fresh() {
    let first = LocalSessionAuthority::new();
    let second = LocalSessionAuthority::new();
    let lifecycle = first.lifecycle();
    lifecycle.publish_native_state(false, false);
    assert_ne!(
        lifecycle.snapshot().expect("first authority").epoch,
        second
            .lifecycle()
            .snapshot()
            .expect("second authority")
            .epoch,
        "a replacement broker must mint a fresh epoch"
    );

    lifecycle.close();
    let closed = lifecycle.snapshot().expect("closed authority");
    assert_eq!(closed.liveness, LocalSessionLiveness::Closed);
    lifecycle.publish_native_state(false, false);
    lifecycle.set_locked(false);
    lifecycle.set_onboarding_active(false);
    assert_eq!(lifecycle.snapshot().expect("terminal close"), closed);
    assert!(first.active_session().is_none());
}

#[test]
fn native_consent_harness_devices_do_not_activate_authority() {
    const TEST: &str = "shell::collab_broker_capnp::local_consent_tests::native_consent_harness_devices_do_not_activate_authority";
    run_isolated_native_consent_test(TEST, || {
        let root = native_consent_fixture_root();
        let broker = crate::shell::collab_broker::spawn_native_harness(
            root.join("harness.sock"),
            crate::shell::state::WorkspaceTree::default().shared(),
            vec![[0x44; 32]],
        )
        .expect("spawn harness broker");
        let lifecycle = broker.local_session_lifecycle();
        let snapshot = lifecycle.snapshot().expect("harness authority");
        assert_eq!(snapshot.liveness, LocalSessionLiveness::Inactive);
        assert_eq!(snapshot.generation, 0);
        assert_eq!(snapshot.locked, None);
        assert_eq!(snapshot.onboarding_active, None);
        drop(broker);
        wait_for_local_session_worker_teardown(&lifecycle);
    });
}

#[test]
fn native_consent_broker_drop_releases_authority_while_action_broker_is_retained() {
    const TEST: &str = "shell::collab_broker_capnp::local_consent_tests::native_consent_broker_drop_releases_authority_while_action_broker_is_retained";
    run_isolated_native_consent_test(TEST, || {
        let root = native_consent_fixture_root();
        let broker = crate::shell::collab_broker::spawn_native_harness(
            root.join("drop.sock"),
            crate::shell::state::WorkspaceTree::default().shared(),
            Vec::new(),
        )
        .expect("spawn retained broker");
        let retained = broker.app_action_broker().expect("retained broker state");
        let lifecycle = broker.local_session_lifecycle();
        lifecycle.publish_native_state(false, false);
        assert_eq!(
            lifecycle.snapshot().expect("active broker").liveness,
            LocalSessionLiveness::Active
        );

        drop(broker);
        assert!(
            lifecycle.snapshot().is_none(),
            "the retained shared action broker must not keep shell consent authority alive"
        );
        drop(retained);
        wait_for_local_session_worker_teardown(&lifecycle);
    });
}
