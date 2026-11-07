//! Telling a person something: `notify(to: who, title:, body:, opens: r)`.
//!
//! The call reaches the host's own notification path, the one its shell already shows the
//! person's news in ([`crate::Runtime::set_notify`]), and the run's journal records that it was
//! told ([`crate::run::JournalEntry::Notified`]): a run resumed after waiting for a person, or
//! replayed, tells them no more than it did the first time.

use crate::ids::Principal;
use crate::value::ResourceRef;

/// Something a person is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Who is told: the person the notification reaches, on their devices.
    pub to: Principal,
    pub title: String,
    pub body: String,
    /// The resource choosing the notification shows: where the person lands.
    pub opens: Option<ResourceRef>,
}

/// Where `notify` reaches: the host's notification path. A host that shows none is told, and
/// shows nothing.
pub trait NotifyHost {
    fn notify(&self, notice: &Notice);
}
