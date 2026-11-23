//! What a turn a workspace's server carries out finds by words: what the caller its tools act
//! as may read, and nothing else. All through the server's own action service, with no device
//! doing anything while it is asked.
use super::*;
use tilo_actions::remote::{ActionProvider as _, Caller, Found, Within};
use tilo_server::workspaces::Workspaces;
use tilo_workspace_service::agents as turns;
use ui::tilo_workspace_agent::{Agent, Bound};

/// What Ada tells her agent, and nobody else.
const HERS: &str = "The code word for Friday is heron.";
/// What Bo tells the same agent, in his own conversation with it.
const HIS: &str = "My locker's number is kestrel.";
/// What the workspace's letter says, which every member reads.
const LETTER: &str = "Gooseberry jam for the embassy.";
/// The app that keeps the letter.
const WRITER: &str = "so.tilo.word-processor";

/// A server of Ada's, and what a test reaches it through.
struct Served {
    /// The workspaces made at the start with Ada's page on each, until a test sets one up.
    opened: Vec<(String, Browser)>,
    /// The server itself and what it keeps on its disk, kept until the test ends.
    alive: crate::harness::hosted::Hosted,
    _state: tempfile::TempDir,
    actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: Workspaces,
    /// The server's own device and the one Ada's page is on, both hers.
    devices: (crate::support::Device, crate::support::Device),
    ada: SigningKey,
}

/// The app that keeps the tracker the issues are in.
const TASKS: &str = "so.tilo.tasks";

/// A rule about reading what is at `node`.
pub(super) fn reading(node: String, answer: &str) -> Bound {
    Bound {
        node,
        kinds: vec!["read".into()],
        answer: answer.into(),
    }
}

/// Where a rule names everything Chat keeps in `workspace`.
pub(super) fn chat_in(workspace: &str) -> String {
    format!("workspace:{workspace}/app:{CHAT}")
}

/// The person a key is the root of, by their id.
fn person_of(root: &SigningKey) -> String {
    hex::encode(root.verifying_key().to_bytes())
}

/// The conversation `root`'s person has with the agent `agent`: their direct conversation, as
/// a turn's session names where its tools look.
fn conversation_with(root: &SigningKey, agent: &str) -> Id {
    Id::from_raw(format!("direct:{}/{}", person_of(root), agent))
}

/// `person`'s session handed to the agent `agent` on a turn that `cause` gave it in
/// `conversation`, held to `rules`: the caller the tools of such a turn act as on a server.
fn on_a_turn(
    person: &Caller,
    agent: &str,
    (conversation, cause): (&Id, &Id),
    rules: Vec<tilo_action_policy::Rule>,
) -> Caller {
    person
        .clone()
        .delegated_to(agent.to_owned())
        .named(agent)
        .in_agent_session(format!("agent-session:test-{agent}"))
        .on_turn(
            tilo_action_policy::turn::Turn::new(conversation.to_string(), cause.to_string()),
            rules,
        )
}

/// A model that searches for each of `queries` at once and then answers. Every request it was
/// sent is kept in `asked`.
fn searching(queries: &[&str], asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let calls: Vec<Content> = queries
        .iter()
        .enumerate()
        .map(|(at, query)| {
            Content::ToolCall(ToolCall {
                id: format!("call-{at}"),
                name: "tilo_find".into(),
                input: serde_json::json!({"query": query}),
                extra_content: None,
            })
        })
        .collect();
    let asked = Arc::clone(asked);
    Arc::new(move || {
        Some(Box::new(Scripted {
            answers: vec![
                answer(calls.clone(), StopReason::ToolCalls, 1),
                answer(
                    vec![Content::Text {
                        text: "I looked.".into(),
                    }],
                    StopReason::EndTurn,
                    1,
                ),
            ],
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// What the tools a model called answered, in the order it called them, as the last request it
/// was sent carries them back.
fn tools_answered(asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Vec<String> {
    asked
        .lock()
        .unwrap()
        .last()
        .into_iter()
        .flat_map(|request| request.messages.iter())
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result.content.clone()),
            _ => None,
        })
        .collect()
}

/// Whether what `tilo_find` answered names something `app` keeps: a line that begins with the
/// path of something in it.
fn names_in(answered: &str, app: &str) -> bool {
    answered.lines().any(|line| {
        line.strip_prefix(app)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '@', ' ']))
    })
}

/// The action service of a server for one of its workspaces, as a turn's tools reach it.
macro_rules! service {
    ($served:expr, $workspace:expr) => {
        turns::WorkspaceActionProvider::new(
            $served.actions.clone(),
            WorkspaceId::parse($workspace).unwrap(),
        )
    };
}

/// What each tool step of a turn says in the server's log.
#[path = "hosted_step_timing.rs"]
mod step_timing;

/// The note of a step holds no tool back.
#[path = "hosted_step_notes.rs"]
mod step_notes;

impl Served {
    /// A server with `count` workspaces on it, Ada's page open on each.
    async fn start(count: usize) -> Self {
        Self::start_certified(count, 1).await
    }

    /// [`Self::start`], the server's own device certified at `issued_at_ns`. Every other device
    /// of the fixture is certified at one early moment, so a later one makes the server's own
    /// device the newest Ada has.
    async fn start_certified(count: usize, issued_at_ns: u64) -> Self {
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let mut server = enrol(&ada, "tilo-sh", DeviceCapabilities::member());
        server.cert.issued_at_ns = issued_at_ns;
        tilo_user_identity::sign_device_cert(&mut server.cert, &ada)
            .expect("her root certifies the server's own device");
        let browser = enrol(&ada, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let served = crate::harness::hosted::start(
            &ada,
            &server,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        let actions = served.roster.actions.clone();
        let workspaces = served.workspaces().unwrap().clone();
        let mut opened = Vec::new();
        for _ in 0..count {
            let workspace = workspaces.create("Team").await.unwrap().workspace;
            let page = Browser::open_at(&served, &browser, &server, &workspace).await;
            opened.push((workspace, page));
        }
        Self {
            opened,
            alive: served,
            _state: state,
            actions,
            workspaces,
            devices: (server, browser),
            ada,
        }
    }

    /// The next workspace made at the start, set up: Ada's agent in it, held to the rules
    /// `bound` makes for it; its chat, with her in it; what she tells the agent there; and a
    /// letter in the word processor. The workspace, her page (which keeps her device admitted) and her
    /// message.
    async fn workspace(&mut self, bound: impl FnOnce(&str) -> Vec<Bound>) -> (String, Browser, Id) {
        let (workspace, page) = self.opened.pop().expect("a workspace made at the start");
        let browser = &self.devices.1;
        let person = person_of(&self.ada);
        make_agent(&page, &self.workspaces, &workspace, &person, None).await;
        narrow_agent(
            &page,
            &self.workspaces,
            &workspace,
            &person,
            bound(&workspace),
        )
        .await;
        let said = write_to_agent(
            &self.actions,
            &self.workspaces,
            &workspace,
            (&self.ada, browser),
            HERS,
        )
        .await;
        // The letter, written through the tools a turn of her agent's has, and both it and
        // what she said found once before a test asks: what follows searches what the server
        // already read. (An agent that takes no part in her conversation is listed nothing of
        // it, so it is her own agent that looks.)
        let her_agent = on_a_turn(
            &Self::person(&self.ada, browser, &workspace),
            SCOUT,
            (&conversation_with(&self.ada, SCOUT), &said),
            Vec::new(),
        );
        for (path, arguments) in [
            ("word-processor#open", serde_json::json!({})),
            (
                "word-processor#append-paragraph",
                serde_json::json!({"text": LETTER}),
            ),
        ] {
            let call = serde_json::json!({"path": path, "args": arguments});
            let (text, failed) = self.tool(&workspace, &her_agent, "tilo_do", call).await;
            assert!(!failed, "{path}: {text}");
        }
        self.finds_soon(&workspace, &her_agent, ("Gooseberry", WRITER))
            .await;
        self.finds_soon(&workspace, &her_agent, ("heron", said.as_str()))
            .await;
        (workspace, page, said)
    }

    /// Admit the person of `device` to `workspace` as an editor: the server issues them an
    /// invitation and their device redeems it.
    async fn admit(&self, workspace: &str, device: &crate::support::Device) {
        crate::workspace_actions::resource_support::admit_with(
            &self.workspaces,
            &self.devices.0.cert,
            workspace,
            device,
            tilo_peer_state::join::Role::Editor,
            "Bo",
        )
        .await;
    }

    /// `root`'s person themselves in `workspace`, on `device`.
    fn person(root: &SigningKey, device: &crate::support::Device, workspace: &str) -> Caller {
        Caller::authenticated(
            root.verifying_key().to_bytes(),
            device.cert.device_public_key,
        )
        .in_workspace(workspace)
    }

    /// `root`'s person said `text` in their conversation with the agent at `when`, in seconds
    /// since the Unix epoch: written as their own words of that moment. `when` is the time of a
    /// post made in step with the others; a live post reads after what the conversation already
    /// holds. The message's id.
    async fn said_at(
        &self,
        workspace: &str,
        root: &SigningKey,
        _conversation: &Id,
        (text, _when): (&str, i64),
    ) -> Id {
        let person = person_of(root);
        super::hosted_agent_turns::write_message(&self.workspaces, workspace, &person, SCOUT, text)
            .await
    }

    /// What the agent tool `name` answers `caller` with for `arguments` in `workspace`: its
    /// words, and whether it failed.
    async fn tool(
        &self,
        workspace: &str,
        caller: &Caller,
        name: &str,
        arguments: serde_json::Value,
    ) -> (String, bool) {
        let tools = tilo_mcp::McpServer::for_turn(service!(self, workspace), caller.clone())
            .expect("a turn has its tools");
        let request = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        let answer = tools.handle(request).await.expect("answered");
        let text = answer["result"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let failed = answer["result"]["isError"] == true || answer.get("error").is_some();
        (text, failed)
    }

    /// Whether `needle` finds `wanted` for `caller` within a few seconds: what a search that
    /// may never find it is asked.
    async fn finds_in_time(&self, workspace: &str, caller: &Caller, wanted: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if self.finds(workspace, caller, wanted).await.contains(wanted) {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// What `needle` finds for `caller` in `workspace`, narrowed to `within`, best first, as
    /// the server's action service answers its search.
    async fn found(
        &self,
        workspace: &str,
        caller: &Caller,
        (needle, within): (&str, &Within),
    ) -> Vec<Found> {
        service!(self, workspace)
            .resource_search()
            .expect("a turn's action service searches its workspace")
            .find_within(caller, needle, within)
            .await
            .expect("the search answers")
            .expect("the search is not refused")
    }

    /// Everything `needle` finds for `caller` in `workspace`, as the server's action service
    /// answers its search.
    async fn finds(&self, workspace: &str, caller: &Caller, needle: &str) -> String {
        let found = self
            .found(workspace, caller, (needle, &Within::default()))
            .await;
        format!("{found:?}")
    }

    /// [`Self::finds`], asked until the answer names `wanted`: what a search answers once the
    /// server read what was just written.
    async fn finds_soon(
        &self,
        workspace: &str,
        caller: &Caller,
        (needle, wanted): (&str, &str),
    ) -> String {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let found = self.finds(workspace, caller, needle).await;
            if found.contains(wanted) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "{needle:?} never finds {wanted:?}: {found}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Ada narrows her agent to `bound`, as her page does: her device writes the rules into the
/// workspace's list of its agents. Answers once the server holds them; nothing for no rules.
pub(super) async fn narrow_agent(
    page: &Browser,
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    bound: Vec<Bound>,
) {
    if bound.is_empty() {
        return;
    }
    let agents = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let held = workspaces
        .resource_document(workspace, ui::tilo_workspace_agent::OWNER, &agents)
        .await
        .unwrap()
        .unwrap();
    let mut narrowed = ui::tilo_resource::Replica::<WorkspaceAgents>::load(&held)
        .unwrap()
        .as_author(person);
    let rules = bound.len();
    narrowed
        .apply(
            WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
                agent: Id::from_raw(SCOUT.to_owned()),
                bound,
            }),
        )
        .unwrap();
    let route = DocumentRoute {
        app_id: ui::tilo_workspace_agent::OWNER.into(),
        cluster_id: workspace.into(),
        document: agents.clone(),
    };
    let bytes = std::mem::take(narrowed.doc_mut()).save();
    page.peer
        .write(route, &collab_types::DocumentMutation::Snapshot(bytes))
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let bytes = workspaces
            .resource_document(workspace, ui::tilo_workspace_agent::OWNER, &agents)
            .await
            .unwrap()
            .unwrap();
        let listed = WorkspaceAgents::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
        if listed
            .agents
            .iter()
            .any(|agent| agent.id.0 == SCOUT && agent.bound.len() == rules)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the server never holds the agent's rules"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Answer the turn `message` gives the agent in `workspace`, on the server, with `model`.
async fn answered(served: &Served, workspace: &str, message: &Id, model: Model) {
    answered_knowing(served, workspace, message, model, None).await;
}

/// [`answered`], by agents told that the devices `placed` holds are the server's own, where
/// they are told any.
async fn answered_knowing(
    served: &Served,
    workspace: &str,
    message: &Id,
    model: Model,
    placed: Option<tilo_server::environment::EnvironmentRegistry>,
) {
    let agents = served
        .alive
        .approving(workspace, &[SCOUT])
        .await
        .scripted(model);
    let agents = if let Some(placed) = placed {
        agents.knowing_placed(placed)
    } else {
        agents
    };
    let deadline = tilo_test_env::hang_deadline();
    let turn = loop {
        // The ask `message` made of the agent: a turn's cause is the ask it is owed, not the
        // message that asked it.
        let asked = asked_on(&served.workspaces, workspace).await;
        let cause = ask_on(&asked, message, SCOUT).map(|ask| ask.at.id_string());
        let turns = agents.turns(workspace).await.unwrap();
        if let Some(turn) = turns
            .into_iter()
            .find(|turn| Some(&turn.cause) == cause.as_ref())
        {
            break turn;
        }
        assert!(
            Instant::now() < deadline,
            "{message} never is a turn of the agent's"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    tokio::time::timeout(Duration::from_secs(60), agents.answer(workspace, &turn))
        .await
        .expect("the turn ends")
        .unwrap();
}

/// The rules a test holds the agent to, made for a workspace by its id.
type Rules = fn(&str) -> Vec<Bound>;

/// A turn the server carries out searches the workspace with the tool every agent has, and is
/// answered with what its agent's rules let it read: everything for an agent nothing narrows,
/// nothing of an app its rules refuse it, and of a workspace it is refused only what a rule
/// lets it read again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_on_the_server_finds_what_its_agents_rules_let_it_read() {
    let mut served = Served::start(3).await;
    // What narrows the agent, whether its turn finds what its person told it, and whether it
    // finds the letter.
    let cases: [(&str, Rules, bool, bool); 3] = [
        ("an agent nothing narrows", |_| Vec::new(), true, true),
        (
            "an agent refused what Chat keeps",
            |workspace| vec![reading(chat_in(workspace), "deny")],
            false,
            true,
        ),
        (
            "an agent refused every read but Chat's",
            |workspace| {
                vec![
                    reading(format!("workspace:{workspace}"), "deny"),
                    reading(chat_in(workspace), "allow"),
                ]
            },
            true,
            false,
        ),
    ];
    for (which, rules, message, letter) in cases {
        let (workspace, page, said) = served.workspace(rules).await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let began = Instant::now();
        Box::pin(answered(
            &served,
            &workspace,
            &said,
            searching(&["heron", "Gooseberry"], &asked),
        ))
        .await;
        let found = tools_answered(&asked);
        eprintln!(
            "{which}: its turn took {:?}; tilo_find answered {found:?}",
            began.elapsed()
        );
        assert_eq!(found.len(), 2, "{which} searched twice: {found:?}");
        for answer in &found {
            assert!(
                !answer.contains("keeps no search"),
                "{which} has a search on its server: {answer}"
            );
        }
        assert_eq!(
            names_in(&found[0], "chat"),
            message,
            "{which}, what its person told it: {}",
            found[0]
        );
        assert_eq!(
            names_in(&found[1], "word-processor"),
            letter,
            "{which}, the letter: {}",
            found[1]
        );
        let shown = format!("{found:?}");
        assert!(
            message || !shown.contains("heron."),
            "{which} is shown nothing of the message it may not read: {shown}"
        );
        assert!(
            letter || !shown.contains("jam"),
            "{which} is shown nothing of the letter it may not read: {shown}"
        );
        page.peer.close();
    }
}

/// What the service's feed of changes answers `caller` after `after`.
async fn feed(
    served: &Served,
    workspace: &str,
    caller: &Caller,
    after: Option<String>,
) -> tilo_actions::remote::ChangePage {
    use tilo_actions::remote::RemoteActionMessage;
    let (actions, caller) = (served.actions.clone(), caller.clone());
    let workspace = WorkspaceId::parse(workspace).unwrap();
    let answered = tokio::task::spawn_blocking(move || {
        let asked = RemoteActionMessage::ChangesRequest {
            correlation: 1,
            after,
        };
        let answer = actions.serve(workspace, &caller, asked)?;
        answer
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
    })
    .await
    .unwrap();
    match answered {
        Some(RemoteActionMessage::ChangesResponse {
            result: Ok(page), ..
        }) => page,
        other => panic!("not a page of the feed: {other:?}"),
    }
}

/// The feed of changes tells a caller of what it may read and of nothing else, as a search
/// does. An agent whose rules refuse it what Chat keeps follows the feed outside a turn, held
/// to the rules the workspace's list keeps for it: it is told nothing of what is said in Chat
/// after it began to follow, neither the words nor that something of Chat changed. An agent
/// nothing narrows is told both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_feed_of_changes_tells_an_agent_nothing_of_an_app_its_rules_refuse_it() {
    let mut served = Served::start(2).await;
    // What narrows the agent, and whether it is told what is said in Chat.
    let cases: [(&str, Rules, bool); 2] = [
        ("an agent nothing narrows", |_| Vec::new(), true),
        (
            "an agent refused what Chat keeps",
            |workspace| vec![reading(chat_in(workspace), "deny")],
            false,
        ),
    ];
    for (which, rules, told) in cases {
        let (workspace, page, _) = served.workspace(rules).await;
        let ada = Served::person(&served.ada, &served.devices.1, &workspace);
        let agent = ada
            .clone()
            .delegated_to(SCOUT.to_owned())
            .named(SCOUT)
            .in_agent_session(format!("agent-session:test-feed-{SCOUT}"));
        let hers = feed(&served, &workspace, &ada, None).await.cursor;
        let its = feed(&served, &workspace, &agent, None).await.cursor;
        let now = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000_000).unwrap();
        served
            .said_at(
                &workspace,
                &served.ada,
                &conversation_with(&served.ada, SCOUT),
                ("The feed's word is siskin.", now - 30),
            )
            .await;
        // Her own read of the feed says when the server has taken the sentence in.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let read = feed(&served, &workspace, &ada, Some(hers.clone())).await;
            if format!("{read:?}").contains("siskin") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{which}: the feed never tells her what she said: {read:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let read = feed(&served, &workspace, &agent, Some(its)).await;
        let of_chat: Vec<_> = read
            .changes
            .iter()
            .filter(|change| change.app == CHAT)
            .collect();
        assert_eq!(
            !of_chat.is_empty(),
            told,
            "{which}, the changes of what Chat keeps that it is told: {of_chat:?}"
        );
        assert_eq!(
            format!("{read:?}").contains("siskin"),
            told,
            "{which}, the words said in Chat: {read:?}"
        );
        page.peer.close();
    }
}

/// The ask the service holds until a write lands in the workspace wakes a caller only for a
/// write to something it may read. An agent whose rules refuse it every read of the workspace
/// waits outside a turn while its person says something in Chat. Once the service has read
/// what landed, which is what wakes a wait, its person's wait is answered and the agent's is
/// not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_wakes_no_agent_whose_rules_refuse_it_the_read() {
    use tilo_actions::remote::{RemoteActionMessage, WatchScope};
    let mut served = Served::start(1).await;
    let (workspace, page, _) = served
        .workspace(|workspace| vec![reading(format!("workspace:{workspace}"), "deny")])
        .await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let agent = ada
        .clone()
        .delegated_to(SCOUT.to_owned())
        .named(SCOUT)
        .in_agent_session(format!("agent-session:test-wait-{SCOUT}"));
    let id = WorkspaceId::parse(&workspace).unwrap();
    let held = |caller: &Caller, correlation| {
        let asked = RemoteActionMessage::ChangedRequest {
            correlation,
            scope: WatchScope::Records,
        };
        served
            .actions
            .serve(id, caller, asked)
            .expect("the service answers a wait for a change")
    };
    // What landed so far is read before anyone waits.
    let from = feed(&served, &workspace, &ada, None).await.cursor;
    let began = Instant::now();
    let (hers, its) = (held(&ada, 1), held(&agent, 2));
    // The service takes its requests in the order they came: once this one is answered, both
    // waits are kept.
    feed(&served, &workspace, &ada, Some(from.clone())).await;
    let now = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000_000).unwrap();
    served
        .said_at(
            &workspace,
            &served.ada,
            &conversation_with(&served.ada, SCOUT),
            ("The wait's word is dunlin.", now - 30),
        )
        .await;
    // Her read of the feed has the service read what landed, and tells when it has.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let read = feed(&served, &workspace, &ada, Some(from.clone())).await;
        if format!("{read:?}").contains("dunlin") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the feed never tells her what she said: {read:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // A wait is kept for ten seconds: one that ended by itself was woken by nothing, and says
    // nothing of whom a write wakes.
    assert!(
        began.elapsed() < Duration::from_secs(8),
        "both waits were still kept when the service read the write: {:?}",
        began.elapsed()
    );
    let (hers, its) = tokio::task::spawn_blocking(move || {
        let hers = hers
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten();
        let its = its.wait_timeout(Duration::from_millis(500)).ok().flatten();
        (hers, its)
    })
    .await
    .unwrap();
    assert!(
        matches!(
            hers,
            Some(RemoteActionMessage::ChangedResponse { result: Ok(()), .. })
        ),
        "her own wait is answered when she says something: {hers:?}"
    );
    assert!(
        its.is_none(),
        "an agent refused every read is not answered when a write lands in Chat: {its:?}"
    );
    page.peer.close();
}

/// A write by another device answers a wait for a change by itself: no request to the service
/// follows the write. A person waits; their page writes the workspace's layout through its own
/// session, as a colleague's device would write anything; the wait is answered within a second.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_by_another_device_answers_a_wait_for_a_change_within_a_second() {
    use tilo_actions::remote::{RemoteActionMessage, WatchScope};
    let mut served = Served::start(1).await;
    let (workspace, mut page, _) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let id = WorkspaceId::parse(&workspace).unwrap();
    let asked = RemoteActionMessage::ChangedRequest {
        correlation: 1,
        scope: WatchScope::Records,
    };
    let waiting = served
        .actions
        .serve(id, &ada, asked)
        .expect("the service answers a wait for a change");
    // The service takes its requests in the order they came: once this one is answered, the
    // wait before it is kept.
    feed(&served, &workspace, &ada, None).await;
    let began = Instant::now();
    page.put_layout();
    let answered = tokio::task::spawn_blocking(move || {
        waiting
            .wait_timeout(Duration::from_secs(1))
            .map_err(|_still_held| ())
    })
    .await
    .unwrap();
    assert!(
        matches!(
            answered,
            Ok(Some(RemoteActionMessage::ChangedResponse {
                result: Ok(()),
                ..
            }))
        ),
        "the wait is answered within a second of another device's write (after {:?}): {answered:?}",
        began.elapsed()
    );
    page.peer.close();
}

/// What a person tells an agent is found by that person and by a turn of theirs, and by nobody
/// else: not by another member, though they read the workspace's chat, and not through a turn
/// another member started on the same agent, though the agent takes part in both conversations.
/// What the workspace shares, every one of them finds; and where Ada holds her agent to Chat
/// alone, a turn Bo starts on it finds the letter no more than the agent may, though Bo himself
/// does: his turn does what both of them may, and no more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_person_tells_an_agent_is_found_by_them_and_their_own_turns_alone() {
    let mut served = Served::start(1).await;
    let (workspace, page, hers) = served.workspace(|_| Vec::new()).await;
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_device = enrol(&bo, "bo-browser", DeviceCapabilities::member());
    served.admit(&workspace, &bo_device).await;
    let his = write_to_agent(
        &served.actions,
        &served.workspaces,
        &workspace,
        (&bo, &bo_device),
        HIS,
    )
    .await;

    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let bo_himself = Served::person(&bo, &bo_device, &workspace);
    let her_turn = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &hers),
        Vec::new(),
    );
    // A turn Bo starts on Ada's agent acts for him as her agent, as a server's turn does.
    let his_turn = on_a_turn(
        &bo_himself,
        SCOUT,
        (&conversation_with(&bo, SCOUT), &his),
        Vec::new(),
    )
    .agent_of(person_of(&served.ada));

    // Each finds their own, themselves and through a turn of theirs.
    for (who, caller, word, message) in [
        ("Ada", &ada, "heron", &hers),
        ("a turn of Ada's", &her_turn, "heron", &hers),
        ("Bo", &bo_himself, "kestrel", &his),
        ("a turn of Bo's", &his_turn, "kestrel", &his),
    ] {
        let began = Instant::now();
        let found = served
            .finds_soon(&workspace, caller, (word, message.as_str()))
            .await;
        eprintln!("{who} finds {word:?} in {:?}: {found}", began.elapsed());
    }
    // Nobody finds the other's.
    for (who, caller, word, message) in [
        ("Bo", &bo_himself, "heron", &hers),
        (
            "a turn Bo started on Ada's agent",
            &his_turn,
            "heron",
            &hers,
        ),
        ("Ada", &ada, "kestrel", &his),
        ("a turn of Ada's", &her_turn, "kestrel", &his),
    ] {
        let found = served.finds(&workspace, caller, word).await;
        assert!(
            !found.contains(message.as_str()) && !found.contains(word),
            "{who} does not find {word:?}: {found}"
        );
    }
    // What the workspace shares is found by all of them.
    for (who, caller) in [
        ("Bo", &bo_himself),
        ("a turn of Bo's", &his_turn),
        ("a turn of Ada's", &her_turn),
    ] {
        let found = served
            .finds_soon(&workspace, caller, ("Gooseberry", WRITER))
            .await;
        assert!(found.contains(WRITER), "{who} finds the letter: {found}");
    }
    let only_chat = Agent {
        id: ui::tilo_resource::Atom(SCOUT.into()),
        acts_for: person_of(&served.ada),
        bound: vec![
            reading(format!("workspace:{workspace}"), "deny"),
            reading(chat_in(&workspace), "allow"),
        ],
        ..Agent::default()
    };
    let his_bounded_turn = on_a_turn(
        &bo_himself,
        SCOUT,
        (&conversation_with(&bo, SCOUT), &his),
        only_chat.turn_rules(&workspace),
    )
    .agent_of(person_of(&served.ada));
    served
        .finds_soon(&workspace, &his_bounded_turn, ("kestrel", his.as_str()))
        .await;
    let found = served
        .finds(&workspace, &his_bounded_turn, "Gooseberry")
        .await;
    assert!(
        !found.contains(WRITER) && !found.contains("Gooseberry"),
        "a turn of his on an agent held to Chat finds no letter, though he does: {found}"
    );
    // What each of them finds of an issue in Tasks by its title is said here and not judged:
    // being held to what they may read takes nothing of Tasks from any of them.
    file_issue(
        &served.actions,
        &served.workspaces,
        &workspace,
        &served.ada,
        (&served.devices.0, &served.devices.1),
    )
    .await;
    let title = "Plan the launch";
    let by_her = served.finds_in_time(&workspace, &ada, title).await;
    let by_him = served
        .finds(&workspace, &bo_himself, title)
        .await
        .contains(title);
    let by_her_turn = served
        .finds(&workspace, &her_turn, title)
        .await
        .contains(title);
    eprintln!(
        "an issue is found by its title: by the server's own person {by_her}, by another \
         member {by_him}, by a turn of hers {by_her_turn}"
    );
    page.peer.close();
}

/// How the service decides an agent the workspace's own list does not hold.
#[path = "hosted_across.rs"]
mod across;

/// A workspace's folder is indexed as the tree's chunks land: a find over a hundred files reads
/// no chunk document and no file, however many of them the word it asks about is in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_find_reads_no_folder_document_and_no_file_once_the_folder_is_indexed() {
    // Where the time goes, as the service logs it, when `RUST_LOG` asks.
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let her_agent = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    let notes = 100;
    let (answered, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_do",
            serde_json::json!({
                "path": "files#new-folder",
                "args": {"parent": "", "name": "Bodies"},
            }),
        )
        .await;
    assert!(!failed, "the folder: {answered}");
    for file in 0..notes {
        let (answered, failed) = served
            .tool(
                &workspace,
                &her_agent,
                "tilo_do",
                serde_json::json!({
                    "path": "files#write-file",
                    "args": {
                        "folder": "Bodies",
                        "name": format!("note-{file}.md"),
                        "text": format!("The word grebe appears in body {file}.\n"),
                    },
                }),
            )
            .await;
        assert!(!failed, "{file}: {answered}");
    }
    // The folder's tree reaches the workspace before a find can index it: every file is a
    // node of it, and a write answered pending is not one until it lands.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let kept = served
            .workspaces
            .resource_document(
                &workspace,
                fs_protocol::tree::FILES_APP,
                &fs_protocol::tree::document_id(),
            )
            .await
            .ok()
            .flatten()
            .is_some();
        if kept {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never keeps the folder's tree after {notes} files"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The folder's files are read into the index between requests; asking a find brings the
    // notes' files into it as the writes land, so wait for every body to be in.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        served.finds(&workspace, &ada, "grebe").await;
        let warmed = served.actions.search_files_warmed() + served.actions.search_files_read();
        if warmed >= notes as u64 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the folder is indexed: {warmed} of {notes} files read"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // A find asked once the writes settle reads nothing of the folder it ranks.
    served.finds(&workspace, &ada, "grebe").await;
    for ask in 0..2 {
        let chunks = served.actions.search_files_chunks_read();
        let files = served.actions.search_files_read();
        let began = Instant::now();
        let found = served
            .found(&workspace, &her_agent, ("grebe", &Within::default()))
            .await;
        let elapsed = began.elapsed();
        let chunk_read = served.actions.search_files_chunks_read() - chunks;
        let file_read = served.actions.search_files_read() - files;
        eprintln!(
            "find {ask} over {notes} files: {elapsed:?}, {} found, {chunk_read} folder documents \
             and {file_read} files read",
            found.len()
        );
        assert!(
            found
                .iter()
                .any(|found| found.app.as_deref() == Some(fs_protocol::tree::FILES_APP)),
            "the word in the notes' files is found: {found:?}"
        );
        if ask == 1 {
            assert_eq!(chunk_read, 0, "a settled find reads no folder document");
            assert_eq!(file_read, 0, "a settled find reads no file");
        }
    }
    page.peer.close();
}

/// A hosted turn's tools reach a tilolang app built into its server: Chat's record is listed
/// through the package the server runs it from, a message is read by its words, and an action of
/// Chat's on it is carried out as the agent, on its person's behalf.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_lists_reads_and_changes_a_tl_app_through_its_package() {
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let her_agent = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    // What Chat publishes is listed under the app, through the record's own package.
    let (listed, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_ls",
            serde_json::json!({"path": "chat"}),
        )
        .await;
    assert!(!failed, "Chat is listed: {listed}");
    assert!(
        listed.contains("tl.so.tilo.chat"),
        "the listing names Chat's resources: {listed}"
    );
    // The search names her message by the path the tools take, as it names any resource.
    let (found, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_find",
            serde_json::json!({"query": "heron"}),
        )
        .await;
    assert!(!failed, "her message is found: {found}");
    let path = found
        .lines()
        .find(|line| line.contains("tl.so.tilo.chat"))
        .and_then(|line| line.split("  ").next())
        .expect("her message is found by its words")
        .to_owned();
    // Her message reads by the words she said.
    let (read, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_read",
            serde_json::json!({"path": path.clone()}),
        )
        .await;
    assert!(!failed, "the message reads: {read}");
    assert!(read.contains("heron"), "the message's words: {read}");
    assert!(!read.contains("true"), "it is not pinned yet: {read}");
    // Chat's own action on it is carried out as her agent, on her behalf.
    let (pinned, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_do",
            serde_json::json!({
                "path": format!("{path}#pin"),
                "args": {"pinned": true},
            }),
        )
        .await;
    assert!(!failed, "Chat's pin runs: {pinned}");
    let (read, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_read",
            serde_json::json!({"path": path.clone()}),
        )
        .await;
    assert!(!failed, "the message reads again: {read}");
    assert!(
        read.contains("true"),
        "the message is pinned, as it was written: {read}"
    );
    page.peer.close();
}

/// What the page's search asks the workspace answers with a word in an issue's body: the page
/// itself no longer reads bodies, its documents being named there from names alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_search_finds_a_word_in_an_issues_body() {
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let her_agent = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    for issue in 0..8 {
        let (answered, failed) = served
            .tool(
                &workspace,
                &her_agent,
                "tilo_do",
                serde_json::json!({
                    "path": "tasks#create",
                    "args": {
                        "title": format!("Maritime Talk {issue}"),
                        "body": "The hospitals are in this body.",
                    },
                }),
            )
            .await;
        assert!(!failed, "{issue}: {answered}");
    }
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = served
            .found(&workspace, &ada, ("hospitals", &Within::default()))
            .await;
        if found
            .iter()
            .any(|found| found.app.as_deref() == Some(TASKS))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the page's search never finds a word in an issue's body: {found:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    page.peer.close();
}

/// What the page's search asks the workspace answers a title prefix with the issue that carries
/// it, named as its own app lists it: what a person types in the search box.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_search_finds_a_title_prefix_in_an_issue() {
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let her_agent = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    let title = "Write a launch checklist note with five short items";
    let (answered, failed) = served
        .tool(
            &workspace,
            &her_agent,
            "tilo_do",
            serde_json::json!({
                "path": "tasks#create",
                "args": {
                    "title": title,
                    "body": "Link it here and move this to In review.",
                },
            }),
        )
        .await;
    assert!(!failed, "tasks#create: {answered}");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let found = served
            .found(&workspace, &ada, ("Write a launch", &Within::default()))
            .await;
        let kept_by = |app: &str| found.iter().any(|found| found.app.as_deref() == Some(app));
        if kept_by(TASKS) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a title prefix finds the issue: {found:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    page.peer.close();
}

/// What a find asks of the workspace is decided for the answers it returns, not for every
/// document it ranks, and kept per caller: the next find asks nothing, however many documents
/// the workspace holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_find_asks_what_its_caller_may_read_only_for_the_answers_it_returns() {
    // Where the time goes, as the service logs it, when `RUST_LOG` asks.
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let mut served = Served::start(1).await;
    let (workspace, page, said) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let her_agent = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &said),
        Vec::new(),
    );
    let notes = 60;
    for file in 0..notes {
        let (answered, failed) = served
            .tool(
                &workspace,
                &her_agent,
                "tilo_do",
                serde_json::json!({
                    "path": "tasks#create",
                    "args": {
                        "title": format!("Issue {file}"),
                        "body": "The word grebe appears in this body.",
                    },
                }),
            )
            .await;
        assert!(!failed, "{file}: {answered}");
    }
    // An issue's write can land after it was answered, and a write landing lets go of what every
    // caller was kept: the agent's finds are timed once the workspace took in every issue, when
    // Ada's next find keeps what her last one was answered.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        served
            .found(&workspace, &ada, ("grebe", &Within::default()))
            .await;
        let before = served.actions.search_permissions_read();
        served
            .found(&workspace, &ada, ("grebe", &Within::default()))
            .await;
        if served.actions.search_permissions_read() == before {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never took in the issues' writes"
        );
    }
    // An agent that may read everything pays for the answers it returns and keeps them: the
    // second find asks the workspace nothing.
    let mut reads = Vec::new();
    for ask in 0..2 {
        let before = served.actions.search_permissions_read();
        let began = Instant::now();
        let found = served
            .found(&workspace, &her_agent, ("grebe", &Within::default()))
            .await;
        let elapsed = began.elapsed();
        let asked = served.actions.search_permissions_read() - before;
        eprintln!(
            "agent find {ask} over {notes} notes: {elapsed:?}, {} found, {asked} permission reads",
            found.len()
        );
        assert!(!found.is_empty(), "the word in every note is found");
        reads.push(asked);
    }
    assert!(
        reads[0] > 0 && reads[0] < notes as u64,
        "a find asks about its own answers, not every document it ranks: {reads:?} for {notes} notes"
    );
    assert_eq!(
        reads[1], 0,
        "the next find answers from what the workspace kept: {reads:?}"
    );
    page.peer.close();
}
