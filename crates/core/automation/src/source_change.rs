//! A trigger on something new in a source
//! ([`TriggerSpec::SourceChanged`](crate::shortcut::TriggerSpec::SourceChanged)):
//! what a source is, what counts as new in it, and what an invocation is
//! told of it.
//!
//! A source holds things its person wants answered as they come, and is no
//! resource of the workspace: the mail the Mail app keeps is the first one
//! read. Whoever keeps the automation is told by the source, or looks at it,
//! and names each thing found there by a key that stays the same however
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
//!
//! A source outside that tells nobody of what is new, a feed or a page on
//! the web, is looked at on a clock by whoever keeps the automation
//! ([`looked_at`]). What a look found is compared here with what the last
//! one found and with nothing else, by no model: the entries of a feed by
//! the id each carries, a page by the lines a person would read there, a
//! time of day and an age left out of them ([`Seen::took`],
//! [`Seen::took_text`]). The bounds are here too: how often at the most,
//! how much of what is new one look hands over, how much longer a source
//! that fails is left alone each time, and when it counts as gone, which is
//! said once ([`Seen::failed`]).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The kind a trigger names for the mail a person's accounts receive, as the
/// Mail app keeps it: the messages the mail connector brings in. `at` names
/// one account by its id, and is empty for every account. What is new is one
/// message, with the fields it is compared by.
pub const PERSON_MAIL: &str = "tilo.accounts.mail";

/// The kind a trigger names for a feed on the web, RSS, Atom or JSON: `at`
/// is its address, and what is new is one entry, named by the id the feed
/// gives it.
pub const WEB_FEED: &str = "tilo.web.feed";

/// The kind a trigger names for a page on the web: `at` is its address, and
/// what is new is the lines a person would read there that were not there
/// at the last look.
pub const WEB_PAGE: &str = "tilo.web.page";

/// The longest a source's kind, or the name of one of its kind, may be.
pub const MAX_NAME_BYTES: usize = 2048;

/// The shortest time between two looks at a source, in seconds.
pub const MIN_LOOK_SECS: u64 = 300;
/// The time between two looks when a trigger names none, in seconds.
pub const DEFAULT_LOOK_SECS: u64 = 3600;
/// The longest time between two looks a trigger may name, in seconds.
pub const MAX_LOOK_SECS: u64 = 7 * 24 * 3600;
/// The longest a source that fails is left alone before it is looked at
/// again, in seconds: how often a source that is gone is still looked at.
pub const MAX_BACKOFF_SECS: u64 = 24 * 3600;
/// How many looks in a row fail before the source counts as gone.
pub const GONE_AFTER_FAILURES: u32 = 6;
/// The most new things one look hands over; the rest are taken in, counted
/// and never handed over.
pub const MAX_NEW_AT_A_LOOK: usize = 8;
/// The most keys kept of what a source held, and the most lines of a page
/// that are compared.
pub const MAX_KEYS: usize = 1024;
/// The most characters one fact of something new carries.
pub const MAX_FACT_CHARS: usize = 600;
/// The most characters of a page's new lines one look hands over.
pub const MAX_ADDED_CHARS: usize = 4000;

/// Whether a source of `kind` tells nobody of what is new, so whoever keeps
/// the automation looks at it on a clock.
#[must_use]
pub fn looked_at(kind: &str) -> bool {
    kind == WEB_FEED || kind == WEB_PAGE
}

/// How often a trigger that names `every_secs` is looked at, in seconds,
/// within the bounds.
#[must_use]
pub fn look_secs(every_secs: Option<u64>) -> u64 {
    every_secs
        .unwrap_or(DEFAULT_LOOK_SECS)
        .clamp(MIN_LOOK_SECS, MAX_LOOK_SECS)
}

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
    /// For a source looked at on a clock: what the looks made of it. None
    /// until the first look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub looked: Option<Looked>,
}

/// What the looks at a source on a clock have made of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Looked {
    /// Whether a look ever answered: until one did, nothing is new.
    #[serde(default)]
    pub answered: bool,
    /// What names each thing the source held at the last look that
    /// answered, at most [`MAX_KEYS`], the newest last.
    #[serde(default)]
    pub keys: Vec<String>,
    /// When it is looked at next, ms since the epoch.
    #[serde(default)]
    pub next_ms: i64,
    /// How many looks in a row failed.
    #[serde(default)]
    pub failures: u32,
    /// Why the source counts as gone, while it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gone: Option<Gone>,
}

/// A source that counts as gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gone {
    /// Since when, ms since the epoch.
    pub since_ms: i64,
    /// Why, in words for the automation's person.
    pub why: String,
    /// No later look can answer, so none is made: the address itself is
    /// refused.
    #[serde(default)]
    pub for_good: bool,
}

/// How a look that failed is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The source may answer the next time: it is left alone for longer
    /// after each failure in a row, and counts as gone after
    /// [`GONE_AFTER_FAILURES`] of them.
    Passing,
    /// The source answered that what was asked for is not there: it counts
    /// as gone at once, and is looked at as seldom as a gone one is.
    Gone,
    /// The address itself is refused: it counts as gone at once and is
    /// never looked at again.
    Refused,
}

/// What one look found that was not there at the look before.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct News {
    /// The new things handed over, at most [`MAX_NEW_AT_A_LOOK`], in the
    /// order they are answered.
    pub new: Vec<Found>,
    /// How many more were new, taken in and not handed over.
    pub passed_over: usize,
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
            looked: None,
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

    /// Whether a source looked at on a clock is due a look at `now_ms`: at
    /// once when it never was looked at, never when no look can answer.
    #[must_use]
    pub fn due(&self, now_ms: i64) -> bool {
        self.looked.as_ref().is_none_or(|looked| {
            !looked.gone.as_ref().is_some_and(|gone| gone.for_good) && now_ms >= looked.next_ms
        })
    }

    /// When a source looked at on a clock is looked at next, ms since the
    /// epoch: none before the first look, which is due at once, and none
    /// when no look can answer.
    #[must_use]
    pub fn next_look_ms(&self) -> Option<i64> {
        self.looked
            .as_ref()
            .filter(|looked| !looked.gone.as_ref().is_some_and(|gone| gone.for_good))
            .map(|looked| looked.next_ms)
    }

    /// Why the source counts as gone, while it does.
    #[must_use]
    pub fn gone(&self) -> Option<&Gone> {
        self.looked.as_ref().and_then(|looked| looked.gone.as_ref())
    }

    /// Take in what a look at `now_ms` found, each thing under its key, and
    /// say which of them were not there at the last look that answered: the
    /// places in `keys` of the new ones, none at the first look that
    /// answers. A source that lists what it held for a while (`listing`, a
    /// feed) keeps what it listed before beside what it lists now, so an
    /// entry that leaves the list and comes back is not new; another (a
    /// page) is compared with the last look alone. The next look is in
    /// `every_secs`, and a source that answered is not gone.
    fn take_in(
        &mut self,
        keys: &[String],
        listing: bool,
        every_secs: u64,
        now_ms: i64,
    ) -> Vec<usize> {
        let looked = self.looked.get_or_insert_with(Looked::default);
        let before: BTreeSet<&str> = looked.keys.iter().map(String::as_str).collect();
        let new = if looked.answered {
            keys.iter()
                .enumerate()
                .filter(|(_, key)| !before.contains(key.as_str()))
                .map(|(place, _)| place)
                .collect()
        } else {
            Vec::new()
        };
        let now: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        let mut kept: Vec<String> = if listing {
            looked
                .keys
                .iter()
                .filter(|key| !now.contains(key.as_str()))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        // What stands first in a list is its newest, and is kept last.
        kept.extend(keys.iter().rev().cloned());
        let over = kept.len().saturating_sub(MAX_KEYS);
        kept.drain(..over);
        looked.keys = kept;
        looked.answered = true;
        looked.failures = 0;
        looked.gone = None;
        looked.next_ms = after(now_ms, every_secs);
        new
    }

    /// Take in the things a look at `now_ms` found in a source that lists
    /// them, in the source's own order, and answer the new ones: none at
    /// the first look that answers, afterwards those whose key the source
    /// did not list before. At most [`MAX_NEW_AT_A_LOOK`] are handed over,
    /// those that stand first, the last of them first; how many more were
    /// new is said with the count and with the last one handed over.
    pub fn took(&mut self, found: Vec<Found>, every_secs: u64, now_ms: i64) -> News {
        let keys: Vec<String> = found.iter().map(|found| found.key.clone()).collect();
        let places = self.take_in(&keys, true, every_secs, now_ms);
        let passed_over = places.len().saturating_sub(MAX_NEW_AT_A_LOOK);
        let mut new: Vec<Found> = found
            .into_iter()
            .enumerate()
            .filter(|(place, _)| places.contains(place))
            .map(|(_, found)| found)
            .take(MAX_NEW_AT_A_LOOK)
            .collect();
        new.reverse();
        if passed_over > 0
            && let Some(last) = new.last_mut()
        {
            last.facts.insert("alsoNew".to_owned(), json!(passed_over));
        }
        News { new, passed_over }
    }

    /// Take in the text a look at `now_ms` read at `at`, a page called
    /// `title`, and answer what is new in it: nothing at the first look
    /// that answers, and nothing while the lines a person reads there are
    /// the ones of the last look, in whatever order and whatever the page
    /// says the time is ([`unclocked`]). Otherwise one thing: the lines
    /// that were not there before, and how many of the earlier ones are
    /// gone.
    pub fn took_text(
        &mut self,
        at: &str,
        title: Option<&str>,
        text: &str,
        every_secs: u64,
        now_ms: i64,
    ) -> Option<Found> {
        let lines: Vec<String> = text
            .lines()
            .map(unclocked)
            .filter(|line| !line.is_empty())
            .take(MAX_KEYS)
            .collect();
        let keys: Vec<String> = lines.iter().map(|line| mark_of(line)).collect();
        let first = !self.looked.as_ref().is_some_and(|looked| looked.answered);
        let before = self
            .looked
            .as_ref()
            .map_or_else(Vec::new, |looked| looked.keys.clone());
        let places = self.take_in(&keys, false, every_secs, now_ms);
        let removed = before.iter().filter(|key| !keys.contains(*key)).count();
        if first || (places.is_empty() && removed == 0) {
            return None;
        }
        let added: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(place, _)| places.contains(place))
            .map(|(_, line)| line.as_str())
            .collect();
        let mut facts = Map::new();
        if let Some(title) = title {
            facts.insert("title".to_owned(), json!(cut(title, MAX_FACT_CHARS)));
        }
        facts.insert(
            "added".to_owned(),
            json!(cut(&added.join("\n"), MAX_ADDED_CHARS)),
        );
        facts.insert("addedLines".to_owned(), json!(added.len()));
        facts.insert("removedLines".to_owned(), json!(removed));
        Some(Found {
            // The text as it stands now and when it was found so: a page
            // that goes back to what it said before has changed again.
            key: format!("{}@{now_ms}", mark_of(&keys.join(" "))),
            at: at.to_owned(),
            at_ms: now_ms,
            facts,
        })
    }

    /// Take in that the look at `now_ms` at the source at `at` failed, and
    /// why. The source is left alone for longer after each failure in a
    /// row, twice as long each time and never longer than
    /// [`MAX_BACKOFF_SECS`]. When this failure makes it count as gone, that
    /// is answered as the one thing to tell its person; later failures of a
    /// source that is gone answer nothing, and it is told again only after
    /// it answered in between.
    pub fn failed(
        &mut self,
        at: &str,
        why: &str,
        failure: Failure,
        every_secs: u64,
        now_ms: i64,
    ) -> Option<Found> {
        let looked = self.looked.get_or_insert_with(Looked::default);
        looked.failures = looked.failures.saturating_add(1);
        let gone = failure != Failure::Passing || looked.failures >= GONE_AFTER_FAILURES;
        let wait = if gone {
            MAX_BACKOFF_SECS.max(every_secs)
        } else {
            every_secs
                .saturating_mul(1_u64.checked_shl(looked.failures).unwrap_or(u64::MAX))
                .min(MAX_BACKOFF_SECS)
                .max(every_secs)
        };
        looked.next_ms = after(now_ms, wait);
        let for_good = failure == Failure::Refused;
        if let Some(already) = looked.gone.as_mut() {
            already.for_good = already.for_good || for_good;
            return None;
        }
        if !gone {
            return None;
        }
        let why = cut(why, MAX_FACT_CHARS);
        looked.gone = Some(Gone {
            since_ms: now_ms,
            why: why.clone(),
            for_good,
        });
        let facts = Map::from_iter([
            ("gone".to_owned(), json!(true)),
            ("why".to_owned(), json!(why)),
            ("failedLooks".to_owned(), json!(looked.failures)),
            ("lookedAtAgain".to_owned(), json!(!for_good)),
        ]);
        Some(Found {
            key: format!("gone@{now_ms}"),
            at: at.to_owned(),
            at_ms: now_ms,
            facts,
        })
    }
}

/// The moment `secs` seconds after `now_ms`, ms since the epoch.
fn after(now_ms: i64, secs: u64) -> i64 {
    now_ms.saturating_add(i64::try_from(secs.saturating_mul(1000)).unwrap_or(i64::MAX))
}

/// At most the first `chars` characters of `text`, its blanks collapsed
/// within each line.
#[must_use]
pub fn cut(text: &str, chars: usize) -> String {
    let collapsed: Vec<String> = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect();
    collapsed.join("\n").chars().take(chars).collect()
}

/// What stands for `text` among the keys kept of a source.
fn mark_of(text: &str) -> String {
    let hex = blake3::hash(text.as_bytes()).to_hex();
    hex.get(..16).unwrap_or_default().to_owned()
}

/// A word without the marks around it, in small letters.
fn bare(word: &str) -> String {
    word.trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_ascii_lowercase()
}

/// Whether `word` writes a time of day: digits, a colon and two digits, as
/// in `10:42`, `10:42:07` and `2026-11-17T10:42:07Z`.
fn is_time(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.iter().enumerate().any(|(at, byte)| {
        *byte == b':'
            && at
                .checked_sub(1)
                .and_then(|before| bytes.get(before))
                .is_some_and(u8::is_ascii_digit)
            && bytes
                .get(at.saturating_add(1))
                .is_some_and(u8::is_ascii_digit)
            && bytes
                .get(at.saturating_add(2))
                .is_some_and(u8::is_ascii_digit)
    })
}

/// Whether the three words count an age from now: `5 minutes ago`, `an
/// hour ago`.
fn is_age(count: &str, unit: &str, ago: &str) -> bool {
    let count = bare(count);
    let unit = bare(unit);
    let unit = unit.strip_suffix('s').unwrap_or(&unit);
    bare(ago) == "ago"
        && (matches!(count.as_str(), "a" | "an" | "one")
            || (!count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit())))
        && matches!(
            unit,
            "second" | "sec" | "minute" | "min" | "hour" | "hr" | "day" | "week" | "month" | "year"
        )
}

/// `line` as it is compared from one look to the next: its blanks
/// collapsed, and without what a page's own clock writes into it, a time of
/// day (with `am`, `pm` or a zone's letters after it) and an age counted
/// from now. A page that only says what time it is has not changed. A date
/// stays: a day that changed is something the page says.
#[must_use]
pub fn unclocked(line: &str) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    let mut at = 0_usize;
    while let Some(word) = words.get(at).copied() {
        let next = words.get(at.saturating_add(1)).copied();
        let third = words.get(at.saturating_add(2)).copied();
        if let (Some(unit), Some(ago)) = (next, third)
            && is_age(word, unit, ago)
        {
            at = at.saturating_add(3);
        } else if is_time(word) {
            let zone = next.is_some_and(|after| {
                matches!(
                    bare(after).as_str(),
                    "am" | "pm" | "a.m" | "p.m" | "utc" | "gmt"
                )
            });
            at = at.saturating_add(if zone { 2 } else { 1 });
        } else {
            kept.push(word);
            at = at.saturating_add(1);
        }
    }
    kept.join(" ")
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

/// Why a trigger on something new in the source of `kind` at `at`, looked at
/// every `every_secs`, cannot be kept for how it names its source and its
/// clock, when it cannot: in words for whoever described it. A source that
/// is looked at is named by its address on the web, and looked at no more
/// often than every [`MIN_LOOK_SECS`]; one that tells of what is new itself
/// has no clock. Whether the address may be reached is said by whoever
/// reaches it.
#[must_use]
pub fn look_refusal(kind: &str, at: &str, every_secs: Option<u64>) -> Option<String> {
    if !looked_at(kind) {
        return every_secs.map(|_| {
            format!(
                "\"{kind}\" tells of what is new as it comes and is not looked at on a clock: \
                 leave `looked at every` out"
            )
        });
    }
    let named = (at.starts_with("https://") || at.starts_with("http://"))
        && !at.chars().any(char::is_whitespace);
    if !named {
        return Some(format!(
            "a feed or a page is named by its address on the web: `when something new is in \
             \"{kind}\" at \"https://example.org/news\"`"
        ));
    }
    match every_secs {
        Some(seconds) if !(MIN_LOOK_SECS..=MAX_LOOK_SECS).contains(&seconds) => Some(format!(
            "a source is looked at every {MIN_LOOK_SECS} seconds at the most often and every \
             {MAX_LOOK_SECS} seconds at the least often; without `looked at every` it is every \
             {DEFAULT_LOOK_SECS} seconds"
        )),
        _ => None,
    }
}

/// Why a trigger on something new in the source of `kind` at `at` cannot be
/// kept, when it cannot: in words for whoever described it. Which kinds are
/// read is said by whoever reads them.
#[must_use]
pub fn refusal(kind: &str, at: &str) -> Option<String> {
    if kind.trim().is_empty() {
        return Some(format!(
            "a source is named by its kind: `when something new is in \"{WEB_FEED}\"`"
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
            key: "entry-1".into(),
            at: "https://example.org/feed.xml".into(),
            at_ms: 1_800_000_000_000,
            facts: Map::from_iter([("from".to_owned(), json!("bo@example.com"))]),
        };
        assert_eq!(
            Value::Object(told(WEB_FEED, &found)),
            json!({
                "kind": "tilo.web.feed",
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

    /// An entry of a feed, as whoever read the feed names it.
    fn entry(key: &str) -> Found {
        Found {
            key: key.to_owned(),
            at: "https://example.org/feed.xml".into(),
            at_ms: 0,
            facts: Map::from_iter([("title".to_owned(), json!(key))]),
        }
    }

    fn keys(news: &News) -> Vec<&str> {
        news.new.iter().map(|found| found.key.as_str()).collect()
    }

    #[test]
    fn a_feed_hands_over_what_it_did_not_list_before_once_and_nothing_at_the_first_look() {
        let mut seen = Seen::starting("feed", 0);
        assert!(seen.due(0), "a source never looked at is due at once");
        let first = seen.took(vec![entry("b"), entry("a")], 600, 1_000);
        assert!(first.new.is_empty(), "what it held at first is not new");
        assert!(!seen.due(600_999) && seen.due(601_000));
        assert_eq!(seen.next_look_ms(), Some(601_000));

        // A new entry stands first; the two it listed before are not new.
        let second = seen.took(vec![entry("c"), entry("b"), entry("a")], 600, 601_000);
        assert_eq!(keys(&second), ["c"]);
        assert_eq!(second.passed_over, 0);
        // The same list again holds nothing new, and neither does an entry
        // that left the list and came back.
        assert!(
            seen.took(vec![entry("c"), entry("b")], 600, 1_201_000)
                .new
                .is_empty()
        );
        assert!(
            seen.took(vec![entry("a"), entry("c")], 600, 1_801_000)
                .new
                .is_empty()
        );

        // Eleven new entries at one look: the eight that stand first are
        // handed over, the last of them first, and the rest are counted.
        let many: Vec<Found> = (0..11).map(|n| entry(&format!("n{n}"))).collect();
        let flood = seen.took(many.clone(), 600, 2_401_000);
        assert_eq!(
            keys(&flood),
            ["n7", "n6", "n5", "n4", "n3", "n2", "n1", "n0"]
        );
        assert_eq!(flood.passed_over, 3);
        assert_eq!(
            flood.new.last().and_then(|last| last.facts.get("alsoNew")),
            Some(&json!(3))
        );
        assert!(
            seen.took(many, 600, 3_001_000).new.is_empty(),
            "what was passed over was taken in, and is not new at the next look"
        );

        // What is kept reads back as it was.
        let kept = serde_json::to_value(&seen).unwrap_or_default();
        assert_eq!(serde_json::from_value::<Seen>(kept).ok(), Some(seen));
    }

    #[test]
    fn a_page_is_compared_by_the_lines_a_person_reads_and_not_by_its_clock() {
        let page = "https://example.org/prices";
        let mut seen = Seen::starting("page", 0);
        let at_first = "Prices\n\nSmall   plan: 10 a month\nLarge plan: 40 a month\nUpdated 10:42 am, 5 minutes ago";
        assert_eq!(
            seen.took_text(page, Some("Prices"), at_first, 600, 1_000),
            None
        );

        // The page's clock moved, its blanks changed and two lines swapped
        // places: nothing a person reads there is new.
        let clock_moved = "Prices\nLarge plan: 40 a month\nSmall plan: 10 a month\n\n\nUpdated 11:07 am, an hour ago";
        assert_eq!(
            seen.took_text(page, Some("Prices"), clock_moved, 600, 601_000),
            None
        );

        // A price changed: the new line is handed over, and that one line
        // of before is gone.
        let dearer = "Prices\nSmall plan: 12 a month\nLarge plan: 40 a month\nUpdated 12:00 pm, 2 minutes ago";
        let found = seen
            .took_text(page, Some("Prices"), dearer, 600, 1_201_000)
            .unwrap_or_else(|| panic!("a changed line is something new"));
        assert_eq!(found.at, page);
        assert_eq!(
            Value::Object(found.facts.clone()),
            json!({
                "title": "Prices",
                "added": "Small plan: 12 a month",
                "addedLines": 1,
                "removedLines": 1,
            })
        );
        assert_eq!(
            seen.took_text(page, Some("Prices"), dearer, 600, 1_801_000),
            None
        );

        // Back to what it said before: that is a change again, under a key
        // of its own.
        let back = seen
            .took_text(page, Some("Prices"), at_first, 600, 2_401_000)
            .unwrap_or_else(|| panic!("a page that goes back has changed"));
        assert_ne!(back.key, found.key);
        assert_eq!(
            back.facts.get("added"),
            Some(&json!("Small plan: 10 a month"))
        );

        assert_eq!(
            unclocked("  Updated   10:42:07 UTC,  3 hours ago. "),
            "Updated"
        );
        assert_eq!(
            unclocked("Released 3 December 2026"),
            "Released 3 December 2026"
        );
    }

    #[test]
    fn a_source_that_fails_is_left_alone_longer_each_time_and_is_said_to_be_gone_once() {
        let at = "https://example.org/feed.xml";
        let mut seen = Seen::starting("feed", 0);
        seen.took(vec![entry("a")], 600, 0);

        // Each failure in a row doubles the wait, up to a day.
        let mut now = 600_000_i64;
        let mut waits = Vec::new();
        let mut told = Vec::new();
        for _ in 0..7 {
            let notice = seen.failed(
                at,
                "the server answered HTTP 503",
                Failure::Passing,
                600,
                now,
            );
            let next = seen.next_look_ms().unwrap_or(now);
            waits.push(next.saturating_sub(now) / 1000);
            told.push(notice.is_some());
            assert!(!seen.due(next.saturating_sub(1)) && seen.due(next));
            now = next;
        }
        assert_eq!(waits, [1_200, 2_400, 4_800, 9_600, 19_200, 86_400, 86_400]);
        assert_eq!(
            told,
            [false, false, false, false, false, true, false],
            "the sixth failure in a row says the source is gone, once"
        );
        let gone = seen.gone().cloned().unwrap_or_else(|| panic!("it is gone"));
        assert_eq!(gone.why, "the server answered HTTP 503");
        assert!(
            !gone.for_good,
            "a source that is gone is still looked at, once a day"
        );

        // It answers again: it is not gone, what it lists now that it did
        // not list before is new, and a later failure starts over.
        let back = seen.took(vec![entry("b"), entry("a")], 600, now);
        assert_eq!(keys(&back), ["b"]);
        assert_eq!(seen.gone(), None);
        assert_eq!(
            seen.failed(at, "timed out", Failure::Passing, 600, now),
            None
        );
        assert_eq!(seen.next_look_ms(), Some(now.saturating_add(1_200_000)));

        // A source that says the thing is not there is gone at once, and
        // looked at again a day later.
        let mut missing = Seen::starting("feed", 0);
        let notice = missing
            .failed(
                at,
                "there is no page at this address (HTTP 404)",
                Failure::Gone,
                600,
                5_000,
            )
            .unwrap_or_else(|| panic!("gone at once"));
        assert_eq!(notice.key, "gone@5000");
        assert_eq!(notice.facts.get("gone"), Some(&json!(true)));
        assert_eq!(notice.facts.get("lookedAtAgain"), Some(&json!(true)));
        assert_eq!(missing.next_look_ms(), Some(86_405_000));
        assert_eq!(
            missing.failed(at, "still not there", Failure::Gone, 600, 86_405_000),
            None
        );

        // An address that is refused is gone for good: no look is ever due.
        let mut refused = Seen::starting("page", 0);
        let notice = refused
            .failed(
                at,
                "it is on this device or its local network",
                Failure::Refused,
                600,
                5_000,
            )
            .unwrap_or_else(|| panic!("gone at once"));
        assert_eq!(notice.facts.get("lookedAtAgain"), Some(&json!(false)));
        assert!(!refused.due(i64::MAX));
        assert_eq!(refused.next_look_ms(), None);
    }

    #[test]
    fn a_source_looked_at_is_named_by_its_address_and_looked_at_within_its_bounds() {
        let feed = "https://example.org/feed.xml";
        assert!(looked_at(WEB_FEED) && looked_at(WEB_PAGE));
        assert_eq!(look_refusal(WEB_FEED, feed, None), None);
        assert_eq!(
            look_refusal(WEB_PAGE, "http://example.org/", Some(300)),
            None
        );
        assert_eq!(look_secs(None), 3_600);
        assert_eq!(look_secs(Some(1)), 300);
        for unnamed in [
            "",
            "example.org/feed.xml",
            "ftp://example.org/",
            "https://example.org/a b",
        ] {
            assert!(
                look_refusal(WEB_FEED, unnamed, None).is_some_and(|why| why.contains("address")),
                "{unnamed}"
            );
        }
        for seconds in [0, 299, 604_801] {
            assert!(
                look_refusal(WEB_PAGE, feed, Some(seconds)).is_some_and(|why| why.contains("300")),
                "{seconds}"
            );
        }
    }

    #[test]
    fn a_source_with_no_kind_or_a_name_past_its_bound_is_refused_in_words() {
        let feed = "https://example.org/feed.xml";
        assert_eq!(refusal(WEB_FEED, feed), None);
        assert!(refusal(" ", "").is_some_and(|why| why.contains(WEB_FEED)));
        let long = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(refusal(&long, "").is_some_and(|why| why.contains("2048")));
        assert!(refusal(WEB_FEED, &long).is_some_and(|why| why.contains("2048")));
    }
}
