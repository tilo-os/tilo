//! Trust policy — which peer certs the relay accepts.
//!
//! Every inbound connection carries a `DeviceCert`. The auth
//! handshake ([`crate::auth`]) checks the cert's signature; this
//! module decides whether the *identity* the cert chains to is
//! allowed to talk to us at all.
//!
//! Three shapes:
//!
//! - `AllowMatchingUser` (default) — cert must chain to the relay's
//!   own user identity. A home server for one household.
//! - `AllowList(pubkeys)` — cert must chain to one of the listed
//!   user public keys. Useful for a small multi-tenant deployment
//!   (Family + roommate, small company shared server).
//! - `AllowAny` — accept any cert whose signature verifies against
//!   its embedded user pubkey. Effectively an open relay — the
//!   handshake still guarantees mutual proof-of-possession, and
//!   workspaces are still keyed by their id so cross-tenant reads
//!   remain impossible. Ships disabled; must be enabled explicitly.

use serde::{Deserialize, Serialize};

use tilo_user_identity::DeviceCert;

/// Which certs the relay accepts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[derive(Default)]
pub enum TrustPolicy {
    /// Only accept certs that chain to the relay's own user identity.
    /// This is the default — a self-hosted relay for one household.
    #[default]
    AllowMatchingUser,

    /// Accept certs chaining to any user identity in the allow list.
    /// Values are 32-byte Ed25519 public keys, hex-encoded in the
    /// TOML.
    AllowList {
        /// Hex-encoded user root public keys (32 bytes each).
        users: Vec<String>,
    },

    /// Accept any cert with a valid signature. Only useful for
    /// public-relay experiments. The workspace-id keying still
    /// isolates tenants from each other.
    AllowAny,
}

impl TrustPolicy {
    /// Returns true if a cert's user identity is admissible under
    /// this policy.
    ///
    /// `own_user_pubkey` is the relay's own user identity — passed
    /// in rather than stored on the enum so the policy stays
    /// serialisable without embedding secrets.
    pub fn admits(&self, cert: &DeviceCert, own_user_pubkey: &[u8; 32]) -> bool {
        match self {
            Self::AllowMatchingUser => &cert.user_public_key == own_user_pubkey,
            Self::AllowList { users } => users.iter().any(|hex_pk| match hex::decode(hex_pk) {
                Ok(bytes) if bytes.len() == 32 => {
                    bytes.as_slice() == cert.user_public_key.as_slice()
                }
                _ => false,
            }),
            Self::AllowAny => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};

    fn fresh_cert(user_label: &str) -> (DeviceCert, [u8; 32]) {
        let (root_sk, user) = UserIdentity::generate(user_label, 1);
        let (_dev_sk, dev) = DeviceIdentity::generate("test-dev", 2);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 3,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        (cert, user.public_key)
    }

    /// Every policy variant against admitted and refused certificates. The
    /// `own` key argument is what `admits` compares against for the default
    /// policy; allow-lists look only at the cert's user key and the list.
    #[test]
    fn trust_policies_admit_exactly_their_users() {
        let (cert_alice, alice_pk) = fresh_cert("Alice");
        let (cert_bob, bob_pk) = fresh_cert("Bob");
        let (cert_anyone, _) = fresh_cert("Anyone");
        let alice_only = TrustPolicy::AllowList {
            users: vec![hex::encode(alice_pk)],
        };
        let bad_hex = TrustPolicy::AllowList {
            users: vec!["not-hex".into(), "00".repeat(31)],
        };
        let cases: [(&str, &TrustPolicy, &DeviceCert, &[u8; 32], bool); 6] = [
            (
                "default admits matching user",
                &TrustPolicy::default(),
                &cert_alice,
                &alice_pk,
                true,
            ),
            (
                "matching user rejects others",
                &TrustPolicy::default(),
                &cert_alice,
                &bob_pk,
                false,
            ),
            // Bob's pubkey argument does not matter — the policy looks
            // only at the cert's user pk and the list.
            (
                "allow list admits listed user",
                &alice_only,
                &cert_alice,
                &bob_pk,
                true,
            ),
            (
                "allow list rejects unlisted",
                &alice_only,
                &cert_bob,
                &[0u8; 32],
                false,
            ),
            (
                "allow any admits any",
                &TrustPolicy::AllowAny,
                &cert_anyone,
                &[0u8; 32],
                true,
            ),
            (
                "allow list ignores bad hex",
                &bad_hex,
                &cert_alice,
                &[0u8; 32],
                false,
            ),
        ];
        for (name, policy, cert, own, want) in cases {
            assert_eq!(policy.admits(cert, own), want, "case {name}");
        }
    }
}
