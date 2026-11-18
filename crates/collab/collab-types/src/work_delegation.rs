//! A member's signed delegation of their own work to a server device.
//!
//! Work in a workspace is the workspace's or a member's. Any key holder takes
//! the workspace's work; a member's work — an agent using their mail, an
//! outbox entry that needs their password, a Browser they are signed in to —
//! is taken by a server of another person only because the member said so. The
//! member's own device signs this delegation: it names the server device, this
//! workspace, the work by the grant paths the workspace decides actions by,
//! the actor the server acts as (the member, or the member's agent), and until
//! when. It lives in the workspace, so every holder replays the same rule; the
//! member revokes it by signing it again with the revocation set.
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::signer::{DeviceSigner, sign_now};
use tilo_action_policy::{ActionKind, Answer, Grant, TreePath};

/// Key of the workspace's work delegations in its own document: a map from
/// each delegation's row (the server's device key and the member's root, hex,
/// [`WorkDelegation::row_key`]) to the signed delegation (JSON).
pub const WORK_DELEGATIONS_KEY: &str = "work_delegations_v1";

/// The signing domain: a signature made over another shape never verifies.
const WORK_DELEGATION_DOMAIN: &[u8] = b"tilo-work-delegation-v1\0";

/// Most grant paths one delegation may name.
pub const MAX_DELEGATED_WORK: usize = 32;
/// Longest agent id a delegation may act as.
pub const MAX_DELEGATED_ACTOR_BYTES: usize = 64;
/// Most of the member's vault items one delegation may let the server use.
pub const MAX_DELEGATED_SECRETS: usize = 32;

/// Why a work delegation is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkDelegationError {
    /// The signature does not verify under the signing device's key.
    #[error("the work delegation's signature does not verify")]
    Signature,
    /// It names another workspace.
    #[error("the work delegation names another workspace")]
    OtherWorkspace,
    /// Outside its validity window, or revoked at this time.
    #[error("the work delegation is not current")]
    NotCurrent,
    /// A field is out of bounds, or it names no work at all.
    #[error("invalid work delegation: {0}")]
    Invalid(&'static str),
    /// The signer answered only later, outside the turn that writes it.
    #[error("the signing device answers only later")]
    Signer,
}

/// A member's signed statement that `server` may take their work in
/// `workspace` as `agent` (or as the member themselves) within `grants`, until
/// `expires_at_ns`, unless revoked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDelegation {
    /// The member delegating their work: their user root key.
    pub person: [u8; 32],
    /// The member's device that signed it.
    pub device: [u8; 32],
    /// The server device that may take the work.
    pub server: [u8; 32],
    /// The workspace the delegation is for, as the workspace names it.
    pub workspace: String,
    /// The agent of the member's the server acts as; `None` acts as the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Which work, by the paths the workspace decides actions by: a change is
    /// the member's work only where one of these allows it.
    pub grants: Vec<Grant>,
    /// Which of the member's vault items the server may use, by their ids: a
    /// delegation names none, and then the server uses no secret of theirs.
    /// An item's own leave and its person's answer still decide each use; this
    /// says only which items the server may be answered about at all.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// When it was issued, ns since the epoch.
    pub issued_at_ns: u64,
    /// When it stops working, ns since the epoch.
    pub expires_at_ns: u64,
    /// When the member revoked it, ns since the epoch; `None` while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ns: Option<u64>,
    /// The signing device's signature over everything above.
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct Unsigned<'a> {
    person: &'a [u8; 32],
    device: &'a [u8; 32],
    server: &'a [u8; 32],
    workspace: &'a str,
    agent: &'a Option<String>,
    grants: &'a [Grant],
    secrets: &'a [String],
    issued_at_ns: u64,
    expires_at_ns: u64,
    revoked_at_ns: &'a Option<u64>,
}

impl WorkDelegation {
    /// A delegation by `key`'s device of its person's work to `server` in
    /// `workspace`, as `agent` (or as the person) and within `grants`, letting
    /// it use the vault items `secrets`, from `valid`'s first instant until its
    /// second, both ns since the epoch.
    ///
    /// # Errors
    /// [`WorkDelegationError::Invalid`] for out-of-bounds fields,
    /// [`WorkDelegationError::Signer`] when the device key answers only later.
    pub fn sign(
        key: &dyn DeviceSigner,
        person: [u8; 32],
        server: [u8; 32],
        workspace: impl Into<String>,
        agent: Option<String>,
        grants: Vec<Grant>,
        secrets: Vec<String>,
        (issued_at_ns, expires_at_ns): (u64, u64),
    ) -> Result<Self, WorkDelegationError> {
        Self::sealed(
            key,
            Self {
                person,
                device: key.device_public_key(),
                server,
                workspace: workspace.into(),
                agent,
                grants,
                secrets,
                issued_at_ns,
                expires_at_ns,
                revoked_at_ns: None,
                signature: [0; 64],
            },
        )
    }

    /// `delegation` with its signature set: the one place a delegation is made
    /// whole, so what is checked is what is signed.
    fn sealed(key: &dyn DeviceSigner, mut delegation: Self) -> Result<Self, WorkDelegationError> {
        delegation.signature = [0; 64];
        delegation.validate()?;
        delegation.signature =
            sign_now(key, delegation.signing_bytes()).map_err(|_| WorkDelegationError::Signer)?;
        Ok(delegation)
    }

    /// The same delegation, revoked at `revoked_at_ns` and signed again by the
    /// member's device `key`.
    ///
    /// # Errors
    /// As [`Self::sign`], and [`WorkDelegationError::Invalid`] when `key` is
    /// not the device that signed it.
    pub fn revoked(
        &self,
        key: &dyn DeviceSigner,
        revoked_at_ns: u64,
    ) -> Result<Self, WorkDelegationError> {
        if key.device_public_key() != self.device {
            return Err(WorkDelegationError::Invalid(
                "only the device that signed it revokes it",
            ));
        }
        let mut revoked = self.clone();
        revoked.revoked_at_ns = Some(revoked_at_ns);
        Self::sealed(key, revoked)
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let unsigned = Unsigned {
            person: &self.person,
            device: &self.device,
            server: &self.server,
            workspace: &self.workspace,
            agent: &self.agent,
            grants: &self.grants,
            secrets: &self.secrets,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
            revoked_at_ns: &self.revoked_at_ns,
        };
        let mut bytes = WORK_DELEGATION_DOMAIN.to_vec();
        bytes.extend(postcard::to_allocvec(&unsigned).unwrap_or_default());
        bytes
    }

    /// The member, or the agent of theirs, the server acts as: the actor
    /// history names the work by.
    #[must_use]
    pub fn actor(&self) -> tilo_action_policy::MemberId {
        match &self.agent {
            Some(agent) => tilo_action_policy::MemberId::agent(agent.clone()),
            None => tilo_action_policy::MemberId::Person(tilo_action_policy::PersonId::from_root(
                self.person,
            )),
        }
    }

    fn validate(&self) -> Result<(), WorkDelegationError> {
        if self.workspace.is_empty() {
            return Err(WorkDelegationError::Invalid("workspace"));
        }
        if self.grants.is_empty() {
            return Err(WorkDelegationError::Invalid("work"));
        }
        let actor = tilo_action_policy::Grantee::Member(self.actor());
        let person = tilo_action_policy::Grantee::person(hex::encode(self.person));
        if self.grants.len() > MAX_DELEGATED_WORK
            || self.grants.iter().any(|grant| {
                grant.answer != Answer::Allow
                    || grant.grantee != actor
                    || grant.granted_by != person
            })
        {
            return Err(WorkDelegationError::Invalid("grants"));
        }
        if self
            .agent
            .as_ref()
            .is_some_and(|agent| agent.is_empty() || agent.len() > MAX_DELEGATED_ACTOR_BYTES)
        {
            return Err(WorkDelegationError::Invalid("agent"));
        }
        if self.secrets.len() > MAX_DELEGATED_SECRETS
            || self.secrets.iter().any(|item| !is_item_id(item))
        {
            return Err(WorkDelegationError::Invalid("secrets"));
        }
        if self.expires_at_ns <= self.issued_at_ns {
            return Err(WorkDelegationError::Invalid("validity window"));
        }
        if self
            .revoked_at_ns
            .is_some_and(|revoked| revoked < self.issued_at_ns)
        {
            return Err(WorkDelegationError::Invalid("revocation"));
        }
        Ok(())
    }

    /// Whether the delegation is signed by its device and names `workspace`, at
    /// no particular time: what a holder checks before it lists one.
    ///
    /// # Errors
    /// [`WorkDelegationError`] when it does not hold.
    pub fn verify_signature(&self, workspace: &str) -> Result<(), WorkDelegationError> {
        if self.workspace != workspace {
            return Err(WorkDelegationError::OtherWorkspace);
        }
        self.validate()?;
        let key =
            VerifyingKey::from_bytes(&self.device).map_err(|_| WorkDelegationError::Signature)?;
        key.verify_strict(
            &self.signing_bytes(),
            &Signature::from_bytes(&self.signature),
        )
        .map_err(|_| WorkDelegationError::Signature)
    }

    /// [`Self::verify_signature`], and that it is live at `now_ns`: issued,
    /// not expired, and not revoked by then.
    ///
    /// # Errors
    /// [`WorkDelegationError`] when it does not hold or is not current.
    pub fn verify(&self, workspace: &str, now_ns: u64) -> Result<(), WorkDelegationError> {
        self.verify_signature(workspace)?;
        // One that was issued after the asking device's clock is not yet
        // anyone's to take: every holder then reads the same live set.
        if now_ns < self.issued_at_ns
            || now_ns >= self.expires_at_ns
            || self.revoked_at_ns.is_some_and(|revoked| revoked <= now_ns)
        {
            return Err(WorkDelegationError::NotCurrent);
        }
        Ok(())
    }

    /// Whether the server may take work at `node` of `kind` under it.
    #[must_use]
    pub fn allows(&self, node: &TreePath, kind: ActionKind) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.kinds.contains(kind) && grant.node.covers(node))
    }

    /// Whether the delegation lets the server use the vault item `item`: only
    /// the items the member named are theirs to use there, and an item they
    /// named none of stays on their own devices.
    #[must_use]
    pub fn allows_secret(&self, item: &str) -> bool {
        self.secrets.iter().any(|named| named == item)
    }

    /// The row it is kept under: the server's device key and the member's
    /// root, hex. A server may hold one delegation per member, and a member
    /// one per server, so two holders' rows never replace each other.
    #[must_use]
    pub fn row_key(&self) -> String {
        row_key(&self.server, &self.person)
    }

    /// The server and member [`Self::row_key`] names, when `key` is one.
    #[must_use]
    pub fn row_parts(key: &str) -> Option<([u8; 32], [u8; 32])> {
        let (server, person) = key.split_once('/')?;
        Some((key_of(server)?, key_of(person)?))
    }
}

fn row_key(server: &[u8; 32], person: &[u8; 32]) -> String {
    format!("{}/{}", hex::encode(server), hex::encode(person))
}

fn key_of(text: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(text).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// Whether `text` is a vault item's id: 32 lowercase hexadecimal digits, as
/// the vault makes them.
fn is_item_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// The one vault item the test's delegation names.
    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn delegation(key: &SigningKey, server: [u8; 32], expires_at_ns: u64) -> WorkDelegation {
        let person = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let grant = Grant::new(
            "send-mail",
            tilo_action_policy::Grantee::person(hex::encode(person)),
            TreePath::workspace("w").app("so.tilo.mail"),
            tilo_action_policy::KindSet::READ_UNDOABLE,
            Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(person)),
        );
        WorkDelegation::sign(
            key,
            person,
            server,
            "w",
            None,
            vec![grant],
            vec![SECRET.to_owned()],
            (1, expires_at_ns),
        )
        .unwrap()
    }

    /// The member's device signs a delegation, every reader checks it, a
    /// revocation is the member's device's and stops it, and another device's
    /// signature names another row.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_delegation_is_the_members_devices_and_stops_when_it_says_so() -> TestResult {
        let key = SigningKey::from_bytes(&[7; 32]);
        let delegation = delegation(&key, [8; 32], 1_000);
        delegation.verify("w", 500)?;
        assert_eq!(
            delegation.verify("w", 1_000),
            Err(WorkDelegationError::NotCurrent),
            "an expired delegation takes no work"
        );
        assert_eq!(
            delegation.verify("other", 500),
            Err(WorkDelegationError::OtherWorkspace)
        );

        let mut forged = delegation.clone();
        forged.server = [3; 32];
        assert_eq!(
            forged.verify("w", 500),
            Err(WorkDelegationError::Signature),
            "the signed bytes bind the server it names"
        );

        let revoked = delegation.revoked(&key, 600)?;
        assert!(revoked.verify("w", 599).is_ok());
        assert_eq!(
            revoked.verify("w", 600),
            Err(WorkDelegationError::NotCurrent),
            "a revocation stops it at once"
        );
        assert_eq!(
            delegation.revoked(&SigningKey::from_bytes(&[6; 32]), 600),
            Err(WorkDelegationError::Invalid(
                "only the device that signed it revokes it"
            )),
            "no other device revokes it"
        );

        let node = TreePath::workspace("w")
            .app("so.tilo.mail")
            .kind("message")
            .action("send");
        assert!(delegation.allows(&node, ActionKind::Undoable));
        assert!(!delegation.allows(&node, ActionKind::Outside));
        let elsewhere = TreePath::workspace("w")
            .app("so.tilo.chat")
            .kind("message")
            .action("send");
        assert!(!delegation.allows(&elsewhere, ActionKind::Undoable));

        // Only the vault items the member named are the server's to use there.
        assert!(delegation.allows_secret(SECRET));
        assert!(!delegation.allows_secret("fedcba9876543210fedcba9876543210"));

        // What names no item of theirs lets it use none: a delegation names
        // what it means to, and a shape no item has is refused where signed.
        let mut unnamed = delegation.clone();
        unnamed.secrets.clear();
        assert!(!unnamed.allows_secret(SECRET));
        unnamed.secrets = vec!["not-an-item".to_owned()];
        assert_eq!(
            unnamed.validate(),
            Err(WorkDelegationError::Invalid("secrets"))
        );
        unnamed.secrets = vec![SECRET.to_owned(); MAX_DELEGATED_SECRETS + 1];
        assert_eq!(
            unnamed.validate(),
            Err(WorkDelegationError::Invalid("secrets"))
        );

        let row = delegation.row_key();
        assert_eq!(
            WorkDelegation::row_parts(&row),
            Some(([8; 32], delegation.person))
        );
        assert_eq!(WorkDelegation::row_parts("nonsense"), None);
        Ok(())
    }
}
