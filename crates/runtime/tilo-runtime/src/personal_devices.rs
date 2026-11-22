//! The person's devices as this device's own store holds them, for a key's
//! sessions with this device's host, which admit another of their devices.

use std::time::SystemTime;

use neighbour_wire::RefusalReason;
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_user_identity::{DeviceCert, RevocationList};

/// Why a device is not taken as one of the person's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The certificate it showed names another device than the one that
    /// connected.
    NotTheDialer,
    /// The device is someone else's.
    OtherPerson,
    /// Its certificate does not hold now.
    NotCertified(String),
    /// The person removed it from their devices.
    Revoked,
    /// A key made for an agent asked for a workspace its scope does not
    /// reach, or named none.
    OutOfScope,
}

impl Refusal {
    /// The reason the dialing side is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotTheDialer => {
                "the certificate shown names another device than the one that connected".to_owned()
            }
            Self::OtherPerson => {
                "that device belongs to someone else; this server serves only its person's own \
                 devices"
                    .to_owned()
            }
            Self::NotCertified(why) => format!("that device's certificate does not hold: {why}"),
            Self::Revoked => {
                "that device was removed from its person's devices; it no longer reaches them"
                    .to_owned()
            }
            Self::OutOfScope => {
                "this key reaches only the workspaces it was made for: name one of them".to_owned()
            }
        }
    }
}

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

impl PersonalDevices {
    /// Whether `certificate`, shown on a connection the transport proved is
    /// from `dialer`, is one of the person's devices now.
    pub fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal> {
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
                .map_err(|why| match why.reason() {
                    RefusalReason::Revoked => Refusal::Revoked,
                    _ => Refusal::NotCertified(why.to_string()),
                })
        })
    }

    /// The revocations the person's identity root signed, as this device's
    /// store holds them.
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
