//! The person's devices as their own workspace keeps them: what each says of
//! itself that lasts, under its key, and the Wi-Fi passphrases they share.
//!
//! Every key the person's root certifies is an entry: a device, an agent's
//! grant, a script's key, each marked with what it is ([`KeyKind`]). The record
//! answers who a key is; what it may do stays in each workspace's grants.
//!
//! Each entry holds the device's certificate, the name the person gave it, the
//! agent it runs and whether their root took it back: what every list of their
//! devices reads, on any device of theirs, with whether it is online now from
//! presence beside it. Each device writes its own facts (what it is, its
//! speaker groups, its disk unlock, when it was last with a host), each the
//! postcard bytes of its type in hex under its name, and a Wi-Fi passphrase one
//! of them learned is kept for all of them, sealed to the devices that may open
//! it. The workspace carries all of it to every device of the person, a device
//! that comes up later included.
//!
//! The device that keeps the person's root is marked so, signed by that root as
//! a revocation is ([`Devices::mark_root_holder`]): what the person's other
//! devices, which keep none, are capped by, and where they leave what only the
//! root signs to be answered.

use tilo_resource::{Collection, Id, Model, Refusal, resource};
use tilo_user_identity::DeviceCert;
use tilo_user_identity::certification::MAX_CERTIFICATION_LABEL_BYTES;

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

/// The person's devices, by device key (lowercase hex), and their Wi-Fi.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Devices {
    pub devices: Collection<Device>,
    /// The passphrases, by network id (lowercase hex).
    pub wifi: Collection<Network>,
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
    /// The person's root took it back ([`Devices::revoke_device`]): it stays
    /// listed, marked, and every device of theirs refuses it.
    pub revoked: bool,
    /// It keeps the person's root: their root's signature over
    /// [`root_holder_digest`] of its key, hex ([`Devices::mark_root_holder`]).
    pub keeps_root: Option<String>,
    /// Each fact under its name, postcard in hex.
    pub facts: std::collections::BTreeMap<String, String>,
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

    /// The devices of `person`'s that keep their root, as the root marked them: not one taken
    /// back.
    #[must_use]
    pub fn root_holders(&self, person: &[u8; 32]) -> Vec<[u8; 32]> {
        self.listed(person)
            .into_iter()
            .filter(|listed| !listed.device.revoked && listed.device.keeps_root(&listed.cert))
            .map(|listed| listed.cert.device_public_key)
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
            .filter(|(id, other)| id.as_str() != device && !other.revoked)
            .filter_map(|(_, other)| other.cert().map(|cert| other.called(&cert)))
            .any(|label| label == name);
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

    /// Mark `device` taken back: the person's root revoked it, and every host
    /// that holds this record refuses it from the moment the mark arrives.
    #[action]
    pub fn revoke_device(&mut self, device: String) -> Result<(), Refusal> {
        let kept = self
            .devices
            .get_mut(&Id::from_raw(device))
            .ok_or_else(|| Refusal::new("that key is not one of your devices"))?;
        kept.revoked = true;
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
/// become an entry every device of theirs lists, and revokes by its mark.
///
/// # Errors
/// The certificate does not encode, this device holds no copy of the person's workspace, or the
/// record refused it.
pub async fn keep(workspaces: &super::Workspaces, cert: &DeviceCert) -> Result<(), String> {
    super::apply(workspaces, DEVICES, Devices::keeping(cert)?).await
}

/// Mark this device, `device`, in the record of the person's own workspace `workspaces` holds, as
/// the one that keeps their root `root`, signed by that root.
///
/// # Errors
/// The root did not sign, or the record refused the mark.
pub async fn mark_root_holder(
    workspaces: &super::Workspaces,
    root: &dyn tilo_user_identity::StatementSigner,
    device: &[u8; 32],
) -> Result<(), String> {
    let signature = root
        .sign_digest(&root_holder_digest(device))
        .map_err(|error| error.to_string())?;
    super::apply(
        workspaces,
        DEVICES,
        Devices::MARK_ROOT_HOLDER.with((hex::encode(device), hex::encode(signature))),
    )
    .await
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
