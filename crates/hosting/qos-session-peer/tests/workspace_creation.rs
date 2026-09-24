use qos_peer_state::{desktop_command::*, owner_commit::*};
use qos_session_peer::SessionOwner;
use std::{path::Path, process::Command};
use ui::automerge::{ROOT, ReadDoc};

fn initialize(path: &Path, name: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_qos-session-peer"))
        .arg("init-workspace")
        .arg(path)
        .arg(name)
        .output()
        .unwrap()
}

fn assert_name(owner: &SessionOwner, name: &str) {
    let room = owner.peer().admitted_room().unwrap();
    assert_eq!(
        room.get(ROOT, "name").unwrap().unwrap().0.to_str(),
        Some(name)
    );
    let authority = owner.peer().authority();
    let directory = tilo_user_identity::WorkspaceDirectory::load(
        authority.user_public_key,
        authority.device_public_key,
        owner.directory(),
    )
    .unwrap();
    let rows = directory.as_map();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows.values().next().unwrap().name, name);
}

fn open_app(owner: &mut SessionOwner, app: &str, placement: TabPlacement) {
    let room = owner.peer().room().to_owned();
    let request = OwnerCommitRequest {
        room: room.clone(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: owner.peer().authority().clone(),
            command: BoundCommand {
                source: CommandSource {
                    room,
                    heads: owner
                        .peer()
                        .admitted_room()
                        .unwrap()
                        .clone()
                        .get_heads()
                        .into_iter()
                        .map(|h| h.0)
                        .collect(),
                },
                command: DesktopCommand::OpenTab {
                    app: app.into(),
                    placement,
                },
            },
        },
    };
    owner
        .commit_desktop_command(&request)
        .unwrap()
        .validate_for(&request)
        .unwrap();
}

#[test]
fn named_empty_workspace_survives_reopen_and_opens_its_first_application() {
    for app in [
        "dev.qos.notes",
        "dev.qos.word-processor",
        "dev.qos.calculator",
        "dev.qos.chat",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("workspace");
        let result = initialize(&path, "  Research 東京  ");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let status: qos_session_peer::Status = serde_json::from_slice(&result.stdout).unwrap();
        assert!(status.records.is_empty());
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), status);
        assert_name(&owner, "Research 東京");
        assert!(
            qos_desktop::projection::RoomProjection::contents_of(
                owner.peer().admitted_room().unwrap()
            )
            .is_empty()
        );
        assert!(
            qos_desktop::projection::RoomProjection::record_routes(
                owner.peer().admitted_room().unwrap()
            )
            .is_empty()
        );
        drop(owner);
        let mut owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), status);
        open_app(&mut owner, app, TabPlacement::EmptyWorkspace);
        let status = owner.status();
        assert_eq!(status.records.len(), 1);
        assert_eq!(status.records[0].app_id, app);
        drop(owner);
        let owner = SessionOwner::open(&path).unwrap();
        assert_eq!(owner.status(), status);
        assert_name(&owner, "Research 東京");
        assert_eq!(
            qos_desktop::projection::RoomProjection::contents_of(
                owner.peer().admitted_room().unwrap()
            )
            .len(),
            1
        );
    }
}

#[test]
fn invalid_workspace_names_leave_no_state_directory() {
    let root = tempfile::tempdir().unwrap();
    for (index, name) in [
        "".to_owned(),
        "   ".into(),
        "x".repeat(81),
        "a\nb".into(),
        "\tname".into(),
        "name\u{7f}".into(),
    ]
    .iter()
    .enumerate()
    {
        let path = root.path().join(index.to_string());
        let result = initialize(&path, name);
        assert!(!result.status.success(), "accepted invalid name {name:?}");
        assert!(!path.exists(), "invalid name left initialized state");
    }
}

#[test]
fn workspace_creation_never_reseeds_existing_directories() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspace");
    let result = initialize(&path, &"界".repeat(80));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let before = std::fs::read(path.join("session.bin")).unwrap();
    assert!(!initialize(&path, "Replacement").status.success());
    assert_eq!(std::fs::read(path.join("session.bin")).unwrap(), before);
    let existing = root.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("keep"), b"untouched").unwrap();
    assert!(!initialize(&existing, "Replacement").status.success());
    assert_eq!(std::fs::read(existing.join("keep")).unwrap(), b"untouched");
    assert!(!existing.join("session.bin").exists());
}

#[test]
fn separately_created_workspaces_have_independent_room_authorities() {
    let root = tempfile::tempdir().unwrap();
    let mut statuses = Vec::new();
    for name in ["One", "Two"] {
        let path = root.path().join(name);
        let result = initialize(&path, name);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        statuses.push(serde_json::from_slice::<qos_session_peer::Status>(&result.stdout).unwrap());
    }
    assert_ne!(statuses[0].room, statuses[1].room);
    assert_ne!(statuses[0].authority, statuses[1].authority);
}

#[test]
fn reopening_chat_reuses_its_empty_primary_record() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspace");
    let mut owner = SessionOwner::initialize_workspace(&path, "Chat workspace").unwrap();
    open_app(&mut owner, "dev.qos.chat", TabPlacement::EmptyWorkspace);
    let before = owner.peer().document("dev.qos.chat", "").unwrap();
    let doc = ui::automerge::AutoCommit::load(&before).unwrap();
    assert!(chat::doc::ChatDoc::list_messages(&doc, "c_general").is_empty());
    assert_eq!(chat::doc::ChatDoc::list_users(&doc).len(), 1);
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    let strip =
        qos_desktop::projection::RoomProjection::contents_of(owner.peer().admitted_room().unwrap())
            [0]
        .id;
    open_app(
        &mut owner,
        "dev.qos.chat",
        TabPlacement::NewGroup { strip, after: None },
    );
    assert_eq!(owner.status().records.len(), 1);
    assert_eq!(owner.peer().document("dev.qos.chat", "").unwrap(), before);
    let topology =
        qos_desktop::projection::RoomProjection::contents_of(owner.peer().admitted_room().unwrap());
    assert_eq!(topology[0].groups.len(), 2);
}
