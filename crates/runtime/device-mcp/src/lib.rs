//! The person's own devices, as one of them knows them: which device is theirs now, and why one
//! is not ([`PersonsDevices`], [`Refusal`]).

use tilo_user_identity::{DeviceCert, RevocationList};

/// Why one of the person's devices is not taken as theirs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It said nothing this server reads.
    Unreadable,
    /// The certificate it showed names another device than the one that
    /// connected.
    NotTheDialer,
    /// The device is someone else's.
    OtherPerson,
    /// Its certificate does not hold now.
    NotCertified(String),
    /// The person removed it from their devices.
    Revoked,
    /// This device serves no agents now.
    Unserved(String),
    /// A key made for an agent asked for a workspace its scope does not
    /// reach, or named none.
    OutOfScope,
}

impl Refusal {
    /// The reason the dialing side is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Unreadable => {
                "the server first reads the dialing device's certificate".to_owned()
            }
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
            Self::Unserved(why) => why.clone(),
            Self::OutOfScope => {
                "this key reaches only the workspaces it was made for: name one of them".to_owned()
            }
        }
    }
}

/// The person's devices, as one of them knows them: who it is, which others
/// it knows, and whether a device that dials it is one of them now.
pub trait PersonsDevices: Send + Sync {
    /// This device's own certificate, which it shows the devices it dials.
    fn this_device(&self) -> DeviceCert;

    /// The person's devices this one knows that may act for them now, this
    /// one included.
    fn devices(&self) -> Vec<DeviceCert>;

    /// Whether `certificate`, shown on a connection the transport proved is
    /// from `dialer`, is one of the person's devices now.
    fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal>;

    /// The revocations the person's identity root signed, as this device's
    /// store holds them: what a proof signed by one of the person's devices
    /// is checked against. Empty for a directory that holds none.
    fn revocations(&self) -> RevocationList {
        RevocationList::new()
    }
}
