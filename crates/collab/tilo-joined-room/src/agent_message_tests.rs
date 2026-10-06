//! Durable record/correlation tests. These do not claim broker admission,
//! managed process, gateway or native interaction proof; their production
//! adapter gates are separate and must use real signed invocation custody.
use super::*;
use crate::application_record::tests::{Fixture, fixture};
use tilo_peer_state::owner_commit::CommitTarget;

fn message(fixture: &Fixture) -> StoredAgentMessage {
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let invocation = uuid::Uuid::from_u128(300);
    StoredAgentMessage {
        source: MessageSource {
            room,
            invocation,
            request_row: format!("m_{}", invocation.simple()),
            channel: "c_agent_test".into(),
            caller: fixture.certificate.clone(),
            host: fixture.certificate.clone(),
            agent: "scribe".into(),
            payload: tilo_actions::agent_message::AgentMessagePayload {
                id: String::new(),
                ts: 7,
                text: "An explicitly selected private question".into(),
                intent: tilo_actions::agent_message::AgentIntent::Message,
                attachment: None,
                mentions: Vec::new(),
                reference: None,
                body: Vec::new(),
            },
            action: b"retained original typed bytes".to_vec(),
            action_digest: *blake3::hash(b"retained original typed bytes").as_bytes(),
            publisher_digest: [8; 32],
            operation: crate::AgentMessageOperationData {
                app: "so.tilo.chat".into(),
                resource: "opaque-agent-resource".into(),
                action: "message".into(),
                contract: tilo_actions::ActionContract::default(),
                foreign_restriction: false,
            },
            executed_action: tilo_actions::ActionId(uuid::Uuid::from_u128(500)),
            executed_actor: tilo_actions::ActionId(uuid::Uuid::from_u128(500))
                .actor_bytes()
                .to_vec(),
            changes: vec![[6; 32]],
            document: CommitBinding {
                room: room.to_string(),
                target: CommitTarget::Document {
                    app: "so.tilo.chat".into(),
                    record: String::new(),
                },
                request_digest: [3; 32],
                basis: [4; 32],
            },
        },
        execution: Some(MessageExecutionBinding {
            room,
            agent: "scribe".into(),
            launch: uuid::Uuid::from_u128(301),
            runtime_epoch: uuid::Uuid::from_u128(302),
            runtime_digest: [6; 32],
            session_generation: 1,
            process_digest: [7; 32],
        }),
        membership_incarnation: [10; 32],
        phase: MessagePhase::Bound,
        admitted_generation: 1,
    }
}

fn publish(fixture: &Fixture, message: StoredAgentMessage) {
    let _turn = fixture.owner.0.mutation.lock();
    let room = message.source.room;
    let mut candidate = fixture
        .owner
        .with_current(room, |view| Ok(view.clone()))
        .unwrap();
    candidate.record.generation = candidate.record.generation.checked_add(1).unwrap();
    candidate
        .record
        .agent_messages
        .insert(message.source.invocation, message);
    fixture.owner.publish_room(room, candidate, None).unwrap();
}

#[test]
fn pending_and_prepared_output_survive_actual_native_owner_store_reopen() {
    let fixture = fixture();
    let mut message = message(&fixture);
    let execution = message.execution.clone().unwrap();
    assert!(message.prepare_reply(&execution, "Not yet").is_err());
    assert!(message.accept(&execution).unwrap());
    assert!(!message.accept(&execution).unwrap());
    assert!(
        message
            .prepare_reply(&execution, "One genuine output")
            .unwrap()
    );
    assert!(
        !message
            .prepare_reply(&execution, "One genuine output")
            .unwrap()
    );
    assert!(
        message
            .prepare_reply(&execution, "Replacement output")
            .is_err()
    );
    let original = message.clone();
    publish(&fixture, message);
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .with_current(original.source.room, |view| {
            let stored = view
                .record
                .agent_messages
                .get(&original.source.invocation)
                .unwrap();
            assert!(stored.source == original.source);
            assert!(stored.execution == original.execution);
            assert!(stored.phase == original.phase);
            Ok(())
        })
        .unwrap();
}

#[test]
fn same_request_uuid_never_changes_typed_body_person_agent_or_launch() {
    let fixture = fixture();
    let mut message = message(&fixture);
    message.check_replay(&message.source).unwrap();
    for changed in 0..4 {
        let mut source = message.source.clone();
        match changed {
            0 => source.payload.text.push_str(" altered"),
            1 => source.agent = "other-agent".into(),
            2 => source.action_digest = [9; 32],
            _ => source.caller.device_label = "another-certificate".into(),
        }
        assert!(message.check_replay(&source).is_err());
    }
    let mut replaced = message.execution.clone().unwrap();
    replaced.launch = uuid::Uuid::from_u128(900);
    assert!(message.accept(&replaced).is_err());
    assert!(message.phase == MessagePhase::Bound);
    let original = message.execution.clone().unwrap();
    message.accept(&original).unwrap();
    assert!(message.prepare_reply(&replaced, "Wrong process").is_err());
    assert!(message.phase == MessagePhase::Accepted);
}

#[test]
fn offline_input_waits_without_process_custody_and_first_binding_is_idempotent() {
    let fixture = fixture();
    let mut message = message(&fixture);
    let execution = message.execution.take().unwrap();
    message.phase = MessagePhase::AwaitingAgent;
    message.validate().unwrap();
    assert!(message.accept(&execution).is_err());
    let original = message.clone();
    publish(&fixture, message.clone());
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .with_current(original.source.room, |view| {
            let stored = &view.record.agent_messages[&original.source.invocation];
            assert!(stored.execution.is_none());
            assert!(stored.phase == MessagePhase::AwaitingAgent);
            Ok(())
        })
        .unwrap();
    assert!(message.bind_first(&execution, [10; 32]).unwrap());
    assert!(!message.bind_first(&execution, [10; 32]).unwrap());
    let mut replacement = execution.clone();
    replacement.launch = uuid::Uuid::from_u128(901);
    assert!(message.bind_first(&replacement, [10; 32]).is_err());
}

#[test]
fn changed_membership_or_revocation_never_resurrects_awaiting_or_bound_input() {
    let fixture = fixture();
    for reason in [
        MessageAttention::MembershipChanged,
        MessageAttention::AuthorRevoked,
        MessageAttention::HostChanged,
        MessageAttention::ExecutionRetired,
    ] {
        let mut message = message(&fixture);
        let execution = message.execution.take().unwrap();
        message.phase = MessagePhase::AwaitingAgent;
        // Same durable Agent and identical admission value still cannot bind
        // if its authenticated winning CRDT operation has changed.
        assert!(message.bind_first(&execution, [11; 32]).is_err());
        assert!(message.execution.is_none());
        assert!(message.needs_attention(reason.clone()));
        assert!(!message.needs_attention(reason));
        assert!(message.bind_first(&execution, [10; 32]).is_err());
        assert!(message.accept(&execution).is_err());
        message.validate().unwrap();
    }
    let mut bound = message(&fixture);
    let execution = bound.execution.clone().unwrap();
    bound.accept(&execution).unwrap();
    bound.needs_attention(MessageAttention::ExecutionRetired);
    assert!(bound.bind_first(&execution, [10; 32]).is_err());
    assert!(bound.accept(&execution).is_err());
    assert!(
        bound
            .prepare_reply(&execution, "Cannot bypass retirement")
            .is_err()
    );
}

#[test]
fn authentic_remove_and_identical_readd_change_membership_incarnation_only() {
    use tilo_peer_state::workspace_membership::{
        AgentMembershipPreset, WorkspaceMembershipCommand,
    };
    let fixture = fixture();
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let mut peer = fixture
        .owner
        .with_current(room, |view| Ok(view.peer().clone()))
        .unwrap();
    let add = WorkspaceMembershipCommand::AddAgent {
        agent: "scribe".into(),
        preset: AgentMembershipPreset::Observer,
    };
    peer.apply_workspace_membership_command(&fixture.certificate, &add, &fixture.key, fixture.now)
        .unwrap();
    let original = membership_incarnation(&peer, "scribe").unwrap();
    let mut awaiting = message(&fixture);
    let execution = awaiting.execution.take().unwrap();
    awaiting.phase = MessagePhase::AwaitingAgent;
    awaiting.membership_incarnation = original;
    let unrelated = WorkspaceMembershipCommand::AddAgent {
        agent: "other".into(),
        preset: AgentMembershipPreset::Observer,
    };
    peer.apply_workspace_membership_command(
        &fixture.certificate,
        &unrelated,
        &fixture.key,
        fixture.now,
    )
    .unwrap();
    assert_eq!(membership_incarnation(&peer, "scribe").unwrap(), original);
    let remove = WorkspaceMembershipCommand::RemoveAgent {
        agent: "scribe".into(),
    };
    peer.apply_workspace_membership_command(
        &fixture.certificate,
        &remove,
        &fixture.key,
        fixture.now,
    )
    .unwrap();
    assert!(membership_incarnation(&peer, "scribe").is_err());
    peer.apply_workspace_membership_command(&fixture.certificate, &add, &fixture.key, fixture.now)
        .unwrap();
    let readded = membership_incarnation(&peer, "scribe").unwrap();
    assert_ne!(readded, original);
    assert!(awaiting.bind_first(&execution, readded).is_err());
    assert!(awaiting.execution.is_none());
}

#[test]
fn input_validation_preserves_chat_bounds_and_exact_canonical_record() {
    let fixture = fixture();
    let mut source = message(&fixture).source;
    source.payload.text = "\u{1f600}".repeat(MAX_TEXT_CHARS);
    source.validate().unwrap();
    assert_eq!(source.payload.text.len(), MAX_TEXT_BYTES);
    source.payload.text.push('x');
    assert!(source.validate().is_err());
    source.payload.text = "hello".into();
    source.document.target = CommitTarget::Document {
        app: "so.tilo.chat".into(),
        record: "foreign".into(),
    };
    assert!(source.validate().is_err());
    source.document.target = CommitTarget::Document {
        app: "so.tilo.notes".into(),
        record: String::new(),
    };
    assert!(source.validate().is_err());
}

#[test]
fn append_only_terminal_index_retains_old_uuid_beyond_hot_capacity_without_content() {
    let fixture = fixture();
    let message = message(&fixture);
    let first = TerminalMessage {
        invocation: message.source.invocation,
        source_digest: message.source.digest().unwrap(),
        outcome: MessageOutcome::Closed,
        generation: 1,
    };
    let store = fixture.owner.0.store.as_ref();
    let mut root = Some(crate::agent_message_history::insert(store, None, first.clone()).unwrap());
    for n in 0..(MAX_PENDING * 2) {
        let fact = TerminalMessage {
            invocation: uuid::Uuid::from_u128(10_000 + n as u128),
            ..first.clone()
        };
        root = Some(crate::agent_message_history::insert(store, root, fact).unwrap());
    }
    let recovered = crate::agent_message_history::lookup(store, root, first.invocation)
        .unwrap()
        .unwrap();
    recovered.check_replay(&message.source).unwrap();
    let mut altered = message.source.clone();
    altered.payload.text.push_str(" altered");
    assert!(recovered.check_replay(&altered).is_err());
    let mut conflicting = first.clone();
    conflicting.source_digest = [7; 32];
    assert!(crate::agent_message_history::insert(store, root, conflicting).is_err());
    assert!(crate::agent_message_history::lookup(store, Some([99; 32]), first.invocation).is_err());
    let encoded = serde_json::to_string(&recovered).unwrap();
    assert!(!encoded.contains(&message.source.payload.text));
    assert!(!encoded.contains(&message.source.caller.device_label));
}

#[test]
fn explicit_delivery_unavailable_receipt_retains_accepted_request_replay_identity() {
    let fixture = fixture();
    let message = message(&fixture);
    let fact = TerminalMessage {
        invocation: message.source.invocation,
        source_digest: message.source.digest().unwrap(),
        outcome: MessageOutcome::DeliveryUnavailable,
        generation: 1,
    };
    let store = fixture.owner.0.store.as_ref();
    let root = crate::agent_message_history::insert(store, None, fact).unwrap();
    let retained =
        crate::agent_message_history::lookup(store, Some(root), message.source.invocation)
            .unwrap()
            .unwrap();
    assert!(retained.outcome == MessageOutcome::DeliveryUnavailable);
    retained.check_replay(&message.source).unwrap();
    let mut altered = message.source;
    altered.agent = "replacement".into();
    assert!(retained.check_replay(&altered).is_err());
}
