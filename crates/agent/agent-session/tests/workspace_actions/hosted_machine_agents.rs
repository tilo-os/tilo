//! `claude` typed in a hosted workspace's Terminal, on a server somebody else operates as
//! tilo.sh is: the program reaches the workspace's server at its machine's agent socket, and
//! works in the workspace as an agent of its own once the person whose session started it
//! lets it in.
//!
//! The ask is the call's own asked invocation record: the person's own device lists it
//! ([`tilo_actions::remote::asked::WAITING`]) and answers it (`resolve`), and the host lets
//! the program in on their approve. Allowed, it acts as that agent for them
//! (`claude-code@dev#1`), and History keeps what it does as that agent's. Denied, it is
//! refused. "Always let it in" leaves a grant that lets the next connection of that program
//! in that environment in unasked, and nowhere else; revoking the grant asks again, and a
//! session nobody started is refused before anyone is asked.

use serde_json::{Value, json};
use std::future::Future;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use tilo_actions::remote::InvokeArgument;
use tilo_actions::remote::asked::{self, Asked, AskedList};
use tilo_actions::remote::workspace_invocations;
use tilo_server::machine_agents::{AgentConnection, MachineAgents as _, MachineAgentsServer};

use super::api::ISSUER;
use super::api_on_server::api_on_server_shelled;
use super::api_unattended::{Key, backend, workspace};
use super::*;

/// The program that connects, as the machine names its executable.
const CLAUDE: &str = "/home/tilo/.local/bin/claude";

/// The question that waits for the person, once it is asked.
async fn asked(page: &mut Page, workspace: &str) -> Asked {
    let _ = workspace;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let listed = page
            .invoke_on(
                asked::PROVIDER,
                ActionAddress::new(asked::WAITING, asked::LIST),
                InvokeArgument::None,
            )
            .await
            .expect("the person's own device reads what waits for them");
        let list: AskedList =
            serde_json::from_value(listed.value.unwrap_or_default()).expect("a list of asks");
        if let Some(question) = list.questions.first() {
            assert_eq!(list.questions.len(), 1, "one ask at a time: {list:?}");
            return question.clone();
        }
        assert!(Instant::now() < deadline, "the person was never asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// What waits for the person now.
async fn waiting(page: &mut Page) -> Vec<Asked> {
    let listed = page
        .invoke_on(
            asked::PROVIDER,
            ActionAddress::new(asked::WAITING, asked::LIST),
            InvokeArgument::None,
        )
        .await
        .expect("the person's own device reads what waits for them");
    serde_json::from_value::<AskedList>(listed.value.unwrap_or_default())
        .expect("a list of asks")
        .questions
}

/// The person answers `question` with `resolution`: the record settles.
async fn answers(page: &mut Page, question: &Asked, resolution: &str) {
    page.invoke_on(
        workspace_invocations::PROVIDER,
        ActionAddress::new(question.record.record.clone(), asked::RESOLVE),
        InvokeArgument::Json(
            json!({"resolution": resolution})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ),
    )
    .await
    .expect("the person's answer reaches the record");
}

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

    /// Send `method` with `params`: its id.
    fn send(&mut self, method: &str, params: &Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stream
            .write_all(format!("{request}\n").as_bytes())
            .unwrap();
        id
    }

    /// Say who it is, as an MCP client opens.
    fn open(&mut self) -> u64 {
        self.send(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "Claude Code", "version": "2.1.0"}}),
        )
    }

    /// Wait for the answer to request `id`; `None` once the server closed the connection.
    async fn answer(&mut self, id: u64) -> Option<Value> {
        let mut lines = self.lines.take().unwrap();
        let (lines, answer) = tokio::task::spawn_blocking(move || {
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
        })
        .await
        .unwrap();
        self.lines = Some(lines);
        answer
    }
}

/// The workspace on the server, its machine's agents, and Ada's own page into it.
struct Fixture {
    workspace: String,
    agents: Arc<MachineAgentsServer>,
    /// Where the person answers what waits for them.
    page: Page,
    /// Ada, the workspace's person.
    person: [u8; 32],
    /// What lets the person's answer leave a grant the next connection reads.
    workspaces: tilo_workspace_service::workspaces::Workspaces,
    laptop: Device,
}

async fn fixture() -> Fixture {
    let on_server = api_on_server_shelled(ISSUER).await;
    let agents = Arc::clone(
        on_server
            .shell
            .as_ref()
            .and_then(|shell| shell.machine_agents())
            .expect("a server running its shells serves their machines' agents"),
    );
    let (ada, token) = backend(&on_server).await;
    let key = Key {
        api: &on_server.api,
        token,
    };
    let workspace = workspace(&key, "Studio").await;
    let person = ada.verifying_key().to_bytes();
    let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
    let page = Page::open_workspace(&on_server.hosted, &laptop, &on_server.host, &workspace).await;
    // Ada ran a command in the workspace's Terminal: her session is the one `claude` starts in.
    let session =
        tilo_actions::remote::Caller::authenticated(person, laptop.cert.device_public_key)
            .in_workspace(workspace.clone());
    agents.started(&workspace, "s1", &session);
    let workspaces = on_server
        .hosted
        .server
        .workspaces()
        .expect("the server governs its workspaces")
        .clone();
    Fixture {
        workspace,
        agents,
        page,
        person,
        workspaces,
        laptop,
    }
}

impl Fixture {
    /// Whoever answers `agent` in `scope`: the grant an "always" leaves, or its revocation.
    async fn answer(&self, agent: &str, offer: tilo_action_policy::Answer) {
        use tilo_action_policy::{ActionKind, AnswerScope, Grantee, agent_answers};
        let scope = AnswerScope::action(
            tilo_server::machine_agents::PROVIDER,
            tilo_server::machine_agents::ENVIRONMENT_KIND,
            tilo_server::machine_agents::LET_IN,
        );
        let grant = agent_answers::answer_grant(
            &self.workspace,
            Grantee::agent(agent),
            (&scope, ActionKind::Outside),
            offer,
            Grantee::person(hex::encode(self.person)),
        );
        self.workspaces
            .put_workspace_grants(&self.workspace, &self.laptop.cert, [7; 32], vec![grant])
            .await
            .expect("the workspace's policy takes the answer");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_program_works_there_as_its_own_agent_once_its_person_lets_it_in() {
    let mut fixture = fixture().await;
    let agents = Arc::clone(&fixture.agents);
    let ws = fixture.workspace.clone();

    // Nobody started a session: a program claiming one is refused, nobody asked.
    let mut stray = Client::connect(&agents, &ws, "dev", "no-such-session");
    let opening = stray.open();
    let refused = stray.answer(opening).await.expect("an answer");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("started by nobody")),
        "{refused}"
    );
    assert!(waiting(&mut fixture.page).await.is_empty());

    // `claude` connects: the person's own page sees the ask, naming the agent it would be.
    let mut first = Client::connect(&agents, &ws, "dev", "s1");
    let opening = first.open();
    let question = asked(&mut fixture.page, &ws).await;
    assert_eq!(
        question.facts.by,
        tilo_actions::remote::asked::AskedBy::Agent(tilo_actions::remote::asked::Named {
            id: "claude-code@dev".into(),
            name: "Claude Code in dev".into(),
        }),
        "{question:?}"
    );
    assert_eq!(question.on.as_deref(), Some("dev"), "{question:?}");
    assert_eq!(question.facts.fact("Environment"), Some("dev"));
    assert_eq!(question.facts.fact("Program"), Some(CLAUDE));
    assert_eq!(question.facts.verb, "let in");
    assert_eq!(question.facts.plural.as_deref(), Some("environments"));

    // Their approve runs it: the program is let in as its own agent.
    answers(&mut fixture.page, &question, "approve").await;
    let opened = first.answer(opening).await.expect("an answer");
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

    // Another connection asks again, and their decline refuses it.
    let mut second = Client::connect(&agents, &ws, "dev", "s1");
    let opening = second.open();
    let question = asked(&mut fixture.page, &ws).await;
    answers(&mut fixture.page, &question, "decline").await;
    let refused = second.answer(opening).await.expect("an answer");
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("did not let")),
        "{refused}"
    );
    assert_eq!(
        agents.running(&ws).len(),
        1,
        "only the one they let in is at work"
    );

    // Allowed for good, the same program in the same environment goes in unasked.
    let mut third = Client::connect(&agents, &ws, "dev", "s1");
    let opening = third.open();
    let question = asked(&mut fixture.page, &ws).await;
    answers(&mut fixture.page, &question, "approve_and_grant").await;
    let opened = third.answer(opening).await.expect("an answer");
    assert!(opened["result"].is_object(), "{opened}");
    let mut fourth = Client::connect(&agents, &ws, "dev", "s1");
    let opening = fourth.open();
    let opened = fourth.answer(opening).await.expect("an answer");
    assert!(opened["result"].is_object(), "{opened}");
    assert!(
        waiting(&mut fixture.page).await.is_empty(),
        "nobody was asked the second time"
    );

    // Another environment is not what they allowed.
    let mut elsewhere = Client::connect(&agents, &ws, "site", "s1");
    let _opening = elsewhere.open();
    let question = asked(&mut fixture.page, &ws).await;
    assert_eq!(
        question.facts.by,
        tilo_actions::remote::asked::AskedBy::Agent(tilo_actions::remote::asked::Named {
            id: "claude-code@site".into(),
            name: "Claude Code in site".into(),
        }),
        "{question:?}"
    );

    // Taking the answer back asks again.
    fixture
        .answer("claude-code@dev", tilo_action_policy::Answer::Ask)
        .await;
    let mut fifth = Client::connect(&agents, &ws, "dev", "s1");
    let _opening = fifth.open();
    let _question = asked(&mut fixture.page, &ws).await;
}
