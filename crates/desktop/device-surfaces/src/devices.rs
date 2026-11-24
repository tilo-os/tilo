//! The person's other devices as the device publishes them: what each one is, what it can take
//! from this one, the sounds that travel between them, and the session one of them hands this one.

use tilo_resource::Effect;

/// The reasons a key's access ends, as the record's revocations keep them.
pub const REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

/// What the resource offers beside its reading: a press on one of its rows.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.devices")]
pub enum DevicesAction {
    /// Press one of the person's devices
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Press {
        /// The row's key, as the reading lists it.
        #[param("The row, as the reading lists it")]
        row: String,
        /// What was pressed on it: its body, the action it names, or its answer to a device asking
        /// for this one's sound.
        #[param("The part of the row that was pressed")]
        part: RowPart,
    },
    /// Move a paired Bluetooth accessory to another of the person's devices
    #[effect(kind = "outside")]
    #[effect(own_device)]
    HandOff {
        /// The accessory's address.
        #[param("The accessory's address")]
        address: String,
        /// The device that takes it, as the reading lists its id.
        #[param("The device to move it to")]
        device: String,
    },
    /// Make this device one of the person's, with the ticket another of their devices printed
    #[effect(kind = "outside")]
    #[effect(own_device)]
    LogIn {
        /// The ticket the person's other device printed.
        #[param("The ticket to log in with")]
        ticket: String,
    },
    /// Take back another of the person's devices: its login ends everywhere, and every host it
    /// reaches refuses it from then on
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Revoke {
        /// The device's key, in hex, as their keys record and every list of theirs names it
        #[param("The device's key, in hex, as their keys record names it")]
        device: String,
        /// Why its access ends: `lost`, `stolen`, `retired` or `compromised`; `retired` when none
        /// is given.
        #[param("Why its access ends: lost, stolen, retired or compromised", choices = REASONS)]
        reason: Option<String>,
    },
    /// Name one of the person's keys: what every list of them, on every device of theirs, reads it
    /// by from then on. Two computers set up the same way enrol under one name, so the person tells
    /// them apart here.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Rename {
        /// The key, in hex, as their keys record names it
        #[param("The key, in hex, as their keys record names it")]
        device: String,
        /// The name it should read by
        #[param("The name it should read by")]
        name: String,
    },
    /// Start a new person on this device, when no person's certificate names it yet
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Found,
    /// Write a backup of this computer's identity, to carry it to another computer: the person
    /// seals it with a passphrase they choose, and the file goes where they choose
    #[effect(kind = "outside")]
    #[effect(own_device)]
    BackUpIdentity,
}

/// The Devices section as a view of another language reads it (`device.devices`): the person's
/// devices, and what plays here that one of them can take. The section itself publishes the rows
/// the panel draws ([`DevicesReading`]); a view reads the devices it shows and presses the same
/// resource, so the two never say different things.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct DevicesView {
    pub devices: Vec<PersonalDeviceView>,
    /// Every key the person's root certified, as the record of their own workspace lists them:
    /// their computers, the agents' grants and their own scripts, with the ones the root took back.
    #[serde(default)]
    pub access: Vec<AccessKey>,
    /// What plays here, titled, while media continuity is on: what a press on another device
    /// sends.
    #[serde(default)]
    pub playing: Option<String>,
    /// The sound of this device's that a press on a device would send.
    #[serde(default)]
    pub chosen: Option<u64>,
    /// The sounds this device can send to another, as the remote audio service reads them.
    #[serde(default)]
    pub sounds: Vec<SendableSound>,
    /// The sounds on their way to or from another device.
    #[serde(default)]
    pub routes: Vec<SoundRoute>,
    /// The devices asking to play a sound of this one's.
    #[serde(default)]
    pub asks: Vec<SoundAsk>,
    /// Whether this device holds a person's certificate: logged in, with workspaces of theirs to
    /// show.
    #[serde(default)]
    pub signed_in: bool,
    /// The workspaces this device's shell lists, as the person reads them.
    #[serde(default)]
    pub workspaces: Vec<DeviceWorkspace>,
    /// Why the last login or revocation failed, in the words of whoever refused it; empty when it
    /// did not.
    #[serde(default)]
    pub refused: String,
}

/// One of the person's devices as a view of another language reads it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the fabric says of this device now: independent facts"
)]
pub struct PersonalDeviceView {
    /// The fabric id, which names the device in the section's actions.
    pub id: String,
    pub label: String,
    /// It has a live session with this device now.
    pub online: bool,
    /// It is heard on this device's local network, so the two reach each other directly.
    pub nearby: bool,
    /// This machine.
    pub local: bool,
    /// It shows, so its display can be arranged beside this one's.
    pub display: bool,
    /// One of its interfaces wakes on a magic packet.
    pub wakeable: bool,
    /// It plays the sound this device sends it.
    pub audio_output: bool,
}

/// One key that may act as the person, as the record of their own workspace keeps it: one of
/// their computers, an agent's grant, or a key they made for their own script or backend.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one fact the record states of a key, read apart by a view"
)]
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct AccessKey {
    /// Its key, in hex: what a rename or a revocation names it by.
    pub id: String,
    /// What a list of the person's keys calls it.
    pub label: String,
    /// What kind of key it is.
    pub kind: AccessKind,
    /// The agent a computer runs ("Claude Code"), as the sessions it opened said; empty for a
    /// computer no agent works on.
    pub agent: String,
    /// This machine.
    pub current: bool,
    /// The person's root took it back: every device of theirs refuses it from then on.
    pub revoked: bool,
    /// It has a live session with this device now.
    pub online: bool,
    /// It is heard on this device's local network, so the two reach each other directly.
    pub nearby: bool,
    /// Its first six bytes, which every list of the person's keys shows it by.
    pub fingerprint: String,
    /// When the person's root certified it, in Unix milliseconds.
    #[serde(default)]
    pub issued_ms: Option<i64>,
    /// When it stops being good, in Unix milliseconds; none while it lasts until it is revoked.
    #[serde(default)]
    pub expires_ms: Option<i64>,
    /// When a session of it was last with a host, in Unix milliseconds; none before it ever was.
    #[serde(default)]
    pub last_active_ms: Option<i64>,
    /// The one workspace an agent's grant reaches, by its id; empty for every workspace of the
    /// person's, and for a computer.
    #[serde(default)]
    pub workspace: String,
}

/// What a key that may act as the person is.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum AccessKind {
    /// One of their devices.
    #[default]
    Device,
    /// An agent's grant: a key an agent signs with for them.
    Agent,
    /// A key they made for their own script or backend.
    Script,
}

/// The Devices section as a view of another language reads it (`device.devices`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<DevicesView, DevicesAction>("devices", "Devices")
}

/// What was pressed on a row of the Devices section.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RowPart {
    /// The row itself.
    Row,
    /// The one action the row names (stop, cancel, try again).
    Action,
    /// Allow, on a request.
    Allow,
    /// Decline, on a request.
    Decline,
    /// Drive the device with this machine's pointer.
    Control,
}

/// One sound of this device's that can be sent to another.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct SendableSound {
    /// Names the sound in the panel's effects, and follows it while the
    /// person can still pick it.
    pub token: u64,
    pub label: String,
    /// One application's sound, or everything this device plays.
    pub application: bool,
}

/// One route of a sound, on its way or playing.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct SoundRoute {
    /// Names the route in the panel's effects.
    pub id: String,
    /// The sound that travels, as the route names it.
    pub label: String,
    /// What the route is doing now, in words that name the other device.
    pub detail: String,
    /// The route stopped and is only shown until it is dismissed.
    pub stopped: bool,
}

/// Another of the person's devices asking to play a sound of this one's.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct SoundAsk {
    /// Names the ask in the panel's effects.
    pub id: String,
    /// The sound it would play here.
    pub media: String,
    /// The device asking, as the person knows it.
    pub source: String,
}

/// A workspace this device's shell lists, as a view of the devices reads it:
/// the same `key` and `name` the layout's workspaces go by.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct DeviceWorkspace {
    pub key: String,
    pub name: String,
}

/// One copy the desktop keeps, as the Clipboard section lists it: the row's
/// face without its bytes.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct ClipboardCopy {
    /// Names the copy in the panel's effects.
    pub key: String,
    /// The format its bytes are in.
    pub mime: String,
    /// Its first line, when it is text.
    pub text: Option<String>,
    /// When it was copied, in Unix milliseconds.
    pub created_ms: u64,
    /// Whether the person keeps it when the history is cleared.
    pub pinned: bool,
    /// The device another of the person's devices shared it from.
    pub from: Option<String>,
}
