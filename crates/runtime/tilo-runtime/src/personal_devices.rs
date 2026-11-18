//! The person's devices as this device's own store holds them, for the action APIs
//! that admit another of their devices: the MCP server between devices and a
//! key's sessions with this device's host.

use std::time::SystemTime;

use tilo_device_mcp::{PersonsDevices, Refusal};
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_user_identity::{DeviceCert, RevocationList};

/// The person's devices as their own store holds them: the identity root it
/// opened on, the certificates it admitted, and the revocations that root
/// signed, which reach it from any of their devices.
#[derive(Clone)]
pub struct PersonalDevices(PersonalSync);

impl PersonalDevices {
    /// The devices `store` holds.
    #[must_use]
    pub fn new(store: PersonalSync) -> Self {
        Self(store)
    }
}

fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        })
}

impl PersonsDevices for PersonalDevices {
    fn this_device(&self) -> DeviceCert {
        self.0.read(|store| store.device().clone())
    }

    fn devices(&self) -> Vec<DeviceCert> {
        let now = SystemTime::now();
        self.0.read(|store| {
            store
                .devices()
                .into_iter()
                .filter(|device| {
                    holds(store.root(), device, now).is_ok()
                        && store.admits_device(&device.device_public_key, now).is_ok()
                })
                .collect()
        })
    }

    fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal> {
        if certificate.device_public_key != dialer {
            return Err(Refusal::NotTheDialer);
        }
        let now = SystemTime::now();
        let revoked = self.revocations().refuses(certificate);
        self.0.read(|store| {
            holds(store.root(), certificate, now)?;
            // A grant a revoked grant minted ends with it.
            if revoked {
                return Err(Refusal::Revoked);
            }
            store
                .admits_device(&dialer, now)
                .map_err(|why| match why.as_str() {
                    "device revoked" => Refusal::Revoked,
                    _ => Refusal::NotCertified(why),
                })
        })
    }

    fn revocations(&self) -> RevocationList {
        let entries = self.0.read(PersonalStore::revocations);
        let mut revocations = RevocationList::new();
        for entry in entries {
            // The store only ever holds entries its root signed.
            let _ = revocations.insert(entry);
        }
        revocations
    }
}

/// Whether `certificate` is one of `root`'s devices that acts for them now:
/// signed by it and current.
fn holds(root: [u8; 32], certificate: &DeviceCert, now: SystemTime) -> Result<(), Refusal> {
    if certificate.user_public_key != root {
        return Err(Refusal::OtherPerson);
    }
    tilo_user_identity::verify_device_cert_at(certificate, now_ns(now))
        .map_err(|error| Refusal::NotCertified(error.to_string()))
}
