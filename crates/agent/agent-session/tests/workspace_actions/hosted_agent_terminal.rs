//! An agent a workspace's server works executes commands in the workspace's Terminal for its
//! person, on their own server, and is held to what they allow.
//!
//! A command asks the person first, in the conversation the turn is in, and the person's own
//! answer carries it out, once, with the agent told what it printed. A second command asks again. Once
//! the person allowed the agent that one action for good, from its Agents settings, it is
//! carried out unasked, and taken back it asks again.
use super::api_unattended::{Person, Sheet, beside};
use super::hosted_agent_turns::{SCOUT, conversation_part, make_agent, write_to_agent};
use super::*;
use std::sync::Arc;
use tilo_server::workspaces::{
    Workspaces,
    person_model::{
        ProviderProtocol,
        scripted::{Linking, Reply, ScriptedProvider},
    },
};
use tilo_server_client::MemberApprover;
use tilo_server_client::questions::{MemberQuestions, answer_questions};
use tilo_workspace_service::agents::ServerAgents;
use tokio::sync::mpsc;
use ui::tilo_resource::{Id, Resource as _};

const CHAT: &str = "so.tilo.chat";
/// Where the workspace keeps its agents.
const AGENTS: &str = ui::tilo_workspace_agent::OWNER;

/// Ada's server, keeping her workspace, with her assistant on it, her own model, and a shell for
/// the workspace's commands.
struct OnTheirServer {
    server: crate::harness::hosted::Hosted,
    actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: Workspaces,
    workspace: String,
    page: Browser,
    ada: SigningKey,
    browser: crate::support::Device,
    agents: ServerAgents,
    model: Arc<ScriptedProvider>,
    address: tilo_server_client::ServerAddress,
    /// The server's own device, as the workspace's documents hold it.
    server_device: [u8; 32],
    _state: tempfile::TempDir,
    _shells: tempfile::TempDir,
    /// The VM host the workspace's programs run on, when the server sells it one.
    vms: Option<ush::hosted_vm::test_host::TestHost>,
}

impl OnTheirServer {
    async fn start() -> Self {
        Self::start_with(None, &[]).await
    }

    /// [`Self::start`], with the server's own model answering her assistant where one is
    /// given: she then links no model of her own. Her assistant is approved for `apps` alone,
    /// where she names any.
    async fn start_with(
        servers_model: Option<tilo_workspace_service::agents::Model>,
        apps: &[&str],
    ) -> Self {
        Self::start_on(servers_model, apps, false).await
    }

    /// [`Self::start`], whose workspace runs its programs in a VM, as a workspace hosted by a
    /// server that sells them does: the VM host keeps what each program it started was given.
    async fn start_with_vm() -> Self {
        Self::start_on(None, &[], true).await
    }

    async fn start_on(
        servers_model: Option<tilo_workspace_service::agents::Model>,
        apps: &[&str],
        in_vm: bool,
    ) -> Self {
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let holder = enrol(&ada, "tilo-sh", DeviceCapabilities::member());
        let browser = enrol(&ada, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let shells = tempfile::tempdir().unwrap();
        let vms = in_vm.then(|| {
            let vms = ush::hosted_vm::test_host::TestHost::start(state.path());
            vms.add_card("acct-1");
            vms
        });
        let shell = match &vms {
            Some(vms) => Arc::new(tilo_server::agents::HostShells::at(
                tilo_server::agents::ShellsAt::Here(
                    ush::environment_terminals::EnvironmentTerminals::new(Arc::new(
                        ush::workspace_shell::WorkspaceShells::new(
                            shells.path().join("shells"),
                            Some(vms.target("acct-1")),
                        ),
                    )),
                    Arc::new(tilo_server::machine_agents::MachineAgentsServer::new(None)),
                ),
            )),
            None => tilo_server::agents::host_shells(shells.path().join("shells")).unwrap(),
        };
        let server = crate::harness::hosted::start(
            &ada,
            &holder,
            state.path(),
            crate::harness::hosted::Options {
                shell: Some(shell),
                ..crate::harness::hosted::Options::offering(crate::common::catalogue)
            },
        )
        .await;
        let actions = server.roster.actions.clone();
        let address = tilo_server_client::ServerAddress::at(server.node_addr());
        let workspaces = server.workspaces().unwrap().clone();
        let workspace = workspaces.create("Team").await.unwrap().workspace;
        let acts_for = ada.verifying_key().to_bytes();
        assert!(
            workspaces
                .name_person_everywhere(acts_for, "Ada", tilo_clock::unix_now_ms())
                .await
        );
        let page = Browser::open_at(&server, &browser, &holder, &workspace).await;
        let person = hex::encode(acts_for);
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
                        key: Some("the-persons-own-model-key"),
                        models: &["gemini-main"],
                    },
                    true,
                )
                .await
                .unwrap();
        }
        let agents = Box::pin(server.approving_for(&workspace, &[SCOUT], apps))
            .await
            .scripted(servers_model);
        Self {
            server,
            actions,
            workspaces,
            workspace,
            page,
            ada,
            browser,
            agents,
            model,
            address,
            server_device: holder.cert.device_public_key,
            _state: state,
            _shells: shells,
            vms,
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

/// An assistant whose approval names no Terminal runs nothing there: its command is refused in
/// words it can tell its person, and nothing is run or asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_approved_without_the_terminal_runs_no_command() {
    let hosted = OnTheirServer::start_with(None, &[CHAT]).await;
    hosted
        .turn(
            "Say hello in the shell",
            vec![
                executes("first", "echo first-echo-$((40+2))"),
                Reply::text("I may not."),
            ],
        )
        .await;
    assert!(
        hosted.was_shown("this agent was not approved to change"),
        "the model was shown:\n{}",
        hosted.shown()
    );
    assert!(!hosted.was_shown("first-echo-42"), "nothing was run");
    assert!(hosted.questions().await.is_empty(), "nobody was asked");
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
async fn an_agent_allowed_to_execute_commands_does_unasked_until_its_person_takes_it_back() {
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
    let model: tilo_workspace_service::agents::Model = {
        let plays = Arc::clone(&plays);
        Arc::new(move || {
            Some(Box::new(Played(Arc::clone(&plays)))
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        })
    };
    let hosted = OnTheirServer::start_with(Some(model), &[]).await;
    let command =
        AnswerScope::action(ui::apps::TERMINAL, "terminal.session", "run").node(&hosted.workspace);
    hosted.bound(&[(&command, "allow")]).await.unwrap();
    // The server looks at the conversation whenever something is written there.
    let looking = {
        let (agents, workspace) = (hosted.agents.clone(), hosted.workspace.clone());
        let mut writes = hosted.workspaces.document_writes();
        tokio::spawn(async move {
            while !matches!(
                writes.recv().await,
                Err(tokio::sync::broadcast::error::RecvError::Closed)
            ) {
                Box::pin(agents.look(&workspace)).await.unwrap();
            }
        })
    };

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

/// What the VM a command ran in was given of the actor who asked for it.
fn launched_env<'a>(
    launched: &'a [ush::hosted_vm::test_host::Launched],
    program: &str,
) -> Option<&'a ush::hosted_vm::test_host::Launched> {
    launched
        .iter()
        .find(|(argv, _)| argv.first().is_some_and(|first| first == program))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commands_git_identity_is_the_agent_that_asked_as_the_workspace_names_it() {
    Box::pin(identity_in_the_vm()).await;
}

async fn identity_in_the_vm() {
    let hosted = OnTheirServer::start_with_vm().await;
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
            "Commit in the shell",
            vec![
                executes(
                    "commit",
                    "sh -c 'echo \"$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL>\"'",
                ),
                Reply::text("I asked you first."),
            ],
        )),
    )
    .await;
    let asked_in_conversation = beside(&mut page, hosted.questions()).await;
    let (question, _) = asked_in_conversation
        .into_iter()
        .next()
        .expect("the command is asked before it runs");

    hosted.model.then(Reply::text("It ran."));
    beside(&mut page, hosted.answers(&question, "allow")).await;
    let settled = beside(&mut page, hosted.settled(&question)).await;
    assert_eq!(
        settled
            .outcome
            .as_ref()
            .map(|outcome| outcome.outcome.0.as_str()),
        Some(chat::question::DONE),
        "the command ran: {settled:?}; the model was shown:\n{}",
        hosted.shown()
    );

    let deadline = tilo_test_env::hang_deadline();
    let vms = hosted
        .vms
        .as_ref()
        .expect("the server sells its workspace a VM");
    loop {
        let launched = vms.launched();
        if let Some((argv, env)) = launched_env(&launched, "sh") {
            let git = |name: &str| -> Vec<String> {
                env.iter()
                    .filter(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
                    .collect()
            };
            for name in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
                assert_eq!(
                    git(name),
                    vec!["Scout".to_owned()],
                    "the agent's own name signs its work, not its key: {argv:?} {env:?}"
                );
            }
            for name in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
                assert_eq!(
                    git(name),
                    vec!["scout@team.tilo".to_owned()],
                    "its address is the workspace's, never the machine's loopback: {argv:?} {env:?}"
                );
            }
            // The person herself, from the device she signed in on: the same VM run, and what
            // it makes signs as her, at her workspace's address.
            let actions = tilo_session_actions::SessionActions::new(
                &hosted.page.peer,
                hosted.page.workspace,
                hosted.server_device,
            );
            let mcp = tilo_mcp::McpServer::for_person(
                actions,
                hosted.page.workspace.to_string(),
                &hex::encode(hosted.ada.verifying_key().to_bytes()),
            );
            let asked_before = person.questions();
            let (text, failed) = beside(
                &mut page,
                Box::pin(super::agent_mcp::tool(
                    &mcp,
                    "tilo_do",
                    serde_json::json!({
                        "path": "terminal#run",
                        "args": {"command": "sh -c 'echo \"$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL>\"'"},
                    }),
                )),
            )
            .await;
            assert!(!failed, "her command is taken: {text}");
            let mut allowed = false;
            loop {
                if !allowed && person.questions() > asked_before {
                    person.answer(tilo_server_protocol::certify::ConfirmationAnswer::AllowOnce);
                    allowed = true;
                }
                let launched = vms.launched();
                let mut hers = launched
                    .iter()
                    .filter(|(argv, _)| argv.first().is_some_and(|program| program == "sh"));
                if let Some((argv, env)) = hers.next_back() {
                    let git = |name: &str| -> Vec<String> {
                        env.iter()
                            .filter(|(key, _)| key == name)
                            .map(|(_, value)| value.clone())
                            .collect()
                    };
                    if git("GIT_AUTHOR_NAME") == ["Ada".to_owned()] {
                        for name in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
                            assert_eq!(
                                git(name),
                                vec!["Ada".to_owned()],
                                "her own name signs her work: {argv:?} {env:?}"
                            );
                        }
                        for name in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
                            assert_eq!(
                                git(name),
                                vec!["ada@team.tilo".to_owned()],
                                "her address is her workspace's: {argv:?} {env:?}"
                            );
                        }
                        return;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "her command never reached the workspace's VM: {launched:?}"
                );
                beside(&mut page, tokio::time::sleep(Duration::from_millis(50))).await;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the command never reached the workspace's VM ({} wakes): {launched:?}; the model was shown:\n{}",
            vms.wakes_asked(),
            hosted.shown()
        );
        beside(&mut page, tokio::time::sleep(Duration::from_millis(50))).await;
    }
}
