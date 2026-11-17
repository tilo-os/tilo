//! The grants a workspace admitted, as its governance records them: each key
//! the person approved for an agent, a backend or a script, with the workspace
//! it reaches and until when.
//!
//! A grant is written by the host that admitted its key and read by every
//! holder of the workspace, so the grant exists wherever the workspace is
//! held and whichever front door serves it. It names the key by its device
//! public key, which is what the workspace's access admits and refuses; the
//! key's certificate remains the grant's proof, and this record is what the
//! workspace lists and revokes.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc as _, Value};
use collab_types::signer::DeviceSigner;
use serde::{Deserialize, Serialize};

/// Key of the workspace's grants in its governance document: a map from each
/// grant's id to the grant (JSON).
pub const WORKSPACE_GRANTS_KEY: &str = "grants_v1";

/// One grant the workspace admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGrant {
    /// The grant's id, the same at every front door that reads it.
    pub id: String,
    /// The person whose key it is, hex.
    pub person: String,
    /// The key's device public key, hex: the device the workspace admits.
    pub device: String,
    /// What the person approved it for, like "Visa backend".
    pub name: String,
    /// The workspace it reaches alone; `None` for every workspace of the
    /// person's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// When it stops working, Unix milliseconds; `None` until its person
    /// revokes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
    /// When the workspace admitted it, Unix milliseconds.
    pub created_ms: u64,
    /// When its person revoked it, Unix milliseconds; `None` while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_ms: Option<u64>,
}

impl WorkspaceGrant {
    /// The id a key's grant is known by wherever it is read: its device key's
    /// digest, so every front door and every holder derive the same grant
    /// from the same key.
    #[must_use]
    pub fn id_of(device_public_key: &[u8; 32]) -> String {
        format!(
            "grant-{}",
            hex::encode(&blake3::hash(device_public_key).as_bytes()[..12])
        )
    }
}

/// The grants `doc` records, ordered by id. An entry this build cannot read is
/// left out.
#[must_use]
pub fn workspace_grants_of(doc: &automerge::AutoCommit) -> Vec<WorkspaceGrant> {
    let Ok(Some((Value::Object(ObjType::Map), grants))) = doc.get(ROOT, WORKSPACE_GRANTS_KEY)
    else {
        return Vec::new();
    };
    doc.keys(&grants)
        .filter_map(|id| {
            let (Value::Scalar(value), _) = doc.get(&grants, id.as_str()).ok()?? else {
                return None;
            };
            let automerge::ScalarValue::Str(json) = value.as_ref() else {
                return None;
            };
            let grant: WorkspaceGrant = serde_json::from_str(json).ok()?;
            (grant.id == id).then_some(grant)
        })
        .collect()
}

impl PeerGovernance<'_> {
    /// The grants the workspace admitted, as its governance records them.
    #[must_use]
    pub fn workspace_grants(self) -> Vec<WorkspaceGrant> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map_or_else(Vec::new, workspace_grants_of)
    }
}

impl PeerGovernanceMut<'_> {
    /// Record `grant` among the workspace's grants, authored by the host
    /// `key`; recording it as it is recorded changes nothing. Returns whether
    /// it changed the workspace.
    ///
    /// # Errors
    /// The grant cannot be encoded, or the workspace's governance cannot be
    /// written.
    pub fn author_workspace_grant(
        self,
        grant: &WorkspaceGrant,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let held = self
            .peer
            .governance()
            .workspace_grants()
            .into_iter()
            .find(|held| held.id == grant.id);
        if held.as_ref() == Some(grant) {
            return Ok(false);
        }
        let json = serde_json::to_string(grant).map_err(|error| error.to_string())?;
        let id = grant.id.clone();
        self.write(key, now, |doc| {
            let grants = match doc.get(ROOT, WORKSPACE_GRANTS_KEY).map_err(err)? {
                Some((Value::Object(ObjType::Map), grants)) => grants,
                _ => doc
                    .put_object(ROOT, WORKSPACE_GRANTS_KEY, ObjType::Map)
                    .map_err(err)?,
            };
            doc.put(&grants, id.as_str(), json.as_str()).map_err(err)
        })
    }

    /// Mark the grant `id` revoked at `revoked_ms`, authored by the host
    /// `key`. Returns whether it changed the workspace.
    ///
    /// # Errors
    /// The workspace's governance cannot be written.
    pub fn author_workspace_grant_revoked(
        self,
        id: &str,
        revoked_ms: u64,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(mut grant) = self
            .peer
            .governance()
            .workspace_grants()
            .into_iter()
            .find(|held| held.id == id)
        else {
            return Ok(false);
        };
        if grant.revoked_ms.is_some() {
            return Ok(false);
        }
        grant.revoked_ms = Some(revoked_ms);
        self.author_workspace_grant(&grant, key, now)
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceGrant;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn grant(device: [u8; 32]) -> WorkspaceGrant {
        WorkspaceGrant {
            id: format!("grant-{}", hex::encode(&device[..6])),
            person: hex::encode([1; 32]),
            device: hex::encode(device),
            name: "Visa backend".into(),
            scope: None,
            expires_ms: None,
            created_ms: 1,
            revoked_ms: None,
        }
    }

    /// The host that admitted a key records its grant, every member's copy
    /// reads it, and its person's revocation is recorded once.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_host_records_the_grant_it_admitted_and_its_revocation() -> TestResult {
        let (mut peer, governor, _, _, now) = crate::admission_tests::seeded();
        assert!(peer.governance().workspace_grants().is_empty());
        let admitted = grant([7; 32]);
        assert!(
            peer.governance_mut()
                .author_workspace_grant(&admitted, &governor, now)?
        );
        assert_eq!(peer.governance().workspace_grants(), vec![admitted.clone()]);
        assert!(
            !peer
                .governance_mut()
                .author_workspace_grant(&admitted, &governor, now)?,
            "recording it as it is recorded changes nothing"
        );
        assert!(peer.governance_mut().author_workspace_grant_revoked(
            &admitted.id,
            42,
            &governor,
            now
        )?);
        let revoked = peer.governance().workspace_grants();
        assert_eq!(revoked[0].revoked_ms, Some(42));
        assert!(
            !peer.governance_mut().author_workspace_grant_revoked(
                &admitted.id,
                43,
                &governor,
                now
            )?,
            "a revoked grant stays revoked at the first time it was"
        );
        assert_eq!(
            peer.governance().workspace_grants().len(),
            1,
            "the workspace's copy holds the grant"
        );
        Ok(())
    }
}
