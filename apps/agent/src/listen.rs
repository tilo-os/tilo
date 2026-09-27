//! Whether something in the room is being said *to* the agent.
//!
//! The agent recognizes explicit paragraph names and structured mentions.
//! Reading every room message and inferring intent would make ordinary
//! conversation mutate documents unpredictably, so this module separates
//! proximity from addressing.
//!
//! ## Close, and addressed
//!
//! **Proximity** decides where the agent *looks*. **Being addressed** decides
//! whether it *acts*. They are not the same test and the second is never
//! weakened by the first:
//!
//! * An [`Address::Mentioned`] — an `@mention` that resolved to the agent's own
//!   participant id. Read from the structured list [`chat::mention`] mints at
//!   send, never by scanning the body for `@`: the room already decided which
//!   "Ada" was meant, and re-deciding it here would be a second, disagreeing
//!   answer.
//! * An [`Address::Named`] — the agent's name opening the text, `Claude: …`.
//!   The rule is [`plan::Instruction::addressed`](crate::plan::Instruction::addressed),
//!   the same function the document path uses, so a name means the same thing
//!   in a message as it does in a paragraph.
//!
//! Nothing else is an address. A message that merely happens to be near the
//! agent's work says nothing to it, and [`next_summons`] will not return it.
//!
//! ## What proximity is actually for
//!
//! It supplies the **subject** of a request that does not state one. "@Claude
//! do that again" names an operation and no resource; the honest readings are
//! "the thing I was just working on" and "I don't know". Which one applies is
//! a question about recency, so that is what [`Subject::Recent`] is:
//!
//! * The message carries an address → [`Subject::Carried`]. That address is
//!   the subject, whatever the prose says. This is the whole point of
//!   [`tilo_reference`]: the agent never has to work out what "this" meant.
//! * It carries none, and the agent acted on something within
//!   [`RECENCY_SECS`] → [`Subject::Recent`], the resource of that last action.
//! * Neither → [`Subject::Nothing`], and an instruction that needed a subject
//!   is refused in the channel rather than aimed at whatever is nearest.
//!
//! The window is short on purpose. Without one, every resource the agent has
//! ever touched stays "close" forever, and the inference stops meaning
//! anything the moment it would be most dangerous — a stale id that still
//! resolves is exactly the failure the dispatch gate exists to prevent.
//!
//! ## Saying so, once
//!
//! Every summons is answered in the channel it came from, including the ones
//! the agent declines — a person who mentions it and sees nothing cannot tell
//! refusal from a crash from never having been heard. That answer is also how
//! the agent knows not to act twice: [`answered`] reads the replies already in
//! the room and treats their parents as done. The record lives in the shared
//! document rather than in this process, so it survives a restart, a reload,
//! and a second copy of the agent joining the same room.

use std::collections::HashSet;

use chat::model::Message;
use tilo_reference::ResourceRef;

use crate::plan::Instruction;

/// How long the agent's last action stays close enough to be the unstated
/// subject of a request.
///
/// Three minutes: long enough to cover "…actually, make it a table too" while
/// the person is still looking at what the agent just did, short enough that
/// coming back after a meeting and typing "@Claude do it" does not silently
/// aim at something from before lunch. The boundary matters more than the
/// exact number — what must not exist is *no* boundary.
pub const RECENCY_SECS: i64 = 180;

/// How a message came to be addressed to the agent.
///
/// There are two ways and there is deliberately no third. Neither is inferred
/// from tone, topic or timing: both are things the author did on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    /// An `@mention` resolved to the agent's participant id when the message
    /// was sent. The strongest signal there is — the author picked this
    /// participant out of the room.
    Mentioned,
    /// The agent's name opens the text: `Claude: turn the bullets into a
    /// table`. The rule the document path has always used.
    Named,
}

impl Address {
    pub fn label(&self) -> &'static str {
        match self {
            Address::Mentioned => "mentioned",
            Address::Named => "named",
        }
    }
}

/// What a request is about, and how that was established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// The message carried the address of what it is about. Authoritative:
    /// the author selected the thing, and the app that owned the selection
    /// minted the id.
    Carried(ResourceRef),
    /// The message stated no subject, and the agent acted on this resource
    /// recently enough for it to be the obvious one. `age` is how long ago,
    /// in seconds, so a caller can say why it thought so.
    Recent { resource: String, age: i64 },
    /// Nothing said what this is about and nothing recent enough to stand in.
    /// An instruction that needs a subject must be refused, not aimed.
    Nothing,
}

impl Subject {
    /// The resource id to act on, or `None` when there is nothing to go on.
    ///
    /// A [`Subject::Carried`] answers with the reference's own id — the
    /// address, never [`ResourceRef::label`](tilo_reference::ResourceRef), which
    /// is the author's wording at the moment of writing and resolves nothing.
    pub fn resource(&self) -> Option<&str> {
        match self {
            Subject::Carried(r) => Some(r.id.as_str()),
            Subject::Recent { resource, .. } => Some(resource.as_str()),
            Subject::Nothing => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Subject::Carried(_) => "carried",
            Subject::Recent { .. } => "recent",
            Subject::Nothing => "none",
        }
    }
}

/// The last thing the agent did, and when.
///
/// Kept as a resource id rather than an app name because "same" has to be
/// answerable exactly: two documents open in one app are not the same subject,
/// and an app name cannot tell them apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastAction {
    /// The resource half of the address the agent last performed.
    pub resource: String,
    /// When, in the same epoch seconds a message's `ts` is in.
    pub at: i64,
}

impl LastAction {
    pub fn new(resource: impl Into<String>, at: i64) -> Self {
        Self {
            resource: resource.into(),
            at,
        }
    }

    /// Whether `ts` is close enough to this action for it to be the unstated
    /// subject.
    ///
    /// Distance, not order: a message written *before* the action is as far
    /// from it as one written the same time after, and neither is what
    /// "carry on from what you just did" means. Taking the absolute
    /// difference is what stops a clock skewed the wrong way from making
    /// every message in the room eligible.
    pub fn within(&self, ts: i64, window: i64) -> bool {
        (ts - self.at).abs() <= window
    }
}

/// Something in the room that is addressed to the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summons {
    /// The message it came from. The idempotence key, and the parent of the
    /// reply that records it as handled.
    pub message: String,
    /// The channel to answer in.
    pub channel: String,
    /// Who asked.
    pub author: String,
    /// When, in epoch seconds.
    pub ts: i64,
    /// What was asked, with the addressing token removed — the words a
    /// decision is made from.
    pub asked: String,
    /// How it came to be addressed to the agent.
    pub address: Address,
    /// What it is about.
    pub subject: Subject,
}

/// How a message addresses the agent, or `None` when it does not.
///
/// `me` is the agent's participant id as the room knows it; `name` is its
/// display name. The two are checked against different things on purpose: a
/// mention carries an id the room resolved, while a name is what somebody
/// typed. An id match is not spoofable by writing a name into a body, and that
/// asymmetry is why the id is tried first.
pub fn addressed(message: &Message, me: &str, name: &str) -> Option<Address> {
    if !me.is_empty() && message.mentions_user(me) {
        return Some(Address::Mentioned);
    }
    Instruction::addressed(&message.text, name).map(|_| Address::Named)
}

/// The request, with whatever addressed it taken off the front.
///
/// A mention is cut out by the byte range the room recorded for it, not by
/// searching the text for the name again — the offsets and the resolution were
/// minted together and only they are guaranteed to agree.
pub fn asked_of(message: &Message, me: &str, name: &str, address: Address) -> String {
    match address {
        Address::Named => Instruction::addressed(&message.text, name).unwrap_or_default(),
        Address::Mentioned => {
            let text = message.text.as_str();
            let mut out = String::with_capacity(text.len());
            let mut cut = 0usize;
            for m in &message.mentions {
                if m.user != me || m.start < cut {
                    continue;
                }
                // Every slice goes through `get`. These offsets came out of a
                // shared document that anything in the room may have written,
                // and a pair that does not land on character boundaries would
                // otherwise take the agent down rather than cost it one
                // mention. A run that will not slice is left in the text: the
                // request reads slightly oddly, which is recoverable, and the
                // agent stays up, which is the point.
                let Some(head) = text.get(cut..m.start) else {
                    continue;
                };
                if text.get(m.end()..).is_none() {
                    continue;
                }
                out.push_str(head);
                cut = m.end();
            }
            out.push_str(text.get(cut..).unwrap_or_default());
            // A body that was only a mention leaves nothing; `trim` is what
            // makes "@Claude   crop this" and "crop this" the same request.
            out.trim().trim_start_matches(':').trim().to_string()
        }
    }
}

/// The instructions this room already has an answer to.
///
/// Read out of the shared document rather than remembered in this process,
/// which is the only version of this that survives what actually happens: the
/// agent is restarted, its document is reloaded from disk, or a second copy
/// joins the same room. An in-memory set answers "no" to all three and the
/// edit is made twice.
///
/// A reply is the agent's own message whose `parent` is the message it
/// answers — including the replies that say it is *not* going to act, because
/// a refusal is an answer and re-deciding it would produce a second one.
///
/// It also includes a reply the agent posted when it *took* the summons and
/// has not finished. That is deliberate, and it is the conservative reading:
/// an agent that stopped halfway may already have changed a document, and a
/// restart that treated an unfinished record as untouched work would apply
/// the same operation twice. What the room shows in that case is where the
/// agent stopped, which is true, rather than a second attempt at it.
///
/// The one state that is **not** an answer is
/// [`AgentWorkState::Waiting`](chat::model::AgentWorkState::Waiting). Nothing
/// has been dispatched, nothing has changed, and the record exists precisely
/// to say that there is no answer yet — treating it as one would make a wait
/// permanent the moment it became visible.
pub fn answered(messages: &[Message], me: &str) -> HashSet<String> {
    messages
        .iter()
        .filter(|m| m.author == me)
        .filter(|m| !waiting(m))
        .filter_map(|m| m.parent.clone())
        .collect()
}

/// Whether `m` is a record that says the agent has not started.
fn waiting(m: &Message) -> bool {
    matches!(
        m.work.as_ref().map(|w| &w.state),
        Some(chat::model::AgentWorkState::Waiting { .. })
    )
}

/// The agent's own reply standing under `parent`, if it has already posted
/// one.
///
/// What [`answered`] finds, but the other way round: the id of the message to
/// go on writing into rather than the set of questions to skip. Without it an
/// agent that answers a summons over several turns — a wait, then the work —
/// leaves one message per turn under the same question.
pub fn record_of<'a>(messages: &'a [Message], me: &str, parent: &str) -> Option<&'a Message> {
    messages
        .iter()
        .rfind(|m| m.author == me && m.parent.as_deref() == Some(parent))
}

/// The next thing in the room addressed to the agent that it has not answered.
///
/// One at a time, oldest first: a person who says two things expects them done
/// in the order they said them, and a run that fans out over everything
/// unanswered is a run nobody can follow.
///
/// `now` and every `ts` are epoch seconds. `handled` is what
/// [`answered`] found, plus anything the caller knows it has in flight.
pub fn next_summons<S: std::hash::BuildHasher>(
    messages: &[Message],
    me: &str,
    name: &str,
    last: Option<&LastAction>,
    now: i64,
    handled: &HashSet<String, S>,
) -> Option<Summons> {
    let mut ordered: Vec<&Message> = messages.iter().collect();
    ordered.sort_by_key(|m| m.ts);
    for m in ordered {
        if handled.contains(&m.id) {
            continue;
        }
        // The agent's own words are not instructions to it. Without this a
        // refusal it posted would address it right back, forever.
        if m.author == me {
            continue;
        }
        let Some(address) = addressed(m, me, name) else {
            continue;
        };
        return Some(Summons {
            message: m.id.clone(),
            channel: m.channel.clone(),
            author: m.author.clone(),
            ts: m.ts,
            asked: asked_of(m, me, name, address),
            address,
            subject: subject_of(m, last, now),
        });
    }
    None
}

/// What a message is about: what it carried, else what the agent was just
/// doing, else nothing.
fn subject_of(message: &Message, last: Option<&LastAction>, _now: i64) -> Subject {
    if let Some(r) = &message.reference {
        return Subject::Carried(r.clone());
    }
    match last {
        // Measured between the message and the action, not from `now`. A
        // message written while the agent was working is close to that work
        // however long the agent then took to read it, and measuring from
        // `now` would let a slow queue turn a timely follow-up into an
        // expired one — the reading changing because the machine was busy is
        // exactly the sort of answer nobody can reason about.
        //
        // One boundary, so there is one thing to state and one thing to test.
        Some(l) if l.within(message.ts, RECENCY_SECS) => Subject::Recent {
            resource: l.resource.clone(),
            age: (message.ts - l.at).abs(),
        },
        _ => Subject::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat::mention::Mention;

    const ME: &str = "agent_claude";
    const NAME: &str = "Claude";

    fn msg(id: &str, author: &str, text: &str, ts: i64) -> Message {
        Message {
            id: id.into(),
            channel: "c_general".into(),
            author: author.into(),
            ts,
            text: text.into(),
            body: vec![],
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![],
            mentions: vec![],
            reference: None,
            attachment: None,
            pinned: false,
            work: None,
        }
    }

    /// `@Claude` as the room resolves it: the run, and the id behind it.
    fn mentioning(mut m: Message) -> Message {
        let at = m.text.find('@').expect("a mention in the fixture");
        let len = m.text[at..].find(' ').unwrap_or(m.text.len() - at);
        m.mentions = vec![Mention {
            user: ME.into(),
            start: at,
            len,
            is_agent: true,
        }];
        m
    }

    #[test]
    fn a_message_addresses_the_agent_by_mention_or_name_and_the_request_drops_the_address() {
        // Case `a_mention_of_the_agent_addresses_it_and_a_mention_of_someone_else_does_not`.
        {
            let mine = mentioning(msg("m1", "u_ada", "@Claude crop this", 100));
            assert_eq!(addressed(&mine, ME, NAME), Some(Address::Mentioned));

            let mut theirs = mentioning(msg("m2", "u_ada", "@Ada crop this", 100));
            theirs.mentions[0].user = "u_ada".into();
            assert_eq!(addressed(&theirs, ME, NAME), None);
        }

        // Case `the_name_rule_is_the_documents_rule`.
        {
            let named = msg("m1", "u_ada", "Claude: make a table", 100);
            assert_eq!(addressed(&named, ME, NAME), Some(Address::Named));
            // Merely containing the name is not addressing it — the same rule the
            // document path applies, so the two cannot disagree.
            let about = msg("m2", "u_ada", "I asked Claude about it", 100);
            assert_eq!(addressed(&about, ME, NAME), None);
        }

        // Case `the_request_is_the_body_without_the_token_that_addressed_it`.
        {
            let m = mentioning(msg("m1", "u_ada", "@Claude crop this square", 100));
            assert_eq!(
                asked_of(&m, ME, NAME, Address::Mentioned),
                "crop this square"
            );
            let n = msg("m2", "u_ada", "Claude: crop this square", 100);
            assert_eq!(asked_of(&n, ME, NAME, Address::Named), "crop this square");
        }
    }

    #[test]
    fn the_subject_is_the_carried_address_or_a_recent_last_action_and_nothing_else() {
        // Case `a_carried_address_beats_anything_recent`.
        {
            let mut m = mentioning(msg("m1", "u_ada", "@Claude crop this", 100));
            m.reference = Some(ResourceRef::whole(
                "files.entry",
                "files.entry:/var/workspace/harbour.png",
                "harbour.png",
            ));
            let last = LastAction::new("word-processor.block:blk_1", 99);
            let s = subject_of(&m, Some(&last), 100);
            assert_eq!(
                s.resource(),
                Some("files.entry:/var/workspace/harbour.png"),
                "the address the author selected, not the one the agent last touched"
            );
        }

        // Case `recency_bounds_the_inferred_subject_on_both_sides`.
        {
            let m = mentioning(msg("m1", "u_ada", "@Claude do that again", 1_000));
            let near = LastAction::new("word-processor.block:blk_1", 1_000 - RECENCY_SECS + 1);
            assert_eq!(
                subject_of(&m, Some(&near), 1_000).resource(),
                Some("word-processor.block:blk_1")
            );
            let far = LastAction::new("word-processor.block:blk_1", 1_000 - RECENCY_SECS - 1);
            assert_eq!(
                subject_of(&m, Some(&far), 1_000),
                Subject::Nothing,
                "past the window there is nothing to infer from, and guessing is the bug"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn the_next_summons_is_the_oldest_addressed_message_the_agent_has_not_answered() {
        // Case `a_reply_the_agent_already_wrote_marks_its_parent_answered`.
        {
            let mut reply = msg("m2", ME, "Claude: I can't do that here.", 101);
            reply.parent = Some("m1".into());
            let done = answered(&[msg("m1", "u_ada", "@Claude x", 100), reply], ME);
            assert!(done.contains("m1"), "a refusal is an answer, not a retry");
        }

        // Case `a_summons_the_agent_took_but_has_not_finished_still_counts_as_answered`.
        // The conservative half of `answered`: a summons the agent took and has
        // not finished still counts as spoken for, so a restart does not repeat
        // work that may already have landed.
        {
            let mut reply = msg("m2", ME, "", 101);
            reply.parent = Some("m1".into());
            reply.work = Some(chat::model::AgentWork::new(
                chat::model::AgentWorkState::Taken,
                101,
            ));
            let done = answered(&[msg("m1", "u_ada", "@Claude x", 100), reply], ME);
            assert!(
                done.contains("m1"),
                "an in-flight record is not untouched work"
            );
        }

        // Case `a_wait_is_not_an_answer_and_the_summons_stays_open`.
        // The one state that is not an answer: nothing has been dispatched and
        // the record exists to say a wait is in progress, not that one happened.
        {
            let mut reply = msg("m2", ME, "", 101);
            reply.parent = Some("m1".into());
            reply.work = Some(chat::model::AgentWork::new(
                chat::model::AgentWorkState::Waiting {
                    app: "Photos".into(),
                },
                101,
            ));
            let done = answered(&[msg("m1", "u_ada", "@Claude x", 100), reply], ME);
            assert!(
                !done.contains("m1"),
                "a wait must not permanently close the summons it belongs to"
            );
        }

        // Case `record_of_finds_the_agents_own_reply_and_nobody_elses`.
        // `record_of` finds the agent's own reply under a parent, the other way
        // round from `answered` — the id to keep writing into rather than the
        // set of questions to skip.
        {
            let mut reply = msg("m2", ME, "Claude: done.", 101);
            reply.parent = Some("m1".into());
            let mut someone_elses = msg("m3", "u_bob", "congrats", 102);
            someone_elses.parent = Some("m1".into());
            let msgs = [msg("m1", "u_ada", "@Claude x", 100), reply, someone_elses];
            let found = record_of(&msgs, ME, "m1").expect("the agent's own reply");
            assert_eq!(found.id, "m2");
            assert!(record_of(&msgs, ME, "nonexistent").is_none());
        }

        // Case `the_oldest_unanswered_summons_is_taken_and_the_agents_own_words_are_not_one`.
        {
            let msgs = vec![
                mentioning(msg("m1", "u_ada", "@Claude first", 100)),
                mentioning(msg("m2", "u_ada", "@Claude second", 200)),
                // The agent addressing itself must never be work.
                mentioning(msg("m3", ME, "@Claude done", 150)),
            ];
            let s = next_summons(&msgs, ME, NAME, None, 300, &HashSet::new()).expect("a summons");
            assert_eq!(s.message, "m1");

            let done: HashSet<String> = ["m1".to_string()].into_iter().collect();
            let s = next_summons(&msgs, ME, NAME, None, 300, &done).expect("the next one");
            assert_eq!(
                s.message, "m2",
                "answered ones are skipped, the rest keep their order"
            );
        }

        // Case `a_message_that_says_nothing_to_the_agent_is_never_a_summons`.
        {
            let msgs = vec![
                msg("m1", "u_ada", "shipping the run sheet now", 100),
                msg("m2", "u_bob", "nice one", 110),
            ];
            // Even sitting right on top of the agent's own last action.
            let last = LastAction::new("word-processor.block:blk_1", 105);
            assert_eq!(
                next_summons(&msgs, ME, NAME, Some(&last), 120, &HashSet::new()),
                None,
                "being near the agent's work is not the same as speaking to it"
            );
        }
    }
}
