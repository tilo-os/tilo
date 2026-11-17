//! A turn a person asks an agent in a conversation: the broker invokes the agent's answer on its
//! connection, every request the agent makes while it answers is the turn's, in its session, and a
//! turn the person ends is cancelled.

use super::agent_connection::asking_about_notes;
use super::*;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};

/// The conversation's message the turn answers.
fn turn() -> tilo_action_policy::turn::Turn {
    tilo_action_policy::turn::Turn::new("chat.workspace/channels/c_tide", "m1")
}

/// The agent's published answer, as the agent app declares it.
fn answer_action() -> runtime_collab::ExportedAction {
    runtime_collab::ExportedAction {
        resource: runtime_collab::TURN_RESOURCE.into(),
        action: runtime_collab::ANSWER.into(),
        resource_kind: "agent.turn".into(),
        resource_label: "Turn".into(),
        action_label: "Answer".into(),
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Read,
            execution: tilo_actions::ExecutionMode::Streaming {
                cancellable: true,
                item_schema: serde_json::json!({"type": "object"}),
            },
            result: tilo_actions::ResultSpec::Job,
            ..Default::default()
        },
        ..notes_append()
    }
}

/// The agent's connection, answering turns: its channel and its invocations.
struct Answering {
    channel: runtime_collab::ChannelHandle,
    invocations: runtime_collab::InvocationStream,
}

/// The person asked the fixture's agent in a conversation: the workspace lists the agent, its
/// session goes, and the turn is one the sessions list.
fn ask(f: &Fixture) {
    f.list_scribe();
    f.native
        .sessions
        .lock()
        .unwrap()
        .push((SESSION.into(), crate::services::GoingSession::default()));
    f.native
        .turns
        .lock()
        .unwrap()
        .push(crate::services::AgentTurn {
            session: SESSION.into(),
            agent: "scribe".into(),
            author: hex::encode(f.native.certificate.user_public_key),
            turn: turn(),
            attempt: 0,
            request: runtime_collab::TurnRequest {
                message: "chat.workspace/channels/c_tide/messages/m1".into(),
                title: "Tide".into(),
                instruction: "Add the release date to the notes".into(),
                agent_instruction: None,
                fold: None,
                subject: None,
                spend: None,
            },
        });
}

/// The person ended the turn.
fn end(f: &Fixture) {
    f.native.turns.lock().unwrap().clear();
    f.native.sessions.lock().unwrap().clear();
}

/// The agent app connects as the workspace's kept agent and publishes how it answers.
async fn answering(f: &Fixture) -> Answering {
    let channel = f.app_channel(100, tilo_automation::AGENT_APP_ID, true);
    let invocations = channel.subscribe_stable_invocations().await.unwrap();
    channel.publish_actions(&[answer_action()]).await.unwrap();
    Answering {
        channel,
        invocations,
    }
}

/// The next invocation `agent` is handed.
async fn handed(agent: &Answering) -> runtime_collab::Invocation {
    tokio::time::timeout(Duration::from_secs(2), agent.invocations.read())
        .await
        .expect("the turn is handed to the agent")
        .unwrap()
}

/// A request continuing the turn `on_behalf_of`: append to the release notes.
fn append(invocation: u128, on_behalf_of: uuid::Uuid) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
        target: RemoteActionTarget {
            provider: "200".into(),
            address: tilo_actions::ActionAddress::new("notes:release", "append"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: Some(tilo_actions::InvocationId(on_behalf_of)),
        appearance: None,
        wait_ms: None,
    }
}

/// `message`, sent on the agent's own connection, and the broker's answer, taking the broker's
/// turn meanwhile, which is where an application's invocation is delivered.
async fn remote(
    f: &Fixture,
    agent: &Answering,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    let frame = RemoteActionFrame::new(message).encode().unwrap();
    let call = agent.channel.remote_action(&frame);
    tokio::pin!(call);
    let answer = tilo_test_env::within("the broker answers", async {
        loop {
            tokio::select! {
                answer = &mut call => break answer,
                () = tokio::task::yield_now() => f.state.take_turn(&f.directories),
            }
        }
    })
    .await
    .unwrap();
    RemoteActionFrame::decode(&answer).unwrap().message
}

/// The turn reaches the agent as its answer, invoked with what the person asked; the agent's step
/// on behalf of it is the turn's: taken as the agent for the person, in the turn's session.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_is_answered_by_its_agent_and_its_steps_are_the_turns() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            grant_notes(&f);
            let (_notes, stream) = notes_target(&f).await;
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            assert_eq!(
                (answer.resource.as_str(), answer.action.as_str()),
                (runtime_collab::TURN_RESOURCE, runtime_collab::ANSWER)
            );
            let request: runtime_collab::TurnRequest =
                serde_json::from_str(answer.argument.as_deref().unwrap()).unwrap();
            assert_eq!(request.instruction, "Add the release date to the notes");
            assert_eq!(
                answer.actor,
                tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe"))
            );

            let turn_id = uuid::Uuid::from_bytes(answer.invocation_uuid);
            let receipt =
                remote(&f, &agent, RemoteActionMessage::Invoke(append(41, turn_id))).await;
            assert!(
                matches!(
                    receipt,
                    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                        status: tilo_actions::remote::ReceiptStatus::Accepted
                            | tilo_actions::remote::ReceiptStatus::Completed { .. },
                        ..
                    })
                ),
                "{receipt:?}"
            );
            let wire = stream.next().await.unwrap();
            assert_eq!(
                wire.actor,
                tilo_actions::Actor::Member(tilo_actions::MemberId::agent("scribe"))
            );
            let group = wire.context().and_then(|context| context.group.clone());
            assert_eq!(
                group.map(|group| group.id).as_deref(),
                Some(SESSION),
                "the step is listed under the turn's session"
            );

            // Handed once: a second look hands nothing new.
            f.state.refresh_turns();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), agent.invocations.read())
                    .await
                    .is_err(),
                "a turn is handed once"
            );
            drop(f);
        })
        .await;
}

/// The person ending the turn cancels its answer, and the agent's requests are refused after it.
#[tokio::test(flavor = "current_thread")]
async fn an_ended_turn_is_cancelled_and_its_agent_does_nothing_more() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            grant_notes(&f);
            let (_notes, _stream) = notes_target(&f).await;
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            end(&f);
            f.state.refresh_turns();
            assert!(
                agent
                    .channel
                    .invocation_cancellation(answer.id)
                    .await
                    .unwrap(),
                "the agent is asked to stop"
            );
            let turn_id = uuid::Uuid::from_bytes(answer.invocation_uuid);
            let refused =
                remote(&f, &agent, RemoteActionMessage::Invoke(append(42, turn_id))).await;
            assert!(
                matches!(
                    refused,
                    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                        status: tilo_actions::remote::ReceiptStatus::Rejected { .. },
                        ..
                    })
                ),
                "{refused:?}"
            );
            drop(f);
        })
        .await;
}

/// What the turn reports it spent is charged to its session.
#[tokio::test(flavor = "current_thread")]
async fn what_a_turn_spent_is_charged_to_its_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            let invocation =
                tilo_actions::InvocationId(uuid::Uuid::from_bytes(answer.invocation_uuid));
            let spent = tilo_actions::InvocationUpdate::Item {
                invocation,
                sequence: 1,
                value: serde_json::json!({"spent": 1200}),
            };
            agent
                .channel
                .update_invocation(answer.id, &serde_json::to_string(&spent).unwrap())
                .await
                .unwrap();
            assert_eq!(
                f.native.spent.lock().unwrap().as_slice(),
                [(SESSION.to_owned(), 1200)]
            );
            drop(f);
        })
        .await;
}

/// A step the person asks first about is put to them on the shell's sheet, under the turn's
/// session, and goes on once they allow it.
#[tokio::test(flavor = "current_thread")]
async fn a_turns_question_is_asked_under_its_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let shell = asking_about_notes(&f, "scribe");
            let (_notes, _stream) = notes_target(&f).await;
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            let turn_id = uuid::Uuid::from_bytes(answer.invocation_uuid);
            let first = remote(&f, &agent, RemoteActionMessage::Invoke(append(43, turn_id))).await;
            assert!(
                matches!(
                    first,
                    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                        status: tilo_actions::remote::ReceiptStatus::Rejected {
                            error: tilo_actions::remote::ActionHostError::ConfirmationRequired { .. }
                        },
                        ..
                    })
                ),
                "{first:?}"
            );
            let asking = remote(&f, &agent,
                RemoteActionMessage::Confirm {
                    correlation: 1,
                    request: Box::new(append(43, turn_id)),
                },
            );
            let answered = async {
                let shown = shell.shown(1).await;
                assert_eq!(shown.agent_session.as_deref(), Some(SESSION), "{shown:?}");
                shell.answer(crate::app_actions::AppActionConsent::Once);
            };
            let (confirmed, ()) = tokio::join!(asking, answered);
            let RemoteActionMessage::ConfirmResponse {
                result: Ok(confirmed),
                ..
            } = confirmed
            else {
                panic!("the person's answer comes back: {confirmed:?}");
            };
            let mut again = append(43, turn_id);
            confirmed.apply(&mut again);
            let receipt = remote(&f, &agent, RemoteActionMessage::Invoke(again)).await;
            assert!(
                matches!(
                    receipt,
                    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                        status: tilo_actions::remote::ReceiptStatus::Accepted
                            | tilo_actions::remote::ReceiptStatus::Completed { .. },
                        ..
                    })
                ),
                "{receipt:?}"
            );
            drop(f);
        })
        .await;
}

/// Whether the turn's answer is still going.
fn still_going(f: &Fixture, invocation: tilo_actions::InvocationId) -> bool {
    f.state
        .pending_invocations
        .lock()
        .unwrap()
        .values()
        .any(|pending| pending.update.invocation() == invocation)
}

/// Make the turn's answer look as if it last moved `ago` before now.
fn last_moved(f: &Fixture, invocation: tilo_actions::InvocationId, ago: Duration) {
    let at = tilo_clock::now()
        .checked_sub(ago)
        .unwrap_or_else(tilo_clock::now);
    let moved = f
        .state
        .pending_invocations
        .lock()
        .unwrap()
        .iter_mut()
        .find(|(_, pending)| pending.update.invocation() == invocation)
        .map(|(_, pending)| pending.last_activity = at)
        .is_some();
    assert!(moved, "the turn waits in the broker");
}

/// Ask the turn's answer to stop, `ago` before now, as the broker does when the person ends it.
fn asked_to_stop(f: &Fixture, invocation: tilo_actions::InvocationId, ago: Duration) {
    let at = tilo_clock::now()
        .checked_sub(ago)
        .unwrap_or_else(tilo_clock::now);
    let stopped = f
        .state
        .pending_invocations
        .lock()
        .unwrap()
        .iter_mut()
        .find(|(_, pending)| pending.update.invocation() == invocation)
        .map(|(_, pending)| pending.cancellation_requested_at = Some(at))
        .is_some();
    assert!(stopped, "the turn waits in the broker");
}

/// A person who answers their question later than the idle deadline keeps their turn: the wait is
/// not the agent's silence, and the turn goes on from their answer.
#[tokio::test(flavor = "current_thread")]
async fn a_question_answered_after_the_deadline_still_continues_its_turn() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let shell = asking_about_notes(&f, "scribe");
            let (_notes, _stream) = notes_target(&f).await;
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            let turn_id = uuid::Uuid::from_bytes(answer.invocation_uuid);
            let invocation = tilo_actions::InvocationId(turn_id);
            let _ = remote(&f, &agent, RemoteActionMessage::Invoke(append(43, turn_id))).await;
            let asking = remote(
                &f,
                &agent,
                RemoteActionMessage::Confirm {
                    correlation: 1,
                    request: Box::new(append(43, turn_id)),
                },
            );
            let answered = async {
                let shown = shell.shown(1).await;
                assert_eq!(shown.agent_session.as_deref(), Some(SESSION), "{shown:?}");
                // The person is away longer than the deadline the broker ends silent work by.
                last_moved(&f, invocation, Duration::from_secs(60 * 60));
                let now = tilo_clock::now();
                f.state.expire_stale_invocations_at(
                    now,
                    Duration::from_secs(60),
                    Duration::from_secs(60),
                );
                assert!(
                    still_going(&f, invocation),
                    "a turn waiting on a person is not idle"
                );
                assert!(
                    f.native.failed.lock().unwrap().is_empty(),
                    "the turn did not fail while the person decided"
                );
                shell.answer(crate::app_actions::AppActionConsent::Once);
            };
            let (confirmed, ()) = tokio::join!(asking, answered);
            let RemoteActionMessage::ConfirmResponse {
                result: Ok(confirmed),
                ..
            } = confirmed
            else {
                panic!("the person's answer comes back: {confirmed:?}");
            };
            let mut again = append(43, turn_id);
            confirmed.apply(&mut again);
            let receipt = remote(&f, &agent, RemoteActionMessage::Invoke(again)).await;
            assert!(
                matches!(
                    receipt,
                    RemoteActionMessage::Receipt(tilo_actions::remote::InvocationReceipt {
                        status: tilo_actions::remote::ReceiptStatus::Accepted
                            | tilo_actions::remote::ReceiptStatus::Completed { .. },
                        ..
                    })
                ),
                "the step the person allowed goes on: {receipt:?}"
            );
            assert!(
                still_going(&f, invocation),
                "the turn continues after the person's answer"
            );
            drop(f);
        })
        .await;
}

/// The person's own time is not the answer's to ignore: a turn waiting on them still ends when the
/// broker asked it to stop and it did not acknowledge.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_waiting_on_a_person_still_ends_when_it_is_cancelled() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let shell = asking_about_notes(&f, "scribe");
            let (_notes, _stream) = notes_target(&f).await;
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            let turn_id = uuid::Uuid::from_bytes(answer.invocation_uuid);
            let invocation = tilo_actions::InvocationId(turn_id);
            let _ = remote(&f, &agent, RemoteActionMessage::Invoke(append(43, turn_id))).await;
            let asking = remote(
                &f,
                &agent,
                RemoteActionMessage::Confirm {
                    correlation: 1,
                    request: Box::new(append(43, turn_id)),
                },
            );
            let cancelled = async {
                let shown = shell.shown(1).await;
                assert_eq!(shown.agent_session.as_deref(), Some(SESSION), "{shown:?}");
                asked_to_stop(&f, invocation, Duration::from_secs(60));
                let now = tilo_clock::now();
                f.state.expire_stale_invocations_at(
                    now,
                    Duration::from_secs(60 * 60),
                    Duration::from_secs(30),
                );
                assert!(
                    !still_going(&f, invocation),
                    "a cancelled turn ends while the person decides"
                );
                shell.answer(crate::app_actions::AppActionConsent::Once);
            };
            let (confirmed, ()) = tokio::join!(asking, cancelled);
            assert!(
                matches!(confirmed, RemoteActionMessage::ConfirmResponse { .. }),
                "the person's own answer still comes back: {confirmed:?}"
            );
            drop(f);
        })
        .await;
}

/// A turn whose answer ended in silence is shown to the person as failed on its message, and is
/// not quietly handed to the agent again.
#[tokio::test(flavor = "current_thread")]
async fn a_turn_that_ends_in_silence_is_shown_as_failed() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = answering(&f).await;
            ask(&f);
            f.state.refresh_turns();
            let answer = handed(&agent).await;
            let invocation =
                tilo_actions::InvocationId(uuid::Uuid::from_bytes(answer.invocation_uuid));
            let now = tilo_clock::now();
            last_moved(&f, invocation, Duration::from_secs(16 * 60));
            f.state.expire_stale_invocations_at(
                now,
                Duration::from_secs(15 * 60),
                Duration::from_secs(30),
            );
            assert_eq!(
                f.native.failed.lock().unwrap().as_slice(),
                [(
                    SESSION.to_owned(),
                    turn(),
                    0,
                    "application stopped reporting action progress before the deadline".to_owned(),
                )],
                "the session is told the turn failed, so it shows on the message"
            );
            f.state.refresh_turns();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), agent.invocations.read())
                    .await
                    .is_err(),
                "a failed turn is not handed out again behind the person's back"
            );
            // The agent's connection goes and comes back: the failure stays what the person reads,
            // rather than the turn running a second time.
            f.state.turns.forget(100);
            f.state
                .turns
                .answer(100, &workspace_id().to_string(), "scribe");
            f.state.refresh_turns();
            assert!(
                tokio::time::timeout(Duration::from_millis(200), agent.invocations.read())
                    .await
                    .is_err(),
                "a turn shown as failed is not run again when its agent reconnects"
            );
            drop(f);
        })
        .await;
}

/// Nobody but the host starts an agent's answer: a person invoking it is refused, and its
/// catalogue does not list it.
#[tokio::test(flavor = "current_thread")]
async fn only_the_host_hands_an_agent_its_turn() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let agent = answering(&f).await;
            let person = Caller::local_person(hex::encode(f.native.certificate.user_public_key))
                .in_workspace(workspace_id().to_string());
            let actor = BrokerActor::for_caller(&person);
            let listed = f
                .state
                .action_catalogue_for(&workspace_id().to_string(), &actor)
                .unwrap();
            assert!(
                listed.iter().all(|surface| surface
                    .actions
                    .iter()
                    .all(|action| action.action != runtime_collab::ANSWER)),
                "no catalogue lists an agent's answer"
            );
            let refused = f.state.start_invocation_confirmed_for_origin(
                100,
                runtime_collab::TURN_RESOURCE,
                runtime_collab::ANSWER,
                InvocationInputs {
                    original_request: None,
                    requested: None,
                    caller: Some(&person),
                    execution: None,
                    argument: None,
                    actor: &actor,
                    confirmation_token: None,
                    context: None,
                },
                None,
            );
            assert!(refused.is_err(), "only the host hands an agent its turn");
            assert!(
                tokio::time::timeout(Duration::from_millis(100), agent.invocations.read())
                    .await
                    .is_err()
            );
            drop(f);
        })
        .await;
}
