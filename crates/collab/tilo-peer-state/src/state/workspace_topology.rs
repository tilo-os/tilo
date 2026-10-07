//! Governor-owned namespace registration for a currently admitted writer.
use super::{
    Actor, AutoCommit, CapDecision, ObjType, ROOT, ReadDoc, ResourceRef, StatePeer, SystemTime,
    Verb, decide_capability,
};
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

const MAX_ROOM_NAMESPACES: usize = 4096;
const MAX_DEVICE_NAMESPACES: usize = 16;

impl StatePeer {
    /// Register `salt` to this exact current admitted writer using governor custody.
    /// The caller supplies no owner identity. Existing ownership is immutable;
    /// a removed device cannot replay even its own previously successful claim.
    ///
    /// # Errors
    /// Refuses zero salts, stale admission, write denial, unavailable governor
    /// custody, malformed registries, another owner and exhausted namespace budgets.
    pub fn claim_workspace_namespace(
        &mut self,
        caller: &DeviceCert,
        salt: u16,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if salt == 0 || self.certificate(&caller.device_public_key, now)? != caller {
            return Err("namespace caller is not exact current admitted evidence".into());
        }
        let (role, caps, person) = self.permissions(caller.device_public_key, now)?;
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "namespace clock precedes Unix epoch")?
            .as_secs();
        let actor = Actor::person(Some(role), person).in_room(self.core.room.clone());
        if decide_capability(
            &actor,
            &ResourceRef::Room(self.core.room.clone()),
            Verb::Write,
            &caps,
            Some(seconds),
        ) != CapDecision::Allow
        {
            return Err("namespace caller cannot currently write this room".into());
        }
        self.require_governance_writer(key, now)?;
        let before = self.admitted_room().ok_or("namespace room is absent")?;
        let value = serde_json::to_value(automerge::AutoSerde::from(before))
            .map_err(|error| error.to_string())?;
        let owners = crate::topology::owners(&value)?;
        let device = hex::encode(caller.device_public_key);
        if let Some(owner) = owners.get(&salt) {
            return if owner == &device {
                Ok(false)
            } else {
                Err("topology namespace already belongs to another device".into())
            };
        }
        if owners.len() >= MAX_ROOM_NAMESPACES
            || owners.values().filter(|owner| *owner == &device).count() >= MAX_DEVICE_NAMESPACES
        {
            return Err("topology namespace capacity exhausted".into());
        }
        let mut candidate: AutoCommit = before.clone();
        candidate.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let registry = match candidate
            .get(ROOT, "contents_owners_v1")
            .map_err(|error| error.to_string())?
        {
            Some((automerge::Value::Object(ObjType::Map), registry)) => registry,
            None => candidate
                .put_object(ROOT, "contents_owners_v1", ObjType::Map)
                .map_err(|error| error.to_string())?,
            _ => return Err("invalid topology ownership registry".into()),
        };
        candidate
            .put(registry, format!("{salt:04x}"), device)
            .map_err(|error| error.to_string())?;
        candidate.commit();
        self.core.room_doc = Some(candidate.into());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn claimed_writer_sync_updates_only_its_namespace() {
        use crate::NeighbourMsg;
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        assert!(
            peer.claim_workspace_namespace(&caller, 7, &governor, now)
                .unwrap()
        );
        let changes = |peer: &crate::StatePeer, salt: u16| {
            let mut doc = peer.admitted_room().unwrap().clone();
            let heads = doc.get_heads();
            doc.set_actor(automerge::ActorId::from(caller.device_public_key.to_vec()));
            let contents = match doc.get(ROOT, "contents").unwrap() {
                Some((_, object)) => object,
                None => doc.put_object(ROOT, "contents", ObjType::Map).unwrap(),
            };
            let strip = doc
                .put_object(
                    contents,
                    format!("{:016x}", (u64::from(salt) << 48) | 1),
                    ObjType::Map,
                )
                .unwrap();
            doc.put(strip, "name", "My shared app").unwrap();
            doc.commit();
            NeighbourMsg::SyncResp {
                room: peer.room().into(),
                changes: doc
                    .get_changes(&heads)
                    .into_iter()
                    .map(|change| change.raw_bytes().to_vec())
                    .collect(),
            }
        };
        peer.receive(caller.device_public_key, changes(&peer, 7), now)
            .unwrap();
        assert!(
            peer.receive(caller.device_public_key, changes(&peer, 8), now)
                .is_err()
        );
        let value = serde_json::to_value(automerge::AutoSerde::from(peer.admitted_room().unwrap()))
            .unwrap();
        assert!(value["contents"].get("0007000000000001").is_some());
        assert!(value["contents"].get("0008000000000001").is_none());
    }

    #[test]
    fn namespace_budget_refuses_new_claims_without_eviction_or_rebinding() {
        let (mut peer, governor, _, caller, now) = crate::admission_tests::seeded();
        for salt in 1..=16 {
            assert!(
                peer.claim_workspace_namespace(&caller, salt, &governor, now)
                    .unwrap()
            );
        }
        assert!(
            peer.claim_workspace_namespace(&caller, 17, &governor, now)
                .is_err()
        );
        assert!(
            !peer
                .claim_workspace_namespace(&caller, 1, &governor, now)
                .unwrap()
        );
        let value = serde_json::to_value(automerge::AutoSerde::from(peer.admitted_room().unwrap()))
            .unwrap();
        let owners = crate::topology::owners(&value).unwrap();
        assert_eq!(owners.len(), 16);
        assert!(
            owners
                .values()
                .all(|owner| owner == &hex::encode(caller.device_public_key))
        );
        let mut forged = caller.clone();
        forged.device_label.push_str(" forged");
        assert!(
            peer.claim_workspace_namespace(&forged, 1, &governor, now)
                .is_err()
        );
    }
}
