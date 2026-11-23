//! The `CalDAV` connector as the calendar origin speaks to it: what an account's server holds, in
//! the account's terms: the calendars of an account, the entries of each since what is held, a
//! write or a removal of one entry, an answer to an invitation, and an invitation put into a
//! calendar. It names accounts by the id the workspace's accounts give them and entries by the
//! place their server keeps them; servers, collection paths and credentials stay behind it, in
//! native code ([`crate::engine`]), reached in-process or over a socket ([`crate::remote`]).

/// A calendar account of the workspace's accounts, as the connector shows it: never its server's
/// sign-in.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountRef {
    /// The id the workspace's accounts give it.
    pub id: String,
    /// The name it signs in with.
    pub address: String,
    /// The server address the person typed.
    pub server: String,
    /// What it is called.
    pub name: String,
    /// Whether nothing is ever written to its server.
    pub read_only: bool,
}

/// One calendar of an account, as its server holds it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarRef {
    pub account: String,
    /// The collection's address on its server: the key the calendar is known by.
    pub key: String,
    pub name: String,
    /// What the calendar is coloured, as its server says it, empty for none.
    pub color: String,
    /// Whether its server lets its person write it, and the account is not read-only.
    pub writable: bool,
    /// Where its last pull ended, as its server names it; empty for one not pulled yet.
    pub token: String,
}

/// One person an entry names, and whether they come.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Guest {
    pub name: String,
    pub email: String,
    /// `accepted`, `declined`, `tentative` or `needs-action`.
    pub status: String,
    /// `required`, `optional` or `chair`.
    pub role: String,
}

/// One occurrence of a repeat that stands apart: cancelled, moved or renamed.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Occurrence {
    /// The date the occurrence was on, `YYYY-MM-DD`.
    pub date: String,
    /// The time of day it was at, `HH:MM:SS`, empty for one all day.
    pub time: String,
    pub cancelled: bool,
    /// When it starts now, an instant, empty to keep the series'.
    pub start: String,
    /// When it ends now, an instant, empty to keep the series'.
    pub end: String,
    /// What it is called now, empty to keep the series'.
    pub title: String,
    /// Where it is now, empty to keep the series'.
    pub location: String,
}

/// An entry as its server holds it: the fields a calendar shows, in the terms the record holds
/// them, so what a server says and what a person edits are one shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// The `UID` the entry goes by, which its server and every client name it by.
    pub uid: String,
    /// Where its server keeps it, absolute: empty for one not on a server yet.
    pub href: String,
    /// The version its server last gave it.
    pub etag: String,
    /// The calendar's key.
    pub calendar: String,
    pub title: String,
    /// When it starts, as iCalendar writes the wall time (`2026-11-19T19:00:00`).
    pub start: String,
    /// When it ends, as iCalendar writes the wall time.
    pub end: String,
    pub all_day: bool,
    /// The zone its wall time is written in: `floating`, `UTC`, or an IANA name.
    pub timezone: String,
    pub location: String,
    /// How it repeats, as its `RRULE`'s frequency and days (`FREQ=WEEKLY;BYDAY=MO`); empty for
    /// one occurrence.
    pub recurrence: String,
    pub recurrence_every: u32,
    /// The last day it repeats on, `YYYY-MM-DD`, empty for no end.
    pub recurrence_until: String,
    /// How many times it repeats, 0 for no count.
    pub recurrence_count: u32,
    /// Dates it is left out on, `YYYY-MM-DD` each.
    pub exdates: Vec<String>,
    /// The occurrences that stand apart: moved, renamed or cancelled.
    pub overrides: Vec<Occurrence>,
    /// The mail address of whoever organises it, empty for nobody.
    pub organizer: String,
    pub attendees: Vec<Guest>,
    pub categories: Vec<String>,
    /// The colour it is shown in, as its `COLOR`; empty for none.
    pub color: String,
    /// What it says: its `DESCRIPTION`.
    pub notes: String,
    /// When its reminder rings, an instant, empty for none: its `VALARM`.
    pub remind: String,
}

/// One entry a server no longer holds.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Removal {
    pub calendar: String,
    pub href: String,
}

/// What one sync of an account found.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sync {
    pub account: String,
    /// Every calendar the account's server holds now.
    pub calendars: Vec<CalendarRef>,
    /// The entries new to what is held, or changed since.
    pub entries: Vec<Entry>,
    /// The entries its server no longer holds.
    pub removed: Vec<Removal>,
}

/// Where a calendar's last pull ended and the version each of its entries was, as the workspace
/// kept them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CalendarKnown {
    pub calendar: String,
    pub token: String,
    pub entries: Vec<(String, String)>,
}

/// A request handed to a server.
pub type Request = uuid::Uuid;

/// What a server answered.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    Synced(Sync),
    /// The server took the request: where it keeps the entry now, empty for a removal.
    Accepted {
        request: Request,
        href: String,
    },
    /// The server refused it: why.
    Refused {
        request: Request,
        reason: String,
    },
    /// The server could not be reached, so it never got the request: why.
    Unsent {
        request: Request,
        reason: String,
    },
    /// The account's server could not be reached: why.
    Unreachable {
        account: String,
        reason: String,
    },
}

/// Why a request was not made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unmade {
    /// The workspace has no such account on this device.
    NoAccount,
    /// The account's server could not be asked, so nothing was written to it.
    EngineStopped,
}

/// The connector to the calendar servers of the workspace's accounts.
pub trait CalDavConnector {
    /// The workspace's calendar accounts.
    fn accounts(&self) -> Vec<AccountRef>;
    /// Ask for the calendars of `account` and what changed in them since what is held.
    fn sync(&mut self, account: &str, known: Vec<CalendarKnown>) -> Option<Request>;
    /// Write `entry` to the server of `account`: over the entry at `href` when it is not empty,
    /// as a new entry where it is. `from` names the entry to take out first, where the entry
    /// moves from one calendar to another.
    fn put(
        &mut self,
        account: &str,
        entry: Entry,
        href: &str,
        from: &str,
    ) -> Result<Request, Unmade>;
    /// Take the entry at `href` out of `calendar`.
    fn remove(&mut self, account: &str, calendar: &str, href: &str) -> Result<Request, Unmade>;
    /// Write a calendar's own properties: its `name` and its `color`, as its server keeps them.
    fn set_calendar(
        &mut self,
        account: &str,
        calendar: &str,
        name: &str,
        color: &str,
    ) -> Result<Request, Unmade>;
    /// Answer an invitation as `attendee`, in the entry at `href`: `accepted`, `declined` or
    /// `tentative`.
    fn answer(
        &mut self,
        account: &str,
        calendar: &str,
        href: &str,
        attendee: &str,
        answer: &str,
    ) -> Result<Request, Unmade>;
    /// Put the iCalendar `text` into `calendar` as an entry of its own, once.
    fn import(&mut self, account: &str, calendar: &str, text: &str) -> Result<Request, Unmade>;
    /// What the servers answered since last asked; never waits.
    fn events(&mut self) -> Vec<Event>;
}
