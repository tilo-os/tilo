//! `claude` typed in a hosted workspace's Terminal, on a server somebody else operates as
//! tilo.sh is: the program reaches the workspace's server at its machine's agent socket, and
//! works in the workspace as an agent of its own once the person whose session started it
//! allows it.
//!
//! The person's page is their approver ([`tilo_server_client::questions`]): each connection is
//! put to them, naming the agent it would be (`claude-code@dev#1`). Allowed, it acts as that
//! agent for them, and History keeps what it does as that agent's. Denied, it is refused. Each
//! connection is an agent of its own (`#2` next), "Always allow" lets the next one of that
//! program in that environment in unasked and nowhere else, a session nobody started is refused
//! before anyone is asked, and Stop ends one agent alone.

use super::api::ISSUER;
use super::api_on_server::api_on_server_shelled;
use super::api_unattended::{Key, Person, Sheet, asked, backend, beside, waiting, workspace};
use super::*;
use serde_json::{Value, json};
use std::future::Future;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use tilo_server::machine_agents::{AgentConnection, MachineAgents as _, MachineAgentsServer};
use tilo_server_client::questions::{MemberQuestions, answer_questions};
use tilo_server_client::{CertifyError, MemberApprover};
use tilo_server_protocol::certify::ConfirmationAnswer;
use tokio::sync::mpsc;

/// The program that connects, as the machine names its executable.
const CLAUDE: &str = "/home/tilo/.local/bin/claude";

/// One program's connection to the machine's agent socket, as `tilo-runtime --connect` holds it.
struct Client {
    stream: UnixStream,
    lines: Option<BufReader<UnixStream>>,
    next: u64,
}

impl Client {
    /// A connection from a program `session` started in `environment` of `workspace`.
    fn connect(
        agents: &Arc<MachineAgentsServer>,
        workspace: &str,
        environment: &str,
        session: &str,
    ) -> Self {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let from = AgentConnection {
            workspace: workspace.to_owned(),
            environment: environment.to_owned(),
            session: session.to_owned(),
            agent: false,
            program: CLAUDE.to_owned(),
        };
        let agents = Arc::clone(agents);
        std::thread::spawn(move || agents.serve(from, theirs));
        Self {
            lines: Some(BufReader::new(ours.try_clone().unwrap())),
            stream: ours,
            next: 1,
        }
    }

    /// Send `method` with `params`: its id, or `None` once the server closed the connection.
    fn send(&mut self, method: &str, params: &Value) -> Option<u64> {
        let id = self.next;
        self.next += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stream
            .write_all(format!("{request}\n").as_bytes())
            .ok()
            .map(|()| id)
    }

    /// Say who it is, as an MCP client opens.
    fn open(&mut self) -> Option<u64> {
        self.send(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "Claude Code", "version": "2.1.0"}}),
        )
    }

    /// Wait, beside the page, for the answer to request `id`; `None` once the server closed
    /// the connection.
    async fn answer(
        &mut self,
        page: &mut Page<'_, impl Future<Output = CertifyError>>,
        id: u64,
    ) -> Option<Value> {
        let mut lines = self.lines.take().unwrap();
        let (lines, answer) = beside(
            page,
            tokio::task::spawn_blocking(move || {
                let answer = loop {
                    let mut line = String::new();
                    match lines.read_line(&mut line) {
                        Ok(0) | Err(_) => break None,
                        Ok(_) => {
                            let message: Value = serde_json::from_str(&line).unwrap();
                            if message["id"] == id {
                                break Some(message);
                            }
                        }
                    }
                };
                (lines, answer)
            }),
        )
        .await
        .unwrap();
        self.lines = Some(lines);
        answer
    }

    /// [`Self::send`], then [`Self::answer`].
    async fn say(
        &mut self,
        page: &mut Page<'_, impl Future<Output = CertifyError>>,
        method: &str,
        params: &Value,
    ) -> Option<Value> {
        let id = self.send(method, params)?;
        self.answer(page, id).await
    }
}

/// The person's page, kept up beside what the story waits for.
type Page<'a, F> = std::pin::Pin<&'a mut F>;

/// The agent names History gives the Terminal versions in `workspace`, by the agent member each
/// names.
async fn terminal_actors(key: &Key<'_>, workspace: &str) -> Vec<(String, String)> {
    let (status, history) = key
        .post(
            &format!("/v1/workspaces/{workspace}/resources/terminal/actions/history"),
            &json!({"last": 100}),
        )
        .await;
    assert_eq!(status, 200, "{history}");
    history["result"]["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|version| {
            Some((
                version["actor"]["by"]["member"]["agent"]
                    .as_str()?
                    .to_owned(),
                version["actor"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_program_in_the_workspaces_machine_works_there_as_its_own_agent_once_allowed() {
    Box::pin(works_as_its_own_agent()).await;
}

#[allow(
    clippy::too_many_lines,
    reason = "one story: the person's first sessions of Claude Code in their machine"
)]
async fn works_as_its_own_agent() {
    let on_server = Box::pin(api_on_server_shelled(ISSUER)).await;
    let api = Arc::clone(&on_server.api);
    let agents = Arc::clone(
        on_server
            .shell
            .as_ref()
            .and_then(|shell| shell.machine_agents())
            .expect("a server running its shells serves their machines' agents"),
    );
    let (ada, token) = backend(&on_server).await;
    let ada_key = Key { api: &api, token };
    let ws = workspace(&ada_key, "Studio").await;

    // Ada's tab holds her root: it is her approver at the server.
    let host = MemberApprover::new(on_server.server_host());
    let (answers, mut answered) = mpsc::unbounded_channel();
    let person = Person {
        asked: Arc::default(),
        answers,
    };
    let mut sheet = Sheet {
        asked: Arc::clone(&person.asked),
    };
    let mut questions = MemberQuestions {
        server: &host,
        root: &ada,
        device: None,
    };
    let page = answer_questions(&mut questions, &mut sheet, &mut answered, || {
        tokio::time::sleep(Duration::from_millis(50))
    });
    tokio::pin!(page);

    // Nobody started a session yet: a program claiming one is refused, nobody asked.
    let mut stray = Client::connect(&agents, &ws, "dev", "no-such-session");
    let id = stray.open().unwrap();
    let refused = stray.answer(&mut page, id).await.expect("an answer");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("started by nobody")),
        "{refused}"
    );
    assert_eq!(person.questions(), 0);

    // Ada runs a command in the workspace's Terminal: her session is the one `claude` starts in.
    let (status, ran) = beside(&mut page, ada_key.run(&ws, "echo hello")).await;
    let mut asked_so_far = 0;
    if let Some(run) = waiting(status, &ran) {
        asked_so_far += 1;
        asked(&mut page, &person, asked_so_far).await;
        person.answer(ConfirmationAnswer::AllowOnce);
        let (status, ran) = beside(&mut page, ada_key.follow(&ws, &run)).await;
        assert_eq!(status, 200, "{ran}");
    } else {
        assert_eq!(status, 200, "{ran}");
    }
    let sessions = agents.sessions(&ws);
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let (session, starter) = sessions[0].clone();
    assert_eq!(starter, hex::encode(ada.verifying_key().to_bytes()));

    // `claude` connects: Ada is asked about the agent it would be.
    let mut first = Client::connect(&agents, &ws, "dev", &session);
    let opening = first.open().unwrap();
    asked_so_far += 1;
    asked(&mut page, &person, asked_so_far).await;
    let question = person.last();
    assert_eq!(question.agent_id, "claude-code@dev#1", "{question:?}");
    assert_eq!(
        question.agent_label, "Claude Code in dev #1",
        "{question:?}"
    );
    assert_eq!(question.workspace, "Studio", "{question:?}");
    assert!(question.allow_always, "{question:?}");
    assert!(
        question
            .arguments
            .contains(&("Program".to_owned(), CLAUDE.to_owned())),
        "{question:?}"
    );
    person.answer(ConfirmationAnswer::AllowOnce);
    let opened = first.answer(&mut page, opening).await.expect("an answer");
    assert!(opened["result"]["serverInfo"].is_object(), "{opened}");
    assert_eq!(
        agents
            .running(&ws)
            .into_iter()
            .map(|running| (running.agent, running.name))
            .collect::<Vec<_>>(),
        [(
            "claude-code@dev#1".to_owned(),
            "Claude Code in dev #1".to_owned()
        )]
    );

    // What it does is its own: its command is asked about as the agent's, and History keeps it
    // as claude-code@dev#1's, for Ada.
    let call = first
        .send(
            "tools/call",
            &json!({"name": "tilo_do", "arguments":
                   {"path": "terminal#run", "args": {"command": "echo from-claude"}}}),
        )
        .unwrap();
    asked_so_far += 1;
    asked(&mut page, &person, asked_so_far).await;
    assert_eq!(
        person.last().agent_id,
        "claude-code@dev#1",
        "{:?}",
        person.last()
    );
    person.answer(ConfirmationAnswer::AllowOnce);
    let called = first.answer(&mut page, call).await.expect("an answer");
    assert_ne!(called["result"]["isError"], true, "{called}");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let actors = beside(&mut page, terminal_actors(&ada_key, &ws)).await;
        if actors
            .iter()
            .any(|(agent, name)| agent == "claude-code@dev#1" && name == "Claude Code in dev #1")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "History never kept the command as the agent's: {actors:?}"
        );
        beside(&mut page, tokio::time::sleep(Duration::from_millis(100))).await;
    }

    // A second connection is an agent of its own, and Ada lets that program in for good there.
    let mut second = Client::connect(&agents, &ws, "dev", &session);
    let opening = second.open().unwrap();
    asked_so_far += 1;
    asked(&mut page, &person, asked_so_far).await;
    assert_eq!(person.last().agent_id, "claude-code@dev#2");
    person.answer(ConfirmationAnswer::AllowAlways);
    let opened = second.answer(&mut page, opening).await.expect("an answer");
    assert!(opened["result"].is_object(), "{opened}");

    // Stop ends the first alone.
    assert!(agents.stop(&ws, "claude-code@dev#1"));
    assert!(
        first
            .say(&mut page, "tools/list", &json!({}))
            .await
            .is_none(),
        "the stopped agent's connection ended"
    );
    let listed = second
        .say(&mut page, "tools/list", &json!({}))
        .await
        .expect("the other agent goes on");
    assert!(listed["result"]["tools"].is_array(), "{listed}");
    assert_eq!(
        agents
            .running(&ws)
            .into_iter()
            .map(|running| running.agent)
            .collect::<Vec<_>>(),
        ["claude-code@dev#2"]
    );

    // "Always allow" lets its next session in that environment in, unasked.
    let mut third = Client::connect(&agents, &ws, "dev", &session);
    let opening = third.open().unwrap();
    let opened = third.answer(&mut page, opening).await.expect("an answer");
    assert!(opened["result"].is_object(), "{opened}");
    assert_eq!(person.questions(), asked_so_far, "nobody was asked");
    assert!(
        agents
            .running(&ws)
            .iter()
            .any(|running| running.agent == "claude-code@dev#3")
    );

    // In another environment it is asked again; denied, it is refused.
    let mut elsewhere = Client::connect(&agents, &ws, "site", &session);
    let opening = elsewhere.open().unwrap();
    asked_so_far += 1;
    asked(&mut page, &person, asked_so_far).await;
    assert_eq!(person.last().agent_id, "claude-code@site#4");
    person.answer(ConfirmationAnswer::Deny);
    let refused = elsewhere
        .answer(&mut page, opening)
        .await
        .expect("an answer");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("did not let")),
        "{refused}"
    );
    assert!(
        !agents
            .running(&ws)
            .iter()
            .any(|running| running.environment == "site")
    );
}
