//! The note of a step holds no tool back. What a turn's reply says the turn is doing is a write
//! into Chat, and no tool waits until Chat has taken it: a tool that only reads is noted when it
//! has answered, any other before it begins, and calls of one tool that follow each other in an
//! answer are noted once. The reply still comes to say each step, in their order, and a reply
//! that ended stays ended.
use super::*;
use crate::workspace_actions::hosted_secret_use::logged;
use std::collections::VecDeque;

/// What the field `name` of a timing line holds.
fn field<'a>(line: &'a str, name: &str) -> &'a str {
    let (_, rest) = line
        .split_once(&format!(" {name}="))
        .unwrap_or_else(|| panic!("{name} stands in {line}"));
    rest.split(' ').next().unwrap_or_default().trim()
}

/// What the agent's reply to `message` says the turn is doing now, where it says so.
fn doing(part: &chat::parts::ChatPart, message: &Id) -> Option<String> {
    let reply = part
        .messages
        .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)))?;
    match &reply.work.as_ref()?.state.as_ref()?.state {
        chat::model::AgentWorkState::Doing { did, .. } => Some(did.clone()),
        _ => None,
    }
}

/// A model whose answers are written out. Each time it is asked after a step, it first looks
/// at what its reply says the turn is doing, as its person would while it thinks, until the
/// reply names the tool it awaits there: what it then read is kept.
struct Watches {
    answers: VecDeque<ModelResponse>,
    awaited: VecDeque<Option<&'static str>>,
    read: Arc<Mutex<Vec<Option<String>>>>,
    workspaces: Workspaces,
    workspace: String,
    message: Id,
}

impl tilo_agent_session::model::PlanTurns for Watches {
    fn turn(
        &mut self,
        _: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        if let Some(Some(awaited)) = self.awaited.pop_front() {
            let here = tokio::runtime::Handle::current();
            let deadline = tilo_test_env::hang_deadline();
            let said = loop {
                let part = here.block_on(conversation_part(&self.workspaces, &self.workspace));
                let said = doing(&part, &self.message);
                if said.as_deref() == Some(awaited) || Instant::now() > deadline {
                    break said;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            self.read.lock().unwrap().push(said);
        }
        self.answers
            .pop_front()
            .ok_or_else(|| ModelError::invalid("nothing further is written"))
    }

    fn model(&self) -> &'static str {
        "watches"
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_holds_no_tool_back_and_the_reply_still_says_each_step_in_order() {
    let lines = logged();
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let call = |id: &str, name: &str, input: serde_json::Value| {
        Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        })
    };
    // One answer with two listings and a search, three tools that only read; then one with a
    // write; then words.
    let answers = vec![
        answer(
            vec![
                call("apps", "tilo_ls", serde_json::json!({})),
                call(
                    "app",
                    "tilo_ls",
                    serde_json::json!({"path": "word-processor"}),
                ),
                call(
                    "find",
                    "tilo_find",
                    serde_json::json!({"query": "Gooseberry"}),
                ),
            ],
            StopReason::ToolCalls,
            1,
        ),
        answer(
            vec![call(
                "write",
                "tilo_do",
                serde_json::json!({"path": "word-processor#append-paragraph",
                    "args": {"text": "The marmalade is ordered."}}),
            )],
            StopReason::ToolCalls,
            1,
        ),
        answer(
            vec![Content::Text {
                text: "I looked, and wrote it down.".into(),
            }],
            StopReason::EndTurn,
            1,
        ),
    ];
    let read = Arc::new(Mutex::new(Vec::new()));
    let model: Model = {
        let (read, workspaces) = (Arc::clone(&read), served.workspaces.clone());
        let (workspace, message) = (workspace.clone(), said.clone());
        Arc::new(move || {
            Some(Box::new(Watches {
                answers: answers.clone().into(),
                awaited: VecDeque::from([None, Some("tilo_find"), Some("tilo_do")]),
                read: Arc::clone(&read),
                workspaces: workspaces.clone(),
                workspace: workspace.clone(),
                message: message.clone(),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })
    };
    answered(&served, &workspace, &said, model).await;

    // When each note went, as the server's log says of each step.
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    let timing: Vec<&str> = logged
        .lines()
        .filter(|line| line.contains("timing tool"))
        .collect();
    assert_eq!(timing.len(), 4, "{timing:#?}");
    let noted: Vec<(&str, &str)> = timing
        .iter()
        .map(|line| (field(line, "tool"), field(line, "note")))
        .collect();
    assert_eq!(
        noted,
        [
            ("tilo_ls", "after"),
            ("tilo_ls", "same"),
            ("tilo_find", "after"),
            ("tilo_do", "before"),
        ],
        "a tool that only reads is noted when it has answered, the same tool once, a tool \
         that may change something before it begins: {timing:#?}"
    );
    for line in &timing {
        assert_eq!(field(line, "outcome"), "ok", "{line}");
    }

    // What its person read on the reply while the model thought: the newest step each time.
    assert_eq!(
        *read.lock().unwrap(),
        [Some("tilo_find".to_owned()), Some("tilo_do".to_owned())],
        "the reply says each step once its tool is at work or has answered"
    );

    // Every note went to the service before the reply's end, and the service takes a
    // workspace's requests in the order they came: once a request made now is answered,
    // nothing of the turn is still on its way, and the reply stands ended.
    let person = Served::person(&served.ada, &served.devices.1, &workspace);
    let _ = served.finds(&workspace, &person, "Gooseberry").await;
    let part = conversation_part(&served.workspaces, &workspace).await;
    let reply = part
        .messages
        .get(&chat::chat::reply_id(&said, &MemberId::agent(SCOUT)))
        .expect("the reply");
    assert!(
        !chat::summons::unanswered(Some(reply)) && doing(&part, &said).is_none(),
        "no late note stands on a reply that ended: {:?}",
        reply.work
    );
    page.peer.close();
}
