//! The person's devices as their own workspace keeps them: what each says of
//! itself that lasts, under its key, and the Wi-Fi passphrases they share.
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

/// What one device says of itself that lasts.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Device {
    /// Its certificate, postcard in hex, which the person's root signed: a
    /// reader shows only a device whose certificate holds.
    pub certificate: String,
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
    /// The action that keeps `cert`, which the person's root signed, among their devices.
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
        match self.devices.get_mut(&device) {
            Some(kept) => kept.certificate = certificate,
            None => {
                self.devices.push(
                    device,
                    Device {
                        certificate,
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
