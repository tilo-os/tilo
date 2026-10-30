//! A chat goes on: a follow-up run reads the conversation before it, so a
//! message that refers to an earlier one is understood.

use super::*;
use agent_provider_config::protocol::{
    Content, Message, ModelResponse, Role, StopReason, ToolCall, Usage,
};

/// A model that reads only what it is sent: the candidates the request names,
/// the ones the earlier conversation says it already reviewed, and the
/// person's newest message. It reviews the others, one a turn.
struct ReadsTheConversation {
    bodies: Mutex<Vec<Value>>,
}

const CANDIDATES: [&str; 3] = ["Jane Doe", "Ravi Patel", "Mei Lin"];

impl TurnTransport for ReadsTheConversation {
    fn start(&self, request: TurnRequest) -> Receiver<TurnEvent> {
        let turn = {
            let mut bodies = self.bodies.lock().unwrap();
            bodies.push(request.body.clone());
            bodies.len() - 1
        };
        let first = self.bodies.lock().unwrap()[0].to_string();
        let earlier = first
            .split("Earlier in this conversation")
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        let asked_for_the_others = first.contains("the same for the other two");
        let left: Vec<&str> = CANDIDATES
            .into_iter()
            .filter(|name| asked_for_the_others && !earlier.contains(&format!("reviewed {name}")))
            .collect();
        let call = match left.get(turn) {
            Some(name) => invoke(
                "append-paragraph",
                &serde_json::json!({"text": format!("Review: {name}")}),
            ),
            None => finish(&format!("Reviewed {}.", left.join(" and ")), true),
        };
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: format!("t{turn}"),
                    name: call.0.into(),
                    input: call.1,
                    extra_content: None,
                })],
            },
            stop_reason: StopReason::ToolCalls,
            usage: Usage::default(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(TurnEvent::Finished(Ok(
            serde_json::to_string(&response).unwrap()
        )))
        .unwrap();
        rx
    }
}

/// The second run of a chat is told the first one's conversation in its
/// instruction (as the tracker writes it) and the person's new message as
/// its direction: "now do the same for the other two" reviews the two the
/// first run did not.
#[test]
fn a_follow_up_that_refers_to_the_first_message_is_understood() {
    follows_up(World::default());
}

/// The host answers a poll the agent sent before it took the message in
/// only after it did: the late answer, from the frontier the poll was sent
/// at, is passed over rather than ending the run, and its message is not
/// taken in twice.
#[test]
fn a_poll_answered_after_the_message_was_taken_in_does_not_end_the_run() {
    let mut f = fixture("Review the applications", World::default(), Vec::new());
    f.session.native_task = Some(crate::session::native_task::NativeTaskState {
        run_id: f.run.run_id,
        task_id: "tasks.tracker/issues/loop".into(),
        base: "native:run".into(),
        revision: 1,
        ..Default::default()
    });
    let late = runtime_collab::TaskRunControl {
        version: 1,
        task_id: "tasks.tracker/issues/loop".into(),
        run_epoch: 2,
        from_revision: 0,
        applied_revision: 0,
        latest_revision: 1,
        directions: vec![runtime_collab::TaskDirection {
            revision: 1,
            author: "person:ada".into(),
            text: "Now do the same for the other two".into(),
            issued_at: 42,
        }],
        has_more: false,
    };
    f.session.observe_native_control(late.clone()).unwrap();
    assert!(f.session.native_task.as_ref().unwrap().pending.is_none());
    let ahead = runtime_collab::TaskRunControl {
        from_revision: 2,
        applied_revision: 2,
        latest_revision: 2,
        directions: Vec::new(),
        ..late
    };
    assert!(f.session.observe_native_control(ahead).is_err());
}

fn follows_up(world: World) {
    let control = runtime_collab::TaskRunControl {
        version: 1,
        task_id: "tasks.tracker/issues/loop".into(),
        run_epoch: 2,
        from_revision: 0,
        applied_revision: 0,
        latest_revision: 1,
        directions: vec![runtime_collab::TaskDirection {
            revision: 1,
            author: "person:ada".into(),
            text: "Now do the same for the other two".into(),
            issued_at: 42,
        }],
        has_more: false,
    };
    let mut f = fixture(
        "Review the applications",
        World {
            native_control: Some(control),
            ..world
        },
        Vec::new(),
    );
    f.run.instruction = Some(format!(
        "Review the applications\n\nJane Doe, Ravi Patel and Mei Lin applied today{}\
         \nPerson: Start with Jane Doe\nYou: 1 of 1: reviewed Jane Doe\
         \nYou: Reviewed Jane Doe and drafted a reply",
        runtime_collab::INSTRUCTION_CONVERSATION
    ));
    let model = Arc::new(ReadsTheConversation {
        bodies: Mutex::new(Vec::new()),
    });
    f.session.chooser = Chooser::scripted_model(
        Duration::from_secs(5),
        agent_provider_config::ModelDescriptor::new("reads"),
        Arc::clone(&model) as Arc<dyn TurnTransport>,
    );
    f.session.run_task(&f.run).unwrap();
    assert_eq!(
        terminal(&f.sent.borrow()),
        runtime_collab::TaskRunUpdate::Succeeded {
            message: "Reviewed Ravi Patel and Mei Lin.".into()
        }
    );
    let written: Vec<String> = f
        .world
        .borrow()
        .blocks
        .iter()
        .map(|(_, text)| text.clone())
        .collect();
    assert_eq!(written, ["Review: Ravi Patel", "Review: Mei Lin"]);
}
