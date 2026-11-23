//! The person's devices as their own workspace keeps them: what each says of
//! itself that lasts, under its key, and the Wi-Fi passphrases they share.
//!
//! Each device writes its own facts (what it is, its speaker groups, its disk
//! unlock), each the postcard bytes of its type in hex under its name, and a
//! Wi-Fi passphrase one of them learned is kept for all of them, sealed to the
//! devices that may open it. The workspace carries both to every device of the
//! person, a device that comes up later included.

use tilo_resource::{Collection, Id, Model, Refusal, resource};

/// The resource of the person's devices in their own workspace.
pub const DEVICES: (&str, &str) = ("so.tilo.devices", "devices");

/// The most bytes one fact or passphrase of a device takes, hex.
const MAX_VALUE: usize = 256 * 1024;
/// The longest name of a fact.
const MAX_NAME: usize = 32;

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
    /// Each fact under its name, postcard in hex.
    pub facts: std::collections::BTreeMap<String, String>,
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
                        facts: std::collections::BTreeMap::new(),
                    },
                );
            }
        }
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
