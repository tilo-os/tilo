//! What a turn a workspace's server carries out finds by words: what the caller its tools act
//! as may read, and nothing else. All through the server's own action service, with no device
//! doing anything while it is asked.
use super::*;
use tilo_actions::remote::{ActionHost as _, Caller};
use tilo_relay::{
    workspace_agents as turns, workspace_join,
    workspaces::{IssueInvitationRequest, Workspaces},
};
use ui::tilo_workspace_agent::{Agent, Bound, Sub};

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
    _alive: Box<dyn std::any::Any>,
    _state: tempfile::TempDir,
    actions: tilo_agent_session::workspace_actions::WorkspaceActions,
    workspaces: Workspaces,
    /// The server's own device and the one Ada's page is on, both hers.
    devices: (crate::support::Device, crate::support::Device),
    ada: SigningKey,
}

/// A rule about reading what is at `node`.
fn reading(node: String, answer: &str) -> Bound {
    Bound {
        node,
        kinds: vec!["read".into()],
        answer: answer.into(),
    }
}

/// Where a rule names everything Chat keeps in `workspace`.
fn chat_in(workspace: &str) -> String {
    format!("workspace:{workspace}/app:{CHAT}")
}

/// The person a key is the root of, by their id.
fn person_of(root: &SigningKey) -> String {
    hex::encode(root.verifying_key().to_bytes())
}

/// The conversation of `root`'s person with the agent `agent`.
fn conversation_with(root: &SigningKey, agent: &str) -> Id {
    chat::Chat::direct_id([MemberId::person(person_of(root)), MemberId::agent(agent)])
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
            tilo_action_policy::turn::Turn::new(
                chat::summons::conversation_address(conversation),
                cause.to_string(),
            ),
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
        turns::WorkspaceActionHost::new(
            $served.actions.clone(),
            WorkspaceId::parse($workspace).unwrap(),
        )
    };
}

impl Served {
    /// A server with `count` workspaces on it, Ada's page open on each.
    async fn start(count: usize) -> Self {
        let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let server = enrol(&ada, "tilo-sh", DeviceCapabilities::member());
        let browser = enrol(&ada, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let (served, actions) = crate::harness::hosted::serving_records_with_actions(
            &ada,
            &server,
            state.path(),
            crate::common::catalogue,
        )
        .await;
        let workspaces = served.workspaces().unwrap().clone();
        let mut opened = Vec::new();
        for _ in 0..count {
            let workspace = workspaces.create("Team").await.unwrap().workspace;
            let page = Browser::open_at(&served, &browser, &server, &workspace).await;
            opened.push((workspace, page));
        }
        Self {
            opened,
            _alive: Box::new(served),
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
        let governor = self.workspaces.governor().clone();
        self.workspaces
            .prepare_application(&workspace, &governor, &crate::common::catalogue(), CHAT, "")
            .await
            .unwrap();
        self.introduce(&workspace, &person, "Ada").await;
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

    /// Put `person` into the chat of `workspace` as `name`: a conversation takes messages from
    /// the people the chat knows.
    async fn introduce(&self, workspace: &str, person: &str, name: &str) {
        let governor = self.workspaces.governor().clone();
        let initial = name.chars().take(1).collect::<String>();
        self.workspaces
            .author_application(
                workspace,
                &governor,
                &crate::common::catalogue(),
                CHAT,
                "",
                |doc| {
                    chat::demo::introduce(doc, person, name, &initial)
                        .map_err(|error| error.to_string())
                },
            )
            .await
            .unwrap();
    }

    /// Admit the person of `device` to `workspace` as an editor: the server issues them an
    /// invitation and their device redeems it.
    async fn admit(&self, workspace: &str, device: &crate::support::Device) {
        let invitation = self
            .workspaces
            .issue(
                workspace,
                &self.devices.0.cert,
                IssueInvitationRequest {
                    request_id: device.cert.device_public_key,
                    role: tilo_peer_state::join::Role::Editor,
                    recipient_user_public_key: Some(device.cert.user_public_key),
                    invitee_name: Some("Bo".into()),
                    inviter_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns()
                        .checked_add(600_000_000_000)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let verified = tilo_peer_state::join::decode_and_verify_invitation(
            &invitation.signed_invitation,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        let endpoint =
            test_relay::endpoint(iroh::SecretKey::from_bytes(&device.key.to_bytes())).await;
        workspace_join::WorkspaceJoinClient::new(endpoint)
            .redeem_hint(
                verified.grant().transport.first().unwrap(),
                invitation.signed_invitation.clone(),
                device.cert.clone(),
                &device.key,
                device.cert.device_public_key,
            )
            .await
            .unwrap();
    }

    /// `root`'s person themselves in `workspace`, on `device`.
    fn person(root: &SigningKey, device: &crate::support::Device, workspace: &str) -> Caller {
        Caller::authenticated(
            root.verifying_key().to_bytes(),
            device.cert.device_public_key,
        )
        .in_workspace(workspace)
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

    /// Everything `needle` finds for `caller` in `workspace`, as the server's action service
    /// answers its search.
    async fn finds(&self, workspace: &str, caller: &Caller, needle: &str) -> String {
        let found = service!(self, workspace)
            .resource_search()
            .expect("a turn's action service searches its workspace")
            .find(caller, needle)
            .await
            .expect("the search answers")
            .expect("the search is not refused");
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
async fn narrow_agent(
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
        .resource_document(workspace, CHAT, &agents)
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
        app_id: CHAT.into(),
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
            .resource_document(workspace, CHAT, &agents)
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
    let agents = HostAgents::new(
        served.workspaces.clone(),
        served.actions.clone(),
        Some(model),
    );
    let deadline = tilo_test_env::hang_deadline();
    let turn = loop {
        let turns = agents.turns(workspace).await.unwrap();
        if let Some(turn) = turns
            .into_iter()
            .find(|turn| turn.cause == message.to_string())
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

/// What a person tells an agent is found by that person and by a turn of theirs, and by nobody
/// else: not by another member, though they read the workspace's chat, and not through a turn
/// another member started on the same agent, though the agent takes part in both conversations.
/// What the workspace shares, every one of them finds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_person_tells_an_agent_is_found_by_them_and_their_own_turns_alone() {
    let mut served = Served::start(1).await;
    let (workspace, page, hers) = served.workspace(|_| Vec::new()).await;
    let (bo, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_device = enrol(&bo, "bo-browser", DeviceCapabilities::member());
    served.admit(&workspace, &bo_device).await;
    served.introduce(&workspace, &person_of(&bo), "Bo").await;
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
    let his_turn = on_a_turn(
        &bo_himself,
        SCOUT,
        (&conversation_with(&bo, SCOUT), &his),
        Vec::new(),
    );

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

/// A sub-agent finds nothing that Chat keeps: not the conversation of the agent that started
/// it, which that agent's own turn finds, and not under a rule set on it to let it. What it may
/// read elsewhere it finds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agent_finds_nothing_of_its_starters_conversation() {
    let mut served = Served::start(1).await;
    let (workspace, page, hers) = served.workspace(|_| Vec::new()).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let starter = on_a_turn(
        &ada,
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &hers),
        Vec::new(),
    );
    served
        .finds_soon(&workspace, &starter, ("heron", hers.as_str()))
        .await;

    // The rules every turn of a sub-agent carries are what its item in the workspace's list of
    // agents gives: with no rule of its own, and with one that would let it read Chat.
    let own = conversation_with(&served.ada, "helper");
    for bound in [Vec::new(), vec![reading(chat_in(&workspace), "allow")]] {
        let helper = Agent {
            id: ui::tilo_resource::Atom("helper".into()),
            name: "Helper".into(),
            keeper: person_of(&served.ada),
            bound,
            sub: Some(Sub {
                started_by: SCOUT.into(),
                conversation: own.to_string(),
                ..Sub::default()
            }),
            ..Agent::default()
        };
        let sub_agent = on_a_turn(&ada, "helper", (&own, &hers), helper.turn_rules(&workspace));
        let found = served.finds(&workspace, &sub_agent, "heron").await;
        assert!(
            !found.contains(hers.as_str()) && !found.contains("heron"),
            "a sub-agent finds nothing of its starter's conversation: {found}"
        );
        let found = served
            .finds_soon(&workspace, &sub_agent, ("Gooseberry", WRITER))
            .await;
        assert!(found.contains(WRITER), "{found}");
    }
    page.peer.close();
}
