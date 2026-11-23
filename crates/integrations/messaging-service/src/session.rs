//! The compositor's side of the messaging session: what the service asks of it,
//! over the service's standard input and output.
//!
//! The compositor alone holds the workspace's records (its accounts and
//! contacts) and its vault, and shows the protected network prompt, so the
//! service asks it for those. Each account's profile key, which seals what the
//! account keeps on this device, is an entry of the person's workspace vault:
//! the service has the compositor keep a new account's key there, and reads it
//! back by the entry the published account names when it starts the account
//! again. Whoever may read that vault may read the key; nothing asks first. The compositor tells the service when the person's session stops
//! being usable (locked or ended), which retires every admission the service
//! made under it. The service stops when that connection closes.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// An account the service publishes into the person's accounts record once
/// the person approved it: its id, the name the person gave it and the
/// link generation, with what each service records beside them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Publication {
    pub account: String,
    pub name: String,
    pub generation: u64,
    /// The vault entry that keeps the account's profile key, which the
    /// account names as its sign-in ([`accounts::email::PASSWORD_ITEM_KEY`]).
    pub key: String,
    pub kind: PublicationKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PublicationKind {
    /// The Signal account identity (ACI).
    Signal {
        aci: String,
    },
    /// The verified `WhatsApp` JID.
    Whatsapp {
        jid: String,
    },
    Sms {
        scope: String,
    },
}

/// A call either side makes.
#[derive(Serialize, Deserialize)]
pub enum Call {
    /// Service: the workspace's accounts and contacts as they are now.
    Person,
    /// Service: publish an approved account.
    Publish(Publication),
    /// Service: ask the person, once, to let Messages reach `hosts`.
    Consent { key: String, hosts: Vec<String> },
    /// Service: the consent request `key` is no longer wanted.
    ConsentWithdrawn { key: String },
    /// Service: keep `key`, a new account's profile key in hex, as a new entry
    /// of the person's workspace vault titled `title` and kept for `origin`.
    /// Answered with [`Reply::Kept`].
    KeepKey {
        title: String,
        origin: String,
        key: Zeroizing<String>,
    },
    /// Service: the profile key the vault entry `item` keeps, which a
    /// published account names. Answered with [`Reply::Key`].
    Key { item: String },
}

#[derive(Serialize, Deserialize)]
pub enum Reply {
    Person {
        /// The workspace's accounts record, in its own JSON form.
        accounts: serde_json::Value,
        /// The contacts record, in its own JSON form.
        directory: serde_json::Value,
    },
    /// The call was carried out, or allowed.
    Done,
    /// It was refused: revoked, denied or impossible now.
    Refused,
    /// It could not be answered now.
    Unavailable,
    /// The vault entry a key is kept in now.
    Kept { item: String },
    /// The profile key an entry keeps, in hex.
    Key { key: Zeroizing<String> },
}

#[derive(Serialize, Deserialize)]
pub enum Event {
    /// Compositor: the person's session is usable, or stopped being so. A
    /// new generation retires everything admitted under an earlier one.
    Session { active: bool, generation: u64 },
}

pub type SessionLink = crate::link::Link<Call, Reply, Event>;

/// What the compositor starts the service with.
pub const ACTOR_ARG: &str = "--actor";
pub const DEVICE_ARG: &str = "--device";
pub const PROFILE_ARG: &str = "--profile";
