//! Actual signed native-owner transaction tests, not broker/Agent/model proof.
use super::*;
use crate::application_record::tests::{Fixture, fixture};
use crate::{AgentMessageAdmissionData, AgentMessageOperationData};
use automerge::transaction::Transactable;
use ed25519_dalek::Signer as _;
use tilo_peer_state::workspace_membership::{AgentMembershipPreset, WorkspaceMembershipCommand};

pub(crate) fn publisher(fixture: &Fixture) -> crate::AgentMessagePublisherLease {
    publisher_on(fixture, &fixture.owner)
}

fn publisher_on(
    fixture: &Fixture,
    owner: &NativeWorkspaceOwner,
) -> crate::AgentMessagePublisherLease {
    owner
        .register_agent_message_publisher(&publisher_registration(fixture), fixture.now)
        .unwrap()
}

pub(crate) fn publisher_registration(
    fixture: &Fixture,
) -> crate::AgentMessagePublisherRegistration {
    let mut registration = crate::AgentMessagePublisherRegistration {
        data: crate::AgentMessagePublisherData {
            room: RoomId::parse(crate::testing::ROOM).unwrap(),
            worker: uuid::Uuid::from_u128(901),
            directory: uuid::Uuid::from_u128(902),
            stream: uuid::Uuid::from_u128(903),
            installation: uuid::Uuid::from_u128(904),
        },
        attestor: fixture.certificate.clone(),
        signature: Vec::new(),
    };
    registration.signature = fixture
        .key
        .sign(&registration.signing_bytes().unwrap())
        .to_bytes()
        .to_vec();
    registration
}

pub(crate) fn add_agent(fixture: &Fixture) {
    let request = OwnerCommitRequest {
        room: crate::testing::ROOM.into(),
        operation: CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::from_u128(800),
            caller: fixture.certificate.clone(),
            command: WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentMembershipPreset::Observer,
            },
        },
    };
    fixture
        .owner
        .commit_workspace_membership_command(
            request,
            &fixture.certificate,
            &fixture.key,
            fixture.now,
        )
        .unwrap();
}

pub(crate) fn source(
    fixture: &Fixture,
    variant: usize,
) -> (OwnerCommitRequest, AgentMessageAdmissionAttestation) {
    prepare_chat(fixture);
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let invocation = uuid::Uuid::from_u128(801);
    let action = tilo_actions::ActionId(uuid::Uuid::from_u128(802));
    let channel =
        tilo_actions::agent_message::agent_conversation_channel_id(crate::testing::ROOM, "scribe");
    let person = fixture
        .owner
        .with_current(room, |view| {
            Ok(view
                .peer()
                .permissions(fixture.certificate.device_public_key, fixture.now)?
                .2)
        })
        .unwrap();
    let mut doc = fixture
        .owner
        .with_current(room, |view| {
            AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                .map_err(|error| error.to_string())
        })
        .unwrap();
    doc.set_actor(automerge::ActorId::from(action.actor_bytes().to_vec()));
    let channels = doc.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let conversation = doc.put_object(&channels, &channel, ObjType::Map).unwrap();
    doc.put(&conversation, "workspace", crate::testing::ROOM)
        .unwrap();
    doc.put(
        &conversation,
        "conversation_with",
        if variant == 3 { "other" } else { "scribe" },
    )
    .unwrap();
    doc.put(&conversation, "kind", "text").unwrap();
    let messages = doc.put_object(ROOT, "messages", ObjType::Map).unwrap();
    let list = doc.put_object(&messages, &channel, ObjType::List).unwrap();
    for index in 0..if variant == 4 { 2 } else { 1 } {
        assert_eq!(doc.length(&list), index);
        write_row(
            &mut doc,
            &list,
            &format!("m_{}", invocation.simple()),
            if variant == 2 {
                "another-person"
            } else {
                &person
            },
            if variant == 1 {
                "Wrong committed text"
            } else {
                "Explicit input"
            },
        );
    }
    let mut executed = action;
    if variant == 8 {
        // A genuinely separate committed contributor in the same resulting
        // document is not allowed to claim the first contributor's row.
        commit_correlated(&mut doc, action, invocation);
        executed = tilo_actions::ActionId(uuid::Uuid::from_u128(803));
        doc.set_actor(automerge::ActorId::from(executed.actor_bytes().to_vec()));
        write_row(
            &mut doc,
            &list,
            "m_other-contributor",
            &person,
            "Explicit input",
        );
    }
    commit_correlated(&mut doc, executed, invocation);
    let changes = doc
        .get_changes(&[])
        .iter()
        .filter(|change| change.actor_id().to_bytes() == executed.actor_bytes())
        .map(|change| change.hash().0)
        .collect();
    let message = fixture
        .owner
        .with_current(room, |view| {
            view.peer().clone().publish_document(
                "so.tilo.chat",
                "",
                doc.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    let request = publication::request(&message).unwrap();
    let mut proof = AgentMessageAdmissionAttestation {
        data: AgentMessageAdmissionData {
            invocation, caller: fixture.certificate.clone(),
            action: serde_json::to_vec(&serde_json::json!({"PostAgentMessage": { "workspace": crate::testing::ROOM, "agent": "scribe", "message": { "ts": 7, "text": "Explicit input", "intent": "message" } }})).unwrap(),
            document: request.binding().unwrap(), publisher_digest: publisher(fixture).digest(),
            operation: AgentMessageOperationData { app: "so.tilo.chat".into(), resource: "opaque-resource".into(), action: "message".into(), contract: tilo_actions::ActionContract::default(), foreign_restriction: false },
            executed_action: executed, executed_actor: executed.actor_bytes().to_vec(), changes,
        },
        attestor: fixture.certificate.clone(), signature: Vec::new(),
    };
    resign(fixture, &mut proof);
    (request, proof)
}

fn prepare_chat(fixture: &Fixture) {
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let route = crate::ApplicationRecord::declared(
        "so.tilo.chat",
        "",
        "chat.workspace",
        &["chat.workspace".into()],
    )
    .unwrap();
    fixture
        .owner
        .prepare_application_record(
            room,
            &fixture.certificate,
            &fixture.key,
            &route,
            fixture.now,
        )
        .unwrap();
}

fn write_row(doc: &mut AutoCommit, list: &ObjId, id: &str, author: &str, text: &str) {
    let row = doc
        .insert_object(list, doc.length(list), ObjType::Map)
        .unwrap();
    doc.put(&row, "id", id).unwrap();
    doc.put(&row, "author", author).unwrap();
    doc.put(&row, "ts", 7i64).unwrap();
    doc.put(&row, "parent", "").unwrap();
    doc.put(&row, "agent_intent", "message").unwrap();
    doc.put(&row, "edited", false).unwrap();
    let content = doc.put_object(&row, "text", ObjType::Text).unwrap();
    doc.splice_text(&content, 0, 0, text).unwrap();
}

fn resign(fixture: &Fixture, proof: &mut AgentMessageAdmissionAttestation) {
    proof.signature = fixture
        .key
        .sign(&proof.signing_bytes().unwrap())
        .to_bytes()
        .to_vec();
}

fn commit_correlated(doc: &mut AutoCommit, action: tilo_actions::ActionId, invocation: uuid::Uuid) {
    let context = tilo_actions::ActionContext {
        executed_action: Some(action),
        source_invocation: Some(tilo_actions::InvocationId(invocation)),
        ..Default::default()
    };
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(context.change_message().unwrap()),
    );
}

// Exercise the production executor's attributed writer, independently of its
// newly allocated ActionId. This fixture action is not a Chat/model proof.
#[derive(Debug)]
struct AttributedRequest {
    author: String,
    row: String,
}

impl tilo_actions::Action for AttributedRequest {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), tilo_actions::ActionError> {
        let channel = tilo_actions::agent_message::agent_conversation_channel_id(
            crate::testing::ROOM,
            "scribe",
        );
        let channels = match doc.get(ROOT, "channels")? {
            Some((_, id)) => id,
            None => doc.put_object(ROOT, "channels", ObjType::Map)?,
        };
        let conversation = match doc.get(&channels, &channel)? {
            Some((_, id)) => id,
            None => doc.put_object(&channels, &channel, ObjType::Map)?,
        };
        doc.put(&conversation, "workspace", crate::testing::ROOM)?;
        doc.put(&conversation, "conversation_with", "scribe")?;
        doc.put(&conversation, "kind", "text")?;
        let messages = match doc.get(ROOT, "messages")? {
            Some((_, id)) => id,
            None => doc.put_object(ROOT, "messages", ObjType::Map)?,
        };
        let list = match doc.get(&messages, &channel)? {
            Some((_, id)) => id,
            None => doc.put_object(&messages, &channel, ObjType::List)?,
        };
        write_row(doc, &list, &self.row, &self.author, "Explicit input");
        Ok(())
    }
}

fn attributed_source(
    fixture: &Fixture,
    claim_second_action: bool,
) -> (OwnerCommitRequest, AgentMessageAdmissionAttestation) {
    attributed_source_with_context(
        fixture,
        claim_second_action,
        Some(tilo_actions::ActionContext {
            executed_action: Some(tilo_actions::ActionId(uuid::Uuid::from_u128(999))),
            source_invocation: Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(801))),
            ..Default::default()
        }),
    )
}

fn apply_attributed(
    doc: &mut AutoCommit,
    action: &AttributedRequest,
    encoded: tilo_actions::EncodedAction,
    writer: &[u8],
    context: Option<tilo_actions::ActionContext>,
) -> tilo_actions::ActionEnvelope {
    match context {
        Some(context) => {
            tilo_actions::apply_local_with_context(doc, action, encoded, Some(writer), context)
        }
        None => tilo_actions::apply_local_by(doc, action, encoded, Some(writer)),
    }
    .unwrap()
}

fn attributed_source_with_context(
    fixture: &Fixture,
    claim_second_action: bool,
    context: Option<tilo_actions::ActionContext>,
) -> (OwnerCommitRequest, AgentMessageAdmissionAttestation) {
    let (_, mut proof) = source(fixture, 0);
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let (mut doc, person) = fixture
        .owner
        .with_current(room, |view| {
            Ok((
                AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                    .map_err(|error| error.to_string())?,
                view.peer()
                    .permissions(fixture.certificate.device_public_key, fixture.now)?
                    .2,
            ))
        })
        .unwrap();
    let writer = format!("{person}\0actual-device-writer").into_bytes();
    let mut actual = apply_attributed(
        &mut doc,
        &AttributedRequest {
            author: person.clone(),
            row: format!("m_{}", proof.data.invocation.simple()),
        },
        tilo_actions::EncodedAction {
            type_id: "fixture.request".into(),
            payload: proof.data.action.clone(),
        },
        &writer,
        context.clone(),
    );
    assert_ne!(actual.actor().to_bytes(), actual.id.actor_bytes());
    if claim_second_action {
        actual = apply_attributed(
            &mut doc,
            &AttributedRequest {
                author: person,
                row: "m_other-contributor".into(),
            },
            actual.action.clone(),
            &writer,
            context,
        );
    }
    proof.data.executed_action = actual.id;
    proof.data.executed_actor = actual.actor().to_bytes().to_vec();
    proof.data.changes = doc
        .get_changes(&actual.base_change_hashes())
        .iter()
        .map(|change| change.hash().0)
        .collect();
    proof.data.changes.sort_unstable();
    let message = fixture
        .owner
        .with_current(room, |view| {
            view.peer().clone().publish_document(
                "so.tilo.chat",
                "",
                doc.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    let request = publication::request(&message).unwrap();
    proof.data.document = request.binding().unwrap();
    resign(fixture, &mut proof);
    (request, proof)
}

#[test]
fn real_executor_attributed_writer_is_bound_separately_from_action_id() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, mut proof) = attributed_source(&fixture, false);
    let mut wrong = proof.clone();
    wrong.data.executed_actor = b"another-writer".to_vec();
    resign(&fixture, &mut wrong);
    assert!(
        fixture
            .owner
            .commit_agent_messages(request.clone(), &[wrong], &publisher(&fixture), fixture.now)
            .unwrap_err()
            .contains("another action actor")
    );
    fixture
        .owner
        .commit_agent_messages(
            request.clone(),
            &[proof.clone()],
            &publisher(&fixture),
            fixture.now,
        )
        .unwrap();
    proof.data.executed_actor = b"another-writer".to_vec();
    resign(&fixture, &mut proof);
    assert!(
        fixture
            .owner
            .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
            .is_err(),
        "same UUID and range with different writer must not replay"
    );
}

#[test]
fn same_attributed_writer_does_not_merge_separate_executed_action_ranges() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = attributed_source(&fixture, true);
    assert!(
        fixture
            .owner
            .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
            .unwrap_err()
            .contains("outside its typed contribution range")
    );
}

#[test]
fn missing_executor_or_missing_or_wrong_original_invocation_cannot_mint_input() {
    for context in [
        None,
        Some(tilo_actions::ActionContext::default()),
        Some(tilo_actions::ActionContext {
            source_invocation: Some(tilo_actions::InvocationId(uuid::Uuid::from_u128(777))),
            ..Default::default()
        }),
    ] {
        let fixture = fixture();
        add_agent(&fixture);
        let (request, proof) = attributed_source_with_context(&fixture, false, context.clone());
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let generation = fixture
            .owner
            .with_current(room, |view| Ok(view.generation()))
            .unwrap();
        let error = fixture
            .owner
            .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
            .unwrap_err();
        assert!(
            error.contains(if context.is_none() {
                "lacks executor correlation"
            } else {
                "original invocation"
            }),
            "{error}"
        );
        fixture
            .owner
            .with_current(room, |view| {
                assert_eq!(view.generation(), generation);
                assert!(view.record.agent_messages.is_empty());
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn canonical_request_and_offline_input_publish_atomically_and_replay_after_reopen() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    let receipt = fixture
        .owner
        .commit_agent_messages(
            request.clone(),
            std::slice::from_ref(&proof),
            &publisher(&fixture),
            fixture.now,
        )
        .unwrap();
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert!(
        reopened
            .commit_agent_messages(
                request.clone(),
                std::slice::from_ref(&proof),
                &publisher(&fixture),
                fixture.now,
            )
            .unwrap_err()
            .contains("another owner")
    );
    let again = reopened
        .commit_agent_messages(
            request,
            std::slice::from_ref(&proof),
            &publisher_on(&fixture, &reopened),
            fixture.now,
        )
        .unwrap();
    assert_eq!(receipt.binding, again.binding);
    assert_eq!(receipt.generation, again.generation);
    reopened
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            assert_eq!(view.record.agent_messages.len(), 1);
            let input = &view.record.agent_messages[&proof.data.invocation];
            assert!(input.execution.is_none());
            assert!(input.phase == MessagePhase::AwaitingAgent);
            assert_eq!(input.source.action, proof.data.action);
            verify_row(view, &input.source, fixture.now)
        })
        .unwrap();
}

#[test]
fn mismatched_committed_row_or_swapped_typed_contributor_never_partially_publishes() {
    for variant in [1, 2, 3, 4, 5, 6, 8] {
        let fixture = fixture();
        add_agent(&fixture);
        let (request, mut proof) = source(&fixture, variant);
        if variant == 5 {
            proof.data.executed_action = tilo_actions::ActionId(uuid::Uuid::from_u128(900));
        } else if variant == 6 {
            proof.data.changes = vec![[9; 32]];
        }
        resign(&fixture, &mut proof);
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
        let generation = fixture
            .owner
            .with_current(room, |view| Ok(view.generation()))
            .unwrap();
        let error = fixture
            .owner
            .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
            .unwrap_err();
        let expected = match variant {
            1 => "committed text differs",
            2 | 3 => "canonical row differs",
            4 => "absent or duplicated",
            5 => "another action",
            6 => "contribution is absent",
            _ => "another action contributor",
        };
        assert!(error.contains(expected), "variant {variant}: {error}");
        fixture
            .owner
            .with_current(room, |view| {
                assert_eq!(view.generation(), generation);
                assert!(view.record.agent_messages.is_empty());
                let doc = document(view).unwrap();
                assert!(doc.get(ROOT, "messages").unwrap().is_none());
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn signed_typed_source_without_current_agent_membership_cannot_mint_input() {
    let fixture = fixture();
    let (request, proof) = source(&fixture, 0);
    assert!(
        fixture
            .owner
            .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
            .is_err()
    );
}

#[test]
fn fixed_uuid_replay_cannot_replace_exact_original_typed_bytes() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    fixture
        .owner
        .commit_agent_messages(
            request.clone(),
            std::slice::from_ref(&proof),
            &publisher(&fixture),
            fixture.now,
        )
        .unwrap();
    let mut changed = proof;
    changed.data.action.push(b' ');
    resign(&fixture, &mut changed);
    assert!(
        fixture
            .owner
            .commit_agent_messages(request, &[changed], &publisher(&fixture), fixture.now)
            .is_err()
    );
}

#[test]
fn operation_restricted_after_source_mint_is_refused_in_actual_owner_turn() {
    use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    {
        let _turn = fixture.owner.0.mutation.lock();
        let mut candidate = fixture
            .owner
            .with_current(room, |view| Ok(view.clone()))
            .unwrap();
        let person = candidate
            .peer()
            .permissions(fixture.certificate.device_public_key, fixture.now)
            .unwrap()
            .2;
        let mut restriction = CapOverride::new(
            Subject::Principal(Principal::Person(person)),
            Scope::Action {
                app: proof.data.operation.app.clone(),
                action: proof.data.operation.action.clone(),
            },
            Effect::Restrict,
            Verb::Read,
        );
        restriction.id = "message-operation-restriction".into();
        candidate
            .peer
            .put_governed_capabilities(&[restriction], &fixture.key, fixture.now)
            .unwrap();
        candidate.record.peer = candidate.peer.durable().unwrap();
        candidate.record.generation += 1;
        publication::replace_governance_checkpoint(
            &mut candidate,
            request.binding().unwrap(),
            fixture.now,
        )
        .unwrap();
        fixture.owner.publish_room(room, candidate, None).unwrap();
    }
    let generation = fixture
        .owner
        .with_current(room, |view| Ok(view.generation()))
        .unwrap();
    let error = fixture
        .owner
        .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
        .unwrap_err();
    assert!(
        error.contains("operation is no longer authorized"),
        "{error}"
    );
    fixture
        .owner
        .with_current(room, |view| {
            assert_eq!(view.generation(), generation);
            assert!(view.record.agent_messages.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn pending_original_bytes_over_owner_budget_keep_chat_and_terminal_replay_without_text() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, mut proof) = source(&fixture, 0);
    // A submitted payload id is ignored by broker-invoked Chat in favor of
    // its canonical invocation UUID. Still retain/digest the full original
    // typed bytes: never truncate inert extras to fit the private hot queue.
    let mut action: serde_json::Value = serde_json::from_slice(&proof.data.action).unwrap();
    action["PostAgentMessage"]["message"]["id"] = "inert-original-id".repeat(200_000).into();
    proof.data.action = serde_json::to_vec(&action).unwrap();
    resign(&fixture, &mut proof);
    let receipt = fixture
        .owner
        .commit_agent_messages(
            request.clone(),
            std::slice::from_ref(&proof),
            &publisher(&fixture),
            fixture.now,
        )
        .unwrap();
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    fixture
        .owner
        .with_current(room, |view| {
            assert!(view.record.agent_messages.is_empty());
            let terminal = agent_message_history::lookup(
                fixture.owner.0.store.as_ref(),
                view.record.agent_message_history_root,
                proof.data.invocation,
            )?
            .unwrap();
            assert!(terminal.outcome == MessageOutcome::DeliveryUnavailable);
            let original = proof.verify(view, &request, fixture.now)?;
            terminal.check_replay(&original)?;
            verify_row(view, &original, fixture.now)?;
            assert!(
                !serde_json::to_string(&terminal)
                    .unwrap()
                    .contains("Explicit input")
            );
            Ok(())
        })
        .unwrap();
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    let replay = reopened
        .commit_agent_messages(
            request,
            &[proof],
            &publisher_on(&fixture, &reopened),
            fixture.now,
        )
        .unwrap();
    assert_eq!(receipt.binding, replay.binding);
    assert_eq!(receipt.generation, replay.generation);
}
