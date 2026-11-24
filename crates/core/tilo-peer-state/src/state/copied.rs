//! Device keys running on two machines, as this copy of a workspace found them.
//!
//! A device numbers the changes it signs from one count, so two different changes under one
//! key and one sequence number are the key on two machines: a disk copied from another, or two
//! processes signing as one device. No receiver can tell such machines apart, and a change of
//! one is refused against the other's at every receiver, so their links stop converging. This
//! copy refuses the key instead, by name ([`CopiedDevice`]), and takes it again once the person
//! re-added one of the machines with a ticket: a device of theirs certified after the clash.

use web_time::SystemTime;

use neighbour_wire::NeighbourMsg;

use super::StatePeer;
use crate::{AdmissionError, CopiedDevice};

/// A key this copy found on two machines, and when.
#[derive(Clone, Debug)]
pub(super) struct Copied {
    device: CopiedDevice,
    /// The key's person, when this copy has the key's certificate.
    person: Option<[u8; 32]>,
    /// When this copy found it, in nanoseconds since the Unix epoch.
    found_ns: u64,
}

/// The signing key of the change `message` carries, when it carries one.
pub(super) fn changed_by(message: &NeighbourMsg) -> Option<[u8; 32]> {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => Some(*envelope.author.as_bytes()),
        _ => None,
    }
}

impl StatePeer {
    /// The device keys this copy refuses as running on two machines.
    #[must_use]
    pub fn copied_devices(&self) -> Vec<CopiedDevice> {
        self.copied
            .values()
            .map(|copied| copied.device.clone())
            .collect()
    }

    /// The refusal of `key`'s changes, while this copy finds it on two machines: until a device
    /// of its person's is certified after the clash was found, the machine the person re-added.
    pub(super) fn refused_as_copied(&mut self, key: [u8; 32]) -> Option<AdmissionError> {
        let copied = self.copied.get(&key)?;
        let readded = copied.person.is_some_and(|person| {
            self.core.certificates.values().any(|cert| {
                cert.user_public_key == person
                    && cert.device_public_key != key
                    && cert.issued_at_ns > copied.found_ns
            })
        });
        if readded {
            tracing::info!(
                device = %hex::encode(key),
                "a device found on two machines was re-added; its key is taken again"
            );
            self.copied.remove(&key);
            return None;
        }
        Some(AdmissionError::CopiedDevice(copied.device.clone()))
    }

    /// `key` signed a change under a sequence number another change of its took here: the key
    /// runs on two machines. Its changes are refused from now on.
    pub(super) fn found_copied(&mut self, key: [u8; 32], now: SystemTime) -> AdmissionError {
        let cert = self.core.certificates.get(&key);
        let name = cert
            .map(|cert| cert.device_label.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                let short = hex::encode(key);
                format!("The device {}", short.get(..8).unwrap_or(&short))
            });
        let copied = Copied {
            device: CopiedDevice { device: key, name },
            person: cert.map(|cert| cert.user_public_key),
            found_ns: now
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
                }),
        };
        tracing::warn!(
            device = %hex::encode(key),
            workspace = %self.core.workspace,
            "{}",
            copied.device
        );
        let refused = AdmissionError::CopiedDevice(copied.device.clone());
        self.copied.insert(key, copied);
        refused
    }
}
