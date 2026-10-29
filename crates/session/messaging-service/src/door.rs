//! The session door: what the service asks of the compositor that started
//! it, over the service's standard input and output.
//!
//! The compositor alone holds the person's records (their accounts and
//! contacts), shows the protected network prompt and owns the device's
//! endpoint that carries private Tilo messages, so the service asks it for
//! those. The compositor tells the service when the person's session stops
//! being usable (locked or ended), which retires every admission the service
//! made under it. The service stops when the door closes.

use serde::{Deserialize, Serialize};

/// An account the service publishes into the person's accounts record once
/// the person approved it: its id, the name the person gave it and the
/// link generation, with what each service records beside them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Publication {
    pub account: String,
    pub name: String,
    pub generation: u64,
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
    Tilo {
        scope: String,
    },
}

/// A device's certificate and the revocations it presents, as private
/// Tilo messages carry them.
#[derive(Clone, Serialize, Deserialize)]
pub struct Material {
    pub cert: tilo_user_identity::DeviceCert,
    pub revocations: Vec<tilo_user_identity::RevocationEntry>,
}

/// One private Tilo text.
#[derive(Clone, Serialize, Deserialize)]
pub struct Text {
    pub id: String,
    pub timestamp_ms: u64,
    pub body: zeroize::Zeroizing<String>,
}

/// A call either side makes.
#[derive(Serialize, Deserialize)]
pub enum Call {
    /// Service: the person's accounts and contacts as they are now.
    Person,
    /// Service: publish an approved account.
    Publish(Publication),
    /// Service: ask the person, once, to let Messages reach `hosts`.
    Consent { key: String, hosts: Vec<String> },
    /// Service: the consent request `key` is no longer wanted.
    ConsentWithdrawn { key: String },
    /// Service: the Tilo account now receives private texts on this device.
    PrivateBind,
    /// Service: send a private text to `peer`, a device of `root`.
    PrivateSend {
        peer: [u8; 32],
        root: [u8; 32],
        text: Text,
    },
    /// Compositor: the bound account's certificate.
    PrivateCertificate,
    /// Compositor: may this peer reach the account?
    PrivateCheckPeer(Material),
    /// Compositor: a private text arrived from this peer.
    PrivateReceive(Material, Text),
}

#[derive(Serialize, Deserialize)]
pub enum Reply {
    Person {
        accounts: accounts::AccountStore,
        /// The contacts record, in its own JSON form.
        directory: serde_json::Value,
    },
    Material(Material),
    /// The call was carried out, or allowed.
    Done,
    /// It was refused: revoked, denied or impossible now.
    Refused,
    /// It could not be answered now.
    Unavailable,
}

#[derive(Serialize, Deserialize)]
pub enum Event {
    /// Compositor: the person's session is usable, or stopped being so. A
    /// new generation retires everything admitted under an earlier one.
    Session { active: bool, generation: u64 },
}

pub type DoorLink = crate::link::Link<Call, Reply, Event>;

/// What the compositor starts the service with.
pub const PRINCIPAL_ARG: &str = "--principal";
pub const DEVICE_ARG: &str = "--device";
pub const PROFILE_ARG: &str = "--profile";
