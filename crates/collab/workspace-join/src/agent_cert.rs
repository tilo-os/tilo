//! Agent certificates: a device's signed statement that a key acts as one of
//! the workspace's agents, for the device's person, in one agent session of one
//! workspace, until it expires (at most a day).
//!
//! The agent is a member: the workspace's record of its agents names it and
//! the person who added it. The certificate is how one key proves it is that
//! agent. It is signed by a device of that person, so any device they use can
//! certify one, and it verifies as a chain: the device's root-signed
//! certificate is in the workspace's device list (the caller looks it up and
//! checks it is neither revoked nor expired), and the device signed the agent
//! certificate inside the device certificate's window.

use collab_types::WorkspaceId;
use collab_types::member::MAX_AGENT_ID_BYTES;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_user_identity::canonical::signing_digest;
use tilo_user_identity::{DeviceCert, StatementSigner};

use crate::delegation::DelegationError;

/// Longest window of an agent certificate.
pub const MAX_AGENT_CERT_NS: u64 = 24 * 3600 * 1_000_000_000;
pub const MAX_SESSION_BYTES: usize = 256;

/// The device signs the BLAKE3 digest of this tag followed by the canonical
/// encoding of the unsigned fields.
const AGENT_CERT_DOMAIN: &[u8] = b"tilo-agent-cert-v2\0";

/// A device-signed statement that `agent_public_key` is the agent `agent_id`
/// of `user_public_key` in `workspace_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCert {
    /// The person who added the agent: the signing device's root.
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub agent_public_key: [u8; 32],
    /// The agent's member id.
    pub agent_id: String,
    /// The agent session the key works in.
    pub session: String,
    #[serde(with = "crate::workspace_text")]
    pub workspace_id: WorkspaceId,
    /// Issue time, ns since the epoch.
    pub issued_at_ns: u64,
    /// Expiry, ns since the epoch, at most a day on.
    pub expires_at_ns: u64,
    /// The device's signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct Unsigned<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    agent_public_key: &'a [u8; 32],
    agent_id: &'a str,
    session: &'a str,
    #[serde(with = "crate::workspace_text")]
    workspace_id: WorkspaceId,
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl AgentCert {
    /// Certify `agent_public_key` as the agent `agent_id`, in its session
    /// `session` of `workspace`, with `device`, a device whose root is
    /// `user_public_key`.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for out-of-bounds fields and
    /// [`DelegationError::Unsigned`] when the statement cannot be encoded or
    /// the device key refuses.
    pub fn sign(
        device: &dyn StatementSigner,
        user_public_key: [u8; 32],
        agent_public_key: [u8; 32],
        (agent_id, session): (impl Into<String>, impl Into<String>),
        workspace_id: WorkspaceId,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Result<Self, DelegationError> {
        let mut cert = Self {
            user_public_key,
            device_public_key: device.public_key(),
            agent_public_key,
            agent_id: agent_id.into(),
            session: session.into(),
            workspace_id,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        cert.validate()?;
        cert.signature = device
            .sign_digest(&cert.digest()?)
            .map_err(|error| DelegationError::Unsigned(error.to_string()))?;
        Ok(cert)
    }

    fn digest(&self) -> Result<[u8; 32], DelegationError> {
        let unsigned = Unsigned {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            agent_public_key: &self.agent_public_key,
            agent_id: &self.agent_id,
            session: &self.session,
            workspace_id: self.workspace_id,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        };
        signing_digest(AGENT_CERT_DOMAIN, &unsigned)
            .map_err(|error| DelegationError::Unsigned(error.to_string()))
    }

    fn validate(&self) -> Result<(), DelegationError> {
        if self.agent_id.is_empty()
            || self.agent_id.len() > MAX_AGENT_ID_BYTES
            || self.agent_id.chars().any(char::is_control)
        {
            return Err(DelegationError::Invalid("agent id"));
        }
        if self.session.is_empty()
            || self.session.len() > MAX_SESSION_BYTES
            || self.session.chars().any(char::is_control)
        {
            return Err(DelegationError::Invalid("session"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(DelegationError::Invalid("validity window"));
        }
        if self.expires_at_ns.saturating_sub(self.issued_at_ns) > MAX_AGENT_CERT_NS {
            return Err(DelegationError::Invalid(
                "an agent certificate lasts at most a day",
            ));
        }
        Ok(())
    }

    /// Check the chain at `now_ns`: `device` is the signing device's
    /// root-signed certificate, valid now, and it signed this certificate for
    /// `workspace` inside the device certificate's window and its own.
    ///
    /// The caller has taken `device` from the workspace's device list and
    /// checked it is not revoked.
    ///
    /// # Errors
    /// [`DelegationError::WrongUser`] when the device certificate is another
    /// device's or another person's, [`DelegationError::Uncertified`] when it
    /// does not verify now, [`DelegationError::Signature`] for a bad
    /// signature, [`DelegationError::Expired`] outside the window and
    /// [`DelegationError::Invalid`] for another workspace or bad bounds.
    pub fn verify(
        &self,
        device: &DeviceCert,
        workspace_id: WorkspaceId,
        now_ns: u64,
    ) -> Result<(), DelegationError> {
        if device.device_public_key != self.device_public_key
            || device.user_public_key != self.user_public_key
        {
            return Err(DelegationError::WrongUser);
        }
        tilo_user_identity::verify_device_cert_at(device, now_ns)
            .map_err(|_| DelegationError::Uncertified)?;
        self.validate()?;
        if self.workspace_id != workspace_id {
            return Err(DelegationError::Invalid("workspace"));
        }
        if self.issued_at_ns < device.issued_at_ns
            || device
                .expires_at_ns
                .is_some_and(|expiry| self.issued_at_ns >= expiry)
        {
            return Err(DelegationError::Uncertified);
        }
        let key = VerifyingKey::from_bytes(&self.device_public_key)
            .map_err(|_| DelegationError::Signature)?;
        key.verify(&self.digest()?, &Signature::from_bytes(&self.signature))
            .map_err(|_| DelegationError::Signature)?;
        if now_ns < self.issued_at_ns || now_ns >= self.expires_at_ns {
            return Err(DelegationError::Expired);
        }
        Ok(())
    }

    /// Check `proof` is the agent key's signature over `message`.
    ///
    /// # Errors
    /// [`DelegationError::Signature`] when it is not.
    pub fn verify_agent_signature(
        &self,
        message: &[u8],
        proof: &[u8],
    ) -> Result<(), DelegationError> {
        let proof = <[u8; 64]>::try_from(proof).map_err(|_| DelegationError::Signature)?;
        VerifyingKey::from_bytes(&self.agent_public_key)
            .map_err(|_| DelegationError::Signature)?
            .verify(message, &Signature::from_bytes(&proof))
            .map_err(|_| DelegationError::Signature)
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).unwrap_or_default()
    }

    /// Decode [`Self::encode`] output. The caller still verifies it.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for undecodable bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, DelegationError> {
        postcard::from_bytes(bytes).map_err(|_| DelegationError::Invalid("encoding"))
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use tilo_user_identity::SignError;

    const HOUR_NS: u64 = 3600 * 1_000_000_000;
    const NOW: u64 = 1_800_000_000_000_000_000;

    fn workspace() -> WorkspaceId {
        WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap()
    }

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[1; 32])
    }

    fn device() -> SigningKey {
        SigningKey::from_bytes(&[2; 32])
    }

    fn agent() -> [u8; 32] {
        SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes()
    }

    fn certificate(root: &SigningKey, device: &SigningKey) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.public_key(),
            device_label: "Laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: NOW - HOUR_NS,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn agent_cert(expires: u64) -> AgentCert {
        AgentCert::sign(
            &device(),
            root().verifying_key().to_bytes(),
            agent(),
            ("claude-code", "session-1"),
            workspace(),
            NOW,
            expires,
        )
        .unwrap()
    }

    #[test]
    fn an_agent_certificate_verifies_against_its_device_certificate() {
        let cert = agent_cert(NOW + HOUR_NS);
        let device = certificate(&root(), &device());
        assert_eq!(cert.verify(&device, workspace(), NOW + 1), Ok(()));
        assert_eq!(AgentCert::decode(&cert.encode()).unwrap(), cert);
    }

    #[test]
    fn delegations_and_session_certificates_accept_the_same_bounded_agent_ids() {
        use crate::delegation::{Delegate, DelegateProposal, DelegateScope};

        let root = root();
        let device = device();
        let owner = root.verifying_key().to_bytes();
        let device_cert = certificate(&root, &device);
        let delegate = |id: &str| {
            DelegateProposal {
                delegate: Delegate::Agent {
                    agent_id: id.to_owned(),
                },
                label: "Assistant".into(),
                color: [1, 2, 3],
                scope: DelegateScope {
                    workspaces: vec![workspace()],
                    apps: vec!["so.tilo.chat".into()],
                },
                expires_at_ns: NOW + HOUR_NS,
            }
            .sign(&root, agent(), NOW)
        };
        let session = |id: &str| {
            AgentCert::sign(
                &device,
                owner,
                agent(),
                (id, "a chat turn"),
                workspace(),
                NOW,
                NOW + HOUR_NS,
            )
        };
        let canonical = format!("assistant-{}", "a1".repeat(32));
        for id in [canonical.clone(), format!("{canonical}-2"), "a".repeat(128)] {
            let grant = delegate(&id).unwrap();
            assert_eq!(grant.delegate.agent_id(), Some(id.as_str()));
            grant.verify(&owner, NOW).unwrap();
            let cert = session(&id).unwrap();
            assert_eq!(cert.agent_id, id);
            cert.verify(&device_cert, workspace(), NOW).unwrap();
        }
        let too_long = "a".repeat(129);
        assert_eq!(
            delegate(&too_long),
            Err(DelegationError::Invalid("agent id"))
        );
        assert_eq!(
            session(&too_long),
            Err(DelegationError::Invalid("agent id"))
        );
    }

    #[test]
    fn an_expired_certificate_is_refused() {
        let cert = agent_cert(NOW + HOUR_NS);
        let device = certificate(&root(), &device());
        assert_eq!(
            cert.verify(&device, workspace(), NOW + HOUR_NS),
            Err(DelegationError::Expired)
        );
        assert_eq!(
            cert.verify(&device, workspace(), NOW - 1),
            Err(DelegationError::Expired)
        );
    }

    #[test]
    fn a_certificate_signed_by_an_uncertified_device_is_refused() {
        let cert = agent_cert(NOW + HOUR_NS);
        let foreign = certificate(&SigningKey::from_bytes(&[9; 32]), &device());
        assert_eq!(
            cert.verify(&foreign, workspace(), NOW + 1),
            Err(DelegationError::WrongUser)
        );
        let mut forged = certificate(&root(), &device());
        forged.device_label = "Changed".into();
        assert_eq!(
            cert.verify(&forged, workspace(), NOW + 1),
            Err(DelegationError::Uncertified)
        );
        let mut late = certificate(&root(), &device());
        late.issued_at_ns = NOW + 1;
        tilo_user_identity::sign_device_cert(&mut late, &root()).unwrap();
        assert_eq!(
            cert.verify(&late, workspace(), NOW + 2),
            Err(DelegationError::Uncertified)
        );
        let mut renamed = cert;
        renamed.agent_id = "scribe".into();
        assert_eq!(
            renamed.verify(&certificate(&root(), &device()), workspace(), NOW + 1),
            Err(DelegationError::Signature)
        );
    }

    #[test]
    fn a_certificate_names_one_workspace_and_lasts_at_most_a_day() {
        let device = certificate(&root(), &device());
        let other = WorkspaceId::parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        assert_eq!(
            agent_cert(NOW + HOUR_NS).verify(&device, other, NOW + 1),
            Err(DelegationError::Invalid("workspace"))
        );
        assert_eq!(
            AgentCert::sign(
                &self::device(),
                root().verifying_key().to_bytes(),
                agent(),
                ("claude-code", "session-1"),
                workspace(),
                NOW,
                NOW + 25 * HOUR_NS,
            ),
            Err(DelegationError::Invalid(
                "an agent certificate lasts at most a day"
            ))
        );
    }

    #[test]
    fn only_the_certified_key_proves_it_holds_the_certificate() {
        let cert = agent_cert(NOW + HOUR_NS);
        let message = b"join";
        let proof = SigningKey::from_bytes(&[3; 32]).sign(message).to_bytes();
        assert_eq!(cert.verify_agent_signature(message, &proof), Ok(()));
        let other = device().sign(message).to_bytes();
        assert_eq!(
            cert.verify_agent_signature(message, &other),
            Err(DelegationError::Signature)
        );
    }

    /// A device key that refuses: the certificate is not issued at all,
    /// rather than carrying a signature over anything less than itself.
    struct Refusing;

    impl StatementSigner for Refusing {
        fn public_key(&self) -> [u8; 32] {
            device().verifying_key().to_bytes()
        }

        fn sign_digest(&self, _digest: &[u8; 32]) -> Result<[u8; 64], SignError> {
            Err(SignError("locked".into()))
        }
    }

    #[test]
    fn a_refused_signature_issues_no_certificate() {
        let refused = AgentCert::sign(
            &Refusing,
            root().verifying_key().to_bytes(),
            agent(),
            ("claude-code", "session-1"),
            workspace(),
            NOW,
            NOW + HOUR_NS,
        );
        assert!(matches!(refused, Err(DelegationError::Unsigned(_))));
    }

    #[test]
    fn the_device_signs_the_domain_separated_digest_of_the_whole_certificate() {
        let cert = agent_cert(NOW + HOUR_NS);
        let digest = cert.digest().unwrap();
        assert_eq!(digest.len(), 32);
        VerifyingKey::from_bytes(&cert.device_public_key)
            .unwrap()
            .verify(&digest, &Signature::from_bytes(&cert.signature))
            .unwrap();
        // The bare domain tag, what a failed encoding used to be signed as,
        // is not what the certificate's signature covers.
        let bare = device().sign(AGENT_CERT_DOMAIN).to_bytes();
        let mut forged = cert;
        forged.signature = bare;
        assert_eq!(
            forged.verify(&certificate(&root(), &device()), workspace(), NOW + 1),
            Err(DelegationError::Signature)
        );
    }
}
