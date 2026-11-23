//! A person mentions the workspace's hosted agent in a Chat message, or writes to it in their
//! direct conversation, and the agent answers under the message: the message is an ask of the
//! agent package, the server claims its turn, works it with its model and writes the reply
//! through the package's own actions, as the agent. No device of the person's has to be on, and
//! nothing of Chat's own fields is read.
//!
//! The reply grows while its model writes it, and what the agent learns about the person who
//! asked is told and kept only where what it was asked about is theirs and the agent's alone.
use super::super::hosted_documents::{content_in, put};
use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_mcp::McpServer;
use tilo_peer_state::join::decode_and_verify_invitation;
use tilo_server::workspace_join::WorkspaceJoinClient;
use tilo_server::workspaces::person_model::{
    Events, ModelProvider, ProviderCall, ProviderFuture, ProviderProtocol, protocol::StreamEvent,
    scripted::Linking,
};
use tilo_server::workspaces::{IssueInvitationRequest, Workspaces};
use tilo_session_actions::SessionActions;
use tilo_workspace_service::agents::{Ask, ServerAgents, Turn};
use tilo_workspace_service::asks::{self, AskState};
use tilo_workspace_service::tl_records::runtime::Value;
use tilo_workspace_service::tl_records::runtime::rich::RichValue;

/// A hosted workspace where Ada keeps the agent Scout on the workspace's server, and Scout is a
/// member acting for her, as making an agent makes it. Ada's page joins once; no device of hers
/// answers.
macro_rules! ada_and_scout {
    ($root:ident, $server:ident, $served:ident, $workspaces:ident, $workspace:ident, $page:ident, $person:ident) => {
        let ($root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let $server = enrol(&$root, "tilo-sh", DeviceCapabilities::member());
        let browser = enrol(&$root, "browser", DeviceCapabilities::member());
        let state = crate::support::scratch_dir();
        let $served = crate::harness::hosted::start(
            &$root,
            &$server,
            state.path(),
            crate::harness::hosted::Options::offering(crate::common::catalogue),
        )
        .await;
        let $workspaces = $served.workspaces().unwrap().clone();
        let $workspace = $workspaces.create("Team").await.unwrap().workspace;
        let $page = Browser::open_at(&$served, &browser, &$server, &$workspace).await;
        let $person = hex::encode($root.verifying_key().to_bytes());
        make_agent(&$page, &$workspaces, &$workspace, &$person, None).await;
    };
}

/// What the agent package holds of the asks in Chat's record now, as `person`'s devices read
/// it.
async fn asked(workspaces: &Workspaces, workspace: &str, person: &str) -> Vec<asks::Asked> {
    let packages = asks::chat_packages().unwrap();
    workspaces
        .with_tl_record(
            workspace,
            &packages,
            asks::Actor::Member(MemberId::person(person.to_owned())),
            |opened| asks::asks(opened),
        )
        .await
        .unwrap()
}

/// `person` opens a channel in Chat and writes `then` there after a mention of Scout. Their
/// device's host makes the ask from the mention as it takes the message in, as theirs.
async fn mention_in_channel(workspaces: &Workspaces, workspace: &str, person: &str, then: &str) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let then = then.to_owned();
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let channel = opened.invoke(
                &who,
                (&chat, "new_channel"),
                vec![Value::resource(&all), Value::Text("plans".into())],
                None,
            )?;
            opened
                .invoke(
                    &who,
                    (&chat, "send"),
                    vec![
                        channel,
                        asks::mentioning(&MemberId::agent(SCOUT), "Scout", &then),
                        Value::Opt(None),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// A file a test message sends: what it is called, what it is, how many bytes, and the hash of
/// its content in the workspace.
struct Sent {
    name: String,
    media: String,
    size: i64,
    hash: String,
}

impl Sent {
    /// The file's value, as the window hands a chosen file's over.
    fn value(&self) -> Value {
        Value::File(std::rc::Rc::new(
            tilo_workspace_service::tl_records::runtime::blob::FileValue::new(
                self.name.clone(),
                self.media.clone(),
                self.size,
                self.hash.clone(),
            ),
        ))
    }
}

/// `person` writes `text` to Scout in their direct conversation with it: Chat's `send` asks
/// it, made from the message.
async fn write_in_direct(workspaces: &Workspaces, workspace: &str, person: &str, text: &str) {
    write_in_direct_with(workspaces, workspace, person, text, Vec::new()).await;
}

/// [`write_in_direct`], sending `files` with the message.
async fn write_in_direct_with(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    text: &str,
    files: Vec<Sent>,
) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let text = text.to_owned();
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let direct = opened.invoke(
                &who,
                (&chat, "message_person"),
                vec![
                    Value::resource(&all),
                    Value::Actor(asks::Actor::Member(MemberId::agent(SCOUT))),
                ],
                None,
            )?;
            let attachments = files.iter().map(Sent::value).collect();
            opened
                .invoke(
                    &who,
                    (&chat, "send"),
                    vec![
                        direct,
                        Value::Rich(RichValue::from_text(&text)),
                        Value::Opt(None),
                        Value::List(attachments),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// Wait until `ready` holds.
async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !ready() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The newest ask's reply once its body reads `words`.
async fn reply_reads(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    words: &str,
) -> asks::Replied {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = asked(workspaces, workspace, person).await;
        let reply = now.last().and_then(asks::Asked::latest).cloned();
        if let Some(reply) = &reply
            && reply.body == words
        {
            return reply.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the reply never read {words:?}: it reads {:?}",
            reply.map(|reply| reply.body)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The one turn the workspace owes the agents, answered: the turn as the server read it.
async fn answered_one(agents: &ServerAgents, workspace: &str) -> Turn {
    let turns = agents.turns(workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "one turn is owed: {turns:?}");
    tokio::time::timeout(Duration::from_secs(30), agents.answer(workspace, &turns[0]))
        .await
        .expect("the turn ends")
        .unwrap();
    turns[0].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_agent_answers_the_message_that_mentions_it() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);
    mention_in_channel(&workspaces, &workspace, &person, " what is planned?").await;
    let waiting = asked(&workspaces, &workspace, &person).await;
    assert_eq!(waiting.len(), 1, "the mention asked once: {waiting:?}");
    assert_eq!(waiting[0].agent, MemberId::agent(SCOUT));
    assert_eq!(waiting[0].state, AskState::Waiting);

    // The server reads the ask as a turn of the agent's, for Ada, with what she wrote.
    let model_asked = Arc::new(Mutex::new(Vec::new()));
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(looks_then_answers(1_000, Arc::clone(&model_asked)));
    let turn = answered_one(&agents, &workspace).await;
    assert!(matches!(turn.ask, Ask::Asked { .. }), "{:?}", turn.ask);
    assert_eq!(turn.person, person, "it works for whoever asked");
    assert!(
        turn.request.contains("what is planned?"),
        "the turn asks what the person wrote: {}",
        turn.request
    );

    // The ask is answered: its reply is the agent's, in the model's words, and the server holds
    // the turn it was taken under.
    let answered = asked(&workspaces, &workspace, &person).await;
    assert_eq!(answered.len(), 1, "{answered:?}");
    let ask = &answered[0];
    assert_eq!(ask.state, AskState::Done, "{ask:?}");
    // The turn of the agent in the conversation it was asked in, which one device answers.
    let lease = format!("turn/{}/{}", turn.agent.id.0, turn.conversation);
    assert_eq!(
        workspaces.lease_holder(&workspace, &lease).await.unwrap(),
        Some(workspaces.governor().device_public_key),
        "the server claimed the turn"
    );
    let reply = ask.latest().expect("the ask has a reply");
    assert_eq!(reply.attempt, 1);
    assert_eq!(reply.state, AskState::Done);
    assert!(reply.body.contains("the plan is ready"), "{}", reply.body);
    assert!(!reply.superseded);
    assert_eq!(
        model_asked.lock().unwrap().len(),
        2,
        "one model call to look, one to answer"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "an answered ask is owed nothing"
    );
    let _ = (root, host);
    page.peer.close();
}

/// What a writing provider does next in an answer.
#[derive(Clone)]
enum Next {
    /// It writes these words.
    Writes(&'static str),
    /// It thinks these words, and shows them.
    Thinks(&'static str),
    /// It waits until the test lets it go on.
    Waits,
    /// It lists the workspace through its tool, which ends the call.
    Looks,
}
use Next::{Looks, Thinks, Waits, Writes};

/// A provider the workspace linked that writes each answer piece by piece, as a provider
/// streams it.
struct Writing {
    /// The calls it answers, in order, each as the steps it takes.
    calls: Mutex<VecDeque<Vec<Next>>>,
    /// What it waits at: one call goes on each time the test lets it.
    paused: tokio::sync::Semaphore,
    /// How many times it came to wait.
    waited: AtomicUsize,
}

impl ModelProvider for Writing {
    fn answer<'a>(&'a self, _: ProviderCall<'a>, events: Events<'a>) -> ProviderFuture<'a> {
        let steps = self.calls.lock().unwrap().pop_front();
        Box::pin(async move {
            let (mut said, mut content) = (String::new(), Vec::new());
            let mut stop_reason = StopReason::EndTurn;
            for step in steps.expect("the provider is asked as often as the test wrote") {
                match step {
                    Writes(words) => {
                        events(StreamEvent::TextDelta {
                            text: words.to_owned(),
                        });
                        said.push_str(words);
                    }
                    Thinks(words) => events(StreamEvent::ReasoningDelta {
                        text: words.to_owned(),
                    }),
                    Waits => {
                        self.waited.fetch_add(1, Ordering::SeqCst);
                        self.paused.acquire().await.unwrap().forget();
                    }
                    Looks => {
                        content.push(Content::ToolCall(ToolCall {
                            id: "call-1".into(),
                            name: "tilo_ls".into(),
                            input: serde_json::json!({}),
                            extra_content: None,
                        }));
                        stop_reason = StopReason::ToolCalls;
                    }
                }
            }
            if !said.is_empty() {
                content.insert(0, Content::Text { text: said });
            }
            Ok(answer(content, stop_reason, 100))
        })
    }
}

/// Lets every call that waits go on when the test ends, however it ends.
struct LetsGo(Arc<Writing>);

impl Drop for LetsGo {
    fn drop(&mut self) {
        self.0.paused.add_permits(64);
    }
}

/// Link the workspace a provider of its own that answers `calls`, each as its steps.
async fn writing(
    workspaces: &Workspaces,
    workspace: &str,
    calls: impl IntoIterator<Item = Vec<Next>>,
) -> (Arc<Writing>, LetsGo) {
    let theirs = Arc::new(Writing {
        calls: Mutex::new(calls.into_iter().collect()),
        paused: tokio::sync::Semaphore::new(0),
        waited: AtomicUsize::new(0),
    });
    workspaces.answer_models_with(theirs.clone());
    workspaces
        .link_model_for(
            workspace,
            &Linking {
                name: "Google",
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "https://generativelanguage.googleapis.com/v1beta/openai",
                key: Some("sk-made-up-for-this-test"),
                models: &["gemini-main"],
            },
            true,
        )
        .await
        .unwrap();
    (theirs.clone(), LetsGo(theirs))
}

/// Begin the one turn the workspace owes `agents`, and go on while it works.
async fn begin(
    agents: &ServerAgents,
    workspace: &str,
) -> tokio::task::JoinHandle<Result<(), String>> {
    let turns = agents.turns(workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let (agents, workspace, turn) = (agents.clone(), workspace.to_owned(), turns[0].clone());
    tokio::spawn(async move { agents.answer(&workspace, &turn).await })
}

/// Her agent's model thinks, then writes its first sentences and is still writing: they stand
/// in the reply already, its thinking beside them, and the reply is working. When the model has written the rest, the reply reads the
/// whole answer and is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_grows_while_its_model_writes_it() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Thinks("May has the fewest clashes."),
            Writes("The launch is planned "),
            Writes("for May. "),
            Waits,
            Writes("The cake is ordered."),
        ]],
    )
    .await;
    mention_in_channel(&workspaces, &workspace, &person, " plan the launch").await;
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(never_asked(Arc::default()));
    let going = begin(&agents, &workspace).await;

    let grown = reply_reads(
        &workspaces,
        &workspace,
        &person,
        "The launch is planned for May.",
    )
    .await;
    assert_eq!(
        (theirs.waited.load(Ordering::SeqCst), going.is_finished()),
        (1, false),
        "the model has not written its last words"
    );
    assert_eq!(
        grown.state,
        AskState::Working,
        "a reply that grows works on"
    );
    assert_eq!(
        grown.thinking, "May has the fewest clashes.",
        "what its model thought shows beside the answer, never in it"
    );

    theirs.paused.add_permits(1);
    going.await.unwrap().unwrap();
    let whole = reply_reads(
        &workspaces,
        &workspace,
        &person,
        "The launch is planned for May. The cake is ordered.",
    )
    .await;
    assert_eq!(whole.state, AskState::Done);
    assert_eq!(
        (whole.provider.as_str(), whole.model.as_str(), whole.cost),
        ("Google", "gemini-main", 100),
        "the finished reply names the provider and model that answered it, and what the turn cost"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );
    let _ = (root, host);
    page.peer.close();
}

/// What a model writes before it uses a tool stays where its person read it, and what it
/// writes after the tool stands a paragraph below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn words_before_a_tool_stay_and_the_answer_stands_below_them() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("I look through the workspace first. "), Looks],
            vec![Waits, Writes("The plan is "), Writes("ready.")],
        ],
    )
    .await;
    mention_in_channel(&workspaces, &workspace, &person, " what is planned?").await;
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(never_asked(Arc::default()));
    let going = begin(&agents, &workspace).await;

    until("the model is asked again after its tool", || {
        theirs.waited.load(Ordering::SeqCst) == 1
    })
    .await;
    let before = reply_reads(
        &workspaces,
        &workspace,
        &person,
        "I look through the workspace first.",
    )
    .await;
    assert_eq!(before.state, AskState::Working);

    theirs.paused.add_permits(1);
    going.await.unwrap().unwrap();
    let whole = reply_reads(
        &workspaces,
        &workspace,
        &person,
        "I look through the workspace first.\n\nThe plan is ready.",
    )
    .await;
    assert_eq!(whole.state, AskState::Done);
    let _ = (root, host);
    page.peer.close();
}

/// A model that does what the words it is asked with say, the way a model does with these
/// tools: `remember: X` keeps X, and asked what it knows it reads the list. It reads the last
/// line of the ask alone: what the person wrote, never what the turn is told around it or after
/// it. Its answer is what the tool answered. Every request it was sent is kept.
struct Minded {
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

/// What opens what a turn is told of what its agent learned about the person.
const LEARNED: &str = "What you learned about this person";

impl tilo_agent_session::model::PlanTurns for Minded {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: std::time::Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let answered = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .find_map(|content| match content {
                Content::ToolResult(result) => Some(result.content.clone()),
                _ => None,
            });
        if let Some(answered) = answered {
            return Ok(answer(
                vec![Content::Text { text: answered }],
                StopReason::EndTurn,
                1,
            ));
        }
        let words = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, Role::User))
            .flat_map(|message| message.content.iter())
            .find_map(|content| match content {
                Content::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let own = words.split(LEARNED).next().unwrap_or_default();
        let last = own.lines().map(str::trim).rfind(|line| !line.is_empty());
        let call = |name: &str, input: serde_json::Value| {
            answer(
                vec![Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: name.into(),
                    input,
                    extra_content: None,
                })],
                StopReason::ToolCalls,
                1,
            )
        };
        Ok(match last {
            Some(line) if line.starts_with("remember: ") => call(
                "learn",
                serde_json::json!({
                    "text": line.trim_start_matches("remember: "),
                    "source": "They said so",
                }),
            ),
            Some(line) if line.starts_with("what do you know") => {
                call("learned", serde_json::json!({}))
            }
            _ => answer(
                vec![Content::Text {
                    text: "Nothing to do.".into(),
                }],
                StopReason::EndTurn,
                1,
            ),
        })
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// The tools `request` offers, by name.
fn tools_of(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

/// Admit `device`'s person to `workspace` as an editor, by an invitation its server issues, as a
/// second member joins.
async fn admit(
    workspaces: &Workspaces,
    server: &crate::support::Device,
    workspace: &str,
    device: &crate::support::Device,
) {
    let invitation = workspaces
        .issue(
            workspace,
            &server.cert,
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

/// Ada tells her agent something lasting in their direct conversation, where nobody else reads:
/// the turn is offered the tools that keep it, keeps it, and her next turn there is told it with
/// where it came from. Once Bo joins the workspace he may read what she wrote there too, so
/// her next turn is told nothing and offered none of the tools.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hosted_turn_keeps_what_its_keeper_tells_it_only_where_nobody_else_reads() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);
    let model_asked = Arc::new(Mutex::new(Vec::new()));
    let minded = {
        let asked = Arc::clone(&model_asked);
        Arc::new(move || {
            Some(Box::new(Minded {
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        }) as Model
    };
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(minded);
    assert!(
        workspaces.learned_about(&workspace).is_none(),
        "nothing yet"
    );

    write_in_direct(
        &workspaces,
        &workspace,
        &person,
        "remember: Takes her coffee black.",
    )
    .await;
    let waiting = asked(&workspaces, &workspace, &person).await;
    assert_eq!(
        waiting.len(),
        1,
        "writing to Scout asked it once: {waiting:?}"
    );
    assert_eq!(waiting[0].agent, MemberId::agent(SCOUT));
    assert_eq!(waiting[0].state, AskState::Waiting);
    assert_eq!(
        waiting[0].who,
        Some(asks::Actor::Member(MemberId::person(person.clone()))),
        "she asked it: {waiting:?}"
    );
    let turn = answered_one(&agents, &workspace).await;
    assert!(
        matches!(turn.ask, Ask::Asked { private: true, .. }),
        "her direct conversation is hers and Scout's alone: {:?}",
        turn.ask
    );
    let offered = tools_of(&model_asked.lock().unwrap()[0]);
    for tool in ["learn", "correct", "forget", "learned"] {
        assert!(
            offered.iter().any(|name| name == tool),
            "{tool} in {offered:?}"
        );
    }
    let kept = workspaces
        .learned_about(&workspace)
        .expect("the server keeps it");
    let facts: Vec<_> = kept.facts.iter().map(|(_, fact)| fact.clone()).collect();
    let [fact] = facts.as_slice() else {
        panic!("one fact, not {facts:?}");
    };
    assert_eq!(fact.text, "Takes her coffee black.");
    assert_eq!(fact.by.as_deref(), Some(SCOUT), "the agent kept it");
    let Ask::Asked { app, ask, .. } = &turn.ask else {
        unreachable!()
    };
    assert_eq!(
        fact.source,
        format!("They said so ({app}/{ask})"),
        "the ask it came from closes its source"
    );
    let reply = reply_reads(&workspaces, &workspace, &person, "Kept.").await;
    assert_eq!(reply.state, AskState::Done);

    // Her next turn there is told it, with where it came from.
    model_asked.lock().unwrap().clear();
    write_in_direct(
        &workspaces,
        &workspace,
        &person,
        "which milk should you order?",
    )
    .await;
    let turn = answered_one(&agents, &workspace).await;
    let line = format!("- Takes her coffee black. (from: {})", fact.source);
    assert!(
        turn.request.contains(LEARNED) && turn.request.contains(&line),
        "{}",
        turn.request
    );

    // Bo joins the workspace and reads its channels: what she asks Scout there is not hers and
    // Scout's alone, and is told nothing it learned about her.
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo = enrol(&bo_root, "bo-browser", DeviceCapabilities::member());
    admit(&workspaces, &host, &workspace, &bo).await;
    model_asked.lock().unwrap().clear();
    mention_in_channel(
        &workspaces,
        &workspace,
        &person,
        " what do you know about me?",
    )
    .await;
    let turn = answered_one(&agents, &workspace).await;
    assert!(
        matches!(turn.ask, Ask::Asked { private: false, .. }),
        "{:?}",
        turn.ask
    );
    assert!(!turn.request.contains("coffee"), "{}", turn.request);
    for request in model_asked.lock().unwrap().iter() {
        let offered = tools_of(request);
        assert!(
            ["learn", "correct", "forget", "learned"]
                .iter()
                .all(|tool| !offered.iter().any(|name| name == tool)),
            "none of the tools: {offered:?}"
        );
    }
    let _ = root;
    page.peer.close();
}

/// `person` makes Scout on the workspace's server, as the Agents tab does: the workspace's record
/// of its agents names it, kept by them.
async fn make_own_agent(workspaces: &Workspaces, workspace: &str, person: &str) {
    let record = collab_types::workspace_document_id(ui::tilo_workspace_agent::KIND);
    let person = person.to_owned();
    workspaces
        .author_workspace_document(
            workspace,
            workspaces.governor(),
            (ui::tilo_workspace_agent::OWNER, &record),
            move |doc| {
                let mut made =
                    ui::tilo_resource::Replica::<WorkspaceAgents>::new().as_author(&person);
                made.apply(
                    WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                        name: ui::tilo_resource::NonBlank::parse("Scout").unwrap(),
                        id: Some(SCOUT.into()),
                        model: None,
                        mcp_client: None,
                    }),
                )
                .map_err(|error| error.to_string())?;
                *doc = std::mem::take(made.doc_mut());
                Ok(())
            },
        )
        .await
        .unwrap();
}

/// Bea, a plain member, makes her own agent, and the server adds it to the workspace's member
/// list acting for her, as a member write for her: nobody sets up a grant for it. Cy, another plain member, may not
/// add an agent acting for her. She mentions Scout in a channel, and it answers her, reading
/// what she reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_makes_their_own_agent_and_it_answers_their_mention() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "tilo-sh", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let served = crate::harness::hosted::start(
        &root,
        &host,
        state.path(),
        crate::harness::hosted::Options::offering(crate::common::catalogue),
    )
    .await;
    let workspaces = served.workspaces().unwrap().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let (bea_root, _) = tilo_user_identity::UserIdentity::generate("Bea", 1);
    let bea = enrol(&bea_root, "bea-laptop", DeviceCapabilities::member());
    admit(&workspaces, &host, &workspace, &bea).await;
    let (cy_root, _) = tilo_user_identity::UserIdentity::generate("Cy", 1);
    let cy = enrol(&cy_root, "cy-laptop", DeviceCapabilities::member());
    admit(&workspaces, &host, &workspace, &cy).await;
    let bea_person = hex::encode(bea.cert.user_public_key);

    let refused = workspaces
        .add_agent(
            &workspace,
            MemberId::person(hex::encode(cy.cert.user_public_key)),
            "cys-scout",
            &bea_person,
        )
        .await
        .expect_err("Cy may not add an agent acting for Bea");
    make_own_agent(&workspaces, &workspace, &bea_person).await;
    workspaces
        .add_agent(
            &workspace,
            MemberId::person(bea_person.clone()),
            SCOUT,
            &bea_person,
        )
        .await
        .expect("Bea adds her own agent");
    let policy = workspaces.policy_of(&workspace).await.unwrap();
    assert_eq!(
        policy
            .agents
            .get(SCOUT)
            .map(|member| member.for_person.as_str()),
        Some(bea_person.as_str()),
        "Scout is a member acting for Bea"
    );
    assert!(
        !policy.agents.contains_key("cys-scout"),
        "Cy's addition changed nothing: {refused}"
    );

    mention_in_channel(&workspaces, &workspace, &bea_person, " what is planned?").await;
    // Bea approves her agent on the server from her own page, as Ada approves hers.
    let agents = served
        .approving_as(&bea_root, &workspace, &[SCOUT], &[])
        .await
        .scripted(looks_then_answers(1_000, Arc::default()));
    let turn = answered_one(&agents, &workspace).await;
    assert_eq!(turn.person, bea_person, "it works for Bea, who asked");
    assert!(
        turn.request.contains("what is planned?"),
        "Scout reads what Bea wrote: {}",
        turn.request
    );
    let answered = asked(&workspaces, &workspace, &bea_person).await;
    let reply = answered[0].latest().expect("the ask has a reply");
    assert_eq!(reply.state, AskState::Done, "{reply:?}");
    assert!(reply.body.contains("the plan is ready"), "{}", reply.body);
}

/// What the file a person sends with their message says.
const SENT_FILE: &[u8] = b"Move the launch to the fifth.\n";

/// A model that reads what the message's file says through Files' `read-text`, then answers;
/// each request it was sent is kept.
fn reads_the_sent_file(content: &str, asked: Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let read = answer(
        vec![Content::ToolCall(ToolCall {
            id: "call-1".into(),
            name: "tilo_do".into(),
            input: serde_json::json!({"path": "files#read-text", "args": {"content": content}}),
            extra_content: None,
        })],
        StopReason::ToolCalls,
        1_000,
    );
    let said = answer(
        vec![Content::Text {
            text: "The launch moves to the fifth.".into(),
        }],
        StopReason::EndTurn,
        1_000,
    );
    Arc::new(move || {
        Some(Box::new(Scripted {
            answers: vec![read.clone(), said.clone()],
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// What the last request tells the model its tools answered, in order, each with whether it
/// failed.
fn tool_answers(asked: &[ModelRequest]) -> Vec<(String, bool)> {
    asked
        .last()
        .expect("the model was asked")
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some((result.content.clone(), result.is_error)),
            _ => None,
        })
        .collect()
}

/// Ada sends a file with the message that asks her agent: the turn's request names the file and
/// the address Files reads it at, and the turn reads what it says through its tools.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_file_a_message_sends_reaches_the_turn_that_answers_it() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);

    // Her page puts a plan into the workspace, as the file her message sends.
    let mcp = McpServer::new(
        SessionActions::new(&page.peer, page.workspace, host.cert.device_public_key),
        workspace.clone(),
        person.clone(),
    );
    let content = content_in(&put(&mcp, SENT_FILE).await);
    let file = Sent {
        name: "launch.md".into(),
        media: "text/markdown".into(),
        size: i64::try_from(SENT_FILE.len()).expect("a small file"),
        hash: content
            .strip_prefix("tilo.content:")
            .expect("the put answers a content address")
            .to_owned(),
    };
    write_in_direct_with(
        &workspaces,
        &workspace,
        &person,
        "when is the launch?",
        vec![file],
    )
    .await;

    // The server reads the ask as a turn that is told the file it carries.
    let model_asked = Arc::new(Mutex::new(Vec::new()));
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(reads_the_sent_file(&content, Arc::clone(&model_asked)));
    let turn = answered_one(&agents, &workspace).await;
    assert!(
        turn.request.contains("when is the launch?"),
        "the turn asks what she wrote: {}",
        turn.request
    );
    assert!(
        turn.request.contains("launch.md")
            && turn.request.contains(&content)
            && turn.request.contains("files#read-text"),
        "the turn is told the file, where it reads, and how: {}",
        turn.request
    );

    // It read the file: what the tool answered the model is what the file says.
    let asked = model_asked.lock().unwrap().clone();
    let answers = tool_answers(&asked);
    assert!(
        answers
            .iter()
            .any(|(answer, failed)| !failed && answer.contains("Move the launch to the fifth.")),
        "the file's words reached the model: {answers:?}"
    );
    let reply = reply_reads(
        &workspaces,
        &workspace,
        &person,
        "The launch moves to the fifth.",
    )
    .await;
    assert_eq!(reply.state, AskState::Done);
    let _ = (root, host);
    page.peer.close();
}

/// `person` asks again for an answer to the last ask the record holds: the agent package's
/// `retry`, invoked as them. What it answered, or how it refused.
async fn retry_as(workspaces: &Workspaces, workspace: &str, person: &str) -> Result<(), String> {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let ask = asks::asks(opened)?
                .into_iter()
                .last()
                .ok_or_else(|| "the record holds no ask".to_owned())?;
            opened
                .invoke(
                    &who,
                    (asks::AGENT_APP, "retry"),
                    vec![Value::resource(&ask.at)],
                    None,
                )
                .map(drop)
        })
        .await
}

/// `person` opens a channel called `plans` and writes `said` there, oldest first, then writes
/// `ask` mentioning Scout, which asks it. Answers the channel's id and type, so a later message
/// can go into the same conversation.
async fn channel_with_chatter(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    said: &[String],
    ask: &str,
) -> (String, String) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let said = said.to_vec();
    let ask = ask.to_owned();
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            let all = opened
                .host()
                .root_of(&chat)
                .map_err(|error| error.to_string())?;
            let channel = opened.invoke(
                &who,
                (&chat, "new_channel"),
                vec![Value::resource(&all), Value::Text("plans".into())],
                None,
            )?;
            for line in &said {
                opened.invoke(
                    &who,
                    (&chat, "send"),
                    vec![
                        channel.clone(),
                        Value::Rich(RichValue::from_text(line)),
                        Value::Opt(None),
                    ],
                    None,
                )?;
            }
            opened.invoke(
                &who,
                (&chat, "send"),
                vec![
                    channel.clone(),
                    asks::mentioning(&MemberId::agent(SCOUT), "Scout", &ask),
                    Value::Opt(None),
                ],
                None,
            )?;
            let Value::Ref(reference) = &channel else {
                return Err("the channel is no resource".to_owned());
            };
            Ok((reference.target.id_string(), reference.target.ty.clone()))
        })
        .await
        .unwrap()
}

/// `person` writes `then` in the channel `channel` (its id and type), mentioning Scout, which
/// asks it there.
async fn mention_in(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    channel: &(String, String),
    then: &str,
) {
    let packages = asks::chat_packages().unwrap();
    let who = asks::Actor::Member(MemberId::person(person.to_owned()));
    let chat = packages.app().to_owned();
    let channel =
        asks::ResourceRef::parse_id(&channel.0, channel.1.clone()).expect("the channel's id");
    let then = then.to_owned();
    workspaces
        .with_tl_record(workspace, &packages, who.clone(), move |opened| {
            opened
                .invoke(
                    &who,
                    (&chat, "send"),
                    vec![
                        Value::resource(&channel),
                        asks::mentioning(&MemberId::agent(SCOUT), "Scout", &then),
                        Value::Opt(None),
                    ],
                    None,
                )
                .map(drop)
        })
        .await
        .unwrap();
}

/// What the agent package holds of the summaries in Chat's record now.
async fn summaries(workspaces: &Workspaces, workspace: &str, person: &str) -> Vec<asks::Summary> {
    let packages = asks::chat_packages().unwrap();
    workspaces
        .with_tl_record(
            workspace,
            &packages,
            asks::Actor::Member(MemberId::person(person.to_owned())),
            |opened| asks::summaries(opened),
        )
        .await
        .unwrap()
}

/// A turn that ends without an answer leaves a reply that failed: its asker asks again, and the
/// next turn is a second attempt whose reply stands beside the one that failed, which stays in
/// the record. Somebody else may not ask it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_answer_is_asked_again_as_its_next_attempt() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);
    mention_in_channel(&workspaces, &workspace, &person, " what is planned?").await;
    // The workspace's server has no model its agent can work with, so the turn fails.
    let approved = served.approving(&workspace, &[SCOUT]).await;
    let agents = approved.clone().scripted(None);
    let failed_turn = answered_one(&agents, &workspace).await;
    assert!(
        matches!(&failed_turn.ask, Ask::Asked { attempt: 1, .. }),
        "{:?}",
        failed_turn.ask
    );
    let failed = asked(&workspaces, &workspace, &person).await;
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].state, AskState::Failed);
    let first = failed[0].latest().expect("the turn wrote a reply").clone();
    assert_eq!(
        (first.attempt, first.state, first.superseded),
        (1, AskState::Failed, false),
        "the reply that failed is there: {first:?}"
    );
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "a failed ask is owed nothing until it is asked again"
    );

    // Somebody else asks it again: refused, and nothing moves.
    let (bo_root, _) = tilo_user_identity::UserIdentity::generate("Bo", 1);
    let bo = enrol(&bo_root, "bo-laptop", DeviceCapabilities::member());
    admit(&workspaces, &host, &workspace, &bo).await;
    let refused = retry_as(
        &workspaces,
        &workspace,
        &hex::encode(bo.cert.user_public_key),
    )
    .await
    .expect_err("only who asked may ask it again");
    assert!(refused.contains("Only who asked"), "{refused}");
    assert_eq!(
        asked(&workspaces, &workspace, &person).await[0].state,
        AskState::Failed,
        "another member's retry changed nothing"
    );

    // Its asker asks again: the same ask waits for a turn once more.
    retry_as(&workspaces, &workspace, &person)
        .await
        .expect("its asker asks it again");
    assert_eq!(
        asked(&workspaces, &workspace, &person).await[0].state,
        AskState::Waiting
    );

    // The next turn is the second attempt, and its reply stands beside the one that failed.
    let agents = approved.scripted(looks_then_answers(1_000, Arc::default()));
    let second = answered_one(&agents, &workspace).await;
    assert!(
        matches!(&second.ask, Ask::Asked { attempt: 2, .. }),
        "the turn is the second attempt: {:?}",
        second.ask
    );
    let answered = asked(&workspaces, &workspace, &person).await;
    assert_eq!(answered[0].state, AskState::Done);
    let replies = &answered[0].replies;
    assert_eq!(replies.len(), 2, "{replies:?}");
    let attempts: Vec<i64> = replies.iter().map(|reply| reply.attempt).collect();
    assert_eq!(attempts, [1, 2]);
    assert_eq!(
        (replies[0].state, replies[0].superseded),
        (AskState::Failed, false),
        "the reply that failed stays in the record, visible: {replies:?}"
    );
    assert_eq!(replies[1].state, AskState::Done);
    assert!(
        replies[1].body.contains("the plan is ready"),
        "{}",
        replies[1].body
    );
    let _ = root;
    page.peer.close();
}

/// The summary a long conversation's turn keeps.
const SUMMARY: &str = "The launch is planned for May, and the cake is ordered.";

/// A model that keeps `SUMMARY` when its turn offers the summary tool, and otherwise answers in
/// words. Every request it was sent is kept.
struct Summarizes {
    asked: Arc<Mutex<Vec<ModelRequest>>>,
}

impl tilo_agent_session::model::PlanTurns for Summarizes {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        self.asked.lock().unwrap().push(request.clone());
        let answered = request
            .messages
            .iter()
            .flat_map(|message| message.content.iter())
            .any(|content| matches!(content, Content::ToolResult(_)));
        if answered {
            return Ok(answer(
                vec![Content::Text {
                    text: "Kept.".into(),
                }],
                StopReason::EndTurn,
                1,
            ));
        }
        if request.tools.iter().any(|tool| tool.name == "keep-summary") {
            return Ok(answer(
                vec![Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "keep-summary".into(),
                    input: serde_json::json!({ "text": SUMMARY }),
                    extra_content: None,
                })],
                StopReason::ToolCalls,
                1,
            ));
        }
        Ok(answer(
            vec![Content::Text {
                text: "Nothing to do.".into(),
            }],
            StopReason::EndTurn,
            1,
        ))
    }

    fn model(&self) -> &'static str {
        "scripted"
    }
}

/// A conversation longer than the window a turn is shown: the model keeps a summary of what
/// came before, and the next turn there is told it, before the part it is shown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_conversation_keeps_a_summary_its_next_turn_is_told() {
    ada_and_scout!(root, host, served, workspaces, workspace, page, person);
    let said: Vec<String> = (0..40)
        .map(|line| format!("line {line}: how the launch is going"))
        .collect();
    let channel = channel_with_chatter(
        &workspaces,
        &workspace,
        &person,
        &said,
        " where does the launch stand?",
    )
    .await;
    let model_asked = Arc::new(Mutex::new(Vec::new()));
    let minded = {
        let asked = Arc::clone(&model_asked);
        Arc::new(move || {
            Some(Box::new(Summarizes {
                asked: Arc::clone(&asked),
            })
                as Box<dyn tilo_agent_session::model::PlanTurns>)
        }) as Model
    };
    let agents = served
        .approving(&workspace, &[SCOUT])
        .await
        .scripted(minded);
    let turn = answered_one(&agents, &workspace).await;
    let offered = tools_of(&model_asked.lock().unwrap()[0]);
    assert!(
        offered.iter().any(|name| name == "keep-summary"),
        "a turn shown less than came before it may keep a summary: {offered:?}"
    );
    assert!(
        !turn.request.contains("line 0:"),
        "the window leaves the oldest out: {}",
        turn.request
    );
    assert!(
        turn.request.contains("line 39:"),
        "the window holds the newest: {}",
        turn.request
    );
    let kept = summaries(&workspaces, &workspace, &person).await;
    let [summary] = kept.as_slice() else {
        panic!("one summary, not {kept:?}")
    };
    assert_eq!(summary.text, SUMMARY);
    assert_eq!(summary.agent, MemberId::agent(SCOUT));
    assert!(
        summary.conversation.is_some(),
        "the summary is keyed by the conversation it is made from"
    );
    let reply = reply_reads(&workspaces, &workspace, &person, "Kept.").await;
    assert_eq!(reply.state, AskState::Done);

    // Her next message there: the turn is told the summary before the part it is shown.
    model_asked.lock().unwrap().clear();
    mention_in(&workspaces, &workspace, &person, &channel, " and the cake?").await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let request = &turns[0].request;
    assert!(request.contains(SUMMARY), "{request}");
    assert!(
        request.find(SUMMARY) < request.find("In \"plans\":"),
        "the summary stands before the window: {request}"
    );
    tokio::time::timeout(
        Duration::from_secs(30),
        agents.answer(&workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    let told = model_asked.lock().unwrap()[0]
        .messages
        .iter()
        .filter(|message| matches!(message.role, Role::User))
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        told.contains(SUMMARY),
        "the model is sent the summary: {told}"
    );
    let kept = summaries(&workspaces, &workspace, &person).await;
    assert_eq!(
        kept.len(),
        1,
        "one summary per conversation and agent: {kept:?}"
    );
    assert_eq!(kept[0].text, SUMMARY);
    let _ = root;
    page.peer.close();
}
