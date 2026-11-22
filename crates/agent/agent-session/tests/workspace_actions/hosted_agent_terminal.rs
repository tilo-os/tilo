//! An agent a workspace's server works executes commands in the workspace's Terminal for its
//! person, on a server somebody else operates, as tilo.sh is: the person is one of the people it
//! keeps workspaces for, and their agent is held to what they allow.
//!
//! A command asks the person first, in the conversation the turn is in, and the person's own
//! answer carries it out, once, with the agent told what it printed. A second command asks again. Once
//! the person allowed the agent that one action for good, from its Agents settings, it is
//! carried out unasked, and taken back it asks again.
use super::api_unattended::{Person, Sheet, beside};
use super::hosted_agent_turns::{SCOUT, conversation_part, make_agent, write_to_agent};
use super::resource_support::{admit_person_at, open_admitted_at};
use super::*;
use std::sync::Arc;
use tilo_peer_state::join::Role;
use tilo_server::workspace_agents::ServerAgents;
use tilo_server::workspaces::{
    Workspaces,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_server_client::MemberApprover;
use tilo_server_client::questions::{MemberQuestions, answer_questions};
use tokio::sync::mpsc;
use ui::tilo_resource::{Id, Resource as _};

const CHAT: &str = "so.tilo.chat";
/// Where the workspace keeps its agents.
const AGENTS: &str = ui::tilo_workspace_agent::OWNER;

/// A server its operator keeps up, keeping Ada's workspace, with her assistant on it, her own
/// model, and a shell for the workspace's commands.
struct OnTheirServer {
    _server: tilo_server::daemon::Server,
    actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: Workspaces,
    workspace: String,
    page: Browser,
    ada: SigningKey,
    browser: crate::support::Device,
    agents: ServerAgents,
    model: Arc<ScriptedProvider>,
    address: tilo_server_client::ServerAddress,
    _state: tempfile::TempDir,
    _shells: tempfile::TempDir,
}

impl OnTheirServer {
    async fn start() -> Self {
        Self::start_with(None).await
    }

    /// [`Self::start`], with the server's own model answering her assistant where one is
    /// given: she then links no model of her own.
    async fn start_with(servers_model: Option<tilo_server::workspace_agents::Model>) -> Self {
        let (operator, _) = tilo_user_identity::UserIdentity::generate("Server operator", 1);
        let holder = enrol(&operator, "tilo-sh", DeviceCapabilities::member());
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let browser = enrol(&ada, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let shells = tempfile::tempdir().unwrap();
        let (server, actions) = tilo_server::agents::start_serving_documents(
            crate::harness::hosted::on_this_machine(state.path()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: operator.verifying_key().to_bytes(),
                device_signing_key: holder.key.clone(),
                device_cert: holder.cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            Some(Arc::new(crate::common::catalogue)),
            Some(tilo_server::agents::host_shells(shells.path().join("shells")).unwrap()),
            None,
        )
        .await
        .unwrap();
        let address = tilo_server_client::ServerAddress::at(server.node_addr());
        let workspaces = server.workspaces().unwrap().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let keeper = ada.verifying_key().to_bytes();
        // As a server keeping people's own workspaces makes her the owner of hers: a member
        // the workspace is kept for, its administrator.
        admit_person_at(&server, &holder, &workspace, &browser, Role::Editor).await;
        workspaces
            .declare_owner(&workspace, keeper, Some("Ada"))
            .await
            .unwrap();
        let page = open_admitted_at(&server, &browser, &holder, &workspace).await;
        let person = hex::encode(keeper);
        make_agent(&page, &workspaces, &workspace, &person, None).await;
        let catalogue = crate::common::catalogue();
        let governor = workspaces.governor().clone();
        workspaces
            .prepare_application(&workspace, &governor, &catalogue, CHAT, "")
            .await
            .unwrap();
        let model = ScriptedProvider::answering(Vec::new());
        workspaces.answer_models_with(model.clone());
        if servers_model.is_none() {
            workspaces
                .link_model_for(
                    &workspace,
                    &Linking {
                        name: "Google",
                        protocol: ProviderProtocol::OpenAiCompatible,
                        endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                        key: Some("the-keepers-own-model-key"),
                        models: &["gemini-main"],
                    },
                    true,
                )
                .await
                .unwrap();
        }
        let agents = ServerAgents::for_test(workspaces.clone(), actions.clone(), servers_model);
        Self {
            _server: server,
            actions,
            workspaces,
            workspace,
            page,
            ada,
            browser,
            agents,
            model,
            address,
            _state: state,
            _shells: shells,
        }
    }

    /// Ada writes `words` to her assistant, and the one turn that makes takes `steps`. A turn of
    /// her assistant's still going (the one her answer to a question woke) takes her words in
    /// once it is free: the turn they make is waited for until her assistant was shown them.
    async fn turn(&self, words: &str, steps: Vec<Reply>) -> Id {
        for step in steps {
            self.model.then(step);
        }
        let message = write_to_agent(
            &self.actions,
            &self.workspaces,
            &self.workspace,
            (&self.ada, &self.browser),
            words,
        )
        .await;
        let deadline = tilo_test_env::hang_deadline();
        while !self.was_shown(words) {
            assert!(
                Instant::now() < deadline,
                "her assistant is never shown {words:?}; it was shown:\n{}",
                self.shown()
            );
            for turn in self.agents.turns(&self.workspace).await.unwrap() {
                tokio::time::timeout(
                    Duration::from_secs(60),
                    Box::pin(self.agents.answer(&self.workspace, &turn)),
                )
                .await
                .expect("the turn ends")
                .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        message
    }

    /// The questions about an action the conversation holds, oldest first.
    async fn questions(&self) -> Vec<(Id, chat::chat::Message)> {
        let part = conversation_part(&self.workspaces, &self.workspace).await;
        let mut asked: Vec<_> = part
            .messages
            .iter()
            .filter(|(_, held)| {
                held.question
                    .as_ref()
                    .is_some_and(|question| question.kind.0 == chat::question::ACTION)
            })
            .map(|(id, held)| (id.clone(), held.clone()))
            .collect();
        asked.sort_by_key(|(id, held)| (held.ts, id.to_string()));
        asked
    }

    /// Ada answers the question `message` asks with `answer`, at a device of hers.
    async fn answers(&self, message: &Id, answer: &str) {
        let saved = self
            .workspaces
            .resource_document(&self.workspace, CHAT, "")
            .await
            .unwrap()
            .expect("the workspace's chat");
        let held = chat::chat::Chat::read(&automerge::AutoCommit::load(&saved).unwrap()).unwrap();
        let person = hex::encode(self.ada.verifying_key().to_bytes());
        let answering = chat::messages::chat_messages::AnswerQuestion {
            message: message.clone(),
            answer: answer.to_owned(),
            device: Some("browser".to_owned()),
        };
        let catalogue = crate::common::catalogue();
        let governor = self.workspaces.governor().clone();
        let part = chat::parts::part(0);
        self.workspaces
            .author_application(
                &self.workspace,
                &governor,
                &catalogue,
                CHAT,
                part.as_str(),
                |doc| {
                    chat::demo::apply_part_doc_as(
                        doc,
                        &held,
                        |replica| replica.as_invoker(&person),
                        None,
                        chat::parts::ChatMessages::ANSWER_QUESTION.with(answering),
                    )
                    .map_err(|error| format!("{error:?}"))
                },
            )
            .await
            .unwrap();
    }

    /// The server looks until the question `message` asks says what came of its answer.
    async fn settled(&self, message: &Id) -> chat::question::Question {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            Box::pin(self.agents.look(&self.workspace)).await.unwrap();
            let part = conversation_part(&self.workspaces, &self.workspace).await;
            let question = part
                .messages
                .get(message)
                .and_then(|held| held.question.clone())
                .expect("the question is in the conversation");
            let ended = question.outcome.as_ref().is_some_and(|outcome| {
                [
                    chat::question::DONE,
                    chat::question::DROPPED,
                    chat::question::FAILED,
                ]
                .contains(&outcome.outcome.0.as_str())
            });
            if ended {
                return question;
            }
            assert!(
                Instant::now() < deadline,
                "the server never said what came of the answer: {:?}",
                question.outcome
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// What the model was shown, every request's last message, for a failed assertion.
    fn shown(&self) -> String {
        self.model
            .asked()
            .iter()
            .filter_map(|asked| asked.request.messages.last())
            .map(|message| format!("{message:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether the model was shown `text` in any request.
    fn was_shown(&self, text: &str) -> bool {
        self.model
            .asked()
            .iter()
            .any(|asked| format!("{:?}", asked.request.messages).contains(text))
    }
}

/// The model's step that executes `command` in the workspace's Terminal.
fn executes(call: &str, command: &str) -> Reply {
    let mut input = serde_json::Map::new();
    input.insert("path".to_owned(), "terminal#run".into());
    input.insert("args".to_owned(), serde_json::json!({ "command": command }));
    Reply::tool_call(call, "tilo_do", serde_json::Value::Object(input))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_command_asks_in_the_conversation_and_is_done_once_on_its_persons_answer() {
    Box::pin(command_asks_then_is_done()).await;
}

async fn command_asks_then_is_done() {
    let hosted = OnTheirServer::start().await;
    // Ada's tab holds her root: whatever the server puts to her device shows there.
    let approver = MemberApprover::new(hosted.address.clone());
    let (answers, mut answered) = mpsc::unbounded_channel();
    let person = Person {
        asked: Arc::default(),
        answers,
    };
    let mut sheet = Sheet {
        asked: Arc::clone(&person.asked),
    };
    let mut questions = MemberQuestions {
        server: &approver,
        root: &hosted.ada,
        device: None,
    };
    let page = answer_questions(&mut questions, &mut sheet, &mut answered, || {
        tokio::time::sleep(Duration::from_millis(50))
    });
    tokio::pin!(page);

    beside(
        &mut page,
        Box::pin(hosted.turn(
            "Say hello in the shell",
            vec![
                executes("first", "echo first-echo-$((40+2))"),
                Reply::text("I asked you first."),
            ],
        )),
    )
    .await;
    let asked_in_conversation = beside(&mut page, hosted.questions()).await;
    assert_eq!(
        (asked_in_conversation.len(), person.questions()),
        (1, 0),
        "the command is asked in the conversation, not on a device; the model was shown:\n{}",
        hosted.shown()
    );
    assert!(!hosted.was_shown("first-echo-42"), "nothing was done yet");
    let (question, _) = asked_in_conversation.into_iter().next().unwrap();

    // Her answer carries it out, once, and her assistant is told what it printed.
    hosted.model.then(Reply::text("It printed first-echo-42."));
    beside(&mut page, hosted.answers(&question, "allow")).await;
    let settled = beside(&mut page, hosted.settled(&question)).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DONE),
        "{settled:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    while !hosted.was_shown("first-echo-42") {
        assert!(
            Instant::now() < deadline,
            "the assistant is told what the command printed; it was shown:\n{}",
            hosted.shown()
        );
        beside(&mut page, hosted.agents.look(&hosted.workspace))
            .await
            .unwrap();
        beside(&mut page, tokio::time::sleep(Duration::from_millis(50))).await;
    }

    // A second command in the same conversation asks again, the same way.
    beside(
        &mut page,
        Box::pin(hosted.turn(
            "And once more",
            vec![
                executes("second", "echo second-echo"),
                Reply::text("I asked you again."),
            ],
        )),
    )
    .await;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let again = beside(&mut page, hosted.questions()).await;
        if again.len() == 2 || Instant::now() >= deadline {
            assert_eq!(
                (again.len(), person.questions()),
                (2, 0),
                "the second command is asked too; the model was shown:\n{}",
                hosted.shown()
            );
            break;
        }
        beside(&mut page, tokio::time::sleep(Duration::from_millis(50))).await;
    }
}

impl OnTheirServer {
    /// Ada sets her assistant's bound to `rules` (each a node and the answer for outside effects
    /// there) at a device of hers, as its Agents settings do: Chat's `set-agent-bound` on the
    /// workspace's agents. Why the list of agents refused it, where it did.
    async fn bound(&self, rules: &[(&tilo_action_policy::TreePath, &str)]) -> Result<(), String> {
        use ui::tilo_workspace_agent::{Bound, WorkspaceAgents, workspace_agents::SetAgentBound};
        let agents_document = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
        let held = self
            .workspaces
            .resource_document(&self.workspace, AGENTS, &agents_document)
            .await
            .unwrap()
            .expect("the workspace's agents");
        let person = hex::encode(self.ada.verifying_key().to_bytes());
        let mut agents = ui::tilo_resource::Replica::<WorkspaceAgents>::from_doc(
            automerge::AutoCommit::load(&held).unwrap(),
        )
        .as_author(&person);
        let bound = rules
            .iter()
            .map(|(node, answer)| Bound {
                node: node.to_string(),
                kinds: vec!["outside".to_owned()],
                answer: (*answer).to_owned(),
            })
            .collect();
        agents
            .apply(WorkspaceAgents::SET_AGENT_BOUND.with(SetAgentBound {
                agent: Id::from_raw(SCOUT.to_owned()),
                bound,
            }))
            .map_err(|refused| refused.to_string())?;
        let saved = std::mem::take(agents.doc_mut()).save();
        let route = DocumentRoute {
            app_id: AGENTS.into(),
            cluster_id: self.workspace.clone(),
            document: agents_document.clone(),
        };
        self.page
            .peer
            .write(
                route,
                &collab_types::DocumentMutation::Snapshot(saved.clone()),
            )
            .unwrap();
        // The server decides with what it holds: wait until it holds this bound.
        let expected = automerge::AutoCommit::load(&saved).unwrap().get_heads();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let now = self
                .workspaces
                .resource_document(&self.workspace, AGENTS, &agents_document)
                .await
                .unwrap()
                .expect("the workspace's agents");
            let mut now = automerge::AutoCommit::load(&now).unwrap();
            if expected
                .iter()
                .all(|head| now.get_change_by_hash(head).is_some())
            {
                return Ok(());
            }
            assert!(
                Instant::now() < deadline,
                "the server never holds the bound"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_allowed_to_execute_commands_does_unasked_until_its_keeper_takes_it_back() {
    Box::pin(allowed_commands_go_unasked()).await;
}

async fn allowed_commands_go_unasked() {
    use tilo_action_policy::agent_answers::AnswerScope;
    let hosted = OnTheirServer::start().await;
    let terminal = ui::apps::TERMINAL;
    // Terminal as a whole is not hers to allow: an outside effect goes unasked only for the one
    // action she names.
    let everything = AnswerScope::app(terminal).node(&hosted.workspace);
    let refused = hosted.bound(&[(&everything, "allow")]).await;
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains("one action they name")),
        "{refused:?}"
    );
    let command = AnswerScope::action(terminal, "terminal.session", "run").node(&hosted.workspace);
    hosted.bound(&[(&command, "allow")]).await.unwrap();

    hosted
        .turn(
            "Say hello in the shell",
            vec![
                executes("first", "echo unasked-$((40+2))"),
                Reply::text("Done."),
            ],
        )
        .await;
    assert!(
        hosted.was_shown("unasked-42"),
        "the command ran unasked; the model was shown:\n{}",
        hosted.shown()
    );
    assert!(hosted.questions().await.is_empty(), "nobody was asked");

    // Taken back, the next command asks again.
    hosted.bound(&[]).await.unwrap();
    hosted
        .turn(
            "And again",
            vec![
                executes("second", "echo again-$((6*7))"),
                Reply::text("I asked you."),
            ],
        )
        .await;
    assert_eq!(hosted.questions().await.len(), 1, "{}", hosted.shown());
    assert!(!hosted.was_shown("again-42"), "nothing ran");
}

/// The agent's reply to `message`, once its work ended.
async fn reply_ended(hosted: &OnTheirServer, message: &Id) -> chat::chat::Message {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
        let reply = part
            .messages
            .get(&chat::chat::reply_id(message, &MemberId::agent(SCOUT)));
        if let Some(reply) = reply
            && !chat::summons::unanswered(Some(reply))
        {
            return reply.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the agent's reply to {message} never ended: {:?}; the model was shown:\n{}",
            reply.map(|reply| (reply.text.clone(), reply.work.clone())),
            hosted.shown()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One call of a model a test plays: its answer to what it was asked.
type Play = Box<
    dyn FnOnce(
            &tilo_model_gateway::host_backend::protocol::ModelRequest,
        ) -> Result<
            tilo_model_gateway::host_backend::protocol::ModelResponse,
            tilo_model_gateway::host_backend::protocol::ModelError,
        > + Send,
>;

/// The server's own model, each call of which is the next of its plays, whichever turn asks.
struct Played(Arc<std::sync::Mutex<std::collections::VecDeque<Play>>>);

impl tilo_agent_session::model::PlanTurns for Played {
    fn turn(
        &mut self,
        request: &tilo_model_gateway::host_backend::protocol::ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<
        tilo_model_gateway::host_backend::protocol::ModelResponse,
        tilo_model_gateway::host_backend::protocol::ModelError,
    > {
        use tilo_model_gateway::host_backend::protocol::{ErrorKind, ModelError};
        let play = self.0.lock().unwrap().pop_front();
        play.map_or_else(
            || {
                Err(ModelError {
                    kind: ErrorKind::InvalidRequest,
                    message: "nothing further is played".into(),
                })
            },
            |play| play(request),
        )
    }

    fn model(&self) -> &'static str {
        "played"
    }
}

/// The model's answer: `content`, ending its turn when it calls no tool.
fn played(
    content: Vec<tilo_model_gateway::host_backend::protocol::Content>,
) -> tilo_model_gateway::host_backend::protocol::ModelResponse {
    use tilo_model_gateway::host_backend::protocol::{
        Content, Message, ModelResponse, Role, StopReason, Usage,
    };
    let calls = content
        .iter()
        .any(|content| matches!(content, Content::ToolCall(_)));
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason: if calls {
            StopReason::ToolCalls
        } else {
            StopReason::EndTurn
        },
        usage: Usage::default(),
        cost_micros: 0,
    }
}

/// The model calls `tool` with `input`, as its call `id`.
fn calls(
    id: &str,
    tool: &str,
    input: serde_json::Value,
) -> tilo_model_gateway::host_backend::protocol::ModelResponse {
    played(vec![
        tilo_model_gateway::host_backend::protocol::Content::ToolCall(
            tilo_model_gateway::host_backend::protocol::ToolCall {
                id: id.into(),
                name: tool.into(),
                input,
                extra_content: None,
            },
        ),
    ])
}

/// The invocation a tool answered `pending` with, in the last message of `request`.
fn pending_invocation(
    request: &tilo_model_gateway::host_backend::protocol::ModelRequest,
) -> String {
    let said = format!("{:?}", request.messages.last());
    said.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .find(|piece| piece.len() == 36 && piece.matches('-').count() == 4)
        .unwrap_or_else(|| panic!("no invocation is pending: {said}"))
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_the_servers_model_a_turn_that_failed_with_a_command_pending_lets_the_next_message_in() {
    Box::pin(servers_model_failed_turn_then_next_message()).await;
}

/// The server's own model, as a server whose operator keeps one for people who linked none: a
/// turn starts a command it does not wait for and asks how it goes, then the model call breaks
/// off and stays down. Her next message in the conversation is a turn of its own, answered.
async fn servers_model_failed_turn_then_next_message() {
    use tilo_action_policy::agent_answers::AnswerScope;
    use tilo_model_gateway::host_backend::protocol::{Content, ErrorKind, ModelError};
    let plays: Arc<std::sync::Mutex<std::collections::VecDeque<Play>>> = Arc::default();
    let model: tilo_server::workspace_agents::Model = {
        let plays = Arc::clone(&plays);
        Arc::new(move || {
            Some(Box::new(Played(Arc::clone(&plays)))
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })
    };
    let hosted = OnTheirServer::start_with(Some(model)).await;
    let command =
        AnswerScope::action(ui::apps::TERMINAL, "terminal.session", "run").node(&hosted.workspace);
    hosted.bound(&[(&command, "allow")]).await.unwrap();
    let looking = tokio::spawn(hosted.agents.clone().run());

    let unknown = || ModelError {
        kind: ErrorKind::Provider,
        message: "Provider outcome is unknown; request was not replayed automatically".into(),
    };
    {
        let mut plays = plays.lock().unwrap();
        plays.push_back(Box::new(|_| {
            Ok(calls(
                "build",
                "tilo_do",
                serde_json::json!({
                    "path": "terminal#run",
                    "args": {"command": "sleep 20; echo built"},
                    "wait_ms": 200,
                }),
            ))
        }));
        plays.push_back(Box::new(|request| {
            let invocation = pending_invocation(request);
            Ok(calls(
                "how",
                "tilo_status",
                serde_json::json!({"invocation_id": invocation}),
            ))
        }));
        plays.push_back(Box::new(|_| Err(ModelError::new(ErrorKind::Transport))));
        for _ in 0..3 {
            plays.push_back(Box::new(move |_| Err(unknown())));
        }
    }
    let written = (&hosted.ada, &hosted.browser);
    let first = write_to_agent(
        &hosted.actions,
        &hosted.workspaces,
        &hosted.workspace,
        written,
        "Build it in the shell",
    )
    .await;
    let failed = reply_ended(&hosted, &first).await;
    assert!(failed.text.ends_with("ask me to go on."), "{}", failed.text);

    plays.lock().unwrap().push_back(Box::new(|_| {
        Ok(played(vec![Content::Text {
            text: "I went on, and it is built.".into(),
        }]))
    }));
    let second = write_to_agent(
        &hosted.actions,
        &hosted.workspaces,
        &hosted.workspace,
        written,
        "Go on.",
    )
    .await;
    let answered = reply_ended(&hosted, &second).await;
    assert_eq!(answered.text, "I went on, and it is built.");
    looking.abort();
}
