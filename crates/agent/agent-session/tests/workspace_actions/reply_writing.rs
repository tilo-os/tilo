//! One provider for the tests that watch a reply grow: its call is a list of steps, so a test
//! says what its model thinks, writes, waits at, looks up or breaks off into, and when.
use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_server::workspaces::person_model::{
    Events, ModelProvider, ProviderCall, ProviderFuture, ProviderProtocol,
    protocol::{ErrorKind, StreamEvent},
    scripted::Linking,
};

/// How many lines a long report has beyond what one message holds.
pub(super) const BEYOND: usize = 120;

/// Line `n` of the long report.
pub(super) fn line(n: usize) -> String {
    format!("Line {n:05} of a long report: nothing is left out and nothing is said twice.")
}

/// How many lines the report has: more than one message holds.
pub(super) fn lines() -> usize {
    (chat::chat::MAX_MESSAGE_TEXT_BYTES / line(0).len().saturating_add(1)).saturating_add(BEYOND)
}

/// The report's lines from `from` to before `to`, as they stand in it.
pub(super) fn report(from: usize, to: usize) -> String {
    (from..to).map(line).collect::<Vec<_>>().join("\n")
}

/// How many words the paced answer is.
pub(super) const WORDS: usize = 400;

/// The wait between two of the paced answer's words.
pub(super) const PACED_EVERY: Duration = Duration::from_millis(25);

/// The paced answer whole: every word once, in order.
pub(super) fn paced_whole() -> String {
    (0..WORDS)
        .map(|n| format!("word{n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a provider does next in an answer.
#[derive(Clone, Copy)]
pub(super) enum Next {
    /// It shows this thought.
    Thinks(&'static str),
    /// It writes these words, streaming them.
    Writes(&'static str),
    /// Its answer is `text` whole, written without being streamed.
    Says(&'static str),
    /// It writes the report's lines from the first to before the second, one after the other.
    Reports(usize, usize),
    /// It writes [`WORDS`] words, one every [`PACED_EVERY`].
    Paced,
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

/// A person's provider that answers each call as its list of steps says, as a provider
/// streams an answer.
pub(super) struct Writing {
    /// The calls it answers, in order, each as the steps it takes.
    calls: Mutex<VecDeque<Vec<Next>>>,
    /// What it waits at: one call goes on each time the test lets it.
    paused: tokio::sync::Semaphore,
    /// How many times it came to wait.
    pub(super) waited: AtomicUsize,
    /// How many calls it was asked to answer.
    asked: AtomicUsize,
    /// How many of its calls whoever asked let go of before they ended.
    pub(super) let_go: AtomicUsize,
    /// When it wrote each word it streamed.
    wrote: Mutex<Vec<Instant>>,
}

impl Writing {
    /// Let the call that waits go on.
    pub(super) fn go_on(&self) {
        self.paused.add_permits(1);
    }

    /// How many calls it was asked to answer.
    pub(super) fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    /// When it wrote each word it streamed.
    pub(super) fn wrote(&self) -> Vec<Instant> {
        self.wrote.lock().unwrap().clone()
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
                    Next::Thinks(thought) => {
                        events(StreamEvent::ReasoningDelta {
                            text: thought.to_owned(),
                        });
                    }
                    Next::Writes(words) => {
                        self.wrote.lock().unwrap().push(Instant::now());
                        events(StreamEvent::TextDelta {
                            text: words.to_owned(),
                        });
                        said.push_str(words);
                    }
                    Next::Says(text) => said.push_str(text),
                    Next::Reports(from, to) => {
                        for n in from..to {
                            let text = if said.is_empty() {
                                line(n)
                            } else {
                                format!("\n{}", line(n))
                            };
                            self.wrote.lock().unwrap().push(Instant::now());
                            said.push_str(&text);
                            events(StreamEvent::TextDelta { text });
                            tokio::task::yield_now().await;
                        }
                    }
                    Next::Paced => {
                        for n in 0..WORDS {
                            if n > 0 {
                                tokio::time::sleep(PACED_EVERY).await;
                            }
                            self.wrote.lock().unwrap().push(Instant::now());
                            events(StreamEvent::TextDelta {
                                text: format!("word{n} "),
                            });
                        }
                        content.push(Content::Text {
                            text: paced_whole(),
                        });
                    }
                    Next::Waits => {
                        self.waited.fetch_add(1, Ordering::SeqCst);
                        self.paused.acquire().await.unwrap().forget();
                    }
                    Next::Looks => {
                        content.push(Content::ToolCall(ToolCall {
                            id: "call-1".into(),
                            name: "tilo_ls".into(),
                            input: serde_json::json!({}),
                            extra_content: None,
                        }));
                        stop_reason = StopReason::ToolCalls;
                    }
                    Next::Breaks => {
                        broke = true;
                        break;
                    }
                    Next::AskedAgain => {
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
pub(super) struct LetsGo(Arc<Writing>);

impl Drop for LetsGo {
    fn drop(&mut self) {
        self.0.paused.add_permits(64);
    }
}

/// Link the workspace a provider of its own that answers `calls`, each as its steps, and have
/// the server ask it for their agent's turns.
pub(super) async fn writing(
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
        wrote: Mutex::default(),
    });
    link(workspaces, workspace, theirs.clone()).await;
    (theirs.clone(), LetsGo(theirs))
}

/// Link the workspace a provider that answers [`Next::Paced`] once, and have the server ask it
/// for their agent's turns.
pub(super) async fn paced(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> (Arc<Writing>, LetsGo) {
    writing(workspaces, workspace, [vec![Next::Paced]]).await
}

/// Have the server answer the workspace's models with `theirs`.
async fn link(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    theirs: Arc<Writing>,
) {
    workspaces.answer_models_with(theirs);
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
pub(super) use begin;

/// The agent's reply to `message` once it reads `words`, whatever its work says.
pub(super) async fn reads(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
    message: &Id,
    words: &str,
) -> chat::chat::Message {
    reads_by(workspaces, workspace, (message, SCOUT), words).await
}

/// The reply of `agent` to `message` once it reads `words`, whatever its work says.
pub(super) async fn reads_by(
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
pub(super) fn written(
    reply: &chat::chat::Message,
) -> (u64, Option<chat::agent_stream::AgentFinish>) {
    let stream = reply
        .work
        .as_ref()
        .and_then(|work| work.stream.as_ref())
        .expect("the reply grew as its model wrote it");
    (stream.sequence, stream.finish)
}
