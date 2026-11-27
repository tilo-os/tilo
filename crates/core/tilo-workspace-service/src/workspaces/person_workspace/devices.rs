//! The person's devices as their own workspace keeps them: what each says of
//! itself that lasts, under its key, and the Wi-Fi passphrases they share.
//!
//! Every key the person's root certifies is an entry: a device, an agent's
//! grant, a script's key, each marked with what it is ([`KeyKind`]). The record
//! answers who a key is; what it may do stays in each workspace's grants.
//!
//! Each entry holds the device's certificate, the name the person gave it, the
//! agent it runs and the revocation their root signed for it: what every list
//! of their devices reads, on any device of theirs, with whether it is online
//! now from presence beside it. Each device writes its own facts (what it is,
//! its speaker groups, its disk unlock, when it was last with a host), each the
//! postcard bytes of its type in hex under its name, and how the person's
//! session on it stands ([`OwnerSession`]), and a Wi-Fi passphrase one
//! of them learned is kept for all of them, sealed to the devices that may open
//! it. The workspace carries all of it to every device of the person, a device
//! that comes up later included.
//!
//! What the person answered when one of their devices asked to unlock its disk at boot is an
//! answer in the record ([`UnlockAnswer`]): the device that asked them writes it as they answer
//! ([`Devices::allow_unlock`], [`Devices::refuse_unlock`]), so their other devices answer the same
//! boot the same way while it holds, and the record's history is the person's list of them.
//!
//! A wake of one of their devices that sleeps is asked in the record too ([`WakeAsk`],
//! [`super::wake`]): the device that asks writes it, the devices on the sleeping one's LAN say
//! they sent the packets, and the woken device says it is back, so any device of theirs reads how
//! it went.
//!
//! Moving one of their Bluetooth accessories from one device of theirs to another is asked there
//! as well ([`AccessoryMove`], [`super::accessory`]): the device that has it asks, the other says
//! whether it holds the accessory's bond, the first says it let go, and the other says it
//! connected it, each step a write the two sessions read from the record.
//!
//! The person's one keyboard and pointer across their devices is handed over in the record too
//! ([`InputHand`]): the device whose pointer pushes past its screen's edge asks the one beyond for
//! it ([`Devices::ask_input`]), that device lets it in for a while when the person lets their
//! devices drive it ([`Devices::grant_input`]), and either ends it ([`Devices::end_input`]). Which
//! device drives which is read there by every device of theirs; the input itself travels between
//! the two.
//!
//! A notification an app sent on one of their devices is shown on the others from the record
//! ([`SharedNotice`]): the device it was sent on keeps it there while it is shown
//! ([`Devices::show_notice`]), a device the person dismissed it or chose one of its actions on
//! says so ([`Devices::answer_notice`]), and the device it was sent on carries that out and takes
//! it away ([`Devices::retire_notice`]), which takes it off every other device.
//!
//! What app each of their devices has in front is offered to the others in the record too
//! ([`ContinuityOffer`]): the device that had it in front writes it ([`Devices::offer_continuity`]),
//! and a device the person takes the offer on, or puts it away on, says so
//! ([`Devices::answer_continuity`]), so the bar there draws the chip once and a newer offer is a
//! new one to answer.
//!
//! What the person's root signs lives in their record: the mark of the device
//! that keeps it ([`Devices::mark_root_holder`]) and the revocation of one it
//! takes back ([`Devices::revoke_device`]), each a signature a reader checks
//! against that root where it reads the record. A revocation is kept once, in
//! the entry of the device it names, and an entry anything else signed is no
//! revocation and every reader leaves it out.

#[cfg(test)]
use tilo_resource::AppNamed;
use tilo_resource::{Collection, Id, Model, Refusal, resource};
use tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
use tilo_user_identity::{DeviceCert, RevocationEntry, revocation_of, revokes};

use super::speaker::{
    self, PlaybackStage, SpeakerConsent, SpeakerDevice, SpeakerLedGroup, SpeakerPlayback,
};

/// The resource of the person's devices in their own workspace.
pub const DEVICES: (&str, &str) = ("so.tilo.devices", "devices");

/// The most bytes one fact or passphrase of a device takes, hex.
const MAX_VALUE: usize = 256 * 1024;
/// The longest name of a fact.
const MAX_NAME: usize = 32;
/// The longest name of the agent a device runs.
const MAX_AGENT: usize = 64;

pub use tilo_user_identity::root_holder::root_holder_digest;

/// The fact under which a device's entry keeps when a session of it was last
/// with a host, ms since the epoch, little-endian.
pub const SEEN_FACT: &str = "seen";

/// The fact under which a device's entry keeps where its endpoint answers, as postcard of its
/// socket addresses in hex: the addresses another of the person's devices dials it at when no
/// relay and no discovery find it, as an offline device pairs on its own network.
pub const ENDPOINT_FACT: &str = "endpoint";

/// The resource under which a device's daemon publishes the person's devices that hold a session
/// with it now: what a shell reads to know which of their devices it reaches at this moment.
pub const ONLINE: &str = "so.tilo.device/online-devices";

/// The reading at [`ONLINE`]: the keys (lowercase hex) of the person's devices that are online
/// now, this device among them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OnlineDevices {
    #[serde(default)]
    pub devices: Vec<String>,
}

/// The resource under which a device's daemon publishes the person's other devices it hears on its
/// local network now: what a device row says is nearby, so reachable without a server.
pub const NEARBY: &str = "so.tilo.device/nearby-devices";

/// The reading at [`NEARBY`]: the keys (lowercase hex) of the person's devices heard on this
/// device's local network now, its own aside.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NearbyDevices {
    #[serde(default)]
    pub devices: Vec<String>,
}

/// The person's devices, by device key (lowercase hex), their Wi-Fi and their answers to boots.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Devices {
    pub devices: Collection<Device>,
    /// The passphrases, by network id (lowercase hex).
    pub wifi: Collection<Network>,
    /// The answers to boots that still hold, by [`UnlockAnswer::key`].
    pub unlocks: Collection<UnlockAnswer>,
    /// The latest wake of each device asked, by the key of the device to wake.
    pub wakes: Collection<WakeAsk>,
    /// The latest move of each Bluetooth accessory asked, by its address (uppercase).
    pub moves: Collection<AccessoryMove>,
    /// Who drives each device's keyboard and pointer, or asked to, by the key of the device driven.
    pub inputs: Collection<InputHand>,
    /// The notifications shown on each of the person's devices, by [`SharedNotice`] id.
    pub notices: Collection<SharedNotice>,
    /// What app each device last had in front, by the key of the device that had it there.
    pub continuity: Collection<ContinuityOffer>,
    /// What each device offers, leads and agreed to of speaker groups, by its key.
    pub speaker_devices: Collection<SpeakerDevice>,
    /// Each playback of a group to one member, by its route.
    pub speaker_playbacks: Collection<SpeakerPlayback>,
}

/// The longest a notification is shown on the person's other devices, ms.
pub const MAX_NOTICE_SHOWN_MS: i64 = 15 * 60 * 1_000;
/// The most actions a notification shown elsewhere offers.
const MAX_NOTICE_ACTIONS: usize = 16;

/// A notification an app sent on one of the person's devices, as their other devices show it: the
/// app, what it says (its text only when the person lets previews leave the device), the actions
/// it offers, and until when. Once the person dismissed it, or chose one of its actions, on another
/// device, the device they answered it on and what they chose. Kept by an id of 32 bytes in hex
/// the device it was sent on draws; times are ms since the epoch.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct SharedNotice {
    /// The device it was sent on, by key (lowercase hex).
    pub from: String,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    /// What it says; empty when its text stays on the device it was sent on.
    pub body: String,
    /// Its text stays on the device it was sent on.
    pub redacted: bool,
    pub actions: Vec<NoticeAction>,
    /// How much it matters: 0 says something, 1 warns, 2 tells of an error.
    pub urgency: i64,
    pub shown_ms: i64,
    pub until_ms: i64,
    /// The device the person answered it on, by key; empty while nobody did.
    pub answered_on: String,
    /// The key of the action they chose there; empty when they dismissed it.
    pub chosen: String,
}

/// One action a notification offers: its key, and what it says.
#[derive(
    Model,
    Default,
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct NoticeAction {
    pub key: String,
    pub label: String,
}

impl SharedNotice {
    /// Whether it is shown at `now_ms`: nobody answered it and its time has not run out.
    #[must_use]
    pub fn shown_at(&self, now_ms: i64) -> bool {
        self.answered_on.is_empty() && now_ms < self.until_ms
    }
}

/// The longest an offer to continue an app is worth showing, ms.
pub const MAX_CONTINUITY_SHOWN_MS: i64 = 60 * 60 * 1_000;

/// What one of the person's devices last had in front: the app, the resource it showed and the
/// view it showed it through, so another of their devices opens the same thing where it was. Each
/// device keeps one, under its own key, replaced as its own focus moves; the person's other devices
/// read it ([`Devices::offer_continuity`]) and say whether they took it or put it away
/// ([`Devices::answer_continuity`]). Times are ms since the epoch.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct ContinuityOffer {
    /// The device it was in front on, by key (lowercase hex).
    pub from: String,
    /// The app that had it in front, by id.
    pub app_id: String,
    /// The app's name, as the person at that device reads it.
    pub app_name: String,
    /// The resource the app showed, by its address; empty for an app opened on nothing of its
    /// own, which the taking device opens as an app.
    pub resource: String,
    /// The view it showed it through; empty for the app's primary one, and for a window that
    /// showed no resource.
    pub view: String,
    /// When it was in front there, ms since the epoch.
    pub shown_ms: i64,
    /// When the offer stops being worth showing, ms since the epoch.
    pub until_ms: i64,
    /// The devices the person took the offer on or put it away on, each once.
    pub answers: Vec<ContinuityAnswer>,
}

impl ContinuityOffer {
    /// Whether it is still worth showing at `now_ms`: its time has not run out.
    #[must_use]
    pub fn shown_at(&self, now_ms: i64) -> bool {
        now_ms < self.until_ms
    }

    /// What the person did with it on `device`: whether they took it, `None` where they have
    /// neither taken nor put it away there.
    #[must_use]
    pub fn answer_of(&self, device: &str) -> Option<bool> {
        self.answers
            .iter()
            .find(|answer| answer.device == device)
            .map(|answer| answer.taken)
    }
}

/// What the person did with an offer on one of their devices.
#[derive(
    Model,
    Default,
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct ContinuityAnswer {
    /// The device they answered on, by key (lowercase hex).
    pub device: String,
    /// They opened what it offered here, rather than putting it away.
    pub taken: bool,
}

/// One move of a Bluetooth accessory from one of the person's devices to another, and how far it
/// got ([`super::accessory`]). Times are ns since the epoch, `0` while it did not happen.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct AccessoryMove {
    /// The device that has the accessory, by key (lowercase hex).
    pub from: String,
    /// The device it moves to, by key.
    pub to: String,
    /// What the accessory is called, as the device that has it read.
    pub name: String,
    pub asked_ns: i64,
    pub stage: MoveStage,
    /// When the device that had it let go of it.
    pub released_ns: i64,
    /// When the move reached its stage.
    pub at_ns: i64,
}

/// How far one move of an accessory got.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MoveStage {
    /// Asked: the device it moves to checks that it holds the accessory's bond.
    #[default]
    Asked,
    /// The device it moves to holds the bond: the one that has it lets go.
    Ready,
    /// The device that had it let go: the other connects it.
    Released,
    /// The device it moved to connected it.
    Moved,
    /// The device it was to move to holds no bond of it, so it stayed.
    NotPaired,
    /// The device that has it could not let go of it, so it stayed.
    NotReleased,
    /// The device it was to move to could not connect it, so it went back.
    NotConnected,
    /// One of the two gave the move up, because the other left or it took too long.
    GivenUp,
}

/// The person's keyboard and pointer of one device of theirs, handed to another: the device that
/// asked to drive it and when, and once the driven device let it in, what it lets in, under which
/// nonce and until when. Times are ms since the epoch; `granted_ms` is `0` while it was only asked.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct InputHand {
    /// The device that drives, or asked to, by key (lowercase hex).
    pub driver: String,
    pub asked_ms: i64,
    /// What the input under this hand-over is told by, 16 bytes in hex: empty while asked.
    pub nonce: String,
    pub keyboard: bool,
    pub pointer: bool,
    pub granted_ms: i64,
    pub until_ms: i64,
}

impl InputHand {
    /// Whether `driver` drives the device under it at `now_ms`, letting in what it lets in.
    #[must_use]
    pub fn holds(&self, driver: &str, now_ms: i64) -> bool {
        self.driver == driver && self.granted_ms > 0 && now_ms < self.until_ms
    }
}

/// One wake of a device of the person's that sleeps: who asked it and when, which device of theirs
/// sent the magic packets and on which LAN, and when the device said it was back
/// ([`super::wake::WakeState`]). Times are ns since the epoch, `0` while it did not happen.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct WakeAsk {
    /// The device that asked, by key (lowercase hex).
    pub asked_by: String,
    pub asked_ns: i64,
    /// The device that sent the packets, by key, empty until one did.
    pub sent_by: String,
    /// The subnet it sent them on, `a.b.c.d/prefix`.
    pub subnet: String,
    pub sent_ns: i64,
    /// When the woken device said it was back.
    pub awake_ns: i64,
}

/// What the person answered one boot of theirs that asked to unlock its disk: on which device,
/// for which boot, and until when it holds. A boot that asks again while it holds hears the same
/// answer on every device of theirs, without the person being asked again.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct UnlockAnswer {
    /// The device the person answered on, by key (lowercase hex).
    pub device: String,
    /// The enrolment that asked: the digest of its boot key's certificate, hex.
    pub enrolment: String,
    /// The boot that asked: the id it drew, hex.
    pub boot: String,
    /// The disk it asked for: its volume's id in hex and its key slot, `volume:slot`.
    pub slot: String,
    /// They let it unlock; else they refused it.
    pub allowed: bool,
    /// When they answered, ns since the epoch.
    pub at_ns: i64,
    /// When it stops holding, and the boot is put to them again, ns since the epoch.
    pub until_ns: i64,
}

impl UnlockAnswer {
    /// What it is kept under: the boot it answers, once per boot, enrolment and disk.
    #[must_use]
    pub fn key(&self) -> Id {
        Id::from_raw(format!("{}-{}-{}", self.enrolment, self.boot, self.slot))
    }
}

/// What a key the person's root certified is.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// One of their devices.
    #[default]
    Device,
    /// An agent's grant: a key an agent signs with for them.
    Agent,
    /// A key the person made for their own script or backend.
    Script,
    /// A host's placement grant: the host, none of their devices, places devices for the
    /// records it names ([`tilo_user_identity::DeviceCapabilities::placing`]).
    Placing,
}

impl KeyKind {
    /// What `cert` certifies.
    #[must_use]
    pub fn of(cert: &DeviceCert) -> Self {
        use tilo_server_protocol::device_tickets::KeyKind as Listed;
        let capabilities = &cert.capabilities;
        match Listed::of(
            capabilities.placing.is_some(),
            capabilities.server_key,
            capabilities.agent.is_some(),
        ) {
            Listed::Device => Self::Device,
            Listed::Agent => Self::Agent,
            Listed::Script => Self::Script,
            Listed::Placing => Self::Placing,
        }
    }
}

/// What one key says of itself that lasts.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Device {
    /// Its certificate, postcard in hex, which the person's root signed: a
    /// reader shows only a device whose certificate holds.
    pub certificate: String,
    /// What the key is, as its certificate says.
    pub kind: KeyKind,
    /// The name the person gave it ([`Devices::name_device`]): what their
    /// lists read it by instead of the label it was certified with, so two
    /// computers enrolled under one name read apart.
    pub name: Option<String>,
    /// The agent the key runs ("Claude Code"), as the sessions it opened said:
    /// a computer set up with `tilo login` is a device an agent works on.
    pub agent: Option<String>,
    /// The revocation the person's root signed for it, postcard in hex
    /// ([`Devices::revoke_device`]): it stays listed, marked, and every device of theirs refuses
    /// it. A reader verifies it against that root ([`Device::revocation`]).
    pub revocation: Option<String>,
    /// It keeps the person's root: their root's signature over
    /// [`root_holder_digest`] of its key, hex ([`Devices::mark_root_holder`]).
    pub keeps_root: Option<String>,
    /// Each fact under its name, postcard in hex.
    pub facts: std::collections::BTreeMap<String, String>,
    /// How the person's session on it stands, as it said last ([`Devices::say_session`]).
    pub session: OwnerSession,
}

/// How the person's session on one of their devices stands: what their other devices follow, so
/// the lock follows them from one device to the next. A device says it only when it changes (a
/// lock, an unlock, the person starting or stopping using it), never as they type, so the record
/// moves a few times an hour at most.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerSession {
    /// The session is locked.
    pub locked: bool,
    /// When the person last locked or unlocked it themselves, ms since the epoch: `0` while it is
    /// locked by its idle timer or by following another device, a lock the others do not follow.
    pub changed_ms: i64,
    /// Their last input on it as of when it last said its session, ms since the epoch.
    pub active_ms: i64,
    /// They were using it when it said its session, and go on until it says otherwise.
    pub using: bool,
}

impl Device {
    /// Its certificate, when it reads: the person's root signed it, which a
    /// reader checks before it shows the device.
    #[must_use]
    pub fn cert(&self) -> Option<DeviceCert> {
        tilo_failure::ignored!(
            "a device's certificate is the person's root's to sign",
            postcard::from_bytes(&hex::decode(&self.certificate).ok()?).ok()
        )
    }

    /// Whether it keeps the person's root, as the root's own signature over its key says.
    #[must_use]
    pub fn keeps_root(&self, cert: &DeviceCert) -> bool {
        self.keeps_root
            .as_deref()
            .is_some_and(|signature| root_signed(cert, signature))
    }

    /// The revocation the person's root signed for it, when the entry the record holds verifies
    /// against that root: one anything else signed, or one naming another root or another device,
    /// is no revocation and reads as none.
    #[must_use]
    pub fn revocation(&self, cert: &DeviceCert) -> Option<RevocationEntry> {
        tilo_failure::ignored!(
            "a revocation in the person's devices record is the root's own signature",
            revocation_of(self.revocation.as_deref()?, cert)
        )
    }

    /// Whether the person's root took it back, as the entry the record holds says.
    #[must_use]
    pub fn revoked(&self, cert: &DeviceCert) -> bool {
        self.revocation(cert).is_some()
    }

    /// What a list calls it: the name the person gave it, else `cert`'s label.
    #[must_use]
    pub fn called(&self, cert: &DeviceCert) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| cert.device_label.clone())
    }

    /// When a session of it was last with a host, when its entry says.
    #[must_use]
    pub fn seen_ms(&self) -> Option<i64> {
        let bytes: [u8; 8] = hex::decode(self.facts.get(SEEN_FACT)?)
            .ok()?
            .try_into()
            .ok()?;
        Some(i64::from_le_bytes(bytes))
    }

    /// Where this device says its endpoint answers, as its entry keeps it: the addresses another
    /// of the person's devices dials it at when no relay and no discovery find it. Empty while
    /// it has said none.
    #[must_use]
    pub fn endpoint_addrs(&self) -> Vec<std::net::SocketAddr> {
        let Some(value) = self.facts.get(ENDPOINT_FACT) else {
            return Vec::new();
        };
        let Ok(bytes) = hex::decode(value) else {
            return Vec::new();
        };
        tilo_failure::ignored!(
            "a device's endpoint fact is the device's own to write",
            postcard::from_bytes(&bytes).ok()
        )
        .unwrap_or_default()
    }
}

/// One of the person's devices as a list of them shows it: the entry, with
/// the certificate it holds read and checked against `person`'s root.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    /// Its key, lowercase hex.
    pub key: String,
    pub cert: DeviceCert,
    pub device: Device,
}

impl Devices {
    /// The action that keeps `cert`, which the person's root signed, among their keys: a device,
    /// an agent's grant or a script's key alike.
    ///
    /// # Errors
    /// The certificate does not encode.
    pub fn keeping(cert: &DeviceCert) -> Result<tilo_resource::Invoke<Self>, String> {
        let certificate = postcard::to_allocvec(cert).map_err(|error| error.to_string())?;
        Ok(Self::KEEP_DEVICE.with((
            hex::encode(cert.device_public_key),
            hex::encode(certificate),
        )))
    }

    /// The action that says `session` as how the person's session on `device` stands.
    #[must_use]
    pub fn saying_session(device: &[u8; 32], session: OwnerSession) -> tilo_resource::Invoke<Self> {
        Self::SAY_SESSION.with((
            hex::encode(device),
            session.locked,
            session.changed_ms,
            session.active_ms,
            session.using,
        ))
    }

    /// The action that asks to drive `driven` from `driver` at `asked_ms`.
    #[must_use]
    pub fn asking_input(
        driven_device: &[u8; 32],
        from_device: &[u8; 32],
        asked_ms: i64,
    ) -> tilo_resource::Invoke<Self> {
        Self::ASK_INPUT.with((
            hex::encode(driven_device),
            hex::encode(from_device),
            asked_ms,
        ))
    }

    /// The action that lets `hand.driver` drive `driven` as `hand` says.
    #[must_use]
    pub fn granting_input(driven: &[u8; 32], hand: &InputHand) -> tilo_resource::Invoke<Self> {
        Self::GRANT_INPUT.with(HandOver {
            driven: hex::encode(driven),
            driver: hand.driver.clone(),
            asked_ms: hand.asked_ms,
            nonce: hand.nonce.clone(),
            keyboard: hand.keyboard,
            pointer: hand.pointer,
            granted_ms: hand.granted_ms,
            until_ms: hand.until_ms,
        })
    }

    /// The action that ends the hand-over of `driven` to `driver` under `nonce`.
    #[must_use]
    pub fn ending_input(
        driven_device: &[u8; 32],
        from_device: &[u8; 32],
        nonce: &str,
    ) -> tilo_resource::Invoke<Self> {
        Self::END_INPUT.with((
            hex::encode(driven_device),
            hex::encode(from_device),
            nonce.to_owned(),
        ))
    }

    /// The action that shows `notice` on the person's other devices under `id`.
    #[must_use]
    pub fn showing_notice(id: &str, notice: &SharedNotice) -> tilo_resource::Invoke<Self> {
        Self::SHOW_NOTICE.with(NoticeShown {
            id: id.to_owned(),
            from: notice.from.clone(),
            app_id: notice.app_id.clone(),
            app_name: notice.app_name.clone(),
            title: notice.title.clone(),
            body: notice.body.clone(),
            redacted: notice.redacted,
            actions: notice.actions.clone(),
            urgency: notice.urgency,
            shown_ms: notice.shown_ms,
            until_ms: notice.until_ms,
        })
    }

    /// The action that says the person answered the notification `id` on `device`: `chosen` the
    /// action they chose, empty for dismissing it.
    #[must_use]
    pub fn answering_notice(
        id: &str,
        device: &[u8; 32],
        chosen: &str,
    ) -> tilo_resource::Invoke<Self> {
        Self::ANSWER_NOTICE.with((id.to_owned(), hex::encode(device), chosen.to_owned()))
    }

    /// The action that takes the notification `id` off the person's devices.
    #[must_use]
    pub fn retiring_notice(id: &str) -> tilo_resource::Invoke<Self> {
        Self::RETIRE_NOTICE.with(id.to_owned())
    }

    /// The action that offers what `offer` says this device has in front to the person's others.
    #[must_use]
    pub fn offering_continuity(offer: &ContinuityOffer) -> tilo_resource::Invoke<Self> {
        Self::OFFER_CONTINUITY.with(OfferShown {
            from: offer.from.clone(),
            app_id: offer.app_id.clone(),
            app_name: offer.app_name.clone(),
            resource: offer.resource.clone(),
            view: offer.view.clone(),
            shown_ms: offer.shown_ms,
            until_ms: offer.until_ms,
        })
    }

    /// The action that says the person took the offer `from` made at `at_ms` on `device`, or put
    /// it away there.
    #[must_use]
    pub fn answering_continuity(
        from: &str,
        at_ms: i64,
        device: &[u8; 32],
        taken: bool,
    ) -> tilo_resource::Invoke<Self> {
        Self::ANSWER_CONTINUITY.with((from.to_owned(), at_ms, hex::encode(device), taken))
    }

    /// The action that keeps `answer` as what the person answered a boot.
    #[must_use]
    pub fn answering(answer: &UnlockAnswer) -> tilo_resource::Invoke<Self> {
        let arguments = (
            answer.device.clone(),
            answer.enrolment.clone(),
            answer.boot.clone(),
            answer.slot.clone(),
            answer.at_ns,
            answer.until_ns,
        );
        if answer.allowed {
            Self::ALLOW_UNLOCK.with(arguments)
        } else {
            Self::REFUSE_UNLOCK.with(arguments)
        }
    }

    /// Keep `answer`, which replaces an earlier answer to the same boot; answers that no longer
    /// hold by the time it was made go.
    fn answer(&mut self, answer: UnlockAnswer) -> Result<(), Refusal> {
        validate_key(&answer.device)?;
        if !self.devices.contains(&Id::from_raw(answer.device.clone())) {
            return Err(Refusal::new("that key is not one of your devices"));
        }
        validate_bytes(&answer.enrolment, 32)?;
        validate_bytes(&answer.boot, 16)?;
        let named = answer.slot.split_once(':').is_some_and(|(volume, slot)| {
            validate_bytes(volume, 16).is_ok()
                && slot
                    .parse::<u16>()
                    .is_ok_and(|parsed| parsed.to_string() == slot)
        });
        if !named {
            return Err(Refusal::new(
                "a disk is named by its volume and its key slot",
            ));
        }
        if answer.at_ns < 0 || answer.until_ns <= answer.at_ns {
            return Err(Refusal::new(
                "an answer holds from when it was made until a later time",
            ));
        }
        let key = answer.key();
        let starting_at = answer.at_ns;
        self.unlocks
            .retain(|id, kept| *id != key && kept.until_ns > starting_at);
        self.unlocks.push(key, answer);
        Ok(())
    }

    /// The devices of `person`'s that keep their root, as the root marked them: not one taken
    /// back.
    #[must_use]
    pub fn root_holders(&self, person: &[u8; 32]) -> Vec<[u8; 32]> {
        self.listed(person)
            .into_iter()
            .filter(|listed| {
                !listed.device.revoked(&listed.cert) && listed.device.keeps_root(&listed.cert)
            })
            .map(|listed| listed.cert.device_public_key)
            .collect()
    }

    /// The revocations the record holds, each verified against the root that certified the device
    /// it names: an entry that does not verify is left out, never trusted.
    #[must_use]
    pub fn revocations(&self, person: &[u8; 32]) -> Vec<RevocationEntry> {
        self.listed(person)
            .into_iter()
            .chain(self.placing_grants(person))
            .filter_map(|listed| listed.device.revocation(&listed.cert))
            .collect()
    }

    /// The placement grants `person` gave hosts that are none of their devices, as each entry
    /// holds it: an entry whose grant does not read, or that another root's chain signed, is left
    /// out. Revoked ones are listed, marked, as a revoked device is.
    #[must_use]
    pub fn placing_grants(&self, person: &[u8; 32]) -> Vec<Listed> {
        self.devices
            .iter()
            .filter_map(|(id, device)| {
                let cert = device.cert()?;
                if cert.user_public_key != *person
                    || tilo_user_identity::verify_placing_grant(&cert).is_err()
                {
                    return None;
                }
                Some(Listed {
                    key: id.as_str().to_owned(),
                    cert,
                    device: device.clone(),
                })
            })
            .collect()
    }

    /// The placement grant `person` gave the host `host` that lets it place a device for
    /// `records` at `now_ns`: live then, naming those records, and refused by none of the
    /// revocations this record holds (its own, or that of the device of theirs that signed it).
    #[must_use]
    pub fn placing_grant(
        &self,
        person: &[u8; 32],
        host: &[u8; 32],
        records: &tilo_user_identity::PlacedRecords,
        now_ns: u64,
    ) -> Option<DeviceCert> {
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in self.revocations(person) {
            tilo_failure::ignored!(
                "each revocation the record lists verified against its root as it was read",
                revocations.insert(entry).ok()
            );
        }
        self.placing_grants(person)
            .into_iter()
            .map(|listed| listed.cert)
            .filter(|grant| {
                grant.device_public_key == *host
                    && grant
                        .capabilities
                        .placing
                        .as_ref()
                        .is_some_and(|granted| records.within(granted))
                    && tilo_user_identity::verify_placing_grant_at(grant, now_ns).is_ok()
                    && !revocations.refuses(grant)
            })
            .max_by_key(|grant| grant.issued_at_ns)
    }

    /// The certificates of `person`'s keys the record keeps that hold at `now_ns`: what one of
    /// their devices seals to, offers a file to or dials as theirs. One another root signed, or
    /// that ran out, is left out.
    #[must_use]
    pub fn certified_at(&self, person: &[u8; 32], now_ns: u64) -> Vec<DeviceCert> {
        self.certified_at_with_endpoints(person, now_ns)
            .into_iter()
            .map(|(cert, _)| cert)
            .collect()
    }

    /// [`Self::certified_at`], each with where its entry says its endpoint answers
    /// ([`Device::endpoint_addrs`]): the addresses another of the person's devices dials it at
    /// when no relay and no discovery find it.
    #[must_use]
    pub fn certified_at_with_endpoints(
        &self,
        person: &[u8; 32],
        now_ns: u64,
    ) -> Vec<(DeviceCert, Vec<std::net::SocketAddr>)> {
        self.devices
            .iter()
            .filter_map(|(_, device)| Some((device.cert()?, device)))
            .filter(|(cert, _)| {
                cert.user_public_key == *person
                    && tilo_user_identity::verify_device_cert_at(cert, now_ns).is_ok()
            })
            .map(|(cert, device)| (cert, device.endpoint_addrs()))
            .collect()
    }

    /// The devices `person`'s root certified, as each entry holds it: an entry
    /// whose certificate does not read, or that another root signed, is left
    /// out.
    #[must_use]
    pub fn listed(&self, person: &[u8; 32]) -> Vec<Listed> {
        self.devices
            .iter()
            .filter_map(|(id, device)| {
                let cert = device.cert()?;
                if cert.user_public_key != *person
                    || tilo_user_identity::verify_device_cert(&cert).is_err()
                {
                    return None;
                }
                Some(Listed {
                    key: id.as_str().to_owned(),
                    cert,
                    device: device.clone(),
                })
            })
            .collect()
    }
}

/// One network's passphrase, sealed to the devices that may open it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Network {
    /// The credential, postcard in hex.
    pub credential: String,
}

/// A notification one device of the person's shows on the others.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct NoticeShown {
    /// The notification, which a later answer or retirement names.
    pub id: String,
    /// The device that showed it, by key (lowercase hex).
    pub from: String,
    /// The app that sent it, by id.
    pub app_id: String,
    /// The app's name, as the person reads it.
    pub app_name: String,
    /// What it says.
    pub title: String,
    /// Its text, empty when it is redacted.
    pub body: String,
    /// Whether the text stays on the device that sent it.
    pub redacted: bool,
    /// What a person may choose on it.
    pub actions: Vec<NoticeAction>,
    /// How it presents itself: 0 says something, 1 warns, 2 tells of an error.
    pub urgency: i64,
    /// When it started showing, ms since the epoch.
    pub shown_ms: i64,
    /// When it stops showing, ms since the epoch.
    pub until_ms: i64,
}

/// What one device offers to the person's others: the app it had in front and where in it.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct OfferShown {
    /// The device it was in front on, by key (lowercase hex).
    pub from: String,
    /// The app that had it in front, by id.
    pub app_id: String,
    /// The app's name, as the person at that device reads it.
    pub app_name: String,
    /// The resource the app showed, by its address; empty for an app opened on nothing of its
    /// own, which the taking device opens as an app.
    pub resource: String,
    /// The view it showed it through; empty for the app's primary one, and for a window that
    /// showed no resource.
    pub view: String,
    /// When it was in front there, ms since the epoch.
    pub shown_ms: i64,
    /// When the offer stops being worth showing, ms since the epoch.
    pub until_ms: i64,
}

/// The hand-over one device of the person's asks for another's keyboard and pointer.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct HandOver {
    /// The device whose keyboard and pointer are handed over, by key (lowercase hex).
    pub driven: String,
    /// The device that drives it, by key (lowercase hex).
    pub driver: String,
    /// When the driving device asked, ms since the epoch.
    pub asked_ms: i64,
    /// What the input under the hand-over is told by, 16 bytes in hex.
    pub nonce: String,
    /// Whether the keyboard is handed over.
    pub keyboard: bool,
    /// Whether the pointer is handed over.
    pub pointer: bool,
    /// When the hand-over was made, ms since the epoch.
    pub granted_ms: i64,
    /// When it ends, ms since the epoch.
    pub until_ms: i64,
}

/// The answer a member device gives to a group its leader wrote: to play in it, or to let it
/// listen with a microphone, as the person on that device approved or declined it.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::JsonSchema,
)]
pub struct SpeakerConsentGiven {
    /// The answering device, by key (lowercase hex).
    pub device: String,
    /// The group, 16 bytes in hex.
    pub group: String,
    /// The device that leads it, by key.
    pub leader: String,
    /// Whether it consents to play in the group; otherwise to listen with a microphone.
    pub play: bool,
    /// The output it plays on, or the microphone it listens with, 16 bytes in hex.
    pub endpoint: String,
    /// The role it plays, as the leader wrote it.
    pub role: String,
    /// Whether the person approved it; false when they declined.
    pub approved: bool,
    /// When they answered, ms since the epoch.
    pub at_ms: i64,
}

#[resource(kind = "tilo.devices", read_content = tilo_resource::read::fields)]
impl Devices {
    /// Keep `certificate` as what `device` is, adding its entry when the
    /// person's root certified it and it is not there yet.
    #[action(housekeeping)]
    pub fn keep_device(&mut self, device: String, certificate: String) -> Result<(), Refusal> {
        let device = Id::from_raw(device);
        validate_key(device.as_str())?;
        validate_hex(&certificate)?;
        let kind = kind_of(&certificate);
        match self.devices.get_mut(&device) {
            Some(kept) => {
                kept.certificate = certificate;
                kept.kind = kind;
            }
            None => {
                self.devices.push(
                    device,
                    Device {
                        certificate,
                        kind,
                        ..Device::default()
                    },
                );
            }
        }
        Ok(())
    }

    /// Call `device` `name` in every list of the person's devices, instead of
    /// the label it was certified with. No other device of theirs that is not
    /// taken back may read by that name already.
    #[action]
    pub fn name_device(&mut self, device: String, name: String) -> Result<(), Refusal> {
        let name = name.trim().to_owned();
        if name.is_empty()
            || name.len() > MAX_CERTIFICATION_LABEL_BYTES
            || name.chars().any(char::is_control)
        {
            return Err(Refusal::new(format!(
                "a device's name is 1 to {MAX_CERTIFICATION_LABEL_BYTES} bytes, without control \
                 characters"
            )));
        }
        let taken = self
            .devices
            .iter()
            .filter(|(id, _)| id.as_str() != device)
            .filter_map(|(_, other)| other.cert().map(|cert| (other, cert)))
            .filter(|(other, cert)| !other.revoked(cert))
            .any(|(other, cert)| other.called(&cert) == name);
        if taken {
            return Err(Refusal::new(format!(
                "another of your devices is already called “{name}”"
            )));
        }
        let kept = self
            .devices
            .get_mut(&Id::from_raw(device))
            .ok_or_else(|| Refusal::new("that key is not one of your devices"))?;
        kept.name = Some(name);
        Ok(())
    }

    /// Say that `device` runs the agent `agent`, as its session named it.
    #[action(housekeeping)]
    pub fn keep_agent(&mut self, device: String, agent: String) -> Result<(), Refusal> {
        let agent = agent.trim().to_owned();
        if agent.is_empty()
            || agent.chars().count() > MAX_AGENT
            || agent.chars().any(char::is_control)
        {
            return Err(Refusal::new(
                "an agent is named in one line of 1 to 64 characters",
            ));
        }
        let kept = self
            .devices
            .get_mut(&Id::from_raw(device))
            .ok_or_else(|| Refusal::new("that key is not one of your devices"))?;
        kept.agent = Some(agent);
        Ok(())
    }

    /// Mark `device` taken back with `revocation`, the entry the person's root signed over it
    /// (postcard in hex, [`RevocationEntry`]): checked against the root its certificate names, so
    /// only that root's own signature marks one of their devices, and every host that holds this
    /// record refuses the device from the moment the entry arrives.
    #[action]
    pub fn revoke_device(&mut self, device: String, revocation: String) -> Result<(), Refusal> {
        let kept = self
            .devices
            .get_mut(&Id::from_raw(device))
            .ok_or_else(|| Refusal::new("that key is not one of your devices"))?;
        validate_hex(&revocation)?;
        let entry: RevocationEntry = hex::decode(&revocation)
            .map_err(|error| {
                Refusal::new(format!("a revocation is postcard bytes in hex: {error}"))
            })
            .and_then(|bytes| {
                postcard::from_bytes(&bytes).map_err(|error| {
                    Refusal::new(format!("a revocation is a signed entry: {error}"))
                })
            })?;
        let cert = kept
            .cert()
            .ok_or_else(|| Refusal::new("that device's certificate does not read"))?;
        if !revokes(&entry, &cert) {
            return Err(Refusal::new(
                "only the person's root revokes one of their devices",
            ));
        }
        kept.revocation = Some(revocation);
        Ok(())
    }

    /// Mark `device` as the one that keeps the person's root, with `signature`, that root's over
    /// [`root_holder_digest`] of its key (hex): checked against the root its certificate names.
    #[action]
    pub fn mark_root_holder(&mut self, device: String, signature: String) -> Result<(), Refusal> {
        let kept = self
            .devices
            .get_mut(&Id::from_raw(device))
            .ok_or_else(|| Refusal::new("that key is not one of your devices"))?;
        let cert = kept
            .cert()
            .ok_or_else(|| Refusal::new("that device's certificate does not read"))?;
        if !root_signed(&cert, &signature) {
            return Err(Refusal::new(
                "only the person's root marks the device that keeps it",
            ));
        }
        kept.keeps_root = Some(signature);
        Ok(())
    }

    /// Keep `value` as the fact `name` of `device`, whose certificate is
    /// `certificate`.
    #[action(housekeeping)]
    pub fn keep_fact(
        &mut self,
        device: String,
        certificate: String,
        name: String,
        value: String,
    ) -> Result<(), Refusal> {
        let device = Id::from_raw(device);
        validate_key(device.as_str())?;
        validate_hex(&certificate)?;
        validate_hex(&value)?;
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(Refusal::new("a device's fact is named in 1 to 32 bytes"));
        }
        match self.devices.get_mut(&device) {
            Some(kept) => {
                kept.certificate = certificate;
                kept.facts.insert(name, value);
            }
            None => {
                self.devices.push(
                    device,
                    Device {
                        kind: kind_of(&certificate),
                        certificate,
                        facts: std::iter::once((name, value)).collect(),
                        ..Device::default()
                    },
                );
            }
        }
        Ok(())
    }

    /// Say how the person's session on `device` stands: whether it is locked, when they last
    /// locked or unlocked it themselves (`changed_ms`, `0` for a lock they did not take there),
    /// their last input on it (`active_ms`) and whether they are using it. A device says only its
    /// own, once the record keeps it.
    #[action(housekeeping)]
    pub fn say_session(
        &mut self,
        device: String,
        locked: bool,
        changed_ms: i64,
        active_ms: i64,
        using: bool,
    ) -> Result<(), Refusal> {
        if changed_ms < 0 || active_ms < 0 {
            return Err(Refusal::new("a session's times are ms since the epoch"));
        }
        let kept = self
            .devices
            .get_mut(&Id::from_raw(device))
            .ok_or_else(|| Refusal::new("that key is not one of your devices"))?;
        kept.session = OwnerSession {
            locked,
            changed_ms,
            active_ms,
            using,
        };
        Ok(())
    }

    /// Let the boot `boot` of the enrolment `enrolment` unlock the disk `slot`, as the person did
    /// on `device` at `at_ns`, until `until_ns`.
    #[action]
    pub fn allow_unlock(
        &mut self,
        device: String,
        enrolment: String,
        boot: String,
        slot: String,
        at_ns: i64,
        until_ns: i64,
    ) -> Result<(), Refusal> {
        self.answer(UnlockAnswer {
            device,
            enrolment,
            boot,
            slot,
            allowed: true,
            at_ns,
            until_ns,
        })
    }

    /// Refuse the boot `boot` of the enrolment `enrolment` the disk `slot`, as the person did on
    /// `device` at `at_ns`, until `until_ns`.
    #[action]
    pub fn refuse_unlock(
        &mut self,
        device: String,
        enrolment: String,
        boot: String,
        slot: String,
        at_ns: i64,
        until_ns: i64,
    ) -> Result<(), Refusal> {
        self.answer(UnlockAnswer {
            device,
            enrolment,
            boot,
            slot,
            allowed: false,
            at_ns,
            until_ns,
        })
    }

    /// Wake `target`, one of the person's devices that sleeps, as `asked_by` asked at `asked_ns`:
    /// replaces how an earlier wake of it went.
    #[action]
    pub fn wake_device(
        &mut self,
        target: String,
        asked_by: String,
        asked_ns: i64,
    ) -> Result<(), Refusal> {
        let target = Id::from_raw(target);
        if !self.devices.contains(&target)
            || !self.devices.contains(&Id::from_raw(asked_by.clone()))
        {
            return Err(Refusal::new("that key is not one of your devices"));
        }
        if asked_ns <= 0 {
            return Err(Refusal::new("a wake is asked at a time since the epoch"));
        }
        let ask = WakeAsk {
            asked_by,
            asked_ns,
            ..WakeAsk::default()
        };
        match self.wakes.get_mut(&target) {
            Some(kept) => *kept = ask,
            None => {
                self.wakes.push(target, ask);
            }
        }
        Ok(())
    }

    /// Say that `by` sent the packets for the wake of `target` asked at `asked_ns`, on `subnet`
    /// at `at_ns`. The first device to send is the one kept; a later wake asked meanwhile is left
    /// as it is.
    #[action(housekeeping)]
    pub fn wake_sent(
        &mut self,
        target: String,
        asked_ns: i64,
        by: String,
        subnet: String,
        at_ns: i64,
    ) -> Result<(), Refusal> {
        validate_key(&by)?;
        if subnet.parse::<super::wake::Ipv4Subnet>().is_err() {
            return Err(Refusal::new("a subnet is a.b.c.d/prefix in canonical form"));
        }
        if at_ns <= 0 {
            return Err(Refusal::new("packets are sent at a time since the epoch"));
        }
        if let Some(kept) = self.wakes.get_mut(&Id::from_raw(target))
            && kept.asked_ns == asked_ns
            && kept.sent_ns == 0
        {
            kept.sent_by = by;
            kept.subnet = subnet;
            kept.sent_ns = at_ns;
        }
        Ok(())
    }

    /// Say that `target` is back, at `at_ns`, from the wake asked at `asked_ns`.
    #[action(housekeeping)]
    pub fn woke(&mut self, target: String, asked_ns: i64, at_ns: i64) -> Result<(), Refusal> {
        if at_ns <= 0 {
            return Err(Refusal::new("a device is back at a time since the epoch"));
        }
        if let Some(kept) = self.wakes.get_mut(&Id::from_raw(target))
            && kept.asked_ns == asked_ns
            && kept.awake_ns == 0
        {
            kept.awake_ns = at_ns;
        }
        Ok(())
    }

    /// Move the Bluetooth accessory at `address`, called `name`, from the device `from` to the
    /// device `to`, as asked at `asked_ns`. Replaces an earlier move of it that ended or ran out
    /// of time; one still under way stays.
    #[action]
    pub fn move_accessory(
        &mut self,
        address: String,
        name: String,
        from: String,
        to: String,
        asked_ns: i64,
    ) -> Result<(), Refusal> {
        if !tilo_bluetooth_address::valid_address(&address) {
            return Err(Refusal::new(
                "a Bluetooth address is six hex pairs, AA:BB:CC:DD:EE:FF",
            ));
        }
        if name.is_empty()
            || name.len() > super::accessory::MAX_NAME
            || name.chars().any(char::is_control)
        {
            return Err(Refusal::new("an accessory is called in 1 to 128 bytes"));
        }
        if from == to
            || !self.devices.contains(&Id::from_raw(from.clone()))
            || !self.devices.contains(&Id::from_raw(to.clone()))
        {
            return Err(Refusal::new(
                "an accessory moves between two of your devices",
            ));
        }
        if asked_ns <= 0 {
            return Err(Refusal::new("a move is asked at a time since the epoch"));
        }
        let key = Id::from_raw(address.to_ascii_uppercase());
        let asked = AccessoryMove {
            from,
            to,
            name,
            asked_ns,
            at_ns: asked_ns,
            ..AccessoryMove::default()
        };
        match self.moves.get_mut(&key) {
            Some(kept) if kept.under_way(asked_ns) => {
                return Err(Refusal::new("that accessory is moving already"));
            }
            Some(kept) => *kept = asked,
            None => {
                self.moves.push(key, asked);
            }
        }
        Ok(())
    }

    /// Ask to drive `driven`'s keyboard and pointer from `driver`, at `asked_ms`: replaces an
    /// earlier hand-over of it that no longer holds, and is left for the one that does.
    #[action]
    pub fn ask_input(
        &mut self,
        driven_device: String,
        from_device: String,
        asked_ms: i64,
    ) -> Result<(), Refusal> {
        validate_key(&driven_device)?;
        validate_key(&from_device)?;
        if driven_device == from_device
            || !self.devices.contains(&Id::from_raw(driven_device.clone()))
            || !self.devices.contains(&Id::from_raw(from_device.clone()))
        {
            return Err(Refusal::new("a device drives another of your devices"));
        }
        if asked_ms <= 0 {
            return Err(Refusal::new(
                "a hand-over is asked at a time since the epoch",
            ));
        }
        let driven_device = Id::from_raw(driven_device);
        if self
            .inputs
            .get(&driven_device)
            .is_some_and(|kept| kept.granted_ms > 0 && asked_ms < kept.until_ms)
        {
            return Ok(());
        }
        let ask = InputHand {
            driver: from_device,
            asked_ms,
            ..InputHand::default()
        };
        match self.inputs.get_mut(&driven_device) {
            Some(kept) => *kept = ask,
            None => {
                self.inputs.push(driven_device, ask);
            }
        }
        Ok(())
    }

    /// Say whether the device the accessory at `address` moves to holds its bond, for the move
    /// asked at `asked_ns`, at `at_ns`.
    #[action(housekeeping)]
    pub fn accessory_ready(
        &mut self,
        address: String,
        asked_ns: i64,
        ready: bool,
        at_ns: i64,
    ) -> Result<(), Refusal> {
        let next = if ready {
            MoveStage::Ready
        } else {
            MoveStage::NotPaired
        };
        self.step_move(&address, asked_ns, MoveStage::Asked, next, at_ns)
    }

    /// Say whether the device that had the accessory at `address` let go of it, for the move asked
    /// at `asked_ns`, at `at_ns`.
    #[action(housekeeping)]
    pub fn accessory_released(
        &mut self,
        address: String,
        asked_ns: i64,
        released: bool,
        at_ns: i64,
    ) -> Result<(), Refusal> {
        let next = if released {
            MoveStage::Released
        } else {
            MoveStage::NotReleased
        };
        self.step_move(&address, asked_ns, MoveStage::Ready, next, at_ns)?;
        if released
            && let Some(kept) = self
                .moves
                .get_mut(&Id::from_raw(address.to_ascii_uppercase()))
            && kept.asked_ns == asked_ns
            && kept.stage == MoveStage::Released
        {
            kept.released_ns = at_ns;
        }
        Ok(())
    }

    /// Say whether the device the accessory at `address` moves to connected it, for the move asked
    /// at `asked_ns`, at `at_ns`.
    #[action(housekeeping)]
    pub fn accessory_taken(
        &mut self,
        address: String,
        asked_ns: i64,
        connected: bool,
        at_ns: i64,
    ) -> Result<(), Refusal> {
        let next = if connected {
            MoveStage::Moved
        } else {
            MoveStage::NotConnected
        };
        self.step_move(&address, asked_ns, MoveStage::Released, next, at_ns)
    }

    /// Give up the move of the accessory at `address` asked at `asked_ns`, at `at_ns`, where it
    /// had not ended.
    #[action(housekeeping)]
    pub fn give_up_move(
        &mut self,
        address: String,
        asked_ns: i64,
        at_ns: i64,
    ) -> Result<(), Refusal> {
        if at_ns <= 0 {
            return Err(Refusal::new("a move is given up at a time since the epoch"));
        }
        if let Some(kept) = self
            .moves
            .get_mut(&Id::from_raw(address.to_ascii_uppercase()))
            && kept.asked_ns == asked_ns
            && !kept.stage.ended()
        {
            kept.stage = MoveStage::GivenUp;
            kept.at_ns = at_ns;
        }
        Ok(())
    }

    /// Take the move of `address` asked at `asked_ns` from `from` to `to`, where it stands at
    /// `from`: a move that went on or was asked again meanwhile is left as it is.
    fn step_move(
        &mut self,
        address: &str,
        asked_ns: i64,
        from: MoveStage,
        to: MoveStage,
        at_ns: i64,
    ) -> Result<(), Refusal> {
        if at_ns <= 0 {
            return Err(Refusal::new("a move goes on at a time since the epoch"));
        }
        if let Some(kept) = self
            .moves
            .get_mut(&Id::from_raw(address.to_ascii_uppercase()))
            && kept.asked_ns == asked_ns
            && kept.stage == from
        {
            kept.stage = to;
            kept.at_ns = at_ns;
        }
        Ok(())
    }

    /// Let `input.driver` drive `input.driven`'s keyboard, pointer or both under `input.nonce`
    /// from `input.granted_ms` until `input.until_ms`, as `input.driven` decides for the ask
    /// `input.driver` made at `input.asked_ms`.
    #[action]
    pub fn grant_input(&mut self, #[param(flatten)] input: HandOver) -> Result<(), Refusal> {
        validate_bytes(&input.nonce, 16)?;
        if !(input.keyboard || input.pointer) {
            return Err(Refusal::new(
                "a hand-over lets in the keyboard, the pointer or both",
            ));
        }
        if input.granted_ms <= 0 || input.until_ms <= input.granted_ms {
            return Err(Refusal::new(
                "a hand-over holds from when it was made until a later time",
            ));
        }
        let Some(kept) = self.inputs.get_mut(&Id::from_raw(input.driven)) else {
            return Err(Refusal::new("nobody asked to drive that device"));
        };
        if kept.driver != input.driver || kept.asked_ms != input.asked_ms {
            return Err(Refusal::new("another ask to drive that device came since"));
        }
        kept.nonce = input.nonce;
        kept.keyboard = input.keyboard;
        kept.pointer = input.pointer;
        kept.granted_ms = input.granted_ms;
        kept.until_ms = input.until_ms;
        Ok(())
    }

    /// End the hand-over of `driven` under `nonce` (empty for one only asked by `driver`): the
    /// keyboard and pointer are its own again.
    #[action]
    pub fn end_input(
        &mut self,
        driven_device: String,
        from_device: String,
        nonce: String,
    ) -> Result<(), Refusal> {
        let driven_device = Id::from_raw(driven_device);
        if self
            .inputs
            .get(&driven_device)
            .is_some_and(|kept| kept.driver == from_device && kept.nonce == nonce)
        {
            self.inputs.retain(|id, _| *id != driven_device);
        }
        Ok(())
    }

    /// Show `notice` on the person's other devices. Replaces what was kept under its id; what
    /// ran out by `notice.shown_ms` leaves the record.
    #[action(housekeeping)]
    pub fn show_notice(&mut self, #[param(flatten)] notice: NoticeShown) -> Result<(), Refusal> {
        let NoticeShown {
            id,
            from,
            app_id,
            app_name,
            title,
            body,
            redacted,
            actions,
            urgency,
            shown_ms,
            until_ms,
        } = notice;
        validate_key(&id)?;
        validate_key(&from)?;
        if app_id.is_empty()
            || app_id.len() > 160
            || !app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(Refusal::new(
                "an app is named by up to 160 letters, digits, dots, dashes and underscores",
            ));
        }
        if app_name.is_empty()
            || app_name.len() > 160
            || title.len() > 512
            || body.len() > 4_096
            || [&app_name, &title]
                .into_iter()
                .any(|value| value.chars().any(char::is_control))
            || body
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
            || (redacted && !body.is_empty())
        {
            return Err(Refusal::new(
                "a notification says its app's name, a title of up to 512 bytes and a text of up to 4096",
            ));
        }
        if actions.len() > MAX_NOTICE_ACTIONS
            || actions.iter().any(|action| {
                action.key.is_empty()
                    || action.key.len() > 128
                    || action.label.len() > 256
                    || action.key.chars().any(char::is_control)
                    || action.label.chars().any(char::is_control)
            })
        {
            return Err(Refusal::new(
                "a notification offers up to 16 actions, each a key and a label",
            ));
        }
        if !(0..=2).contains(&urgency) {
            return Err(Refusal::new(
                "a notification says something, warns or tells of an error",
            ));
        }
        if shown_ms <= 0
            || until_ms <= shown_ms
            || until_ms.saturating_sub(shown_ms) > MAX_NOTICE_SHOWN_MS
        {
            return Err(Refusal::new(
                "a notification is shown elsewhere for up to 15 minutes",
            ));
        }
        self.notices.retain(|_, kept| kept.until_ms > shown_ms);
        let id = Id::from_raw(id);
        let notice = SharedNotice {
            from,
            app_id,
            app_name,
            title,
            body,
            redacted,
            actions,
            urgency,
            shown_ms,
            until_ms,
            ..SharedNotice::default()
        };
        match self.notices.get_mut(&id) {
            Some(kept) => *kept = notice,
            None => {
                self.notices.push(id, notice);
            }
        }
        Ok(())
    }

    /// Say the outputs and microphones `device` offers to speaker groups, as the postcard bytes
    /// of its [`SpeakerEndpoint`](super::speaker::SpeakerEndpoint)s in hex: this device's own.
    #[action(housekeeping)]
    pub fn say_speaker_endpoints(
        &mut self,
        device: String,
        endpoints: String,
    ) -> Result<(), Refusal> {
        validate_key(&device)?;
        if !self.devices.contains(&Id::from_raw(device.clone())) {
            return Err(Refusal::new("that key is not one of your devices"));
        }
        let endpoints = speaker::decode_endpoints(&endpoints).map_err(Refusal::new)?;
        if endpoints.len() > speaker::MAX_ENDPOINTS {
            return Err(Refusal::new(format!(
                "a device offers at most {} outputs and microphones",
                speaker::MAX_ENDPOINTS
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for endpoint in &endpoints {
            if !speaker::valid_selector(&endpoint.selector)
                || !speaker::valid_label(&endpoint.label)
                || !seen.insert(endpoint.selector.clone())
            {
                return Err(Refusal::new(
                    "each endpoint is named once, by 16 bytes in hex and a printable label",
                ));
            }
        }
        let id = Id::from_raw(device);
        match self.speaker_devices.get_mut(&id) {
            Some(kept) => kept.endpoints = endpoints,
            None => {
                self.speaker_devices.push(
                    id,
                    SpeakerDevice {
                        endpoints,
                        ..SpeakerDevice::default()
                    },
                );
            }
        }
        Ok(())
    }

    /// Say the person answered the notification `id` on the device `device`: `chosen` the key of
    /// the action they chose, empty when they dismissed it. The first answer stands.
    #[action]
    pub fn answer_notice(
        &mut self,
        id: String,
        device: String,
        chosen: String,
    ) -> Result<(), Refusal> {
        validate_key(&device)?;
        let Some(kept) = self.notices.get_mut(&Id::from_raw(id)) else {
            return Err(Refusal::new("that notification is no longer shown"));
        };
        if !chosen.is_empty() && !kept.actions.iter().any(|action| action.key == chosen) {
            return Err(Refusal::new("that notification offers no such action"));
        }
        if kept.answered_on.is_empty() {
            kept.answered_on = device;
            kept.chosen = chosen;
        }
        Ok(())
    }

    /// Take the notification `id` off the person's devices.
    #[action(housekeeping)]
    pub fn retire_notice(&mut self, id: String) -> Result<(), Refusal> {
        let id = Id::from_raw(id);
        self.notices.retain(|kept, _| *kept != id);
        Ok(())
    }

    /// Offer the app this device has in front to your other devices.
    ///
    /// It replaces what this device offered before, and offers that ran out by the time it was
    /// made leave the record.
    #[action(housekeeping)]
    pub fn offer_continuity(&mut self, #[param(flatten)] offer: OfferShown) -> Result<(), Refusal> {
        let OfferShown {
            from,
            app_id,
            app_name,
            resource,
            view,
            shown_ms,
            until_ms,
        } = offer;
        validate_key(&from)?;
        if !self.devices.contains(&Id::from_raw(from.clone())) {
            return Err(Refusal::new("that key is not one of your devices"));
        }
        if app_id.is_empty()
            || app_id.len() > 160
            || !app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            || app_name.is_empty()
            || app_name.len() > 160
            || app_name.chars().any(char::is_control)
        {
            return Err(Refusal::new(
                "an app is named by up to 160 letters, digits, dots, dashes and underscores, and its name",
            ));
        }
        if resource.len() > 512
            || view.len() > 160
            || (resource.is_empty() && !view.is_empty())
            || [&resource, &view]
                .into_iter()
                .any(|value| value.chars().any(char::is_control))
        {
            return Err(Refusal::new(
                "an offer names the resource the app showed, in up to 512 bytes, and its view",
            ));
        }
        if shown_ms <= 0
            || until_ms <= shown_ms
            || until_ms.saturating_sub(shown_ms) > MAX_CONTINUITY_SHOWN_MS
        {
            return Err(Refusal::new("an offer is worth showing for up to an hour"));
        }
        self.continuity.retain(|_, kept| kept.until_ms > shown_ms);
        let id = Id::from_raw(from.clone());
        let offer = ContinuityOffer {
            from,
            app_id,
            app_name,
            resource,
            view,
            shown_ms,
            until_ms,
            answers: Vec::new(),
        };
        match self.continuity.get_mut(&id) {
            Some(kept) => *kept = offer,
            None => {
                self.continuity.push(id, offer);
            }
        }
        Ok(())
    }

    /// Take an app offered from another device, or put it away.
    ///
    /// The offer is the one `from` made at `at_ms`, answered on `device`: the first answer each
    /// device gives stands, and a later offer from `from` is a new one to answer.
    #[action]
    pub fn answer_continuity(
        &mut self,
        from: String,
        at_ms: i64,
        device: String,
        taken: bool,
    ) -> Result<(), Refusal> {
        validate_key(&from)?;
        validate_key(&device)?;
        if !self.devices.contains(&Id::from_raw(device.clone())) {
            return Err(Refusal::new("that key is not one of your devices"));
        }
        let Some(kept) = self.continuity.get_mut(&Id::from_raw(from)) else {
            return Err(Refusal::new("that device offers nothing now"));
        };
        if kept.shown_ms != at_ms {
            return Err(Refusal::new("that device offers something newer"));
        }
        if kept.answer_of(&device).is_none() {
            kept.answers.push(ContinuityAnswer { device, taken });
        }
        Ok(())
    }

    /// Keep the group `group` led by `device`, called `name`, with its `members` (the postcard
    /// bytes of [`SpeakerLedMember`](super::speaker::SpeakerLedMember)s in hex), written at
    /// `at_ms`: it replaces the group of that id, and only its leader writes it.
    #[action(housekeeping)]
    pub fn lead_speaker_group(
        &mut self,
        device: String,
        group: String,
        name: String,
        members: String,
        at_ms: i64,
    ) -> Result<(), Refusal> {
        validate_key(&device)?;
        if !self.devices.contains(&Id::from_raw(device.clone())) {
            return Err(Refusal::new("that key is not one of your devices"));
        }
        if at_ms <= 0 {
            return Err(Refusal::new("a group is written at a time since the epoch"));
        }
        let led = SpeakerLedGroup {
            id: group,
            name,
            members: speaker::decode_members(&members).map_err(Refusal::new)?,
            at_ms,
        };
        for member in &led.members {
            if !self.devices.contains(&Id::from_raw(member.device.clone())) {
                return Err(Refusal::new("a group names only your devices"));
            }
            let offers = self
                .speaker_devices
                .get(&Id::from_raw(member.device.clone()))
                .is_some_and(|known| speaker::offers_output(known, &member.output));
            if !offers {
                return Err(Refusal::new(
                    "a group member plays on an output that device offers",
                ));
            }
        }
        let id = Id::from_raw(device);
        if let Some(state) = self.speaker_devices.get_mut(&id) {
            return speaker::keep_group(&mut state.groups, led).map_err(Refusal::new);
        }
        let mut state = SpeakerDevice::default();
        speaker::keep_group(&mut state.groups, led).map_err(Refusal::new)?;
        self.speaker_devices.push(id, state);
        Ok(())
    }

    /// Forget the group `group` that `device` led, and every playback of it.
    #[action(housekeeping)]
    pub fn drop_speaker_group(&mut self, device: String, group: String) -> Result<(), Refusal> {
        validate_key(&device)?;
        let id = Id::from_raw(device);
        if let Some(state) = self.speaker_devices.get_mut(&id) {
            state.groups.retain(|led| led.id != group);
        }
        self.speaker_playbacks
            .retain(|_, playback| playback.group != group);
        Ok(())
    }

    /// Keep the answer a member device gives to a group its leader wrote: to play the role the
    /// leader gave it on an output, or to let the group listen with one of its microphones, as
    /// the person on it approved or declined it. It covers the group only as its leader wrote it.
    #[action(housekeeping)]
    pub fn consent_speaker_group(
        &mut self,
        #[param(flatten)] given: SpeakerConsentGiven,
    ) -> Result<(), Refusal> {
        let SpeakerConsentGiven {
            device,
            group,
            leader,
            play,
            endpoint,
            role,
            approved,
            at_ms,
        } = given;
        validate_key(&device)?;
        validate_key(&leader)?;
        if at_ms <= 0 {
            return Err(Refusal::new("a consent is given at a time since the epoch"));
        }
        let Some(led) = speaker::group(self, &group).cloned() else {
            return Err(Refusal::new("that group is not in your record"));
        };
        let led_by = self.speaker_devices.iter().any(|(id, state)| {
            state.groups.iter().any(|led| led.id == group) && id.as_str() == leader
        });
        if !led_by {
            return Err(Refusal::new("that group is not led by that device"));
        }
        if play {
            let member = led
                .member(&device)
                .ok_or_else(|| Refusal::new("that group names no output here"))?;
            if member.output != endpoint || member.role != role {
                return Err(Refusal::new(
                    "a consent covers the output and role the leader wrote",
                ));
            }
        } else {
            let offers = self
                .speaker_devices
                .get(&Id::from_raw(device.clone()))
                .is_some_and(|known| speaker::offers_microphone(known, &endpoint));
            if !offers {
                return Err(Refusal::new(
                    "a listening consent names a microphone this device offers",
                ));
            }
        }
        let consent = SpeakerConsent {
            group,
            leader,
            play,
            output: if play {
                endpoint.clone()
            } else {
                String::new()
            },
            role: if play { role } else { String::new() },
            microphone: if play { String::new() } else { endpoint },
            approved,
            at_ms,
        };
        let id = Id::from_raw(device);
        match self.speaker_devices.get_mut(&id) {
            Some(state) => {
                let same = |kept: &SpeakerConsent| {
                    kept.group == consent.group
                        && kept.leader == consent.leader
                        && kept.play == consent.play
                };
                if let Some(kept) = state.consents.iter_mut().find(|kept| same(kept)) {
                    *kept = consent;
                } else {
                    if state.consents.len() >= speaker::MAX_CONSENTS {
                        return Err(Refusal::new(format!(
                            "a device keeps at most {} consents",
                            speaker::MAX_CONSENTS
                        )));
                    }
                    state.consents.push(consent);
                }
            }
            None => {
                self.speaker_devices.push(
                    id,
                    SpeakerDevice {
                        consents: vec![consent],
                        ..SpeakerDevice::default()
                    },
                );
            }
        }
        Ok(())
    }

    /// Take back `device`'s consent to the group `group` led by `leader`: to play in it, or to let
    /// it listen with a microphone.
    #[action(housekeeping)]
    pub fn withdraw_speaker_group(
        &mut self,
        device: String,
        group: String,
        leader: String,
        play: bool,
    ) -> Result<(), Refusal> {
        validate_key(&device)?;
        let id = Id::from_raw(device.clone());
        if let Some(state) = self.speaker_devices.get_mut(&id) {
            state.consents.retain(|consent| {
                !(consent.group == group && consent.leader == leader && consent.play == play)
            });
        }
        if play {
            self.speaker_playbacks
                .retain(|_, playback| playback.group != group || playback.target != device);
        }
        Ok(())
    }

    /// Offer the group `group` led by `source` to `target` as one playback, at `at_ms`: the
    /// member admits it, with a capability bound to that exact source, or refuses it.
    #[action(housekeeping)]
    pub fn offer_speaker_playback(
        &mut self,
        route: String,
        session: String,
        source: String,
        target: String,
        group: String,
        at_ms: i64,
    ) -> Result<(), Refusal> {
        validate_bytes(&route, 16)?;
        validate_bytes(&session, 16)?;
        validate_key(&source)?;
        validate_key(&target)?;
        if !self.devices.contains(&Id::from_raw(source.clone()))
            || !self.devices.contains(&Id::from_raw(target.clone()))
        {
            return Err(Refusal::new("a playback plays between your devices"));
        }
        if at_ms <= 0 {
            return Err(Refusal::new(
                "a playback is offered at a time since the epoch",
            ));
        }
        let Some(led) = speaker::group(self, &group) else {
            return Err(Refusal::new("that group is not in your record"));
        };
        if led.member(&target).is_none() {
            return Err(Refusal::new("that group names no output there"));
        }
        if self
            .speaker_playbacks
            .get(&Id::from_raw(route.clone()))
            .is_none()
            && self.speaker_playbacks.iter().count() >= speaker::MAX_PLAYBACKS
        {
            return Err(Refusal::new(format!(
                "at most {} playbacks stand at once",
                speaker::MAX_PLAYBACKS
            )));
        }
        let playback = SpeakerPlayback {
            session,
            source,
            target,
            group,
            stage: PlaybackStage::Offered,
            authorization: String::new(),
            at_ms,
        };
        let id = Id::from_raw(route);
        match self.speaker_playbacks.get_mut(&id) {
            Some(kept) => {
                kept.stage = playback.stage;
                kept.authorization.clear();
                kept.at_ms = playback.at_ms;
            }
            None => {
                self.speaker_playbacks.push(id, playback);
            }
        }
        Ok(())
    }

    /// Admit the playback `session` with `authorization`, the capability bound to its exact
    /// source, at `at_ms`: the member's own answer to the offer.
    #[action(housekeeping)]
    pub fn admit_speaker_playback(
        &mut self,
        route: String,
        authorization: String,
        at_ms: i64,
    ) -> Result<(), Refusal> {
        validate_bytes(&authorization, 16)?;
        if authorization == hex::encode([0; 16]) {
            return Err(Refusal::new("a capability is 16 non-zero bytes in hex"));
        }
        if at_ms <= 0 {
            return Err(Refusal::new(
                "a playback is admitted at a time since the epoch",
            ));
        }
        let id = Id::from_raw(route);
        let Some(kept) = self.speaker_playbacks.get_mut(&id) else {
            return Err(Refusal::new("nobody offered that playback"));
        };
        if kept.stage == PlaybackStage::Admitted {
            return Ok(());
        }
        kept.stage = PlaybackStage::Admitted;
        kept.authorization = authorization;
        kept.at_ms = at_ms;
        Ok(())
    }

    /// Refuse the playback `session` at `at_ms`.
    #[action(housekeeping)]
    pub fn refuse_speaker_playback(&mut self, route: String, at_ms: i64) -> Result<(), Refusal> {
        if at_ms <= 0 {
            return Err(Refusal::new(
                "a playback is refused at a time since the epoch",
            ));
        }
        if let Some(kept) = self.speaker_playbacks.get_mut(&Id::from_raw(route)) {
            kept.stage = PlaybackStage::Refused;
            kept.authorization.clear();
            kept.at_ms = at_ms;
        }
        Ok(())
    }

    /// End the playback `session`: under `authorization` when the member ends what it admitted,
    /// or empty when the playing device ends it.
    #[action(housekeeping)]
    pub fn stop_speaker_playback(
        &mut self,
        route: String,
        authorization: String,
    ) -> Result<(), Refusal> {
        let id = Id::from_raw(route);
        let ends = self
            .speaker_playbacks
            .get(&id)
            .is_some_and(|kept| authorization.is_empty() || kept.authorization == authorization);
        if ends {
            self.speaker_playbacks.retain(|kept, _| *kept != id);
        }
        Ok(())
    }

    /// Keep `credential` as the passphrase of the network `network`, replacing
    /// what was kept for it.
    #[action(housekeeping)]
    pub fn keep_wifi(&mut self, network: String, credential: String) -> Result<(), Refusal> {
        let network = Id::from_raw(network);
        validate_key(network.as_str())?;
        validate_hex(&credential)?;
        match self.wifi.get_mut(&network) {
            Some(kept) => kept.credential = credential,
            None => {
                self.wifi.push(network, Network { credential });
            }
        }
        Ok(())
    }
}

/// Keep `cert`, a key the person's root certified on this device, in the record of their own
/// workspace `workspaces` holds: the one way a device, an agent's grant and a script's key alike
/// become an entry every device of theirs lists, and where a revocation their root signed waits.
///
/// # Errors
/// The certificate does not encode, this device holds no copy of the person's workspace, or the
/// record refused it.
pub async fn keep(workspaces: &super::Workspaces, cert: &DeviceCert) -> Result<(), String> {
    super::apply(workspaces, DEVICES, Devices::keeping(cert)?).await
}

/// Keep `value` as the fact `name` in the entry of `cert`'s device, in the record of its person's
/// own workspace as `workspaces` keeps it: what a device says of itself, each written by that device
/// alone.
///
/// # Errors
/// The certificate does not encode, this device keeps no copy of the person's workspace, or the
/// record refused the fact.
pub async fn keep_own_fact(
    workspaces: &super::Workspaces,
    cert: &DeviceCert,
    name: &str,
    value: &str,
) -> Result<(), String> {
    let certificate = hex::encode(postcard::to_allocvec(cert).map_err(|error| error.to_string())?);
    let workspace = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
    super::apply_on(
        workspaces,
        &workspace,
        DEVICES,
        Devices::KEEP_FACT.with((
            hex::encode(cert.device_public_key),
            certificate,
            name.to_owned(),
            value.to_owned(),
        )),
    )
    .await
}

/// Keep `entry`, the root of `person`'s revocation of one of their devices, in the record of their
/// own workspace `workspaces` holds: the one way a device becomes one every host of theirs refuses.
///
/// # Errors
/// The entry does not encode, this device holds no copy of their workspace, or the record refused
/// the entry.
pub async fn revoke(
    workspaces: &super::Workspaces,
    person: &[u8; 32],
    entry: &RevocationEntry,
) -> Result<(), String> {
    let key = hex::encode(entry.revoked_device_public_key);
    let entry = hex::encode(postcard::to_allocvec(entry).map_err(|error| error.to_string())?);
    let workspace = tilo_server_protocol::servers::person_workspace_id(person);
    super::apply_on(
        workspaces,
        &workspace,
        DEVICES,
        Devices::REVOKE_DEVICE.with((key, entry)),
    )
    .await
}

/// The record of `person`'s devices as `workspaces` holds it: empty where this device holds no
/// copy of their own workspace.
pub async fn of(workspaces: &super::Workspaces, person: &[u8; 32]) -> Devices {
    workspaces.devices_of(person).await
}

/// Mark this device, `cert`'s, in the record of the person's own workspace `workspaces` holds, as
/// one that keeps their root `root`, signed by that root: kept there first where it is not yet.
///
/// The entry that lists the device and the mark are one write of one document, so a record they
/// found is founded once, at one author's first number. A mark this device cannot sign, or the
/// record refuses, leaves the entry that lists the device all the same: the certificate their
/// root signed is what lists it, and the mark only says it keeps that root.
///
/// # Errors
/// The root did not sign, the record refused the device, or it refused the mark (and lists the
/// device anyway).
pub async fn mark_root_holder(
    workspaces: &super::Workspaces,
    root: &(dyn tilo_user_identity::StatementSigner + Sync),
    cert: &DeviceCert,
) -> Result<(), String> {
    let device = cert.device_public_key;
    let signature = match root.sign_digest(&root_holder_digest(&device)) {
        Ok(signature) => signature,
        // A root this device holds only part of signs nothing with it: the entry that lists the
        // device is written on its own then, as the certificate their root signed is what lists
        // it. That write is the first one, so the record is still founded once.
        Err(error) => {
            let listed = keep(workspaces, cert).await;
            return match listed {
                Ok(()) => Err(error.to_string()),
                Err(refused) => Err(format!("{error}; {refused}")),
            };
        }
    };
    // The entry that lists this device and the mark that it keeps the root are one write, and a
    // mark the record refuses keeps the entry all the same: the certificate their root signed is
    // what lists it, and the mark only says it keeps that root.
    super::apply_actions_on(
        workspaces,
        &super::workspace_id(workspaces),
        DEVICES,
        vec![
            Devices::keeping(cert)?,
            Devices::MARK_ROOT_HOLDER.with((hex::encode(device), hex::encode(signature))),
        ],
        Some(1),
    )
    .await
}

/// Whether the person's devices record marks `cert`'s device as one that keeps their root: what
/// decides whether a call only that root signs runs on it ([`tilo_actions::ROOT_ONLY_TAG`]).
pub async fn keeps_root(workspaces: &super::Workspaces, cert: &DeviceCert) -> bool {
    let person = cert.user_public_key;
    workspaces
        .devices_of(&person)
        .await
        .root_holders(&person)
        .contains(&cert.device_public_key)
}

/// The mark the person's devices record keeps for `cert`'s device as the one keeping their root:
/// that root's own signature over [`root_holder_digest`] of its key, which a change only the root
/// makes carries for its receivers to check ([`tilo_user_identity::root_holder::keeps_root`]).
/// None where the record marks it as no such device, or the device was revoked.
pub async fn root_mark(workspaces: &super::Workspaces, cert: &DeviceCert) -> Option<[u8; 64]> {
    let person = cert.user_public_key;
    let listed = workspaces
        .devices_of(&person)
        .await
        .listed(&person)
        .into_iter()
        .find(|listed| listed.cert.device_public_key == cert.device_public_key)?;
    if listed.device.revoked(&listed.cert) {
        return None;
    }
    let mut mark = [0; 64];
    hex::decode_to_slice(listed.device.keeps_root.as_deref()?, &mut mark).ok()?;
    tilo_user_identity::root_holder::keeps_root(cert, &mark).then_some(mark)
}

/// Whether `signature` (hex) is the root `cert` names signing [`root_holder_digest`] of the key
/// `cert` certifies.
fn root_signed(cert: &DeviceCert, signature: &str) -> bool {
    let mut bytes = [0; 64];
    hex::decode_to_slice(signature, &mut bytes).is_ok()
        && tilo_user_identity::root_holder::keeps_root(cert, &bytes)
}

/// What the certificate `certificate` (postcard in hex) certifies: a device when it does not read.
fn kind_of(certificate: &str) -> KeyKind {
    Device {
        certificate: certificate.to_owned(),
        ..Device::default()
    }
    .cert()
    .map_or(KeyKind::Device, |cert| KeyKind::of(&cert))
}

/// `key` as 32 bytes in lowercase hex.
fn validate_key(key: &str) -> Result<(), Refusal> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(key, &mut bytes)
        .ok()
        .filter(|()| key == hex::encode(bytes))
        .ok_or_else(|| Refusal::new("a device or a network is named by 32 bytes in hex"))
}

/// `value` as `len` bytes in lowercase hex.
fn validate_bytes(value: &str, len: usize) -> Result<(), Refusal> {
    hex::decode(value)
        .ok()
        .filter(|bytes| bytes.len() == len && hex::encode(bytes) == value)
        .map(drop)
        .ok_or_else(|| Refusal::new("a boot's ids are bytes in lowercase hex"))
}

/// `value` as bytes in lowercase hex, no longer than [`MAX_VALUE`].
fn validate_hex(value: &str) -> Result<(), Refusal> {
    if value.len() > MAX_VALUE
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(Refusal::new("a device's value is postcard bytes in hex"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_says_its_session_and_only_a_kept_one_does() {
        let device = [7; 32];
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        let said = OwnerSession {
            locked: true,
            changed_ms: 1_700_000_000_000,
            active_ms: 1_699_999_990_000,
            using: false,
        };
        assert!(
            devices
                .apply(Devices::saying_session(&device, said))
                .is_err(),
            "a device the record does not keep says nothing"
        );
        devices
            .apply(Devices::KEEP_DEVICE.with((hex::encode(device), "00".to_owned())))
            .expect("the device is kept");
        devices
            .apply(Devices::saying_session(&device, said))
            .expect("the device says its session");
        let state = devices.state().expect("the record reads");
        let kept = state
            .devices
            .get(&Id::from_raw(hex::encode(device)))
            .expect("the device's entry");
        assert_eq!(kept.session, said);
        assert!(
            devices
                .apply(Devices::saying_session(
                    &device,
                    OwnerSession {
                        changed_ms: -1,
                        ..said
                    }
                ))
                .is_err(),
            "a time before the epoch is refused"
        );
    }

    /// An answer to a boot is kept until it no longer holds: a later answer to the same boot
    /// takes its place, one made after it ran out sees it go, and a device the record does not
    /// keep answers nothing.
    #[test]
    fn an_answer_to_a_boot_holds_until_it_runs_out() {
        let device = hex::encode([7; 32]);
        let allowed = UnlockAnswer {
            device: device.clone(),
            enrolment: hex::encode([1; 32]),
            boot: hex::encode([2; 16]),
            slot: format!("{}:1", hex::encode([3; 16])),
            allowed: true,
            at_ns: 1_000,
            until_ns: 2_000,
        };
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        assert!(
            devices.apply(Devices::answering(&allowed)).is_err(),
            "a device the record does not keep answers nothing"
        );
        devices
            .apply(Devices::KEEP_DEVICE.with((device, "00".to_owned())))
            .expect("the device is kept");
        devices
            .apply(Devices::answering(&allowed))
            .expect("the person let the boot unlock");
        let refused = UnlockAnswer {
            allowed: false,
            at_ns: 1_500,
            until_ns: 2_500,
            ..allowed.clone()
        };
        devices
            .apply(Devices::answering(&refused))
            .expect("then refused the same boot");
        let other = UnlockAnswer {
            boot: hex::encode([4; 16]),
            at_ns: 2_600,
            until_ns: 3_000,
            ..allowed.clone()
        };
        let answers = |devices: &tilo_resource::Replica<Devices, AppNamed>| {
            devices
                .state()
                .expect("the record reads")
                .unlocks
                .values()
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(answers(&devices), [refused], "the later answer holds");
        devices
            .apply(Devices::answering(&other))
            .expect("another boot is answered once that ran out");
        assert_eq!(answers(&devices), [other], "an answer that ran out goes");
        for slot in ["volume:1", &format!("{}:01", hex::encode([3; 16]))] {
            let named = UnlockAnswer {
                slot: slot.to_owned(),
                ..allowed.clone()
            };
            assert!(
                devices.apply(Devices::answering(&named)).is_err(),
                "{slot} names no disk"
            );
        }
    }

    /// The person's keyboard and pointer of one device are handed to another in the record: asked
    /// by the device that would drive, let in by the driven one for the ask it answers, and ended
    /// by either; an ask while a hand-over holds leaves it as it is.
    #[test]
    fn a_device_is_driven_by_another_only_once_it_let_it_in() {
        let (laptop, phone) = ([1; 32], [2; 32]);
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        assert!(
            devices
                .apply(Devices::asking_input(&phone, &laptop, 10))
                .is_err(),
            "a device the record does not keep drives nothing"
        );
        for device in [laptop, phone] {
            devices
                .apply(Devices::KEEP_DEVICE.with((hex::encode(device), "00".to_owned())))
                .expect("the device is kept");
        }
        assert!(
            devices
                .apply(Devices::asking_input(&laptop, &laptop, 10))
                .is_err(),
            "a device does not drive itself"
        );
        devices
            .apply(Devices::asking_input(&phone, &laptop, 10))
            .expect("the laptop asks to drive the phone");
        let hand = |state: &Devices| state.inputs.get(&Id::from_raw(hex::encode(phone))).cloned();
        let asked = hand(&devices.state().unwrap()).expect("the ask is kept");
        assert!(
            !asked.holds(&hex::encode(laptop), 11),
            "an ask drives nothing yet"
        );

        let granted = InputHand {
            nonce: hex::encode([9; 16]),
            keyboard: true,
            pointer: true,
            granted_ms: 12,
            until_ms: 1_000,
            ..asked
        };
        assert!(
            devices
                .apply(Devices::granting_input(
                    &phone,
                    &InputHand {
                        asked_ms: 9,
                        ..granted.clone()
                    }
                ))
                .is_err(),
            "a grant answers the ask it was made for"
        );
        devices
            .apply(Devices::granting_input(&phone, &granted))
            .expect("the phone lets the laptop in");
        assert!(
            hand(&devices.state().unwrap())
                .unwrap()
                .holds(&hex::encode(laptop), 500)
        );
        assert!(
            !hand(&devices.state().unwrap())
                .unwrap()
                .holds(&hex::encode(laptop), 1_000)
        );

        // Another ask while it holds leaves it.
        devices
            .apply(Devices::asking_input(&phone, &laptop, 20))
            .expect("asked again");
        assert_eq!(hand(&devices.state().unwrap()), Some(granted.clone()));

        devices
            .apply(Devices::ending_input(&phone, &laptop, &granted.nonce))
            .expect("the hand-over ends");
        assert_eq!(
            hand(&devices.state().unwrap()),
            None,
            "the phone's input is its own again"
        );
    }

    /// A group one device leads, the member's consent and one playback stand in the record, and
    /// each is refused where it names what the record does not hold.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario keeps the group, its consent and its playback visible together"
    )]
    fn a_speaker_group_its_consent_and_playback_are_in_the_record() {
        let (office, shelf) = ([1; 32], [2; 32]);
        let (office_key, shelf_key) = (hex::encode(office), hex::encode(shelf));
        let group = hex::encode([5; 16]);
        let (office_output, shelf_output) = (hex::encode([8; 16]), hex::encode([9; 16]));
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        for key in [&office_key, &shelf_key] {
            devices
                .apply(Devices::KEEP_DEVICE.with((key.clone(), "00".to_owned())))
                .expect("the device is kept");
        }
        let endpoint = |selector: &str, microphone: bool| super::speaker::SpeakerEndpoint {
            selector: selector.to_owned(),
            microphone,
            label: "Speakers".into(),
            is_default: true,
        };
        for (key, output) in [(&office_key, &office_output), (&shelf_key, &shelf_output)] {
            let endpoints = vec![
                endpoint(output, false),
                endpoint(&hex::encode([7; 16]), true),
            ];
            devices
                .apply(Devices::SAY_SPEAKER_ENDPOINTS.with((
                    key.clone(),
                    hex::encode(postcard::to_allocvec(&endpoints).unwrap()),
                )))
                .expect("the device offers its endpoints");
        }
        let member = |device: &str, output: &str, role: &str| super::speaker::SpeakerLedMember {
            device: device.to_owned(),
            output: output.to_owned(),
            role: role.to_owned(),
            ..super::speaker::SpeakerLedMember::default()
        };
        let members = vec![
            member(&office_key, &office_output, "left"),
            member(&shelf_key, &shelf_output, "right"),
        ];
        assert!(
            devices
                .apply(Devices::LEAD_SPEAKER_GROUP.with((
                    office_key.clone(),
                    group.clone(),
                    "Living room".to_owned(),
                    hex::encode(postcard::to_allocvec(&members).unwrap()),
                    1,
                )))
                .is_ok(),
            "the office leads the pair"
        );
        let state = devices.state().unwrap();
        assert_eq!(
            super::speaker::group(&state, &group).map(|led| led.members.len()),
            Some(2)
        );
        assert!(
            state
                .speaker_devices
                .get(&Id::from_raw(shelf_key.clone()))
                .unwrap()
                .consents
                .is_empty(),
            "the shelf has not agreed yet"
        );

        // A consent for the shelf's own role is kept; one for another role is refused.
        let consent = |role: &str| {
            Devices::CONSENT_SPEAKER_GROUP.with(SpeakerConsentGiven {
                device: shelf_key.clone(),
                group: group.clone(),
                leader: office_key.clone(),
                play: true,
                endpoint: shelf_output.clone(),
                role: role.to_owned(),
                approved: true,
                at_ms: 2,
            })
        };
        assert!(
            devices.apply(consent("left")).is_err(),
            "a consent covers the written role"
        );
        devices.apply(consent("right")).expect("the shelf agrees");

        // One playback, offered to the shelf under its route, is admitted with its capability
        // bound to the office, and ended from either end.
        let (session, route, authorization) = (
            hex::encode([3; 16]),
            hex::encode([4; 16]),
            hex::encode([6; 16]),
        );
        assert!(
            devices
                .apply(Devices::ADMIT_SPEAKER_PLAYBACK.with((
                    route.clone(),
                    authorization.clone(),
                    2,
                )))
                .is_err(),
            "a playback nobody offered is not admitted"
        );
        devices
            .apply(Devices::OFFER_SPEAKER_PLAYBACK.with((
                route.clone(),
                session.clone(),
                office_key.clone(),
                shelf_key.clone(),
                group.clone(),
                3,
            )))
            .expect("the office offers it");
        let offered = |devices: &tilo_resource::Replica<Devices, AppNamed>| {
            devices
                .state()
                .unwrap()
                .speaker_playbacks
                .get(&Id::from_raw(route.clone()))
                .cloned()
        };
        let kept = offered(&devices).expect("the playback is kept");
        assert_eq!(kept.session, session);
        assert_eq!(kept.stage, PlaybackStage::Offered);
        devices
            .apply(Devices::ADMIT_SPEAKER_PLAYBACK.with((route.clone(), authorization.clone(), 4)))
            .expect("the shelf admits it");
        let kept = offered(&devices).expect("the playback is still kept");
        assert_eq!(kept.stage, PlaybackStage::Admitted);
        assert_eq!(kept.authorization, authorization);
        devices
            .apply(Devices::STOP_SPEAKER_PLAYBACK.with((route.clone(), String::new())))
            .expect("the office ends it");
        assert!(offered(&devices).is_none(), "an ended playback is let go");
    }

    /// What one device has in front is offered to the person's others there, taken or put away
    /// once on each of them, and a newer offer is a new one to answer.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario keeps the refusals, the answer and the newer offer visible together"
    )]
    fn an_app_in_front_is_offered_to_the_other_devices_once() {
        let (laptop, phone, other) = ([1; 32], [2; 32], [9; 32]);
        let (laptop_key, phone_key) = (hex::encode(laptop), hex::encode(phone));
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        let offer = |shown_ms: i64| ContinuityOffer {
            from: laptop_key.clone(),
            app_id: "so.tilo.notes".into(),
            app_name: "Notes".into(),
            resource: "notes:groceries".into(),
            view: String::new(),
            shown_ms,
            until_ms: shown_ms + 60_000,
            answers: Vec::new(),
        };
        assert!(
            devices
                .apply(Devices::offering_continuity(&offer(1_000)))
                .is_err(),
            "a device the record does not keep offers nothing"
        );
        for key in [&laptop_key, &phone_key] {
            devices
                .apply(Devices::KEEP_DEVICE.with((key.clone(), "00".to_owned())))
                .expect("the device is kept");
        }
        for refused in [
            ContinuityOffer {
                app_id: String::new(),
                ..offer(1_000)
            },
            ContinuityOffer {
                resource: String::new(),
                view: "a view".into(),
                ..offer(1_000)
            },
            ContinuityOffer {
                until_ms: 1_000 + MAX_CONTINUITY_SHOWN_MS + 1,
                ..offer(1_000)
            },
        ] {
            assert!(
                devices
                    .apply(Devices::offering_continuity(&refused))
                    .is_err(),
                "{refused:?} is no offer a device makes"
            );
        }
        devices
            .apply(Devices::offering_continuity(&offer(1_000)))
            .expect("the laptop offers the note");
        let kept = devices
            .state()
            .unwrap()
            .continuity
            .get(&Id::from_raw(laptop_key.clone()))
            .cloned()
            .expect("the offer is kept");
        assert_eq!(
            kept.answer_of(&phone_key),
            None,
            "the phone has not answered"
        );

        // The phone puts it away, and the first answer it gave stands.
        devices
            .apply(Devices::answering_continuity(
                &laptop_key,
                1_000,
                &phone,
                false,
            ))
            .expect("the phone puts it away");
        devices
            .apply(Devices::answering_continuity(
                &laptop_key,
                1_000,
                &phone,
                true,
            ))
            .expect("a second answer changes nothing");
        let kept = devices
            .state()
            .unwrap()
            .continuity
            .get(&Id::from_raw(laptop_key.clone()))
            .cloned()
            .expect("the offer is kept");
        assert_eq!(kept.answer_of(&phone_key), Some(false));
        assert!(
            devices
                .apply(Devices::answering_continuity(
                    &laptop_key,
                    2_000,
                    &phone,
                    true
                ))
                .is_err(),
            "an answer names the offer it was made for"
        );
        assert!(
            devices
                .apply(Devices::answering_continuity(
                    &laptop_key,
                    1_000,
                    &other,
                    true
                ))
                .is_err(),
            "a device the record does not keep answers nothing"
        );

        // A newer offer from the laptop carries no answers, so the phone is offered it again; the
        // one it put away ran out of the record.
        devices
            .apply(Devices::offering_continuity(&offer(2_000)))
            .expect("the laptop offers the note again");
        let state = devices.state().unwrap();
        let kept = state
            .continuity
            .get(&Id::from_raw(laptop_key.clone()))
            .cloned()
            .expect("the offer is kept");
        assert_eq!(kept.shown_ms, 2_000);
        assert_eq!(kept.answer_of(&phone_key), None, "it is offered afresh");
        assert_eq!(state.continuity.len(), 1, "one offer per device");

        // An offer nobody looks at runs out of the record as the next one lands.
        devices
            .apply(Devices::offering_continuity(&offer(100_000)))
            .expect("the laptop offers it later");
        assert!(
            devices
                .state()
                .unwrap()
                .continuity
                .get(&Id::from_raw(laptop_key.clone()))
                .is_some_and(|kept| kept.shown_at(120_000)),
            "an offer its hour has not run out shows"
        );
        devices
            .apply(Devices::offering_continuity(&offer(400_000)))
            .expect("the laptop offers it much later");
        let state = devices.state().unwrap();
        assert_eq!(state.continuity.len(), 1, "an offer that ran out leaves it");
        assert!(
            state
                .continuity
                .get(&Id::from_raw(laptop_key))
                .is_some_and(|kept| kept.shown_ms == 400_000),
            "the newer offer is the one the record keeps"
        );
    }

    #[test]
    fn a_notification_is_answered_once_and_runs_out_of_the_record() {
        let (desktop, laptop, phone) = ([1; 32], [2; 32], [3; 32]);
        let (mail, later) = (hex::encode([7; 32]), hex::encode([8; 32]));
        let notice = SharedNotice {
            from: hex::encode(desktop),
            app_id: "so.tilo.mail".into(),
            app_name: "Mail".into(),
            title: "New mail".into(),
            body: "From Ada".into(),
            actions: vec![NoticeAction {
                key: "archive".into(),
                label: "Archive".into(),
            }],
            shown_ms: 1_000,
            until_ms: 61_000,
            ..SharedNotice::default()
        };
        let mut devices = tilo_resource::Replica::<Devices>::new().in_app("so.tilo.model", "Model");
        for refused in [
            SharedNotice {
                body: "From Ada".into(),
                redacted: true,
                ..notice.clone()
            },
            SharedNotice {
                until_ms: 1_000 + MAX_NOTICE_SHOWN_MS + 1,
                ..notice.clone()
            },
            SharedNotice {
                title: "New\u{7}mail".into(),
                ..notice.clone()
            },
        ] {
            assert!(
                devices
                    .apply(Devices::showing_notice(&mail, &refused))
                    .is_err(),
                "{refused:?} is no notification the record shows"
            );
        }
        devices
            .apply(Devices::showing_notice(&mail, &notice))
            .expect("the desktop shows its mail on the person's other devices");
        let kept = |devices: &tilo_resource::Replica<Devices, AppNamed>, id: &str| {
            devices
                .state()
                .unwrap()
                .notices
                .get(&Id::from_raw(id))
                .cloned()
        };
        assert!(kept(&devices, &mail).unwrap().shown_at(2_000));

        assert!(
            devices
                .apply(Devices::answering_notice(&mail, &laptop, "wipe"))
                .is_err(),
            "an action the app never offered is not chosen"
        );
        devices
            .apply(Devices::answering_notice(&mail, &laptop, "archive"))
            .expect("the person archives it on the laptop");
        devices
            .apply(Devices::answering_notice(&mail, &phone, ""))
            .expect("a later answer is heard");
        let answered = kept(&devices, &mail).unwrap();
        assert_eq!(
            (answered.answered_on.as_str(), answered.chosen.as_str()),
            (hex::encode(laptop).as_str(), "archive"),
            "the first answer stands"
        );
        assert!(
            !answered.shown_at(2_000),
            "an answered notification is shown nowhere"
        );

        // What ran out leaves the record as the next one is shown.
        devices
            .apply(Devices::showing_notice(
                &later,
                &SharedNotice {
                    shown_ms: 61_000,
                    until_ms: 70_000,
                    ..notice
                },
            ))
            .expect("a later mail is shown");
        assert_eq!(kept(&devices, &mail), None, "the first ran out");
        devices
            .apply(Devices::retiring_notice(&later))
            .expect("the desktop takes it away");
        assert!(devices.state().unwrap().notices.is_empty());
    }
}
