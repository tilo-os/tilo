//! The trust a workspace keeps for packages: the attestors whose signatures it
//! accepts and the publisher keys it refuses.
//!
//! Both travel in the maps of the workspace tree every member reads: an
//! attestor by id, and a revoked publisher key by its hex. A change to
//! either is a member's change, admitted by the workspace's install
//! grant, and every device verifies the packages it fetches against the lists
//! its workspace keeps.

use automerge::{ROOT, ReadDoc, transaction::Transactable};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::roles::governance;
use tilo_user_identity::DeviceCert;

use super::facades::PeerGovernance;
use super::{StatePeer, SystemTime, err, governed_map, person_of};
use crate::workspace_membership::WorkspaceMembershipCommand;

/// The map of the trust list, by attestor id.
pub(super) const ATTESTORS: &str = "package_attestors_v1";
/// The map of the revoked publisher keys, by hex key.
pub(super) const REVOKED: &str = "package_revoked_v1";

/// An attestor whose signature the workspace accepts, as its trust list keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustAttestor {
    /// The id a package's `[[trust.attested_by]]` names.
    pub id: String,
    /// Its Ed25519 public key, hex.
    pub pubkey_hex: String,
    /// What its signature covers: `all`, `mime:<list>` or `prefix:<list>`.
    pub scope: String,
}

/// A publisher key the workspace refuses, as its revocation list keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevokedPublisher {
    pub pubkey_hex: String,
    #[serde(default)]
    pub reason: String,
}

impl StatePeer {
    /// Apply a trust command; `None` for every other command.
    pub(super) fn apply_trust_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Option<Result<bool, String>> {
        match command {
            WorkspaceMembershipCommand::AddAttestor(attestor) => {
                Some(self.apply_add_attestor(caller, attestor, key, now))
            }
            WorkspaceMembershipCommand::RemoveAttestor { id } => {
                Some(self.apply_remove_attestor(caller, id, key, now))
            }
            WorkspaceMembershipCommand::RevokePublisher { pubkey, reason } => {
                Some(self.apply_revoke_publisher(caller, pubkey, reason, key, now))
            }
            _ => None,
        }
    }

    fn apply_add_attestor(
        &mut self,
        caller: &DeviceCert,
        attestor: &TrustAttestor,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_install_grant(caller, now)?;
        if self
            .governance()
            .trusted_attestors()
            .iter()
            .any(|kept| kept == attestor)
        {
            return Ok(false);
        }
        let bytes = serde_json::to_vec(attestor).map_err(err)?;
        self.put_trust_row(ATTESTORS, &attestor.id, Some(bytes), key, now)
    }

    fn apply_remove_attestor(
        &mut self,
        caller: &DeviceCert,
        id: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_install_grant(caller, now)?;
        if !self
            .governance()
            .trusted_attestors()
            .iter()
            .any(|kept| kept.id == id)
        {
            return Ok(false);
        }
        self.put_trust_row(ATTESTORS, id, None, key, now)
    }

    fn apply_revoke_publisher(
        &mut self,
        caller: &DeviceCert,
        pubkey: &str,
        reason: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_install_grant(caller, now)?;
        let row = RevokedPublisher {
            pubkey_hex: pubkey.to_ascii_lowercase(),
            reason: reason.to_owned(),
        };
        let bytes = serde_json::to_vec(&row).map_err(err)?;
        self.put_trust_row(REVOKED, &row.pubkey_hex, Some(bytes), key, now)
    }

    /// The person behind `caller`, when the workspace lets them choose the
    /// packages it trusts.
    fn check_install_grant(&self, caller: &DeviceCert, now: SystemTime) -> Result<String, String> {
        let member = self.membership(caller.device_public_key, now)?;
        member
            .policy
            .check_action(
                &member.authority(),
                governance::INSTALL_APP,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(person_of(&member)?.to_owned())
    }

    /// Write or remove the row `key_in_map` names in the workspace map `map`, in
    /// one commit.
    fn put_trust_row(
        &mut self,
        map: &str,
        key_in_map: &str,
        bytes: Option<Vec<u8>>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("the workspace tree is missing")?;
        let before = doc.get_heads();
        doc.set_actor(self.workspace_actor_of(key));
        let rows = governed_map(&doc, map)?;
        match bytes {
            Some(bytes) => doc
                .put(&rows, key_in_map, automerge::ScalarValue::Bytes(bytes))
                .map_err(err)?,
            None => doc.delete(&rows, key_in_map).map_err(err)?,
        }
        self.write_governance(doc, &before, key, now)
    }
}

impl PeerGovernance<'_> {
    /// Every attestor the workspace trusts, by id.
    #[must_use]
    pub fn trusted_attestors(self) -> Vec<TrustAttestor> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Vec::new();
        };
        let Ok(Some((_, rows))) = doc.get(ROOT, ATTESTORS) else {
            return Vec::new();
        };
        let mut out: Vec<TrustAttestor> = doc
            .keys(&rows)
            .filter_map(|id| {
                read_row::<TrustAttestor>(&**doc, &rows, &id).filter(|row| row.id == id)
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Every publisher key the workspace refuses.
    #[must_use]
    pub fn revoked_publishers(self) -> Vec<RevokedPublisher> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Vec::new();
        };
        let Ok(Some((_, rows))) = doc.get(ROOT, REVOKED) else {
            return Vec::new();
        };
        let mut out: Vec<RevokedPublisher> = doc
            .keys(&rows)
            .filter_map(|key| read_row(&**doc, &rows, &key))
            .filter(|row: &RevokedPublisher| valid_pubkey(&row.pubkey_hex))
            .collect();
        out.sort_by(|a, b| a.pubkey_hex.cmp(&b.pubkey_hex));
        out
    }
}

/// The row kept under `key`, if it reads as a row of its map.
fn read_row<T: serde::de::DeserializeOwned>(
    doc: &impl ReadDoc,
    rows: &automerge::ObjId,
    key: &str,
) -> Option<T> {
    match doc.get(rows, key).ok()?? {
        (automerge::Value::Scalar(value), _) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => tilo_failure::ignored!(
                "a trust row that does not decode is no row",
                serde_json::from_slice::<T>(bytes).ok()
            ),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `attestor` is a row a workspace may keep: an id and a key, the key
/// 64 hex characters, and a scope spelling one of the three forms.
#[must_use]
pub(crate) fn valid_attestor(attestor: &TrustAttestor) -> bool {
    let id = attestor.id.trim();
    !id.is_empty()
        && id.len() <= 128
        && !id.chars().any(char::is_control)
        && valid_pubkey(&attestor.pubkey_hex)
        && valid_scope(&attestor.scope)
}

/// Whether `pubkey` is an Ed25519 key in hex.
#[must_use]
pub(crate) fn valid_pubkey(pubkey: &str) -> bool {
    pubkey.len() == 64
        && !pubkey.eq_ignore_ascii_case("0".repeat(64).as_str())
        && pubkey.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether `scope` is `all`, or a non-empty `mime:` or `prefix:` list.
#[must_use]
pub(crate) fn valid_scope(scope: &str) -> bool {
    let scope = scope.trim();
    ["mime:", "prefix:"]
        .iter()
        .find_map(|prefix| scope.strip_prefix(prefix))
        .map_or(scope == "all", |items| {
            !items.is_empty()
                && items.split(',').all(|item| {
                    !item.trim().is_empty() && !item.trim().chars().any(char::is_control)
                })
        })
}
