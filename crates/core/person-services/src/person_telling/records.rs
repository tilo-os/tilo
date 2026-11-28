//! What a person's assistant tells them, as their log keeps it and as a device reads it, beside
//! the actions that change it ([`super::declared`]).
//!
//! Everything their assistant tells them outside an answer to their own words joins one log,
//! each thing with a mark and the reason in one line: so a device can say why something
//! notified and why something else did not. What a thing is comes from what caused it and is
//! asked once: a reminder, a brief, a question that waits, a mention of them, or news. A
//! question, a reminder, a brief and a mention reach the person by what they are, and news
//! stays in the thread and in the log.
//!
//! The same words reach a device as a push and as the log's own reading, so [`words_outside`]
//! words both, and [`known_as`] names one thing on one device alone.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The provider that answers a person's tellings under.
pub const PROVIDER: &str = "tilo.telling";
/// The resource the log of what they were told is actions of.
pub const TOLD: &str = "told";

/// The kind a cause names for a reminder its person set.
pub const CAUSE_REMINDER: &str = "reminder";
/// The kind a cause names for a brief: a rundown before something begins,
/// or at a time its person chose.
pub const CAUSE_BRIEF: &str = "brief";
/// The kind a cause names for an event the host ran.
pub const CAUSE_EVENT: &str = "event";

/// The longest words of one thing told, in bytes: what a notification
/// shows, and what fits a push beside its buttons.
pub const MAX_WORDS_BYTES: usize = 600;
/// The longest title of one thing told, in bytes.
pub const MAX_TITLE_BYTES: usize = 120;
/// The most buttons one thing told offers.
pub const MAX_ACTIONS: usize = 6;
/// The longest a host waits on a `list` for the log to move.
pub const MAX_LIST_WAIT_MS: u64 = 10_000;

/// The time between two raisings of a thing that must not be missed, until
/// its person acknowledges it, in seconds.
pub const INSIST_EVERY_SECS: u64 = 10 * 60;
/// How often a thing that must not be missed is raised at most, the first
/// telling included.
pub const INSIST_TIMES: u32 = 6;

/// The person opened the thing.
pub const BY_OPENED: &str = "opened";
/// The person put the notification away.
pub const BY_DISMISSED: &str = "dismissed";
/// The question the thing tells was answered.
pub const BY_ANSWERED: &str = "answered";

/// What kind of thing was told. It comes from what caused it, never from
/// whoever tells.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// It asks the person something and waits for their answer.
    Question,
    /// The person asked to be reminded of it.
    Reminder,
    /// A rundown before something begins, or at a time the person chose.
    Brief,
    /// Anything else: what came in, what was found, what was done.
    #[default]
    News,
    /// Someone in a workspace named the person in what they wrote: it is addressed to them.
    Mention,
}

impl Kind {
    /// What a thing caused by `cause` is, where it asks nothing: a
    /// reminder, a brief, or news. It is for whatever carries out an event
    /// of a person's own to say of that event's own cause; a cause that
    /// whoever tells names says nothing about what the thing is.
    #[must_use]
    pub fn of_cause(cause: Option<&Cause>) -> Self {
        match cause.map(|cause| cause.kind.as_str()) {
            Some(CAUSE_REMINDER) => Self::Reminder,
            Some(CAUSE_BRIEF) => Self::Brief,
            _ => Self::News,
        }
    }

    /// Whether a thing of this kind reaches its person: a question, a
    /// reminder, a brief and a mention reach by what they are; news never.
    #[must_use]
    pub const fn reaches(self) -> bool {
        !matches!(self, Self::News)
    }

    /// The word this kind is written as, where a wire names it: an event's
    /// `toldAs`, or a cause.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Reminder => CAUSE_REMINDER,
            Self::Brief => CAUSE_BRIEF,
            Self::News => "news",
            Self::Mention => "mention",
        }
    }

    /// Why a thing of this kind carries its mark.
    const fn why(self) -> WhyCode {
        match self {
            Self::Question => WhyCode::Question,
            Self::Reminder => WhyCode::Reminder,
            Self::Brief => WhyCode::Brief,
            Self::Mention => WhyCode::Mention,
            Self::News => WhyCode::News,
        }
    }
}

/// What caused a thing to be told: the cause of the turn that told it, as
/// Chat keeps it on the message that started the turn.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cause {
    /// What kind of cause: [`CAUSE_REMINDER`], [`CAUSE_BRIEF`],
    /// [`CAUSE_EVENT`], or a word of the starter's own.
    pub kind: String,
    /// What a person calls it, in a few words.
    #[serde(default)]
    pub label: String,
    /// The starter's own id for it.
    #[serde(default)]
    pub key: String,
}

/// Why a thing carries its mark: what it is.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum WhyCode {
    /// It is a question that waits for its person's answer.
    Question,
    /// It is a reminder its person asked for.
    Reminder,
    /// It is a brief.
    Brief,
    /// It is news: it stays in the log and reaches nobody. A log kept
    /// while a person's rule and a model judged what reached them names
    /// such a thing by why it did not reach them; it reads as news.
    #[default]
    #[serde(
        alias = "no-rule",
        alias = "nothing-holds",
        alias = "judged",
        alias = "unjudged"
    )]
    News,
    /// It names the person.
    Mention,
}

/// The reason for a mark.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct Why {
    pub code: WhyCode,
}

impl Why {
    /// The reason that is `code` and says nothing more.
    #[must_use]
    pub const fn of(code: WhyCode) -> Self {
        Self { code }
    }
}

/// What a thing told is, and why: its kind decides.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    pub why: Why,
}

impl Mark {
    /// The mark of a thing of `kind`.
    #[must_use]
    pub const fn of(kind: Kind) -> Self {
        Self {
            why: Why::of(kind.why()),
        }
    }
}

/// Where a thing told is: the route of a Tilo link, which opening it
/// follows.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct About {
    /// The workspace, by its id spelled out.
    pub workspace: String,
    pub app: String,
    #[serde(default)]
    pub record: String,
    #[serde(default)]
    pub resource: String,
}

/// The task a thing told belongs to: an issue of a tracker.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRef {
    /// The tracker's record.
    pub record: String,
    pub issue: String,
}

/// Who told a thing: an agent of the person's.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Teller {
    /// The agent's id in its workspace.
    pub agent: String,
    /// Its name, as its person reads it.
    pub name: String,
}

/// The typed action a button invokes.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfferedTarget {
    pub app: String,
    #[serde(default)]
    pub record: String,
    pub resource: String,
    pub action: String,
}

/// One button a thing told offers: a typed action a device invokes as the
/// person, with exactly these arguments.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Offered {
    /// What the button is called where pressing it is written down.
    pub id: String,
    /// What the button says.
    pub label: String,
    /// Whether it is the answer expected first: the one a notification
    /// offers beside Open.
    #[serde(default)]
    pub main: bool,
    pub target: OfferedTarget,
    #[serde(default)]
    pub arguments: Map<String, Value>,
}

/// How a thing that insists stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Insists {
    /// The time between two raisings, in seconds.
    pub every_secs: u64,
    /// How often it is raised at most.
    pub times: u32,
    /// How often it was raised so far, the first telling included: a
    /// device raises its notice again when this moves.
    pub raised: u32,
    /// When it is raised next, in milliseconds since the epoch; none once
    /// it was raised [`Self::times`] times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at_ms: Option<u64>,
}

/// The person saw a thing, or acted on it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Acknowledged {
    pub at_ms: u64,
    /// The device of theirs it happened on, as that device names itself;
    /// empty when it named none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    /// How: [`BY_OPENED`], [`BY_DISMISSED`], [`BY_ANSWERED`], or the id of
    /// the button pressed.
    pub by: String,
}

/// One time a host told a thing outside Tilo.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pushed {
    pub at_ms: u64,
    /// Through what: `web-push`.
    pub by: String,
    /// How many of the person's devices took it.
    pub took: u32,
}

/// One thing a person's assistant told them: a row of the log, which a view of another language
/// reads with its actions.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    tilo_resource::JsonSchema,
    tilo_resource::Row,
)]
#[serde(rename_all = "camelCase")]
#[row(kind = "told", label = "Told")]
pub struct Told {
    /// Its place in the log. A thing that changes takes a new place and
    /// keeps its id.
    pub seq: u64,
    /// Its id, which stays: telling the same thing twice tells it once.
    pub id: String,
    /// When it was first told, in milliseconds since the epoch.
    pub at_ms: u64,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Teller>,
    /// A few words over it; empty when it has none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// What is told, as plain text.
    pub words: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<About>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
    pub mark: Mark,
    /// Whether it is to reach the person now, on their devices.
    pub reaches: bool,
    /// Whether its words may leave the host in a push.
    #[serde(default)]
    pub outside: bool,
    /// Its buttons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Offered>,
    /// Who offered its buttons, where an agent's own telling carried them:
    /// a device names them beside the buttons, since pressing one invokes
    /// what that agent chose. None for the buttons Tilo makes itself, like
    /// the answers of a question that waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_by: Option<Teller>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insists: Option<Insists>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged: Option<Acknowledged>,
    /// When what it tells was taken back, in milliseconds since the
    /// epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_at_ms: Option<u64>,
    /// Each time a host told it outside Tilo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pushed: Vec<Pushed>,
}

impl Told {
    /// Whether it still waits for its person: it reaches them, and was
    /// neither acknowledged nor taken back.
    #[must_use]
    pub const fn waits(&self) -> bool {
        self.reaches && self.acknowledged.is_none() && self.withdrawn_at_ms.is_none()
    }
}

/// What was told, oldest first.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct ToldList {
    pub told: Vec<Told>,
    /// The newest place the log has given out.
    pub newest: u64,
}

/// What happened while a person was away, for the one message at the top
/// of the new stretch.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct Stretch {
    /// When they left: the last time a device of theirs looked before the
    /// absence. None when they were never away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_at_ms: Option<u64>,
    /// When they came back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_at_ms: Option<u64>,
    /// What came in and was told.
    pub came_in: Vec<Told>,
    /// What the assistant did alone, by an event's lasting instruction.
    pub did_alone: Vec<Told>,
    /// What waits for them, with its buttons.
    pub waits: Vec<Told>,
}

/// What a telling says of `told`, wherever it is shown off the host: its
/// title and its body. The words are the thing's own where they may leave
/// the host; where its words stay on the host, the body says only where to
/// look. A device of theirs that runs shows the same words as a push does,
/// so a person reads one telling one way.
#[must_use]
pub fn words_outside(told: &Told) -> (String, String) {
    let name = told
        .from
        .as_ref()
        .map(|from| from.name.trim())
        .filter(|name| !name.is_empty());
    let title = if told.title.trim().is_empty() {
        match (told.kind, name) {
            (Kind::Question, Some(name)) => format!("{name} needs you"),
            (Kind::Question, None) => "An agent needs you".to_owned(),
            (Kind::Mention, Some(name)) => format!("{name} mentioned you"),
            (_, Some(name)) => name.to_owned(),
            (_, None) => "Tilo".to_owned(),
        }
    } else {
        told.title.trim().to_owned()
    };
    let body = if told.outside {
        told.words.clone()
    } else {
        "Open Tilo to see it.".to_owned()
    };
    (title, body)
}

/// What the thing `id` is called in what one device shows and is pushed:
/// a name only that device can tie to the thing, made with the device's
/// own secret (its address's auth). A thing's id says which workspace it
/// came from and what caused it, so the id itself is in no push; and one
/// thing has another name on each device, so nothing ties the pushes of
/// one person together.
#[must_use]
pub fn known_as(auth: &[u8], id: &str) -> String {
    use hmac::{Hmac, KeyInit as _, Mac as _};
    // HMAC-SHA256 is what a device can work out with what every Apple system gives it
    // (CryptoKit), and a browser with the Web Crypto API, so a push and what the device shows
    // itself name one thing one way.
    // HMAC takes a key of any length, so this never refuses.
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(auth) else {
        return String::new();
    };
    mac.update(b"tilo told\0");
    mac.update(id.as_bytes());
    // A tag names it on the device alone: short enough for a notification.
    let tag = mac.finalize().into_bytes();
    hex::encode(tag.get(..16).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A thing's name on one device is the one the device itself works out: the first
    /// sixteen bytes of HMAC-SHA256 under its secret over "tilo told", a zero byte and the
    /// id, in hex. The names here were worked out outside this crate, and the device's own
    /// test (`dist/apple`) reads the same ones.
    #[test]
    fn a_things_name_is_the_one_its_device_works_out() {
        let secret = [7u8; 16];
        assert_eq!(
            known_as(&secret, "question:ws-1:m_ask_1"),
            "adffaab0b7c0ff9f5bbae4b9cf6a4a7c"
        );
        assert_eq!(
            known_as(&secret, "turn:ws-1:m_ask_1:0"),
            "a5d9cf8f3af9ad5f1490eb71ed44f85d"
        );
        // Another device's secret names the same thing another way.
        assert_ne!(
            known_as(&[8u8; 16], "question:ws-1:m_ask_1"),
            known_as(&secret, "question:ws-1:m_ask_1")
        );
    }

    /// A thing's mark says what kind of thing it is.
    #[test]
    fn a_mark_says_what_a_thing_is() {
        for (kind, code) in [
            (Kind::Question, WhyCode::Question),
            (Kind::Reminder, WhyCode::Reminder),
            (Kind::Brief, WhyCode::Brief),
            (Kind::Mention, WhyCode::Mention),
            (Kind::News, WhyCode::News),
        ] {
            let mark = Mark::of(kind);
            assert_eq!(mark.why.code, code);
        }
    }

    /// A thing a log kept before kinds alone decided calls news by why it
    /// did not reach its person, and reads as news.
    #[test]
    fn a_reason_kept_before_reads_as_news() {
        for old in ["no-rule", "nothing-holds", "judged", "unjudged"] {
            let code: WhyCode = serde_json::from_value(serde_json::json!(old)).unwrap();
            assert_eq!(code, WhyCode::News, "{old}");
        }
        assert_eq!(serde_json::to_value(WhyCode::News).unwrap(), "news");
    }

    /// The names a device reads are the ones written down for it.
    #[test]
    fn a_told_thing_reads_as_written() {
        let told = Told {
            seq: 3,
            id: "t1".into(),
            kind: Kind::News,
            words: "The quarterly figures arrived.".into(),
            mark: Mark::of(Kind::News),
            ..Told::default()
        };
        let json = serde_json::to_value(&told).unwrap();
        assert_eq!(json["kind"], "news");
        assert_eq!(json["mark"]["why"]["code"], "news");
        assert_eq!(json["reaches"], false);
        assert!(json.get("acknowledged").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<Told>(json).unwrap(), told);
    }
}
