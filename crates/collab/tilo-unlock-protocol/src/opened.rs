//! How and when this boot's system volume opened.
//!
//! Early boot opens the volume one of several ways, and nothing else keeps
//! the answer: the booted system cannot tell an automatic unlock from one a
//! person approved afterwards. `tilo-auth` leaves this as root in the
//! initrd's `/run`, which the booted system keeps at the same path, and the
//! server reports it in its status.

use serde::{Deserialize, Serialize};

/// Where early boot leaves the [`Opening`]: in the initrd's `/run`, carried
/// into the booted system at the same path.
pub const OPENED_PATH: &str = "/run/tilo-unlock.json";

/// Which way the volume opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnlockMethod {
    /// The TPM released the person's sealed key for this boot.
    Tpm,
    /// Another of the person's devices released its half on the boot chain
    /// this machine attested, with nobody asked to approve it: a server
    /// unlocking itself.
    SiblingServer,
    /// The person approved the release on one of their devices.
    PersonDevice,
    /// A person's secret opened their keyslot without the TPM.
    Passphrase,
    /// The recovery key opened the volume.
    RecoveryKey,
}

/// One boot's unlock, as early boot left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opening {
    /// Which way the volume opened.
    pub method: UnlockMethod,
    /// When it opened, in nanoseconds since the Unix epoch.
    pub at_ns: u64,
}

impl Opening {
    /// The bytes early boot writes.
    ///
    /// # Errors
    /// The opening could not be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// The opening in `bytes`, if it is one.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opening_round_trips_and_names_every_way_in_words() {
        for (method, named) in [
            (UnlockMethod::Tpm, "tpm"),
            (UnlockMethod::SiblingServer, "sibling-server"),
            (UnlockMethod::PersonDevice, "person-device"),
            (UnlockMethod::Passphrase, "passphrase"),
            (UnlockMethod::RecoveryKey, "recovery-key"),
        ] {
            let opened = Opening {
                method,
                at_ns: 1_700_000_000_000_000_000,
            };
            let bytes = opened.encode().unwrap();
            assert!(
                String::from_utf8_lossy(&bytes).contains(&format!("\"method\":\"{named}\"")),
                "{} names its way plainly",
                String::from_utf8_lossy(&bytes)
            );
            assert_eq!(Opening::decode(&bytes), Some(opened));
        }
        assert_eq!(Opening::decode(b""), None);
        assert_eq!(Opening::decode(b"{\"method\":\"telepathy\"}"), None);
    }
}
