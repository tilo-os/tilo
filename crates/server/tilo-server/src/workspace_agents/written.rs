//! A reply grows while its model writes it.
//!
//! A turn's answer is its reply in the conversation, and its model writes that answer over
//! seconds. The words go into the reply as they arrive, through Chat's own `stream-output`,
//! which puts them at the end of the reply's text as one change of the conversation: the
//! person reads along on every device. Each piece goes the moment it comes, and no clock
//! holds a word back. What a model writes while a piece is on its way to Chat goes with the
//! next one, so a reply takes no more changes than its server can write.
//!
//! What a model writes before it uses a tool stays where its person read it, and what it
//! writes afterwards stands a paragraph below. A reply whose turn ends before its answer
//! keeps the words that came and says below them why ([`Reply::grown_ends`]).
//!
//! Only the reply to a message grows: an issue's turn answers with a comment, written once.
//! A sub-agent's reply grows in the conversation of its own, the one place its turn writes,
//! and whoever serves that turn takes its model's words for that reply alone
//! ([`super::sub_agents::admits`]). The reply of a model whose answers arrive whole is
//! written once, at its turn's end.

use std::sync::Mutex;

use chat::agent_stream::{AgentFinish, AgentOutput, AgentStreamEvent};
use serde_json::{Map, json};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;
use ui::permissions::MemberId;

use super::{Ask, CHAT, Outcome, Reply, Turn, asks, invoke_named};

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

/// The reply `turn` answers with, by the id Chat derives for it: none for an issue's turn,
/// whose answer is a comment.
pub(super) fn reply_of(turn: &Turn) -> Option<String> {
    match &turn.ask {
        Ask::Message {
            message, attempt, ..
        } => Some(
            chat::chat::reply_id_at(
                &tilo_resource::Id::from_raw(message),
                &MemberId::agent(&turn.agent.id.0),
                *attempt,
            )
            .to_string(),
        ),
        Ask::Issue { .. } => None,
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
    /// comment. A sub-agent's grows in the conversation its item names, the only one its
    /// turn writes in.
    pub(super) fn of(turn: &Turn) -> Self {
        let place = reply_of(turn).map(|reply| Place {
            conversation: turn.agent.sub.as_ref().map_or_else(
                || asks::conversation_id_of(&turn.conversation, &turn.person, &turn.agent.id.0),
                |sub| sub.conversation.clone(),
            ),
            reply,
        });
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

/// What takes a model's words while it writes them: a turn's reply.
pub(super) trait Takes {
    /// The model is asked anew: what it writes now stands below what came.
    fn asked_anew(&self);

    /// Put `more`, which the model wrote just now, at the end.
    fn take(&self, more: &str) -> impl std::future::Future<Output = ()> + Send;

    /// What of `whole`, the words of the answer to the call that just ended, it lacks.
    fn lacks(&self, whole: &str) -> Option<String>;
}

impl Takes for Reply {
    fn asked_anew(&self) {
        self.written.asked_anew();
    }

    async fn take(&self, more: &str) {
        self.wrote(more).await;
    }

    fn lacks(&self, whole: &str) -> Option<String> {
        self.written.lacks(whole)
    }
}

/// Wait for `answer`, a model's answer on its way, and put what the model writes meanwhile
/// (`writes`) into `reply`, each piece the moment it comes: what arrives while a piece is on
/// its way goes with the next. Words go in whole: a blank a model wrote last waits for the
/// word after it, and is left out at the answer's end. When the answer is there, `whole`
/// reads its words, and what of them the provider said only at the end goes into the reply
/// as well.
pub(super) async fn follow<T, E>(
    reply: &(impl Takes + Sync),
    mut answer: tokio::task::JoinHandle<Result<T, E>>,
    writes: &Writes,
    whole: impl FnOnce(&T) -> String,
) -> Result<Result<T, E>, tokio::task::JoinError> {
    reply.asked_anew();
    let mut words = String::new();
    let answer = loop {
        tokio::select! {
            answer = &mut answer => break answer,
            () = writes.more.notified() => {
                words.push_str(&writes.take());
                words = settled(&words);
                // No clock holds a word back: it goes now. What the model writes while it
                // is on its way waits for nothing but that, and goes next.
                if !words.trim_end().is_empty() {
                    let rest = words.split_off(words.trim_end().len());
                    reply.take(&words).await;
                    words = rest;
                }
            }
        }
    };
    words.push_str(&writes.take());
    reply.take(settled(&words).trim_end()).await;
    if let Ok(Ok(answer)) = &answer
        && let Some(rest) = reply.lacks(&whole(answer))
    {
        reply.take(&rest).await;
    }
    answer
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    /// Keeps each piece it is given with the millisecond it came, on the test's own clock.
    /// Taking a piece costs it `costs`, as a write into a conversation costs a server.
    struct Kept {
        began: tokio::time::Instant,
        costs: Duration,
        pieces: Mutex<Vec<(u128, String)>>,
    }

    impl Kept {
        fn new(costs: Duration) -> Self {
            Self {
                began: tokio::time::Instant::now(),
                costs,
                pieces: Mutex::default(),
            }
        }

        /// What it was given, each piece with the millisecond it came.
        fn pieces(&self) -> Vec<(u128, String)> {
            self.pieces.lock_recover().clone()
        }
    }

    impl Takes for Kept {
        fn asked_anew(&self) {}

        async fn take(&self, more: &str) {
            if more.is_empty() {
                return;
            }
            self.pieces
                .lock_recover()
                .push((self.began.elapsed().as_millis(), more.to_owned()));
            tokio::time::sleep(self.costs).await;
        }

        fn lacks(&self, _: &str) -> Option<String> {
            None
        }
    }

    /// A model that writes each of `pieces` after its wait in milliseconds, and answers a
    /// tenth of a second after the last.
    fn writing(
        writes: &Arc<Writes>,
        pieces: &'static [(u64, &'static str)],
    ) -> tokio::task::JoinHandle<Result<(), ()>> {
        let writes = Arc::clone(writes);
        tokio::spawn(async move {
            for (after, piece) in pieces {
                tokio::time::sleep(Duration::from_millis(*after)).await;
                writes.push(piece);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        })
    }

    /// A word that arrives alone stands in the reply the millisecond it came: no clock holds
    /// it back until more has come.
    #[tokio::test(start_paused = true)]
    async fn a_word_that_arrives_alone_is_written_when_it_arrives() {
        let writes = Arc::new(Writes::default());
        let kept = Kept::new(Duration::from_millis(2));
        let model = writing(
            &writes,
            &[
                (0, "The "),
                (10, "launch "),
                (10, "is "),
                (150, "planned "),
                (10, "for May."),
            ],
        );
        let answered = follow(&kept, model, &writes, |()| String::new()).await;
        assert!(matches!(answered, Ok(Ok(()))), "the model answered");
        assert_eq!(
            kept.pieces(),
            [
                (0, "The"),
                (10, " launch"),
                (20, " is"),
                (170, " planned"),
                (180, " for May.")
            ]
            .map(|(at, said)| (at, said.to_owned())),
            "each word stands in the reply the millisecond it came"
        );
    }

    /// What a model writes while a piece is on its way goes with the next one: a reply takes
    /// no more changes than its server can write, and waits for nothing else.
    #[tokio::test(start_paused = true)]
    async fn what_arrives_while_a_piece_is_on_its_way_goes_with_the_next() {
        let writes = Arc::new(Writes::default());
        let kept = Kept::new(Duration::from_millis(30));
        let model = writing(
            &writes,
            &[(0, "One "), (5, "two "), (5, "three "), (5, "four.")],
        );
        let answered = follow(&kept, model, &writes, |()| String::new()).await;
        assert!(matches!(answered, Ok(Ok(()))), "the model answered");
        assert_eq!(
            kept.pieces(),
            [(0, "One"), (30, " two three four.")].map(|(at, said)| (at, said.to_owned())),
            "the pieces that came during the first write go together, when it has ended"
        );
    }
}
