//! Skills on the server, with no device of the person doing anything: the person's own agent
//! writes down how a kind of work is done and corrects it through its turn's own tools, a fresh
//! turn is told the skill that fits what it is asked, a result the person rejected is kept as
//! the skill's example, a sub-agent started for a job is told its skill and works with the
//! model the skill names, and nothing of one person's skills reaches a turn another person
//! started. What a sub-agent is handed is given to its turn by the server and stands in
//! nothing the workspace shares: not in the list of its agents, not in its messages, on the
//! server and on another member's device alike.
use super::*;
use tilo_peer_state::join::decode_and_verify_invitation;
use tilo_server::workspace_join::WorkspaceJoinClient;
use tilo_server::workspaces::IssueInvitationRequest;
use tilo_server::workspaces::person_model::scripted::Linking;
use tilo_server::workspaces::person_model::{
    Events, ModelProvider, ProviderCall, ProviderFuture, ProviderProtocol,
};
use tilo_server::workspaces::person_skills;
use ui::tilo_workspace_agent::Bound;

/// A turn's own provider of its keeper's skills, as the agent tools name it, and its verbs.
const SKILLS: &str = "skills/list";
const WRITE: &str = "skills/list#write-skill";
const CORRECT: &str = "skills/list#correct-skill";
const KEEP_EXAMPLE: &str = "skills/list#keep-example";
/// The team's verb that starts a sub-agent.
const START: &str = "sub-agents/team#start-sub-agent";

/// The keys of the two providers the keeper links: made up for these tests.
const MAIN_KEY: &str = "sk-made-up-for-the-main-thread";
const PARTS_KEY: &str = "sk-made-up-for-certain-sub-agents";

/// What opens the part of a request that tells a turn its keeper's skills.
const TOLD: &str = "Skills you keep for this person";
/// What a turn that keeps skills is told about doing so, before every ask.
const TOLD_HOW: &str = "You keep skills for this person";
/// How the market analysis is done once it is corrected, and how it was written first.
const CORRECTED: &str = "One row per company, each with its source.";
const FIRST: &str = "Write three paragraphs of prose.";
/// How the other skill is done: no job of these tests is of its kind.
const OTHER: &str = "Say what moved first.";
/// The team of a turn's agent, as the agent tools name it.
const TEAM: &str = "sub-agents/team";
/// What opens the part of a request that tells a sub-agent's turn the skill it was handed.
const HANDED: &str = "You work by a skill your person keeps";
/// A skill its person wrote on a device of theirs and never said in a conversation: what it
/// is called, and what it says. Wherever one of these words stands, a skill was written there.
const REVIEW: &str = "Supplier review";
const REVIEW_SAYS: [&str; 5] = [
    "Reviewing a supplier ahead of an order",
    "Hold the last three deliveries against their dates.",
    "One verdict, then every late delivery.",
    "Dependable: none of the last three came late.",
    "Short, and it gives the count",
];

/// One call a person's provider was asked.
#[derive(Clone)]
struct Seen {
    /// Whether it is a call of a sub-agent's turn.
    sub: bool,
    /// The provider, as the person named it.
    provider: String,
    /// The model asked.
    model: String,
    /// The key the call was handed.
    key: Option<String>,
    request: ModelRequest,
}

/// A person's provider that answers as a model does what the words it is asked with say: a
/// first line `do PATH ARGS` invokes what the path names with the arguments, `read PATH` reads
/// it, and once a tool answered, the answer is what the tool said, so a test reads in the
/// reply what the turn's tool did. A sub-agent's turn says its job is done. It keeps every
/// call with the provider, the model and the key it was made with.
#[derive(Default)]
struct Routed {
    seen: Mutex<Vec<Seen>>,
}

impl Routed {
    /// Every call so far, in the order asked.
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl ModelProvider for Routed {
    fn answer<'a>(&'a self, call: ProviderCall<'a>, _: Events<'a>) -> ProviderFuture<'a> {
        let request = call.request;
        self.seen.lock().unwrap().push(Seen {
            sub: of_sub_agent(request),
            provider: call.profile.label.clone(),
            model: call.model.id.clone(),
            key: call.key().map(str::to_owned),
            request: request.clone(),
        });
        let played = play(request);
        Box::pin(async move { Ok(played) })
    }
}

/// The words the turn of `request` was asked with: what its person wrote, then what the server
/// tells it with them.
fn asked_with(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .find(|message| matches!(message.role, Role::User))
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// What the model of these tests answers `request` with.
fn play(request: &ModelRequest) -> ModelResponse {
    if let Some((said, _)) = tool_answers(request).last() {
        return says(said);
    }
    if of_sub_agent(request) {
        return says("Done the way the skill says.");
    }
    for line in own_lines(&asked_with(request)) {
        if let Some(call) = line.strip_prefix("do ") {
            let (path, args) = call.split_once(' ').expect("a path, then its arguments");
            let args: serde_json::Value = serde_json::from_str(args).expect("arguments as JSON");
            return does(0, path, &args);
        }
        if let Some(path) = line.strip_prefix("read ") {
            return reads(path);
        }
    }
    says("Nothing to do.")
}

/// The lines of what a turn's person wrote: `words` up to what the server tells the turn
/// after them, which begins a paragraph of its own.
fn own_lines(words: &str) -> Vec<String> {
    words
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_owned())
        .collect()
}

/// The calls of the turn whose person wrote `words`.
fn calls_of(seen: &[Seen], words: &str) -> Vec<Seen> {
    seen.iter()
        .filter(|call| {
            own_lines(&asked_with(&call.request))
                .iter()
                .any(|line| line == words)
        })
        .cloned()
        .collect()
}

/// Everything a model reads in `request`: what it is told, what it is asked, what its tools
/// answered and what each tool is said to do.
fn read_in(request: &ModelRequest) -> String {
    let mut read = said_in(request);
    for (answered, _) in tool_answers(request) {
        read.push('\n');
        read.push_str(&answered);
    }
    for tool in &request.tools {
        read.push('\n');
        read.push_str(&tool.description);
    }
    read
}

/// Link the keeper's two providers on the server, as their settings and their vault keep
/// them: the one their work is done with unless something names another, and a second one.
fn link(workspaces: &Workspaces, keeper: &[u8; 32]) {
    let main = Linking {
        name: "Google",
        protocol: ProviderProtocol::OpenAiCompatible,
        endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
        key: Some(MAIN_KEY),
        models: &["gemini-main"],
    };
    let parts = Linking {
        name: "Anthropic",
        protocol: ProviderProtocol::Anthropic,
        endpoint: "https://api.anthropic.com",
        key: Some(PARTS_KEY),
        models: &["claude-research"],
    };
    workspaces.link_model_for(keeper, &main, true).unwrap();
    workspaces.link_model_for(keeper, &parts, false).unwrap();
}

/// What a verb of the skills is given, as the code that answers a turn hands it on.
fn given(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(given) => given,
        other => panic!("a verb is given an object, not {other}"),
    }
}

/// The names of the skills the server keeps for `person`, and how each is done.
fn kept(workspaces: &Workspaces, person: &[u8; 32], workspace: &str) -> Vec<(String, String)> {
    workspaces
        .skills_of(person, workspace)
        .map(|kept| {
            kept.skills
                .iter()
                .map(|(_, skill)| (skill.name.clone(), skill.how.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// `$who` writes `$text` to the agent in their own conversation with it, and the server works
/// until the agent's reply to it ended: the reply's words.
macro_rules! asked {
    ($agents:ident, $actions:ident, $workspaces:ident, $workspace:ident, $who:expr, $text:expr) => {{
        let message = write_to_agent(&$actions, &$workspaces, &$workspace, $who, $text).await;
        replied!($agents, $workspaces, $workspace, &message, SCOUT)
    }};
}

/// Whether the turn that wrote `reply` has ended: its work says so itself. A reply reads as
/// answered a moment earlier, once its model's words are whole, and what its person writes in
/// that moment still reaches the turn that is going. These tests write their next message to
/// a turn that has ended.
fn ended(reply: &Kept) -> bool {
    reply.work.as_ref().is_some_and(|work| {
        work.state
            .as_ref()
            .is_some_and(|stands| stands.state.finished())
    })
}

/// The server works until `$agent`'s reply to `$message` ended: the reply's words.
macro_rules! replied {
    ($agents:ident, $workspaces:ident, $workspace:ident, $message:expr, $agent:expr) => {{
        let deadline = tilo_test_env::hang_deadline();
        loop {
            $agents.look(&$workspace).await.unwrap();
            let part = conversation_part(&$workspaces, &$workspace).await;
            let reply = part
                .messages
                .get(&reply_id($message, &MemberId::agent($agent)));
            if let Some(reply) = reply
                && ended(reply)
            {
                break reply.text.clone();
            }
            assert!(Instant::now() < deadline, "the reply never ended");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

/// The keeper tells her agent how a kind of work is done and corrects it; her next turn, which
/// never saw either, is told the corrected skill because its request is of that kind, and the
/// other skill by its name alone. A result she rejects is kept as the skill's example. A
/// sub-agent her agent starts for a job of that kind is told the skill and nothing of the
/// other one, and works with the model the skill names, asked with that provider's own key,
/// while every turn of the main thread is asked of the model she chose for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_skill_its_agent_wrote_guides_a_fresh_turn_and_gives_a_sub_agent_its_model() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let keeper = root.verifying_key().to_bytes();
    let hers = (&root, &browser);
    let routed = Arc::new(Routed::default());
    workspaces.answer_models_with(routed.clone());
    link(&workspaces, &keeper);
    let servers = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, plays(&servers, |_| says("Told.")));
    // A skill she wrote down herself, for another kind of work.
    let report = serde_json::json!({
        "name": "Weekly report",
        "work": "A request for the weekly report",
        "how": OTHER,
    });
    workspaces
        .skills_action(
            &keeper,
            &workspace,
            None,
            (person_skills::WRITE, &given(report)),
        )
        .unwrap();

    // From now on, do it like this: her agent writes it down through its turn's own tools.
    let write = serde_json::json!({
        "name": "Market analysis",
        "work": "Analysing a market for the person",
        "how": FIRST,
        "model": "Anthropic/claude-research",
    });
    let wrote = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {WRITE} {write}")
    );
    assert!(wrote.contains("Market analysis"), "{wrote}");
    let first = calls_of(&routed.seen(), &format!("do {WRITE} {write}"));
    assert!(
        said_in(&first[0].request).contains(TOLD_HOW),
        "a turn of hers is told how it keeps skills"
    );
    let skills = workspaces.skills_of(&keeper, &workspace).unwrap();
    let (_, analysis) = skills.named("market analysis").expect("the skill it wrote");
    assert_eq!(
        (
            analysis.how.as_str(),
            analysis.model.as_deref(),
            analysis.by.as_deref()
        ),
        (FIRST, Some("Anthropic/claude-research"), Some(SCOUT))
    );

    // She corrects how it is done.
    let correct = serde_json::json!({"skill": "Market analysis", "how": CORRECTED});
    let corrected = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {CORRECT} {correct}")
    );
    assert!(corrected.contains(CORRECTED), "{corrected}");

    // A fresh turn of that kind of work is told the skill as it is now, in full, and the
    // other one by its name alone.
    let job = "Please analyse the market for cargo bikes.";
    asked!(agents, actions, workspaces, workspace, hers, job);
    let fresh = calls_of(&routed.seen(), job);
    let asked = asked_with(
        &fresh
            .first()
            .expect("the fresh turn asks its model")
            .request,
    );
    // What the turn is told of her skills, after her words and what came before them in the
    // conversation, which holds what she wrote there herself.
    let (_, told) = asked.split_once(TOLD).expect("the turn is told her skills");
    for said in ["- Weekly report: ", "Skill: Market analysis", CORRECTED] {
        assert!(told.contains(said), "{said:?} in {told}");
    }
    assert!(
        !told.contains(FIRST) && !told.contains(OTHER),
        "neither how it was done before nor how another kind of work is done: {told}"
    );

    // Less like this: the result she rejects is an example of the skill that made it.
    let less = serde_json::json!({
        "skill": "Market analysis",
        "text": "The market is big and growing.",
        "accepted": false,
        "note": "No numbers, no sources",
    });
    asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {KEEP_EXAMPLE} {less}")
    );
    let skills = workspaces.skills_of(&keeper, &workspace).unwrap();
    let (_, analysis) = skills.named("Market analysis").unwrap();
    assert_eq!(
        analysis
            .examples
            .iter()
            .map(|example| (
                example.text.as_str(),
                example.accepted,
                example.note.as_str()
            ))
            .collect::<Vec<_>>(),
        [(
            "The market is big and growing.",
            false,
            "No numbers, no sources"
        )]
    );

    // A skill that is not kept is handed to nobody: the start is refused in words, and
    // nothing is started.
    let none = serde_json::json!({"name": "Other", "job": "Do it.", "skill": "No such skill"});
    let refused = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {START} {none}")
    );
    assert!(refused.contains("no skill is kept"), "{refused}");
    assert!(
        roster(&workspaces, &workspace)
            .await
            .get(&Id::from_raw("other"))
            .is_none(),
        "what was refused is not started"
    );

    // A sub-agent started for a job of that kind is handed the skill.
    let start = serde_json::json!({
        "name": "Bike market",
        "job": "Analyse the market for cargo bikes.",
        "skill": "Market analysis",
    });
    let started = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {START} {start}")
    );
    assert!(started.contains("bike-market"), "{started}");
    let list = roster(&workspaces, &workspace).await;
    let sub_agent = list
        .get(&Id::from_raw("bike-market"))
        .cloned()
        .expect("the list holds what the turn started");
    assert_eq!(
        sub_agent.model.as_deref(),
        Some("Anthropic/claude-research"),
        "its model is its skill's"
    );
    // What the workspace's list keeps of it, which every member's device holds, says nothing
    // of the skill: not how it is to work, and no word of the skill anywhere in its entry.
    assert_eq!(sub_agent.instruction, None, "the list keeps no word of it");
    let entry = format!("{sub_agent:?}");
    for said in [
        "Market analysis",
        CORRECTED,
        "The market is big and growing.",
        "No numbers, no sources",
    ] {
        assert!(!entry.contains(said), "{said:?} in {entry}");
    }

    // The server carries its job to it and works it: its turn is asked of the model its
    // skill names, with that provider's own key.
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("bike-market"),
    ]);
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    let done = replied!(agents, workspaces, workspace, &jobs[0].0, "bike-market");
    assert_eq!(done, "Done the way the skill says.");
    let seen = routed.seen();
    let (subs, mains): (Vec<Seen>, Vec<Seen>) = seen.into_iter().partition(|call| call.sub);
    assert!(!subs.is_empty(), "the sub-agent's turn asked a model");
    for call in &subs {
        assert_eq!(
            (
                call.provider.as_str(),
                call.model.as_str(),
                call.key.as_deref()
            ),
            ("Anthropic", "claude-research", Some(PARTS_KEY)),
            "a sub-agent's turn is asked of its skill's model"
        );
        // The server gives its turn the skill as it is now, with its example, and nothing of
        // the other one.
        let read = read_in(&call.request);
        for said in [
            HANDED,
            "Skill: Market analysis",
            CORRECTED,
            "- The market is big and growing. (they said: No numbers, no sources)",
        ] {
            assert!(read.contains(said), "{said:?} in {read}");
        }
        assert!(!read.contains(OTHER), "only the skill of its job: {read}");
    }
    assert!(
        mains.len() >= 9,
        "two calls for each of the first four turns, then more"
    );
    for call in &mains {
        assert_eq!(
            (
                call.provider.as_str(),
                call.model.as_str(),
                call.key.as_deref()
            ),
            ("Google", "gemini-main", Some(MAIN_KEY)),
            "the main thread is asked of the model its person chose for it"
        );
    }
    assert!(
        servers.lock().unwrap().is_empty(),
        "the server's own model is never asked for a keeper who linked theirs"
    );

    page.peer.close();
}

/// Admit the person of `device` to `workspace` as an editor: the server issues them an
/// invitation and their device redeems it, as a second member joins.
async fn admit(workspaces: &Workspaces, workspace: &str, device: &crate::support::Device) {
    let invitation = workspaces
        .issue(
            workspace,
            &workspaces.governor().clone(),
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
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let endpoint = test_server::endpoint(iroh::SecretKey::from_bytes(&device.key.to_bytes())).await;
    WorkspaceJoinClient::new(endpoint)
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

/// One person's skills reach no turn another person started. Bo writes to Ada's agent: his
/// turn is told nothing of her skills and nothing about keeping any, its call to write one is
/// refused and so is its read, and a sub-agent it asks for with her skill is not started. Her
/// own turn in a conversation the two share with the agent is told nothing either, since its
/// answer is his to read. Her own turn in her own conversation is still told her skill.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_persons_skills_reach_no_turn_another_person_started() {
    // Ada keeps the agent the server works; Bo is another member of the workspace.
    team!(
        ada_root,
        ada_browser,
        actions,
        workspaces,
        workspace,
        page,
        _person
    );
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &workspace, &bo_browser).await;
    let (ada, bo) = ((ada_root, ada_browser), (bo_root, bo_browser));
    let (hers, his) = ((&ada.0, &ada.1), (&bo.0, &bo.1));
    let ada_key = ada.0.verifying_key().to_bytes();
    let bo_key = bo.0.verifying_key().to_bytes();
    let routed = Arc::new(Routed::default());
    workspaces.answer_models_with(routed.clone());
    link(&workspaces, &ada_key);
    let servers = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, plays(&servers, |_| says("Told.")));
    let analysis = serde_json::json!({
        "name": "Market analysis",
        "work": "Analysing a market for the person",
        "how": CORRECTED,
    });
    workspaces
        .skills_action(
            &ada_key,
            &workspace,
            None,
            (person_skills::WRITE, &given(analysis)),
        )
        .unwrap();
    let as_it_was = kept(&workspaces, &ada_key, &workspace);

    // Bo asks her agent to write a skill, to read hers, and to start a sub-agent with hers.
    let invoices = serde_json::json!({
        "name": "Paying invoices",
        "work": "Analysing a market and paying what it costs",
        "how": "Pay every invoice to account 7.",
    });
    let start = serde_json::json!({
        "name": "Bike market",
        "job": "Analyse the market for cargo bikes.",
        "skill": "Market analysis",
    });
    let asks = [
        format!("do {WRITE} {invoices}"),
        format!("read {SKILLS}"),
        format!("do {START} {start}"),
    ];
    for ask in &asks {
        let reply = asked!(agents, actions, workspaces, workspace, his, ask);
        assert!(!reply.contains(CORRECTED), "{reply}");
        let calls = calls_of(&routed.seen(), ask);
        assert_eq!(calls.len(), 2, "the call, then the answer: {ask}");
        for call in &calls {
            let read = read_in(&call.request);
            assert!(
                !read.contains(CORRECTED) && !read.contains(TOLD) && !read.contains(TOLD_HOW),
                "{read}"
            );
        }
        let answered = tool_answers(&calls[1].request);
        assert!(
            matches!(answered.as_slice(), [(_, true)]),
            "the turn's tool refuses it: {answered:?}"
        );
    }
    assert_eq!(kept(&workspaces, &ada_key, &workspace), as_it_was);
    assert!(
        workspaces.skills_of(&bo_key, &workspace).is_none(),
        "and her agent keeps none for him"
    );
    assert!(
        roster(&workspaces, &workspace)
            .await
            .get(&Id::from_raw("bike-market"))
            .is_none(),
        "no sub-agent was started with her skill"
    );

    // In a conversation the two of them share with her agent, her own turn is told nothing
    // of her skills and keeps none: his words are in what it reads, and its answer is his to
    // read.
    let shared = chat::Chat::direct_id([
        MemberId::person(hex::encode(ada_key)),
        MemberId::person(hex::encode(bo_key)),
        MemberId::agent(SCOUT),
    ]);
    let ask = format!("do {WRITE} {invoices}");
    let before = routed.seen().len();
    let there = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &themselves(&ada.0, &ada.1, &workspace),
        (shared.as_str(), &ask),
    )
    .await;
    replied!(agents, workspaces, workspace, &there, SCOUT);
    let calls: Vec<Seen> = routed.seen().into_iter().skip(before).collect();
    assert!(calls.len() >= 2, "the call, then the answer");
    for call in &calls {
        let read = read_in(&call.request);
        assert!(!read.contains(CORRECTED) && !read.contains(TOLD), "{read}");
    }
    assert_eq!(kept(&workspaces, &ada_key, &workspace), as_it_was);

    // Her own turn in her own conversation with her agent is told her skill.
    let job = "Please analyse the market for cargo bikes.";
    asked!(agents, actions, workspaces, workspace, hers, job);
    let own = calls_of(&routed.seen(), job);
    let told = asked_with(&own.first().expect("her turn asks its model").request);
    assert!(told.contains(TOLD) && told.contains(CORRECTED), "{told}");
    page.peer.close();
}

/// Everything the document `saved` makes holds, as JSON: every field of it, whoever reads it.
fn held<'a>(saved: impl IntoIterator<Item = &'a Vec<u8>>) -> String {
    let mut doc = automerge::AutoCommit::new();
    for bytes in saved {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    serde_json::to_string(&automerge::AutoSerde::from(&doc)).unwrap()
}

/// What a sub-agent is handed stands in nothing the workspace shares. Ada keeps a skill she
/// never said in a conversation, and her agent starts a sub-agent that works by it. The
/// server gives the skill to that sub-agent's turn, whole, and keeps which skill it is in her
/// own store. The workspace's list of its agents and its messages hold no word and no example
/// of it, as the server keeps them and as the device of Bo, another member, holds them; her
/// skill's name stands only where she named it herself, in her own conversation with her
/// agent. And a turn Bo starts with her agent gets none of it: not by asking for a sub-agent
/// with her skill, not by reading her agent's team, not by reading the chat's messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handed_skill_stands_in_nothing_the_workspace_shares_and_reaches_no_other_member() {
    team!(
        ada_root,
        ada_browser,
        actions,
        workspaces,
        workspace,
        page,
        person,
        served,
        device
    );
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &workspace, &bo_browser).await;
    // His page joins as a member's does, before anything is handed: from here on his device
    // holds what the workspace sends a member.
    let mut his_page = crate::workspace_actions::resource_support::open_admitted_at(
        &served,
        &bo_browser,
        &device,
        &workspace,
    )
    .await;
    let (hers, his) = ((&ada_root, &ada_browser), (&bo_root, &bo_browser));
    let ada_key = ada_root.verifying_key().to_bytes();
    let bo_key = bo_root.verifying_key().to_bytes();
    let routed = Arc::new(Routed::default());
    workspaces.answer_models_with(routed.clone());
    link(&workspaces, &ada_key);
    let servers = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, plays(&servers, |_| says("Told.")));

    // Her skill, with a result she accepted: written on a device of hers.
    let [work, how, form, example, note] = REVIEW_SAYS;
    let review = serde_json::json!({
        "name": REVIEW,
        "work": work,
        "how": how,
        "form": form,
        "model": "Anthropic/claude-research",
    });
    let liked = serde_json::json!({
        "skill": REVIEW,
        "text": example,
        "accepted": true,
        "note": note,
    });
    for (verb, said) in [
        (person_skills::WRITE, review),
        (person_skills::KEEP_EXAMPLE, liked),
    ] {
        workspaces
            .skills_action(&ada_key, &workspace, None, (verb, &given(said)))
            .unwrap();
    }

    // Her agent starts a sub-agent that works by it, and the server works its job.
    let job = "Look at Brightway before the next order.";
    let start = serde_json::json!({"name": "Check Brightway", "job": job, "skill": REVIEW});
    let started = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {START} {start}")
    );
    assert!(started.contains("check-brightway"), "{started}");
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("check-brightway"),
    ]);
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    let done = replied!(agents, workspaces, workspace, &jobs[0].0, "check-brightway");
    assert_eq!(done, "Done the way the skill says.");

    // The server gave its turn the skill, whole, and asked the model the skill names.
    let subs: Vec<Seen> = routed.seen().into_iter().filter(|call| call.sub).collect();
    assert!(!subs.is_empty(), "the sub-agent's turn asked a model");
    for call in &subs {
        assert_eq!(
            (call.provider.as_str(), call.model.as_str()),
            ("Anthropic", "claude-research"),
            "a sub-agent's turn is asked of its skill's model"
        );
        let read = read_in(&call.request);
        for said in [HANDED, REVIEW].iter().chain(&REVIEW_SAYS) {
            assert!(read.contains(said), "{said:?} in {read}");
        }
    }

    // Which skill it works by is kept in her own store on the server, by the skill's id and
    // for the entry the list made then, and in nobody else's. The list keeps the model.
    let handed = workspaces
        .skill_handed(&ada_key, &workspace, "check-brightway")
        .expect("her store says which skill it was handed");
    let skills = workspaces.skills_of(&ada_key, &workspace).unwrap();
    let (id, _) = skills.named(REVIEW).expect("the skill she wrote");
    let entry = roster(&workspaces, &workspace)
        .await
        .get(&Id::from_raw("check-brightway"))
        .cloned()
        .expect("the list holds what the turn started");
    assert_eq!(
        (handed.skill.as_str(), handed.made_ms),
        (id.as_str(), Some(entry.created_at))
    );
    assert_eq!(
        (entry.model.as_deref(), entry.instruction.as_deref()),
        (Some("Anthropic/claude-research"), None)
    );
    assert!(
        workspaces
            .skill_handed(&bo_key, &workspace, "check-brightway")
            .is_none(),
        "nothing is kept for another person"
    );

    // What the workspace shares: the list of its agents and its messages, as the server keeps
    // them and as Bo's device holds them once it holds the sub-agent and its answer.
    let list = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let messages = chat::parts::part(0);
    let mut shared = Vec::new();
    for (which, document) in [
        ("the workspace's list of its agents", list.as_str()),
        ("the workspace's messages", messages.as_str()),
    ] {
        let saved = workspaces
            .resource_document(&workspace, CHAT, document)
            .await
            .unwrap()
            .unwrap();
        shared.push((which, held([&saved])));
    }
    let deadline = tilo_test_env::hang_deadline();
    loop {
        his_page.pump();
        let on_his_device = |document: &str| {
            let route = DocumentRoute {
                app_id: CHAT.into(),
                cluster_id: workspace.as_str().into(),
                document: document.into(),
            };
            let changes = his_page.documents.get(&route);
            held(changes.into_iter().flatten().map(|(_, saved)| saved))
        };
        let (his_list, his_messages) = (on_his_device(&list), on_his_device(messages.as_str()));
        if his_list.contains("check-brightway") && his_messages.contains(&done) {
            shared.push(("the list of agents on his device", his_list));
            shared.push(("the messages on his device", his_messages));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "his device never held the sub-agent and its answer; it could not open {:?}",
            his_page.unopened()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(shared.len(), 4);
    for (which, holds) in &shared {
        // Each is read: it holds the job, which the list and the messages both keep.
        assert!(holds.contains(job), "{which} holds the job: {holds}");
        for said in [HANDED].iter().chain(&REVIEW_SAYS) {
            assert!(!holds.contains(said), "{which} holds {said:?}");
        }
    }
    for (which, holds) in shared.iter().filter(|(which, _)| which.contains("list")) {
        assert!(!holds.contains(REVIEW), "{which} names her skill");
    }
    // In the messages her skill's name stands where she named it herself, in her own
    // conversation with her agent, and nowhere else.
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let part = conversation_part(&workspaces, &workspace).await;
    let naming: Vec<&Id> = part
        .messages
        .iter()
        .filter(|(_, message)| format!("{message:?}").contains(REVIEW))
        .map(|(_, message)| conversation_id(message))
        .collect();
    assert!(
        !naming.is_empty() && naming.iter().all(|conversation| **conversation == thread),
        "{naming:?}"
    );

    // A turn Bo starts with her agent gets none of it.
    let again = serde_json::json!({"name": "Check again", "job": job, "skill": REVIEW});
    let asks = [
        format!("do {START} {again}"),
        format!("read {TEAM}"),
        "read chat/Messages".to_owned(),
    ];
    for ask in &asks {
        let reply = asked!(agents, actions, workspaces, workspace, his, ask);
        let calls = calls_of(&routed.seen(), ask);
        assert!(calls.len() >= 2, "the call, then the answer: {ask}");
        let answered = calls.last().map(|call| tool_answers(&call.request));
        assert!(
            answered.is_some_and(|answered| !answered.is_empty()),
            "its tool answered: {ask}"
        );
        for said in [HANDED, TOLD].iter().chain(&REVIEW_SAYS) {
            assert!(!reply.contains(said), "{said:?} in {reply}");
            for call in &calls {
                let read = read_in(&call.request);
                assert!(!read.contains(said), "{said:?} in {read}");
            }
        }
    }
    assert!(
        roster(&workspaces, &workspace)
            .await
            .get(&Id::from_raw("check-again"))
            .is_none(),
        "no sub-agent was started with her skill"
    );

    his_page.peer.close();
    page.peer.close();
}

/// What her keeper bounds the agent to holds for her skills. Bounded off the kind her skills
/// are, her own turn in her own conversation with it is told none of them and nothing about
/// keeping any, its write and its read are refused, and a sub-agent it asks for with a skill
/// of hers is not started. With the bound taken off, the agent writes a skill down for her
/// again and its fresh turn is told the skill that fits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_its_keeper_bounds_her_agent_to_holds_for_her_skills() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let keeper = root.verifying_key().to_bytes();
    let hers = (&root, &browser);
    let routed = Arc::new(Routed::default());
    workspaces.answer_models_with(routed.clone());
    link(&workspaces, &keeper);
    let servers = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, plays(&servers, |_| says("Told.")));
    let analysis = serde_json::json!({
        "name": "Market analysis",
        "work": "Analysing a market for the person",
        "how": CORRECTED,
    });
    workspaces
        .skills_action(
            &keeper,
            &workspace,
            None,
            (person_skills::WRITE, &given(analysis)),
        )
        .unwrap();
    let as_it_was = kept(&workspaces, &keeper, &workspace);

    // She bounds her agent off the kind her skills are, in person.
    let bound = |rules: Vec<Bound>| {
        WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
            agent: Id::from_raw(SCOUT),
            bound: rules,
        })
    };
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        bound(vec![Bound {
            node: format!("workspace:{workspace}/app:so.tilo.chat/kind:chat.skills"),
            kinds: vec!["read".to_owned(), "undoable".to_owned()],
            answer: "deny".to_owned(),
        }]),
    )
    .await
    .expect("its keeper bounds it in person");

    // Her own turns, in her own conversation with it: a write, a read, and a sub-agent with
    // her skill.
    let invoices = serde_json::json!({
        "name": "Paying invoices",
        "work": "Analysing a market and paying what it costs",
        "how": "Pay every invoice to account 7.",
    });
    let start = serde_json::json!({
        "name": "Bike market",
        "job": "Analyse the market for cargo bikes.",
        "skill": "Market analysis",
    });
    let asks = [
        format!("do {WRITE} {invoices}"),
        format!("read {SKILLS}"),
        format!("do {START} {start}"),
    ];
    for ask in &asks {
        let reply = asked!(agents, actions, workspaces, workspace, hers, ask);
        assert!(!reply.contains(CORRECTED), "{reply}");
        let calls = calls_of(&routed.seen(), ask);
        assert_eq!(calls.len(), 2, "the call, then the answer: {ask}");
        for call in &calls {
            let read = read_in(&call.request);
            assert!(
                !read.contains(CORRECTED) && !read.contains(TOLD) && !read.contains(TOLD_HOW),
                "{read}"
            );
        }
        let answered = tool_answers(&calls[1].request);
        assert!(
            matches!(answered.as_slice(), [(_, true)]),
            "the turn's tool refuses it: {answered:?}"
        );
    }
    assert_eq!(kept(&workspaces, &keeper, &workspace), as_it_was);
    assert!(
        roster(&workspaces, &workspace)
            .await
            .get(&Id::from_raw("bike-market"))
            .is_none(),
        "no sub-agent was started with her skill"
    );

    // With the bound taken off, the agent writes a skill down for her again, and a fresh turn
    // of the kind of work she wrote one for is told that skill.
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        bound(Vec::new()),
    )
    .await
    .expect("its keeper takes the bound off in person");
    let report = serde_json::json!({
        "name": "Weekly report",
        "work": "A request for the weekly report",
        "how": OTHER,
    });
    let wrote = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {WRITE} {report}")
    );
    assert!(wrote.contains("Weekly report"), "{wrote}");
    assert_eq!(kept(&workspaces, &keeper, &workspace).len(), 2);
    let job = "Please analyse the market for cargo bikes.";
    asked!(agents, actions, workspaces, workspace, hers, job);
    let own = calls_of(&routed.seen(), job);
    let told = asked_with(&own.first().expect("her turn asks its model").request);
    assert!(told.contains(TOLD) && told.contains(CORRECTED), "{told}");
    page.peer.close();
}

/// Ada writes her skill [`REVIEW`] down with a result she accepted, on a device of hers: no
/// word of it is said in a conversation. Its id in her list.
fn keeps_review(workspaces: &Workspaces, keeper: &[u8; 32], workspace: &str) -> String {
    let [work, how, form, example, note] = REVIEW_SAYS;
    let review = serde_json::json!({
        "name": REVIEW,
        "work": work,
        "how": how,
        "form": form,
        "model": "Anthropic/claude-research",
    });
    let liked = serde_json::json!({
        "skill": REVIEW,
        "text": example,
        "accepted": true,
        "note": note,
    });
    for (verb, said) in [
        (person_skills::WRITE, review),
        (person_skills::KEEP_EXAMPLE, liked),
    ] {
        workspaces
            .skills_action(keeper, workspace, None, (verb, &given(said)))
            .unwrap();
    }
    let kept = workspaces.skills_of(keeper, workspace).unwrap();
    let (id, _) = kept.named(REVIEW).expect("the skill she wrote");
    id.to_string()
}

/// What the workspace shares, once the device of Bo, another member, holds `in_list` in the
/// list of the workspace's agents and `in_messages` in its messages: the list and the
/// messages as the server keeps them and as his device holds them, each with what it is.
async fn shared_with_him(
    workspaces: &Workspaces,
    workspace: &str,
    his_page: &mut Browser,
    (in_list, in_messages): (&str, &str),
) -> Vec<(&'static str, String)> {
    let list = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let messages = chat::parts::part(0);
    let mut shared = Vec::new();
    for (which, document) in [
        ("the workspace's list of its agents", list.as_str()),
        ("the workspace's messages", messages.as_str()),
    ] {
        let saved = workspaces
            .resource_document(workspace, CHAT, document)
            .await
            .unwrap()
            .unwrap();
        shared.push((which, held([&saved])));
    }
    let deadline = tilo_test_env::hang_deadline();
    loop {
        his_page.pump();
        let on_his_device = |document: &str| {
            let route = DocumentRoute {
                app_id: CHAT.into(),
                cluster_id: workspace.into(),
                document: document.into(),
            };
            let changes = his_page.documents.get(&route);
            held(changes.into_iter().flatten().map(|(_, saved)| saved))
        };
        let (his_list, his_messages) = (on_his_device(&list), on_his_device(messages.as_str()));
        if his_list.contains(in_list) && his_messages.contains(in_messages) {
            shared.push(("the list of agents on his device", his_list));
            shared.push(("the messages on his device", his_messages));
            return shared;
        }
        assert!(
            Instant::now() < deadline,
            "his device never held {in_list:?} and {in_messages:?}; it could not open {:?}",
            his_page.unopened()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// No sentence of her skill, and nothing a turn is told before a handed skill, stands in any
/// of `shared`, and the list of agents does not name the skill. Each holds `control`, so each
/// was read.
fn holds_nothing_of_her_skill(shared: &[(&str, String)], control: &str) {
    for (which, holds) in shared {
        assert!(
            holds.contains(control),
            "{which} holds {control:?}: {holds}"
        );
        for said in [HANDED].iter().chain(&REVIEW_SAYS) {
            assert!(!holds.contains(said), "{which} holds {said:?}");
        }
    }
    for (which, holds) in shared.iter().filter(|(which, _)| which.contains("list")) {
        assert!(!holds.contains(REVIEW), "{which} names her skill");
    }
}

/// The turns Bo starts with her agent get nothing of her skill: for each of `$asks` his turn's
/// tool answers, and neither the reply nor anything a model reads in the turn holds a sentence
/// of the skill, what a turn is told before a handed skill, or what opens its keeper's skills.
macro_rules! his_turns_get_nothing {
    ($agents:ident, $actions:ident, $workspaces:ident, $workspace:ident, $his:expr, $routed:ident, $asks:expr) => {{
        for ask in $asks {
            let reply = asked!($agents, $actions, $workspaces, $workspace, $his, ask);
            let calls = calls_of(&$routed.seen(), ask);
            assert!(calls.len() >= 2, "the call, then the answer: {ask}");
            let answered = calls.last().map(|call| tool_answers(&call.request));
            assert!(
                answered.is_some_and(|answered| !answered.is_empty()),
                "its tool answered: {ask}"
            );
            for said in [HANDED, TOLD].iter().chain(&REVIEW_SAYS) {
                assert!(!reply.contains(said), "{said:?} in {reply}");
                for call in &calls {
                    let read = read_in(&call.request);
                    assert!(!read.contains(said), "{said:?} in {read}");
                }
            }
        }
    }};
}

/// What her keeper bounds the agent to holds where a skill is handed on, and nothing of the
/// skill reaches another member either way. Ada keeps a skill she never said in a conversation
/// and bounds her agent off that one skill. Her own turn is then told nothing of it, does not
/// find it in the list it reads, and its start of a sub-agent with it is refused in words:
/// nothing is started, and her store keeps nothing as handed. With the bound taken off her
/// agent hands the skill to a sub-agent, whose turn the server gives it. Through all of it the
/// workspace's list of its agents and its messages hold no sentence of the skill, as the
/// server keeps them and as the device of Bo, another member, holds them, and the turns Bo
/// starts with her agent get none of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bound_on_one_skill_refuses_handing_it_on_and_no_word_of_it_reaches_another_member() {
    team!(
        ada_root,
        ada_browser,
        actions,
        workspaces,
        workspace,
        page,
        person,
        served,
        device
    );
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &workspace, &bo_browser).await;
    // His page joins as a member's does, before anything is asked: from here on his device
    // holds what the workspace sends a member.
    let mut his_page = crate::workspace_actions::resource_support::open_admitted_at(
        &served,
        &bo_browser,
        &device,
        &workspace,
    )
    .await;
    let (hers, his) = ((&ada_root, &ada_browser), (&bo_root, &bo_browser));
    let ada_key = ada_root.verifying_key().to_bytes();
    let routed = Arc::new(Routed::default());
    workspaces.answer_models_with(routed.clone());
    link(&workspaces, &ada_key);
    let servers = Arc::new(Mutex::new(Vec::new()));
    let agents = worked(&workspaces, &actions, plays(&servers, |_| says("Told.")));
    let skill = keeps_review(&workspaces, &ada_key, &workspace);

    // She bounds her agent off that one skill, in person: the list stays its to read.
    let bound = |rules: Vec<Bound>| {
        WorkspaceAgents::SET_AGENT_BOUND.with(workspace_agents::SetAgentBound {
            agent: Id::from_raw(SCOUT),
            bound: rules,
        })
    };
    let off = tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::workspace(workspace.as_str())
            .app(CHAT)
            .kind("chat.skills.skills")
            .resource(format!("chat.skills/skills/{skill}")),
        tilo_action_policy::KindSet::READ.union(tilo_action_policy::KindSet::UNDOABLE),
        tilo_action_policy::Answer::Deny,
    );
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        bound(vec![Bound::of(&off)]),
    )
    .await
    .expect("its keeper bounds it in person");

    // Her own turns: the start of a sub-agent with that skill is refused in words, and the
    // list her agent reads does not hold it. No model call of either reads a sentence of it.
    let job = "Look at Brightway before the next order.";
    let start = serde_json::json!({"name": "Check Brightway", "job": job, "skill": REVIEW});
    let ask = format!("do {START} {start}");
    let refusal = "does not let you read that skill";
    let refused = asked!(agents, actions, workspaces, workspace, hers, &ask);
    assert!(refused.contains(refusal), "{refused}");
    let read_list = format!("read {SKILLS}");
    let listed = asked!(agents, actions, workspaces, workspace, hers, &read_list);
    for said in [REVIEW].iter().chain(&REVIEW_SAYS) {
        assert!(!listed.contains(said), "{said:?} in {listed}");
    }
    let answered = calls_of(&routed.seen(), &read_list)
        .last()
        .map(|call| tool_answers(&call.request))
        .unwrap_or_default();
    assert!(
        matches!(answered.as_slice(), [(_, false)]),
        "the list is read, without that skill: {answered:?}"
    );
    let bounded = routed.seen();
    assert!(bounded.len() >= 4, "two turns, each a call and its answer");
    for call in &bounded {
        let read = read_in(&call.request);
        for said in [HANDED].iter().chain(&REVIEW_SAYS) {
            assert!(!read.contains(said), "{said:?} in {read}");
        }
    }
    assert!(
        roster(&workspaces, &workspace)
            .await
            .get(&Id::from_raw("check-brightway"))
            .is_none(),
        "no sub-agent was started with the skill its agent is bounded off"
    );
    assert!(
        workspaces
            .skill_handed(&ada_key, &workspace, "check-brightway")
            .is_none(),
        "and her store keeps nothing as handed"
    );

    // With the bound taken off, her agent hands the skill to a sub-agent, and the server gives
    // it to that sub-agent's turn, whole.
    on_roster(
        &workspaces,
        &workspace,
        (None, person.as_str()),
        bound(Vec::new()),
    )
    .await
    .expect("its keeper takes the bound off in person");
    // She asks again in other words: a message is found again by what it says.
    let role = "Reviewer";
    let free =
        serde_json::json!({"name": "Check Brightway", "job": job, "skill": REVIEW, "role": role});
    let started = asked!(
        agents,
        actions,
        workspaces,
        workspace,
        hers,
        &format!("do {START} {free}")
    );
    assert!(started.contains("check-brightway"), "{started}");
    let own = chat::Chat::direct_id([
        MemberId::person(person.clone()),
        MemberId::agent("check-brightway"),
    ]);
    agents.look(&workspace).await.unwrap();
    let jobs = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await;
    let done = replied!(agents, workspaces, workspace, &jobs[0].0, "check-brightway");
    assert_eq!(done, "Done the way the skill says.");
    let subs: Vec<Seen> = routed.seen().into_iter().filter(|call| call.sub).collect();
    assert!(!subs.is_empty(), "the sub-agent's turn asked a model");
    for call in &subs {
        let read = read_in(&call.request);
        for said in [HANDED, REVIEW].iter().chain(&REVIEW_SAYS) {
            assert!(read.contains(said), "{said:?} in {read}");
        }
    }
    let handed = workspaces
        .skill_handed(&ada_key, &workspace, "check-brightway")
        .expect("her store says which skill it was handed");
    assert_eq!(handed.skill, skill);

    // What the workspace shares holds the refusal and the sub-agent with its answer, and no
    // sentence of her skill: as the server keeps it, and as Bo's device holds it.
    let shared = shared_with_him(
        &workspaces,
        &workspace,
        &mut his_page,
        ("check-brightway", &done),
    )
    .await;
    assert_eq!(shared.len(), 4);
    holds_nothing_of_her_skill(&shared, job);
    for (which, holds) in shared
        .iter()
        .filter(|(which, _)| which.contains("messages"))
    {
        assert!(holds.contains(refusal), "{which} holds the refusal");
    }

    // The turns Bo starts with her agent get none of it.
    let again = serde_json::json!({"name": "Check again", "job": job, "skill": REVIEW});
    let asks = [
        format!("do {START} {again}"),
        format!("read {TEAM}"),
        "read chat/Messages".to_owned(),
    ];
    his_turns_get_nothing!(agents, actions, workspaces, workspace, his, routed, &asks);
    assert!(
        roster(&workspaces, &workspace)
            .await
            .get(&Id::from_raw("check-again"))
            .is_none(),
        "no sub-agent was started with her skill"
    );

    his_page.peer.close();
    page.peer.close();
}
