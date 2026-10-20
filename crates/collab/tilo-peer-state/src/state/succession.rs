//! A room whose governing device its root revoked follows the device the root
//! named in its place.
use super::{StatePeer, SystemTime, VerifiedCertificate, now_ns};
use std::collections::BTreeSet;
use tilo_user_identity::GovernorSuccession;

impl StatePeer {
    /// Move this room's governance to the device its governor's root named in
    /// the revoked governor's place, following `successions` from the current
    /// authority by [`tilo_user_identity::current_governor`]'s rule: every
    /// statement verifies now and is the authority's own root's; where one
    /// device has two successors the lowest key governs; a chain stops at a
    /// device seen before. Each revocation on the way is recorded, so the
    /// revoked devices' governance is refused from then on, and the
    /// successor's admitted. Returns whether the authority moved.
    ///
    /// The authority is an input of [`Self::restore`], not part of the
    /// durable state: whoever keeps this peer keeps the successions too.
    /// Governance the revoked device wrote before stays for the successor
    /// to replace, durable receipts included.
    ///
    /// # Errors
    ///
    /// Returns an error if the successor's certificate or a revocation on the
    /// way is refused; nothing changes then.
    pub fn follow_successions(
        &mut self,
        successions: &[GovernorSuccession],
        now: SystemTime,
    ) -> Result<bool, String> {
        let at = now_ns(now);
        let root = self.core.authority.user_public_key;
        let verified: Vec<&GovernorSuccession> = successions
            .iter()
            .filter(|succession| {
                succession.previous.user_public_key == root && succession.verify_at(at).is_ok()
            })
            .collect();
        let mut current = self.core.authority.device_public_key;
        let mut seen = BTreeSet::from([current]);
        let mut chain = Vec::new();
        for _ in 0..tilo_user_identity::MAX_SUCCESSIONS {
            let Some(next) = verified
                .iter()
                .filter(|succession| succession.previous.device_public_key == current)
                .min_by_key(|succession| succession.successor.device_public_key)
            else {
                break;
            };
            if !seen.insert(next.successor.device_public_key) {
                break;
            }
            current = next.successor.device_public_key;
            chain.push(*next);
        }
        let Some(last) = chain.last() else {
            return Ok(false);
        };
        let mut candidate = self.clone();
        for succession in &chain {
            candidate
                .core
                .revocations
                .insert(succession.revocation.clone())
                .map_err(|error| error.to_string())?;
        }
        let successor = last.successor.clone();
        if candidate
            .core
            .revocations
            .is_revoked(&successor.device_public_key)
        {
            return Err("the successor is revoked too".into());
        }
        candidate.core.certificates.insert(
            successor.device_public_key,
            VerifiedCertificate::new(successor.clone(), at)?,
        );
        candidate.core.authority = successor;
        *self = candidate;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NeighbourMsg, SigningKey, StatePeer, SystemTime, TransportOrigin};
    use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, GovernorSuccession, RevocationEntry, RevocationList,
        RevocationReason, sign_device_cert, sign_revocation_entry,
    };

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";

    fn certificate(root: &SigningKey, device: &SigningKey) -> DeviceCert {
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, root).unwrap();
        certificate
    }

    fn succession(
        root: &SigningKey,
        previous: &DeviceCert,
        successor: &DeviceCert,
    ) -> GovernorSuccession {
        let mut revocation = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: previous.device_public_key,
            revoked_at_ns: 2,
            reason: RevocationReason::Stolen,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, root).unwrap();
        GovernorSuccession::sign(previous.clone(), revocation, successor.clone(), root).unwrap()
    }

    /// A governed change `key` signs, naming the laptop `name`, delivered as
    /// the hop from that device.
    fn rename(peer: &mut StatePeer, key: &SigningKey, name: &str) -> Result<(), String> {
        let mut doc = peer
            .core
            .room_doc
            .as_deref()
            .ok_or("the room has no governance")?
            .clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let Ok(Some((automerge::Value::Object(_), peers))) = doc.get(ROOT, "peers") else {
            return Err("the room names no peers".into());
        };
        doc.put(&peers, "laptop", name)
            .map_err(|error| error.to_string())?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        peer.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                room: ROOM.into(),
                changes,
            },
            SystemTime::now(),
        )
        .map(|_| ())
    }

    #[test]
    fn a_room_follows_its_revoked_governors_successor() {
        let now = SystemTime::now();
        let root = SigningKey::from_bytes(&[1; 32]);
        let (laptop_key, desktop_key, tablet_key) = (
            SigningKey::from_bytes(&[2; 32]),
            SigningKey::from_bytes(&[3; 32]),
            SigningKey::from_bytes(&[4; 32]),
        );
        let (laptop, desktop, tablet) = (
            certificate(&root, &laptop_key),
            certificate(&root, &desktop_key),
            certificate(&root, &tablet_key),
        );
        let roster = vec![laptop.clone(), desktop.clone(), tablet.clone()];
        let mut peer = StatePeer::new(
            ROOM,
            laptop.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.install_seed(
            super::super::SeedState {
                room_changes: super::super::seed_room(ROOM, &laptop, &roster).unwrap(),
                documents: Vec::new(),
            },
            &laptop_key,
            now,
        )
        .unwrap();
        assert!(rename(&mut peer, &desktop_key, "not the governor").is_err());

        let stranger_root = SigningKey::from_bytes(&[9; 32]);
        let stranger = certificate(&stranger_root, &SigningKey::from_bytes(&[8; 32]));
        let foreign = succession(
            &stranger_root,
            &stranger,
            &certificate(&stranger_root, &tablet_key),
        );
        assert_eq!(
            peer.follow_successions(&[foreign], now),
            Ok(false),
            "another root's"
        );

        // Two successors named for the laptop: the lower key governs.
        let to_desktop = succession(&root, &laptop, &desktop);
        let to_tablet = succession(&root, &laptop, &tablet);
        let (successor, other) = if desktop.device_public_key < tablet.device_public_key {
            (&desktop_key, &tablet_key)
        } else {
            (&tablet_key, &desktop_key)
        };
        assert_eq!(
            peer.follow_successions(&[to_tablet, to_desktop], now),
            Ok(true)
        );
        assert_eq!(
            peer.authority().device_public_key,
            successor.verifying_key().to_bytes()
        );
        assert!(rename(&mut peer, &laptop_key, "from the stolen laptop").is_err());
        assert!(rename(&mut peer, other, "from the other device").is_err());
        rename(&mut peer, successor, "from the successor").unwrap();
        let names = super::super::room::governance_of(peer.core.room_doc.as_deref()).peers;
        assert!(
            names.values().any(|name| name == "from the successor"),
            "{names:?}"
        );
        assert_eq!(peer.follow_successions(&[], now), Ok(false));
    }
}
