//! A trigger at a time a resource holds: which of the times read now have a
//! moment that has come, which are still to come, and which are never
//! answered.
//!
//! [`TriggerSpec::ResourceTime`](crate::shortcut::TriggerSpec::ResourceTime)
//! names a field of a resource's items, or the occurrences of an entry that
//! repeats, and a moment before, at or after each time found there. Whoever
//! keeps the automation reads the times as they stand now ([`Held`], inside
//! [`window`]) and asks [`due`] which moments have come. A time is named by
//! a key that stays when the time moves (an item's id; an entry and the
//! occurrence of its series), and what was answered is kept under that key
//! with the time it was answered for ([`Answered`]). From that follows all
//! of it:
//!
//! - a moment fires once for a time: the same key at the same time is never
//!   answered twice, however often it is read, also across a restart;
//! - a time that moved is another time under the same key, and its moment
//!   fires again when it comes;
//! - a time that is no longer held (its item removed, its occurrence
//!   cancelled or moved away) is not read, so nothing fires for it;
//! - late, after whoever keeps the time was down over the moment: a moment
//!   *before* a time is still answered while the time itself has not come,
//!   and dropped once it has, since a word ahead of something that has begun
//!   is ahead of nothing; a moment *at or after* a time is answered once
//!   however late, for as long as the time is still read ([`LATE_MS`]);
//! - a moment at or after a time that had come before the trigger was first
//!   seen never fires: an automation answers what happens from when it was
//!   set up, not everything its resource ever held.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The kind a trigger names for the calendars a person keeps on a calendar
/// server of their own, read by the holder that keeps their accounts: their
/// provider and the resource of it, as its actions are addressed. Its fields
/// are [`CALENDAR_START`] and [`CALENDAR_END`], one time for every
/// occurrence of every entry.
pub const PERSON_CALENDARS: &str = "tilo.accounts.calendars";
/// When each occurrence of an entry of [`PERSON_CALENDARS`] starts.
pub const CALENDAR_START: &str = "entries.*.start";
/// When each occurrence of an entry of [`PERSON_CALENDARS`] ends.
pub const CALENDAR_END: &str = "entries.*.end";

/// How long after its moment a moment at or after a time is still answered,
/// and so how far back times are read: a week. Whoever keeps the time and
/// was down for longer answers nothing for what passed before that.
pub const LATE_MS: i64 = 7 * DAY_MS;
/// How far past the next moment times are read, so the next one to wait for
/// is known: two days.
pub const AHEAD_MS: i64 = 2 * DAY_MS;
/// The furthest a moment may be from its time, either way: 300 days.
pub const MAX_OFFSET_SECS: i64 = 300 * 86_400;

const DAY_MS: i64 = 86_400_000;

/// One time a resource holds, as read now.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    /// What holds it, which stays the same when the time moves: an item's
    /// id, or an entry and the occurrence of its series.
    pub key: String,
    /// The time, ms since the epoch.
    pub at_ms: i64,
    /// The resource it was read from, as whoever read it names it.
    pub resource: String,
    /// What a person calls the item: its title or name; empty when it has
    /// none.
    pub label: String,
    /// The item's own fields: what the trigger's filter is held to and what
    /// the invocation is told.
    pub facts: Map<String, Value>,
}

/// What one trigger has answered, kept across restarts by whoever keeps its
/// time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    /// The trigger this is the progress of, as whoever keeps it marks one:
    /// another trigger in its place starts over.
    #[serde(default)]
    pub mark: String,
    /// When the trigger was first seen, ms since the epoch.
    #[serde(default)]
    pub since_ms: i64,
    /// By [`Held::key`]: the time its moment was answered for.
    #[serde(default)]
    pub times: BTreeMap<String, i64>,
}

impl Answered {
    /// The progress of the trigger marked `mark`, first seen at `now_ms`.
    #[must_use]
    pub fn starting(mark: &str, now_ms: i64) -> Self {
        Self {
            mark: mark.to_owned(),
            since_ms: now_ms,
            times: BTreeMap::new(),
        }
    }
}

/// What [`due`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Due<'a> {
    /// The times whose moment has come and is answered now, in the order
    /// they were read.
    pub fire: Vec<&'a Held>,
    /// The earliest moment still to come among the times read, ms since the
    /// epoch.
    pub next_ms: Option<i64>,
}

fn offset_ms(offset_secs: i64) -> i64 {
    offset_secs.saturating_mul(1000)
}

/// The times to read at `now_ms` for a moment `offset_secs` from each: from
/// the first ms to before the second. Behind it nothing can fire any more,
/// and it reaches [`AHEAD_MS`] past the times whose moment is now.
#[must_use]
pub fn window(offset_secs: i64, now_ms: i64) -> (i64, i64) {
    let offset = offset_ms(offset_secs);
    let from = if offset_secs < 0 {
        // Only a time that has not come has a moment before it left.
        now_ms
    } else {
        now_ms.saturating_sub(offset).saturating_sub(LATE_MS)
    };
    let to = now_ms.saturating_sub(offset).saturating_add(AHEAD_MS);
    (from, to)
}

/// Which of the times `held` have a moment `offset_secs` from them that has
/// come by `now_ms` and is answered now, each written into `answered` as it
/// is found, and the earliest moment still to come. What `answered` holds of
/// times that can no longer be read is dropped.
pub fn due<'a>(
    offset_secs: i64,
    held: &'a [Held],
    answered: &mut Answered,
    now_ms: i64,
) -> Due<'a> {
    let offset = offset_ms(offset_secs);
    let mut found = Due::default();
    for one in held {
        if answered.times.get(&one.key) == Some(&one.at_ms) {
            continue;
        }
        let moment = one.at_ms.saturating_add(offset);
        if moment > now_ms {
            found.next_ms = Some(found.next_ms.map_or(moment, |next| next.min(moment)));
            continue;
        }
        let comes = if offset_secs < 0 {
            // Late, and the time itself has not come: still ahead of it.
            one.at_ms > now_ms
        } else {
            moment >= answered.since_ms && moment >= now_ms.saturating_sub(LATE_MS)
        };
        if comes {
            answered.times.insert(one.key.clone(), one.at_ms);
            found.fire.push(one);
        }
    }
    let (from, _) = window(offset_secs, now_ms);
    answered.times.retain(|_, at_ms| *at_ms >= from);
    found
}

/// Whether an item with the fields `facts` is one `filter` takes: every
/// field the filter names holds the value it names. Text is compared without
/// regard to case or to the blanks around it. A field that holds several
/// values (an entry's categories) matches when one of them does, and so does
/// one that holds something with an address, a name or an id of its own (an
/// entry's guests) when one of those matches.
#[must_use]
pub fn matches(filter: &Map<String, Value>, facts: &Map<String, Value>) -> bool {
    filter
        .iter()
        .all(|(field, wanted)| facts.get(field).is_some_and(|held| holds(held, wanted)))
}

fn holds(held: &Value, wanted: &Value) -> bool {
    match (held, wanted) {
        (Value::Array(several), _) => several.iter().any(|one| holds(one, wanted)),
        (Value::String(held), Value::String(wanted)) => {
            held.trim().to_lowercase() == wanted.trim().to_lowercase()
        }
        (Value::Object(parts), Value::String(_)) => ["email", "name", "id"]
            .iter()
            .any(|part| parts.get(*part).is_some_and(|held| holds(held, wanted))),
        _ => held == wanted,
    }
}

/// What an invocation is told of the time that fired it, as its `event`
/// input: the `kind` and `field` the trigger names, the `resource` and the
/// `item` that hold the time and the item's `label`, the time itself
/// (`at_ms`, and `at` as a date and time in UTC), the moment it fired for
/// (`fires_ms`), how late that is answered (`late_ms`), how far the moment
/// is from the time (`offset_secs`), and the item's own fields (`facts`).
#[must_use]
pub fn told(
    kind: &str,
    field: &str,
    offset_secs: i64,
    held: &Held,
    now_ms: i64,
) -> Map<String, Value> {
    let fires_ms = held.at_ms.saturating_add(offset_ms(offset_secs));
    let at = jiff::Timestamp::from_millisecond(held.at_ms)
        .map(|at| format!("{at:.0}"))
        .unwrap_or_default();
    let told = json!({
        "kind": kind,
        "field": field,
        "resource": held.resource,
        "item": held.key,
        "label": held.label,
        "at_ms": held.at_ms,
        "at": at,
        "fires_ms": fires_ms,
        "late_ms": now_ms.saturating_sub(fires_ms).max(0),
        "offset_secs": offset_secs,
        "facts": Value::Object(held.facts.clone()),
    });
    match told {
        Value::Object(told) => told,
        _ => Map::new(),
    }
}

/// Why a trigger at a time a resource holds cannot be kept, when it cannot:
/// what it has to name, in words its writer can act on.
#[must_use]
pub fn refusal(kind: &str, field: &str, offset_secs: i64) -> Option<String> {
    if kind.trim().is_empty() {
        return Some("name the kind of resource that holds the time, in quotation marks".into());
    }
    if offset_secs.unsigned_abs() > MAX_OFFSET_SECS.unsigned_abs() {
        return Some(
            "a moment is at most 300 days before or after the time it is counted from".into(),
        );
    }
    if kind == PERSON_CALENDARS {
        return (field != CALENDAR_START && field != CALENDAR_END).then(|| {
            format!(
                "a person's calendars hold `{CALENDAR_START}` and `{CALENDAR_END}`, not `{field}`"
            )
        });
    }
    let named = field.split('.').all(|part| !part.trim().is_empty());
    (!named || !field.contains('.')).then(|| {
        "name the field that holds the time as the resource's collection, a star for each item \
         and the field: `issues.*.due_ms`"
            .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    /// A moment well after the epoch, so nothing saturates.
    const NOON: i64 = 1_800_000_000_000;

    fn held(key: &str, at_ms: i64) -> Held {
        Held {
            key: key.to_owned(),
            at_ms,
            resource: "work".into(),
            label: key.to_owned(),
            facts: Map::new(),
        }
    }

    fn keys(due: &Due<'_>) -> Vec<String> {
        due.fire.iter().map(|one| one.key.clone()).collect()
    }

    #[test]
    fn a_moment_before_a_time_fires_once_and_again_only_when_the_time_moves() {
        let quarter = -900;
        let mut answered = Answered::starting("m", NOON);
        let review = [held("review", NOON + HOUR)];

        // Not yet: the moment is 45 minutes away, and that is what to wait for.
        let early = due(quarter, &review, &mut answered, NOON);
        assert!(early.fire.is_empty());
        assert_eq!(early.next_ms, Some(NOON + 45 * MINUTE));

        // The moment comes: once, however often the time is read afterwards.
        let now = NOON + 45 * MINUTE;
        assert_eq!(keys(&due(quarter, &review, &mut answered, now)), ["review"]);
        for later in [now, now + MINUTE, now + 14 * MINUTE] {
            let again = due(quarter, &review, &mut answered, later);
            assert!(again.fire.is_empty(), "read again at {later}");
            assert_eq!(again.next_ms, None);
        }

        // The entry moves to an hour later: the same key holds another time,
        // which has its own moment.
        let moved = [held("review", NOON + 2 * HOUR)];
        let waits = due(quarter, &moved, &mut answered, now + MINUTE);
        assert!(waits.fire.is_empty());
        assert_eq!(waits.next_ms, Some(NOON + 2 * HOUR - 15 * MINUTE));
        let comes = due(
            quarter,
            &moved,
            &mut answered,
            NOON + 2 * HOUR - 15 * MINUTE,
        );
        assert_eq!(keys(&comes), ["review"]);

        // What is kept reads back, so a restart answers nothing twice.
        let kept: Answered =
            serde_json::from_value(serde_json::to_value(&answered).unwrap()).unwrap();
        let mut kept = kept;
        let after = due(quarter, &moved, &mut kept, NOON + 2 * HOUR - 10 * MINUTE);
        assert!(after.fire.is_empty());
    }

    #[test]
    fn a_time_no_longer_held_fires_nothing_and_each_occurrence_is_its_own_time() {
        let quarter = -900;
        let mut answered = Answered::starting("m", NOON);
        // Three days of one series; the middle one is cancelled before its
        // moment, so it is no longer read.
        let monday = held("standup/2026-11-16", NOON + HOUR);
        let tuesday = held("standup/2026-11-17", NOON + 25 * HOUR);
        let wednesday = held("standup/2026-11-18", NOON + 49 * HOUR);
        let all = [monday.clone(), tuesday, wednesday.clone()];
        assert_eq!(
            keys(&due(quarter, &all, &mut answered, NOON + 45 * MINUTE)),
            ["standup/2026-11-16"]
        );
        let without = [monday, wednesday];
        let tuesdays_moment = NOON + 25 * HOUR - 15 * MINUTE;
        assert!(
            due(quarter, &without, &mut answered, tuesdays_moment)
                .fire
                .is_empty()
        );
        assert_eq!(
            keys(&due(
                quarter,
                &without,
                &mut answered,
                NOON + 49 * HOUR - 15 * MINUTE
            )),
            ["standup/2026-11-18"]
        );
    }

    #[test]
    fn late_is_answered_before_a_time_only_while_that_time_has_not_come() {
        let quarter = -900;
        let meeting = [held("meeting", NOON + HOUR)];

        // Down over the moment, up five minutes before the meeting: late by
        // ten minutes, and still ahead of it.
        let mut answered = Answered::starting("m", NOON);
        let late = due(quarter, &meeting, &mut answered, NOON + 55 * MINUTE);
        assert_eq!(keys(&late), ["meeting"]);

        // Up again only once it has begun: nothing, then and ever.
        let mut answered = Answered::starting("m", NOON);
        for now in [NOON + HOUR, NOON + HOUR + MINUTE, NOON + 3 * HOUR] {
            assert!(due(quarter, &meeting, &mut answered, now).fire.is_empty());
        }

        // Set up ten minutes before a meeting: its moment has passed and the
        // meeting has not begun, so its person is still told.
        let mut answered = Answered::starting("m", NOON + 50 * MINUTE);
        let set_up_late = due(quarter, &meeting, &mut answered, NOON + 50 * MINUTE);
        assert_eq!(keys(&set_up_late), ["meeting"]);
    }

    #[test]
    fn a_moment_at_or_after_a_time_fires_once_however_late_but_not_for_the_past() {
        let day_after = 86_400;
        let due_time = NOON + HOUR;
        let task = [held("task", due_time)];

        // The keeper was down for three days over the moment: once, late.
        let mut answered = Answered::starting("m", NOON);
        let back = due_time + 4 * 24 * HOUR;
        assert_eq!(keys(&due(day_after, &task, &mut answered, back)), ["task"]);
        assert!(
            due(day_after, &task, &mut answered, back + MINUTE)
                .fire
                .is_empty()
        );

        // Down for longer than a week past the moment: no longer answered.
        let mut answered = Answered::starting("m", NOON);
        let much_later = due_time + 24 * HOUR + LATE_MS + MINUTE;
        assert!(
            due(day_after, &task, &mut answered, much_later)
                .fire
                .is_empty()
        );

        // The trigger is first seen after the moment: the past stays quiet,
        // what comes due from then on is answered.
        let seen = due_time + 30 * HOUR;
        let mut answered = Answered::starting("m", seen);
        let tasks = [held("old", due_time), held("new", seen - 23 * HOUR)];
        let first = due(day_after, &tasks, &mut answered, seen);
        assert!(first.fire.is_empty());
        assert_eq!(first.next_ms, Some(seen + HOUR));
        assert_eq!(
            keys(&due(day_after, &tasks, &mut answered, seen + HOUR)),
            ["new"]
        );

        // At the time itself.
        let mut answered = Answered::starting("m", NOON);
        assert!(due(0, &task, &mut answered, due_time - 1).fire.is_empty());
        assert_eq!(keys(&due(0, &task, &mut answered, due_time)), ["task"]);
    }

    #[test]
    fn what_was_answered_is_forgotten_only_once_its_time_can_no_longer_be_read() {
        let mut answered = Answered::starting("m", NOON);
        let task = [held("task", NOON + HOUR)];
        assert_eq!(keys(&due(0, &task, &mut answered, NOON + HOUR)), ["task"]);
        // Still read for a week, and remembered for as long.
        let within = NOON + HOUR + LATE_MS;
        assert!(due(0, &task, &mut answered, within).fire.is_empty());
        assert_eq!(answered.times.len(), 1);
        due(0, &[], &mut answered, within + MINUTE);
        assert!(answered.times.is_empty());
    }

    #[test]
    fn the_times_read_reach_from_what_may_still_fire_to_past_the_next_moment() {
        assert_eq!(window(-900, NOON), (NOON, NOON + 900_000 + AHEAD_MS));
        assert_eq!(window(0, NOON), (NOON - LATE_MS, NOON + AHEAD_MS));
        assert_eq!(
            window(3_600, NOON),
            (NOON - HOUR - LATE_MS, NOON - HOUR + AHEAD_MS)
        );
    }

    #[test]
    fn a_filter_takes_an_item_by_its_own_fields() {
        let facts = json!({
            "title": "Review",
            "categories": ["Board", "Deal, signed"],
            "allDay": false,
            "attendees": [{"name": "Bo", "email": "bo@example.test", "status": "accepted"}],
            "priority": 2,
        });
        let Value::Object(facts) = facts else {
            panic!("facts")
        };
        let takes = |filter: Value| {
            let Value::Object(filter) = filter else {
                panic!("filter")
            };
            matches(&filter, &facts)
        };
        assert!(takes(json!({})), "no filter takes everything");
        assert!(
            takes(json!({"categories": "board"})),
            "one of several, whatever its case"
        );
        assert!(takes(json!({"categories": " Deal, signed "})));
        assert!(
            !takes(json!({"categories": "Deal"})),
            "the whole value, not a part of it"
        );
        assert!(takes(json!({"attendees": "BO@example.test"})));
        assert!(takes(
            json!({"allDay": false, "priority": 2, "title": "review"})
        ));
        assert!(!takes(json!({"allDay": true})));
        assert!(
            !takes(json!({"categories": "board", "title": "Standup"})),
            "every match holds"
        );
        assert!(
            !takes(json!({"location": "Berlin"})),
            "a field the item does not hold"
        );
    }

    #[test]
    fn an_invocation_is_told_the_time_its_moment_and_how_late_it_is() {
        let mut review = held("review-1/2026-11-17", 1_794_906_000_000);
        review.label = "Review".into();
        review.facts.insert("title".into(), "Review".into());
        let told = told(
            PERSON_CALENDARS,
            CALENDAR_START,
            -900,
            &review,
            1_794_905_160_000,
        );
        assert_eq!(
            Value::Object(told),
            json!({
                "kind": "tilo.accounts.calendars",
                "field": "entries.*.start",
                "resource": "work",
                "item": "review-1/2026-11-17",
                "label": "Review",
                "at_ms": 1_794_906_000_000_i64,
                "at": "2026-11-17T09:00:00Z",
                "fires_ms": 1_794_905_100_000_i64,
                "late_ms": 60_000,
                "offset_secs": -900,
                "facts": {"title": "Review"},
            })
        );
    }

    #[test]
    fn a_trigger_that_cannot_be_kept_says_what_it_has_to_name() {
        assert_eq!(refusal(PERSON_CALENDARS, CALENDAR_START, -900), None);
        assert_eq!(refusal(PERSON_CALENDARS, CALENDAR_END, 0), None);
        assert_eq!(refusal("tasks.issues", "issues.*.due_ms", 86_400), None);
        assert!(
            refusal(PERSON_CALENDARS, "entries.*.title", 0)
                .is_some_and(|why| why.contains("entries.*.start"))
        );
        assert!(refusal("tasks.issues", "due", 0).is_some());
        assert!(refusal("tasks.issues", "issues..due_ms", 0).is_some());
        assert!(refusal("", "issues.*.due_ms", 0).is_some());
        assert!(refusal("tasks.issues", "issues.*.due_ms", MAX_OFFSET_SECS + 1).is_some());
        assert!(refusal("tasks.issues", "issues.*.due_ms", -MAX_OFFSET_SECS - 1).is_some());
        assert_eq!(
            refusal("tasks.issues", "issues.*.due_ms", -MAX_OFFSET_SECS),
            None
        );
    }
}
