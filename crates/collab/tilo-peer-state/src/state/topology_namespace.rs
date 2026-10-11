//! Governor-only registration of a currently admitted writer's own windows.
use super::{
    Actor, AutoCommit, CapDecision, ObjType, ROOT, ReadDoc, ResourceRef, StatePeer, SystemTime,
    Verb, decide_capability, err, now_secs,
};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

impl StatePeer {
    /// Register `salt` only to the authenticated caller's current admitted device.
    /// The transport must bind that certificate independently to its endpoint.
    /// This grants no room, app, record, input or enrollment permission.
    ///
    /// # Errors
    /// Refuses stale certificates, missing membership, denied workspace Write,
    /// zero/conflicting namespaces or unavailable pinned governor custody.
    pub fn claim_topology_namespace(
        &mut self,
        caller: &DeviceCert,
        salt: u16,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if salt == 0 {
            return Err("topology namespace must be nonzero".into());
        }
        if self
            .core
            .certificates
            .get(&caller.device_public_key)
            .map(AsRef::as_ref)
            != Some(caller)
        {
            return Err("topology caller is not current admitted evidence".into());
        }
        let (role, caps, person) = self.permissions(caller.device_public_key, now)?;
        let actor = Actor::person(Some(role), person).in_room(self.core.room.clone());
        if decide_capability(
            &actor,
            &ResourceRef::Workspace,
            Verb::Write,
            &caps,
            Some(now_secs(now)),
        ) != CapDecision::Allow
        {
            return Err("topology namespace requires current workspace Write".into());
        }
        self.require_governance_writer(key, now)?;
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
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
        let before = self
            .admitted_room()
            .map(AutoCommit::base_heads)
            .ok_or("room governance missing")?;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        doc.put(&owners, name, device).map_err(err)?;
        doc.commit();
        self.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::caps::{CapOverride, Scope, Subject};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn topology_namespace_is_durable_exact_and_never_rebound() -> TestResult {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(peer.claim_topology_namespace(&caller, 42, &governor, now)?);
        let stored = peer.durable()?;
        assert!(!peer.claim_topology_namespace(&caller, 42, &governor, now)?);
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
            peer.claim_topology_namespace(&caller, 0, &governor, now)
                .is_err()
        );
        assert!(
            peer.claim_topology_namespace(&caller, 42, &caller_key, now)
                .is_err()
        );
        let mut substitute = caller.clone();
        substitute.device_label.push('!');
        tilo_user_identity::sign_device_cert(&mut substitute, &SigningKey::from_bytes(&[1; 32]))?;
        assert!(
            peer.claim_topology_namespace(&substitute, 42, &governor, now)
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
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(peer.claim_topology_namespace(&caller, 42, &governor, now)?);
        let (_, _, person) = peer.permissions(caller.device_public_key, now)?;
        peer.put_governed_capabilities(
            &[CapOverride::new(
                Subject::person(person),
                Scope::Workspace,
                collab_types::caps::Effect::Restrict,
                Verb::Read,
            )
            .with_id("fixture-topology-deny")],
            &governor,
            now,
        )?;
        let denied = peer.durable()?;
        assert!(
            peer.claim_topology_namespace(&caller, 42, &governor, now)
                .is_err()
        );
        assert!(
            peer.claim_topology_namespace(&caller, 43, &governor, now)
                .is_err()
        );
        assert_eq!(denied, peer.durable()?);
        Ok(())
    }
}
