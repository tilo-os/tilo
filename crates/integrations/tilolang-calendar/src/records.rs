//! The times the Calendar app's records hold, as its own views read them: an entry that does not
//! repeat once, a series on every date its rule falls on, the dates it leaves out left out, and a
//! date a record standing apart from its series covers left to that record.
//!
//! The records are what the calendar bridge wrote into the app's store, each field by the name the
//! app declares it under. Nothing here reads a calendar server or a store: a reader hands the
//! records over, and gets back each time the field names, the item it belongs to and the fields
//! that item holds.
//!
//! An entry's wall times are read in the zone its `timezone` names, and a `floating` one is read
//! where its reader is (UTC here, as the shared expander reads one in a view): the same answer
//! the Calendar app shows.

use jiff::civil::{Date, DateTime, Time as WallTime};
use serde_json::{Map, Value};

use tilo_recurrence::Rule;

/// The field naming when an occurrence of an entry starts.
const START: &str = "start";
/// The field naming when an occurrence of an entry ends.
const END: &str = "end";

/// One record of the app: the id the store keeps it by, and the fields it holds.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub fields: Map<String, Value>,
}

/// One time an entry holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Moment {
    /// The entry and the date of the occurrence in it, which stay when the time moves: an
    /// occurrence moved to another hour is the same one, a date the series leaves out is none.
    pub key: String,
    /// The time `START` or `END` names, milliseconds since the epoch.
    pub at_ms: i64,
    /// The entry's own fields, with this occurrence's wall times: what a trigger's filter reads
    /// and what the invocation is told.
    pub facts: Map<String, Value>,
}

/// The time `field` names of every occurrence of every entry of `entries`, from `from_ms` to
/// before `to_ms`, earliest first.
#[must_use]
pub fn times(entries: &[Entry], field: &str, from_ms: i64, to_ms: i64) -> Vec<Moment> {
    // A date a record standing apart from a series covers is the series' own no longer.
    let mut covered: std::collections::HashMap<String, std::collections::BTreeSet<Date>> =
        std::collections::HashMap::new();
    for entry in entries {
        let Some((series, date)) = standing_apart(entry) else {
            continue;
        };
        covered.entry(series).or_default().insert(date);
    }
    let mut found = Vec::new();
    for entry in entries {
        let Some(held) = Held::of(entry) else {
            continue;
        };
        found.extend(held.times(field, from_ms, to_ms, covered.get(&entry.id)));
    }
    found.sort_by(|one, other| (one.at_ms, &one.key).cmp(&(other.at_ms, &other.key)));
    found
}

/// The series `entry` stands apart from, by its record's id, and the date it stands in for, as
/// the record itself says.
fn standing_apart(entry: &Entry) -> Option<(String, Date)> {
    let master = text(&entry.fields, "recurrence_of").filter(|master| !master.is_empty())?;
    let original = text(&entry.fields, "original_start")?;
    Some((record_id(master), wall(original)?.date()))
}

/// The record id an address or a plain id names.
fn record_id(named: &str) -> String {
    named.rsplit([':', '/']).next().unwrap_or(named).to_owned()
}

/// One entry as its record holds it, read once.
struct Held {
    /// What names the entry for good: its key, or the record's id where it holds none.
    key: String,
    zone: String,
    start: DateTime,
    end: DateTime,
    cancelled: bool,
    /// Its repeat as the shared expander reads it; empty for one that does not repeat.
    rule: String,
    exdates: Vec<Date>,
    /// The date of the series this record stands in for, where it is one date of a series.
    apart: Option<Date>,
    /// The record's own fields, as an invocation is told them.
    facts: Map<String, Value>,
}

impl Held {
    fn of(entry: &Entry) -> Option<Self> {
        let fields = &entry.fields;
        let zone = zone_of(fields);
        let start = wall(text(fields, "start")?)?;
        // A zone this crate cannot read leaves the entry's times unknown, and nothing fires.
        in_zone(zone, start)?;
        let end = wall(text(fields, "end")?).unwrap_or(start);
        let key = text(fields, "key")
            .filter(|key| !key.is_empty())
            .unwrap_or(entry.id.as_str());
        Some(Self {
            key: key.to_owned(),
            zone: zone.to_owned(),
            start,
            end,
            cancelled: flag(fields, "cancelled"),
            rule: rule_of(fields),
            exdates: set(fields, "exdates")
                .iter()
                .filter_map(|day| date(day))
                .collect(),
            apart: standing_apart(entry).map(|(_, date)| date),
            facts: fields.clone(),
        })
    }

    /// Every time `field` names that this entry holds between the window's ends, without the
    /// dates `covered` leaves to the records standing apart from it.
    fn times(
        &self,
        field: &str,
        from_ms: i64,
        to_ms: i64,
        covered: Option<&std::collections::BTreeSet<Date>>,
    ) -> Vec<Moment> {
        if self.cancelled {
            return Vec::new();
        }
        // An entry that stands apart from a series is one date of it, wherever it was moved to.
        if let Some(date) = self.apart {
            return self.one(field, date, from_ms, to_ms).into_iter().collect();
        }
        if self.rule.trim().is_empty() {
            return self
                .one(field, self.start.date(), from_ms, to_ms)
                .into_iter()
                .collect();
        }
        let Some(rule) = Rule::parse(&self.rule) else {
            return Vec::new();
        };
        // A day either side of the window holds every time of the zone the window's ends fall in.
        let (from, to) = (self.day_of(from_ms), self.day_of(to_ms));
        let Some((from, to)) = from.zip(to) else {
            return Vec::new();
        };
        let reach = rule.with_exdates(self.exdates.iter().copied());
        let (from, to) = (
            from.saturating_sub(jiff::Span::new().days(1)),
            to.saturating_add(jiff::Span::new().days(1)),
        );
        reach
            .occurrences(self.start.date(), from, to)
            .into_iter()
            .filter(|date| !self.exdates.contains(date))
            .filter(|date| covered.is_none_or(|covered| !covered.contains(date)))
            .filter_map(|date| self.one(field, date, from_ms, to_ms))
            .collect()
    }

    /// This entry's one occurrence on `date`, where it is a time `field` names inside the window.
    fn one(&self, field: &str, date: Date, from_ms: i64, to_ms: i64) -> Option<Moment> {
        let start = self.at(date)?;
        let end = self.end_at(date)?;
        let at_ms = if field == END { end } else { start };
        if at_ms < from_ms || at_ms >= to_ms {
            return None;
        }
        let mut facts = self.facts.clone();
        facts.insert(
            START.to_owned(),
            Value::String(wall_text(date, self.start.time())),
        );
        facts.insert(
            END.to_owned(),
            Value::String(wall_text(date, self.end.time())),
        );
        Some(Moment {
            key: format!("{}/{date}", self.key),
            at_ms,
            facts,
        })
    }

    /// When this entry starts on `date`, in the zone it names.
    fn at(&self, date: Date) -> Option<i64> {
        in_zone(&self.zone, date.to_datetime(self.start.time()))
    }

    /// When it ends on its `date`'s occurrence: the length it has, read off the entry's own end.
    fn end_at(&self, date: Date) -> Option<i64> {
        let length = self.end.duration_since(self.start);
        let end = date
            .to_datetime(self.start.time())
            .checked_add(length)
            .ok()?;
        in_zone(&self.zone, end)
    }

    /// The day `ms` is on where this entry's times are read.
    fn day_of(&self, ms: i64) -> Option<Date> {
        let at = jiff::Timestamp::from_millisecond(ms).ok()?;
        let zone = tilo_recurrence::zone::named(&self.zone).unwrap_or(jiff::tz::TimeZone::UTC);
        Some(at.to_zoned(zone).date())
    }
}

/// The zone an entry's wall times are read in: its own, and UTC for one written nowhere in
/// particular.
fn zone_of(fields: &Map<String, Value>) -> &str {
    match text(fields, "timezone").unwrap_or_default() {
        "" | "floating" => "UTC",
        zone => zone,
    }
}

/// The repeat of `fields` as the shared expander reads it: its frequency, how often it comes, and
/// how it ends.
fn rule_of(fields: &Map<String, Value>) -> String {
    let frequency = text(fields, "recurrence").unwrap_or_default();
    if frequency.trim().is_empty() {
        return String::new();
    }
    let mut parts = vec![frequency.to_owned()];
    let every = integer(fields, "recurrence_every").unwrap_or(1);
    if every > 1 {
        parts.push(format!("INTERVAL={every}"));
    }
    if let Some(until) = text(fields, "recurrence_until").filter(|until| !until.is_empty()) {
        parts.push(format!("UNTIL={}", until.replace('-', "")));
    }
    if let Some(count) = integer(fields, "recurrence_count").filter(|count| *count > 0) {
        parts.push(format!("COUNT={count}"));
    }
    let weekdays = set(fields, "recurrence_weekdays");
    if !weekdays.is_empty() {
        let mut days = weekdays;
        days.sort_unstable();
        parts.push(format!("BYDAY={}", days.join(",")));
    }
    parts.join(";")
}

/// The instant the wall time `at` is in `zone`.
fn in_zone(zone: &str, at: DateTime) -> Option<i64> {
    let instant = tilo_recurrence::zone::wall_to_utc(zone, at)
        .map(|(instant, _)| instant)
        .or_else(|| match zone {
            "" | "floating" => Some(at.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp()),
            _ => None,
        })?;
    Some(instant.as_millisecond())
}

/// The wall time `text` writes: `YYYY-MM-DDTHH:MM:SS`, a space in place of the `T`, and a trailing
/// `Z` read as the wall time it names.
fn wall(text: &str) -> Option<DateTime> {
    let text = text.trim().trim_end_matches('Z');
    let (day, time) = text.split_once(['T', ' '])?;
    let day = Date::strptime("%Y-%m-%d", day).ok()?;
    let time = WallTime::strptime("%H:%M:%S", time).ok()?;
    Some(day.to_datetime(time))
}

/// The wall time `date` and `time` write, as a record keeps one.
fn wall_text(date: Date, time: WallTime) -> String {
    format!("{date}T{time}")
}

/// The day `text` names, as a record keeps one.
fn date(text: &str) -> Option<Date> {
    Date::strptime("%Y-%m-%d", text.trim()).ok()
}

fn text<'a>(fields: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    fields.get(name)?.as_str()
}

fn integer(fields: &Map<String, Value>, name: &str) -> Option<i64> {
    fields.get(name)?.as_i64()
}

fn flag(fields: &Map<String, Value>, name: &str) -> bool {
    fields.get(name).and_then(Value::as_bool) == Some(true)
}

/// The members of the set `name`, each as the record writes it.
fn set<'a>(fields: &'a Map<String, Value>, name: &str) -> Vec<&'a str> {
    fields
        .get(name)
        .and_then(Value::as_array)
        .map(|members| members.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One record `id` holds, with `fields` as the app keeps them.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "a test writes each record's fields as one json value"
    )]
    fn entry(id: &str, fields: Value) -> Entry {
        Entry {
            id: id.to_owned(),
            fields: fields.as_object().cloned().unwrap_or_default(),
        }
    }

    /// What [`times`] answered between the moments `from` and `to`, each as its item and instant.
    fn held(entries: &[Entry], field: &str, from: &str, to: &str) -> Vec<(String, i64)> {
        let moment = |text: &str| {
            tilo_recurrence::zone::wall_instant("UTC", wall(text).unwrap()).as_millisecond()
        };
        times(entries, field, moment(from), moment(to))
            .into_iter()
            .map(|one| (one.key, one.at_ms))
            .collect()
    }

    /// A time as the tests write it, in milliseconds.
    fn at(text: &str) -> i64 {
        text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn a_series_fires_on_each_date_its_rule_falls_on_without_the_days_it_leaves_out() {
        let standup = entry(
            "standup-1",
            serde_json::json!({
                "key": "standup-1@example.org",
                "title": "Standup",
                "start": "2026-10-19T09:30:00",
                "end": "2026-10-19T10:00:00",
                "timezone": "Europe/Berlin",
                "recurrence": "FREQ=DAILY",
                "recurrence_count": 4,
                "exdates": ["2026-10-21"],
                "categories": ["Board"],
            }),
        );
        assert_eq!(
            held(
                &[standup],
                START,
                "2026-10-19T00:00:00",
                "2026-10-24T00:00:00"
            ),
            [
                (
                    "standup-1@example.org/2026-10-19".to_owned(),
                    at("2026-10-19T07:30:00Z")
                ),
                (
                    "standup-1@example.org/2026-10-20".to_owned(),
                    at("2026-10-20T07:30:00Z")
                ),
                (
                    "standup-1@example.org/2026-10-22".to_owned(),
                    at("2026-10-22T07:30:00Z")
                ),
            ]
        );
    }

    #[test]
    fn a_date_a_record_stands_apart_for_is_that_records_time_where_it_was_moved_to() {
        let series = entry(
            "standup-1",
            serde_json::json!({
                "key": "standup-1@example.org",
                "start": "2026-10-19T09:30:00",
                "end": "2026-10-19T10:00:00",
                "timezone": "Europe/Berlin",
                "recurrence": "FREQ=DAILY",
                "recurrence_count": 2,
            }),
        );
        let moved = entry(
            "standup-1-2026-10-20",
            serde_json::json!({
                "key": "standup-1@example.org",
                "start": "2026-10-20T14:00:00",
                "end": "2026-10-20T14:30:00",
                "timezone": "Europe/Berlin",
                "original_start": "2026-10-20T09:30:00",
                "recurrence_of": "tl:so.tilo.calendar:standup-1",
            }),
        );
        assert_eq!(
            held(
                &[series, moved],
                START,
                "2026-10-19T00:00:00",
                "2026-10-22T00:00:00"
            ),
            [
                (
                    "standup-1@example.org/2026-10-19".to_owned(),
                    at("2026-10-19T07:30:00Z")
                ),
                // The moved one is the series' own date, at the hour it was moved to.
                (
                    "standup-1@example.org/2026-10-20".to_owned(),
                    at("2026-10-20T12:00:00Z")
                ),
            ]
        );
    }

    #[test]
    fn an_end_is_the_length_the_entry_holds_after_and_a_cancelled_date_is_no_time() {
        let series = entry(
            "standup-1",
            serde_json::json!({
                "key": "standup-1@example.org",
                "start": "2026-10-26T09:30:00",
                "end": "2026-10-26T10:00:00",
                "timezone": "Europe/Berlin",
                "recurrence": "FREQ=DAILY",
                "recurrence_count": 2,
            }),
        );
        let cancelled = entry(
            "standup-1-2026-10-27",
            serde_json::json!({
                "key": "standup-1@example.org",
                "start": "2026-10-27T09:30:00",
                "end": "2026-10-27T10:00:00",
                "timezone": "Europe/Berlin",
                "original_start": "2026-10-27T09:30:00",
                "recurrence_of": "tl:so.tilo.calendar:standup-1",
                "cancelled": true,
            }),
        );
        assert_eq!(
            held(
                &[series, cancelled],
                END,
                "2026-10-26T00:00:00",
                "2026-10-29T00:00:00"
            ),
            [(
                "standup-1@example.org/2026-10-26".to_owned(),
                at("2026-10-26T09:00:00Z")
            )],
            "09:30 in winter time, and half an hour long"
        );
    }
}
