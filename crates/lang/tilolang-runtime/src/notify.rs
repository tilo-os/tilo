//! Telling a person something: `notify(to: who, title:, body:, opens: r)`.
//!
//! What the call writes is a record of the host's `Notice` type, addressed to the person named,
//! in the same change as the action that told them ([`crate::store::Store::create_in`]). It is
//! the workspace's record of the notice like any other resource: every device of that person's
//! reads it, so every one of them shows it through its notifications and none of anyone else's
//! does, and what one device does with it — the person dismissing or opening it — is a change
//! of the record too, so it clears there and everywhere ([`NOTICE_SEEN`]).
//!
//! The notice is an outside effect of the run, and its record is the effect's key: the run's
//! identity and how many effects it queued before it ([`crate::handles::RunState::next_effect_key`]).
//! A replica that runs the same invocation — one that holds no journal of the run, before or
//! after a person was asked — names the same record, so the workspace holds one notice and the
//! person is told once.
//!
//! The run's journal records that it was told ([`crate::run::JournalEntry::Notified`]): a run
//! resumed after waiting for a person, or replayed, writes no second notice.

use crate::ids::Principal;
use crate::value::ResourceRef;

/// The record of a notice written under the effect key `key`: one name for it, whatever the
/// run's name and the id of a record hold. A run's name holds `/` where it is a step of another
/// run, and a record's id holds the `/` an item of a collection is, both of which a resource id
/// reads as the item it names; folding the key into one name keeps the notice one resource, the
/// same on every replica that writes it.
#[must_use]
pub fn record_id(key: &str) -> String {
    format!("notice:{}", blake3::hash(key.as_bytes()).to_hex())
}

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
