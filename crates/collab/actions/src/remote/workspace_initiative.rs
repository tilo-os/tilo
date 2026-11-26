//! Each workspace member can ask their personal assistant to check incoming mail,
//! changed documents and due tasks. Automatic checks run as a native workspace
//! event ([`super::workspace_events`]) owned by that person. A scheduled check
//! starts a model turn only when it finds news and has budget left.
//!
//! Each member has their own schedule, daily limit and proposals. A proposal
//! waits for the person's consent unless they have already authorized that kind
//! of task. Two separate declines suppress future proposals of the same kind
//! until the person allows them again. Raising the limit also needs their consent.

use serde::{Deserialize, Serialize};

use super::workspace_events::SetUp;
use super::{ActionAddress, RemoteActionTarget};

/// The provider the server answers a person's assistant's own initiative
/// under.
pub const PROVIDER: &str = "tilo.initiative";

/// The resource the look-round's actions are of.
pub const LOOK_ROUND: &str = "look-round";
/// The kind of that resource.
pub const LOOK_ROUND_KIND: &str = "initiative.look-round";
/// The look-round as it stands, as a [`LookRound`].
pub const READ: &str = "read";
/// Have the assistant look round ([`TurnOn`]), answering a [`LookRound`].
pub const TURN_ON: &str = "turn-on";
/// Stop it looking round, answering a [`LookRound`]. What it proposed
/// stays.
pub const TURN_OFF: &str = "turn-off";
/// Change its rhythm or its day's limit ([`Settings`]), answering a
/// [`LookRound`].
pub const CHANGE: &str = "change";
/// Take a look today, returning the updated [`LookRound`] without enabling a schedule.
pub const LOOK_NOW: &str = "look-now";

/// The resource the proposals' actions are of.
pub const PROPOSALS: &str = "proposals";
/// The kind of that resource.
pub const PROPOSALS_KIND: &str = "initiative.proposals";
/// What was proposed and what was declined, as [`Proposals`].
pub const LIST: &str = "list";
/// Put a piece of work in front of the person ([`Propose`]), answering the
/// [`Proposal`].
pub const PROPOSE: &str = "propose";
/// The person's nod ([`Accept`]), answering the [`Proposal`]. The person's
/// alone: an agent is refused.
pub const ACCEPT: &str = "accept";
/// Turn a proposal down ([`Decline`]), answering the [`Proposal`].
pub const DECLINE: &str = "decline";
/// Require a fresh decision for future tasks of this kind ([`OfKind`]).
pub const ASK_FIRST: &str = "ask-first";
/// Allow the assistant to propose a previously suppressed kind again ([`OfKind`]).
pub const PROPOSE_AGAIN: &str = "propose-again";

/// The actions exposed to a person reviewing the proposals in Assistant.
#[must_use]
pub fn is_review_action(action: &str) -> bool {
    matches!(action, LIST | ACCEPT | DECLINE | ASK_FIRST | PROPOSE_AGAIN)
}

/// How often it looks round when nobody said: every half hour.
pub const DEFAULT_WHEN: &str = "every 1800 seconds";
/// The shortest time between two looks, in seconds.
pub const MIN_PERIOD_SECS: u64 = 300;
/// The most its looking may spend in a day when nobody said, in millionths
/// of the currency. A higher limit is the person's to set.
pub const DEFAULT_DAY_LIMIT_MICROS: u64 = 1_000_000;
/// The looks a [`LookRound`] lists, newest first.
pub const MAX_LOOKS: usize = 20;
/// The most proposals that wait for a person at once. A further one is
/// refused until some are decided.
pub const MAX_WAITING: usize = 20;
/// How many separate proposals of a kind a person declines before future ones are suppressed.
pub const QUIET_AFTER: u32 = 2;
/// The longest a kind's word may be, in bytes.
pub const MAX_KIND_BYTES: usize = 64;
/// The longest a proposal's title may be, in characters.
pub const MAX_TITLE_CHARS: usize = 160;
/// The longest a proposal's one line of why may be, in characters.
pub const MAX_WHY_CHARS: usize = 400;

/// The target of action `action` of `resource`.
#[must_use]
pub fn target(resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// What turns the look-round on. What is left out is as it was, or the
/// default the first time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnOn {
    /// Its rhythm, as the line a Shortcut opens with: `every 1800 seconds`,
    /// `every day at 09:00`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// The most its looking may spend in a day, in millionths of the
    /// currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_limit_micros: Option<u64>,
}

/// A change to the look-round. What is left out stays as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// Its rhythm, as [`TurnOn::when`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Its day's limit, as [`TurnOn::day_limit_micros`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_limit_micros: Option<u64>,
}

/// What one look found, in numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Found {
    /// What came in for the person since the last look.
    pub came_in: u64,
    /// The documents of the workspace somebody other than the assistant
    /// changed since the last look.
    pub changed: u64,
    /// The tasks that became due: within a day, or overdue.
    pub due: u64,
    /// The open tasks that are the person's or their assistant's.
    pub open: u64,
}

impl Found {
    /// Whether anything is new: something came in, changed or became due.
    /// What is merely still open is not.
    #[must_use]
    pub const fn anything_new(&self) -> bool {
        self.came_in > 0 || self.changed > 0 || self.due > 0
    }
}

/// How a look ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LookEnded {
    /// Nothing was new: no model was asked and nothing was written.
    Quiet,
    /// What was new was handed to the assistant, as one turn.
    Told,
    /// Something was new and its day was spent: nothing was started.
    OverLimit,
    Failed,
}

/// One look.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Look {
    pub id: String,
    /// When it looked, ms since the Unix epoch.
    pub at_ms: i64,
    pub found: Found,
    pub ended: LookEnded,
    /// Why it failed, in words for its person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Cost reported by Chat on the completed answers to this check, in millionths of the currency.
    pub cost_micros: u64,
}

/// The look-round, as [`READ`] and every action on it answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookRound {
    /// Whether the assistant looks round now.
    pub on: bool,
    /// Its rhythm, as the line a Shortcut opens with.
    pub when: String,
    /// The next time it looks, ms since the Unix epoch, while it is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at_ms: Option<i64>,
    /// The id of the event it is among the person's events, once it was
    /// turned on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// The most its looking may spend in a day, in millionths of the
    /// currency.
    pub day_limit_micros: u64,
    /// What it spent on `day`.
    pub spent_today_micros: u64,
    /// The day counted, `YYYY-MM-DD`.
    pub day: String,
    /// Its last looks, newest first, [`MAX_LOOKS`] at the most.
    pub looks: Vec<Look>,
}

/// A piece of work, or an event, to put in front of the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Propose {
    /// The assistant's own short word for this sort of work, the same each
    /// time: lower case, digits and dashes.
    pub kind: String,
    /// What would be done, as a task's title reads.
    pub title: String,
    /// One line of why.
    pub why: String,
    /// The event to set up, when what is proposed is that something that
    /// comes round again is done by itself. Left out, a task is proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<SetUp>,
}

/// The person's nod to a proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Accept {
    pub proposal: String,
    /// Start future tasks of this kind on the person's standing instruction.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub always: bool,
}

/// A proposal kind named by its normalized word.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OfKind {
    pub kind: String,
}

/// A proposal turned down.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Decline {
    pub proposal: String,
    /// Why, in the person's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Where a proposal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// It waits for its person's nod.
    Proposed,
    /// Its person entrusted tasks of this kind to the assistant.
    Started,
    Accepted,
    Declined,
}

impl State {
    /// Whether the proposal still waits for its person.
    #[must_use]
    pub const fn waits(self) -> bool {
        matches!(self, Self::Proposed)
    }
}

/// The task a proposal stands as in the workspace's tracker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposedTask {
    /// The issue, by its address in the tracker.
    pub issue: String,
    /// Its number there, when the tracker said it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
}

/// One proposal, as [`LIST`] and every action on it answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proposal {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub why: String,
    /// The task it stands as, for a proposed piece of work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<ProposedTask>,
    /// The event that would be set up, for a proposed event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<SetUp>,
    pub state: State,
    /// When it was proposed, ms since the Unix epoch.
    pub proposed_at_ms: i64,
    /// When its person decided, or it began alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at_ms: Option<i64>,
    /// The look it came from, when a look-round's turn proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<String>,
    /// The id of the event set up, once a proposed event was accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_up: Option<String>,
}

/// What the person currently wants for one kind of proposal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Kind {
    pub kind: String,
    pub declined: u32,
    pub quiet: bool,
    pub always: bool,
}

/// What a person's assistant proposed, newest first, and their preferences.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proposals {
    pub proposals: Vec<Proposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<Kind>,
}

/// `kind` as it is kept: trimmed, lower case, what is neither a letter nor
/// a digit as one dash. None for a word that names nothing or is longer
/// than [`MAX_KIND_BYTES`].
#[must_use]
pub fn kind_word(kind: &str) -> Option<String> {
    let mut word = String::new();
    for c in kind.trim().chars() {
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        } else if !word.is_empty() && !word.ends_with('-') {
            word.push('-');
        }
    }
    let word = word.trim_end_matches('-').to_owned();
    (!word.is_empty() && word.len() <= MAX_KIND_BYTES).then_some(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_is_one_word_however_it_was_written() {
        assert_eq!(
            kind_word("  Reply to unanswered mail! ").as_deref(),
            Some("reply-to-unanswered-mail")
        );
        assert_eq!(
            kind_word("price_page--watch").as_deref(),
            Some("price-page-watch")
        );
        assert_eq!(kind_word("Überblick").as_deref(), Some("überblick"));
        assert_eq!(kind_word(" - "), None, "a word that names nothing");
        assert_eq!(kind_word(&"x".repeat(MAX_KIND_BYTES + 1)), None);
    }

    #[test]
    fn only_what_came_in_changed_or_became_due_is_new() {
        assert!(!Found::default().anything_new());
        assert!(
            !Found {
                open: 7,
                ..Found::default()
            }
            .anything_new(),
            "what is still open is not new"
        );
        for found in [
            Found {
                came_in: 1,
                ..Found::default()
            },
            Found {
                changed: 1,
                ..Found::default()
            },
            Found {
                due: 1,
                ..Found::default()
            },
        ] {
            assert!(found.anything_new(), "{found:?}");
        }
    }

    #[test]
    fn the_shapes_read_as_a_client_writes_them() {
        let on: TurnOn =
            serde_json::from_value(serde_json::json!({"when": "every day at 09:00"})).unwrap();
        assert_eq!(on.when.as_deref(), Some("every day at 09:00"));
        assert_eq!(on.day_limit_micros, None);
        let look = Look {
            id: "l1".into(),
            at_ms: 5,
            found: Found {
                came_in: 2,
                ..Found::default()
            },
            ended: LookEnded::OverLimit,
            why: None,
            cost_micros: 0,
        };
        assert_eq!(
            serde_json::to_value(&look).unwrap(),
            serde_json::json!({
                "id": "l1",
                "atMs": 5,
                "found": {"cameIn": 2, "changed": 0, "due": 0, "open": 0},
                "ended": "over-limit",
                "costMicros": 0,
            })
        );
        let accept: Accept = serde_json::from_value(serde_json::json!({"proposal": "p1"})).unwrap();
        assert_eq!(accept.proposal, "p1");
        assert!(State::Proposed.waits() && !State::Accepted.waits());
    }
}
