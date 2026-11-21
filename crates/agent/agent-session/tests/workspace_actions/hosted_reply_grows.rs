//! A reply grows while its model writes it.
//!
//! The model a person linked writes its answer over seconds, and the server puts the words
//! into the reply as they arrive, so its person reads along on every device. The provider
//! here writes each answer piece by piece, as a provider streams it, and waits where a test
//! tells it to: a test reads the reply while the model is in the middle of its answer.
use super::*;
use chat::agent_stream::AgentFinish;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_server::workspaces::person_model::{
    Events, ModelProvider, ProviderCall, ProviderFuture, ProviderProtocol,
    protocol::{ErrorKind, StreamEvent},
    scripted::Linking,
};

/// What a writing provider does next in an answer.
#[derive(Clone)]
enum Next {
    /// It writes these words.
    Writes(&'static str),
    /// It waits until the test lets it go on.
    Waits,
    /// It lists the workspace through its tool, which ends the call.
    Looks,
    /// Its answer breaks off into what nobody can read, which ends the call without an
    /// answer. Asking again does not cure it, so whatever carries the call asks no more.
    Breaks,
    /// Its connection is lost and whatever carries the call asks again: it says so, and what
    /// it writes from here on is the answer.
    AskedAgain,
}
use Next::{AskedAgain, Breaks, Looks, Waits, Writes};

/// A person's provider that writes each answer piece by piece, as a provider streams it.
struct Writing {
    /// The calls it answers, in order, each as the steps it takes.
    calls: Mutex<VecDeque<Vec<Next>>>,
    /// What it waits at: one call goes on each time the test lets it.
    paused: tokio::sync::Semaphore,
    /// How many times it came to wait.
    waited: AtomicUsize,
    /// How many calls it was asked to answer.
    asked: AtomicUsize,
    /// How many of its calls whoever asked let go of before they ended.
    let_go: AtomicUsize,
}

impl Writing {
    /// Let the call that waits go on.
    fn go_on(&self) {
        self.paused.add_permits(1);
    }

    /// How many calls it was asked to answer.
    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

/// A call under way: counted as let go of when it is dropped before it ended.
struct Call<'a> {
    of: &'a Writing,
    ended: bool,
}

impl Drop for Call<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.of.let_go.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl ModelProvider for Writing {
    fn answer<'a>(&'a self, _: ProviderCall<'a>, events: Events<'a>) -> ProviderFuture<'a> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let steps = self.calls.lock().unwrap().pop_front();
        Box::pin(async move {
            let mut call = Call {
                of: self,
                ended: false,
            };
            let (mut said, mut content) = (String::new(), Vec::new());
            let mut stop_reason = StopReason::EndTurn;
            let mut broke = false;
            for step in steps.expect("the provider is asked as often as the test wrote") {
                match step {
                    Writes(words) => {
                        events(StreamEvent::TextDelta {
                            text: words.to_owned(),
                        });
                        said.push_str(words);
                    }
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
                    Breaks => {
                        broke = true;
                        break;
                    }
                    AskedAgain => {
                        events(StreamEvent::Failed {
                            error: ModelError {
                                kind: ErrorKind::Unavailable,
                                message: "the connection was lost".into(),
                            },
                        });
                        said.clear();
                    }
                }
            }
            call.ended = true;
            if broke {
                return Err(ModelError {
                    kind: ErrorKind::InvalidResponse,
                    message: "its answer could not be read to the end".into(),
                });
            }
            if !said.is_empty() {
                content.insert(0, Content::Text { text: said });
            }
            Ok(answer(content, stop_reason, 100))
        })
    }
}

/// Lets every call that waits go on when the test ends, however it ends, so that none of the
/// provider's threads is left waiting.
struct LetsGo(Arc<Writing>);

impl Drop for LetsGo {
    fn drop(&mut self) {
        self.0.paused.add_permits(64);
    }
}

/// Link the workspace a provider of its own that answers `calls`, each as its steps, and have
/// the server ask it for their agent's turns.
async fn writing(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    calls: impl IntoIterator<Item = Vec<Next>>,
) -> (Arc<Writing>, LetsGo) {
    let theirs = Arc::new(Writing {
        calls: Mutex::new(calls.into_iter().collect()),
        paused: tokio::sync::Semaphore::new(0),
        waited: AtomicUsize::new(0),
        asked: AtomicUsize::new(0),
        let_go: AtomicUsize::new(0),
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

/// Begin the one turn the workspace owes `$agents`, and go on while it works.
macro_rules! begin {
    ($agents:expr, $workspace:expr) => {{
        let turns = $agents.turns(&$workspace).await.unwrap();
        assert_eq!(turns.len(), 1, "{turns:?}");
        let (agents, workspace, turn) = ($agents.clone(), $workspace.clone(), turns[0].clone());
        tokio::spawn(async move { agents.answer(&workspace, &turn).await })
    }};
}

/// The agent's reply to `message` once it reads `words`, whatever its work says.
async fn reads(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    message: &Id,
    words: &str,
) -> chat::chat::Message {
    reads_by(workspaces, workspace, (message, SCOUT), words).await
}

/// The reply of `agent` to `message` once it reads `words`, whatever its work says.
async fn reads_by(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    (message, agent): (&Id, &str),
    words: &str,
) -> chat::chat::Message {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(workspaces, workspace).await;
        let reply = part
            .messages
            .get(&chat::chat::reply_id(message, &MemberId::agent(agent)));
        if let Some(reply) = reply
            && reply.text == words
        {
            return reply.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the reply of {agent} to {message} never read {words:?}: it reads {:?}",
            reply.map(|reply| reply.text.clone())
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// How many changes the words and the end of `reply` came in, and how its writing ended.
fn written(reply: &chat::chat::Message) -> (u64, Option<AgentFinish>) {
    let stream = reply
        .work
        .as_ref()
        .and_then(|work| work.stream.as_ref())
        .expect("the reply grew as its model wrote it");
    (stream.sequence, stream.finish)
}

/// Her assistant's model writes its first sentences and is still writing: they stand in the
/// reply already, on the server's own copy of the conversation, and the reply says its work
/// goes on. When the model has written the rest, the reply reads the whole answer and says
/// it ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_grows_while_its_model_writes_it() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Writes("The launch is planned "),
            Writes("for May. "),
            Waits,
            Writes("The cake is ordered."),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    let going = begin!(agents, workspace);

    let grown = reads(
        &workspaces,
        &workspace,
        &first,
        "The launch is planned for May.",
    )
    .await;
    assert_eq!(
        (theirs.waited.load(Ordering::SeqCst), going.is_finished()),
        (1, false),
        "the model has not written its last words"
    );
    assert!(
        chat::summons::unanswered(Some(&grown)),
        "a reply that grows says its work goes on: {:?}",
        grown.work
    );
    assert_eq!(grown.author, MemberId::agent(SCOUT), "the agent writes it");

    theirs.go_on();
    going.await.unwrap().unwrap();
    let whole = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        whole.text,
        "The launch is planned for May. The cake is ordered."
    );
    assert_eq!(end_of(&whole), Some(AgentFinish::Complete));
    assert_eq!(written(&whole).1, Some(AgentFinish::Complete));
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "answered once"
    );
    let _ = person;
    page.peer.close();
}

/// A model that writes three hundred pieces in one go does not make three hundred changes of
/// the conversation: the first words go into the reply at once and the rest together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_written_fast_is_few_changes() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (_theirs, _lets_go) = writing(&workspaces, &workspace, [vec![Writes("word "); 300]]).await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "count to three hundred",
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    begin!(agents, workspace).await.unwrap().unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(reply.text, "word ".repeat(300).trim_end());
    let (changes, end) = written(&reply);
    assert_eq!(end, Some(AgentFinish::Complete));
    assert!(
        changes <= 8,
        "three hundred pieces written at once came in {changes} changes"
    );
    let _ = person;
    page.peer.close();
}

/// What a model writes before it uses a tool stays where its person read it, and what it
/// writes after the tool stands a paragraph below.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn words_before_a_tool_stay_and_the_answer_stands_below_them() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("I look through the workspace first. "), Looks],
            vec![Waits, Writes("The plan is "), Writes("ready.")],
        ],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "what is planned?",
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    let going = begin!(agents, workspace);

    // The tool was used and the model is asked again: what it wrote before stands there.
    until("the model is asked again after its tool", || {
        theirs.waited.load(Ordering::SeqCst) == 1
    })
    .await;
    let before = reads(
        &workspaces,
        &workspace,
        &first,
        "I look through the workspace first.",
    )
    .await;
    assert!(
        chat::summons::unanswered(Some(&before)),
        "{:?}",
        before.work
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let whole = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        whole.text,
        "I look through the workspace first.\n\nThe plan is ready."
    );
    assert_eq!(end_of(&whole), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// Her stop ends the call to the provider itself, while the provider is in the middle of its
/// answer. The reply keeps the words that came and says it was stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_ends_the_models_call_and_the_reply_keeps_what_came() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![Writes("The launch is "), Waits, Writes("never read")]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    agents.look(&workspace).await.unwrap();
    reads(&workspaces, &workspace, &first, "The launch is").await;
    assert_eq!(theirs.let_go.load(Ordering::SeqCst), 0);

    let me = themselves(&root, &browser, &workspace);
    stop_agent(&actions, &workspace, &me, &first)
        .await
        .expect("the person stops their agent's work");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.work
    );
    assert_eq!(reply.text, "The launch is", "what came stays");
    until("the provider's call is let go of", || {
        theirs.let_go.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(
        theirs.waited.load(Ordering::SeqCst),
        1,
        "the call ended where it stood: the provider wrote nothing more"
    );
    let _ = person;
    page.peer.close();
}

/// A provider whose answer breaks off in the middle, in a way asking again does not cure: the
/// reply keeps the words that came, and says below them that the model did not answer and why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_that_breaks_off_keeps_what_came_and_says_why() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (_theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![Writes("The launch is "), Breaks]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    begin!(agents, workspace).await.unwrap().unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text,
        "The launch is\n\nMy model did not answer: its answer could not be read to the end"
    );
    assert_eq!(end_of(&reply), Some(AgentFinish::Failed));
    assert_eq!(written(&reply).1, Some(AgentFinish::Failed));
    let _ = person;
    page.peer.close();
}

/// A call that breaks off and is asked again within the turn: what came of the first asking
/// stays, and the answer stands a paragraph below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_answer_of_a_call_asked_again_stands_below_what_came() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (_theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [vec![
            Writes("The launch is "),
            AskedAgain,
            Writes("The launch is planned "),
            Writes("for May."),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    begin!(agents, workspace).await.unwrap().unwrap();

    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text,
        "The launch is\n\nThe launch is planned for May."
    );
    assert_eq!(end_of(&reply), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// The server stops while a reply grows. Started again it asks the model anew, since the turn
/// changed nothing, and the answer stands below the words the reply held: nothing a person
/// read is taken away, and the answer is not lost behind them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_that_grew_on_a_server_that_stopped_gets_its_answer_below() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("The launch is "), Waits, Writes("never read")],
            vec![Writes("The launch is planned for May.")],
        ],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "plan the launch",
    )
    .await;
    let dying = worked(&workspaces, &actions, never_asked(Arc::default()));
    let going = begin!(dying, workspace);
    reads(&workspaces, &workspace, &first, "The launch is").await;
    // The server's process ends: its turn is dropped where it stands, and the call nobody
    // waits for any more ends into nothing.
    going.abort();
    let _ = going.await;
    drop(dying);
    theirs.go_on();

    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    begin!(agents, workspace).await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text,
        "The launch is\n\nThe launch is planned for May."
    );
    assert_eq!(end_of(&reply), Some(AgentFinish::Complete));
    let _ = person;
    page.peer.close();
}

/// A sub-agent's model writes its answer over seconds as well. The words stand in its reply,
/// in the conversation of its own, while the model is still writing, and none of them stands
/// in the thread of the agent that started it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agents_reply_grows_in_its_own_conversation() {
    team!(root, browser, actions, workspaces, workspace, page, person);
    let ada = themselves(&root, &browser, &workspace);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let own = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent("writer")]);
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            // Its job, then what its starter says to the news of it.
            vec![Writes("The notes are tidy.")],
            vec![Writes("Noted.")],
            // What its person asks it in its own conversation.
            vec![
                Writes("Two drafts "),
                Writes("are left. "),
                Waits,
                Writes("I file them next."),
            ],
        ],
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    starts(
        &workspaces,
        &workspace,
        &person,
        ("Writer", "Tidy the notes."),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let job = caused(&workspaces, &workspace, (own.as_str(), "job"), 1).await[0]
        .0
        .clone();
    agents.look(&workspace).await.unwrap();
    let done = reply_of(&workspaces, &workspace, &job, "writer").await;
    assert_eq!(done.text, "The notes are tidy.");
    agents.look(&workspace).await.unwrap();
    let news = caused(&workspaces, &workspace, (thread.as_str(), "news"), 1).await[0]
        .0
        .clone();
    agents.look(&workspace).await.unwrap();
    reply_of(&workspaces, &workspace, &news, SCOUT).await;

    let asked = writes_in(
        &actions,
        &workspaces,
        &workspace,
        &ada,
        (own.as_str(), "Where do the notes stand?"),
    )
    .await;
    let going = begin!(agents, workspace);
    let grown = reads_by(
        &workspaces,
        &workspace,
        (&asked, "writer"),
        "Two drafts are left.",
    )
    .await;
    assert_eq!(
        (theirs.waited.load(Ordering::SeqCst), going.is_finished()),
        (1, false),
        "its model has not written its last words"
    );
    assert_eq!(
        grown.author,
        MemberId::agent("writer"),
        "the sub-agent writes it"
    );
    assert_eq!(
        *conversation_id(&grown),
        own,
        "in the conversation of its own"
    );
    assert!(
        chat::summons::unanswered(Some(&grown)),
        "a reply that grows says its work goes on: {:?}",
        grown.work
    );
    let above = said_there(&workspaces, &workspace, thread.as_str()).await;
    assert!(
        above
            .iter()
            .all(|(_, message)| !message.text.contains("Two drafts")),
        "none of its words stands in its starter's thread: {above:?}"
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let whole = reply_of(&workspaces, &workspace, &asked, "writer").await;
    assert_eq!(whole.text, "Two drafts are left. I file them next.");
    assert_eq!(end_of(&whole), Some(AgentFinish::Complete));
    assert_eq!(written(&whole).1, Some(AgentFinish::Complete));
    page.peer.close();
}

/// The guest `guest` holds `role` in `workspace` from now on, as its administrator puts them
/// there: what the server may write for them is decided by it.
async fn guest_holds(
    workspaces: &Workspaces,
    workspace: &str,
    server: &Device,
    guest: &SigningKey,
    role: tilo_peer_state::join::Role,
) {
    workspaces
        .add_member(
            workspace,
            &server.cert,
            tilo_server::workspaces::AddMemberRequest {
                request_id: *blake3::hash(
                    format!(
                        "{} {role:?} {}",
                        hex::encode(guest.verifying_key().to_bytes()),
                        tilo_clock::unix_now_ns()
                    )
                    .as_bytes(),
                )
                .as_bytes(),
                user_public_key: guest.verifying_key().to_bytes(),
                place: tilo_server::workspaces::MemberPlace::Role(role),
                name: Some("Eve".into()),
            },
        )
        .await
        .unwrap();
}

/// Chat refuses the end of the answer: the turn ends with the error that nobody kept it, and its
/// reply is not taken for one its person read as answered. What the model wrote stays where they
/// read it, and once the workspace lets them write again, the answer stands below those words
/// and the reply ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reply_whose_end_chat_refused_is_not_taken_for_answered() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served, device
    );
    // A guest of the workspace asks the agent, and the workspace decides what the server may
    // write for them: an editor, until the answer is on its way.
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Eve", 2);
    let guest_device = enrol(&guest, "eve-browser", DeviceCapabilities::member());
    let editor = tilo_peer_state::join::Role::Editor;
    guest_holds(&workspaces, &workspace, &device, &guest, editor).await;
    // The guest's own device is the one the server acts as for them.
    let _guest =
        resource_support::open_admitted_at(&served, &guest_device, &device, &workspace).await;
    let asked = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&guest, &guest_device),
        "plan the launch",
    )
    .await;
    let (theirs, _lets_go) = writing(
        &workspaces,
        &workspace,
        [
            vec![Writes("The plan is ready."), Waits],
            vec![Writes("The plan is ready. The cake is ordered.")],
        ],
    )
    .await;
    let agents = worked(&workspaces, &actions, never_asked(Arc::default()));
    let going = begin!(agents, workspace);
    let grown = reads(&workspaces, &workspace, &asked, "The plan is ready.").await;
    assert!(
        chat::summons::unanswered(Some(&grown)),
        "its end is not recorded yet: {:?}",
        grown.work
    );

    // The guest may read the workspace from now on and change nothing in it: Chat refuses the
    // end of the answer the server writes for them.
    guest_holds(
        &workspaces,
        &workspace,
        &device,
        &guest,
        tilo_peer_state::join::Role::Viewer,
    )
    .await;
    theirs.go_on();
    let refused = going.await.unwrap();
    assert!(
        refused.is_err(),
        "the end Chat did not take is not taken for one: {refused:?}"
    );
    let part = conversation_part(&workspaces, &workspace).await;
    let reply = part
        .messages
        .get(&chat::chat::reply_id(&asked, &MemberId::agent(SCOUT)))
        .expect("the reply stands where its person read it")
        .clone();
    assert_eq!(reply.text, "The plan is ready.");
    assert!(
        chat::summons::unanswered(Some(&reply)),
        "no end was kept, and the reply says nothing it did not: {:?}",
        reply.work
    );

    // The workspace lets the guest write for themselves again. The next look ends the turn, and
    // the answer the model wrote now stands below the words its person already read.
    guest_holds(&workspaces, &workspace, &device, &guest, editor).await;
    agents.look(&workspace).await.unwrap();
    let finished = ended(&workspaces, &workspace, &asked).await;
    assert_eq!(end_of(&finished), Some(AgentFinish::Complete));
    assert_eq!(
        finished.text,
        "The plan is ready.\n\nThe plan is ready. The cake is ordered."
    );
    assert_eq!(theirs.asked(), 2, "the answer is continued, not lost");
    assert!(
        agents.turns(&workspace).await.unwrap().is_empty(),
        "and it is not owed a turn again"
    );
    let _ = person;
    page.peer.close();
}
