//! What a person's assistant tells them, and what may interrupt them.
//!
//! A person says in their own words what is important to them, and names
//! the situations those words depend on. The host that runs their
//! workspace keeps both, because it is the host that judges and tells
//! while every device of theirs is off. Everything their assistant tells
//! them outside an answer to their own words joins one log, each thing
//! with a mark, important or not, and the reason in one line: so a device
//! can say why something notified and why something else did not.
//!
//! What a thing is comes from what caused it and is asked once: a
//! reminder, a brief, a question that waits, a mention of them, or news. A
//! question, a reminder, a brief and a mention reach the person by what
//! they are. News reaches
//! them only when it is important by their own rule. The rest stays in
//! the thread and in the log, and pushes nothing.
//!
//! All of it is the person's own: the host answers the person on a device
//! of theirs and an agent acting for them over such a device, and nobody
//! else in a workspace they share.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers a person's importance and tellings under.
pub const PROVIDER: &str = "tilo.telling";
/// The resource the person's rule is actions of.
pub const IMPORTANCE: &str = "importance";
/// The resource the log of what they were told is actions of.
pub const TOLD: &str = "told";

/// The rule as an [`Importance`].
pub const READ: &str = "read";
/// Add a line to the rule ([`Say`]), answering the [`Importance`].
pub const SAY: &str = "say";
/// Remove a line ([`Unsay`]), answering the [`Importance`].
pub const UNSAY: &str = "unsay";
/// Add or change a situation ([`SetFact`]), answering the [`Importance`].
pub const SET_FACT: &str = "set-fact";
/// Remove a situation ([`ForgetFact`]), answering the [`Importance`].
/// Refused while a line waits on it.
pub const FORGET_FACT: &str = "forget-fact";
/// Set how a thing that must not be missed reminds again ([`Insisting`]),
/// answering the [`Importance`].
pub const SET_INSISTING: &str = "set-insisting";
/// Say whether one kind of thing may break do-not-disturb
/// ([`LetThrough`]), answering the [`Importance`]. The person's alone.
pub const LET_THROUGH: &str = "let-through";
/// Say whether a push may carry words that came from one workspace
/// ([`KeepOnHost`]), answering the [`Importance`]. The person's alone.
pub const KEEP_ON_HOST: &str = "keep-on-host";

/// Tell the person something ([`Tell`]), answering it as a [`Told`] with
/// its mark. An agent of the person's asks this; it never says what kind
/// of thing it is, nor how important. What it tells is news whatever cause
/// it names, and a button it offers is an action it may invoke itself.
pub const TELL: &str = "tell";
/// What was told after a place in the log ([`ListAfter`]): a [`ToldList`].
/// The person's alone, and every ask counts as a device of theirs looking.
pub const LIST: &str = "list";
/// The person saw one thing or acted on it ([`Acknowledge`]), answering the
/// [`Told`]. The person's alone.
pub const ACKNOWLEDGE: &str = "acknowledge";
/// Say whether one thing told must not be missed ([`Insist`]), answering
/// the [`Told`].
pub const INSIST: &str = "insist";
/// What happened while the person was away: a [`Stretch`]. The person's
/// alone.
pub const SINCE_YOU_LEFT: &str = "since-you-left";

/// The kind a cause names for a reminder its person set.
pub const CAUSE_REMINDER: &str = "reminder";
/// The kind a cause names for a brief: a rundown before something begins,
/// or at a time its person chose.
pub const CAUSE_BRIEF: &str = "brief";
/// The kind a cause names for an event the host ran.
pub const CAUSE_EVENT: &str = "event";
/// The kind a cause names for the person's own words.
pub const CAUSE_PERSON: &str = "person";

/// The most lines a rule keeps.
pub const MAX_LINES: usize = 50;
/// The most situations a rule keeps.
pub const MAX_FACTS: usize = 50;
/// The longest line of a rule, in bytes.
pub const MAX_LINE_BYTES: usize = 500;
/// The longest name of a situation, in bytes.
pub const MAX_FACT_NAME_BYTES: usize = 80;
/// The longest value of a situation, in bytes.
pub const MAX_FACT_VALUE_BYTES: usize = 200;
/// The longest words of one thing told, in bytes: what a notification
/// shows, and what fits a push beside its buttons.
pub const MAX_WORDS_BYTES: usize = 600;
/// The longest title of one thing told, in bytes.
pub const MAX_TITLE_BYTES: usize = 120;
/// The most buttons one thing told offers.
pub const MAX_ACTIONS: usize = 6;
/// The shortest time between two raisings of a thing that insists.
pub const MIN_INSIST_SECS: u64 = 60;
/// The longest time between two raisings of a thing that insists.
pub const MAX_INSIST_SECS: u64 = 24 * 60 * 60;
/// The most often a thing that insists is raised.
pub const MAX_INSIST_TIMES: u32 = 24;
/// The longest a host holds a [`LIST`] that waits for the log to move.
pub const MAX_LIST_WAIT_MS: u64 = 10_000;

/// The target of `action` of `resource` ([`IMPORTANCE`] or [`TOLD`]).
#[must_use]
pub fn target(resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// What a person calls important, in their own words, with the situations
/// those words depend on, and how they want to be told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Importance {
    /// What is important, one sentence of the person's own a line, in the
    /// order they said them.
    #[serde(default)]
    pub lines: Vec<Line>,
    /// The situations the lines depend on.
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// The kinds of thing that may break do-not-disturb on a device. None
    /// until the person says so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub through_quiet: Vec<Reach>,
    /// How a thing that must not be missed reminds again.
    #[serde(default)]
    pub insists: Insisting,
    /// The workspaces whose words no push carries off the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_on_host: Vec<String>,
    /// Counts the changes: a judgement made under one revision stands
    /// until the rule changes.
    #[serde(default)]
    pub revision: u64,
}

/// One sentence of a person's about what is important.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// The line's id, as [`UNSAY`] names it.
    pub id: String,
    /// The person's own words, kept as they said them.
    pub words: String,
    /// The situation the line counts in: with one, the line says nothing
    /// while that situation is otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_while: Option<Holds>,
    /// The agent that wrote the line down for the person, by its name;
    /// empty when the person did, on a device of theirs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// When, in milliseconds since the epoch.
    #[serde(default)]
    pub at_ms: u64,
}

/// A situation having one value: what a line may wait on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Holds {
    /// The situation's name, as [`Fact::name`] has it.
    pub fact: String,
    /// The value it must have for the line to count.
    pub is: String,
}

/// One situation a person's lines depend on, as it stands now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    /// What the person calls it.
    pub name: String,
    /// How it stands now, in their words.
    pub value: String,
    /// The agent that set it for the person, by its name; empty when the
    /// person did.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// When, in milliseconds since the epoch.
    #[serde(default)]
    pub at_ms: u64,
}

/// How a thing that must not be missed reminds again until its person
/// acknowledges it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Insisting {
    /// The time between two raisings, in seconds.
    pub every_secs: u64,
    /// How often it is raised at most, the first telling included.
    pub times: u32,
}

impl Default for Insisting {
    fn default() -> Self {
        Self {
            every_secs: 10 * 60,
            times: 6,
        }
    }
}

impl Insisting {
    /// These, when a person may ask for them: not more often than every
    /// minute, not rarer than daily, and not more than
    /// [`MAX_INSIST_TIMES`] times.
    ///
    /// # Errors
    /// Why not, in words the person reads.
    pub fn checked(self) -> Result<Self, String> {
        if !(MIN_INSIST_SECS..=MAX_INSIST_SECS).contains(&self.every_secs) {
            return Err("A reminder repeats between once a minute and once a day".to_owned());
        }
        if !(1..=MAX_INSIST_TIMES).contains(&self.times) {
            return Err(format!(
                "A reminder repeats at most {MAX_INSIST_TIMES} times"
            ));
        }
        Ok(self)
    }
}

/// What kind of thing was told. It comes from what caused it, never from
/// whoever tells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
}

/// Why a thing reaches its person: what do-not-disturb is let through by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reach {
    /// A question waits for them.
    Question,
    /// A reminder they asked for.
    Reminder,
    /// A brief.
    Brief,
    /// News their own rule calls important.
    Important,
    /// Someone named them.
    Mention,
}

impl Reach {
    /// Why a thing of `kind` reaches its person, if it does: a question, a
    /// reminder, a brief and a mention by what they are, news only when `important`.
    #[must_use]
    pub const fn of(kind: Kind, important: bool) -> Option<Self> {
        match kind {
            Kind::Question => Some(Self::Question),
            Kind::Reminder => Some(Self::Reminder),
            Kind::Brief => Some(Self::Brief),
            Kind::Mention => Some(Self::Mention),
            Kind::News if important => Some(Self::Important),
            Kind::News => None,
        }
    }
}

/// What the rule says about a thing before anybody is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decided<'a> {
    /// The person said nothing about what is important: nothing is.
    NoRule,
    /// Every line waits on a situation that is otherwise now: nothing is
    /// important while that stands. Names the one situation they all wait
    /// on and how it stands, when it is one.
    NothingHolds(Option<(&'a str, &'a str)>),
    /// These lines count now, and whether the thing falls under one of
    /// them takes judgement.
    Judge(Vec<&'a Line>),
}

/// Whether two names or values a person wrote are the same: without
/// regard to case and to space around them.
fn same(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

impl Importance {
    /// The situation called `name`, as it stands now.
    #[must_use]
    pub fn fact(&self, name: &str) -> Option<&Fact> {
        self.facts.iter().find(|fact| same(&fact.name, name))
    }

    /// Whether `line` counts now: it waits on nothing, or the situation it
    /// waits on stands as it says.
    #[must_use]
    pub fn holds(&self, line: &Line) -> bool {
        line.only_while.as_ref().is_none_or(|holds| {
            self.fact(&holds.fact)
                .is_some_and(|fact| same(&fact.value, &holds.is))
        })
    }

    /// What the rule says before anybody is asked: nothing at all, nothing
    /// while the situations stand as they do, or that it takes judgement.
    #[must_use]
    pub fn decided(&self) -> Decided<'_> {
        if self.lines.is_empty() {
            return Decided::NoRule;
        }
        let holding: Vec<&Line> = self.lines.iter().filter(|line| self.holds(line)).collect();
        if !holding.is_empty() {
            return Decided::Judge(holding);
        }
        // Every line waits. When they all wait on one situation, that is
        // the reason a person reads.
        let mut waited = self
            .lines
            .iter()
            .filter_map(|line| line.only_while.as_ref())
            .map(|holds| holds.fact.as_str());
        let first = waited.next();
        let one = first
            .filter(|first| waited.all(|name| same(name, first)))
            .map(|name| {
                self.fact(name)
                    .map_or((name, ""), |fact| (fact.name.as_str(), fact.value.as_str()))
            });
        Decided::NothingHolds(one)
    }

    /// Whether a thing that reaches its person by `reach` may break
    /// do-not-disturb on a device: only what the person let through.
    #[must_use]
    pub fn lets_through(&self, reach: Reach) -> bool {
        self.through_quiet.contains(&reach)
    }

    /// Whether a push may carry words that came from `workspace` off the
    /// host.
    #[must_use]
    pub fn carries_outside(&self, workspace: &str) -> bool {
        !self.kept_on_host.iter().any(|kept| kept == workspace)
    }
}

/// A line to add to the rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Say {
    /// The person's own words.
    pub words: String,
    /// The situation the line counts in, when it depends on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_while: Option<Holds>,
}

/// A line to remove from the rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Unsay {
    /// The line's id.
    pub line: String,
}

/// A situation to add, or to change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetFact {
    pub name: String,
    pub value: String,
}

/// A situation to remove.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForgetFact {
    pub name: String,
}

/// Whether one kind of thing may break do-not-disturb.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LetThrough {
    pub kind: Reach,
    pub may: bool,
}

/// Whether the words that came from one workspace stay on the host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeepOnHost {
    /// The workspace, by its id spelled out.
    pub workspace: String,
    pub kept: bool,
}

/// What caused a thing to be told: the cause of the turn that told it, as
/// Chat keeps it on the message that started the turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cause {
    /// What kind of cause: [`CAUSE_REMINDER`], [`CAUSE_BRIEF`],
    /// [`CAUSE_EVENT`], or a word of the starter's own.
    pub kind: String,
    /// What a person calls it, in a few words.
    #[serde(default)]
    pub label: String,
    /// The starter's own id for it: one cause is judged once.
    #[serde(default)]
    pub key: String,
}

/// Why a thing carries its mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhyCode {
    /// It is a question: it reaches its person as that, and nobody judged
    /// it.
    Question,
    /// It is a reminder its person asked for.
    Reminder,
    /// It is a brief.
    Brief,
    /// The person said nothing about what is important.
    #[default]
    NoRule,
    /// Every line of the rule waits on a situation that is otherwise now.
    NothingHolds,
    /// The person's own model judged it by the rule.
    Judged,
    /// It could not be judged, so it is not called important.
    Unjudged,
    /// It names the person: it reaches them as that, and nobody judged it.
    Mention,
}

/// The reason for a mark, in one line a device words for its person.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Why {
    pub code: WhyCode,
    /// For [`WhyCode::NothingHolds`]: the one situation every line waits
    /// on, when it is one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fact: String,
    /// For [`WhyCode::NothingHolds`]: how that situation stands now. For
    /// [`WhyCode::Unjudged`]: the refusal's stable name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// For [`WhyCode::Judged`]: the judge's one line, in the language of
    /// the rule. For [`WhyCode::Unjudged`]: why it could not be asked.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub words: String,
}

impl Why {
    /// The reason that is `code` and says nothing more.
    #[must_use]
    pub fn of(code: WhyCode) -> Self {
        Self {
            code,
            ..Self::default()
        }
    }
}

/// Whether a thing is important to its person, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mark {
    pub important: bool,
    pub why: Why,
    /// The revision of the rule the mark was made under.
    #[serde(default)]
    pub revision: u64,
}

/// Where a thing told is: the route of a Tilo link, which opening it
/// follows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRef {
    /// The tracker's record.
    pub record: String,
    pub issue: String,
}

/// Who told a thing: an agent of the person's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Teller {
    /// The agent's id in its workspace.
    pub agent: String,
    /// Its name, as its person reads it.
    pub name: String,
}

/// The typed action a button invokes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

/// The person opened the thing.
pub const BY_OPENED: &str = "opened";
/// The person put the notification away.
pub const BY_DISMISSED: &str = "dismissed";
/// The question the thing tells was answered.
pub const BY_ANSWERED: &str = "answered";

/// One time a host told a thing outside Tilo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pushed {
    pub at_ms: u64,
    /// Through what: `web-push`.
    pub by: String,
    /// How many of the person's devices took it.
    pub took: u32,
}

/// One thing a person's assistant told them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// Whether it may break do-not-disturb on a device.
    #[serde(default)]
    pub through_quiet: bool,
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

/// What a telling says of `told`, wherever it is shown off the host: its
/// title and its body. The words are the thing's own where they may leave
/// the host; where a person keeps them on the host, the body says only
/// where to look. A device of theirs that runs shows the same words as a
/// push does, so a person reads one telling one way.
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
    let key = blake3::hash(auth);
    let mut hasher = blake3::Hasher::new_keyed(key.as_bytes());
    hasher.update(b"tilo told\0");
    hasher.update(id.as_bytes());
    let tag = hasher.finalize();
    // A tag names it on the device alone: short enough for a notification.
    tag.to_hex().get(..16).unwrap_or_default().to_owned()
}

/// Something to tell the person.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tell {
    /// What is told, as plain text.
    pub words: String,
    #[serde(default)]
    pub title: String,
    /// What caused it, in the teller's words. With none it is news of the
    /// teller's own, and with any it is news still: no cause a teller
    /// names makes a reminder or a brief of what it tells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<About>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<Offered>,
    /// Whether it must not be missed: raised again, by the person's own
    /// rhythm, until they acknowledge it.
    #[serde(default)]
    pub insists: bool,
}

/// Ask what was told after a place in the log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListAfter {
    /// The place read up to; everything the log keeps when omitted.
    #[serde(default)]
    pub after: u64,
    /// How long to wait for the log to move when nothing is newer, in
    /// milliseconds: at most [`MAX_LIST_WAIT_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// The device asking, as it names itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
}

/// What was told, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToldList {
    pub told: Vec<Told>,
    /// The newest place the log has given out.
    pub newest: u64,
}

/// The person saw a thing or acted on it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Acknowledge {
    /// The thing, by its id.
    pub told: String,
    #[serde(default)]
    pub device: String,
    /// How; [`BY_OPENED`] when omitted.
    #[serde(default)]
    pub by: String,
}

/// Whether one thing told must not be missed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Insist {
    /// The thing, by its id.
    pub told: String,
    pub insists: bool,
}

/// What happened while a person was away, for the one message at the top
/// of the new stretch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn line(words: &str, only_while: Option<(&str, &str)>) -> Line {
        Line {
            id: words.len().to_string(),
            words: words.to_owned(),
            only_while: only_while.map(|(fact, is)| Holds {
                fact: fact.to_owned(),
                is: is.to_owned(),
            }),
            ..Line::default()
        }
    }

    fn fact(name: &str, value: &str) -> Fact {
        Fact {
            name: name.to_owned(),
            value: value.to_owned(),
            ..Fact::default()
        }
    }

    /// A person who said nothing has no rule, and nothing of theirs is
    /// important by one.
    #[test]
    fn nothing_said_is_no_rule() {
        assert_eq!(Importance::default().decided(), Decided::NoRule);
    }

    /// A line that waits on a situation says nothing while the situation is
    /// otherwise, and the reason names the situation as the person wrote
    /// it: no judgement is needed to know that.
    #[test]
    fn a_line_waits_on_its_situation() {
        let mut rule = Importance {
            lines: vec![line(
                "A new customer is important only while we are hiring",
                Some(("hiring", "yes")),
            )],
            facts: vec![fact("Hiring", "no")],
            ..Importance::default()
        };
        assert_eq!(
            rule.decided(),
            Decided::NothingHolds(Some(("Hiring", "no")))
        );
        // The situation changes, and the same line counts.
        rule.facts = vec![fact("Hiring", " Yes ")];
        let Decided::Judge(holding) = rule.decided() else {
            panic!("the line counts now");
        };
        assert_eq!(holding.len(), 1);
        // A situation nobody set stands as nothing.
        rule.facts.clear();
        assert_eq!(rule.decided(), Decided::NothingHolds(Some(("hiring", ""))));
    }

    /// A line that waits on nothing always counts, whatever the others
    /// wait on; and lines waiting on different situations name none.
    #[test]
    fn a_plain_line_always_counts() {
        let waiting = line("Only while travelling", Some(("travelling", "yes")));
        let other = line("Only at quarter end", Some(("quarter end", "yes")));
        let plain = line("My landlord is always important", None);
        let rule = Importance {
            lines: vec![waiting.clone(), plain.clone()],
            ..Importance::default()
        };
        assert_eq!(rule.decided(), Decided::Judge(vec![&plain]));
        let rule = Importance {
            lines: vec![waiting, other],
            ..Importance::default()
        };
        assert_eq!(rule.decided(), Decided::NothingHolds(None));
    }

    /// What a thing is comes from its cause, and only a question, a
    /// reminder, a brief and important news reach a person.
    #[test]
    fn a_cause_says_what_a_thing_is() {
        let caused = |kind: &str| Cause {
            kind: kind.to_owned(),
            ..Cause::default()
        };
        assert_eq!(Kind::of_cause(Some(&caused("reminder"))), Kind::Reminder);
        assert_eq!(Kind::of_cause(Some(&caused("brief"))), Kind::Brief);
        assert_eq!(Kind::of_cause(Some(&caused("event"))), Kind::News);
        assert_eq!(Kind::of_cause(Some(&caused("competitors"))), Kind::News);
        assert_eq!(Kind::of_cause(None), Kind::News);
        assert_eq!(Reach::of(Kind::News, false), None);
        assert_eq!(Reach::of(Kind::News, true), Some(Reach::Important));
        assert_eq!(Reach::of(Kind::Reminder, false), Some(Reach::Reminder));
        assert_eq!(Reach::of(Kind::Question, false), Some(Reach::Question));
        assert_eq!(Reach::of(Kind::Brief, false), Some(Reach::Brief));
    }

    /// The names a device reads are the ones written down for it.
    #[test]
    fn a_told_thing_reads_as_written() {
        let told = Told {
            seq: 3,
            id: "t1".into(),
            kind: Kind::News,
            words: "The quarterly figures arrived.".into(),
            mark: Mark {
                important: false,
                why: Why {
                    code: WhyCode::NothingHolds,
                    fact: "Hiring".into(),
                    value: "no".into(),
                    words: String::new(),
                },
                revision: 2,
            },
            ..Told::default()
        };
        let json = serde_json::to_value(&told).unwrap();
        assert_eq!(json["kind"], "news");
        assert_eq!(json["mark"]["why"]["code"], "nothing-holds");
        assert_eq!(json["mark"]["why"]["fact"], "Hiring");
        assert_eq!(json["reaches"], false);
        assert!(json.get("acknowledged").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<Told>(json).unwrap(), told);
        let letting: LetThrough =
            serde_json::from_value(serde_json::json!({"kind": "reminder", "may": true})).unwrap();
        assert_eq!(letting.kind, Reach::Reminder);
    }

    /// A rhythm a person may ask for is bounded both ways.
    #[test]
    fn insisting_is_bounded() {
        assert!(Insisting::default().checked().is_ok());
        let often = Insisting {
            every_secs: 5,
            times: 3,
        };
        assert!(often.checked().is_err());
        let endless = Insisting {
            every_secs: 600,
            times: 1000,
        };
        assert!(endless.checked().is_err());
    }
}
