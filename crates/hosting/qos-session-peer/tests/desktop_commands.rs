use qos_peer_state::{desktop_command::*, owner_commit::*};
use qos_session_peer::SessionOwner;

fn request(owner: &SessionOwner, command: DesktopCommand) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: owner.peer().authority().clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: owner.peer().room().into(),
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
                command,
            },
        },
    }
}

fn tree(owner: &SessionOwner) -> qos_desktop::WorkspaceTree {
    let mut tree = qos_desktop::WorkspaceTree::default();
    qos_desktop::projection::RoomProjection::apply_admitted_room(
        &mut tree,
        qos_desktop::RoomId::parse(owner.peer().room()).unwrap(),
        owner.peer().admitted_room().unwrap(),
    )
    .unwrap();
    tree
}

#[test]
fn owner_commands_create_checked_record_before_topology_and_retain_closed_data_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let strip = tree(&owner).workspaces[0].strips[0].id.0;
    let opening = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "dev.qos.notes".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    let baseline = owner.outgoing().to_vec();
    let receipts =
        qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap()).unwrap();
    let receipt = owner.commit_desktop_command(&opening).unwrap();
    receipt.validate_for(&opening).unwrap();
    assert!(owner.outgoing().starts_with(&baseline));
    assert_eq!(
        owner.outgoing().len() - baseline.len(),
        2,
        "launch publishes room prerequisites and new record only"
    );
    assert_eq!(
        &owner.outgoing_kinds()[baseline.len()..],
        &[
            qos_peer_state::transport::Retention::Delta,
            qos_peer_state::transport::Retention::Checkpoint
        ]
    );
    for (route, receipt) in receipts {
        assert_eq!(
            qos_peer_state::durability::entries(owner.peer().admitted_room().unwrap())
                .unwrap()
                .get(&route),
            Some(&receipt)
        );
    }
    let tree = tree(&owner);
    let group = tree.workspaces[0].strips[0].window_groups.last().unwrap();
    let tab = group.tabs[0].id;
    let record = ui::record::RecordId::for_tab(tab).to_string();
    let data = owner.peer().document("dev.qos.notes", &record).unwrap();
    let close = request(
        &owner,
        DesktopCommand::CloseTab {
            location: TabLocation {
                strip,
                group: group.id,
                tab,
            },
        },
    );
    let before_close = owner.outgoing().len();
    owner.commit_desktop_command(&close).unwrap();
    assert_eq!(
        owner.outgoing().len() - before_close,
        1,
        "closing topology publishes no unchanged applications"
    );
    assert_eq!(
        owner.outgoing_kinds().last(),
        Some(&qos_peer_state::transport::Retention::Delta)
    );
    assert_eq!(
        owner.peer().document("dev.qos.notes", &record).unwrap(),
        data
    );
    let status = owner.status();
    drop(owner);
    let mut restored = SessionOwner::open(&path).unwrap();
    assert_eq!(restored.status(), status);
    assert_eq!(
        restored.peer().document("dev.qos.notes", &record).unwrap(),
        data
    );
    assert!(
        restored.commit_desktop_command(&opening).is_err(),
        "stale command cannot create a second tab"
    );
}

#[test]
fn stale_or_forged_caller_rejection_does_not_change_durable_owner() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let mut forged = request(
        &owner,
        DesktopCommand::CreateStrip {
            name: "Forged".into(),
        },
    );
    if let CommitOperation::DesktopCommand { caller, .. } = &mut forged.operation {
        caller.device_public_key = [55; 32];
    }
    let before = owner.status();
    assert!(matches!(
        owner.commit_desktop_command(&forged),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert_eq!(owner.status(), before);
    let stale = request(
        &owner,
        DesktopCommand::CreateStrip {
            name: "Stale".into(),
        },
    );
    owner
        .commit_desktop_command(&request(
            &owner,
            DesktopCommand::CreateStrip {
                name: "Fresh".into(),
            },
        ))
        .unwrap();
    let fresh = owner.status();
    assert!(matches!(
        owner.commit_desktop_command(&stale),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert_eq!(owner.status(), fresh);
}

fn execute(owner: &mut SessionOwner, command: DesktopCommand) {
    let command = request(owner, command);
    owner
        .commit_desktop_command(&command)
        .unwrap()
        .validate_for(&command)
        .unwrap();
}
fn notes(owner: &SessionOwner) -> TabLocation {
    let tree = tree(owner);
    for strip in &tree.workspaces[0].strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if matches!(&tab.tile, qos_desktop::TileContent::AppPlaceholder { kind: qos_desktop::AppKind::Other(app), .. } if app == "dev.qos.notes")
                {
                    return TabLocation {
                        strip: strip.id.0,
                        group: group.id,
                        tab: tab.id,
                    };
                }
            }
        }
    }
    panic!("Notes missing")
}
#[test]
fn canonical_command_matrix_preserves_record_routes_and_stable_ids_across_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let original = notes(&owner);
    let original_record = qos_desktop::projection::RoomProjection::record_routes(
        owner.peer().admitted_room().unwrap(),
    )
    .get(&original.tab)
    .cloned()
    .unwrap_or_else(|| ui::record::RecordId::for_tab(original.tab).to_string());
    let original_data = owner
        .peer()
        .document("dev.qos.notes", &original_record)
        .unwrap();
    execute(
        &mut owner,
        DesktopCommand::DuplicateTab { location: original },
    );
    let copy = tree(&owner).workspaces[0]
        .strips
        .iter()
        .flat_map(|s| &s.window_groups)
        .find(|g| g.id == original.group)
        .unwrap()
        .tabs
        .last()
        .unwrap()
        .id;
    assert_ne!(copy, original.tab);
    let copy_record = ui::record::RecordId::for_tab(copy).to_string();
    assert!(
        owner
            .peer()
            .document("dev.qos.notes", &copy_record)
            .is_some()
    );
    let copy_location = TabLocation {
        tab: copy,
        ..original
    };
    let mut expected_order = tree(&owner).workspaces[0]
        .strips
        .iter()
        .flat_map(|s| &s.window_groups)
        .find(|g| g.id == original.group)
        .unwrap()
        .tabs
        .iter()
        .map(|t| t.id)
        .collect::<Vec<_>>();
    expected_order.retain(|id| *id != copy);
    let original_index = expected_order
        .iter()
        .position(|id| *id == original.tab)
        .unwrap();
    expected_order.insert(original_index, copy);
    execute(
        &mut owner,
        DesktopCommand::ReorderTab {
            location: copy_location,
            before: Some(original.tab),
        },
    );
    let projected = tree(&owner);
    let tabs = &projected.workspaces[0]
        .strips
        .iter()
        .flat_map(|s| &s.window_groups)
        .find(|g| g.id == original.group)
        .unwrap()
        .tabs;
    assert_eq!(
        tabs.iter().map(|t| t.id).collect::<Vec<_>>(),
        expected_order
    );
    execute(
        &mut owner,
        DesktopCommand::SplitTab {
            location: copy_location,
            before: Some(original.group),
        },
    );
    let copy_group = tree(&owner).workspaces[0]
        .strips
        .iter()
        .flat_map(|s| &s.window_groups)
        .find(|g| g.tabs.iter().any(|t| t.id == copy))
        .unwrap()
        .id;
    execute(
        &mut owner,
        DesktopCommand::ReorderGroup {
            strip: original.strip,
            group: copy_group,
            before: None,
        },
    );
    execute(
        &mut owner,
        DesktopCommand::CreateStrip {
            name: "Second".into(),
        },
    );
    let second = tree(&owner).workspaces[0]
        .strips
        .iter()
        .find(|s| s.name == "Second")
        .unwrap()
        .id
        .0;
    execute(
        &mut owner,
        DesktopCommand::RenameStrip {
            strip: second,
            name: "Renamed".into(),
        },
    );
    execute(
        &mut owner,
        DesktopCommand::MoveTab {
            location: original,
            placement: TabPlacement::NewGroup {
                strip: second,
                after: None,
            },
        },
    );
    // Find original by stable ID, independently of current tab/strip selection.
    let projected = tree(&owner);
    let moved_group = projected.workspaces[0]
        .strips
        .iter()
        .find(|s| s.id.0 == second)
        .unwrap()
        .window_groups[0]
        .id;
    assert_eq!(
        qos_desktop::projection::RoomProjection::record_routes(
            owner.peer().admitted_room().unwrap()
        )
        .get(&original.tab)
        .cloned()
        .unwrap_or_else(|| ui::record::RecordId::for_tab(original.tab).to_string()),
        original_record
    );
    execute(
        &mut owner,
        DesktopCommand::MoveTab {
            location: TabLocation {
                strip: second,
                group: moved_group,
                tab: original.tab,
            },
            placement: TabPlacement::InGroup {
                strip: original.strip,
                group: copy_group,
                after: Some(copy),
            },
        },
    );
    execute(&mut owner, DesktopCommand::RemoveStrip { strip: second });
    execute(
        &mut owner,
        DesktopCommand::RenameWorkspace {
            name: "Shared name".into(),
        },
    );
    assert_eq!(tree(&owner).workspaces[0].name, "Shared name");
    assert_eq!(
        owner
            .peer()
            .document("dev.qos.notes", &original_record)
            .unwrap(),
        original_data
    );
    let status = owner.status();
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.status(), status);
    execute(
        &mut owner,
        DesktopCommand::CloseTab {
            location: TabLocation {
                strip: original.strip,
                group: copy_group,
                tab: copy,
            },
        },
    );
    execute(
        &mut owner,
        DesktopCommand::DuplicateTab {
            location: TabLocation {
                strip: original.strip,
                group: copy_group,
                tab: original.tab,
            },
        },
    );
    assert!(
        tree(&owner).workspaces[0]
            .strips
            .iter()
            .flat_map(|s| &s.window_groups)
            .flat_map(|g| &g.tabs)
            .all(|t| t.id != copy),
        "closed IDs never reused"
    );
    assert!(
        owner
            .peer()
            .document("dev.qos.notes", &copy_record)
            .is_some(),
        "closed data retained"
    );
}

#[test]
fn invalid_command_and_failed_flush_leave_original_topology_and_records_recoverable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let before = owner.status();
    let strip = tree(&owner).workspaces[0].strips[0].id.0;
    let invalid = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "dev.qos.unknown".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    assert!(matches!(
        owner.commit_desktop_command(&invalid),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert!(owner.permits_publication());
    assert_eq!(owner.status(), before);
    let close = request(
        &owner,
        DesktopCommand::CloseTab {
            location: notes(&owner),
        },
    );
    std::fs::rename(path.join("session.bin"), path.join("saved.bin")).unwrap();
    std::fs::create_dir(path.join("session.bin")).unwrap();
    assert!(matches!(
        owner.commit_desktop_command(&close),
        Err(DesktopCommandError::Uncertain(_))
    ));
    assert!(!owner.permits_publication());
    assert_eq!(owner.status(), before);
    drop(owner);
    std::fs::remove_dir(path.join("session.bin")).unwrap();
    std::fs::rename(path.join("saved.bin"), path.join("session.bin")).unwrap();
    let mut owner = SessionOwner::open(&path).unwrap();
    assert_eq!(owner.status(), before);
    owner
        .commit_desktop_command(&close)
        .unwrap()
        .validate_for(&close)
        .unwrap();
}

#[test]
fn caller_revoked_after_request_capture_is_refused_at_execution() {
    use qos_user_identity::{
        RevocationEntry, RevocationList, RevocationReason, WorkspaceDirectory,
        persist::{self, IdentityPaths},
        sign_revocation_entry,
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[34; 32]);
    let now = qos_relay::clock::now_ns();
    let mut enrollment = qos_session_peer::enrollment::EnrollmentAuthority::default();
    let challenge = enrollment
        .challenge(
            &owner,
            qos_peer_state::enrollment::ChallengeRequest {
                device_public_key: hex::encode(key.verifying_key().to_bytes()),
                request_id: hex::encode([34; 32]),
            },
            now,
        )
        .unwrap();
    let proof = qos_peer_state::enrollment::sign(
        &qos_peer_state::enrollment::decode(&challenge.challenge).unwrap(),
        &key,
    )
    .unwrap();
    let certificate = enrollment.complete(&mut owner, proof, now).unwrap();
    let mut captured = request(
        &owner,
        DesktopCommand::CreateStrip {
            name: "Captured before revocation".into(),
        },
    );
    if let CommitOperation::DesktopCommand { caller, .. } = &mut captured.operation {
        *caller = certificate;
    }
    let root_key =
        persist::read_signing_key(&IdentityPaths::under(path.join("identity")).user_privkey)
            .unwrap();
    let mut entry = RevocationEntry {
        user_public_key: root_key.verifying_key().to_bytes(),
        revoked_device_public_key: key.verifying_key().to_bytes(),
        revoked_at_ns: now,
        reason: RevocationReason::Retired,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root_key).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    let candidate = qos_peer_state::StatePeer::restore(
        &owner.peer().durable().unwrap(),
        owner.peer().room(),
        owner.peer().authority().clone(),
        vec![],
        revocations,
        web_time::SystemTime::now(),
    )
    .unwrap();
    let mut directory = WorkspaceDirectory::load(
        root_key.verifying_key().to_bytes(),
        owner.peer().authority().device_public_key,
        owner.directory(),
    )
    .unwrap();
    directory
        .set_member_devices(
            qos_relay::room_workspace_id(owner.peer().room()).unwrap(),
            &[owner.peer().authority().device_public_key],
        )
        .unwrap();
    owner.commit_admission(candidate, directory.save()).unwrap();
    let before = owner.status();
    let error = owner.commit_desktop_command(&captured).unwrap_err();
    assert!(
        matches!(&error, DesktopCommandError::Rejected(reason) if !reason.contains("stale")),
        "current authority check must precede source revision: {error}"
    );
    assert!(owner.permits_publication());
    assert_eq!(owner.status(), before);
}
