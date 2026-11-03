//! Reminders as a workspace fact: when a record's reminder comes due, and whom it reminds, read
//! from the record itself and the schema its app published.
//!
//! An app that reminds someone of an item says so in the item's type, by its fields' roles
//! (`tilolang::types::FieldRole`): the field holding the `std.Reminder` is marked `(reminds:
//! due, at, who: assignee)`, naming the day it is due on, the time of day where the type has
//! one, and the person field where it names one. That is enough for whoever holds the record to
//! find the reminder without the app's code — the person's desktop, on any device, whether or
//! not the app is open — as [`mentions`](crate::mentions) finds mentions in the text a record
//! holds.
//!
//! [`Reminder::of`] reads the role off a type's metadata; [`due`] reads the record's fields
//! under the ids it names and answers the one instant the reminder comes, in UTC, and the person
//! it is for. A record that names nobody is the creator's to be reminded of: the caller knows who
//! created the record, this module never guesses.
//!
//! The instant is the due moment moved by the trigger: the item's time of day, or the start of
//! its day in the person's zone where it has none, so `PT9H` on a day is 09:00 and `-PT15M` is a
//! quarter of an hour before the day begins. The caller knows the zone and resolves a day's
//! start for it ([`due`] takes it as a function): the shared runtime every app maps carries no
//! time-zone database, and a reminder is read the same wherever the record is held.

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value as Json;
use tilolang::meta::TypeMeta;
use tilolang::types::FieldRole;

/// Where a type's reminder is: the fields its record stores the trigger, the day, the time of
/// day and the person under, by the id the record keys them by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reminder<'a> {
    /// The `std.Reminder` field: its text is the RFC 5545 trigger, `-PT15M`.
    pub trigger: &'a str,
    /// The day it is due on: a reminder of a record with no day is one that never comes.
    pub date: &'a str,
    /// The time of day it is due at, where the type has one.
    pub time: Option<&'a str>,
    /// The person field, where the type names one: a record naming nobody is its creator's.
    pub who: Option<&'a str>,
}

impl<'a> Reminder<'a> {
    /// The reminder `ty` declares by its fields' roles (`reminds: std.Reminder (reminds: due,
    /// at, who: assignee)`), or `None` for a type that declares none.
    #[must_use]
    pub fn of(ty: &'a TypeMeta) -> Option<Self> {
        ty.fields.iter().find_map(|field| {
            let FieldRole::Reminds { date, time, who } = field.role.as_ref()? else {
                return None;
            };
            Some(Self {
                trigger: field.id.as_str(),
                date: id_of(ty, date)?,
                time: time.as_deref().and_then(|name| id_of(ty, name)),
                who: who.as_deref().and_then(|name| id_of(ty, name)),
            })
        })
    }
}

/// When a record's reminder comes due, and whom it reminds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    /// The instant it comes, in UTC.
    pub at: DateTime<Utc>,
    /// The person reminded: whom the record names, or its creator where it names nobody.
    pub person: String,
}

/// The reminder the record whose fields are at `fields` in `doc` comes due on, reminding
/// `creator` where the record names nobody; `None` when the record holds no reminder.
///
/// `day_start` is the instant the record's due day begins for the person it concerns, in their
/// zone (`tilo_recurrence::wall_instant`); it is read only for a record with no time of day,
/// whose reminder counts from the start of its day.
#[must_use]
pub fn due(
    doc: &AutoCommit,
    fields: &ObjId,
    ty: &TypeMeta,
    day_start: impl Fn(NaiveDate) -> Option<DateTime<Utc>>,
    creator: &str,
) -> Option<Due> {
    let reminder = Reminder::of(ty)?;
    let trigger = value(doc, fields, reminder.trigger)?;
    let trigger = tilolang::repeat::Reminder::parse(text(&trigger)?)
        .ok()?
        .seconds();
    let day = value(doc, fields, reminder.date)?;
    let day = NaiveDate::parse_from_str(text(&day)?, "%Y-%m-%d").ok()?;
    let due_at = reminder
        .time
        .and_then(|id| value(doc, fields, id))
        .as_ref()
        .and_then(text)
        .and_then(tilolang::calendar::instant)
        .or_else(|| day_start(day).map(|at| at.timestamp()))?;
    let person = reminder
        .who
        .and_then(|id| value(doc, fields, id))
        .as_ref()
        .and_then(named)
        .map_or_else(|| creator.to_owned(), ToOwned::to_owned);
    Some(Due {
        at: DateTime::from_timestamp(due_at.checked_add(trigger)?, 0)?,
        person,
    })
}

/// The id `ty` stores the field named `name` under.
fn id_of<'a>(ty: &'a TypeMeta, name: &str) -> Option<&'a str> {
    ty.fields
        .iter()
        .find(|field| field.name == name)
        .map(|field| field.id.as_str())
}

/// The value the record at `fields` holds under the field id `id`, as JSON: a text as a string,
/// a structured value (an option, a person) as the JSON it is stored as.
fn value(doc: &AutoCommit, fields: &ObjId, id: &str) -> Option<Json> {
    match doc.get(fields, id).ok()? {
        Some((Value::Scalar(scalar), _)) => match scalar.as_ref() {
            ScalarValue::Str(text) => Some(Json::String(text.to_string())),
            ScalarValue::Bytes(bytes) => serde_json::from_slice(bytes).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// The text a value holds: a text, or the text an option holds (`{"some": "…"}`).
fn text(value: &Json) -> Option<&str> {
    match value {
        Json::String(text) => Some(text),
        Json::Object(held) => held.get("some").and_then(Json::as_str),
        _ => None,
    }
}

/// The person a value names, in either form a principal is stored in: `{"principal": "…"}`, or
/// `{"some": {"principal": "…"}}` for an optional one.
fn named(value: &Json) -> Option<&str> {
    let Json::Object(held) = value else {
        return None;
    };
    let person = match held.get("some") {
        Some(Json::Object(some)) => some,
        _ => held,
    };
    person.get("principal").and_then(Json::as_str)
}

#[cfg(test)]
#[path = "reminders_tests.rs"]
mod tests;
