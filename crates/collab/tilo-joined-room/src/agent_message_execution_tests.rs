//! Genuine signed-package/runtime, private-channel and kernel custody tests.
//! No provider/model is invoked: retained fixture output is not model evidence.
use super::*;
use crate::AgentSessionReservation;
use crate::agent_message::{MessageAttention, MessagePhase};
use crate::agent_message_commit::tests::{add_agent, publisher, source};
use crate::application_record::tests::{Fixture, fixture};
use launcher::{
    managed::{
        ManagedBinding, ManagedLaunchAuthority, ManagedLaunchEvent, ManagedLaunchScope,
        ManagedLauncher, VerifiedManagedSupervisor,
        channel::{ManagedProcessAuthority, PendingManagedConnection},
        test_support::{fixture_package, fixture_runtime},
    },
    services::{EnrolledServiceContext, ManagedProviderSelection},
};
use std::{
    io::Read,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

struct OwnerAuthority {
    owner: NativeWorkspaceOwner,
    attestor: DeviceCert,
    session: OnceLock<AgentSessionBinding>,
    now: SystemTime,
}

impl OwnerAuthority {
    fn exact_session(&self, binding: &ManagedBinding) -> Result<&AgentSessionBinding, String> {
        let session = self.session.get().ok_or("fixture owner session absent")?;
        if binding.room != session.room.to_string()
            || binding.app != launcher::managed::WORKSPACE_AGENT_PACKAGE_ID
            || binding.agent != session.agent
            || binding.scope != ManagedLaunchScope::AgentSession
            || binding.launch != session.launch
            || binding.generation != session.generation
        {
            return Err("fixture live launch differs from owner session".into());
        }
        Ok(session)
    }
}

impl ManagedLaunchAuthority for OwnerAuthority {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        self.owner
            .claim_agent_session(self.exact_session(binding)?, &self.attestor, self.now)
    }

    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        self.owner
            .revalidate_agent_session(self.exact_session(binding)?, &self.attestor, self.now)
    }
}

impl ManagedProcessAuthority for OwnerAuthority {
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String> {
        let facts = process.facts()?;
        let session = self.exact_session(&facts.binding)?;
        if facts.runtime.runtime_epoch != session.runtime_epoch
            || runtime_digest(&facts.runtime)? != session.runtime_digest
        {
            return Err("fixture process runtime differs from owner session".into());
        }
        self.owner.bind_agent_session_process(
            session,
            &self.attestor,
            process_digest(&facts)?,
            self.now,
        )
    }
}

struct Running {
    launcher: ManagedLauncher,
    service: AgentMessageHostService,
    process: VerifiedManagedProcess,
    _model: UnixStream,
    _identity: UnixStream,
    _collab: UnixStream,
}

fn started(launcher: &mut ManagedLauncher) -> VerifiedManagedSupervisor {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let events = launcher.drain_events().unwrap();
        if !events.is_empty() {
            assert_eq!(events.len(), 1, "one actual supervisor event required");
            let event = events.into_iter().next().unwrap();
            match event {
                ManagedLaunchEvent::Started(supervisor) => return supervisor,
                _ => panic!("actual fixture did not start"),
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual launch timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn launch(fixture: &Fixture) -> Running {
    launch_for(fixture, "scribe")
}

fn launch_for(fixture: &Fixture, agent: &str) -> Running {
    // The explicit qualification build must supply actual same-worktree binaries.
    // Missing binaries fail; never substitute a shell script or arbitrary PID.
    let executable = std::env::current_exe().unwrap();
    let binaries = executable.parent().unwrap().parent().unwrap();
    let root = fixture
        .directory
        .path()
        .join(format!("managed-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (package, trust) = fixture_package(&root, &binaries.join("managed-tree-fixture")).unwrap();
    let runtime = fixture_runtime(&root, &binaries.join("tilo-launcher")).unwrap();
    let authority = Arc::new(OwnerAuthority {
        owner: fixture.owner.clone(),
        attestor: fixture.certificate.clone(),
        session: OnceLock::new(),
        now: fixture.now,
    });
    let launcher = ManagedLauncher::new(package, trust, runtime, authority.clone()).unwrap();
    let runtime = launcher.runtime_lease().revalidate().unwrap();
    let session = fixture
        .owner
        .reserve_agent_session(
            AgentSessionReservation {
                room: RoomId::parse(crate::testing::ROOM).unwrap(),
                caller: fixture.certificate.clone(),
                attestor: fixture.certificate.clone(),
                agent: agent.into(),
                launch: uuid::Uuid::new_v4().to_string(),
                runtime_epoch: runtime.runtime_epoch.clone(),
                runtime_digest: runtime_digest(&runtime).unwrap(),
            },
            fixture.now,
        )
        .unwrap();
    authority.session.set(session.clone()).unwrap();
    connect(
        fixture,
        launcher,
        authority.as_ref(),
        session,
        runtime,
        root,
    )
}

fn connect(
    fixture: &Fixture,
    mut launcher: ManagedLauncher,
    authority: &OwnerAuthority,
    session: AgentSessionBinding,
    runtime: ManagedRuntimeFacts,
    root: std::path::PathBuf,
) -> Running {
    let binding = ManagedBinding {
        room: session.room.to_string(),
        app: launcher::managed::WORKSPACE_AGENT_PACKAGE_ID.into(),
        agent: session.agent.clone(),
        scope: ManagedLaunchScope::AgentSession,
        launch: session.launch.clone(),
        generation: session.generation,
    };
    let mut permit = launcher.authorize(binding.clone()).unwrap();
    let (pending, collab_child) = PendingManagedConnection::pair(&mut permit).unwrap();
    let (identity_child, identity) = UnixStream::pair().unwrap();
    let (model_child, mut model) = UnixStream::pair().unwrap();
    model
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let services = EnrolledServiceContext::new(
        binding,
        root,
        runtime.runtime_epoch,
        pending.channel_id().to_string(),
        ManagedProviderSelection {
            profile_id: "qualification-only".into(),
            model_id: "no-provider".into(),
            protocol: "anthropic".into(),
        },
        identity_child,
        collab_child,
        model_child,
    )
    .unwrap();
    let mut size = [0; 4];
    model.read_exact(&mut size).unwrap();
    let size = usize::try_from(u32::from_le_bytes(size)).unwrap();
    assert!(size <= 4096, "bounded actual service handoff required");
    let mut handoff = vec![0; size];
    model.read_exact(&mut handoff).unwrap();
    launcher.launch(permit, services).unwrap();
    let supervisor = started(&mut launcher);
    let (process, mut collab) = pending
        .admit_connection(&supervisor, authority)
        .unwrap()
        .into_parts();
    collab
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut ready = [0; 2];
    collab.read_exact(&mut ready).unwrap();
    ready.sort_unstable();
    assert_eq!(
        ready, *b"PR",
        "actual admitted fixture and orphan must be ready"
    );
    let service = AgentMessageHostService::from_admitted_process(
        fixture.owner.clone(),
        fixture.certificate.clone(),
        session,
        process.clone(),
        fixture.now,
    )
    .unwrap();
    Running {
        launcher,
        service,
        process,
        _model: model,
        _identity: identity,
        _collab: collab,
    }
}

fn admitted() -> (Fixture, uuid::Uuid) {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, attestation) = source(&fixture, 0);
    let invocation = attestation.data.invocation;
    fixture
        .owner
        .commit_agent_messages(request, &[attestation], &publisher(&fixture), fixture.now)
        .unwrap();
    (fixture, invocation)
}

fn phase(fixture: &Fixture, invocation: uuid::Uuid) -> MessagePhase {
    fixture
        .owner
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            Ok(view
                .record
                .agent_messages
                .get(&invocation)
                .unwrap()
                .phase
                .clone())
        })
        .unwrap()
}

fn change_chat(fixture: &Fixture, invocation: uuid::Uuid, change: usize) {
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc};
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let mutation = fixture
        .owner
        .with_current(room, |view| {
            let mut doc = AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                .map_err(|error| error.to_string())?;
            doc.set_actor(automerge::ActorId::from(b"ordinary-person-editor".to_vec()));
            if change == 2 {
                doc.put(ROOT, "ordinary-person-note", "Unrelated change")
                    .unwrap();
            } else {
                let channel = tilo_actions::agent_message::agent_conversation_channel_id(
                    crate::testing::ROOM,
                    "scribe",
                );
                let (_, messages) = doc.get(ROOT, "messages").unwrap().unwrap();
                let (_, list) = doc.get(&messages, channel).unwrap().unwrap();
                assert_eq!(doc.length(&list), 1);
                let (_, row) = doc.get(&list, 0).unwrap().unwrap();
                assert_eq!(
                    doc.get(&row, "id").unwrap().unwrap().0.as_str(),
                    Some(format!("m_{}", invocation.simple()).as_str())
                );
                if change == 0 {
                    doc.delete(&list, 0).unwrap();
                } else {
                    let (_, text) = doc.get(&row, "text").unwrap().unwrap();
                    let length = isize::try_from(doc.length(&text)).unwrap();
                    doc.splice_text(&text, 0, length, "Changed explicit input")
                        .unwrap();
                    doc.put(&row, "edited", true).unwrap();
                }
            }
            view.peer().clone().publish_document(
                "so.tilo.chat",
                "",
                doc.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_document(crate::publication::request(&mutation).unwrap(), fixture.now)
        .unwrap();
}

#[test]
fn current_request_edit_or_deletion_fences_each_actual_process_boundary() {
    for change in 0..2 {
        for boundary in 0..3 {
            let (fixture, invocation) = admitted();
            let mut running = launch(&fixture);
            if boundary > 0 {
                running.service.deliver(invocation, fixture.now).unwrap();
            }
            if boundary > 1 {
                running.service.accept(invocation, fixture.now).unwrap();
            }
            let room = RoomId::parse(crate::testing::ROOM).unwrap();
            let original = fixture
                .owner
                .with_current(room, |view| {
                    Ok(view.record.agent_messages[&invocation].clone())
                })
                .unwrap();
            change_chat(&fixture, invocation, change);
            assert!(
                running.process.facts().is_ok(),
                "actual process remains live"
            );
            let result = match boundary {
                0 => running.service.deliver(invocation, fixture.now).map(|_| ()),
                1 => running.service.accept(invocation, fixture.now),
                _ => running
                    .service
                    .prepare_reply(invocation, "Must not retain", fixture.now)
                    .map(|_| ()),
            };
            assert!(result.is_err());
            assert!(running.service.deliver(invocation, fixture.now).is_err());
            let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
            reopened
                .with_current(room, |view| {
                    let current = &view.record.agent_messages[&invocation];
                    assert!(matches!(
                        current.phase,
                        MessagePhase::NeedsAttention(MessageAttention::RequestChanged)
                    ));
                    // Preserve immutable already-delivered custody and the request;
                    // attention must not rewrite input, cancel the model or retarget.
                    assert!(current.execution == original.execution);
                    assert_eq!(
                        current.source.digest().unwrap(),
                        original.source.digest().unwrap()
                    );
                    assert_eq!(current.source.invocation, invocation);
                    Ok(())
                })
                .unwrap();
            running.launcher.shutdown().unwrap();
        }
    }
}

#[test]
fn unrelated_person_document_edit_does_not_invalidate_addressed_input() {
    let (fixture, invocation) = admitted();
    let mut running = launch(&fixture);
    // Closing/replacing Chat fences only new addressed publication. It must
    // neither cancel durable input nor borrow authority for ordinary edits.
    publisher(&fixture).retire();
    change_chat(&fixture, invocation, 2);
    let delivered = running.service.deliver(invocation, fixture.now).unwrap();
    assert_eq!(delivered.text, "Explicit input");
    running.service.accept(invocation, fixture.now).unwrap();
    assert!(matches!(
        phase(&fixture, invocation),
        MessagePhase::Accepted
    ));
    running.launcher.shutdown().unwrap();
}

#[test]
fn real_process_binds_accepts_and_prepares_only_explicit_input() {
    let (fixture, invocation) = admitted();
    let mut running = launch(&fixture);
    let mut wrong_session = running.service.session.clone();
    wrong_session.generation = wrong_session.generation.checked_add(1).unwrap();
    assert!(
        AgentMessageHostService::from_admitted_process(
            fixture.owner.clone(),
            fixture.certificate.clone(),
            wrong_session,
            running.process.clone(),
            fixture.now,
        )
        .is_err(),
        "live proof must match the exact durable session, not itself"
    );
    let delivered = running.service.deliver(invocation, fixture.now).unwrap();
    assert_eq!(delivered.text, "Explicit input");
    assert_eq!(delivered.request_row, format!("m_{}", invocation.simple()));
    assert_eq!(delivered.agent, "scribe");
    assert_eq!(delivered.launch, running.process.binding().launch);
    assert_eq!(
        running
            .service
            .deliver(invocation, fixture.now)
            .unwrap()
            .source_digest,
        delivered.source_digest
    );
    running.service.accept(invocation, fixture.now).unwrap();
    let reply = running
        .service
        .prepare_reply(invocation, "Fixture output", fixture.now)
        .unwrap();
    assert_eq!(
        running
            .service
            .prepare_reply(invocation, "Fixture output", fixture.now)
            .unwrap(),
        reply
    );
    assert!(
        running
            .service
            .prepare_reply(invocation, "Changed output", fixture.now)
            .is_err()
    );
    assert!(
        matches!(phase(&fixture, invocation), MessagePhase::ReplyPrepared { invocation: id, .. }
        if id == reply)
    );
    running.launcher.shutdown().unwrap();
    assert!(running.process.facts().is_err());
}

#[test]
fn actual_process_commits_one_attributed_same_parent_reply_and_exact_replay() {
    let (fixture, invocation) = admitted();
    let mut running = launch(&fixture);
    let delivery = running.service.deliver(invocation, fixture.now).unwrap();
    assert!(
        running
            .service
            .commit_reply(
                invocation,
                delivery.source_digest,
                &fixture.key,
                fixture.now
            )
            .is_err()
    );
    running.service.accept(invocation, fixture.now).unwrap();
    let reply = running
        .service
        .prepare_reply(
            invocation,
            "Fixture output; not model evidence",
            fixture.now,
        )
        .unwrap();
    let wrong_key = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
    assert!(
        running
            .service
            .commit_reply(invocation, delivery.source_digest, &wrong_key, fixture.now)
            .is_err()
    );
    assert!(
        running
            .service
            .commit_reply(invocation, [9; 32], &fixture.key, fixture.now)
            .is_err()
    );
    // An unrelated collaborator edit must survive the receipt-scoped append.
    change_chat(&fixture, invocation, 2);
    let receipt = running
        .service
        .commit_reply(
            invocation,
            delivery.source_digest,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
    assert_eq!(receipt.source, invocation);
    assert_eq!(receipt.reply, reply);
    assert_committed_reply(&fixture, invocation, &delivery, reply);
    assert_terminal_reply_isolation(&fixture, invocation, &delivery, &receipt, &mut running);
}

fn assert_committed_reply(
    fixture: &Fixture,
    invocation: uuid::Uuid,
    delivery: &AgentMessageDelivery,
    reply: uuid::Uuid,
) {
    use automerge::{AutoCommit, ROOT, ReadDoc};
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    fixture
        .owner
        .with_current(room, |view| {
            assert!(!view.record.agent_messages.contains_key(&invocation));
            let mut doc =
                AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap()).unwrap();
            assert_eq!(
                doc.get(ROOT, "ordinary-person-note")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("Unrelated change")
            );
            let (_, messages) = doc.get(ROOT, "messages").unwrap().unwrap();
            let (_, list) = doc.get(&messages, &delivery.channel).unwrap().unwrap();
            assert_eq!(doc.length(&list), 2);
            let (_, row) = doc.get(&list, 1).unwrap().unwrap();
            for (key, expected) in [
                ("id", format!("m_{}", reply.simple())),
                ("parent", delivery.request_row.clone()),
                ("author", "agent:scribe".into()),
            ] {
                assert_eq!(
                    doc.get(&row, key).unwrap().unwrap().0.as_str(),
                    Some(expected.as_str())
                );
            }
            let (_, text) = doc.get(&row, "text").unwrap().unwrap();
            assert_eq!(
                doc.text(&text).unwrap(),
                "Fixture output; not model evidence"
            );
            let changes = doc.get_changes(&[]);
            let reply_change = changes.last().unwrap();
            assert_eq!(reply_change.actor_id().to_bytes(), b"agent:scribe");
            let context =
                tilo_actions::ActionContext::from_change_message(reply_change.message().unwrap())
                    .unwrap();
            assert_eq!(context.invoker.unwrap().actor, "agent:scribe");
            assert_eq!(
                context.source_invocation,
                Some(tilo_actions::InvocationId(reply))
            );
            Ok(())
        })
        .unwrap();
}

fn assert_terminal_reply_isolation(
    fixture: &Fixture,
    invocation: uuid::Uuid,
    delivery: &AgentMessageDelivery,
    receipt: &AgentMessageReplyReceipt,
    running: &mut Running,
) {
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let before_replay = fixture
        .owner
        .with_current(room, |view| Ok((view.generation(), view.peer().durable()?)))
        .unwrap();
    let historical = running
        .service
        .commit_reply(
            invocation,
            delivery.source_digest,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
    assert_eq!(historical.source, invocation);
    assert_eq!(historical.generation, before_replay.0);
    assert!(receipt == &historical);
    fixture
        .owner
        .with_current(room, |view| {
            assert_eq!((view.generation(), view.peer().durable()?), before_replay);
            Ok(())
        })
        .unwrap();
    fixture
        .owner
        .commit_workspace_membership_command(
            OwnerCommitRequest {
                room: crate::testing::ROOM.into(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: fixture.certificate.clone(),
                    command: WorkspaceMembershipCommand::AddAgent {
                        agent: "another-agent".into(),
                        preset:
                            tilo_peer_state::workspace_membership::AgentMembershipPreset::Observer,
                    },
                },
            },
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
    let mut other = launch_for(fixture, "another-agent");
    assert!(
        other
            .service
            .commit_reply(
                invocation,
                delivery.source_digest,
                &fixture.key,
                fixture.now
            )
            .is_err(),
        "another genuinely admitted Agent cannot observe this terminal receipt"
    );
    other.launcher.shutdown().unwrap();
    fixture
        .owner
        .close_agent_session(&running.service.session, &fixture.certificate, fixture.now)
        .unwrap();
    running.launcher.shutdown().unwrap();
    let mut replacement = launch(fixture);
    assert!(
        replacement
            .service
            .commit_reply(
                invocation,
                delivery.source_digest,
                &fixture.key,
                fixture.now
            )
            .is_err(),
        "a replacement process for the same Agent cannot inherit this terminal receipt"
    );
    replacement.launcher.shutdown().unwrap();
}

#[test]
fn prepared_reply_cannot_publish_after_source_change_or_actual_process_exit() {
    use automerge::ReadDoc;
    for changed_source in [true, false] {
        let (fixture, invocation) = admitted();
        let mut running = launch(&fixture);
        let delivery = running.service.deliver(invocation, fixture.now).unwrap();
        running.service.accept(invocation, fixture.now).unwrap();
        running
            .service
            .prepare_reply(invocation, "Must not publish", fixture.now)
            .unwrap();
        if changed_source {
            change_chat(&fixture, invocation, 1);
        } else {
            running.launcher.shutdown().unwrap();
            assert!(running.process.facts().is_err());
        }
        assert!(
            running
                .service
                .commit_reply(
                    invocation,
                    delivery.source_digest,
                    &fixture.key,
                    fixture.now
                )
                .is_err()
        );
        fixture
            .owner
            .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
                assert!(view.record.agent_messages.contains_key(&invocation));
                let doc =
                    automerge::AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                        .unwrap();
                let (_, messages) = doc.get(automerge::ROOT, "messages").unwrap().unwrap();
                let (_, list) = doc.get(&messages, &delivery.channel).unwrap().unwrap();
                assert_eq!(
                    doc.length(&list),
                    1,
                    "refusal must not append an Agent reply"
                );
                Ok(())
            })
            .unwrap();
        if changed_source {
            running.launcher.shutdown().unwrap();
        }
    }
}

#[test]
fn actual_exit_before_each_owner_boundary_cannot_accept_or_prepare_reply() {
    for boundary in 0..3 {
        let (fixture, invocation) = admitted();
        let mut running = launch(&fixture);
        if boundary > 0 {
            running.service.deliver(invocation, fixture.now).unwrap();
        }
        if boundary > 1 {
            running.service.accept(invocation, fixture.now).unwrap();
        }
        // Retained proof and service existed while the real process was alive.
        // Actual shutdown cancels custody, joins workers and kills its namespace.
        running.launcher.shutdown().unwrap();
        assert!(running.process.facts().is_err());
        let result = match boundary {
            0 => running.service.deliver(invocation, fixture.now).map(|_| ()),
            1 => running.service.accept(invocation, fixture.now),
            _ => running
                .service
                .prepare_reply(invocation, "Must not retain", fixture.now)
                .map(|_| ()),
        };
        assert!(result.is_err());
        assert!(matches!(
            phase(&fixture, invocation),
            MessagePhase::NeedsAttention(MessageAttention::ExecutionRetired)
        ));
    }
}

#[test]
fn membership_revoked_after_actual_proof_capture_cannot_first_bind() {
    let (fixture, invocation) = admitted();
    let mut running = launch(&fixture);
    fixture
        .owner
        .commit_workspace_membership_command(
            OwnerCommitRequest {
                room: crate::testing::ROOM.into(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: fixture.certificate.clone(),
                    command: WorkspaceMembershipCommand::RemoveAgent {
                        agent: "scribe".into(),
                    },
                },
            },
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
    assert!(
        running.process.facts().is_ok(),
        "kernel proof remains genuinely live"
    );
    assert!(running.service.deliver(invocation, fixture.now).is_err());
    assert!(matches!(
        phase(&fixture, invocation),
        MessagePhase::NeedsAttention(MessageAttention::MembershipChanged)
    ));
    running.launcher.shutdown().unwrap();
}
