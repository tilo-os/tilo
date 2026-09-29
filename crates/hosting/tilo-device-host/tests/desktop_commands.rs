tilo_automerge_isolate::test_worker!();

use tilo_room::{desktop_command::*, owner_commit::*};
use tilo_workspace_host::SessionOwner;

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

fn tree(owner: &SessionOwner) -> tilo_desktop_model::DesktopTree {
    let mut tree = tilo_desktop_model::DesktopTree::default();
    tilo_desktop_model::projection::apply_admitted_room(
        &mut tree,
        tilo_desktop_model::RoomId::parse(owner.peer().room()).unwrap(),
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
            app: "so.tilo.notes".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    let baseline = outgoing_bytes(&owner).to_vec();
    let receipts = tilo_room::durability::entries(owner.peer().admitted_room().unwrap()).unwrap();
    let receipt = owner.commit_desktop_command(&opening).unwrap();
    receipt.validate_for(&opening).unwrap();
    assert!(outgoing_bytes(&owner).starts_with(&baseline));
    assert_eq!(
        outgoing_bytes(&owner).len() - baseline.len(),
        2,
        "launch publishes room prerequisites and new record only"
    );
    assert_eq!(
        &outgoing_kinds(&owner)[baseline.len()..],
        &[
            tilo_room::transport::Retention::Delta,
            tilo_room::transport::Retention::Checkpoint
        ]
    );
    for (route, receipt) in receipts {
        assert_eq!(
            tilo_room::durability::entries(owner.peer().admitted_room().unwrap())
                .unwrap()
                .get(&route),
            Some(&receipt)
        );
    }
    let tree = tree(&owner);
    let group = tree.workspaces[0].strips[0].window_groups.last().unwrap();
    let tab = group.tabs[0].id;
    let record = ui::record::RecordId::for_tab(tab).to_string();
    let data = owner.peer().document("so.tilo.notes", &record).unwrap();
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
    let before_close = outgoing_bytes(&owner).len();
    owner.commit_desktop_command(&close).unwrap();
    assert_eq!(
        outgoing_bytes(&owner).len() - before_close,
        1,
        "closing topology publishes no unchanged applications"
    );
    assert_eq!(
        outgoing_kinds(&owner).last(),
        Some(&tilo_room::transport::Retention::Delta)
    );
    assert_eq!(
        owner.peer().document("so.tilo.notes", &record).unwrap(),
        data
    );
    let status = owner.status();
    drop(owner);
    let mut restored = SessionOwner::open(&path).unwrap();
    assert_eq!(restored.status(), status);
    assert_eq!(
        restored.peer().document("so.tilo.notes", &record).unwrap(),
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

async fn record_effect_receipt(owner: &mut SessionOwner) {
    let invocation = uuid::Uuid::new_v4();
    let receipt = tilo_desktop_model::WorkspaceEffectReceipt {
        invocation: serde_json::from_value(serde_json::json!(invocation)).unwrap(),
        actor: hex::encode(owner.peer().authority().user_public_key),
        context: Default::default(),
        status: serde_json::from_value(serde_json::json!({"status":"succeeded"})).unwrap(),
        completed_at: 42,
    };
    let request = OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::EffectReceipt {
            invocation,
            redacted: serde_json::to_vec(&receipt).unwrap(),
        },
    };
    let (service, mut mailbox) = tilo_workspace_host::owner_commit::OwnerCommitMailbox::bounded(1);
    let acknowledgement = tokio::spawn(service.commit(request.clone()));
    mailbox.recv().await.unwrap().apply(owner).unwrap();
    acknowledgement
        .await
        .unwrap()
        .unwrap()
        .validate_for(&request)
        .unwrap();
}

#[tokio::test]
async fn restart_and_unrelated_room_receipts_do_not_invalidate_open_or_close() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let strip = tree(&owner).workspaces[0].strips[0].id.0;
    let opening = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "so.tilo.notes".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    record_effect_receipt(&mut owner).await;
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    owner
        .commit_desktop_command(&opening)
        .unwrap()
        .validate_for(&opening)
        .unwrap();
    let snapshot = tree(&owner);
    let group = snapshot.workspaces[0].strips[0]
        .window_groups
        .last()
        .unwrap();
    let closing = request(
        &owner,
        DesktopCommand::CloseTab {
            location: TabLocation {
                strip,
                group: group.id,
                tab: group.tabs[0].id,
            },
        },
    );
    record_effect_receipt(&mut owner).await;
    drop(owner);
    let mut owner = SessionOwner::open(&path).unwrap();
    owner
        .commit_desktop_command(&closing)
        .unwrap()
        .validate_for(&closing)
        .unwrap();
    assert!(
        !tree(&owner).workspaces[0].strips[0]
            .window_groups
            .iter()
            .any(|g| g.id == group.id)
    );
}

#[test]
fn unknown_desktop_source_revision_is_rejected_without_mutation() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let mut command = request(
        &owner,
        DesktopCommand::CreateStrip {
            name: "Unknown".into(),
        },
    );
    if let CommitOperation::DesktopCommand { command, .. } = &mut command.operation {
        command.source.heads = vec![[55; 32]];
    }
    let before = owner.status();
    assert!(matches!(
        owner.commit_desktop_command(&command),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert_eq!(owner.status(), before);
}

#[test]
fn changed_nested_desktop_contents_are_rejected_even_without_an_allocator_change() {
    use ui::automerge::{ActorId, ROOT, ReadDoc, transaction::Transactable};
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let command = request(
        &owner,
        DesktopCommand::CloseTab {
            location: notes(&owner),
        },
    );
    let author = owner.peer().authority().device_public_key;
    let mut doc = owner.peer().admitted_room().unwrap().clone();
    let counter_before = doc
        .get(ROOT, "desktop_counter_v1")
        .unwrap()
        .map(|(v, id)| (v.into_owned(), id));
    let contents = doc.get(ROOT, "contents").unwrap().unwrap().1;
    let strip = doc.keys(&contents).next().unwrap();
    let strip = doc.get(&contents, strip).unwrap().unwrap().1;
    doc.set_actor(ActorId::from(author.to_vec()));
    doc.put(&strip, "name", "Changed elsewhere").unwrap();
    doc.commit();
    assert_eq!(doc.get(ROOT, "desktop_counter_v1").unwrap(), counter_before);
    let mut candidate = owner.peer().clone();
    candidate
        .receive_from(
            tilo_room::TransportOrigin::RelayDelta(author),
            tilo_room::NeighbourMsg::SyncResp {
                room: owner.peer().room().into(),
                changes: doc
                    .get_changes(&[])
                    .iter()
                    .map(|c| c.raw_bytes().to_vec())
                    .collect(),
            },
            web_time::SystemTime::now(),
        )
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    let before = owner.status();
    assert!(matches!(
        owner.commit_desktop_command(&command),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert_eq!(owner.status(), before);
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
                if matches!(&tab.tile, tilo_desktop_model::TileContent::AppPlaceholder { kind: tilo_desktop_model::AppKind::Other(app), .. } if app == "so.tilo.notes")
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
    let original_record =
        tilo_desktop_model::apps::record_routes(owner.peer().admitted_room().unwrap())
            .get(&original.tab)
            .cloned()
            .unwrap_or_else(|| ui::record::RecordId::for_tab(original.tab).to_string());
    let original_data = owner
        .peer()
        .document("so.tilo.notes", &original_record)
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
            .document("so.tilo.notes", &copy_record)
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
        tilo_desktop_model::apps::record_routes(owner.peer().admitted_room().unwrap())
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
    execute(
        &mut owner,
        DesktopCommand::SaveShortcut {
            shortcut_json: r#"{"id":"tidy","name":"Tidy","steps":[],"saved_by":"forged"}"#.into(),
        },
    );
    let saved = tree(&owner).workspaces[0].shortcuts.clone();
    assert_eq!(saved.len(), 1, "the room carries the saved shortcut");
    assert!(
        saved[0]
            .saved_by
            .as_deref()
            .is_some_and(|saver| saver != "forged"),
        "the owner records the admitted caller as the saver"
    );
    execute(
        &mut owner,
        DesktopCommand::DeleteShortcut { id: "tidy".into() },
    );
    assert!(tree(&owner).workspaces[0].shortcuts.is_empty());
    assert_eq!(
        owner
            .peer()
            .document("so.tilo.notes", &original_record)
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
            .document("so.tilo.notes", &copy_record)
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
            app: "so.tilo.unknown".into(),
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
    use tilo_user_identity::{
        RevocationEntry, RevocationList, RevocationReason, WorkspaceDirectory,
        persist::{self, IdentityPaths},
        sign_revocation_entry,
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owner");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[34; 32]);
    let now = tilo_delivery_protocol::clock::now_ns();
    let mut enrollment = tilo_workspace_host::enrollment::EnrollmentAuthority::default();
    let challenge = enrollment
        .challenge(
            &owner,
            tilo_room::enrollment::ChallengeRequest {
                device_public_key: hex::encode(key.verifying_key().to_bytes()),
                request_id: hex::encode([34; 32]),
            },
            now,
        )
        .unwrap();
    let proof = tilo_room::enrollment::sign(
        &tilo_room::enrollment::decode(&challenge.challenge).unwrap(),
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
    let root_key = persist::read_signing_key(
        &IdentityPaths::under(
            std::fs::canonicalize(owner.root())
                .unwrap()
                .join("identity"),
        )
        .user_privkey,
    )
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
    let candidate = tilo_room::StatePeer::restore(
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
            tilo_relay::room_workspace_id(owner.peer().room()).unwrap(),
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

#[test]
fn committed_desktop_opens_without_relay_retention_and_rejects_forged_delivery() {
    use tilo_delivery_protocol::transport_v2::EnvelopeV2;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("direct");
    let mut owner = SessionOwner::initialize(&path).unwrap();
    let browser = owner.peer().clone();
    let strip = tree(&owner).workspaces[0].strips[0].id.0;
    let opening = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "so.tilo.notes".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    let (receipt, delivery) = owner
        .commit_desktop_command_with_delivery(&opening)
        .unwrap();
    receipt.validate_for(&opening).unwrap();
    let delivery = delivery.expect("normal app launch has a bounded immediate publication");
    let source_user = owner.peer().authority().user_public_key;
    let target = delivery.target;
    let tilo_delivery_protocol::peer_actions::ActionMessage::CommittedDesktop {
        opened: Some(opened_route),
        ..
    } = &delivery.message
    else {
        panic!("launch carries its exact opened route")
    };
    let new_tab = tree(&owner).workspaces[0].strips[0]
        .window_groups
        .last()
        .unwrap()
        .tabs[0]
        .id;
    assert_eq!(opened_route.tab, new_tab.to_string());
    let event = EnvelopeV2::Delivered {
        action: delivery.clone(),
        source_user,
    };
    let admit = |state: &tilo_room::StatePeer, event: &EnvelopeV2| {
        tilo_browser_runtime::committed_desktop::admit(
            state,
            event,
            target,
            web_time::SystemTime::now(),
        )
    };
    let admitted = |state: &tilo_room::StatePeer, event: &EnvelopeV2| match admit(state, event)
        .unwrap()
    {
        tilo_browser_runtime::committed_desktop::Admission::Ready { candidate, .. } => candidate,
        _ => panic!("committed desktop is ready"),
    };
    let opened = admitted(&browser, &event);
    let new_row = owner
        .peer()
        .rows()
        .into_iter()
        .find(|r| {
            r.app_id == "so.tilo.notes"
                && !browser
                    .rows()
                    .iter()
                    .any(|old| old.app_id == r.app_id && old.record_id == r.record_id)
        })
        .unwrap();
    assert_eq!(
        opened.document(&new_row.app_id, &new_row.record_id),
        owner.peer().document(&new_row.app_id, &new_row.record_id)
    );
    assert_eq!(
        opened.admitted_room().unwrap().clone().get_heads(),
        owner.peer().admitted_room().unwrap().clone().get_heads()
    );
    assert!(
        opened
            .durable_covers_current(&new_row.app_id, &new_row.record_id)
            .unwrap()
    );
    assert_eq!(
        admitted(&opened, &event).durable().unwrap(),
        opened.durable().unwrap(),
        "ordinary duplicate admission is idempotent"
    );
    for field in 0..6 {
        let mut wrong = delivery.clone();
        let mut root = source_user;
        match field {
            0 => wrong.source = [91; 32],
            1 => wrong.target = [91; 32],
            2 => wrong.room = uuid::Uuid::new_v4(),
            3 => root = [91; 32],
            4 => {
                let tilo_delivery_protocol::peer_actions::ActionMessage::CommittedDesktop {
                    packets,
                    ..
                } = &mut wrong.message
                else {
                    panic!()
                };
                let last = packets.last_mut().unwrap();
                let mut bytes = tilo_room::desktop_delivery::decode(std::slice::from_ref(last))
                    .unwrap()
                    .remove(0);
                let end = bytes.len() - 1;
                bytes[end] ^= 1;
                *last = hex::encode(bytes);
            }
            _ => {
                let tilo_delivery_protocol::peer_actions::ActionMessage::CommittedDesktop {
                    opened: Some(opened),
                    ..
                } = &mut wrong.message
                else {
                    panic!()
                };
                opened.tab = u64::MAX.to_string();
            }
        }
        assert!(
            admit(
                &browser,
                &EnvelopeV2::Delivered {
                    action: wrong,
                    source_user: root
                }
            )
            .is_err()
        );
    }
    let opened_tree = tree(&owner);
    let group = opened_tree.workspaces[0].strips[0]
        .window_groups
        .last()
        .unwrap();
    let close = request(
        &owner,
        DesktopCommand::CloseTab {
            location: TabLocation {
                strip,
                group: group.id,
                tab: group.tabs[0].id,
            },
        },
    );
    let (_, close_delivery) = owner.commit_desktop_command_with_delivery(&close).unwrap();
    let closed_event = EnvelopeV2::Delivered {
        action: close_delivery.unwrap(),
        source_user,
    };
    let closed = admitted(&opened, &closed_event);
    assert_eq!(
        closed.admitted_room().unwrap().clone().get_heads(),
        owner.peer().admitted_room().unwrap().clone().get_heads()
    );
    assert_eq!(
        closed.document(&new_row.app_id, &new_row.record_id),
        opened.document(&new_row.app_id, &new_row.record_id),
        "close removes topology without deleting app data"
    );
    drop(owner);
    let owner = SessionOwner::open(&path).unwrap();
    assert_eq!(
        closed.document(&new_row.app_id, &new_row.record_id),
        owner.peer().document(&new_row.app_id, &new_row.record_id)
    );
}

#[test]
fn committed_desktop_catches_up_a_missing_visible_record_without_retained_delivery() {
    use tilo_delivery_protocol::transport_v2::EnvelopeV2;
    use tilo_room::{NeighbourMsg, TransportOrigin};

    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("deferred-direct")).unwrap();
    let mut browser = owner.peer().clone();
    let source = owner.peer().authority().device_public_key;
    let source_user = owner.peer().authority().user_public_key;
    let strip = tree(&owner).workspaces[0].strips[0].id.0;

    let first = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "so.tilo.notes".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    let first_publication = outgoing_bytes(&owner).len();
    owner.commit_desktop_command(&first).unwrap();
    let mut withheld = None;
    for bytes in &outgoing_bytes(&owner)[first_publication..] {
        let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            assert!(withheld.replace(bytes.clone()).is_none());
        } else {
            browser
                .receive_from(
                    TransportOrigin::RetainedRelayDelta(source),
                    message,
                    web_time::SystemTime::now(),
                )
                .unwrap();
        }
    }
    let withheld = withheld.expect("first launch has one retained record prerequisite");
    assert_eq!(
        browser.admitted_room().unwrap().clone().get_heads(),
        owner.peer().admitted_room().unwrap().clone().get_heads(),
        "the browser can know the current command-source topology while its record is still missing"
    );

    let second = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "so.tilo.calculator".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    let expected_invocation = match &second.operation {
        CommitOperation::DesktopCommand { invocation, .. } => *invocation,
        _ => unreachable!(),
    };
    let (_, delivery) = owner.commit_desktop_command_with_delivery(&second).unwrap();
    let delivery = delivery.expect("second launch has direct committed delivery");
    let target = delivery.target;
    let tilo_delivery_protocol::peer_actions::ActionMessage::CommittedDesktop { packets, .. } =
        &delivery.message
    else {
        panic!("second launch keeps its committed desktop delivery")
    };
    assert!(
        packets
            .iter()
            .flat_map(
                |packet| tilo_room::desktop_delivery::decode(std::slice::from_ref(packet)).unwrap()
            )
            .any(|packet| packet == withheld),
        "the direct delivery includes the exact already-persisted prerequisite that the browser missed"
    );
    let expected_opened = match &delivery.message {
        tilo_delivery_protocol::peer_actions::ActionMessage::CommittedDesktop {
            opened: Some(opened),
            ..
        } => opened.clone(),
        _ => panic!("launch carries its exact opened route"),
    };
    let event = EnvelopeV2::Delivered {
        action: delivery,
        source_user,
    };

    let ready = tilo_browser_runtime::committed_desktop::admit(
        &browser,
        &event,
        target,
        web_time::SystemTime::now(),
    )
    .unwrap();
    let tilo_browser_runtime::committed_desktop::Admission::Ready {
        candidate,
        notice: ready_notice,
    } = ready
    else {
        panic!("the direct delivery supplies every visible record prerequisite")
    };
    let ready_notice =
        ready_notice.expect("exact launch notice is retained on the ready candidate");
    assert_eq!(ready_notice.invocation.0, expected_invocation);
    assert_eq!(ready_notice.room.to_string(), browser.room());
    assert_eq!(ready_notice.opened, expected_opened);
    assert_eq!(candidate.rows(), owner.peer().rows());
    let mut admitted = tilo_desktop_model::DesktopTree::default();
    tilo_desktop_model::projection::apply_admitted_room(
        &mut admitted,
        tilo_desktop_model::RoomId::parse(candidate.room()).unwrap(),
        candidate.admitted_room().unwrap(),
    )
    .unwrap();
    assert!(
        admitted
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .any(|candidate_strip| {
                candidate_strip.id.0.to_string() == expected_opened.strip
                    && candidate_strip.window_groups.iter().any(|group| {
                        group.id.to_string() == expected_opened.group
                            && group
                                .tabs
                                .iter()
                                .any(|tab| tab.id.to_string() == expected_opened.tab)
                    })
            })
    );
}

#[test]
fn committed_desktop_emits_exact_launch_notice_when_retained_state_arrives_first() {
    use tilo_delivery_protocol::transport_v2::EnvelopeV2;
    use tilo_room::{NeighbourMsg, TransportOrigin};

    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("retained-first")).unwrap();
    let mut browser = owner.peer().clone();
    let source = owner.peer().authority().device_public_key;
    let source_user = owner.peer().authority().user_public_key;
    let strip = tree(&owner).workspaces[0].strips[0].id.0;
    let opening = request(
        &owner,
        DesktopCommand::OpenTab {
            app: "so.tilo.notes".into(),
            placement: TabPlacement::NewGroup { strip, after: None },
        },
    );
    let invocation = match &opening.operation {
        CommitOperation::DesktopCommand { invocation, .. } => *invocation,
        _ => unreachable!(),
    };
    let publication = outgoing_bytes(&owner).len();
    let (_, delivery) = owner
        .commit_desktop_command_with_delivery(&opening)
        .unwrap();
    let delivery = delivery.expect("launch has direct committed delivery");
    let target = delivery.target;
    let expected_opened = match &delivery.message {
        tilo_delivery_protocol::peer_actions::ActionMessage::CommittedDesktop {
            opened: Some(opened),
            ..
        } => opened.clone(),
        _ => panic!("launch carries its exact opened route"),
    };
    for bytes in &outgoing_bytes(&owner)[publication..] {
        let message: NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        browser
            .receive_from(
                TransportOrigin::RetainedRelayDelta(source),
                message,
                web_time::SystemTime::now(),
            )
            .unwrap();
    }
    let event = EnvelopeV2::Delivered {
        action: delivery,
        source_user,
    };
    let admitted = tilo_browser_runtime::committed_desktop::admit(
        &browser,
        &event,
        target,
        web_time::SystemTime::now(),
    )
    .unwrap();
    let tilo_browser_runtime::committed_desktop::Admission::Ready {
        notice: Some(notice),
        ..
    } = admitted
    else {
        panic!("the private delivery retains its live launch notice after retained admission")
    };
    assert_eq!(notice.invocation.0, invocation);
    assert_eq!(notice.room.to_string(), browser.room());
    assert_eq!(notice.opened, expected_opened);
}

fn outgoing_bytes(owner: &tilo_workspace_host::SessionOwner) -> Vec<Vec<u8>> {
    owner.outgoing().iter().map(|p| p.bytes.to_vec()).collect()
}
fn outgoing_kinds(
    owner: &tilo_workspace_host::SessionOwner,
) -> Vec<tilo_room::transport::Retention> {
    owner.outgoing().iter().map(|p| p.kind).collect()
}

/// The native owner keeps the issuer's command id through validation and
/// commit: its receipt binds exactly that (device, invocation), and a
/// refusal commits nothing and maps to a stable reason.
#[test]
fn native_owner_receipt_binds_the_command_id() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&root.path().join("owner")).unwrap();
    let open = request(
        &owner,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
    );
    let id = DesktopCommandId::of(&open).unwrap();
    assert_eq!(id.device, owner.peer().authority().device_public_key);
    let receipt = owner.commit_desktop_command(&open).unwrap();
    receipt.validate_for(&open).unwrap();
    assert_eq!(
        receipt.binding.target,
        CommitTarget::DesktopCommand {
            invocation: id.invocation
        }
    );
    assert_eq!(id.to_string().parse::<DesktopCommandId>().unwrap(), id);
    let status = owner.status();
    let unknown = request(
        &owner,
        DesktopCommand::OpenApp {
            app: "so.tilo.not-installed".into(),
        },
    );
    let Err(DesktopCommandError::Rejected(message)) = owner.commit_desktop_command(&unknown) else {
        panic!("an unknown application is refused")
    };
    assert_eq!(
        DesktopRefusal::classify(&message),
        DesktopRefusal::UnknownApp
    );
    assert_eq!(owner.status(), status, "a refusal commits nothing");
}
