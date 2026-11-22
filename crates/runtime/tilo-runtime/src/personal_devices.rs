//! The person's devices as their own workspace says them, for a key's
//! sessions with this device's host, which admit another of their devices.

use std::time::SystemTime;

use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::workspaces::Workspaces;

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

/// The person's devices: every device their identity root certified, less the
/// revocations from that root this host holds.
#[derive(Clone)]
pub struct PersonalDevices {
    workspaces: Workspaces,
    /// The person's identity root.
    root: [u8; 32],
}

impl PersonalDevices {
    /// The devices of the person `this`, this device's own certificate, names, with the
    /// revocations `workspaces`, this device's own service, holds.
    #[must_use]
    pub fn new(workspaces: Workspaces, this: &DeviceCert) -> Self {
        Self {
            workspaces,
            root: this.user_public_key,
        }
    }

    /// Whether `certificate`, shown on a connection the transport proved is
    /// from `dialer`, is one of the person's devices now.
    pub fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal> {
        if certificate.device_public_key != dialer {
            return Err(Refusal::NotTheDialer);
        }
        if certificate.user_public_key != self.root {
            return Err(Refusal::OtherPerson);
        }
        tilo_user_identity::verify_device_cert_at(certificate, now_ns(SystemTime::now()))
            .map_err(|error| Refusal::NotCertified(error.to_string()))?;
        // A grant a revoked device minted ends with it.
        if self.revocations().refuses(certificate) {
            return Err(Refusal::Revoked);
        }
        Ok(())
    }

    fn revocations(&self) -> RevocationList {
        self.workspaces.revocations()
    }
}

fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        })
}
