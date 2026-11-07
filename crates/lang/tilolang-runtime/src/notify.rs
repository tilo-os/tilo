//! Telling a person something: `notify(to: who, title:, body:, opens: r)`.
//!
//! What the call writes is a record of the host's `Notice` type, addressed to the person named,
//! in the same change as the action that told them ([`crate::store::Store::create_in`]). It is
//! the workspace's record of the notice like any other resource: every device of that person's
//! reads it, so every one of them shows it through its notifications and none of anyone else's
//! does, and what one device does with it — the person dismissing or opening it — is a change
//! of the record too, so it clears there and everywhere ([`NOTICE_SEEN`]).
//!
//! The run's journal records that it was told ([`crate::run::JournalEntry::Notified`]): a run
//! resumed after waiting for a person, or replayed, writes no second notice.

use crate::ids::Principal;
use crate::value::ResourceRef;

/// Something a person is told, as the runtime writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// Who is told: the person the notification reaches, on their devices.
    pub to: Principal,
    pub title: String,
    pub body: String,
    /// The resource choosing the notification opens: where the person lands.
    pub opens: ResourceRef,
}
