//! Separate signed-source SDK journeys; the existing untyped journey is unchanged.
use crate::{RuntimeServices as _, standalone::Standalone};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const TEST: &str = "standalone::committed_documents::tests::independent_sdk::signed_independent_sdk_children_share_durable_record_and_refuse_undeclared_kind";
const HELPER: &str = "TILO_SIGNED_INDEPENDENT_SDK_HELPER";
const APP: &str = "so.tilo.independent-sdk";
const KIND: &str = "independent.document";

fn helper(root: &Path, mode: &str) {
    let kind = if mode == "denied" {
        "independent.undeclared"
    } else {
        KIND
    };
    let checkout = ui::record::RecordCheckout::new(
        ui::record::RecordId::for_file_record(if mode == "denied" {
            uuid::Uuid::from_u128(1)
        } else {
            uuid::Uuid::nil()
        }),
        ui::record::RecordKind::new(kind),
    );
    let mut manager = ui::collaboration::CollaborationManager::for_native_record(&checkout);
    let started = Instant::now();
    while manager.state() == ui::collaboration::CollaborationState::Connecting
        && started.elapsed() < tilo_test_env::hang_bound()
    {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    if mode == "production-denied" {
        assert!(!manager.is_connected(), "{:?}", manager.state());
        return;
    }
    if mode == "denied" {
        assert!(!manager.is_connected());
        assert!(
            format!("{:?}", manager.state()).contains("valid installed resource declaration"),
            "{:?}",
            manager.state()
        );
        return;
    }
    assert!(manager.is_connected(), "{:?}", manager.state());
    assert!(
        !manager.send(ui::collaboration::CollabRequest::document_snapshot(
            automerge::AutoCommit::new().save()
        )),
        "native construction cannot publish an unauthorised peer snapshot"
    );
    std::fs::write(root.join(format!("ready-{mode}")), b"connected").unwrap();
    while !root.join("release").exists() && started.elapsed() < tilo_test_env::hang_bound() {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        root.join("release").exists(),
        "parent did not release the owned SDK child"
    );
}

fn command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    let log = std::fs::File::create(root.join(format!("sdk-{mode}.log"))).unwrap();
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(HELPER, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    command
}

struct Children(Vec<(Child, String)>);
impl Drop for Children {
    fn drop(&mut self) {
        for (child, _) in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn signed_independent_sdk_children_share_durable_record_and_refuse_undeclared_kind() {
    if let Ok(mode) = std::env::var(HELPER) {
        helper(
            &std::path::PathBuf::from(std::env::var_os("TILO_STANDALONE_TEST_ROOT").unwrap()),
            &mode,
        );
        return;
    }
    let Some(root) = crate::standalone::tests::isolated_root(TEST) else {
        return;
    };
    let store = crate::apps::independent_source_fixture(&root, APP, KIND);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_APPS_DATA_DIR", store);
    environment.remove("TILO_APPS_SYSTEM_DIR");
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    let owner = root.join(crate::standalone::owner::ROOM_OWNER);
    let initial = std::fs::read(&owner).unwrap();
    let mut children = Children(Vec::new());
    for mode in ["one", "two"] {
        children.0.push((
            session.launch(APP, command(&root, mode)).unwrap(),
            mode.into(),
        ));
    }
    let started = Instant::now();
    while !(root.join("ready-one").exists() && root.join("ready-two").exists())
        && started.elapsed() < tilo_test_env::hang_bound()
    {
        for (child, mode) in &mut children.0 {
            assert!(
                child.try_wait().unwrap().is_none(),
                "SDK child {mode} exited: {}",
                std::fs::read_to_string(root.join(format!("sdk-{mode}.log"))).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(root.join("ready-one").exists() && root.join("ready-two").exists());
    let durable = std::fs::read(&owner).unwrap();
    assert_ne!(
        durable, initial,
        "typed SDK creation must reach the durable owner"
    );
    let mut denied = session.launch(APP, command(&root, "denied")).unwrap();
    assert!(
        denied.wait().unwrap().success(),
        "{}",
        std::fs::read_to_string(root.join("sdk-denied.log")).unwrap()
    );
    assert_eq!(
        std::fs::read(&owner).unwrap(),
        durable,
        "undeclared creation must preserve durable bytes"
    );
    let error = session.launch(APP, Command::new("/bin/true")).unwrap_err();
    assert!(
        error.to_string().contains("original signed entrypoint"),
        "{error}"
    );
    assert_eq!(std::fs::read(&owner).unwrap(), durable);
    std::fs::write(root.join("release"), b"release").unwrap();
    for (child, mode) in &mut children.0 {
        assert!(child.wait().unwrap().success(), "SDK child {mode} failed");
    }
    drop(children);
    drop(session);
    let restarted = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    assert_eq!(
        std::fs::read(&owner).unwrap(),
        durable,
        "restart must retain the exact durable graph"
    );
    drop(restarted);
}

#[test]
fn uninstalled_local_dev_sdk_creation_requires_explicit_development_host() {
    const DEVELOPMENT_TEST: &str = "standalone::committed_documents::tests::independent_sdk::uninstalled_local_dev_sdk_creation_requires_explicit_development_host";
    const DEVELOPMENT_APP: &str = "local.dev.independent-sdk";
    let Some(root) = crate::standalone::tests::isolated_root(DEVELOPMENT_TEST) else {
        return;
    };
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.remove("TILO_APPS_DATA_DIR");
    environment.remove("TILO_APPS_SYSTEM_DIR");
    let session = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    let owner = root.join(crate::standalone::owner::ROOM_OWNER);
    let before = std::fs::read(&owner).unwrap();
    let mut denied = session
        .launch(DEVELOPMENT_APP, command(&root, "production-denied"))
        .unwrap();
    assert!(
        denied.wait().unwrap().success(),
        "{}",
        std::fs::read_to_string(root.join("sdk-production-denied.log")).unwrap()
    );
    assert_eq!(std::fs::read(&owner).unwrap(), before);
    drop(session);

    let session = Standalone::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let mut children = Children(vec![(
        session
            .launch(DEVELOPMENT_APP, command(&root, "one"))
            .unwrap(),
        "one".into(),
    )]);
    tilo_test_env::wait_until("uninstalled local.dev SDK connection", || {
        assert!(
            children.0[0].0.try_wait().unwrap().is_none(),
            "{}",
            std::fs::read_to_string(root.join("sdk-one.log")).unwrap()
        );
        root.join("ready-one").exists()
    });
    let durable = std::fs::read(&owner).unwrap();
    assert_ne!(durable, before, "dev creation must reach the durable owner");
    std::fs::write(root.join("release"), b"release").unwrap();
    assert!(children.0[0].0.wait().unwrap().success());
    drop(children);
    drop(session);
    let restarted = Standalone::start_reporting(&root, &mut |_| {}).unwrap();
    assert_eq!(std::fs::read(&owner).unwrap(), durable);
    drop(restarted);
}

#[test]
fn development_host_keeps_installed_resource_declarations_narrow() {
    const DEVELOPMENT_TEST: &str = "standalone::committed_documents::tests::independent_sdk::development_host_keeps_installed_resource_declarations_narrow";
    let Some(root) = crate::standalone::tests::isolated_root(DEVELOPMENT_TEST) else {
        return;
    };
    let store = crate::apps::independent_source_fixture(&root, APP, KIND);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_APPS_DATA_DIR", store);
    environment.remove("TILO_APPS_SYSTEM_DIR");
    let session = Standalone::start_in_dev_mode(&root, &mut |_| {}).unwrap();
    let owner = root.join(crate::standalone::owner::ROOM_OWNER);
    let before = std::fs::read(&owner).unwrap();
    let mut denied = session.launch(APP, command(&root, "denied")).unwrap();
    assert!(
        denied.wait().unwrap().success(),
        "{}",
        std::fs::read_to_string(root.join("sdk-denied.log")).unwrap()
    );
    // Keep the governor serving until the actual SDK child has left.
    drop(session);
    assert_eq!(
        std::fs::read(&owner).unwrap(),
        before,
        "dev mode cannot expand an installed package's resource declarations"
    );
}
