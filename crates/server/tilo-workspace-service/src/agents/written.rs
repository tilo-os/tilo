//! A reply grows while its model writes it.
//!
//! A turn's answer is its reply in the conversation, and its model writes that answer over
//! seconds. The words go into the reply as they arrive, through Chat's own `stream-output`,
//! which puts them at the end of the reply's text as one change of the conversation: the
//! person reads along on every device. Each piece goes the moment it comes, and no clock
//! holds a word back. What a model writes while a piece is on its way to Chat goes with the
//! next one, so a reply takes no more changes than its server can write.
//!
//! What a model thinks before it writes, where its provider shows that, goes into the reply's
//! thinking the same way and never among its words ([`Reply::thought`]): a wait shows the
//! model thinking.
//!
//! What a model writes before it uses a tool stays where its person read it, and what it
//! writes afterwards stands a paragraph below. A reply whose turn ends before its answer
//! keeps the words that came and says below them why ([`Reply::grown_ends`]).
//!
//! A message of Chat holds a bounded text, and an answer may be longer. Where the reply holds
//! all a message takes, the answer goes on in the agent's next message, which grows as the
//! reply did: its id is the one Chat derives from the full message's, so every server finds
//! it again with nothing kept beside the conversation. Every word stands once and in order,
//! and each message ends where a word ends. The reply keeps the turn's thinking, its steps
//! and its end: every message of an answer says it goes on until the turn has ended.
//!
//! Only the reply to a message grows: an issue's turn answers with a comment, written once.
//! A sub-agent's reply grows in the conversation of its own, the one place its turn writes,
//! and whoever serves that turn takes its model's words for that reply alone
//! ([`super::sub_agents::admits`]). The reply of a model whose answers arrive whole is
//! written once, at its turn's end.

use std::sync::Mutex;

use chat::agent_stream::{AgentFinish, AgentOutput, AgentStreamEvent};
use collaboration::permissions::MemberId;
use serde_json::{Map, json};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::document_frame::DocumentRoute;

use super::{Ask, CHAT, Outcome, Reply, Turn, invoke_named};

/// What stands between what a model wrote before a tool, or before its turn ended early, and
/// what follows it: a paragraph's break.
const BELOW: &str = "\n\n";

/// How much thinking Chat keeps of one reply (`chat::agent_stream::AgentStream::accepts`): it
/// refuses a piece past that, so what a model thinks beyond it is not shown.
const MOST_THINKING: usize = 256 * 1024;

/// How much of a message is kept free for the word its model is in the middle of. An answer
/// longer than one message goes on in the next from the last word's end that leaves this
/// much, so a word begun in a message ends in it and every message ends where a word ends.
/// It bounds nothing an agent writes: it is where an answer turns to its next message.
const KEPT_FOR_A_WORD: usize = 1024;

/// Where words go, in Chat's own names: the conversation, and the message there, which is
/// the reply or a message its answer went on in.
#[derive(Clone)]
struct Place {
    conversation: String,
    reply: String,
}

/// One more piece of a reply's text on its way to Chat.
struct Piece {
    place: Place,
    /// Which message of the answer it goes into: none for the reply itself, else that
    /// message's number among those the answer went on in.
    on: Option<usize>,
    /// Its number among the events of that message's stream.
    sequence: u64,
    /// What stands before the words: [`BELOW`], or nothing.
    lead: &'static str,
    /// The blanks of the model's that stand before the words in its answer and in no
    /// message: a message the answer went on in begins with a word.
    passed: String,
    said: String,
}

/// What a model wrote that its reply does not hold yet. The thread the model is asked on
/// puts the words here as they arrive, and whoever follows the answer takes them.
#[derive(Default)]
pub(super) struct Writes {
    words: Mutex<String>,
    /// What the model thought, as its provider shows it, that the reply does not hold yet.
    thoughts: Mutex<String>,
    more: tokio::sync::Notify,
}

impl Writes {
    /// The model wrote `text`.
    pub(super) fn push(&self, text: &str) {
        self.words.lock_recover().push_str(text);
        self.more.notify_one();
    }

    /// The model thought `text`, and its provider shows it.
    pub(super) fn thought(&self, text: &str) {
        self.thoughts.lock_recover().push_str(text);
        self.more.notify_one();
    }

    /// What it thought since that was last taken.
    fn take_thoughts(&self) -> String {
        std::mem::take(&mut *self.thoughts.lock_recover())
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

/// How far a turn's reply has grown, and the messages its answer went on in.
pub(super) struct Written {
    /// None for a reply that does not grow.
    place: Option<Place>,
    /// Who writes the reply: Chat derives the message an answer goes on in from the full
    /// one's id and its author's ([`chat::chat::reply_id`]).
    agent: MemberId,
    grown: Mutex<Grown>,
}

/// A message an answer went on in after its reply.
struct Link {
    /// Its id, which Chat derives from the id of the message before it.
    reply: String,
    /// How many events of its stream Chat took.
    sequence: u64,
    /// Whether its stream took its end: it takes no more.
    ended: bool,
}

#[derive(Default)]
struct Grown {
    /// Whether Chat holds the reply as this turn's own, so that words may go into it.
    open: bool,
    /// How many events of the reply's stream Chat took.
    sequence: u64,
    /// The messages the answer went on in after the reply, in order.
    on: Vec<Link>,
    /// How many bytes of words the newest message of the answer holds.
    bytes: usize,
    /// What the call to the model that is under way put into the reply.
    call: String,
    /// How many bytes of thinking the reply holds.
    thought: usize,
    /// Whether the call under way put thinking into the reply.
    thinking: bool,
    /// Whether Chat refused a piece: no more goes in, and the reply may lack words.
    lost: bool,
}

impl Written {
    /// Where the reply of `turn` grows: nowhere for an issue's turn, whose answer is a
    /// comment. A sub-agent's grows in the conversation its item names, where its turns are.
    pub(super) fn of(turn: &Turn) -> Self {
        let place = reply_of(turn).map(|reply| Place {
            conversation: turn.agent.sub.as_ref().map_or_else(
                || conversation_id_of(&turn.conversation, &turn.person, &turn.agent.id.0),
                |sub| sub.conversation.clone(),
            ),
            reply,
        });
        Self {
            place,
            agent: MemberId::agent(&turn.agent.id.0),
            grown: Mutex::default(),
        }
    }

    /// Chat holds the reply as this turn's own from now on.
    pub(super) fn open(&self) {
        self.grown.lock_recover().open = true;
    }

    /// Take up what the reply holds already, as `held` keeps it: words a turn wrote into it
    /// on a server that stopped before that turn ended, and into the messages its answer went
    /// on in. What this turn writes stands below them, in the newest of those messages.
    pub(super) fn goes_on_from(&self, held: &chat::parts::ChatMessages) {
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
        let in_reply = reply.text.len();
        let newest = self.takes_up(place, held);
        let mut grown = self.grown.lock_recover();
        grown.open = true;
        grown.sequence = stream.sequence;
        grown.bytes = newest.unwrap_or(in_reply);
        // A message whose stream took its end takes no more: the answer goes on in the next.
        if grown.on.last().is_some_and(|link| link.ended) {
            self.turns(&mut grown, place);
        }
    }

    /// Take up the messages the reply's answer went on in, as `held` keeps them, for a turn
    /// that is ended without work: each of them ends with it.
    pub(super) fn follows_from(&self, held: &chat::parts::ChatMessages) {
        if let Some(place) = &self.place {
            self.takes_up(place, held);
        }
    }

    /// The id of the message an answer goes on in after the message `full`.
    fn after(&self, full: &str) -> String {
        chat::chat::reply_id(&tilo_resource::Id::from_raw(full), &self.agent).to_string()
    }

    /// Find the messages the answer went on in after the reply, as `held` keeps them: each by
    /// the id Chat derives from the one before it, written by this agent in this
    /// conversation, for as far as the part holds them. How many bytes of words the newest
    /// holds; none where the answer stands in the reply alone.
    fn takes_up(&self, place: &Place, held: &chat::parts::ChatMessages) -> Option<usize> {
        let mut on = Vec::new();
        let mut newest = None;
        let mut full = place.reply.clone();
        // Each step finds another message of the part, so the part's size ends the walk.
        for _ in held.messages.iter() {
            let next = self.after(&full);
            let Some(message) = held
                .messages
                .get(&tilo_resource::Id::from_raw(&next))
                .filter(|message| {
                    message.author == self.agent
                        && super::sub_agents::conversation_id(message).as_str()
                            == place.conversation
                })
            else {
                break;
            };
            let stream = message.work.as_ref().and_then(|work| work.stream.as_ref());
            on.push(Link {
                reply: next.clone(),
                sequence: stream.map_or(0, |stream| stream.sequence),
                ended: stream.is_some_and(|stream| stream.finish.is_some()),
            });
            newest = Some(message.text.len());
            full = next;
        }
        self.grown.lock_recover().on = on;
        newest
    }

    /// The newest message of the answer takes no more: the answer goes on in the agent's next
    /// message, by the id Chat derives for it from the full one's.
    fn goes_on(&self) {
        if let Some(place) = &self.place {
            self.turns(&mut self.grown.lock_recover(), place);
        }
    }

    /// Turn `grown` to the message after its newest.
    fn turns(&self, grown: &mut Grown, place: &Place) {
        let full = grown
            .on
            .last()
            .map_or(place.reply.as_str(), |link| link.reply.as_str());
        let next = self.after(full);
        grown.on.push(Link {
            reply: next,
            sequence: 0,
            ended: false,
        });
        grown.bytes = 0;
    }

    /// The model is asked anew: what it writes now stands below what the reply holds.
    fn asked_anew(&self) {
        let mut grown = self.grown.lock_recover();
        grown.call.clear();
        grown.thinking = false;
    }

    /// What of `more` the newest message of the answer takes at its end, as a piece, and
    /// what of it is left for the message the answer then goes on in ([`Self::goes_on`]).
    /// No piece where that message takes none of it; nothing at all where the reply does not
    /// grow or `more` is no words.
    fn next<'a>(&self, more: &'a str) -> Option<(Option<Piece>, &'a str)> {
        let place = self.place.as_ref()?;
        let grown = self.grown.lock_recover();
        if !grown.open || grown.lost {
            return None;
        }
        // A call's first words begin without the blanks a model may lead with, and below
        // whatever the reply holds. A message with nothing in it begins with a word as well,
        // also where the answer went on in it in the middle of a call.
        let (lead, words) = match (grown.call.is_empty(), grown.bytes) {
            (_, 0) => ("", more.trim_start()),
            (true, _) => (BELOW, more.trim_start()),
            (false, _) => ("", more),
        };
        if words.is_empty() {
            return None;
        }
        // The blanks left out inside a call are the model's all the same ([`Self::lacks`]).
        let passed = if grown.call.is_empty() {
            ""
        } else {
            more.strip_suffix(words).unwrap_or_default()
        };
        let left = chat::chat::MAX_MESSAGE_TEXT_BYTES
            .saturating_sub(grown.bytes)
            .saturating_sub(lead.len());
        let empty = grown.bytes == 0;
        // Whether the message ends inside a word that `words` goes on with.
        let within = !empty && lead.is_empty() && !words.starts_with(char::is_whitespace);
        let (on, reply, sequence) = match grown.on.last() {
            Some(link) => (
                grown.on.len().checked_sub(1),
                link.reply.clone(),
                link.sequence.saturating_add(1),
            ),
            None => (None, place.reply.clone(), grown.sequence.saturating_add(1)),
        };
        drop(grown);
        let (said, rest) = words.split_at_checked(taken(words, left, within || empty))?;
        let piece = (!said.is_empty()).then(|| Piece {
            place: Place {
                conversation: place.conversation.clone(),
                reply,
            },
            on,
            sequence,
            lead,
            passed: passed.to_owned(),
            said: said.to_owned(),
        });
        Some((piece, rest))
    }

    /// Chat took `piece`.
    fn took(&self, piece: &Piece) {
        let mut grown = self.grown.lock_recover();
        match piece.on {
            Some(at) => {
                if let Some(link) = grown.on.get_mut(at) {
                    link.sequence = piece.sequence;
                }
            }
            None => grown.sequence = piece.sequence,
        }
        grown.bytes = grown
            .bytes
            .saturating_add(piece.lead.len())
            .saturating_add(piece.said.len());
        grown.call.push_str(&piece.passed);
        grown.call.push_str(&piece.said);
    }

    /// Chat refused a piece.
    fn lost(&self) {
        self.grown.lock_recover().lost = true;
    }

    /// The piece that puts `more` at the end of the reply's thinking: none where the reply
    /// does not grow, where `more` is nothing, or where its thinking holds all Chat keeps of
    /// one reply's.
    fn next_thought(&self, more: &str) -> Option<Piece> {
        let place = self.place.as_ref()?;
        let grown = self.grown.lock_recover();
        if !grown.open || grown.lost {
            return None;
        }
        // A call's first thought begins without the blanks a model may lead with, and below
        // what the calls before it thought.
        let (lead, more) = match (grown.thinking, grown.thought) {
            (false, 0) => ("", more.trim_start()),
            (false, _) => (BELOW, more.trim_start()),
            (true, _) => ("", more),
        };
        let fits = MOST_THINKING
            .saturating_sub(grown.thought)
            .saturating_sub(lead.len());
        let sequence = grown.sequence.saturating_add(1);
        drop(grown);
        let said = beginning(more, fits);
        (!said.is_empty()).then(|| Piece {
            place: place.clone(),
            on: None,
            sequence,
            lead,
            passed: String::new(),
            said: said.to_owned(),
        })
    }

    /// Chat took `piece` into the reply's thinking.
    fn thought_taken(&self, piece: &Piece) {
        let mut grown = self.grown.lock_recover();
        grown.sequence = piece.sequence;
        grown.thought = grown
            .thought
            .saturating_add(piece.lead.len())
            .saturating_add(piece.said.len());
        grown.thinking = true;
    }

    /// What of `whole`, the words of the model's answer to the call that just ended, the
    /// reply lacks: what its provider said only at the end. None for a reply nothing was
    /// written into, not even thinking, whose words its turn's end says, and none where the
    /// answer does not begin with what the reply was given, which stays as its person read it.
    fn lacks(&self, whole: &str) -> Option<String> {
        let grown = self.grown.lock_recover();
        // A reply whose stream holds thinking and no word yet has its words still to come:
        // they go into it here, or its stream would end without them.
        if grown.call.is_empty() && grown.bytes == 0 && grown.thought == 0 && grown.on.is_empty() {
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

    /// Where the end of each message the answer went on in goes, with that message's number
    /// among them and the end's among its stream's events: the newest first, and none for a
    /// message whose stream took its end or took nothing.
    fn went_on(&self) -> Vec<(usize, Place, u64)> {
        let Some(place) = &self.place else {
            return Vec::new();
        };
        let grown = self.grown.lock_recover();
        grown
            .on
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, link)| !link.ended && link.sequence > 0)
            .map(|(at, link)| {
                let place = Place {
                    conversation: place.conversation.clone(),
                    reply: link.reply.clone(),
                };
                (at, place, link.sequence.saturating_add(1))
            })
            .collect()
    }

    /// The stream of the message numbered `at` among those the answer went on in took its
    /// end.
    fn ended(&self, at: usize) {
        if let Some(link) = self.grown.lock_recover().on.get_mut(at) {
            link.ended = true;
        }
    }

    /// Whether `words`, an answer that arrived whole, go into the reply as words written
    /// while they came: nothing was written into it yet, and they are more than one message
    /// holds, so the turn's end cannot say them in the reply alone.
    fn takes_whole(&self, words: &str) -> bool {
        let grown = self.grown.lock_recover();
        self.place.is_some()
            && grown.open
            && !grown.lost
            && grown.sequence == 0
            && grown.on.is_empty()
            && words.len() > chat::chat::MAX_MESSAGE_TEXT_BYTES
    }

    /// Whether the answer holds words and may lack some, because Chat refused a piece.
    pub(super) fn lacks_words(&self) -> bool {
        let grown = self.grown.lock_recover();
        grown.lost && (grown.bytes > 0 || !grown.on.is_empty())
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

/// Where each word of `words` ends: every place a blank follows what is none.
fn word_ends(words: &str) -> impl Iterator<Item = usize> + '_ {
    let mut in_word = false;
    words.char_indices().filter_map(move |(at, letter)| {
        let ends = in_word && letter.is_whitespace();
        in_word = !letter.is_whitespace();
        ends.then_some(at)
    })
}

/// How many bytes of `words`, the next of an answer, a message takes that has `left` bytes
/// to its bound. All of them where that leaves it [`KEPT_FOR_A_WORD`]; else up to the last
/// word's end that does, and the answer goes on in the next message from there. A message
/// that takes none that way still takes its word's end where it `must`, because it ends
/// inside that word or holds nothing yet: then what is kept for a word is used, and a word
/// longer than the message has left is cut where the message is full.
fn taken(words: &str, left: usize, must: bool) -> usize {
    let free = left.saturating_sub(KEPT_FOR_A_WORD);
    if words.len() <= free {
        return words.len();
    }
    if let Some(end) = word_ends(words).take_while(|end| *end <= free).last() {
        return end;
    }
    if !must {
        return 0;
    }
    match word_ends(words).next() {
        Some(end) if end <= left => end,
        None if words.len() <= left => words.len(),
        _ => beginning(words, left).len(),
    }
}

impl Reply {
    /// Put `more`, which the turn's model wrote just now, at the end of the answer: into the
    /// reply, and where that holds all a message takes, on into the agent's next message, so
    /// that every word stands once and in order.
    pub(super) async fn wrote(&self, more: &str) {
        let mut more = more;
        // Whether the answer just went on in a message that then took nothing.
        let mut idle = false;
        while let Some((piece, rest)) = self.written.next(more) {
            if let Some(piece) = piece {
                let text = format!("{}{}", piece.lead, piece.said);
                let event = AgentOutput::TextDelta { text };
                if !self.stream(&piece.place, piece.sequence, event).await {
                    self.written.lost();
                    return;
                }
                self.written.took(&piece);
                idle = false;
            } else if idle {
                // A message with nothing in it takes words, so this is not reached.
                self.written.lost();
                return;
            } else {
                idle = true;
            }
            if rest.is_empty() {
                return;
            }
            self.written.goes_on();
            more = rest;
        }
    }

    /// Put `more`, which the turn's model thought just now and its provider showed, at the
    /// end of the reply's thinking: beside its words, never among them.
    pub(super) async fn thought(&self, more: &str) {
        let Some(piece) = self.written.next_thought(more) else {
            return;
        };
        let text = format!("{}{}", piece.lead, piece.said);
        if self
            .stream(
                &piece.place,
                piece.sequence,
                AgentOutput::ReasoningDelta { text },
            )
            .await
        {
            self.written.thought_taken(&piece);
        } else {
            self.written.lost();
        }
    }

    /// End a reply that grew, as `outcome`: one that ended before its answer says why below
    /// the words that came, and its stream ends as the turn did. Whether it was such a reply
    /// and Chat took its end; where not, its end is written as a reply's that did not grow.
    pub(super) async fn grown_ends(&self, outcome: &Outcome) -> bool {
        // An answer that arrived whole and is longer than one message goes in as one that
        // was written while it came.
        if let Outcome::Answered(words) = outcome
            && self.written.takes_whole(words)
        {
            self.written.asked_anew();
            self.wrote(words).await;
        }
        let (finish, why) = match outcome {
            Outcome::Answered(_) => (AgentFinish::Complete, ""),
            Outcome::Stopped(words) => (AgentFinish::Truncated, words.as_str()),
            Outcome::Failed(words) => (AgentFinish::Failed, words.as_str()),
            Outcome::Cancelled => (AgentFinish::Cancelled, ""),
        };
        if self.written.end().is_some() {
            self.written.asked_anew();
            self.wrote(why).await;
        }
        let ended = || AgentOutput::Finished {
            outcome: finish,
            detail: String::new(),
        };
        // Every message the answer went on in ends as the turn does, the newest first and
        // the reply last. The reply keeps the turn's end: a turn is over only when no message
        // of its answer says it goes on, and a server that stops in between finds the turn
        // open and ends the rest.
        for (at, place, sequence) in self.written.went_on() {
            if self.stream(&place, sequence, ended()).await {
                self.written.ended(at);
            }
        }
        let Some((place, sequence)) = self.written.end() else {
            return false;
        };
        self.stream(&place, sequence, ended()).await
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
            &collab_types::document::chain_resource_id(&chat::parts::chain()),
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

    /// Put `more`, which the model thought just now, at the end of its thinking.
    fn think(&self, more: &str) -> impl std::future::Future<Output = ()> + Send;

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

    async fn think(&self, more: &str) {
        self.thought(more).await;
    }

    fn lacks(&self, whole: &str) -> Option<String> {
        self.written.lacks(whole)
    }
}

/// Wait for `answer`, a model's answer on its way, and put what the model writes meanwhile
/// (`writes`) into `reply`, each piece the moment it comes: what arrives while a piece is on
/// its way goes with the next. Words go in whole: a blank a model wrote last waits for the
/// word after it, and is left out at the answer's end. What of the answer's words the provider
/// said only at the end is put in when the answer is there ([`Takes::lacks`]).
pub(super) async fn follow<R>(
    reply: &(impl Takes + Sync),
    answer: impl std::future::Future<Output = R>,
    writes: &Writes,
) -> R {
    let mut answer = std::pin::pin!(answer);
    reply.asked_anew();
    let mut words = String::new();
    let mut thoughts = String::new();
    let answer = loop {
        tokio::select! {
            answer = &mut answer => break answer,
            () = writes.more.notified() => {
                // What the model thought stands in the reply before what it then wrote, and
                // goes the moment it comes, as its words do.
                thoughts.push_str(&writes.take_thoughts());
                if !thoughts.trim_end().is_empty() {
                    let rest = thoughts.split_off(thoughts.trim_end().len());
                    reply.think(&thoughts).await;
                    thoughts = rest;
                }
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
    thoughts.push_str(&writes.take_thoughts());
    reply.think(thoughts.trim_end()).await;
    words.push_str(&writes.take());
    reply.take(settled(&words).trim_end()).await;
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
        thoughts: Mutex<Vec<(u128, String)>>,
    }

    impl Kept {
        fn new(costs: Duration) -> Self {
            Self {
                began: tokio::time::Instant::now(),
                costs,
                pieces: Mutex::default(),
                thoughts: Mutex::default(),
            }
        }

        /// What it was given, each piece with the millisecond it came.
        fn pieces(&self) -> Vec<(u128, String)> {
            self.pieces.lock_recover().clone()
        }

        /// What it was given as thinking, each piece with the millisecond it came.
        fn thoughts(&self) -> Vec<(u128, String)> {
            self.thoughts.lock_recover().clone()
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

        async fn think(&self, more: &str) {
            if more.is_empty() {
                return;
            }
            self.thoughts
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
        let answered = follow(&kept, model, &writes).await;
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
        let answered = follow(&kept, model, &writes).await;
        assert!(matches!(answered, Ok(Ok(()))), "the model answered");
        assert_eq!(
            kept.pieces(),
            [(0, "One"), (30, " two three four.")].map(|(at, said)| (at, said.to_owned())),
            "the pieces that came during the first write go together, when it has ended"
        );
    }

    /// A model that thinks each of `thoughts` after its wait in milliseconds, then writes each
    /// of `pieces` likewise, and answers a tenth of a second after the last.
    fn thinking(
        writes: &Arc<Writes>,
        thoughts: &'static [(u64, &'static str)],
        pieces: &'static [(u64, &'static str)],
    ) -> tokio::task::JoinHandle<Result<(), ()>> {
        let writes = Arc::clone(writes);
        tokio::spawn(async move {
            for (after, thought) in thoughts {
                tokio::time::sleep(Duration::from_millis(*after)).await;
                writes.thought(thought);
            }
            for (after, piece) in pieces {
                tokio::time::sleep(Duration::from_millis(*after)).await;
                writes.push(piece);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        })
    }

    /// What a model thinks goes into its reply's thinking the millisecond it comes, before
    /// the words it then writes, and never among them.
    #[tokio::test(start_paused = true)]
    async fn a_thought_is_shown_when_it_arrives_and_never_among_the_words() {
        let writes = Arc::new(Writes::default());
        let kept = Kept::new(Duration::from_millis(2));
        let model = thinking(
            &writes,
            &[(0, "The plan names "), (10, "one month: "), (10, "May.")],
            &[(50, "The launch "), (10, "is in May.")],
        );
        let answered = follow(&kept, model, &writes).await;
        assert!(matches!(answered, Ok(Ok(()))), "the model answered");
        assert_eq!(
            kept.thoughts(),
            [(0, "The plan names"), (10, " one month:"), (20, " May.")]
                .map(|(at, said)| (at, said.to_owned())),
            "each thought stands in the reply's thinking the millisecond it came"
        );
        assert_eq!(
            kept.pieces(),
            [(70, "The launch"), (80, " is in May.")].map(|(at, said)| (at, said.to_owned())),
            "the words stand in the reply, and no thought among them"
        );
    }

    /// A reply that grows, as a turn's reply to a message does, with nothing in it yet.
    fn growing() -> Written {
        let written = Written {
            place: Some(Place {
                conversation: "conversation".to_owned(),
                reply: "reply".to_owned(),
            }),
            agent: MemberId::agent("scout"),
            grown: Mutex::default(),
        };
        written.open();
        written
    }

    /// A reply that holds thinking and no word yet lacks the whole of an answer that came at
    /// once: its words go into it, and its stream does not end without them.
    #[test]
    fn a_reply_that_only_thought_lacks_the_words_of_an_answer_that_came_whole() {
        let written = growing();
        assert_eq!(
            written.lacks("The launch is in May."),
            None,
            "nothing was written into it: its turn's end says its words"
        );
        let thought = written
            .next_thought("The plan names May.")
            .expect("a reply that grows takes a thought");
        written.thought_taken(&thought);
        assert_eq!(
            written.lacks("The launch is in May.").as_deref(),
            Some("The launch is in May."),
            "it thought and holds no word: the answer's words are still to go into it"
        );
        assert_eq!(
            written.end().map(|(_, sequence)| sequence),
            Some(2),
            "its stream ends after the thought"
        );
    }

    /// What a later call to the model thinks stands a paragraph below what the first thought,
    /// and more of one call's thinking follows on as it came.
    #[test]
    fn what_a_later_call_thinks_stands_below_what_the_first_thought() {
        let written = growing();
        let first = written
            .next_thought("  The plan names May.")
            .expect("the first thought");
        assert_eq!(
            (first.lead, first.said.as_str()),
            ("", "The plan names May.")
        );
        written.thought_taken(&first);
        let more = written
            .next_thought(" It says so twice.")
            .expect("more of the same call's thinking");
        assert_eq!((more.lead, more.said.as_str()), ("", " It says so twice."));
        written.thought_taken(&more);
        written.asked_anew();
        let later = written
            .next_thought("The tool agrees.")
            .expect("the next call's thought");
        assert_eq!(
            (later.lead, later.said.as_str()),
            (BELOW, "The tool agrees.")
        );
    }

    /// A reply is given no more thinking than Chat keeps of one: a thought is cut there and
    /// nothing more is handed on, so no piece is refused for a reply's thinking.
    #[test]
    fn a_reply_is_given_no_more_thinking_than_chat_keeps_of_one() {
        let written = growing();
        let most = written
            .next_thought(&"a".repeat(MOST_THINKING.saturating_add(9)))
            .expect("a reply that grows takes a thought");
        assert_eq!(
            most.said.len(),
            MOST_THINKING,
            "a thought is cut where Chat keeps no more"
        );
        written.thought_taken(&most);
        assert!(
            written.next_thought("more").is_none(),
            "nothing more is handed to Chat"
        );
    }

    /// Put `words` into `written` as a turn's reply does, Chat taking every piece: the
    /// message each piece went into, and the piece.
    fn put(written: &Written, words: &str) -> Vec<(String, String)> {
        let mut pieces = Vec::new();
        let mut more = words;
        while let Some((piece, rest)) = written.next(more) {
            if let Some(piece) = piece {
                pieces.push((
                    piece.place.reply.clone(),
                    format!("{}{}", piece.lead, piece.said),
                ));
                written.took(&piece);
            }
            if rest.is_empty() {
                break;
            }
            written.goes_on();
            more = rest;
        }
        pieces
    }

    /// The id Chat derives for the message that follows `full` in an answer of the agent.
    fn after(full: &str) -> String {
        chat::chat::reply_id(
            &tilo_resource::Id::from_raw(full),
            &MemberId::agent("scout"),
        )
        .to_string()
    }

    /// An answer longer than one message goes on in the agent's next message, by the id Chat
    /// derives for it: every word once and in order, each message within what a message
    /// holds, and each ending where a word ends. What a message keeps free for a word is
    /// where one message ends, never where an answer ends: every word still arrives.
    #[test]
    fn an_answer_longer_than_one_message_goes_on_in_the_next_where_a_word_ends() {
        let written = growing();
        let words: Vec<String> = (0..15_000).map(|n| format!("word{n}")).collect();
        let answer = words.join(" ");
        assert!(
            answer.len() > chat::chat::MAX_MESSAGE_TEXT_BYTES.saturating_mul(2),
            "the answer is longer than two messages hold"
        );
        let pieces = put(&written, &answer);
        let (second, third) = (after("reply"), after(&after("reply")));
        assert_eq!(
            pieces
                .iter()
                .map(|(into, _)| into.as_str())
                .collect::<Vec<_>>(),
            ["reply", second.as_str(), third.as_str()],
            "one piece into the reply and one into each message the answer went on in"
        );
        for (into, said) in &pieces {
            assert!(
                said.len() <= chat::chat::MAX_MESSAGE_TEXT_BYTES && said.trim() == said,
                "{into} holds no more than a message does, and begins and ends with a word"
            );
        }
        let stands: Vec<&str> = pieces
            .iter()
            .flat_map(|(_, said)| said.split(' '))
            .collect();
        assert_eq!(stands, words, "every word stands once and in order");
    }

    /// A word the model is in the middle of ends in the message it began in, in what a
    /// message keeps free for that, and the answer goes on in the next message with the word
    /// after it: the blank between them stands in neither, and is the model's all the same.
    #[test]
    fn a_word_begun_in_a_message_ends_in_it() {
        let written = growing();
        let free = chat::chat::MAX_MESSAGE_TEXT_BYTES.saturating_sub(KEPT_FOR_A_WORD);
        let begun = format!("{} rep", "a".repeat(free.saturating_sub(4)));
        assert_eq!(begun.len(), free);
        let mut pieces = put(&written, &begun);
        pieces.extend(put(&written, "ort: nothing is left out"));
        let next = after("reply");
        assert_eq!(
            pieces,
            [
                ("reply", begun.as_str()),
                ("reply", "ort:"),
                (next.as_str(), "nothing is left out"),
            ]
            .map(|(into, said)| (into.to_owned(), said.to_owned())),
            "the word ends where it began, and the next message begins with a word"
        );
        assert_eq!(
            written
                .lacks(&format!(
                    "{begun}ort: nothing is left out, and nothing twice."
                ))
                .as_deref(),
            Some(", and nothing twice."),
            "what the provider said only at the end is what the messages lack"
        );
    }
}

/// The conversation a turn writes in: the one the turn is in, or for a turn an issue caused the
/// person's own conversation with the agent.
fn conversation_id_of(conversation: &str, person: &str, agent: &str) -> String {
    // A conversation's address as Chat itself writes it, up to the conversation's own id.
    let conversation_ids =
        chat::summons::conversation_address(&tilo_resource::Id::from_raw(String::new()));
    conversation
        .strip_prefix(&conversation_ids)
        .filter(|conversation_id| !conversation_id.is_empty())
        .map_or_else(
            || {
                chat::Chat::direct_id([MemberId::person(person), MemberId::agent(agent)])
                    .to_string()
            },
            str::to_owned,
        )
}
