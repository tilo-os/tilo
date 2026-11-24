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
//! What the person's root signs lives in their record: the mark of the device
//! that keeps it ([`Devices::mark_root_holder`]) and the revocation of one it
//! takes back ([`Devices::revoke_device`]), each a signature a reader checks
//! against that root where it reads the record. A revocation is kept once, in
//! the entry of the device it names, and an entry anything else signed is no
//! revocation and every reader leaves it out.

use tilo_resource::{Collection, Id, Model, Refusal, resource};
use tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;
use tilo_user_identity::{DeviceCert, RevocationEntry, revocation_of, revokes};

/// The resource of the person's devices in their own workspace.
pub const DEVICES: (&str, &str) = ("so.tilo.devices", "devices");

/// The most bytes one fact or passphrase of a device takes, hex.
const MAX_VALUE: usize = 256 * 1024;
/// The longest name of a fact.
const MAX_NAME: usize = 32;
/// The longest name of the agent a device runs.
const MAX_AGENT: usize = 64;

/// What the person's root signs to mark the device that keeps it, beside that device's key.
const ROOT_HOLDER_DOMAIN: &[u8] = b"tilo-root-holder-v1\0";

/// The digest the person's root signs to mark `device` as the one that keeps it.
#[must_use]
pub fn root_holder_digest(device: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ROOT_HOLDER_DOMAIN);
    hasher.update(device);
    *hasher.finalize().as_bytes()
}

/// The fact under which a device's entry keeps when a session of it was last
/// with a host, ms since the epoch, little-endian.
pub const SEEN_FACT: &str = "seen";

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
}

impl KeyKind {
    /// What `cert` certifies.
    #[must_use]
    pub fn of(cert: &DeviceCert) -> Self {
        if cert.capabilities.server_key {
            Self::Script
        } else if cert.capabilities.agent.is_some() {
            Self::Agent
        } else {
            Self::Device
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
        self.unlocks
            .retain(|id, kept| *id != key && kept.until_ns > answer.at_ns);
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
            .filter_map(|listed| listed.device.revocation(&listed.cert))
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
/// # Errors
/// The root did not sign, or the record refused the device or the mark.
pub async fn mark_root_holder(
    workspaces: &super::Workspaces,
    root: &(dyn tilo_user_identity::StatementSigner + Sync),
    cert: &DeviceCert,
) -> Result<(), String> {
    let device = cert.device_public_key;
    let signature = root
        .sign_digest(&root_holder_digest(&device))
        .map_err(|error| error.to_string())?;
    keep(workspaces, cert).await?;
    super::apply(
        workspaces,
        DEVICES,
        Devices::MARK_ROOT_HOLDER.with((hex::encode(device), hex::encode(signature))),
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

/// Whether `signature` (hex) is the root `cert` names signing [`root_holder_digest`] of the key
/// `cert` certifies.
fn root_signed(cert: &DeviceCert, signature: &str) -> bool {
    let mut bytes = [0; 64];
    if hex::decode_to_slice(signature, &mut bytes).is_err() {
        return false;
    }
    ed25519_dalek::VerifyingKey::from_bytes(&cert.user_public_key).is_ok_and(|root| {
        root.verify_strict(
            &root_holder_digest(&cert.device_public_key),
            &ed25519_dalek::Signature::from_bytes(&bytes),
        )
        .is_ok()
    })
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
        let mut devices = tilo_resource::Replica::<Devices>::new();
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
        let mut devices = tilo_resource::Replica::<Devices>::new();
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
        let answers = |devices: &tilo_resource::Replica<Devices>| {
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
}
