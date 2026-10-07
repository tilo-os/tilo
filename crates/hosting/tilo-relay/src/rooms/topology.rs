//! Governed publication of authenticated members' owned workspace topology.
use super::{Rooms, SystemTime};
use tilo_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};

fn governance(peer: &StatePeer) -> Result<serde_json::Value, String> {
    let doc = peer.admitted_room().ok_or("topology room is absent")?;
    let mut value =
        serde_json::to_value(automerge::AutoSerde::from(doc)).map_err(|error| error.to_string())?;
    value
        .as_object_mut()
        .ok_or("topology room is not an object")?
        .remove("contents");
    Ok(value)
}

impl Rooms {
    /// The transport supplies the certified author. `StatePeer` rechecks current
    /// admission, room write capability and ownership of every changed entry.
    /// Only contents may change here; namespace registration and governance stay
    /// exclusively behind the governor's authenticated command service.
    pub(super) async fn absorb_member_topology(
        &self,
        room: &str,
        author: [u8; 32],
        message: NeighbourMsg,
    ) -> Result<(), String> {
        if self.revocations().is_revoked(&author) {
            return Err("topology author is revoked".into());
        }
        let host = &self.inner.host;
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let mut governed = governed.lock().await;
        let now = SystemTime::now();
        let mut candidate = governed.peer().clone();
        let report =
            candidate.receive_from(TransportOrigin::RetainedRelayDelta(author), message, now)?;
        if governance(&candidate)? != governance(governed.peer())? {
            return Err("member topology cannot change room governance".into());
        }
        if report.changed {
            governed.commit(candidate, &host.key, now, |record, _| {
                Ok(record.collaboration.clone())
            })?;
        }
        let settled = self.settle(&mut governed).await;
        drop(governed);
        settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

    fn update(peer: &StatePeer, author: [u8; 32], salt: u16, rename: bool) -> NeighbourMsg {
        let mut doc = peer.admitted_room().unwrap().clone();
        let heads = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(author.to_vec()));
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
        doc.put(strip, "name", "Shared native app").unwrap();
        if rename {
            doc.put(ROOT, "name", "Member must not rename the governed room")
                .unwrap();
        }
        doc.commit();
        NeighbourMsg::SyncResp {
            room: peer.room().into(),
            changes: doc
                .get_changes(&heads)
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
        }
    }

    async fn start_relay(
        path: &std::path::Path,
        key: &ed25519_dalek::SigningKey,
        cert: &tilo_user_identity::DeviceCert,
    ) -> crate::daemon::Relay {
        crate::daemon::Relay::start_with_relay_mode(
            crate::RelayConfig {
                state_root: path.into(),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key.clone(),
                device_cert: cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn member_topology_is_durable_and_cannot_mutate_other_namespaces_or_governance() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = super::super::tests::governor();
        let mut relay = Some(start_relay(dir.path(), &key, &cert).await);
        let rooms = relay.as_ref().unwrap().rooms().unwrap();
        let created = rooms.create("Owned topology").await.unwrap();
        let member_key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let mut member = cert;
        member.device_public_key = member_key.verifying_key().to_bytes();
        member.capabilities = DeviceCapabilities::member();
        sign_device_cert(
            &mut member,
            &ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
        )
        .unwrap();
        let governed = rooms.room(&created.room).await.unwrap();
        let (valid, wrong_namespace, rename, path, initial_generation) = {
            let mut room = governed.lock().await;
            let now = SystemTime::now();
            let mut candidate = room.peer().clone();
            candidate
                .admit_certificate(member.clone(), &key, now)
                .unwrap();
            candidate
                .claim_workspace_namespace(&member, 7, &key, now)
                .unwrap();
            room.commit(candidate, &key, now, |record, _| {
                Ok(record.collaboration.clone())
            })
            .unwrap();
            (
                update(room.peer(), member.device_public_key, 7, false),
                update(room.peer(), member.device_public_key, 8, false),
                update(room.peer(), member.device_public_key, 7, true),
                room.path.clone(),
                room.record.generation,
            )
        };
        for refused in [wrong_namespace, rename] {
            assert!(
                rooms
                    .absorb(
                        &created.room,
                        member.device_public_key,
                        &postcard::to_stdvec(&refused).unwrap()
                    )
                    .await
                    .is_err()
            );
            assert_eq!(governed.lock().await.record.generation, initial_generation);
        }
        let other = rooms.create("Other room").await.unwrap();
        let body = postcard::to_stdvec(&valid).unwrap();
        assert!(
            rooms
                .absorb(&other.room, member.device_public_key, &body)
                .await
                .is_err()
        );
        rooms
            .absorb(&created.room, member.device_public_key, &body)
            .await
            .unwrap();
        let restored =
            super::super::GovernedRoom::open(path, &rooms.inner.host.cert, &key).unwrap();
        assert_eq!(restored.record.generation, initial_generation + 1);
        assert_eq!(restored.record.published, restored.record.outgoing.len());
        let value = serde_json::to_value(automerge::AutoSerde::from(
            restored.peer().admitted_room().unwrap(),
        ))
        .unwrap();
        assert_eq!(
            value["contents"]["0007000000000001"]["name"],
            "Shared native app"
        );
        assert!(value["contents"].get("0008000000000001").is_none());
        rooms
            .absorb(&created.room, member.device_public_key, &body)
            .await
            .unwrap();
        assert_eq!(
            governed.lock().await.record.generation,
            initial_generation + 1
        );
        drop(restored);
        drop(governed);
        relay.take().unwrap().shutdown().await;
        drop(relay);
    }
}
