//! The rows Calendar publishes besides its document's events, tasks and
//! projects: the calendars of this device's accounts, the guests of one event
//! and the people of the address book to invite.
//!
//! A calendar's, a guest's and a contact's effects are [`CalendarEffect`]'s,
//! offered on each row.

use crate::resource::CalendarEffect;

use ui::tilo_resource;

/// A calendar of this device's accounts.
#[derive(tilo_resource::Row)]
#[row(
    kind = "calendar.collection",
    label = "Calendar",
    effects = CalendarEffect
)]
pub struct CalendarRow;

/// One guest of an event.
#[derive(tilo_resource::Row)]
#[row(
    kind = "calendar.attendee",
    label = "Attendee",
    effects = CalendarEffect
)]
pub struct AttendeeRow;

/// The people of the address book the picker offers to invite.
#[derive(tilo_resource::Row)]
#[row(kind = "calendar.contacts", label = "Contacts")]
pub struct ContactAttendees;

/// One person the picker offers, added to the event by the calendar's own
/// `invite` verb (a row edit, not an effect).
#[derive(tilo_resource::Row)]
#[row(kind = "calendar.contact-attendee", label = "Contact")]
pub struct ContactAttendee;

/// The calendar's id a row of `kind` was published under: the row's id, or
/// the id itself as the calendar's own controls hand it over.
pub(crate) fn calendar_of_row(id: &str) -> &str {
    id.strip_prefix("calendar.collection:").unwrap_or(id)
}

/// The event and guest ids an attendee row's id names: its event's row id,
/// then the guest.
pub(crate) fn attendee_of_row(row: &str) -> Option<(String, String)> {
    let (event, attendee) = row.split_once("/attendees/")?;
    let (collection, event) = event.rsplit_once('/')?;
    collection.ends_with("/events").then_some(())?;
    Some((event.to_owned(), attendee.to_owned()))
}
