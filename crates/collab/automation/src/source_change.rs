//! A trigger on something new in a source
//! ([`TriggerSpec::SourceChanged`](crate::shortcut::TriggerSpec::SourceChanged)):
//! what a source is, what counts as new in it, and what an invocation is
//! told of it.
//!
//! A source holds things its person wants answered as they come, and is no
//! resource of the workspace: the mail their accounts receive is the first
//! one read. Whoever keeps the automation is told by the source, or looks at
//! it, and names each thing found there by a key that stays the same however
//! often it is found ([`Found`]). What was taken in before is kept
//! ([`Seen`]), and from that follows all of it:
//!
//! - what the source held before the trigger was first seen is never new:
//!   an automation answers what comes from when it was set up, not
//!   everything its source ever held;
//! - each new thing fires once, however often it is found, also across a
//!   restart;
//! - what came while whoever keeps the automation was down is answered once
//!   when it is up again, for as long as the source still holds it;
//! - what is new goes with the invocation as what it is ([`told`]): words
//!   somebody else wrote, which are data and never an instruction of the
//!   automation's own person.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The kind a trigger names for the mail a person's accounts receive, as the
/// holder that keeps those accounts hears it arrive. `at` names one account
/// by its id, and is empty for every account. What is new is one message,
/// with the fields an arrival is compared by.
pub const PERSON_MAIL: &str = "tilo.accounts.mail";

/// The longest a source's kind, or the name of one of its kind, may be.
pub const MAX_NAME_BYTES: usize = 2048;

/// One thing found in a source that was not there before.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// What names it for good: the same for the same thing however often it
    /// is found.
    pub key: String,
    /// Which source of its kind it was found in: an account, an address.
    pub at: String,
    /// When whoever keeps the automation found it, ms since the epoch.
    pub at_ms: i64,
    /// Its own fields: what the trigger's filter is held to and what the
    /// invocation is told.
    pub facts: Map<String, Value>,
}

/// What one trigger has taken in of its source, kept across restarts by
/// whoever keeps the automation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// The trigger this is the progress of, as whoever keeps it marks one:
    /// another trigger in its place starts over.
    #[serde(default)]
    pub mark: String,
    /// When the trigger was first seen, ms since the epoch.
    #[serde(default)]
    pub since_ms: i64,
    /// For a source that numbers what it holds: the number read up to. None
    /// until the source was first looked at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<u64>,
}

impl Seen {
    /// The progress of the trigger marked `mark`, first seen at `now_ms`:
    /// nothing of its source was looked at yet.
    #[must_use]
    pub fn starting(mark: &str, now_ms: i64) -> Self {
        Self {
            mark: mark.to_owned(),
            since_ms: now_ms,
            place: None,
        }
    }

    /// Make this the progress of the trigger marked `mark`: kept when it is
    /// that trigger's already, started over at `now_ms` when another
    /// trigger stood in its place.
    pub fn follow(&mut self, mark: &str, now_ms: i64) {
        if self.mark != mark {
            *self = Self::starting(mark, now_ms);
        }
    }
}

/// What an invocation is told of the new thing that fired it, as its `event`
/// input: the `kind` of source and which one of that kind (`at`), what
/// names the thing (`item`), when it was found (`found_ms`), and its own
/// fields (`facts`).
#[must_use]
pub fn told(kind: &str, found: &Found) -> Map<String, Value> {
    let told = json!({
        "kind": kind,
        "at": found.at,
        "item": found.key,
        "found_ms": found.at_ms,
        "facts": Value::Object(found.facts.clone()),
    });
    match told {
        Value::Object(told) => told,
        _ => Map::new(),
    }
}

/// Why a trigger on something new in the source of `kind` at `at` cannot be
/// kept, when it cannot: in words for whoever described it. Which kinds are
/// read is said by whoever reads them.
#[must_use]
pub fn refusal(kind: &str, at: &str) -> Option<String> {
    if kind.trim().is_empty() {
        return Some(format!(
            "a source is named by its kind: `when something new is in \"{PERSON_MAIL}\"`"
        ));
    }
    if kind.len() > MAX_NAME_BYTES || at.len() > MAX_NAME_BYTES {
        return Some(format!(
            "a source's kind and its name are at most {MAX_NAME_BYTES} bytes each"
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_new_is_told_as_its_own_fields_under_the_source_it_was_found_in() {
        let found = Found {
            key: "ada@example.com:7:4".into(),
            at: "ada@example.com".into(),
            at_ms: 1_800_000_000_000,
            facts: Map::from_iter([("from".to_owned(), json!("bo@example.com"))]),
        };
        assert_eq!(
            Value::Object(told(PERSON_MAIL, &found)),
            json!({
                "kind": "tilo.accounts.mail",
                "at": "ada@example.com",
                "item": "ada@example.com:7:4",
                "found_ms": 1_800_000_000_000_i64,
                "facts": {"from": "bo@example.com"},
            })
        );
    }

    #[test]
    fn progress_is_kept_for_its_own_trigger_and_starts_over_for_another() {
        let mut seen = Seen::starting("first", 10);
        assert_eq!(seen.place, None, "nothing was looked at yet");
        seen.place = Some(4);
        seen.follow("first", 20);
        assert_eq!((seen.since_ms, seen.place), (10, Some(4)));
        seen.follow("second", 30);
        assert_eq!(seen, Seen::starting("second", 30));
        // What is kept reads back, and a place never looked at is left out.
        let kept = serde_json::to_value(&seen).unwrap_or_default();
        assert_eq!(kept, json!({"mark": "second", "since_ms": 30}));
        assert_eq!(serde_json::from_value::<Seen>(kept).ok(), Some(seen));
    }

    #[test]
    fn a_source_with_no_kind_or_a_name_past_its_bound_is_refused_in_words() {
        assert_eq!(refusal(PERSON_MAIL, ""), None);
        assert_eq!(refusal(PERSON_MAIL, "ada@example.com"), None);
        assert!(refusal(" ", "").is_some_and(|why| why.contains(PERSON_MAIL)));
        let long = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(refusal(&long, "").is_some_and(|why| why.contains("2048")));
        assert!(refusal(PERSON_MAIL, &long).is_some_and(|why| why.contains("2048")));
    }
}
