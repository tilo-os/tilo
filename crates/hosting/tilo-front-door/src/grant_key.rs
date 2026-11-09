//! The key of one grant: what the door acts with, and the only thing.
//!
//! The door holds no device of anyone. When a person approves an agent, the
//! door generates a key for that one grant, and the person's own Tilo
//! certifies it with their root as an agent's grant
//! ([`tilo_user_identity::AgentScope`]): it reaches the workspaces the person
//! chose (all of them and new ones, or one), and it expires. Everything the
//! door then does for the agent (listing and making workspaces on the
//! person's hosts, opening sessions at their runtime) it does as that key,
//! and every host and runtime checks the certificate, its scope and the
//! person's revocations themselves. Revoking the grant forgets the key.

use ed25519_dalek::Signer as _;
use std::time::Duration;
use tilo_user_identity::{AgentScope, DeviceCert, SignInProof};

/// How long the person is asked to let a grant last: a day short of the
/// most a grant's certificate may (90 days), so a page whose clock runs
/// behind the door's still certifies it. Past it the agent's tokens stop
/// working and it asks the person again.
pub const GRANT_LIFETIME: Duration = Duration::from_secs(89 * 24 * 60 * 60);

/// A grant's key and the person's certificate for it.
#[derive(Clone)]
pub struct GrantKey {
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    scope: AgentScope,
}

impl std::fmt::Debug for GrantKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantKey")
            .field("device", &hex::encode(self.cert.device_public_key))
            .field("person", &hex::encode(self.cert.user_public_key))
            .field("scope", &self.cert.capabilities.agent)
            .finish_non_exhaustive()
    }
}

/// Why a certificate is not the one asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl GrantKey {
    /// A fresh key for a grant not certified yet.
    ///
    /// # Errors
    /// The system's random source failed.
    pub fn generate() -> std::io::Result<ed25519_dalek::SigningKey> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)?;
        Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// `key` with `cert`, which must certify it as an agent's grant of
    /// `scope` that holds at `now_ns`.
    ///
    /// # Errors
    /// The certificate is for another key or scope, is no grant, or does not
    /// hold.
    pub fn new(
        key: ed25519_dalek::SigningKey,
        cert: DeviceCert,
        scope: AgentScope,
        now_ns: u64,
    ) -> Result<Self, Refused> {
        if cert.device_public_key != key.verifying_key().to_bytes() {
            return Err(Refused("the certificate is for another key".into()));
        }
        if cert.capabilities.agent != Some(scope) {
            return Err(Refused(
                "the certificate does not grant what was approved".into(),
            ));
        }
        tilo_user_identity::verify_device_cert_at(&cert, now_ns)
            .map_err(|error| Refused(format!("the certificate does not hold: {error}")))?;
        Ok(Self { key, cert, scope })
    }

    /// A grant as the store keeps it: the key's 32 secret bytes and the
    /// certificate's canonical encoding. Not checked against the clock: a
    /// kept grant that expired is still read, to be refused where it is used.
    ///
    /// # Errors
    /// The bytes are not a key and a grant certificate for it.
    pub fn from_kept(secret: &[u8], cert: &[u8]) -> Result<Self, Refused> {
        let secret: [u8; 32] = secret
            .try_into()
            .map_err(|_| Refused("a kept grant key is 32 bytes".into()))?;
        let key = ed25519_dalek::SigningKey::from_bytes(&secret);
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(cert)
            .map_err(|error| Refused(format!("a kept grant certificate: {error}")))?;
        let scope = cert
            .capabilities
            .agent
            .ok_or_else(|| Refused("a kept certificate is no grant".into()))?;
        Self::new(key, cert.clone(), scope, cert.issued_at_ns)
    }

    /// The key's secret bytes, for the store only.
    #[must_use]
    pub fn secret(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    /// The certificate's canonical encoding, for the store.
    #[must_use]
    pub fn cert_bytes(&self) -> Vec<u8> {
        tilo_user_identity::canonical::canonical_bytes(&self.cert).unwrap_or_default()
    }

    #[must_use]
    pub const fn key(&self) -> &ed25519_dalek::SigningKey {
        &self.key
    }

    #[must_use]
    pub const fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    /// The person, by their identity root key, hex.
    #[must_use]
    pub fn person(&self) -> String {
        hex::encode(self.cert.user_public_key)
    }

    /// What the grant reaches.
    #[must_use]
    pub const fn scope(&self) -> AgentScope {
        self.scope
    }

    /// Whether the certificate still holds at `now_ns`.
    #[must_use]
    pub fn holds(&self, now_ns: u64) -> bool {
        tilo_user_identity::verify_device_cert_at(&self.cert, now_ns).is_ok()
    }

    /// The key's answer to `challenge` from the service `audience`.
    #[must_use]
    pub fn prove(&self, audience: &str, challenge: [u8; 32]) -> Option<SignInProof> {
        let digest = tilo_user_identity::sign_in_digest(&self.cert, audience, &challenge).ok()?;
        Some(SignInProof {
            cert: self.cert.clone(),
            audience: audience.to_owned(),
            challenge,
            signature: self.key.sign(&digest).to_bytes(),
        })
    }
}
