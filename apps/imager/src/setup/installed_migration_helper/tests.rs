use super::*;
use crate::setup::{
    flatpak_transfer::Runner,
    user_transfer::{self, TransferRequest},
};
use std::os::unix::fs::PermissionsExt;
struct NoApps;
impl Runner for NoApps {
    fn run(&self, _home: &Path, _args: &[String]) -> Result<String, String> {
        panic!("Home-only continuation must not run Flatpak")
    }
}
#[test]
fn native_session_copies_then_requires_adoption_and_preserves_selected_custody() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("old-home");
    let home = temp.path().join("new-home");
    for directory in [&source, &home] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for relative in [".config/tilo", ".local/state/tilo/migration/setup"] {
        std::fs::create_dir_all(home.join(relative)).unwrap();
        std::fs::set_permissions(home.join(relative), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    std::fs::set_permissions(
        home.join(".local/state/tilo"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let workspace = home.join(".local/state/tilo/migration/setup");
    let receipts = child(&workspace, "receipts").unwrap();
    std::fs::create_dir_all(source.join(".config/tilo")).unwrap();
    std::fs::write(source.join(".config/tilo/primary.json"), b"old identity").unwrap();
    std::fs::write(home.join(".config/tilo/primary.json"), b"selected identity").unwrap();
    std::fs::write(source.join("document"), b"my document").unwrap();
    let review = user_transfer::review(
        &NoApps,
        TransferRequest {
            source_home: source.clone(),
            target_home: workspace.join("imported-home"),
            private_journals: receipts,
            copy_home: true,
            copy_flatpak: false,
            system_refs: Vec::new(),
        },
    )
    .unwrap();
    let mut session =
        Session::from_transfer("setup".into(), home.clone(), workspace.clone(), review).unwrap();
    assert!(session.command("adopt").is_err());
    session.command("start").unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while session.state != "ready-to-adopt" {
        assert!(Instant::now() < deadline, "{}", session.pending);
        session.poll().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!home.join("document").exists());
    assert_eq!(
        std::fs::read(workspace.join("imported-home/document")).unwrap(),
        b"my document"
    );
    session.command("adopt").unwrap();
    while session.state != "complete" {
        assert!(Instant::now() < deadline, "{}", session.pending);
        session.poll().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        std::fs::read(home.join("document")).unwrap(),
        b"my document"
    );
    assert_eq!(
        std::fs::read(source.join("document")).unwrap(),
        b"my document"
    );
    assert_eq!(
        std::fs::read(home.join(".config/tilo/primary.json")).unwrap(),
        b"selected identity"
    );
    assert_eq!(
        std::fs::read(workspace.join("imported-home/.config/tilo/primary.json")).unwrap(),
        b"old identity"
    );
    assert_eq!(session.pending["inactive_custody"], 2);
}
#[test]
fn bounded_protocol_refuses_extra_fields_and_oversized_lines() {
    let (tx, rx) = mpsc::sync_channel(8);
    lines(
        io::Cursor::new(b"{\"op\":\"start\",\"source\":\"invented\"}\n".to_vec()),
        tx,
    );
    assert!(rx.recv().unwrap().is_err());
    let (tx, rx) = mpsc::sync_channel(8);
    lines(io::Cursor::new(vec![b'x'; LIMIT + 1]), tx);
    assert!(rx.recv().unwrap().is_err());
}

#[test]
fn native_applications_session_adopts_only_independent_app_data_then_acknowledges() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("old-home");
    let home = temp.path().join("new-home");
    for root in [&source, &home] {
        std::fs::create_dir(root).unwrap();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for relative in [".config/tilo", ".local/state/tilo/migration/apps"] {
        std::fs::create_dir_all(home.join(relative)).unwrap();
        std::fs::set_permissions(home.join(relative), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    std::fs::set_permissions(
        home.join(".local/state/tilo"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::create_dir_all(source.join(".var/app/org.example.Chat/data")).unwrap();
    std::fs::create_dir_all(source.join(".config/tilo")).unwrap();
    std::fs::write(
        source.join(".var/app/org.example.Chat/data/session"),
        b"original app state",
    )
    .unwrap();
    std::fs::write(source.join(".config/tilo/primary.json"), b"old identity").unwrap();
    std::fs::write(source.join("document"), b"shared original document").unwrap();
    std::fs::write(home.join(".config/tilo/primary.json"), b"chosen identity").unwrap();
    let workspace = home.join(".local/state/tilo/migration/apps");
    let review = user_transfer::review(
        &NoApps,
        TransferRequest {
            source_home: source.clone(),
            target_home: workspace.join("imported-applications"),
            private_journals: child(&workspace, "receipts-applications").unwrap(),
            copy_home: false,
            copy_flatpak: true,
            system_refs: Vec::new(),
        },
    )
    .unwrap();
    let mut session =
        Session::from_transfer("apps".into(), home.clone(), workspace.clone(), review).unwrap();
    session.applications_only = true;
    session.adoption_journal = workspace.join("adoption-applications.journal");
    let digest = "a".repeat(64);
    session.application_plan_digest = Some(digest.clone());
    let pin = PinnedDirectory::open_absolute(&workspace).unwrap();
    user_transfer::publish_private_artifact(
        &pin,
        "applications-consent.json",
        &serde_json::to_vec(
            &serde_json::json!({"schema":1,"operation":"apps","original_plan_sha256":digest}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(session.command("adopt").is_err());
    session.command("start").unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while session.state != "ready-to-adopt" {
        assert!(Instant::now() < deadline, "{}", session.pending);
        session.poll().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!home.join(".var/app").exists());
    assert!(!workspace.join("applications-complete.json").exists());
    session.command("adopt").unwrap();
    while session.state != "complete" {
        assert!(Instant::now() < deadline, "{}", session.pending);
        session.poll().unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        std::fs::read(home.join(".var/app/org.example.Chat/data/session")).unwrap(),
        b"original app state"
    );
    std::fs::write(
        home.join(".var/app/org.example.Chat/data/session"),
        b"new app state",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(source.join(".var/app/org.example.Chat/data/session")).unwrap(),
        b"original app state"
    );
    assert!(!home.join("document").exists());
    assert_eq!(
        std::fs::read(source.join("document")).unwrap(),
        b"shared original document"
    );
    assert_eq!(
        std::fs::read(home.join(".config/tilo/primary.json")).unwrap(),
        b"chosen identity"
    );
    let receipt: serde_json::Value = serde_json::from_slice(
        &user_transfer::read_private_artifact(&pin, "applications-complete.json").unwrap(),
    )
    .unwrap();
    assert_eq!(
        receipt,
        serde_json::json!({"schema":1,"operation":"apps","original_plan_sha256":digest,"complete":true})
    );
}
