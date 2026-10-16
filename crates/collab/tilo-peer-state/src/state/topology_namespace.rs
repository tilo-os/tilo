//! Governor-only registration of a currently admitted writer's own windows.
use super::facades::PeerGovernanceMut;
use super::{ObjType, ROOT, ReadDoc, SystemTime, err};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{ActionKind, TreePath};
use tilo_user_identity::DeviceCert;

impl PeerGovernanceMut<'_> {
    /// Register `salt` only to the authenticated caller's current admitted device.
    /// The transport must bind that certificate independently to its endpoint.
    /// This grants no room, app, record, input or enrollment permission.
    ///
    /// # Errors
    /// Refuses stale certificates, missing membership, a caller not granted
    /// undoable changes in the room, zero/conflicting namespaces or
    /// unavailable pinned governor custody.
    pub fn claim_topology_namespace(
        self,
        caller: &DeviceCert,
        salt: u16,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if salt == 0 {
            return Err("topology namespace must be nonzero".into());
        }
        if self.peer.certificate(&caller.device_public_key, now)? != caller {
            return Err("topology caller is not current admitted evidence".into());
        }
        let member = self.peer.permissions(caller.device_public_key, now)?;
        if !member.allows(
            &TreePath::room(&self.peer.core.room),
            ActionKind::Undoable,
            now,
        ) {
            return Err("topology namespace requires changing this workspace".into());
        }
        self.peer.require_governance_writer(key, now)?;
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let owners = match doc.get(ROOT, "contents_owners_v1").map_err(err)? {
            Some((automerge::Value::Object(ObjType::Map), object)) => object,
            Some(_) => return Err("topology ownership registry is malformed".into()),
            None => doc
                .put_object(ROOT, "contents_owners_v1", ObjType::Map)
                .map_err(err)?,
        };
        let name = format!("{salt:04x}");
        let device = hex::encode(caller.device_public_key);
        if let Some((existing, _)) = doc.get(&owners, &name).map_err(err)? {
            return if existing.as_str() == Some(device.as_str()) {
                Ok(false)
            } else {
                Err("topology namespace belongs to another device".into())
            };
        }
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(&owners, name, device).map_err(err)?;
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatePeer;
    use tilo_action_policy::{Answer, Grant, KindSet, Subject};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn topology_namespace_is_durable_exact_and_never_rebound() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, 42, &governor, now)?
        );
        let stored = peer.durable()?;
        assert!(
            !peer
                .governance_mut()
                .claim_topology_namespace(&caller, 42, &governor, now)?
        );
        assert_eq!(stored, peer.durable()?);
        let mut restored = StatePeer::restore(
            &stored,
            peer.room(),
            peer.authority().clone(),
            vec![],
            tilo_user_identity::RevocationList::new(),
            now,
        )?;
        assert_eq!(
            tilo_desktop_model::projection::contents_owners_of(
                restored.admitted_room().ok_or("room missing")?
            )
            .get(&42),
            Some(&hex::encode(caller.device_public_key))
        );
        let other = restored.authority().clone();
        assert!(
            restored
                .governance_mut()
                .claim_topology_namespace(&other, 42, &governor, now)
                .is_err()
        );
        assert_eq!(stored, restored.durable()?);
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn topology_namespace_requires_current_exact_evidence_and_governor_custody() -> TestResult {
        let (mut peer, governor, caller_key, caller, now) = crate::admission_tests::seeded();
        let before = peer.durable()?;
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, 0, &governor, now)
                .is_err()
        );
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, 42, &caller_key, now)
                .is_err()
        );
        let mut substitute = caller;
        substitute.device_label.push('!');
        tilo_user_identity::sign_device_cert(&mut substitute, &SigningKey::from_bytes(&[1; 32]))?;
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&substitute, 42, &governor, now)
                .is_err()
        );
        assert_eq!(before, peer.durable()?);
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn topology_namespace_retry_rechecks_deny_biased_write_before_observing_ownership() -> TestResult
    {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded_editor();
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, 42, &governor, now)?
        );
        let person = peer.permissions(caller.device_public_key, now)?.person;
        let room = peer.room().to_owned();
        peer.governance_mut().put_governed_grants(
            &[Grant::new(
                "fixture-topology-deny",
                Subject::Person(person),
                TreePath::room(&room),
                KindSet::ALL,
                Answer::Deny,
                Subject::person("fixture-governor"),
            )],
            &governor,
            now,
        )?;
        let denied = peer.durable()?;
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, 42, &governor, now)
                .is_err()
        );
        assert!(
            peer.governance_mut()
                .claim_topology_namespace(&caller, 43, &governor, now)
                .is_err()
        );
        assert_eq!(denied, peer.durable()?);
        Ok(())
    }
}
