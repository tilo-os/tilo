//! The grants a workspace admitted, as its governance records them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc as _, Value};
use collab_types::signer::DeviceSigner;
use collab_types::workspace_grant::{WORKSPACE_GRANTS_KEY, WorkspaceGrant};

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
            parent: None,
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
