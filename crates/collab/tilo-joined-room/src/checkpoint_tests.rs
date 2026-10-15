use super::{CheckpointCapture, CheckpointRoute};
use crate::application_record::tests::{Fixture, fixture};
use crate::{ApplicationRecord, DocumentRead, NativeWorkspaceOwner, RoomId, open_file, testing};
use automerge::transaction::Transactable as _;
use collab_types::CommitNamed as _;

fn room() -> RoomId {
    RoomId::parse(testing::ROOM).unwrap()
}

fn routes() -> Vec<CheckpointRoute> {
    vec![
        CheckpointRoute {
            app: "so.tilo.chat".into(),
            record: String::new(),
        },
        CheckpointRoute {
            app: "so.tilo.chat".into(),
            record: "thread".into(),
        },
    ]
}

fn prepare(fixture: &Fixture) {
    for route in routes() {
        let declared = ApplicationRecord::declared(
            &route.app,
            &route.record,
            "chat.workspace",
            &["chat.workspace".into()],
        )
        .unwrap();
        assert!(
            fixture
                .owner
                .prepare_application_record(
                    room(),
                    &fixture.certificate,
                    &fixture.key,
                    &declared,
                    fixture.now,
                )
                .unwrap()
        );
        mutate(fixture, &route, &route.record);
    }
}

fn mutate(fixture: &Fixture, route: &CheckpointRoute, value: &str) {
    let message = fixture
        .owner
        .with_current(room(), |view| {
            let mut document = automerge::AutoCommit::load(
                &view.peer().document(&route.app, &route.record).unwrap(),
            )
            .unwrap();
            document.put(automerge::ROOT, "authored", value).unwrap();
            document.commit_edit("fixture", "authored");
            view.peer().clone().publish_document(
                &route.app,
                &route.record,
                document.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_document(crate::publication::request(&message).unwrap(), fixture.now)
        .unwrap();
}

fn capture(
    owner: &NativeWorkspaceOwner,
    fixture: &Fixture,
    id: uuid::Uuid,
    name: &str,
    routes: &[CheckpointRoute],
) -> Result<super::CheckpointAcknowledgement, String> {
    owner.capture_checkpoint(
        room(),
        &fixture.certificate,
        &fixture.key,
        CheckpointCapture { id, name, routes },
        fixture.now,
    )
}

fn publish_peer_change(
    fixture: &Fixture,
    change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<(), String>,
) {
    let messages = fixture
        .owner
        .with_current(room(), |view| {
            let mut peer = view.peer().clone();
            change(&mut peer)?;
            peer.durable_mutation_messages(
                view.peer(),
                &fixture.key,
                fixture.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        fixture
            .owner
            .receive(
                room(),
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                message,
                fixture.now,
            )
            .unwrap();
    }
}

fn checkpoint_member(fixture: &mut Fixture) -> tilo_user_identity::DeviceCert {
    use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse};
    let key = ed25519_dalek::SigningKey::from_bytes(&[116; 32]);
    let member = testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[117; 32]),
        &key,
        "Checkpoint reader",
    );
    let hint = fixture.governor.hint(iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&fixture.certificate.device_public_key).unwrap(),
    ));
    let invitation = fixture.governor.issue(
        vec![hint],
        join::MemberRole::Editor,
        Some(member.user_public_key),
        None,
        testing::ns(fixture.now)
            .unwrap()
            .checked_add(60_000_000_000)
            .unwrap(),
    );
    let JoinTransportResponse::Challenge(challenge) = fixture.governor.handle(
        member.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: member.clone(),
            request_id: [118; 32],
        }),
    ) else {
        panic!("checkpoint reader challenge refused")
    };
    assert!(matches!(
        fixture.governor.handle(
            member.device_public_key,
            JoinTransportRequest::Complete(join::JoinCompleteRequest {
                proof: join::sign_join_proof(challenge.challenge, &key).unwrap(),
            }),
        ),
        JoinTransportResponse::Complete(_)
    ));
    let frames = fixture
        .governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect::<Vec<_>>();
    for frame in frames {
        fixture
            .owner
            .receive(
                room(),
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                postcard::from_bytes(&frame).unwrap(),
                fixture.now,
            )
            .unwrap();
    }
    member
}

fn generation(owner: &NativeWorkspaceOwner) -> u64 {
    owner
        .with_current(room(), |view| Ok(view.generation()))
        .unwrap()
}

#[test]
fn checkpoint_two_routes_share_generation_and_reopen_retry_pins_original_bytes() {
    let fixture = fixture();
    prepare(&fixture);
    let routes = routes();
    let (generation_before, aggregate, expected) = fixture
        .owner
        .with_current(room(), |view| {
            let mut expected = Vec::new();
            for route in &routes {
                let DocumentRead::Captured(document) = view.read_document(
                    fixture.certificate.device_public_key,
                    &route.app,
                    &route.record,
                    None,
                    64 * 1024,
                    fixture.now,
                )?
                else {
                    panic!("native route must capture")
                };
                expected.push(document);
            }
            Ok((
                view.generation(),
                *blake3::hash(&view.record.peer).as_bytes(),
                expected,
            ))
        })
        .unwrap();
    let id = uuid::Uuid::new_v4();
    let ack = capture(&fixture.owner, &fixture, id, "original", &routes).unwrap();
    assert_eq!(ack.captured_generation, generation_before);
    assert_eq!(ack.publication_generation, generation_before + 1);
    mutate(&fixture, &routes[0], "later first route");
    mutate(&fixture, &routes[1], "later second route");
    let path = fixture.directory.path().join("owner.json");
    let reopened = open_file(&path).unwrap();
    let durable = std::fs::read(&path).unwrap();
    assert_eq!(
        capture(&reopened, &fixture, id, "original", &routes).unwrap(),
        ack
    );
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    let materialized = reopened
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    assert_eq!(materialized.manifest.captured_generation, generation_before);
    assert_eq!(materialized.manifest.captured_aggregate, aggregate);
    assert_eq!(materialized.manifest.entries.len(), 2);
    for (index, expected) in expected.iter().enumerate() {
        let entry = &materialized.manifest.entries[index];
        assert_eq!(entry.route, routes[index]);
        assert_eq!(entry.heads, expected.heads);
        assert_eq!(entry.schema.encoding, "automerge-save-v1");
        assert_eq!(entry.schema.kind, None);
        assert_eq!(entry.schema.surface, None);
        assert_eq!(entry.schema.storage_version, None);
        assert_eq!(materialized.documents[index], expected.bytes);
    }
    assert!(
        capture(&reopened, &fixture, id, "original", &routes[..1])
            .unwrap_err()
            .contains("immutable")
    );
    assert_eq!(std::fs::read(&path).unwrap(), durable);
}

#[test]
fn checkpoint_failed_publication_and_missing_route_leave_no_acknowledgement() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let fixture = fixture();
    prepare(&fixture);
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    let before = generation(&fixture.owner);
    let id = uuid::Uuid::new_v4();
    assert!(
        with_failure(Checkpoint::BeforePublish, || {
            capture(&fixture.owner, &fixture, id, "failed", &routes())
        })
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    assert_eq!(generation(&fixture.owner), before);
    assert!(
        fixture
            .owner
            .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
            .is_err()
    );
    let reopened = open_file(&path).unwrap();
    assert!(
        reopened
            .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
            .is_err()
    );
    let mut missing = routes();
    missing[1].record = "missing".into();
    assert!(capture(&fixture.owner, &fixture, id, "failed", &missing).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    let ack = capture(&fixture.owner, &fixture, id, "failed", &routes()).unwrap();
    assert_eq!(ack.captured_generation, before);
    assert_eq!(ack.publication_generation, before + 1);
}

#[test]
fn checkpoint_missing_or_corrupt_second_snapshot_never_returns_read_find_or_retry_ack() {
    use tilo_peer_state::workspace_checkpoint::{CheckpointCommand, CheckpointRequest};
    let fixture = fixture();
    prepare(&fixture);
    let id = uuid::Uuid::new_v4();
    let ack = capture(&fixture.owner, &fixture, id, "retained", &routes()).unwrap();
    let find = CheckpointRequest {
        room: testing::ROOM.into(),
        invocation: uuid::Uuid::new_v4(),
        caller: fixture.certificate.clone(),
        command: CheckpointCommand::Find {
            name: "retained".into(),
        },
    };
    let resolve = || {
        fixture
            .owner
            .resolve_checkpoint_name(room(), &fixture.certificate, "retained", fixture.now)
    };
    let execute_find = || {
        fixture
            .owner
            .execute_checkpoint(&find, &fixture.key, fixture.now)
    };
    assert_eq!(resolve().unwrap(), ack);
    assert_eq!(execute_find().unwrap(), serde_json::to_value(&ack).unwrap());
    let read = fixture
        .owner
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    let address = blake3::Hash::from_bytes(read.manifest.entries[1].content)
        .to_hex()
        .to_string();
    let block = fixture
        .directory
        .path()
        .join("content")
        .join(address.get(..2).unwrap())
        .join(address);
    let original = std::fs::read(&block).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    std::fs::remove_file(&block).unwrap();
    assert!(resolve().is_err(), "resolution must require every snapshot");
    assert!(execute_find().is_err(), "find must require every snapshot");
    assert!(
        fixture
            .owner
            .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
            .is_err()
    );
    assert!(capture(&fixture.owner, &fixture, id, "retained", &routes()).is_err());
    assert!(
        open_file(&path).is_err(),
        "restore must require every snapshot"
    );
    std::fs::write(&block, b"damaged retained snapshot").unwrap();
    assert!(resolve().is_err(), "resolution must verify every snapshot");
    assert!(execute_find().is_err(), "find must verify every snapshot");
    assert!(
        fixture
            .owner
            .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
            .is_err()
    );
    assert!(capture(&fixture.owner, &fixture, id, "retained", &routes()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    assert!(
        open_file(&path).is_err(),
        "restore must verify every snapshot"
    );
    std::fs::write(block, original).unwrap();
    assert_eq!(resolve().unwrap(), ack);
    assert_eq!(execute_find().unwrap(), serde_json::to_value(&ack).unwrap());
    assert!(open_file(&path).is_ok());
    assert_eq!(
        fixture
            .owner
            .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
            .unwrap()
            .documents,
        read.documents
    );
}

#[test]
fn checkpoint_name_collision_is_refused_but_binding_moves_name_without_moving_content() {
    let fixture = fixture();
    prepare(&fixture);
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let first_ack = capture(&fixture.owner, &fixture, first, "latest", &routes()).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    assert!(capture(&fixture.owner, &fixture, second, "latest", &routes()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    mutate(&fixture, &routes()[0], "new checkpoint");
    let second_ack = capture(&fixture.owner, &fixture, second, "second", &routes()).unwrap();
    fixture
        .owner
        .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, "latest", second)
        .unwrap();
    let before = generation(&fixture.owner);
    fixture
        .owner
        .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, "latest", second)
        .unwrap();
    assert_eq!(generation(&fixture.owner), before);
    let reopened = open_file(&path).unwrap();
    assert_eq!(
        reopened
            .resolve_checkpoint_name(room(), &fixture.certificate, "latest", fixture.now)
            .unwrap(),
        second_ack
    );
    assert_eq!(
        capture(&reopened, &fixture, first, "latest", &routes()).unwrap(),
        first_ack
    );
    assert!(capture(&reopened, &fixture, first, "changed", &routes()).is_err());
    reopened
        .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, "first", first)
        .unwrap();
    assert_eq!(
        reopened
            .resolve_checkpoint_name(room(), &fixture.certificate, "first", fixture.now)
            .unwrap(),
        first_ack
    );
    assert!(
        reopened
            .bind_checkpoint_name(
                room(),
                &fixture.certificate,
                &fixture.key,
                "absent",
                uuid::Uuid::new_v4()
            )
            .is_err()
    );
}

fn member_routes() -> Vec<CheckpointRoute> {
    ["first", "thread"]
        .into_iter()
        .map(|record| CheckpointRoute {
            app: "so.tilo.chat".into(),
            record: record.into(),
        })
        .collect()
}

fn checkpoint_reader_fixture() -> (
    Fixture,
    tilo_user_identity::DeviceCert,
    Vec<tilo_action_policy::Grant>,
) {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};
    use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
    let mut fixture = fixture();
    let member = checkpoint_member(&mut fixture);
    prepare(&fixture);
    let declared = ApplicationRecord::declared(
        "so.tilo.chat",
        "first",
        "chat.workspace",
        &["chat.workspace".into()],
    )
    .unwrap();
    assert!(
        fixture
            .owner
            .prepare_application_record(
                room(),
                &fixture.certificate,
                &fixture.key,
                &declared,
                fixture.now,
            )
            .unwrap()
    );
    mutate(&fixture, &member_routes()[0], "first captured route");
    let subject = Subject::person(
        blake3::Hash::from_bytes(member.user_public_key)
            .to_hex()
            .as_str(),
    );
    let issuer = Subject::person(
        blake3::Hash::from_bytes(fixture.certificate.user_public_key)
            .to_hex()
            .as_str(),
    );
    // The app-wide denial makes the two explicit route grants necessary,
    // rather than silently relying on the member role's inherited Read.
    let mut grants = vec![Grant::new(
        "checkpoint-room-deny",
        subject.clone(),
        TreePath::room(testing::ROOM).app("so.tilo.chat"),
        KindSet::READ,
        Answer::Deny,
        issuer.clone(),
    )];
    for (index, route) in member_routes().iter().enumerate() {
        grants.push(Grant::new(
            format!("checkpoint-route-{index}"),
            subject.clone(),
            collab_types::document_node(testing::ROOM, &route.app, &route.record),
            KindSet::READ,
            Answer::Allow,
            issuer.clone(),
        ));
    }
    for grant in &grants {
        fixture
            .owner
            .commit_workspace_membership_command(
                tilo_owner_commit::OwnerCommitRequest {
                    room: testing::ROOM.into(),
                    operation: tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
                        invocation: uuid::Uuid::new_v4(),
                        caller: fixture.certificate.clone(),
                        command: WorkspaceMembershipCommand::Grant {
                            grant: grant.clone(),
                        },
                    },
                },
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
            .unwrap_or_else(|error| panic!("grant {}: {error}", grant.id.as_str()));
    }
    (fixture, member, grants)
}

fn capture_member(
    fixture: &Fixture,
    owner: &NativeWorkspaceOwner,
    caller: &tilo_user_identity::DeviceCert,
    id: uuid::Uuid,
) -> Result<super::CheckpointAcknowledgement, String> {
    owner.capture_checkpoint(
        room(),
        caller,
        &fixture.key,
        CheckpointCapture {
            id,
            name: "member",
            routes: &member_routes(),
        },
        fixture.now,
    )
}

#[test]
fn checkpoint_public_commands_bind_original_invocation_and_caller() {
    use tilo_peer_state::workspace_checkpoint::{CheckpointCommand, CheckpointRequest};
    let fixture = fixture();
    prepare(&fixture);
    let request = CheckpointRequest {
        room: testing::ROOM.into(),
        invocation: uuid::Uuid::new_v4(),
        caller: fixture.certificate.clone(),
        command: CheckpointCommand::Capture {
            name: "public".into(),
            routes: routes(),
        },
    };
    let execute = |request: &CheckpointRequest| {
        fixture
            .owner
            .execute_checkpoint(request, &fixture.key, fixture.now)
    };
    let first = execute(&request).unwrap();
    let reopened = open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(
        reopened
            .execute_checkpoint(&request, &fixture.key, fixture.now)
            .unwrap(),
        first
    );
    let mut changed = request.clone();
    changed.command = CheckpointCommand::Capture {
        name: "changed".into(),
        routes: routes(),
    };
    assert!(execute(&changed).is_err());
    changed = request.clone();
    changed.caller.device_label = "substitution".into();
    assert!(execute(&changed).is_err());
    let mut inspect = request.clone();
    inspect.invocation = uuid::Uuid::new_v4();
    inspect.command = CheckpointCommand::Inspect {
        id: request.invocation,
    };
    let metadata = execute(&inspect).unwrap();
    assert!(metadata.get("documents").is_none());
    assert_eq!(
        metadata["author"],
        serde_json::json!(fixture.certificate.user_public_key)
    );
    let mut binding = inspect.clone();
    binding.invocation = uuid::Uuid::new_v4();
    binding.command = CheckpointCommand::NameBind {
        name: "moving".into(),
        id: request.invocation,
    };
    let original_binding = execute(&binding).unwrap();
    let mut second = request;
    second.invocation = uuid::Uuid::new_v4();
    second.command = CheckpointCommand::Capture {
        name: "second".into(),
        routes: routes(),
    };
    execute(&second).unwrap();
    let mut movement = binding.clone();
    movement.invocation = uuid::Uuid::new_v4();
    movement.command = CheckpointCommand::NameBind {
        name: "moving".into(),
        id: second.invocation,
    };
    execute(&movement).unwrap();
    assert_eq!(
        execute(&binding).unwrap(),
        original_binding,
        "retry must not move the name back"
    );
    let mut find = inspect;
    find.command = CheckpointCommand::Find {
        name: "moving".into(),
    };
    assert_eq!(execute(&find).unwrap()["id"], second.invocation.to_string());
    changed = movement;
    changed.command = binding.command;
    assert!(
        execute(&changed).is_err(),
        "one invocation cannot name a different checkpoint"
    );
}

fn deny_checkpoint_naming(fixture: &Fixture, member: &tilo_user_identity::DeviceCert) {
    // This member's fixture role includes writing; explicitly revoke room naming.
    fixture
        .owner
        .commit_workspace_membership_command(
            tilo_owner_commit::OwnerCommitRequest {
                room: testing::ROOM.into(),
                operation: tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: fixture.certificate.clone(),
                    command:
                        tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Grant {
                            grant: tilo_action_policy::Grant::new(
                                "checkpoint-no-naming",
                                tilo_action_policy::Subject::person(
                                    blake3::Hash::from_bytes(member.user_public_key)
                                        .to_hex()
                                        .as_str(),
                                ),
                                tilo_action_policy::TreePath::room(testing::ROOM),
                                tilo_action_policy::KindSet::UNDOABLE,
                                tilo_action_policy::Answer::Deny,
                                tilo_action_policy::Subject::person(
                                    blake3::Hash::from_bytes(fixture.certificate.user_public_key)
                                        .to_hex()
                                        .as_str(),
                                ),
                            ),
                        },
                },
            },
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
}

#[test]
fn checkpoint_public_read_and_mutation_require_current_source_rights() {
    use tilo_peer_state::workspace_checkpoint::{CheckpointCommand, CheckpointRequest};
    let (fixture, member, grants) = checkpoint_reader_fixture();
    let capture = CheckpointRequest {
        room: testing::ROOM.into(),
        invocation: uuid::Uuid::new_v4(),
        caller: fixture.certificate.clone(),
        command: CheckpointCommand::Capture {
            name: "public-member".into(),
            routes: member_routes(),
        },
    };
    fixture
        .owner
        .execute_checkpoint(&capture, &fixture.key, fixture.now)
        .unwrap();
    let mut read = capture.clone();
    read.invocation = uuid::Uuid::new_v4();
    read.caller = member.clone();
    read.command = CheckpointCommand::Inspect {
        id: capture.invocation,
    };
    fixture
        .owner
        .execute_checkpoint(&read, &fixture.key, fixture.now)
        .unwrap();
    let mut other_caller = capture.clone();
    other_caller.caller = member.clone();
    assert!(
        fixture
            .owner
            .execute_checkpoint(&other_caller, &fixture.key, fixture.now)
            .unwrap_err()
            .contains("another command or caller")
    );
    deny_checkpoint_naming(&fixture, &member);
    assert!(capture_member(&fixture, &fixture.owner, &member, uuid::Uuid::new_v4()).is_err());
    assert!(
        fixture
            .owner
            .bind_checkpoint_name(
                room(),
                &member,
                &fixture.key,
                "host-no-bypass",
                capture.invocation,
            )
            .is_err()
    );
    let mut denied_capture = capture.clone();
    denied_capture.caller = member;
    denied_capture.invocation = uuid::Uuid::new_v4();
    assert!(
        fixture
            .owner
            .execute_checkpoint(&denied_capture, &fixture.key, fixture.now)
            .is_err()
    );
    let mut naming = read.clone();
    naming.command = CheckpointCommand::NameBind {
        name: "not-authorized".into(),
        id: capture.invocation,
    };
    assert!(
        fixture
            .owner
            .execute_checkpoint(&naming, &fixture.key, fixture.now)
            .is_err()
    );
    revoke_membership_grant(&fixture, grants[2].id.clone());
    assert!(
        fixture
            .owner
            .execute_checkpoint(&read, &fixture.key, fixture.now)
            .is_err()
    );
    read.command = CheckpointCommand::Find {
        name: "public-member".into(),
    };
    assert!(
        fixture
            .owner
            .execute_checkpoint(&read, &fixture.key, fixture.now)
            .is_err()
    );
}

/// Revoke `grant` through the owner's membership command path.
fn revoke_membership_grant(fixture: &Fixture, grant: tilo_action_policy::GrantId) {
    fixture
        .owner
        .commit_workspace_membership_command(
            tilo_owner_commit::OwnerCommitRequest {
                room: testing::ROOM.into(),
                operation: tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: fixture.certificate.clone(),
                    command:
                        tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Revoke {
                            grant,
                        },
                },
            },
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
}

#[test]
fn checkpoint_current_member_route_revocation_gates_reads_and_retries() {
    let (fixture, member, grants) = checkpoint_reader_fixture();
    let id = uuid::Uuid::new_v4();
    let ack = capture_member(&fixture, &fixture.owner, &member, id).unwrap();
    let original = fixture
        .owner
        .read_checkpoint(room(), &member, id, fixture.now)
        .unwrap();
    assert_eq!(
        capture_member(&fixture, &fixture.owner, &member, id).unwrap(),
        ack
    );
    let path = fixture.directory.path().join("owner.json");
    revoke_membership_grant(&fixture, grants[2].id.clone());
    let reopened = open_file(&path).unwrap();
    let durable = std::fs::read(&path).unwrap();
    for owner in [&fixture.owner, &reopened] {
        owner
            .with_current(room(), |view| {
                view.peer().workspace().check_checkpoint_reader(
                    &member,
                    &member_routes()[0].app,
                    &member_routes()[0].record,
                    fixture.now,
                )?;
                assert!(
                    view.peer()
                        .workspace()
                        .check_checkpoint_reader(
                            &member,
                            &member_routes()[1].app,
                            &member_routes()[1].record,
                            fixture.now
                        )
                        .is_err()
                );
                Ok(())
            })
            .unwrap();
        assert!(
            owner
                .read_checkpoint(room(), &member, id, fixture.now)
                .is_err()
        );
        assert!(
            capture_member(&fixture, owner, &member, id).is_err(),
            "historic Read must not authorize an immutable retry"
        );
        assert_eq!(
            owner
                .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
                .unwrap()
                .documents,
            original.documents
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), durable);
}

#[test]
fn checkpoint_certificate_evidence_gates_reads_and_retries() {
    let (fixture, member, _) = checkpoint_reader_fixture();
    let id = uuid::Uuid::new_v4();
    let ack = capture_member(&fixture, &fixture.owner, &member, id).unwrap();
    assert!(
        fixture
            .owner
            .read_checkpoint(room(), &member, id, fixture.now)
            .is_ok()
    );
    assert_eq!(
        capture_member(&fixture, &fixture.owner, &member, id).unwrap(),
        ack
    );
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    let mut substituted = member.clone();
    substituted.device_label.push_str(" substituted");
    assert!(
        fixture
            .owner
            .read_checkpoint(room(), &substituted, id, fixture.now)
            .is_err()
    );
    assert!(capture_member(&fixture, &fixture.owner, &substituted, id).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    publish_peer_change(&fixture, |peer| {
        assert!(peer.governance_mut().remove_member(
            member.user_public_key,
            &fixture.key,
            fixture.now
        )?);
        Ok(())
    });
    let reopened = open_file(&path).unwrap();
    assert!(
        reopened
            .read_checkpoint(room(), &member, id, fixture.now)
            .is_err()
    );
    assert!(
        capture_member(&fixture, &reopened, &member, id).is_err(),
        "removed certificate evidence is stale"
    );
}

#[test]
fn checkpoint_signed_root_outgoing_reaches_governor_and_names_and_snapshots_reopen() {
    let mut fixture = fixture();
    prepare(&fixture);
    let id = uuid::Uuid::new_v4();
    let ack = capture(&fixture.owner, &fixture, id, "original", &routes()).unwrap();
    fixture
        .owner
        .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, "alias", id)
        .unwrap();
    let before = fixture
        .owner
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    let root = fixture
        .owner
        .with_current(room(), |view| {
            view.peer().workspace().workspace_checkpoint_root()
        })
        .unwrap();
    assert!(root.is_some());
    let reopened = open_file(&fixture.directory.path().join("owner.json")).unwrap();
    let frames = reopened
        .with_outgoing(room(), fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert!(!frames.is_empty(), "signed publication must survive reopen");
    for frame in frames {
        fixture
            .governor
            .receive(
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    assert_eq!(
        fixture
            .governor
            .peer()
            .workspace()
            .workspace_checkpoint_root()
            .unwrap(),
        root
    );
    for name in ["original", "alias"] {
        assert_eq!(
            reopened
                .resolve_checkpoint_name(room(), &fixture.certificate, name, fixture.now)
                .unwrap(),
            ack
        );
    }
    let after = reopened
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&after.manifest).unwrap(),
        serde_json::to_vec(&before.manifest).unwrap()
    );
    assert_eq!(after.documents, before.documents);
}

#[test]
fn checkpoint_stale_signed_publication_cannot_rewind_current_root_or_name() {
    let fixture = fixture();
    prepare(&fixture);
    let first = uuid::Uuid::new_v4();
    capture(&fixture.owner, &fixture, first, "moving", &routes()).unwrap();
    let stale = fixture
        .owner
        .with_outgoing(room(), fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert!(!stale.is_empty());
    let second = uuid::Uuid::new_v4();
    let ack = capture(&fixture.owner, &fixture, second, "second", &routes()).unwrap();
    fixture
        .owner
        .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, "moving", second)
        .unwrap();
    let root = fixture
        .owner
        .with_current(room(), |view| {
            view.peer().workspace().workspace_checkpoint_root()
        })
        .unwrap();
    for bytes in stale {
        // The receiver may classify an old signed frame as duplicate or refuse
        // it; neither classification may reinstall its older checkpoint root.
        let _ = fixture.owner.receive(
            room(),
            tilo_peer_state::TransportOrigin::RetainedNativePublication {
                publisher: fixture.certificate.device_public_key,
            },
            postcard::from_bytes(&bytes).unwrap(),
            fixture.now,
        );
        assert_eq!(
            fixture
                .owner
                .with_current(room(), |view| view
                    .peer()
                    .workspace()
                    .workspace_checkpoint_root())
                .unwrap(),
            root
        );
    }
    let reopened = open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(
        reopened
            .resolve_checkpoint_name(room(), &fixture.certificate, "moving", fixture.now)
            .unwrap(),
        ack
    );
}

#[test]
fn checkpoint_full_name_index_refuses_capture_without_damaging_existing_checkpoint() {
    let fixture = fixture();
    prepare(&fixture);
    let id = uuid::Uuid::new_v4();
    let ack = capture(&fixture.owner, &fixture, id, "original", &routes()).unwrap();
    for index in 0..127 {
        fixture
            .owner
            .bind_checkpoint_name(
                room(),
                &fixture.certificate,
                &fixture.key,
                &format!("alias-{index}"),
                id,
            )
            .unwrap();
    }
    let original = fixture
        .owner
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    let before = generation(&fixture.owner);
    let rejected = uuid::Uuid::new_v4();
    assert!(
        capture(&fixture.owner, &fixture, rejected, "overflow", &routes())
            .unwrap_err()
            .contains("capacity")
    );
    assert_eq!(generation(&fixture.owner), before);
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    let reopened = open_file(&path).unwrap();
    for owner in [&fixture.owner, &reopened] {
        assert_eq!(
            owner
                .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
                .unwrap()
                .documents,
            original.documents
        );
        assert_eq!(
            capture(owner, &fixture, id, "original", &routes()).unwrap(),
            ack
        );
        for name in ["original", "alias-0", "alias-126"] {
            assert_eq!(
                owner
                    .resolve_checkpoint_name(room(), &fixture.certificate, name, fixture.now)
                    .unwrap(),
                ack
            );
        }
        assert!(
            owner
                .read_checkpoint(room(), &fixture.certificate, rejected, fixture.now)
                .is_err()
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), durable);
}

#[test]
fn checkpoint_name_binding_verifies_damaged_snapshot_even_for_noop() {
    let fixture = fixture();
    prepare(&fixture);
    let id = uuid::Uuid::new_v4();
    capture(&fixture.owner, &fixture, id, "original", &routes()).unwrap();
    let original = fixture
        .owner
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    let address = blake3::Hash::from_bytes(original.manifest.entries[1].content)
        .to_hex()
        .to_string();
    let block = fixture
        .directory
        .path()
        .join("content")
        .join(address.get(..2).unwrap())
        .join(address);
    let bytes = std::fs::read(&block).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    let before = generation(&fixture.owner);
    std::fs::write(&block, b"damaged retained snapshot").unwrap();
    for name in ["original", "new alias"] {
        assert!(
            fixture
                .owner
                .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, name, id)
                .is_err(),
            "binding {name} must verify every snapshot, including an unchanged binding"
        );
    }
    assert_eq!(generation(&fixture.owner), before);
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    std::fs::write(&block, bytes).unwrap();
    fixture
        .owner
        .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, "original", id)
        .unwrap();
    assert_eq!(generation(&fixture.owner), before);
    assert_eq!(std::fs::read(&path).unwrap(), durable);
    assert_eq!(
        open_file(&path)
            .unwrap()
            .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
            .unwrap()
            .documents,
        original.documents
    );
}

#[test]
fn checkpoint_captures_full_stored_schema_identity() {
    let fixture = fixture();
    prepare(&fixture);
    let route = &routes()[0];
    let message = fixture
        .owner
        .with_current(room(), |view| {
            let mut document = automerge::AutoCommit::load(
                &view.peer().document(&route.app, &route.record).unwrap(),
            )
            .unwrap();
            let identity = document
                .put_object(automerge::ROOT, "tilo:record", automerge::ObjType::Map)
                .unwrap();
            document.put(&identity, "kind", "chat.workspace").unwrap();
            document.put(&identity, "surface", "so.tilo.chat").unwrap();
            document
                .put(automerge::ROOT, "tilo:version", 7_u64)
                .unwrap();
            document.commit_edit("fixture", "stored schema identity");
            view.peer().clone().publish_document(
                &route.app,
                &route.record,
                document.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_document(crate::publication::request(&message).unwrap(), fixture.now)
        .unwrap();
    let id = uuid::Uuid::new_v4();
    capture(&fixture.owner, &fixture, id, "schema", &routes()).unwrap();
    mutate(&fixture, route, "later edit");
    let reopened = open_file(&fixture.directory.path().join("owner.json")).unwrap();
    let checkpoint = reopened
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    let schema = &checkpoint.manifest.entries[0].schema;
    assert_eq!(schema.encoding, "automerge-save-v1");
    assert_eq!(schema.kind.as_deref(), Some("chat.workspace"));
    assert_eq!(schema.surface.as_deref(), Some("so.tilo.chat"));
    assert_eq!(schema.storage_version, Some(7));
}

fn enroll_checkpoint_reader(
    fixture: &mut Fixture,
    path: &std::path::Path,
) -> tilo_user_identity::DeviceCert {
    use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse};
    let key = ed25519_dalek::SigningKey::from_bytes(&[116; 32]);
    let member = testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[117; 32]),
        &key,
        "Checkpoint reader",
    );
    let hint = fixture.governor.hint(iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&fixture.certificate.device_public_key).unwrap(),
    ));
    let now_ns = testing::ns(fixture.now).unwrap();
    let invitation = fixture.governor.issue(
        vec![hint.clone()],
        join::MemberRole::Editor,
        Some(member.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(member.user_public_key),
        &tilo_user_identity::RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let JoinTransportResponse::Challenge(challenge) = fixture.governor.handle(
        member.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: member.clone(),
            request_id: [118; 32],
        }),
    ) else {
        panic!("checkpoint reader challenge refused")
    };
    let complete = join::JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &key).unwrap(),
    };
    let intent = crate::VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        member.clone(),
        hint,
        fixture.now,
    )
    .unwrap();
    let owner = open_file(path).unwrap();
    let pending = owner
        .prepare_enrollment_into(&intent, None, fixture.now)
        .unwrap();
    let JoinTransportResponse::Complete(bootstrap) = fixture.governor.handle(
        member.device_public_key,
        JoinTransportRequest::Complete(complete),
    ) else {
        panic!("checkpoint reader completion refused")
    };
    owner
        .install(
            &testing::finalize(intent, pending, bootstrap).unwrap(),
            fixture.now,
        )
        .unwrap();
    member
}

#[test]
fn checkpoint_receiving_member_reads_hydrated_signed_index_with_different_generation() {
    let mut fixture = fixture();
    prepare(&fixture);
    let id = uuid::Uuid::new_v4();
    let ack = capture(&fixture.owner, &fixture, id, "original", &routes()).unwrap();
    let original = fixture
        .owner
        .read_checkpoint(room(), &fixture.certificate, id, fixture.now)
        .unwrap();
    let frames = fixture
        .owner
        .with_outgoing(room(), fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    for frame in frames {
        fixture
            .governor
            .receive(
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    let directory = tempfile::tempdir().unwrap();
    // Hydrate the member's independent content store with the addressed blocks.
    // The checkpoint root itself must arrive through signed enrollment state.
    for prefix in std::fs::read_dir(fixture.directory.path().join("content")).unwrap() {
        let prefix = prefix.unwrap();
        if !prefix.file_type().unwrap().is_dir() {
            continue;
        }
        let destination = directory.path().join("content").join(prefix.file_name());
        std::fs::create_dir_all(&destination).unwrap();
        for block in std::fs::read_dir(prefix.path()).unwrap() {
            let block = block.unwrap();
            std::fs::copy(block.path(), destination.join(block.file_name())).unwrap();
        }
    }
    let path = directory.path().join("member.json");
    let member = enroll_checkpoint_reader(&mut fixture, &path);
    let receiver = open_file(&path).unwrap();
    assert!(ack.publication_generation > generation(&receiver));
    receiver
        .with_current(room(), |view| {
            assert_eq!(view.device(), &member);
            assert_ne!(view.device(), view.governor());
            assert_eq!(
                view.peer().workspace().workspace_checkpoint_root()?,
                fixture
                    .governor
                    .peer()
                    .workspace()
                    .workspace_checkpoint_root()?
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        receiver
            .resolve_checkpoint_name(room(), &member, "original", fixture.now)
            .unwrap(),
        ack
    );
    let materialized = receiver
        .read_checkpoint(room(), &member, id, fixture.now)
        .unwrap();
    assert_eq!(materialized.documents, original.documents);
    assert_eq!(
        serde_json::to_vec(&materialized.manifest).unwrap(),
        serde_json::to_vec(&original.manifest).unwrap()
    );
}

#[test]
fn checkpoint_invalid_bounds_duplicate_routes_and_wrong_custody_do_not_publish() {
    let fixture = fixture();
    prepare(&fixture);
    let path = fixture.directory.path().join("owner.json");
    let durable = std::fs::read(&path).unwrap();
    let id = uuid::Uuid::new_v4();
    for name in [String::new(), "x".repeat(257), "bad\nname".into()] {
        assert!(capture(&fixture.owner, &fixture, id, &name, &routes()).is_err());
        assert!(
            fixture
                .owner
                .bind_checkpoint_name(room(), &fixture.certificate, &fixture.key, &name, id)
                .is_err()
        );
    }
    let mut reversed = routes();
    reversed.reverse();
    let invalid = [
        Vec::new(),
        vec![routes()[0].clone(); 33],
        vec![routes()[0].clone(); 2],
        reversed,
        vec![CheckpointRoute {
            app: String::new(),
            record: String::new(),
        }],
        vec![CheckpointRoute {
            app: "a".repeat(257),
            record: String::new(),
        }],
        vec![CheckpointRoute {
            app: "so.tilo.chat".into(),
            record: "x".repeat(4097),
        }],
        vec![CheckpointRoute {
            app: "so.tilo.chat".into(),
            record: "bad\nroute".into(),
        }],
    ];
    for routes in invalid {
        assert!(capture(&fixture.owner, &fixture, id, "invalid", &routes).is_err());
    }
    assert!(
        capture(
            &fixture.owner,
            &fixture,
            uuid::Uuid::nil(),
            "nil",
            &routes()
        )
        .is_err()
    );
    assert!(
        fixture
            .owner
            .capture_checkpoint(
                room(),
                &fixture.certificate,
                &ed25519_dalek::SigningKey::from_bytes(&[8; 32]),
                CheckpointCapture {
                    id,
                    name: "wrong custody",
                    routes: &routes()
                },
                fixture.now,
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), durable);
}
