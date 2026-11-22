//! An answer longer than one message holds.
//!
//! A message of Chat holds a bounded text, and a model may write more than that in one
//! answer: a long report, a whole document. Where the reply is full the answer goes on in the
//! agent's next message, which grows as the first did: every word stands once and in order,
//! a message ends where a word ends, and no message says its work ended while the answer
//! still goes on. The provider here writes a report a little longer than one message holds,
//! line by line as a provider streams it, and waits where a test tells it to.
use super::*;
use chat::agent_stream::AgentFinish;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_server::workspaces::person_model::{
    Events, ModelProvider, ProviderCall, ProviderFuture, ProviderProtocol, protocol::StreamEvent,
    scripted::Linking,
};

/// How many lines the report has beyond what one message holds.
const BEYOND: usize = 120;

/// Line `n` of the long report.
fn line(n: usize) -> String {
    format!("Line {n:05} of a long report: nothing is left out and nothing is said twice.")
}

/// How many lines the report has: more than one message holds.
fn lines() -> usize {
    (chat::chat::MAX_MESSAGE_TEXT_BYTES / line(0).len().saturating_add(1)).saturating_add(BEYOND)
}

/// The report's lines from `from` to before `to`, as they stand in it.
fn report(from: usize, to: usize) -> String {
    (from..to).map(line).collect::<Vec<_>>().join("\n")
}

/// What a reporting provider does next in an answer.
#[derive(Clone)]
enum Next {
    /// It shows this thought.
    Thinks(&'static str),
    /// It writes these words.
    Writes(&'static str),
    /// It writes the report's lines from the first to before the second, one after the other.
    Reports(usize, usize),
    /// It waits until the test lets it go on.
    Waits,
    /// It lists the workspace through its tool, which ends the call.
    Looks,
}
use Next::{Looks, Reports, Thinks, Waits, Writes};

/// A person's provider that writes each answer piece by piece, as a provider streams it.
struct Reporting {
    /// The calls it answers, in order, each as the steps it takes.
    calls: Mutex<VecDeque<Vec<Next>>>,
    /// What it waits at: one call goes on each time the test lets it.
    paused: tokio::sync::Semaphore,
    /// How many times it came to wait.
    waited: AtomicUsize,
    /// How many of its calls whoever asked let go of before they ended.
    let_go: AtomicUsize,
}

/// A call under way: counted as let go of when it is dropped before it ended.
struct Call<'a> {
    of: &'a Reporting,
    ended: bool,
}

impl Drop for Call<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.of.let_go.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Reporting {
    /// Let the call that waits go on.
    fn go_on(&self) {
        self.paused.add_permits(1);
    }
}

impl ModelProvider for Reporting {
    fn answer<'a>(&'a self, _: ProviderCall<'a>, events: Events<'a>) -> ProviderFuture<'a> {
        let steps = self.calls.lock().unwrap().pop_front();
        Box::pin(async move {
            let mut call = Call {
                of: self,
                ended: false,
            };
            let (mut said, mut content) = (String::new(), Vec::new());
            let mut stop_reason = StopReason::EndTurn;
            for step in steps.expect("the provider is asked as often as the test wrote") {
                match step {
                    Thinks(thought) => events(StreamEvent::ReasoningDelta {
                        text: thought.to_owned(),
                    }),
                    Writes(words) => {
                        events(StreamEvent::TextDelta {
                            text: words.to_owned(),
                        });
                        said.push_str(words);
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
                    Reports(from, to) => {
                        for n in from..to {
                            let text = if said.is_empty() {
                                line(n)
                            } else {
                                format!("\n{}", line(n))
                            };
                            said.push_str(&text);
                            events(StreamEvent::TextDelta { text });
                            tokio::task::yield_now().await;
                        }
                    }
                    Waits => {
                        self.waited.fetch_add(1, Ordering::SeqCst);
                        self.paused.acquire().await.unwrap().forget();
                    }
                }
            }
            call.ended = true;
            if !said.is_empty() {
                content.insert(0, Content::Text { text: said });
            }
            Ok(answer(content, stop_reason, 100))
        })
    }
}

/// Lets every call that waits go on when the test ends, however it ends, so that none of the
/// provider's threads is left waiting.
struct LetsGo(Arc<Reporting>);

impl Drop for LetsGo {
    fn drop(&mut self) {
        self.0.paused.add_permits(64);
    }
}

/// Link `root` a provider of their own that answers `calls`, each as its steps, and have the
/// server ask it for their agent's turns.
async fn reporting(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    calls: impl IntoIterator<Item = Vec<Next>>,
) -> (Arc<Reporting>, LetsGo) {
    let theirs = Arc::new(Reporting {
        calls: Mutex::new(calls.into_iter().collect()),
        paused: tokio::sync::Semaphore::new(0),
        waited: AtomicUsize::new(0),
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

/// The messages `agent` answers `message` with, as the conversation holds them now: its
/// reply, and each message the answer goes on in, by the id Chat derives for the message
/// that follows a full one.
async fn answer_of(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    (message, agent): (&Id, &str),
) -> Vec<chat::chat::Message> {
    let part = conversation_part(workspaces, workspace).await;
    let by = MemberId::agent(agent);
    let mut said = Vec::new();
    let mut id = chat::chat::reply_id(message, &by);
    while let Some(held) = part.messages.get(&id) {
        said.push(held.clone());
        id = chat::chat::reply_id(&id, &by);
    }
    said
}

/// No turn a test holds: what [`reads_to`] is given where the server's own look began it.
const LOOKED: Option<&tokio::task::JoinHandle<()>> = None;

/// The answer of `agent` to `message` once its last message ends with `words`. `going` is the
/// turn that writes it, where the test holds it. A failure says what the answer holds, the
/// most it was seen to hold, how many messages the conversation's part holds and whether the
/// turn has ended.
async fn reads_to<T>(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    asked: (&Id, &str),
    words: &str,
    going: Option<&tokio::task::JoinHandle<T>>,
) -> Vec<chat::chat::Message> {
    let deadline = tilo_test_env::hang_deadline();
    let mut most = (0usize, String::new());
    loop {
        let said = answer_of(workspaces, workspace, asked).await;
        if said.last().is_some_and(|held| held.text.ends_with(words)) {
            return said;
        }
        let bytes: usize = said.iter().map(|held| held.text.len()).sum();
        if bytes >= most.0 {
            most = (bytes, sizes(&said));
        }
        let late = Instant::now() >= deadline;
        let held = if late {
            conversation_part(workspaces, workspace)
                .await
                .messages
                .iter()
                .count()
        } else {
            0
        };
        assert!(
            !late,
            "the answer of {} to {} never read to {words:?}: it holds {}; the most it was seen \
             to hold is {}; the conversation's part holds {held} messages; its turn has ended: \
             {:?}",
            asked.1,
            asked.0,
            sizes(&said),
            most.1,
            going.map(tokio::task::JoinHandle::is_finished)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The last words of `text`, for a failure's message.
fn end_of_text(text: &str) -> String {
    let from = text.len().saturating_sub(60);
    text.get(from..).unwrap_or_default().to_owned()
}

/// How much each of `said` holds and how the last ends, for a failure's message.
fn sizes(said: &[chat::chat::Message]) -> String {
    let bytes: Vec<usize> = said.iter().map(|held| held.text.len()).collect();
    let last = said
        .last()
        .map(|held| end_of_text(&held.text))
        .unwrap_or_default();
    format!(
        "{bytes:?} bytes in {} of the agent's messages, ending {last:?}",
        said.len()
    )
}

/// How the writing of `message` ended, as its stream says: none while it goes on.
fn finish_of(message: &chat::chat::Message) -> Option<AgentFinish> {
    message.work.as_ref()?.stream.as_ref()?.finish
}

/// What `message` shows of its model's thinking.
fn thinking_of(message: &chat::chat::Message) -> String {
    message
        .work
        .as_ref()
        .and_then(|work| work.stream.as_ref())
        .map_or_else(String::new, |stream| stream.reasoning.clone())
}

/// How far the work kept on `message` has come.
fn state_of(message: &chat::chat::Message) -> Option<&chat::model::AgentWorkState> {
    Some(&message.work.as_ref()?.state.as_ref()?.state)
}

/// The text of each of `said`.
fn texts(said: &[chat::chat::Message]) -> Vec<&str> {
    said.iter().map(|held| held.text.as_str()).collect()
}

/// `said`, read one after the other, is `whole`: every word once and in order, each message
/// within what a message holds, and each ends where a word ends, so that the next begins with
/// the word after it.
fn stands(whole: &str, said: &[chat::chat::Message]) {
    let mut rest = whole;
    for (nth, held) in said.iter().enumerate() {
        assert!(
            held.text.len() <= chat::chat::MAX_MESSAGE_TEXT_BYTES,
            "message {nth} stays within what a message holds: {}",
            sizes(said)
        );
        assert!(
            !held.text.is_empty() && held.text.trim() == held.text,
            "message {nth} begins with a word and ends with one: {}",
            sizes(said)
        );
        if nth > 0 {
            let next = rest.trim_start();
            assert!(
                next.len() < rest.len(),
                "the message before message {nth} ends where a word ends, not inside one: the \
                 answer goes on {:?}",
                next.get(..20)
            );
            rest = next;
        }
        let after = rest.strip_prefix(held.text.as_str());
        assert!(
            after.is_some(),
            "message {nth} goes on where the one before it ended, every word once and in \
             order: {}",
            sizes(said)
        );
        rest = after.unwrap_or_default();
    }
    assert!(
        rest.trim().is_empty(),
        "the model wrote {} bytes and the conversation lacks the last {} of them: it holds {}",
        whole.len(),
        rest.len(),
        sizes(said)
    );
}

/// Her assistant's model writes more than one message of Chat holds. While it is still
/// writing, past what the first message holds, the answer goes on in the agent's next
/// message and neither says its work ended. When the turn has ended, the agent's messages,
/// read one after the other, are the answer: every word once and in order, the first ending
/// where a word ends, each within what a message holds, and each says the answer ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_longer_than_one_message_stands_whole_in_the_conversation() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    // The provider waits with sixty lines written past what one message holds.
    let waits_at = lines().saturating_sub(BEYOND / 2);
    let (theirs, _lets_go) = reporting(
        &workspaces,
        &workspace,
        [vec![
            Reports(0, waits_at),
            Waits,
            Reports(waits_at, lines()),
        ]],
    )
    .await;
    assert!(
        report(0, waits_at).len() > chat::chat::MAX_MESSAGE_TEXT_BYTES,
        "what the model writes before it waits is longer than one message holds"
    );
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "write the long report",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(agents, workspace);

    let midway = reads_to(
        &workspaces,
        &workspace,
        (&first, SCOUT),
        &line(waits_at.saturating_sub(1)),
        Some(&going),
    )
    .await;
    assert_eq!(
        (theirs.waited.load(Ordering::SeqCst), going.is_finished()),
        (1, false),
        "its model has not written its last words"
    );
    assert_eq!(
        midway.len(),
        2,
        "the answer goes on in the agent's next message: {}",
        sizes(&midway)
    );
    stands(&report(0, waits_at), &midway);
    for (nth, held) in midway.iter().enumerate() {
        assert!(
            chat::summons::unanswered(Some(held)) && finish_of(held).is_none(),
            "message {nth} says its work goes on while the answer does: {:?}",
            held.work
        );
    }

    theirs.go_on();
    going.await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Complete),
        "the reply says the turn ended with its answer: {:?}",
        reply.work
    );
    let said = answer_of(&workspaces, &workspace, (&first, SCOUT)).await;
    stands(&report(0, lines()), &said);
    for (nth, held) in said.iter().enumerate() {
        assert_eq!(
            finish_of(held),
            Some(AgentFinish::Complete),
            "message {nth} says the answer ended"
        );
    }
    let part = conversation_part(&workspaces, &workspace).await;
    let scout = MemberId::agent(SCOUT);
    assert_eq!(
        part.messages
            .iter()
            .filter(|(_, held)| held.author == scout)
            .count(),
        said.len(),
        "the agent wrote the messages of its answer and no other"
    );
    let _ = person;
    page.peer.close();
}

/// A provider that does not stream answers whole, with more than one message holds. Nothing
/// of it was written while it came, so its turn's end writes it: the agent's messages, read
/// one after the other, are the answer, as for one that was written piece by piece.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_that_arrives_whole_and_is_longer_than_one_message_stands_whole() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "write the long report",
    )
    .await;
    let whole = report(0, lines());
    let answered = whole.clone();
    let agents = worked(
        &served,
        &workspace,
        plays(&Arc::default(), move |_| says(&answered)),
    )
    .await;
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Complete),
        "the reply says the turn ended with its answer: {:?}",
        reply.work
    );
    let said = answer_of(&workspaces, &workspace, (&first, SCOUT)).await;
    assert_eq!(
        said.len(),
        2,
        "the answer goes on in the agent's next message: {}",
        sizes(&said)
    );
    stands(&whole, &said);
    let _ = person;
    page.peer.close();
}

/// Her assistant is in the middle of a long answer, past what its first message holds, and
/// she stops it by naming the message of the answer numbered `nth`: the one a window shows the
/// work on, or the one the words arrive in. Either stop ends the one turn: the call to the
/// provider ends where it stood, every message of the answer keeps the words that came and
/// says it was stopped, and the reply says so for the turn.
async fn a_stop_that_names_a_message_of_a_long_answer_ends_its_turn(nth: usize) {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let waits_at = lines().saturating_sub(BEYOND / 2);
    let (theirs, _lets_go) = reporting(
        &workspaces,
        &workspace,
        [vec![
            Reports(0, waits_at),
            Waits,
            Reports(waits_at, lines()),
        ]],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "write the long report",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    agents.look(&workspace).await.unwrap();
    let midway = reads_to(
        &workspaces,
        &workspace,
        (&first, SCOUT),
        &line(waits_at.saturating_sub(1)),
        LOOKED,
    )
    .await;
    assert_eq!(
        (midway.len(), theirs.let_go.load(Ordering::SeqCst)),
        (2, 0),
        "the answer went on in a second message and its model is still asked: {}",
        sizes(&midway)
    );

    let scout = MemberId::agent(SCOUT);
    let reply = chat::chat::reply_id(&first, &scout);
    let named = [reply.clone(), chat::chat::reply_id(&reply, &scout)][nth].clone();
    let me = themselves(&root, &browser, &workspace);
    stop_agent(&actions, &workspace, &me, &named)
        .await
        .expect("the person stops their agent's work by a message of its answer");
    agents.look(&workspace).await.unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Cancelled),
        "its reply says it was stopped: {:?}",
        reply.work
    );
    until("the provider's call is let go of", || {
        theirs.let_go.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(
        theirs.waited.load(Ordering::SeqCst),
        1,
        "the call ended where it stood: the provider wrote nothing more"
    );
    let said = answer_of(&workspaces, &workspace, (&first, SCOUT)).await;
    assert_eq!(texts(&said), texts(&midway), "what came stays, as it stood");
    for (nth, held) in said.iter().enumerate() {
        assert_eq!(
            finish_of(held),
            Some(AgentFinish::Cancelled),
            "message {nth} says the answer was stopped"
        );
    }
    let _ = person;
    page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_that_names_the_first_message_of_a_long_answer_ends_its_turn() {
    Box::pin(a_stop_that_names_a_message_of_a_long_answer_ends_its_turn(
        0,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_that_names_a_following_message_of_a_long_answer_ends_its_turn() {
    Box::pin(a_stop_that_names_a_message_of_a_long_answer_ends_its_turn(
        1,
    ))
    .await;
}

/// The server stops while the second message of a long answer grows. Started again it asks
/// the model anew, since the turn changed nothing, and that answer stands below the words the
/// newest message held: the first message is as its person read it, no word that stood is
/// lost and none is written twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_long_answer_goes_on_below_its_newest_message_after_a_server_stopped() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let waits_at = lines().saturating_sub(BEYOND / 2);
    let (theirs, _lets_go) = reporting(
        &workspaces,
        &workspace,
        [
            vec![Reports(0, waits_at), Waits, Reports(waits_at, lines())],
            vec![Writes("The report ends here.")],
        ],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "write the long report",
    )
    .await;
    let dying = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(dying, workspace);
    let before = reads_to(
        &workspaces,
        &workspace,
        (&first, SCOUT),
        &line(waits_at.saturating_sub(1)),
        Some(&going),
    )
    .await;
    assert_eq!(
        before.len(),
        2,
        "the answer went on in a second message: {}",
        sizes(&before)
    );
    // The server's process ends: its turn is dropped where it stands, and the call nobody
    // waits for any more ends into nothing.
    going.abort();
    let _ = going.await;
    drop(dying);
    theirs.go_on();

    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    begin!(agents, workspace).await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        end_of(&reply),
        Some(AgentFinish::Complete),
        "the reply says the turn ended with its answer: {:?}",
        reply.work
    );
    let said = answer_of(&workspaces, &workspace, (&first, SCOUT)).await;
    let below = format!("{}\n\nThe report ends here.", before[1].text);
    assert_eq!(
        texts(&said),
        [before[0].text.as_str(), below.as_str()],
        "the first message is as it stood, and the answer stands below what the second held"
    );
    for (nth, held) in said.iter().enumerate() {
        assert_eq!(
            finish_of(held),
            Some(AgentFinish::Complete),
            "message {nth} says the answer ended"
        );
    }
    let _ = person;
    page.peer.close();
}

/// What a model thinks and the steps its turn takes stand with the first message of a long
/// answer, also once the words go on in the next: a thought that comes after the first
/// message is full is shown beside the first, below the thoughts before it, and the message
/// the answer went on in holds words alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_thinking_and_the_steps_of_a_long_answer_stay_with_its_first_message() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let (theirs, _lets_go) = reporting(
        &workspaces,
        &workspace,
        [
            vec![Thinks("The plan names May."), Reports(0, lines()), Looks],
            vec![
                Thinks("The tool agrees."),
                Waits,
                Writes("That is the whole report."),
            ],
        ],
    )
    .await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "write the long report",
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let going = begin!(agents, workspace);
    let deadline = tilo_test_env::hang_deadline();
    let midway = loop {
        let said = answer_of(&workspaces, &workspace, (&first, SCOUT)).await;
        if said
            .first()
            .is_some_and(|held| thinking_of(held).ends_with("The tool agrees."))
        {
            break said;
        }
        assert!(
            Instant::now() < deadline,
            "the first message never showed the thought that came after it was full: it \
             shows {:?} and the answer holds {}",
            said.first().map(thinking_of),
            sizes(&said)
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(
        (midway.len(), theirs.waited.load(Ordering::SeqCst)),
        (2, 1),
        "the answer went on in a second message: {}",
        sizes(&midway)
    );
    assert_eq!(
        thinking_of(&midway[0]),
        "The plan names May.\n\nThe tool agrees.",
        "what its model thought stands beside the first message"
    );
    assert_eq!(
        thinking_of(&midway[1]),
        "",
        "and none of it beside the message the answer went on in"
    );
    assert!(
        !matches!(
            state_of(&midway[0]),
            None | Some(chat::model::AgentWorkState::Taken)
        ),
        "the step the turn took is noted on the first message: {:?}",
        midway[0].work
    );
    assert!(
        matches!(
            state_of(&midway[1]),
            Some(chat::model::AgentWorkState::Taken)
        ) && midway[1]
            .work
            .as_ref()
            .is_some_and(|work| work.trace.is_empty()),
        "the message the answer went on in holds no step of the turn: {:?}",
        midway[1].work
    );

    theirs.go_on();
    going.await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(end_of(&reply), Some(AgentFinish::Complete));
    let said = answer_of(&workspaces, &workspace, (&first, SCOUT)).await;
    assert_eq!(said.len(), 2, "{}", sizes(&said));
    stands(
        &format!("{}\n\nThat is the whole report.", report(0, lines())),
        &said,
    );
    assert_eq!(
        (
            thinking_of(&said[0]).as_str(),
            thinking_of(&said[1]).as_str()
        ),
        ("The plan names May.\n\nThe tool agrees.", ""),
        "the thinking stands beside the first message at the end as well"
    );
    let _ = person;
    page.peer.close();
}

/// A sub-agent's model writes more than one message holds, in the conversation of its own.
/// Whoever serves its turn takes its words for the reply its turn answers and for the message
/// its server derived for that answer to go on in: the answer stands whole there, and none of
/// it in the thread of the agent that started it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sub_agents_long_answer_goes_on_in_its_own_conversation() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let ada = themselves(&root, &browser, &workspace);
    let thread = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent(SCOUT)]);
    let own = chat::Chat::direct_id([MemberId::person(person.clone()), MemberId::agent("writer")]);
    let (_theirs, _lets_go) = reporting(
        &workspaces,
        &workspace,
        [
            // Its job, then what its starter says to the news of it.
            vec![Writes("The notes are tidy.")],
            vec![Writes("Noted.")],
            // What its person asks it in its own conversation.
            vec![Reports(0, lines())],
        ],
    )
    .await;
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
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
        (own.as_str(), "Where do the notes stand, at length?"),
    )
    .await;
    begin!(agents, workspace).await.unwrap().unwrap();
    reply_of(&workspaces, &workspace, &asked, "writer").await;
    let said = answer_of(&workspaces, &workspace, (&asked, "writer")).await;
    assert_eq!(
        said.len(),
        2,
        "its answer goes on in its next message: {}",
        sizes(&said)
    );
    stands(&report(0, lines()), &said);
    for (nth, held) in said.iter().enumerate() {
        assert_eq!(
            (&held.author, conversation_id(held)),
            (&MemberId::agent("writer"), &own),
            "the sub-agent writes message {nth} in the conversation of its own"
        );
        assert_eq!(
            finish_of(held),
            Some(AgentFinish::Complete),
            "message {nth} says the answer ended"
        );
    }
    let above = said_there(&workspaces, &workspace, thread.as_str()).await;
    assert!(
        above
            .iter()
            .all(|(_, message)| !message.text.contains("long report")),
        "none of its words stands in its starter's thread"
    );
    page.peer.close();
}
