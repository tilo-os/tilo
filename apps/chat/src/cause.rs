//! What started a turn a person did not type: the cause a message carries.
//!
//! An agent works when its person asks it. A person asks by writing.
//! Everything else that starts a turn for them (a reminder that is due, an
//! event that fired, a sub-agent with news, software of their own) asks
//! through one verb, `ask-agent` ([`crate::messages`]), and the message it
//! writes carries its cause: what kind of thing it was, what a person calls
//! it, the starter's own key for it, and the person the turn is for.
//!
//! The person is never an argument. Chat pins them from who invokes the verb
//! ([`starter`]): the person themselves, or the one person the software that
//! invokes acts for. So a cause carries no authority its person did not
//! give: whoever may not act for a person cannot start a turn of that
//! person's agent, and what the agent does in the turn is held to what that
//! person may do, as in a turn they typed.
//!
//! One key is one message. Its id derives from the person and the key
//! ([`message_id`]), so a starter that asks again (a run taken again after a
//! restart) finds its message and starts nothing. The answer is the reply
//! Chat derives from that message and the agent
//! ([`crate::chat::reply_id_at`]), so whoever started a turn can name its
//! answer before either exists, and reads there how it ended.
//!
//! A message with a cause shows as what it is ([`line`]): a reminder or an
//! event by its name, never as words its person wrote.

use serde::{Deserialize, Serialize};
use tilo_actions::policy::MemberId;
use ui::tilo_resource::{self, ActionCx, Atom, Id, JsonSchema, Model, Refusal};

use crate::i18n::t_args;

/// The kind of a cause that is something its person set up and that fired.
pub const EVENT: &str = "event";
/// The kind of a cause that is a time its person asked to be told at.
pub const REMINDER: &str = "reminder";
/// The kind of a cause that is news of work started for its person.
pub const NEWS: &str = "news";
/// An explicit Chat request to start a child.
pub const TEAM_START: &str = "sub-agent";
/// The native team's job and note delivered into a child's Chat.
pub const TEAM_JOB: &str = "job";
pub const TEAM_NOTE: &str = "note";
/// The namespace used for native team deliveries, with a hash for exceptionally long keys.
pub const TEAM_KEY_PREFIX: &str = "sub-agent:";
/// The kind of a cause that is its person asking themselves, through the
/// verb instead of the message field.
pub const PERSON: &str = "person";

/// The longest kind, in bytes.
pub const MAX_KIND_BYTES: usize = 64;
/// The longest label, in bytes.
pub const MAX_LABEL_BYTES: usize = 120;
/// The longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 200;

/// What started the turn a message asks for, as the message keeps it.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Cause {
    /// What kind of thing it was: [`REMINDER`], [`EVENT`], [`NEWS`],
    /// [`PERSON`], or a word of its starter's own.
    pub kind: Atom,
    /// What a person calls it, in a few words: the event's name, what the
    /// reminder is about.
    pub label: String,
    /// Its starter's own id for it: one key is one message.
    pub key: Atom,
    /// The person the turn is for, as the asking pinned them: the agent
    /// acts for them. Never an argument.
    pub person: MemberId,
    /// A fingerprint that restricts an assistant's look to the authority that gathered it.
    pub look_grant: Option<Atom>,
}

/// A cause as its starter gives it: the `cause` of `ask-agent`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Starting {
    /// What kind of thing started it: `reminder`, `event`, `news`, or a word
    /// of your own
    pub kind: String,
    /// What a person calls it, in a few words
    #[serde(default)]
    pub label: String,
    /// Your own id for this cause: one key starts one turn however often it
    /// is asked
    pub key: String,
    /// Restrict this look to the same assistant and signing authority at pickup.
    #[serde(default, rename = "lookGrant", skip_serializing_if = "Option::is_none")]
    pub look_grant: Option<String>,
}

/// Who starts a turn with the action applying now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Starter {
    /// The person the turn is for.
    pub person: MemberId,
    /// Whether that person invokes the action themselves.
    pub themselves: bool,
}

/// The one person `acting` leads to: the actor when it is a person, else the
/// single person down the chain of who it acts for. `None` when it acts for
/// nobody, or for several people at once.
pub(crate) fn led_to(acting: &tilo_actions::Acting) -> Option<&str> {
    let mut node = acting;
    loop {
        if let Some(person) = node.person_id() {
            return Some(person).filter(|person| !person.is_empty());
        }
        match node.acted_for.as_slice() {
            [one] => node = one,
            _ => return None,
        }
    }
}

/// Who starts a turn with the action `cx` applies, and for whom: the person
/// themselves, or the one person the software that invokes it acts for.
///
/// # Errors
/// The invoker acts for nobody, or for several people: an agent works for
/// one person, and nothing else may start its turn.
pub fn starter(cx: &ActionCx) -> Result<Starter, Refusal> {
    let nobody = || {
        Refusal::new(
            "an agent is asked for one person: by them, or by what acts for them and nobody else",
        )
    };
    let acting = cx.invoked_by().ok_or_else(nobody)?;
    let person = led_to(&acting).ok_or_else(nobody)?;
    Ok(Starter {
        person: MemberId::person(person),
        themselves: acting.is_person(),
    })
}

/// Whether `word` names a kind: short, lower case, with nothing a label
/// would hold.
fn is_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= MAX_KIND_BYTES
        && word.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
}

impl Cause {
    /// A child agent reporting its Assistant-owned job to its lead through Chat.
    #[must_use]
    pub fn team_news(&self, author: &MemberId) -> bool {
        self.kind.0 == NEWS
            && author
                .agent_id()
                .is_some_and(|agent| self.has_team_key(agent))
    }

    /// Team work uses the runner that can verify the roster and its Assistant grant.
    #[must_use]
    pub fn needs_team(&self, author: &MemberId, recipient: &str) -> bool {
        match self.kind.0.as_str() {
            TEAM_START => author == &self.person,
            TEAM_JOB | TEAM_NOTE => author.agent_id().is_some() && self.has_team_key(recipient),
            _ => self.team_news(author),
        }
    }

    fn has_team_key(&self, agent: &str) -> bool {
        self.key.0.strip_prefix(TEAM_KEY_PREFIX).is_some_and(|key| {
            key.strip_prefix(agent)
                .is_some_and(|tail| tail.starts_with(':'))
                || key.len() == 40 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }

    /// The cause `starting` gives, for the person `starter` starts the turn
    /// for.
    ///
    /// # Errors
    /// A kind that is no word, or [`PERSON`] from anyone but the person
    /// themselves; a label that is missing, too long or more than a line; a
    /// key that is empty or too long.
    pub fn started(starting: Starting, starter: &Starter) -> Result<Self, Refusal> {
        let kind = starting.kind.trim().to_owned();
        if !is_word(&kind) {
            return Err(Refusal::new("a cause's kind is one short word"));
        }
        if kind == PERSON && !starter.themselves {
            return Err(Refusal::new(
                "only a person themselves asks as a person: what acts for them says what it is",
            ));
        }
        let label = starting.label.trim().to_owned();
        if label.len() > MAX_LABEL_BYTES || label.chars().any(char::is_control) {
            return Err(Refusal::new("a cause is named in a few words on one line"));
        }
        if label.is_empty() && kind != PERSON {
            return Err(Refusal::new("a cause says what a person calls it"));
        }
        let key = starting.key;
        if key.trim().is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
            return Err(Refusal::new("a cause carries its starter's own key for it"));
        }
        if starting.look_grant.as_ref().is_some_and(|grant| {
            grant.len() != 64 || !grant.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(Refusal::new(
                "a look names its original grant by its fingerprint",
            ));
        }
        Ok(Self {
            look_grant: starting.look_grant.map(Atom),
            kind: Atom(kind),
            label,
            key: Atom(key),
            person: starter.person.clone(),
        })
    }

    /// The message it is carried by: the one its person and key derive.
    #[must_use]
    pub fn message(&self) -> Id {
        message_id(&self.person, &self.key.0)
    }

    /// Whether it is its person asking themselves: such a message is theirs
    /// like one they typed.
    #[must_use]
    pub fn by_person(&self) -> bool {
        self.kind.0 == PERSON
    }
}

/// The id of the message that asks for `person` under the starter's `key`:
/// the same on every replica and for every time it is asked, so one key is
/// one message, and its starter can name the message, and with
/// [`crate::chat::reply_id_at`] its answer, before either exists. Two
/// FNV-1a passes give 128 bits.
#[must_use]
pub fn message_id(person: &MemberId, key: &str) -> Id {
    fn fnv1a(parts: [&[u8]; 3], basis: u64) -> u64 {
        parts.into_iter().flatten().fold(basis, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
    let who = person.actor_bytes();
    let parts: [&[u8]; 3] = [&who[..], &[0x1f], key.as_bytes()];
    Id::from_raw(format!(
        "m_ask_{:016x}{:016x}",
        fnv1a(parts, 0xcbf2_9ce4_8422_2325),
        fnv1a(parts, 0x0102_0304_0506_0708)
    ))
}

/// Refuses an ask the conversation `channel` does not take: `person`, whom
/// the turn is for, takes no part in it, no agent does, or the only agent
/// there is `asker` itself.
///
/// A direct conversation's id names who it is with. A channel's members are
/// `members`, read from the chat record where the ask is invoked
/// ([`ui::framework::Chained::prepare`]); the reader of asks checks them
/// against the record again ([`crate::summons`]).
///
/// # Errors
/// As above.
pub fn takes(
    channel: &Id,
    members: &[MemberId],
    person: &MemberId,
    asker: &MemberId,
) -> Result<(), Refusal> {
    let named = crate::chat::Chat::direct_members(channel);
    let members = named.as_deref().unwrap_or(members);
    if !members.contains(person) {
        return Err(Refusal::new(
            "the person an agent is asked for takes part in the conversation",
        ));
    }
    let mut agents = members
        .iter()
        .filter(|member| member.agent_id().is_some())
        .peekable();
    if agents.peek().is_none() {
        return Err(Refusal::new("no agent takes part in this conversation"));
    }
    if agents.all(|agent| agent == asker) {
        return Err(Refusal::new(
            "an agent does not ask itself: nobody else takes part here to answer it",
        ));
    }
    Ok(())
}

/// What a cause shows over its message, where its author's name would
/// stand: a reminder or an event by its name. `None` for a person asking
/// themselves, whose message is theirs.
#[must_use]
pub fn line(cause: &Cause) -> Option<String> {
    let label = cause.label.trim();
    let named = |key: &str| Some(t_args(key, &[("label", label.into())]));
    match cause.kind.0.as_str() {
        PERSON => None,
        REMINDER => named("chat-cause-reminder"),
        EVENT => named("chat-cause-event"),
        NEWS => named("chat-cause-news"),
        _ => Some(label.to_owned()),
    }
}

/// Who an agent is told said a message with this cause, where it reads the
/// conversation before a turn: what started it, never the agent itself and
/// never its person typing. `None` for a person asking themselves.
#[must_use]
pub fn said_by(cause: &Cause) -> Option<String> {
    let label = cause.label.trim();
    Some(match cause.kind.0.as_str() {
        PERSON => return None,
        REMINDER => format!("A reminder ({label:?})"),
        EVENT => format!("An event ({label:?})"),
        NEWS => format!("News ({label:?})"),
        kind => format!("{label:?} ({kind})"),
    })
}

/// What the agent asked is told of the cause, before the instruction it
/// carries: what started the turn, and that its person did not type it just
/// now. `None` for a person asking themselves.
#[must_use]
pub fn told(cause: &Cause) -> Option<String> {
    let label = cause.label.trim();
    let what = match cause.kind.0.as_str() {
        PERSON => return None,
        REMINDER => format!("A reminder your person set is due now: {label:?}."),
        EVENT => format!("An event your person set up fired: {label:?}."),
        NEWS => format!("There is news of work started for your person: {label:?}."),
        kind => format!("{label:?} ({kind}) started this turn for your person."),
    };
    Some(format!(
        "{what} Your person did not type what follows just now: it is what was set up to be \
         asked of you for them."
    ))
}
