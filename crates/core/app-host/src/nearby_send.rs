//! Nearby Send as the shell shows it and as its actions are named: the devices this one
//! can hand a file to, the offers another device made back, and what became of each
//! transfer.
//!
//! The offers and transfers themselves are the device daemon's
//! ([`tilo_runtime::device::nearby`]): the session reads the daemon's resource over the
//! shell link and invokes its actions there, so this module holds the reading's shape and
//! the actions' names, which the daemon and the session's views both use.

use serde::{Deserialize, Serialize};
use tilo_actions::ResourceSpec;
use tilo_resource::Effect;

#[cfg(target_os = "linux")]
mod airdrop;
#[cfg(target_os = "linux")]
pub use airdrop::{AirDrops, Attempt, HELPER_TIMEOUT, Host, lab};

/// The question a file handed to a receiver that is not one of the person's devices waits on: the
/// person types the fingerprint of the certificate that receiver showed.
pub const VERIFY_QUESTION: &str = "nearby-send.verify-receiver";

/// What [`VERIFY_QUESTION`] puts to the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyQuestion {
    /// The file's name.
    pub name: String,
    /// Its size, in bytes.
    pub bytes: u64,
    /// The receiver, by its endpoint.
    pub endpoint: String,
    /// The SHA-256 digest of the certificate the receiver showed, 64 hexadecimal digits: what the
    /// person's typing is held to.
    pub certificate: String,
}

/// The resource the shell reads it as.
pub const RESOURCE: &str = "session:nearby-send";

/// The device-facing name the shell's views know it by (`device.nearby_send`),
/// which `device-surface.json` declares.
pub const SURFACE: &str = "nearby-send";

/// What a nearby device is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Another of the person's own devices, reached over the device fabric.
    #[default]
    Device,
    /// A receiver at an address the host names, as a lab's stands: the
    /// AirDrop-style hand-off that needs no account on either end.
    LabReceiver,
}

/// One device a file can be handed to.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct NearbyDevice {
    /// The name the device goes by.
    pub name: String,
    pub kind: DeviceKind,
    /// Whether it is here now.
    pub reachable: bool,
    /// Its address, for a lab receiver; empty for the person's own devices,
    /// which the fabric reaches without one.
    #[serde(default)]
    pub address: String,
    /// Its port, for a lab receiver; 0 otherwise.
    #[serde(default)]
    pub port: u16,
    /// The handle `send` names it by.
    pub handle: String,
}

/// A folder a received file can land in.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct NearbyDestination {
    /// The folder's name as the person reads it.
    pub name: String,
    /// The handle `accept` names it by.
    pub handle: String,
    /// Whether the last accept here remembered it.
    #[serde(default)]
    pub remembered: bool,
}

/// Where a transfer stands, in the words the shell shows.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TransferState {
    /// Waiting for the person at either end: an offer not yet decided.
    #[default]
    Offered,
    /// Its bytes are moving.
    Sending,
    /// Every byte landed.
    Done,
    /// The person at either end turned it down or stopped it.
    Declined,
    /// It could not finish.
    Failed,
}

/// One transfer, live or in the history the engine keeps.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct NearbyTransferRow {
    pub state: TransferState,
    /// How much of it landed, in percent.
    pub progress: u8,
    /// The other device's name.
    pub peer: String,
    /// Where a received file landed, when this device accepted it; empty
    /// otherwise.
    #[serde(default)]
    pub destination: String,
    /// When this device first saw it, in nanoseconds since the Unix epoch.
    #[serde(default)]
    pub time: u64,
    /// Whether it comes to this device.
    pub incoming: bool,
    /// The file's name.
    pub name: String,
    /// The handle `accept`, `decline`, `cancel` and `retry` name it by.
    pub handle: String,
}

/// What the resource reads: what is in reach, where a received file may land,
/// and what was transferred.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Reading {
    #[serde(default)]
    pub devices: Vec<NearbyDevice>,
    #[serde(default)]
    pub destinations: Vec<NearbyDestination>,
    #[serde(default)]
    pub transfers: Vec<NearbyTransferRow>,
}

/// The resource's declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "session",
    label = "Nearby Send",
    effects = NearbySendAction
)]
pub struct NearbySendRoot;

/// What the resource offers: hand a file to a device, answer an offer, stop or
/// offer again a transfer, and share a resource with people.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.device.nearby-send")]
pub enum NearbySendAction {
    /// Send files to a nearby device
    ///
    /// Offers each file to the device; the person there chooses where it
    /// lands, so nothing arrives unasked.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Send {
        /// The files, by their path in the workspace's files.
        #[param("The files to send")]
        items: Vec<String>,
        /// The device, as the reading lists it.
        #[param("The device")]
        device: String,
    },
    /// Keep a file a nearby device offered
    ///
    /// The receiving person's choice of where it lands; without one, the
    /// folder the last accept remembered.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Accept {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
        /// The folder it lands in, as the reading lists them.
        #[param("Where it lands")]
        destination: Option<String>,
    },
    /// Turn down a file a nearby device offered
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Decline {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
    },
    /// Stop a transfer, sent or received
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Cancel {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
    },
    /// Offer again a transfer that failed
    #[effect(kind = "outside")]
    #[effect(own_device)]
    RetryTransfer {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
    },
    /// Share a resource with people, opening the invitation sheet
    #[effect(kind = "outside")]
    InvitePeople {
        /// The resource, by the id the catalogue lists it under.
        #[param("The resource")]
        resource: String,
    },
}

/// The resource as the shell's views read and press it (`device.nearby_send`).
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, NearbySendAction>(SURFACE, "Nearby Send")
}

/// The resource as the session's services publish it.
#[must_use]
pub fn resource(label: impl Into<String>) -> ResourceSpec {
    // The verbs go with the resource: a press, a script and an agent reach the same ones, and
    // what moved is personal to the person the engine answers for.
    tilo_resource::offer::<NearbySendAction>(
        tilo_resource::LiveRow::typed::<NearbySendRoot>(RESOURCE, label)
            .into_resource()
            .with_personal_reading(),
    )
}
