//! Reminders as a workspace fact: which of a workspace's reminders are due, whom each one is for,
//! and what the item it is about offers, read from the records and the schemas their apps
//! published.
//!
//! An app that reminds someone of an item says so in the item's type, by its fields' roles
//! (`tilolang::types::FieldRole`): the field holding the `std.Reminder` is marked `(reminds:
//! due, at, who: assignee)`, naming the day it is due on, the time of day where the type has
//! one, and the person field where it names one. That is enough for whoever holds the record to
//! find the reminder without the app's code, as the platform finds a mention in the text a
//! record holds.
//!
//! [`due`] reads a record's reminder — the instant it comes, in UTC, the moment the item is due,
//! and the person it is for. A record that names nobody is the creator's to be reminded of: the
//! creator is the record's first version's author, which the store holds. [`look`] walks what a
//! host holds — every package's resources, each item of each collection among them — and answers
//! the reminders whose instant has come for one person, and the next instant any of their
//! reminders comes, so a window wakes exactly then and never polls.
//!
//! The instant is the due moment moved by the trigger: the item's time of day, or the start of
//! its day where it has none, so `PT9H` on a day is 09:00 and `-PT15M` is a quarter of an hour
//! before the day begins. The caller knows the person's zone and resolves a day and a time of day
//! to an instant ([`InZone`], which [`due`] takes): the shared runtime every app maps carries no
//! time-zone database, and a reminder is read the same wherever the record is held.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde_json::Value as Json;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::value::ResourceRef;

use crate::meta::MetaExt as _;
use crate::record::{DynRecord, DynValue};
use crate::schema::{FieldRole, MergeKind};

/// The instant of a day, and of a time of day on it, in the zone of the person a reminder is
/// for: the start of `day` for `None`, that wall-clock time for `Some`. `None` for a reading the
/// zone has none of (a day or an hour a change of clocks skips).
pub type InZone<'a> = &'a dyn Fn(NaiveDate, Option<NaiveTime>) -> Option<DateTime<Utc>>;

/// When a record's reminder comes due: the instant it reminds, the moment the item itself is
/// due, and whom it reminds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    /// The instant the reminder comes, in UTC.
    pub at: DateTime<Utc>,
    /// The moment the item is due, in UTC: what the reminder gives the person time before.
    pub deadline: DateTime<Utc>,
    /// The person reminded: whom the record names; `None` for a record that names nobody, which
    /// is its creator's.
    pub person: Option<String>,
}

/// The reminder `record` holds comes due on, `None` when the record holds no reminder of a day.
///
/// `zone` reads the record's due day, and the time of day it names, in the zone of the person it
/// concerns; a record with no time of day counts from the start of its day.
#[must_use]
pub fn due(record: &DynRecord, zone: InZone<'_>) -> Option<Due> {
    let reminds = record.schema().reminds()?;
    let trigger = tilolang::repeat::Reminder::parse(text(&held(record, reminds.trigger)?)?)
        .ok()?
        .seconds();
    let day = NaiveDate::parse_from_str(text(&held(record, reminds.date)?)?, "%Y-%m-%d").ok()?;
    let deadline = match reminds
        .time
        .and_then(|id| held(record, id))
        .as_ref()
        .and_then(text)
    {
        // A time of day that carries its own zone is an instant already.
        Some(named) if named.ends_with('Z') => tilolang::calendar::instant(named)?,
        Some(named) => zone(day, Some(clock(named)?))?.timestamp(),
        None => zone(day, None)?.timestamp(),
    };
    let person = reminds
        .who
        .and_then(|id| held(record, id))
        .as_ref()
        .and_then(named);
    Some(Due {
        at: DateTime::from_timestamp(deadline.checked_add(trigger)?, 0)?,
        deadline: DateTime::from_timestamp(deadline, 0)?,
        person,
    })
}

/// What an item offers as a one-step action, as a notification beside it does: its id and the
/// words it is called by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub id: String,
    pub label: String,
}

/// One reminder whose instant has come, for one person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alarm {
    /// The package the item is a resource of.
    pub app: String,
    /// What the package is called: its app's name.
    pub app_title: String,
    /// The resource inside the package: the document, or the document and an item of it.
    pub path: String,
    /// The id of the item's type.
    pub ty: String,
    /// The person reminded.
    pub person: String,
    /// When it came due.
    pub at: DateTime<Utc>,
    /// When the item itself is due.
    pub deadline: DateTime<Utc>,
    /// What the item is called.
    pub title: String,
    /// What the item's type offers as one-step actions, in its order.
    pub actions: Vec<Offer>,
    /// The reminder as one string, stable per occurrence: the record and the instant.
    pub key: String,
}

impl Alarm {
    /// The item's resource as the action door names it (`tl:<app>:<path>`).
    #[must_use]
    pub fn resource(&self) -> String {
        crate::publish::resource_id(&self.app, &self.path)
    }

    /// The reminder's notification tag: short, stable, and the same wherever it is shown.
    #[must_use]
    pub fn tag(&self) -> String {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.key.hash(&mut hasher);
        format!("r{:014x}", hasher.finish() & 0x00ff_ffff_ffff_ffff)
    }
}

/// What a host holds that is due now, for one person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// The reminders whose instant has come, earliest first.
    pub due: Vec<Alarm>,
    /// The reminders whose instant has come and that `told` says the person was told of already:
    /// nothing left to say to them, kept so whoever shows a reminder knows not to take the one
    /// it showed back while the item still stands.
    pub heard: Vec<Alarm>,
    /// The earliest instant, later than the one asked at, that any of the person's reminders
    /// comes.
    pub next: Option<DateTime<Utc>>,
}

/// The reminders `host` holds for `person` that have come due by `now`, told or not, and when
/// their next one comes: every package's documents and the items of their collections, the
/// reminders of items their type marks done left out.
#[must_use]
pub fn look(
    host: &crate::host::Host,
    person: &str,
    now: DateTime<Utc>,
    zone: InZone<'_>,
    told: &crate::personal::PersonalState,
) -> Look {
    let mut walk = Walk {
        host,
        person,
        now,
        zone,
        told,
        app: String::new(),
        app_title: String::new(),
        found: Look::default(),
    };
    for (package, _ty, root) in host.records() {
        let Ok(record) = host.store().record(&root) else {
            continue;
        };
        walk.app.clone_from(&package.meta.app);
        walk.app_title.clone_from(&package.meta.title);
        walk.visit(&root, &record);
    }
    walk.found.due.sort_by_key(|alarm| alarm.at);
    walk.found.heard.sort_by_key(|alarm| alarm.at);
    walk.found
}

/// One walk over what a host holds, for one person at one instant: the packages and the items
/// their collections hold, and what is due for them.
struct Walk<'a> {
    host: &'a crate::host::Host,
    person: &'a str,
    now: DateTime<Utc>,
    zone: InZone<'a>,
    told: &'a crate::personal::PersonalState,
    /// The package the record at hand is a resource of, and what it is called.
    app: String,
    app_title: String,
    found: Look,
}

impl Walk<'_> {
    /// What the item `record`, the resource `path`, holds for the person: its reminder, and the
    /// reminders of the items its collections hold.
    fn visit(&mut self, path: &str, record: &DynRecord) {
        if !is_done(record)
            && let Some(reminder) = due(record, self.zone)
        {
            let who = reminder
                .person
                .clone()
                .or_else(|| creator(self.host, path, &record.schema().id))
                .filter(|who| who.eq_ignore_ascii_case(self.person));
            if who.is_some() {
                if reminder.at > self.now {
                    self.found.next = Some(
                        self.found
                            .next
                            .map_or(reminder.at, |next| next.min(reminder.at)),
                    );
                } else {
                    self.ring(path, record, &reminder, who.as_deref().unwrap_or_default());
                }
            }
        }
        for field in record
            .schema()
            .fields
            .iter()
            .filter(|field| field.kind == MergeKind::Collection)
        {
            let Some(DynValue::Collection(items)) = record.get(&field.id) else {
                continue;
            };
            for (id, item) in items.iter() {
                let child = crate::index::derive::item_path(path, id.as_str());
                self.visit(&child, item);
            }
        }
    }

    /// Keep the reminder of the item `record` at `path` as one the person `who` is due, and as
    /// one they were told of already when `told` says so: on this device or, through `told`,
    /// another of theirs.
    fn ring(&mut self, path: &str, record: &DynRecord, reminder: &Due, who: &str) {
        let key = format!("{path}@{}", reminder.at.timestamp());
        let heard = self
            .told
            .told
            .iter()
            .any(|held| held.person.eq_ignore_ascii_case(self.person) && held.reminder == key);
        let alarm = Alarm {
            app: self.app.clone(),
            app_title: self.app_title.clone(),
            path: path.to_owned(),
            ty: record.schema().id.clone(),
            person: who.to_owned(),
            at: reminder.at,
            deadline: reminder.deadline,
            title: title(record),
            actions: offers(self.host, &record.schema().id),
            key,
        };
        if heard {
            self.found.heard.push(alarm);
        } else {
            self.found.due.push(alarm);
        }
    }
}

/// Whether `record` is marked done by the field its type roles as such: a done item reminds
/// nobody.
#[must_use]
pub fn is_done(record: &DynRecord) -> bool {
    record
        .schema()
        .done()
        .and_then(|id| record.get(id))
        .is_some_and(|value| matches!(value, DynValue::Register(Json::Bool(true))))
}

/// What `record` is called: the text its type roles as its title, else what the type is called.
#[must_use]
pub fn title(record: &DynRecord) -> String {
    record
        .schema()
        .fields
        .iter()
        .find(|field| field.role == FieldRole::Title)
        .and_then(|field| record.get(&field.id))
        .and_then(plain)
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| record.schema().spoken_name())
}

/// Who made the resource `path` of the type `ty`: the author of its first version.
fn creator(host: &crate::host::Host, path: &str, ty: &str) -> Option<String> {
    let (doc, item) = match path.split_once('/') {
        Some((doc, item)) => (doc.to_owned(), Some(item.to_owned())),
        None => (path.to_owned(), None),
    };
    let at = ResourceRef {
        doc,
        item,
        ty: ty.to_owned(),
    };
    host.store()
        .version_end(&at, false)
        .ok()
        .flatten()
        .map(|version| version.who)
}

/// What a type whose id is `type_id` offers as one-step actions: the actions on its records that
/// ask nothing, in the order their package declares them.
#[must_use]
pub fn offers(host: &crate::host::Host, type_id: &str) -> Vec<Offer> {
    let mut offered = Vec::new();
    for package in host.packages() {
        for action in &package.meta.actions {
            let subject = package.meta.subject_of(&action.params);
            if subject.is_none_or(|subject| subject.id != type_id) {
                continue;
            }
            if action.scenario
                || !crate::surface::arguments(&package.meta, &action.params)
                    .iter()
                    .all(|param| param.default.is_some())
            {
                continue;
            }
            offered.push(Offer {
                id: action.id.clone(),
                label: if action.label.trim().is_empty() {
                    action.name.clone()
                } else {
                    action.label.clone()
                },
            });
        }
    }
    offered
}

/// The value `record` holds under the field id `id`, as JSON: a text as a string, a register as
/// the JSON it is stored as (an option, a person, a day).
fn held(record: &DynRecord, id: &str) -> Option<Json> {
    match record.get(id)? {
        DynValue::Text(text) => Some(Json::String(text.clone())),
        DynValue::Register(value) => Some(value.clone()),
        _ => None,
    }
}

/// The time of day a `Time` value names (`2026-10-27T09:30[:00]`, its day read from the record's
/// own day field): `None` for text that names none, which the record then holds no time of day
/// in.
fn clock(named: &str) -> Option<NaiveTime> {
    let time = named.split_once('T')?.1;
    if time.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    let part = |range: std::ops::Range<usize>| -> Option<u32> {
        // ignored-failure: the value's type check wrote this text; digits that do not parse are no time
        time.get(range)?.parse().ok()
    };
    let (hour, minute) = (part(0..2)?, part(3..5)?);
    let second = match time.get(6..8) {
        Some(seconds) => seconds.parse().ok()?,
        None => 0,
    };
    NaiveTime::from_hms_opt(hour, minute, second)
}

/// The text a value holds: a text, or the text an option holds (`{"some": "…"}`).
fn text(value: &Json) -> Option<&str> {
    match value {
        Json::String(text) => Some(text),
        Json::Object(held) => held.get("some").and_then(Json::as_str),
        _ => None,
    }
}

/// The text a field holds, as a record names itself by it.
fn plain(value: &DynValue) -> Option<String> {
    match value {
        DynValue::Text(text) => Some(text.clone()),
        DynValue::Register(value) => text(value).map(str::to_owned),
        _ => None,
    }
}

/// The person a value names, in either form a principal is stored in: `{"principal": "…"}`, or
/// `{"some": {"principal": "…"}}` for an optional one.
fn named(value: &Json) -> Option<String> {
    let Json::Object(held) = value else {
        return None;
    };
    let person = match held.get("some") {
        Some(Json::Object(some)) => some,
        _ => held,
    };
    person
        .get("principal")
        .and_then(Json::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "reminders_tests.rs"]
mod tests;
