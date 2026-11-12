//! One signed membership operation: what a member's device wrote, and proof
//! that it wrote it.
//!
//! An operation is immutable and names itself: [`SignedMembershipOp::id`] is
//! the BLAKE3 of the bytes every device stores. The workspace document keeps
//! each under a root key of its own ([`op_key`]), so a receiver never has to
//! choose between two writers' rows: it keeps both operations and derives the
//! membership from the set ([`super::replay`]). Two devices writing the
//! workspace's first operation at once write two keys, where two maps to
//! merge would be two objects only one device's history could keep.

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Change, Grant, Role, WorkspacePolicy};
use tilo_user_identity::DeviceCert;

use crate::workspace::GovernanceRow;

/// The prefix of the workspace document keys holding membership operations,
/// each [`SignedMembershipOp::id`] in hex after it.
pub const MEMBERS_KEY: &str = "members_v2";

/// The document key `id`'s operation is stored under.
#[must_use]
pub fn op_key(id: OpId) -> String {
    format!("{MEMBERS_KEY}/{}", id.to_key())
}

/// The operation key a document key names, when it names one.
#[must_use]
pub fn keyed(key: &str) -> Option<&str> {
    key.strip_prefix(MEMBERS_KEY)?.strip_prefix('/')
}

/// What an operation signs over, before anything else: a device that signs
/// this domain string for some other purpose signs no operation.
const SIGNING_DOMAIN: &[u8] = b"tilo.workspace.membership/1";

/// The id of one membership operation: the BLAKE3 of its signed bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OpId(pub [u8; 32]);

impl OpId {
    /// The id as the workspace document keys it.
    #[must_use]
    pub fn to_key(self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Display for OpId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

/// The membership an operation carries, as stored.
///
/// [`WorkspacePolicy`] is written as JSON in a workspace's rows, where a field
/// an older build does not know is a field it leaves out; an operation is
/// postcard, where leaving a field out moves every field after it. What an
/// operation carries is therefore its own fixed shape, and reads back as the
/// policy it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub workspace: String,
    pub owner: Option<String>,
    pub members: std::collections::BTreeMap<String, Role>,
    pub joined: std::collections::BTreeMap<String, u64>,
    pub grants: Vec<Grant>,
}

impl Membership {
    /// `policy` as an operation carries it.
    #[must_use]
    pub fn of(policy: &WorkspacePolicy) -> Self {
        Self {
            workspace: policy.workspace.clone(),
            owner: policy.owner.clone(),
            members: policy.members.clone(),
            joined: policy.joined.clone(),
            grants: policy.grants.clone(),
        }
    }

    /// The membership as the policy it decides authority by.
    #[must_use]
    pub fn policy(&self) -> WorkspacePolicy {
        WorkspacePolicy {
            workspace: self.workspace.clone(),
            owner: self.owner.clone(),
            members: self.members.clone(),
            joined: self.joined.clone(),
            grants: self.grants.clone(),
        }
    }
}

/// What a membership operation does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MembershipOpCommand {
    /// The first operation of a workspace, the owner's: the membership the
    /// workspace starts from, and the heads of the history it was read at.
    /// Every receiver checks those heads are history it holds, so the owner
    /// names a membership in the workspace's own past; a genesis by anyone
    /// else's device has no effect.
    Genesis {
        legacy_heads: Vec<Vec<u8>>,
        membership: Box<Membership>,
    },
    /// The workspace's membership changes its author's device made, in the
    /// order it made them. Replay checks each against the membership at the
    /// point the operation is reached, so an operation naming a change its
    /// author was not allowed to make changes nothing at all.
    ///
    /// The changes are steps, not the membership they lead to: two members who
    /// wrote at the same time each wrote what they did, and neither one's
    /// operation takes the other's away.
    Project { changes: Vec<Change> },
    /// The governance parts its author's device changed, as the rows they are
    /// ([`GovernanceRow`]). What the values are is the document's history; the
    /// operation says which parts of it the change touched, and every receiver
    /// checks each is its author's to change. A batch that changes a governed
    /// part without an operation naming it changes nothing at all.
    Governance { rows: Vec<GovernanceRow> },
}

/// One member's change to a workspace's authority, signed by their device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMembershipOp {
    /// The device that wrote it: its certificate names the person it acts for.
    pub author: DeviceCert,
    /// The operations this one was written on top of: the membership heads its
    /// writer held.
    pub parents: Vec<OpId>,
    /// When its writer made it, in nanoseconds since the Unix epoch: the
    /// instant its grants are judged at, on every device.
    pub at_ns: u64,
    pub command: MembershipOpCommand,
    /// The author device's signature over [`SignedMembershipOp::signing_bytes`].
    #[serde(with = "serde_bytes")]
    pub sig: [u8; 64],
}

/// The fields an operation signs over, in the order they are signed.
#[derive(Serialize)]
struct Signed<'a> {
    author: &'a DeviceCert,
    parents: &'a [OpId],
    at_ns: u64,
    command: &'a MembershipOpCommand,
}

impl SignedMembershipOp {
    /// Write `command` as `device`, on top of the operations `parents`.
    pub fn signed(
        device: &DeviceCert,
        key: &SigningKey,
        parents: Vec<OpId>,
        at_ns: u64,
        command: MembershipOpCommand,
    ) -> Result<Self, String> {
        let unsigned = Self {
            author: device.clone(),
            parents,
            at_ns,
            command,
            sig: [0; 64],
        };
        let signature = key.sign(&unsigned.signing_bytes()?);
        Ok(Self {
            sig: signature.to_bytes(),
            ..unsigned
        })
    }

    /// The bytes the author device signs: the domain, then the operation
    /// without its signature.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        let mut bytes = SIGNING_DOMAIN.to_vec();
        bytes.extend(
            postcard::to_stdvec(&Signed {
                author: &self.author,
                parents: &self.parents,
                at_ns: self.at_ns,
                command: &self.command,
            })
            .map_err(|error| error.to_string())?,
        );
        Ok(bytes)
    }

    /// Its id: the BLAKE3 of its stored bytes.
    pub fn id(&self) -> Result<OpId, String> {
        Ok(OpId(*blake3::hash(&self.to_bytes()?).as_bytes()))
    }

    /// The bytes a workspace document keeps for it.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        postcard::to_stdvec(self).map_err(|error| error.to_string())
    }

    /// Read the operation `bytes` hold, when they hold one that proves its
    /// writer: its device certificate chain verifies and the signature over
    /// its own bytes is the author device's.
    pub fn verified(bytes: &[u8]) -> Result<Self, String> {
        let op: Self = postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
        op.verify()?;
        Ok(op)
    }

    /// [`Self::verified`] and the operation's id, remembered by the stored
    /// bytes: every read of the workspace's membership reads every operation
    /// its log holds (one more with each admission), and an operation's
    /// bytes prove the same writer whenever they are read. Only operations
    /// that prove their writer are remembered.
    ///
    /// # Errors
    ///
    /// As [`Self::verified`], or the operation has no id.
    pub fn verified_stored(bytes: &[u8]) -> Result<(OpId, std::sync::Arc<Self>), String> {
        use std::sync::Arc;
        /// Room for the logs of every workspace a host holds, kept from
        /// growing without bound.
        const LIMIT: usize = 65_536;
        let seen = verified_ops();
        if let Some(known) = seen.lock().ok().and_then(|seen| seen.get(bytes).cloned()) {
            return Ok(known);
        }
        let op = Self::verified(bytes)?;
        let id = op.id()?;
        let known = (id, Arc::new(op));
        if let Ok(mut seen) = seen.lock() {
            if seen.len() >= LIMIT {
                seen.clear();
            }
            seen.insert(bytes.to_vec(), known.clone());
        }
        Ok(known)
    }

    /// Whether the operation proves its writer.
    pub fn verify(&self) -> Result<(), String> {
        tilo_user_identity::verify_device_cert(&self.author).map_err(|error| error.to_string())?;
        // Every replay of the workspace's membership reads every operation
        // its log holds: one that verified before is remembered.
        tilo_user_identity::verify_remembered(
            &self.author.device_public_key,
            &self.signing_bytes()?,
            &self.sig,
            true,
        )
        .map_err(|refused| match refused {
            tilo_user_identity::RememberedRefusal::Key => {
                "membership operation names no device key".to_owned()
            }
            tilo_user_identity::RememberedRefusal::Signature => {
                "membership operation is not signed by its author's device".to_owned()
            }
        })
    }

    /// The person whose device wrote it, hex.
    #[must_use]
    pub fn person(&self) -> String {
        hex::encode(self.author.user_public_key)
    }
}

type VerifiedOps = std::sync::Mutex<
    std::collections::HashMap<Vec<u8>, (OpId, std::sync::Arc<SignedMembershipOp>)>,
>;

/// The operations [`SignedMembershipOp::verified_stored`] remembers, by
/// their stored bytes.
fn verified_ops() -> &'static VerifiedOps {
    static SEEN: std::sync::OnceLock<VerifiedOps> = std::sync::OnceLock::new();
    SEEN.get_or_init(VerifiedOps::default)
}

/// Forget every operation [`SignedMembershipOp::verified_stored`]
/// remembers, as a process that just started holds none.
#[cfg(test)]
pub(crate) fn forget_verified_ops() {
    if let Ok(mut seen) = verified_ops().lock() {
        seen.clear();
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    pub(crate) fn device(seed: u8) -> (SigningKey, DeviceCert) {
        let root = SigningKey::from_bytes(&[seed; 32]);
        let key = SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device {seed}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    }

    pub(crate) fn an_op() -> (SigningKey, SignedMembershipOp) {
        let (key, cert) = device(9);
        let op = SignedMembershipOp::signed(
            &cert,
            &key,
            Vec::new(),
            1_700_000_000_000_000_000,
            MembershipOpCommand::Project {
                changes: vec![Change::AddMember {
                    person: "member".to_owned(),
                    role: Role::Editor,
                }],
            },
        )
        .unwrap();
        (key, op)
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{an_op, device};
    use super::*;

    /// An operation's bytes are what every device stores and what its id names:
    /// they are stored format, and its id is what other operations name as a
    /// parent.
    #[test]
    fn an_operation_keeps_its_bytes_and_its_id() {
        let (_, op) = an_op();
        let bytes = op.to_bytes().unwrap();
        assert_eq!(
            hex::encode(blake3::hash(&bytes).as_bytes()),
            op.id().unwrap().to_string(),
            "the id is the hash of the stored bytes"
        );
        assert_eq!(SignedMembershipOp::verified(&bytes).unwrap(), op);
        // The stored bytes are the operation itself: its author's person key
        // starts them, so a store reads who wrote it without decoding the
        // command.
        assert_eq!(&bytes[..32], &op.author.user_public_key);
    }

    #[test]
    fn an_operation_that_is_not_its_authors_is_refused() {
        let (_, mut op) = an_op();
        assert!(op.verify().is_ok());
        let (other, other_cert) = device(11);
        // Signed by another device, but naming its original author.
        op.sig = other.sign(&op.signing_bytes().unwrap()).to_bytes();
        assert!(op.verify().is_err(), "another device signed it");
        // Signed by the device it names: the author was rewritten, and so was
        // the signature.
        op.author = other_cert;
        op.sig = other.sign(&op.signing_bytes().unwrap()).to_bytes();
        op.verify().expect("signed by the device it names");
        let (_, mut tampered) = an_op();
        if let MembershipOpCommand::Project { changes } = &mut tampered.command {
            changes.push(Change::SetRole {
                person: "member".to_owned(),
                role: Role::Owner,
            });
        }
        assert!(tampered.verify().is_err(), "the command was changed");
        let (_, mut forged) = an_op();
        forged.author.device_label = "someone else's".into();
        assert!(
            forged.verify().is_err(),
            "the certificate chain no longer verifies"
        );
    }

    #[test]
    fn the_key_is_the_id_in_hex() {
        let (_, op) = an_op();
        let key = op.id().unwrap().to_key();
        assert_eq!(key.len(), 64);
        assert_eq!(hex::decode(&key).unwrap(), op.id().unwrap().0.to_vec());
    }
}
