//! A reply grows while its model writes it.
//!
//! A turn's answer is its reply in the conversation, and its model writes that answer over
//! seconds. The words go into the reply as they arrive, through Chat's own `stream-output`,
//! which puts them at the end of the reply's text as one change of the conversation: the
//! person reads along on every device. The first words go at once. After them the reply
//! takes what came at most once in [`EVERY`], so an answer a minute long is some hundreds of
//! changes for everyone who holds the conversation, where one for each piece a model sends
//! would be thousands.
//!
//! What a model writes before it uses a tool stays where its person read it, and what it
//! writes afterwards stands a paragraph below. A reply whose turn ends before its answer
//! keeps the words that came and says below them why ([`Reply::grown_ends`]).
//!
//! Only the reply to a message grows, and only for an agent a person made: a sub-agent's
//! turn is let three writes in Chat, and this is none of them, so its reply is written once,
//! at its turn's end. So is the reply of a model whose answers arrive whole.

use std::sync::Mutex;
use std::time::Duration;

use chat::agent_stream::{AgentFinish, AgentOutput, AgentStreamEvent};
use serde_json::{Map, json};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use ui::permissions::MemberId;

use super::{Ask, CHAT, Outcome, Reply, Turn, asks, invoke_named};

/// How often, at most, a reply takes more of what its model writes, once it holds the first
/// words: five changes a second read as writing, and keep a long answer to hundreds of
/// changes.
const EVERY: Duration = Duration::from_millis(200);

/// What stands between what a model wrote before a tool, or before its turn ended early, and
/// what follows it: a paragraph's break.
const BELOW: &str = "\n\n";

/// Where a reply grows, in Chat's own names: its conversation, and the reply.
#[derive(Clone)]
struct Place {
    conversation: String,
    reply: String,
}

/// One more piece of a reply's text on its way to Chat.
struct Piece {
    place: Place,
    /// Its number among the events of the reply's stream.
    sequence: u64,
    /// What stands before the words: [`BELOW`], or nothing.
    lead: &'static str,
    said: String,
}

/// What a model wrote that its reply does not hold yet. The thread the model is asked on
/// puts the words here as they arrive, and whoever follows the answer takes them.
#[derive(Default)]
pub(super) struct Writes {
    words: Mutex<String>,
    more: tokio::sync::Notify,
}

impl Writes {
    /// The model wrote `text`.
    pub(super) fn push(&self, text: &str) {
        self.words.lock_recover().push_str(text);
        self.more.notify_one();
    }

    /// The model's call broke off. Where it is asked again, what it writes then stands
    /// below what came; where it is not, the break is the end and adds nothing.
    pub(super) fn broke_off(&self) {
        self.push(BELOW);
    }

    /// What came since it was last taken.
    fn take(&self) -> String {
        std::mem::take(&mut *self.words.lock_recover())
    }
}

/// How far a turn's reply has grown.
pub(super) struct Written {
    /// None for a reply that does not grow.
    place: Option<Place>,
    grown: Mutex<Grown>,
}

#[derive(Default)]
struct Grown {
    /// Whether Chat holds the reply as this turn's own, so that words may go into it.
    open: bool,
    /// How many events of the reply's stream Chat took.
    sequence: u64,
    /// How many bytes of words the reply holds.
    bytes: usize,
    /// What the call to the model that is under way put into the reply.
    call: String,
    /// Whether Chat refused a piece: no more goes in, and the reply may lack words.
    lost: bool,
}

impl Written {
    /// Where the reply of `turn` grows: nowhere for an issue's turn, whose answer is a
    /// comment, and nowhere for a sub-agent's.
    pub(super) fn of(turn: &Turn) -> Self {
        let place = match &turn.ask {
            Ask::Message {
                message, attempt, ..
            } if !turn.agent.is_sub() => Some(Place {
                conversation: asks::conversation_id_of(
                    &turn.conversation,
                    &turn.person,
                    &turn.agent.id.0,
                ),
                reply: chat::chat::reply_id_at(
                    &tilo_resource::Id::from_raw(message),
                    &MemberId::agent(&turn.agent.id.0),
                    *attempt,
                )
                .to_string(),
            }),
            _ => None,
        };
        Self {
            place,
            grown: Mutex::default(),
        }
    }

    /// Chat holds the reply as this turn's own from now on.
    pub(super) fn open(&self) {
        self.grown.lock_recover().open = true;
    }

    /// Take up what the reply holds already, as `held` keeps it: words a turn wrote into it
    /// on a server that stopped before that turn ended. What this turn writes stands below
    /// them.
    pub(super) fn goes_on_from(&self, held: &chat::parts::ChatPart) {
        let Some(place) = &self.place else {
            return;
        };
        let Some(reply) = held
            .messages
            .get(&tilo_resource::Id::from_raw(&place.reply))
        else {
            return;
        };
        let Some(stream) = reply.work.as_ref().and_then(|work| work.stream.as_ref()) else {
            return;
        };
        let mut grown = self.grown.lock_recover();
        grown.open = true;
        grown.sequence = stream.sequence;
        grown.bytes = reply.text.len();
    }

    /// The model is asked anew: what it writes now stands below what the reply holds.
    fn asked_anew(&self) {
        self.grown.lock_recover().call.clear();
    }

    /// The piece that puts `more` at the reply's end: none where the reply does not grow,
    /// where `more` is no words, or where the reply holds all a message may.
    fn next(&self, more: &str) -> Option<Piece> {
        let place = self.place.as_ref()?;
        let grown = self.grown.lock_recover();
        if !grown.open || grown.lost {
            return None;
        }
        // A call's first words begin without the blanks a model may lead with, and below
        // whatever the reply holds.
        let (lead, more) = match (grown.call.is_empty(), grown.bytes) {
            (true, 0) => ("", more.trim_start()),
            (true, _) => (BELOW, more.trim_start()),
            (false, _) => ("", more),
        };
        let fits = chat::chat::MAX_MESSAGE_TEXT_BYTES
            .saturating_sub(grown.bytes)
            .saturating_sub(lead.len());
        let sequence = grown.sequence.saturating_add(1);
        drop(grown);
        let said = beginning(more, fits);
        (!said.is_empty()).then(|| Piece {
            place: place.clone(),
            sequence,
            lead,
            said: said.to_owned(),
        })
    }

    /// Chat took `piece`.
    fn took(&self, piece: &Piece) {
        let mut grown = self.grown.lock_recover();
        grown.sequence = piece.sequence;
        grown.bytes = grown
            .bytes
            .saturating_add(piece.lead.len())
            .saturating_add(piece.said.len());
        grown.call.push_str(&piece.said);
    }

    /// Chat refused a piece.
    fn lost(&self) {
        self.grown.lock_recover().lost = true;
    }

    /// What of `whole`, the words of the model's answer to the call that just ended, the
    /// reply lacks: what its provider said only at the end. None for a reply nothing was
    /// written into, whose words its turn's end says, and none where the answer does not
    /// begin with what the reply was given, which stays as its person read it.
    fn lacks(&self, whole: &str) -> Option<String> {
        let grown = self.grown.lock_recover();
        if grown.call.is_empty() && grown.bytes == 0 {
            return None;
        }
        let rest = whole.strip_prefix(grown.call.as_str())?;
        drop(grown);
        (!rest.trim().is_empty()).then(|| rest.to_owned())
    }

    /// Where the end of the reply's stream goes and its number, for a reply that grew and
    /// lost nothing.
    fn end(&self) -> Option<(Place, u64)> {
        let place = self.place.as_ref()?;
        let grown = self.grown.lock_recover();
        (grown.open && !grown.lost && grown.sequence > 0)
            .then(|| (place.clone(), grown.sequence.saturating_add(1)))
    }

    /// Whether the reply holds words and may lack some, because Chat refused a piece.
    pub(super) fn lacks_words(&self) -> bool {
        let grown = self.grown.lock_recover();
        grown.lost && grown.bytes > 0
    }
}

/// `words` with the blanks before each paragraph's break left out: where a call broke off
/// after a blank, the break alone stands between what came and what follows.
fn settled(words: &str) -> String {
    let mut pieces: Vec<&str> = words.split(BELOW).collect();
    let last = pieces.pop().unwrap_or_default();
    let mut settled = String::with_capacity(words.len());
    for piece in pieces {
        settled.push_str(piece.trim_end());
        settled.push_str(BELOW);
    }
    settled.push_str(last);
    settled
}

/// The longest beginning of `text` that is at most `most` bytes.
fn beginning(text: &str, most: usize) -> &str {
    let mut end = most.min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

impl Reply {
    /// Put `more`, which the turn's model wrote just now, at the end of the reply.
    pub(super) async fn wrote(&self, more: &str) {
        let Some(piece) = self.written.next(more) else {
            return;
        };
        let text = format!("{}{}", piece.lead, piece.said);
        if self
            .stream(
                &piece.place,
                piece.sequence,
                AgentOutput::TextDelta { text },
            )
            .await
        {
            self.written.took(&piece);
        } else {
            self.written.lost();
        }
    }

    /// End a reply that grew, as `outcome`: one that ended before its answer says why below
    /// the words that came, and its stream ends as the turn did. Whether it was such a reply
    /// and Chat took its end; where not, its end is written as a reply's that did not grow.
    pub(super) async fn grown_ends(&self, outcome: &Outcome) -> bool {
        if self.written.end().is_none() {
            return false;
        }
        let (outcome, why) = match outcome {
            Outcome::Answered(_) => (AgentFinish::Complete, ""),
            Outcome::Stopped(words) => (AgentFinish::Truncated, words.as_str()),
            Outcome::Failed(words) => (AgentFinish::Failed, words.as_str()),
            Outcome::Cancelled => (AgentFinish::Cancelled, ""),
        };
        self.written.asked_anew();
        self.wrote(why).await;
        let Some((place, sequence)) = self.written.end() else {
            return false;
        };
        let ended = AgentOutput::Finished {
            outcome,
            detail: String::new(),
        };
        self.stream(&place, sequence, ended).await
    }

    /// Hand Chat the event numbered `sequence` of the reply's stream: whether it took it.
    /// Chat is asked through its messages as a whole, as for a post, and puts the event into
    /// the part that holds the reply.
    async fn stream(&self, place: &Place, sequence: u64, event: AgentOutput) -> bool {
        let route = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        };
        let event = AgentStreamEvent {
            run: place.reply.clone(),
            sequence,
            event,
        };
        let mut arguments = Map::new();
        arguments.insert("channel".into(), json!(place.conversation));
        arguments.insert("reply".into(), json!(place.reply));
        arguments.insert("event".into(), json!(event));
        match invoke_named(
            &self.provider,
            &self.caller,
            &route,
            &ui::resource::chain_resource_id(&chat::parts::chain()),
            "stream-output",
            arguments,
        )
        .await
        {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    workspace = %self.workspace,
                    reply = %place.reply,
                    %error,
                    "a reply did not take what its model wrote"
                );
                false
            }
        }
    }
}

/// Wait for `answer`, a model's answer on its way, and put what the model writes meanwhile
/// (`writes`) into `reply`: the first words at once, then what came since at most once in
/// [`EVERY`]. Words go in whole: a blank a model wrote last waits for the word after it,
/// and is left out at the answer's end. When the answer is there, `whole` reads its words,
/// and what of them the provider said only at the end goes into the reply as well.
pub(super) async fn follow<T, E>(
    reply: &Reply,
    mut answer: tokio::task::JoinHandle<Result<T, E>>,
    writes: &Writes,
    whole: impl FnOnce(&T) -> String,
) -> Result<Result<T, E>, tokio::task::JoinError> {
    reply.written.asked_anew();
    let mut words = String::new();
    let mut due = tokio::time::Instant::now();
    let answer = loop {
        tokio::select! {
            answer = &mut answer => break answer,
            () = writes.more.notified() => {
                words.push_str(&writes.take());
                words = settled(&words);
            }
            () = tokio::time::sleep_until(due), if !words.trim_end().is_empty() => {
                // Counted from where this write begins: the time Chat takes for it is no
                // wait on top.
                due = tokio::time::Instant::now().checked_add(EVERY).unwrap_or(due);
                let rest = words.split_off(words.trim_end().len());
                reply.wrote(&words).await;
                words = rest;
            }
        }
    };
    words.push_str(&writes.take());
    reply.wrote(settled(&words).trim_end()).await;
    if let Ok(Ok(answer)) = &answer
        && let Some(rest) = reply.written.lacks(&whole(answer))
    {
        reply.wrote(&rest).await;
    }
    answer
}
