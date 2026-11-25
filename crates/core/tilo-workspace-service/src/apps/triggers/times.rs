//! The triggers at a time a resource holds ([`TriggerSpec::ResourceTime`]): the times are read as
//! they stand now, and each moment that has come fires once
//! ([`tilo_automation::resource_time`]).
//!
//! The times read here are the Calendar app's: its entries, as the workspace's store holds the
//! records the calendar bridge keeps for it, every occurrence of an entry that repeats among
//! them, the dates it leaves out left out, and a date a record standing apart from its series
//! covers left to that record. The repeat itself is expanded by the one expander every view and
//! app reads a calendar through ([`tilo_recurrence::days`]). Nothing of a calendar is kept by the
//! host that runs the shortcut, and nothing is asked of a calendar server: the app publishes its
//! records and the trigger reads them as it reads any app's.
//!
//! What each trigger answered is kept in the workspace ([`super::keep_progress`]), so a moment
//! answered is not answered again after a restart, and the device that looks next carries on
//! from it. Where two devices read the same moment at once, the turn's lease keeps it answered
//! once: its invocation id is derived from the event ([`super::dispatched`]), so both name one
//! firing.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use jiff::civil::{Date, DateTime, Time as WallTime};
use serde::Deserialize;
use serde_json::{Map, Value};
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::{Event, TimeEvent};
use tilo_automation::resource_time::{self, Answered, Held};

use super::{Armed, Runner, dispatched, fields_of, keep_progress, kept_progress};

/// The longest the times wait before they are read again, so a time another client moved or
/// added is found within it: as often as the runner looks by itself.
const LOOK_AGAIN: Duration = Duration::from_secs(30);

/// The most records one look reads: every entry of every calendar of a workspace, well past what
/// one person keeps.
const MOST_RECORDS: usize = 10_000;

/// The field naming when an occurrence of an entry starts.
const START: &str = "start";
/// The field naming when an occurrence of an entry ends.
const END: &str = "end";

/// What one trigger at a time a resource holds has answered, and when its times are read next.
pub(super) struct Progress {
    answered: Answered,
    /// When the times are read next, ms since the epoch: the next moment still to come, and no
    /// later than [`LOOK_AGAIN`] after the last read.
    read_at_ms: i64,
}

impl Progress {
    /// The progress of the trigger marked `mark`, first seen at `now_ms`.
    fn starting(mark: &str, now_ms: i64) -> Self {
        Self {
            answered: Answered::starting(mark, now_ms),
            read_at_ms: now_ms,
        }
    }
}

/// The times `field` names in the records of `kind` that `filter` takes, from `from_ms` to before
/// `to_ms`.
async fn times_read(
    runner: &Runner,
    armed: &Armed,
    kind: &str,
    field: &str,
    filter: &Map<String, Value>,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Held>, String> {
    let found = runner
        .workspaces
        .store_found(
            &armed.workspace,
            collab_types::store::Query::of(kind, MOST_RECORDS),
        )
        .await?;
    let entries: Vec<(String, Map<String, Value>)> = found
        .into_iter()
        .map(|(id, held)| (id, fields_of(&held)))
        .collect();
    Ok(times(&entries, field, from_ms, to_ms)
        .into_iter()
        .filter(|one| resource_time::matches(filter, &one.facts))
        .collect())
}

/// The time `LOOK_AGAIN` is, in milliseconds.
fn look_again_ms() -> i64 {
    i64::try_from(LOOK_AGAIN.as_millis()).unwrap_or(i64::MAX)
}

impl Runner {
    /// Fire the triggers at a time a resource holds whose moment has come, each moment answered
    /// once ([`resource_time::due`]). The times are read at the next moment still to come, and
    /// otherwise every [`LOOK_AGAIN`], so a time that moved is found.
    pub(super) async fn moments(&mut self, now_ms: i64) {
        let mut firings = Vec::new();
        for armed in &self.armed {
            for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::ResourceTime {
                    kind,
                    field,
                    offset_secs,
                    filter,
                } = trigger
                else {
                    continue;
                };
                // The Calendar app's entries are the one kind of resource whose times are read
                // here; a kind this host knows nothing of fires nothing.
                if kind != resource_time::CALENDAR_EVENTS {
                    continue;
                }
                let key = (armed.workspace.clone(), armed.shortcut.id.clone(), index);
                let mark = tilo_peer_state::trigger_mark(trigger);
                // A trigger first seen here, or another trigger in this one's place, carries on
                // from what the workspace kept of it, and starts over where nothing was.
                if self
                    .times
                    .get(&key)
                    .is_none_or(|progress| progress.answered.mark != mark)
                {
                    let kept = kept_progress::<Answered>(&self.workspaces, armed, index, &mark)
                        .await
                        .filter(|answered| answered.mark == mark);
                    self.times.insert(
                        key.clone(),
                        kept.map_or_else(
                            || Progress::starting(&mark, now_ms),
                            |answered| Progress {
                                answered,
                                read_at_ms: now_ms,
                            },
                        ),
                    );
                }
                if self
                    .times
                    .get(&key)
                    .is_some_and(|progress| now_ms < progress.read_at_ms)
                {
                    continue;
                }
                let (from_ms, to_ms) = resource_time::window(*offset_secs, now_ms);
                let times = times_read(self, armed, kind, field, filter, from_ms, to_ms).await;
                let Some(progress) = self.times.get_mut(&key) else {
                    continue;
                };
                let before = progress.answered.clone();
                match times {
                    Ok(times) => {
                        let due = resource_time::due(
                            *offset_secs,
                            &times,
                            &mut progress.answered,
                            now_ms,
                        );
                        progress.read_at_ms = due.next_ms.map_or_else(
                            || now_ms.saturating_add(look_again_ms()),
                            |at| at.min(now_ms.saturating_add(look_again_ms())),
                        );
                        for one in due.fire {
                            let event = TimeEvent {
                                workspace: armed.workspace.clone(),
                                trigger: index,
                                key: one.key.clone(),
                                at_ms: one.at_ms,
                                payload: resource_time::told(
                                    kind,
                                    field,
                                    *offset_secs,
                                    one,
                                    now_ms,
                                ),
                            };
                            firings.extend(dispatched(
                                std::slice::from_ref(armed),
                                Event::Time(&event),
                                now_ms,
                            ));
                        }
                    }
                    // Times that could not be read are looked at again, and nothing is answered
                    // or forgotten for them meanwhile.
                    Err(error) => {
                        tracing::warn!(%error, shortcut = %armed.shortcut.id, "the times a shortcut waits on could not be read; it looks again");
                        progress.read_at_ms = now_ms.saturating_add(look_again_ms());
                    }
                }
                // What was answered is kept in the workspace, so a moment answered here is not
                // answered again after a restart or on the device that looks next.
                if progress.answered != before {
                    let answered = progress.answered.clone();
                    keep_progress(&self.workspaces, armed, index, &mark, &answered).await;
                }
            }
        }
        let waited: BTreeSet<(String, String, usize)> = self
            .armed
            .iter()
            .flat_map(|armed| {
                armed
                    .shortcut
                    .triggers
                    .iter()
                    .enumerate()
                    .filter(|(_, trigger)| matches!(trigger, TriggerSpec::ResourceTime { .. }))
                    .map(|(index, _)| (armed.workspace.clone(), armed.shortcut.id.clone(), index))
            })
            .collect();
        self.times.retain(|key, _| waited.contains(key));
        self.offer(firings);
    }
}

/// One entry's fields, as the Calendar app's record keeps them.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Fields {
    key: String,
    title: String,
    start: String,
    end: String,
    timezone: String,
    recurrence: String,
    recurrence_every: i64,
    recurrence_until: Option<String>,
    recurrence_count: Option<i64>,
    recurrence_weekdays: Vec<String>,
    exdates: Vec<String>,
    cancelled: Option<bool>,
    recurrence_of: Option<String>,
    original_start: String,
}

/// The times `field` names of every occurrence of every entry of `entries`, from `from_ms` to
/// before `to_ms`, earliest first. Each is named by its entry and the date of the occurrence in
/// it, which stay when the time moves: an occurrence moved to another hour is the same one.
fn times(
    entries: &[(String, Map<String, Value>)],
    field: &str,
    from_ms: i64,
    to_ms: i64,
) -> Vec<Held> {
    // A date a record standing apart from a series covers is the series' own no longer.
    let mut covered: HashMap<String, BTreeSet<Date>> = HashMap::new();
    for (_, record) in entries {
        let fields = Fields::of(record);
        if let Some((series, date)) = fields.standing_apart() {
            covered.entry(series).or_default().insert(date);
        }
    }
    let mut found = Vec::new();
    for (id, record) in entries {
        found.extend(of_record(id, record, field, from_ms, to_ms, &covered));
    }
    found.sort_by(|one, other| (one.at_ms, &one.key).cmp(&(other.at_ms, &other.key)));
    found
}

impl Fields {
    fn of(record: &Map<String, Value>) -> Self {
        tilo_failure::ignored!(
            "a record whose fields do not read as the app declares them holds no time, and nothing fires for it",
            serde_json::from_value(Value::Object(record.clone())).unwrap_or_default()
        )
    }

    /// The series this record stands apart from, by its record's id, and the date it stands in
    /// for, as the record itself says.
    fn standing_apart(&self) -> Option<(String, Date)> {
        let master = self
            .recurrence_of
            .as_deref()
            .filter(|master| !master.is_empty())?;
        let series = master
            .rsplit([':', '/'])
            .next()
            .unwrap_or(master)
            .to_owned();
        Some((series, wall(&self.original_start)?.date()))
    }

    /// The repeat as the shared expander reads it: its frequency, how often it comes, and how it
    /// ends.
    fn rule(&self) -> String {
        if self.recurrence.trim().is_empty() {
            return String::new();
        }
        let mut parts = vec![self.recurrence.clone()];
        if self.recurrence_every > 1 {
            parts.push(format!("INTERVAL={}", self.recurrence_every));
        }
        if let Some(until) = self
            .recurrence_until
            .as_deref()
            .filter(|until| !until.is_empty())
        {
            parts.push(format!("UNTIL={}", until.replace('-', "")));
        }
        if let Some(count) = self.recurrence_count.filter(|count| *count > 0) {
            parts.push(format!("COUNT={count}"));
        }
        if !self.recurrence_weekdays.is_empty() {
            let mut days = self.recurrence_weekdays.clone();
            days.sort_unstable();
            parts.push(format!("BYDAY={}", days.join(",")));
        }
        parts.join(";")
    }
}

/// Every time `field` names that the entry `record` holds between the window's ends, without the
/// dates `covered` leaves to the records standing apart from it.
fn of_record(
    id: &str,
    record: &Map<String, Value>,
    field: &str,
    from_ms: i64,
    to_ms: i64,
    covered: &HashMap<String, BTreeSet<Date>>,
) -> Vec<Held> {
    let fields = Fields::of(record);
    let Some(start) = wall(&fields.start) else {
        return Vec::new();
    };
    // An entry whose zone this crate cannot read has unknown times, and nothing fires.
    let zone = match fields.timezone.as_str() {
        "" | "floating" => "UTC",
        zone => zone,
    };
    if tilo_recurrence::zone::wall_to_utc(zone, start).is_none() {
        return Vec::new();
    }
    if fields.cancelled.unwrap_or(false) {
        return Vec::new();
    }
    // An entry that stands apart from a series is one date of it, wherever it was moved to.
    let dates = if let Some((_, date)) = fields.standing_apart() {
        vec![date]
    } else {
        // A day either side of the window holds every time of the zone its ends fall in.
        let (Some(from), Some(to)) = (day_of(zone, from_ms), day_of(zone, to_ms)) else {
            return Vec::new();
        };
        let excluded: Vec<Date> = fields.exdates.iter().filter_map(|one| date(one)).collect();
        let Some(days) = tilo_recurrence::days(
            &fields.rule(),
            start.date(),
            &excluded,
            from.saturating_sub(jiff::Span::new().days(1)),
            to.saturating_add(jiff::Span::new().days(1)),
        ) else {
            return Vec::new();
        };
        days.into_iter()
            .filter(|date| {
                covered
                    .get(id)
                    .is_none_or(|covered| !covered.contains(date))
            })
            .collect()
    };
    let end = wall(&fields.end).unwrap_or(start);
    let key = if fields.key.is_empty() {
        id.to_owned()
    } else {
        fields.key.clone()
    };
    let name = |name: &str| {
        record
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    dates
        .into_iter()
        .filter_map(|date| {
            let mut facts = record.clone();
            facts.insert(
                START.to_owned(),
                Value::String(wall_text(date, start.time())),
            );
            facts.insert(END.to_owned(), Value::String(wall_text(date, end.time())));
            let at_ms = match field {
                END => {
                    let end_wall = date
                        .to_datetime(start.time())
                        .checked_add(end.duration_since(start))
                        .ok()?;
                    instant(zone, end_wall)?
                }
                _ => instant(zone, date.to_datetime(start.time()))?,
            };
            (from_ms..to_ms).contains(&at_ms).then(|| Held {
                key: format!("{key}/{date}"),
                at_ms,
                resource: name("calendar"),
                label: fields.title.clone(),
                facts,
            })
        })
        .collect()
}

/// The day `ms` is on in `zone`.
fn day_of(zone: &str, ms: i64) -> Option<Date> {
    let at = jiff::Timestamp::from_millisecond(ms).ok()?;
    let zone = tilo_recurrence::zone::named(zone).unwrap_or(jiff::tz::TimeZone::UTC);
    Some(at.to_zoned(zone).date())
}

/// The instant the wall time `at` is in `zone`, in milliseconds.
fn instant(zone: &str, at: DateTime) -> Option<i64> {
    Some(
        tilo_recurrence::zone::wall_to_utc(zone, at)?
            .0
            .as_millisecond(),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One record `id` holds, with `fields` as the app keeps them.
    fn entry(id: &str, fields: Value) -> (String, Map<String, Value>) {
        (
            id.to_owned(),
            fields.as_object().cloned().unwrap_or_default(),
        )
    }

    /// An end is the length the entry holds after, and a cancelled date is no time.
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
        let held: Vec<(String, i64)> = times(
            &[series, cancelled],
            END,
            instant("UTC", wall("2026-10-26T00:00:00").unwrap()).unwrap(),
            instant("UTC", wall("2026-10-29T00:00:00").unwrap()).unwrap(),
        )
        .into_iter()
        .map(|one| (one.key, one.at_ms))
        .collect();
        assert_eq!(
            held,
            [(
                "standup-1@example.org/2026-10-26".to_owned(),
                "2026-10-26T09:00:00Z"
                    .parse::<jiff::Timestamp>()
                    .unwrap()
                    .as_millisecond()
            )],
            "09:30 in winter time, and half an hour long"
        );
    }
}
