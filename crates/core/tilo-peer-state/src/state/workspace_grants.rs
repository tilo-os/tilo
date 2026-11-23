//! The grants a workspace admitted, as its governance records them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err, governed_map};
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

/// Whether `doc` lists the key `device` speaks for as revoked by its person: read from that one
/// key's entry, without reading the others.
#[must_use]
pub fn revoked_key_in(doc: &automerge::AutoCommit, device: &[u8; 32]) -> bool {
    let Ok(Some((Value::Object(ObjType::Map), grants))) = doc.get(ROOT, WORKSPACE_GRANTS_KEY)
    else {
        return false;
    };
    let id = WorkspaceGrant::id_of(device);
    let Ok(Some((Value::Scalar(value), _))) = doc.get(&grants, id.as_str()) else {
        return false;
    };
    let automerge::ScalarValue::Str(json) = value.as_ref() else {
        return false;
    };
    serde_json::from_str::<WorkspaceGrant>(json).is_ok_and(|grant| grant.revoked_ms.is_some())
}

impl PeerGovernance<'_> {
    /// Whether the workspace lists the key `device` speaks for as revoked: such a key is no
    /// member here, whatever its certificate says, and every read, write and action it would make
    /// is refused, as its person's revocation of it says.
    #[must_use]
    pub fn revoked_key(self, device: &[u8; 32]) -> bool {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .is_some_and(|doc| revoked_key_in(doc, device))
    }

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
            let grants = governed_map(doc, WORKSPACE_GRANTS_KEY)?;
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

    /// A key its person approved acts as their agent in the workspace on
    /// every device holding it, whoever signs what it changes: the server's
    /// device here. Revoking its grant ends that with the next change.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn an_approved_key_acts_as_its_persons_agent_until_its_grant_is_revoked() -> TestResult {
        use ed25519_dalek::SigningKey;
        use tilo_action_policy::{Acting, ActionKind, Answer, Context, TreePath};
        use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};
        let (mut peer, governor, _, server, now) = crate::admission_tests::seeded();
        let root = SigningKey::from_bytes(&[1; 32]);
        let key = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key,
            device_label: "Visa backend".into(),
            capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            issued_at_ns: 1_000_000_000_000,
            // A day, as an agent's grant lasts at most a bounded while.
            expires_at_ns: Some(87_400_000_000_000),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root)?;
        peer.admit_certificate(cert, &governor, now)?;
        let person = hex::encode(root.verifying_key().to_bytes());
        let agent = tilo_action_policy::key_agent_id(&key);
        // The key's call, carried out and signed by another device of the
        // workspace's: the change names the key's agent, for its person.
        let invoker = Acting::handed_agent(agent, person);
        let node = TreePath::workspace(peer.workspace_id()).app("test.app");
        let decided = |peer: &crate::StatePeer| {
            peer.permissions(server.device_public_key, now)
                .map(|signer| {
                    signer.invoked_by(&invoker).answer(
                        &node,
                        ActionKind::Undoable,
                        &Context::at_secs(2000),
                    )
                })
                .map_err(|error| error.to_string())
        };
        assert_eq!(decided(&peer)?, Answer::Allow, "the approved key writes");
        let mut listed = grant(key);
        listed.id = WorkspaceGrant::id_of(&key);
        peer.governance_mut()
            .author_workspace_grant(&listed, &governor, now)?;
        assert_eq!(decided(&peer)?, Answer::Allow, "listing it changes nothing");
        peer.governance_mut()
            .author_workspace_grant_revoked(&listed.id, 42, &governor, now)?;
        assert_ne!(decided(&peer)?, Answer::Allow, "its revoked grant ends it");
        Ok(())
    }
}
