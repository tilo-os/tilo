//! Delegations: a person's root-signed statement that one of their devices
//! acts for them as an agent, how it is shown, where it may act and until
//! when.
//!
//! The device's certificate root must have signed it, it must name that
//! device and the workspace, and it must be inside its window.

use collab_types::WorkspaceId;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_user_identity::StatementSigner;
use tilo_user_identity::canonical::signing_digest;

/// Largest agent id and label.
pub const MAX_DELEGATE_TEXT_BYTES: usize = 64;
pub const MAX_DELEGATED_WORKSPACES: usize = 16;
pub const MAX_DELEGATED_APPS: usize = 64;

/// The signing domain: the root signs the BLAKE3 digest of this tag followed
/// by the canonical encoding of the unsigned fields, so a delegation's
/// signature can never be read as any other statement's.
const DELEGATION_DOMAIN: &[u8] = b"tilo-delegation-v3\0";

/// Who a delegation names.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
// Externally tagged: postcard, the canonical encoding, cannot decode an
// internally tagged enum.
#[serde(rename_all = "snake_case")]
pub enum Delegate {
    Agent {
        /// Stable id, 1..=[`MAX_DELEGATE_TEXT_BYTES`] bytes.
        agent_id: String,
    },
    /// What an installed bot's delegation named, kept in its place so the
    /// delegations after it read as they were written. Bots are gone: such a
    /// delegation never verifies.
    RetiredBot { installation: String },
}

impl Delegate {
    /// The agent id, when this names an agent.
    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { agent_id } => Some(agent_id),
            Self::RetiredBot { .. } => None,
        }
    }
}

/// Where a delegate may act.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateScope {
    /// Workspaces it may act in, 1..=[`MAX_DELEGATED_WORKSPACES`].
    #[serde(with = "crate::workspace_text::list")]
    pub workspaces: Vec<WorkspaceId>,
    /// App ids; empty means every app in those workspaces.
    pub apps: Vec<String>,
}

/// A root-signed statement that `device_public_key` acts for
/// `user_public_key` as `delegate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delegation {
    /// The delegating person's root.
    pub user_public_key: [u8; 32],
    /// The delegate's certified device key.
    pub device_public_key: [u8; 32],
    /// Whom the device acts as.
    pub delegate: Delegate,
    /// Display name, 1..=[`MAX_DELEGATE_TEXT_BYTES`] bytes.
    pub label: String,
    /// Cursor colour, sRGB.
    pub color: [u8; 3],
    /// Where it may act.
    pub scope: DelegateScope,
    /// Issue time, ns since the epoch.
    pub issued_at_ns: u64,
    /// Expiry, ns since the epoch. Delegations always expire.
    pub expires_at_ns: u64,
    /// Root signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Why a delegation is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DelegationError {
    /// Signed by another root than the one expected.
    #[error("the delegation names another user")]
    WrongUser,
    /// The signature does not verify.
    #[error("the delegation signature does not verify")]
    Signature,
    /// The signing device's certificate does not verify now, or the
    /// delegation was signed outside the certificate's window.
    #[error("the delegating device is not certified")]
    Uncertified,
    /// Outside its validity window.
    #[error("the delegation has expired")]
    Expired,
    /// A field is out of bounds.
    #[error("invalid delegation: {0}")]
    Invalid(&'static str),
    /// The statement could not be encoded or the key refused to sign it.
    /// Nothing is signed then: a signature over anything but the full
    /// statement would vouch for something else.
    #[error("the delegation could not be signed: {0}")]
    Unsigned(String),
}

#[derive(Serialize)]
struct Unsigned<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    delegate: &'a Delegate,
    label: &'a str,
    color: &'a [u8; 3],
    scope: &'a DelegateScope,
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl Delegation {
    /// Sign a delegation of `device_public_key` with the person's `root`.
    ///
    /// # Errors
    /// [`DelegationError::Invalid`] for out-of-bounds fields.
    pub fn sign(
        root: &dyn StatementSigner,
        device_public_key: [u8; 32],
        delegate: Delegate,
        label: impl Into<String>,
        color: [u8; 3],
        scope: DelegateScope,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Result<Self, DelegationError> {
        let mut delegation = Self {
            user_public_key: root.public_key(),
            device_public_key,
            delegate,
            label: label.into(),
            color,
            scope,
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
        };
        delegation.validate()?;
        delegation.signature = root
            .sign_digest(&delegation.digest()?)
            .map_err(|error| DelegationError::Unsigned(error.to_string()))?;
        Ok(delegation)
    }

    fn digest(&self) -> Result<[u8; 32], DelegationError> {
        let unsigned = Unsigned {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            delegate: &self.delegate,
            label: &self.label,
            color: &self.color,
            scope: &self.scope,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        };
        signing_digest(DELEGATION_DOMAIN, &unsigned)
            .map_err(|error| DelegationError::Unsigned(error.to_string()))
    }

    fn validate(&self) -> Result<(), DelegationError> {
        let text = |value: &str| !value.is_empty() && value.len() <= MAX_DELEGATE_TEXT_BYTES;
        if !text(&self.label) {
            return Err(DelegationError::Invalid("label"));
        }
        match &self.delegate {
            Delegate::Agent { agent_id } if !text(agent_id) => {
                return Err(DelegationError::Invalid("agent id"));
            }
            Delegate::Agent { .. } => {}
            Delegate::RetiredBot { .. } => {
                return Err(DelegationError::Invalid("a bot's delegation is retired"));
            }
        }
        if self.scope.workspaces.is_empty()
            || self.scope.workspaces.len() > MAX_DELEGATED_WORKSPACES
        {
            return Err(DelegationError::Invalid("workspaces"));
        }
        if self.scope.apps.len() > MAX_DELEGATED_APPS
            || self
                .scope
                .apps
                .iter()
                .any(|app| app.is_empty() || app.len() > 128)
        {
            return Err(DelegationError::Invalid("apps"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(DelegationError::Invalid("validity window"));
        }
        Ok(())
    }

    /// Check the delegation was signed by `user_public_key` and is valid at
    /// `now_ns`.
    pub fn verify(&self, user_public_key: &[u8; 32], now_ns: u64) -> Result<(), DelegationError> {
        if &self.user_public_key != user_public_key {
            return Err(DelegationError::WrongUser);
        }
        self.validate()?;
        let key =
            VerifyingKey::from_bytes(user_public_key).map_err(|_| DelegationError::Signature)?;
        key.verify(&self.digest()?, &Signature::from_bytes(&self.signature))
            .map_err(|_| DelegationError::Signature)?;
        if now_ns < self.issued_at_ns || now_ns >= self.expires_at_ns {
            return Err(DelegationError::Expired);
        }
        Ok(())
    }

    /// Whether the delegate may be in `workspace`.
    #[must_use]
    pub fn permits_workspace(&self, workspace: WorkspaceId) -> bool {
        self.scope.workspaces.contains(&workspace)
    }

    /// Whether the delegate may act in `app_id`.
    #[must_use]
    pub fn permits_app(&self, app_id: &str) -> bool {
        self.scope.apps.is_empty() || self.scope.apps.iter().any(|app| app == app_id)
    }

    /// Canonical encoding, for carrying the delegation to a browser.
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
    use collab_types::WorkspaceId;
    use ed25519_dalek::{Signer, SigningKey};

    const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const OTHER_WORKSPACE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[1; 32])
    }

    fn scope(workspaces: &[&str]) -> DelegateScope {
        DelegateScope {
            workspaces: workspaces
                .iter()
                .filter_map(|workspace| WorkspaceId::parse(workspace))
                .collect(),
            apps: Vec::new(),
        }
    }

    fn scribe(
        workspaces: &[&str],
        issued: u64,
        expires: u64,
    ) -> Result<Delegation, DelegationError> {
        Delegation::sign(
            &root(),
            [9; 32],
            Delegate::Agent {
                agent_id: "scribe".into(),
            },
            "Scribe",
            [88, 101, 242],
            scope(workspaces),
            issued,
            expires,
        )
    }

    #[test]
    fn an_agent_delegation_round_trips_and_verifies() {
        let delegation = Delegation::sign(
            &root(),
            [8; 32],
            Delegate::Agent {
                agent_id: "scribe".into(),
            },
            "Scribe",
            [10, 20, 30],
            scope(&[WORKSPACE, OTHER_WORKSPACE]),
            10,
            20,
        )
        .unwrap();
        let decoded = Delegation::decode(&delegation.encode()).unwrap();
        assert_eq!(decoded, delegation);
        decoded
            .verify(&root().verifying_key().to_bytes(), 15)
            .unwrap();
        assert_eq!(decoded.delegate.agent_id(), Some("scribe"));
        assert!(decoded.permits_workspace(WorkspaceId::parse(OTHER_WORKSPACE).unwrap()));
        assert_eq!(
            decoded.verify(&root().verifying_key().to_bytes(), 20),
            Err(DelegationError::Expired)
        );
        assert_eq!(
            decoded.verify(&[2; 32], 15),
            Err(DelegationError::WrongUser)
        );
    }

    #[test]
    fn a_delegation_signed_in_the_old_shape_never_verifies() {
        // The agent shape signed `tilo-agent-delegation-v1\0` over a struct
        // with a bare agent id. A signature over those bytes must not verify
        // as a delegation of the new type.
        #[derive(serde::Serialize)]
        struct OldUnsigned<'a> {
            user_public_key: &'a [u8; 32],
            agent_device_public_key: &'a [u8; 32],
            agent_id: &'a str,
            label: &'a str,
            color: &'a [u8; 3],
            scope: &'a DelegateScope,
            issued_at_ns: u64,
            expires_at_ns: u64,
        }
        let root = root();
        let user = root.verifying_key().to_bytes();
        let scope = scope(&[WORKSPACE]);
        let mut old = b"tilo-agent-delegation-v1\0".to_vec();
        old.extend(
            postcard::to_allocvec(&OldUnsigned {
                user_public_key: &user,
                agent_device_public_key: &[8; 32],
                agent_id: "scribe",
                label: "Scribe",
                color: &[10, 20, 30],
                scope: &scope,
                issued_at_ns: 10,
                expires_at_ns: 20,
            })
            .unwrap(),
        );
        let forged = Delegation {
            user_public_key: user,
            device_public_key: [8; 32],
            delegate: Delegate::Agent {
                agent_id: "scribe".into(),
            },
            label: "Scribe".into(),
            color: [10, 20, 30],
            scope,
            issued_at_ns: 10,
            expires_at_ns: 20,
            signature: root.sign(&old).to_bytes(),
        };
        assert_eq!(forged.verify(&user, 15), Err(DelegationError::Signature));
    }

    #[test]
    fn a_retired_bots_delegation_is_never_signed_nor_verified() {
        let retired = || Delegate::RetiredBot {
            installation: "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6".into(),
        };
        let refused = Err(DelegationError::Invalid("a bot's delegation is retired"));
        let signed = Delegation::sign(
            &root(),
            [9; 32],
            retired(),
            "Bridge",
            [0; 3],
            scope(&[WORKSPACE]),
            1,
            2,
        );
        assert_eq!(signed, refused);
        // Bytes signed before bots were retired read back, and do not verify.
        let mut kept = scribe(&[WORKSPACE], 1, 2).unwrap();
        kept.delegate = retired();
        kept.signature = root().sign_digest(&kept.digest().unwrap()).unwrap();
        let read = Delegation::decode(&kept.encode()).unwrap();
        assert_eq!(read.delegate.agent_id(), None);
        assert_eq!(
            read.verify(&root().verifying_key().to_bytes(), 1),
            refused.map(drop)
        );
    }

    #[test]
    fn changing_the_delegate_breaks_the_signature() {
        let mut delegation = scribe(&[WORKSPACE], 1, 2).unwrap();
        delegation.delegate = Delegate::Agent {
            agent_id: "editor".into(),
        };
        assert_eq!(
            delegation.verify(&root().verifying_key().to_bytes(), 1),
            Err(DelegationError::Signature)
        );
    }
}
