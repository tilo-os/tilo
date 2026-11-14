//! What the tests of a turn's steps that ask first share.
#![expect(
    clippy::future_not_send,
    reason = "the broker's Rc-backed channel handles stay on the test LocalSet"
)]
use super::*;

/// Notes' `append`, taking a `text`, published on peer 200.
pub(super) async fn append_target(
    f: &Fixture,
) -> (runtime_collab::ChannelHandle, InvocationStreamServer) {
    let handle = Rc::new(f.app_handle(200, "so.tilo.notes", false));
    let target = wire_channel_rc(Rc::clone(&handle));
    let mut append = notes_append();
    append.params = vec![tilo_actions::ActionParam::required(
        "text",
        tilo_actions::ParamType::Text,
        "What to append",
    )];
    target.publish_actions(&[append]).await.unwrap();
    let stream = handle.invocation_feed(true).unwrap();
    (target, stream)
}

/// The person tells the agent Notes asks first.
pub(super) fn ask_first_for_notes(f: &Fixture) {
    put_grant(f, asks_first_for_notes(f));
}

/// The grant [`ask_first_for_notes`] puts.
fn asks_first_for_notes(f: &Fixture) -> tilo_action_policy::Grant {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    agent_answers::grant(
        &workspace_id().to_string(),
        "scribe",
        (
            &AnswerScope::app("so.tilo.notes"),
            tilo_action_policy::ActionKind::Undoable,
        ),
        tilo_action_policy::Answer::Ask,
        tilo_action_policy::Grantee::person(hex::encode(f.native.certificate.user_public_key)),
    )
}

pub(super) async fn append(
    channel: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
    invocation: u128,
    text: &str,
) -> Result<(u64, tilo_actions::InvocationUpdate), runtime_portal::NotAvailable> {
    let argument = serde_json::json!({ "text": text }).to_string();
    let target = runtime_collab::TaskAction {
        argument: Some(&argument),
        ..notes_action()
    };
    let (id, update) = channel
        .invoke_task_action(
            run.run_id,
            &run.lease,
            target,
            &uuid::Uuid::from_u128(invocation).to_string(),
        )
        .await?;
    Ok((id, serde_json::from_str(&update).unwrap()))
}

/// Nothing was dispatched to the target app (peer 200) that it has not read.
pub(super) fn nothing_delivered() -> bool {
    crate::collab_app_broker::unread_invocations(200) == 0
}
