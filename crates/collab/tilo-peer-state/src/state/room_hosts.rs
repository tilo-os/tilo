//! The hosts serving a room beside its governor, which its governor alone
//! writes and every member reads.
//!
//! Each is a host of the governor's own person that serves the room as well
//! (a home relay the person chose for it), named by its own signed address.
//! A member reaches the room at any of them: while the governor is away its
//! sessions go to one of these, and they come back to the governor when it
//! returns. Members learn of a host added after they joined as they learn of
//! any change to the room's governance.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use crate::join::TransportHint;
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc as _, Value};
use ed25519_dalek::SigningKey;

/// Key of the room's hosts in its governance document: a map from each
/// host's device key (hex) to its signed address (JSON).
pub const ROOM_HOSTS_KEY: &str = "hosts_v1";

/// The hosts `doc` names, ordered by device key. An entry this build cannot
/// read is left out.
#[must_use]
pub fn room_hosts_of(doc: &automerge::AutoCommit) -> Vec<TransportHint> {
    let Ok(Some((Value::Object(ObjType::Map), hosts))) = doc.get(ROOT, ROOM_HOSTS_KEY) else {
        return Vec::new();
    };
    doc.keys(&hosts)
        .filter_map(|device| {
            let (Value::Scalar(value), _) = doc.get(&hosts, device.as_str()).ok()?? else {
                return None;
            };
            let automerge::ScalarValue::Str(json) = value.as_ref() else {
                return None;
            };
            let hint: TransportHint = serde_json::from_str(json).ok()?;
            (hex::encode(hint.host.device_public_key) == device).then_some(hint)
        })
        .collect()
}

impl PeerGovernance<'_> {
    /// The hosts serving the room beside its governor, as its governance
    /// names them.
    #[must_use]
    pub fn room_hosts(self) -> Vec<TransportHint> {
        self.peer
            .core
            .room_doc
            .as_deref()
            .map_or_else(Vec::new, room_hosts_of)
    }
}

impl PeerGovernanceMut<'_> {
    /// Name `hint`'s host among the room's hosts at that address, authored
    /// by the governor `key`; it replaces the host's earlier address. Naming
    /// it as it is named changes nothing. Returns whether it changed the
    /// room.
    pub fn author_room_host(
        self,
        hint: &TransportHint,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        if self.peer.governance().room_hosts().contains(hint) {
            return Ok(false);
        }
        let json = serde_json::to_string(hint).map_err(|error| error.to_string())?;
        self.write(key, now, |doc| {
            let hosts = match doc.get(ROOT, ROOM_HOSTS_KEY).map_err(err)? {
                Some((Value::Object(ObjType::Map), hosts)) => hosts,
                _ => doc
                    .put_object(ROOT, ROOM_HOSTS_KEY, ObjType::Map)
                    .map_err(err)?,
            };
            doc.put(
                &hosts,
                hex::encode(hint.host.device_public_key).as_str(),
                json.as_str(),
            )
            .map_err(err)
        })
    }

    /// Name the host `device` among the room's hosts no more, authored by
    /// the governor `key`. Returns whether it changed the room.
    pub fn author_room_host_removed(
        self,
        device: &[u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        if !self
            .peer
            .governance()
            .room_hosts()
            .iter()
            .any(|hint| &hint.host.device_public_key == device)
        {
            return Ok(false);
        }
        self.write(key, now, |doc| {
            let Some((Value::Object(ObjType::Map), hosts)) =
                doc.get(ROOT, ROOM_HOSTS_KEY).map_err(err)?
            else {
                return Ok(());
            };
            doc.delete(&hosts, hex::encode(device).as_str())
                .map_err(err)
        })
    }

    /// Apply `change` to the room's governance as the governor `key`.
    fn write(
        self,
        key: &SigningKey,
        now: SystemTime,
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
    ) -> Result<bool, String> {
        let mut doc = self.peer.core.room_doc.clone().ok_or("missing room")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        change(&mut doc)?;
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use crate::join::TransportHint;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn hint(host: &tilo_user_identity::DeviceCert, at: &str) -> TransportHint {
        TransportHint {
            kind: "iroh".into(),
            opaque_json: at.into(),
            host: host.clone(),
            service_public_key: [3; 32],
            host_signature: [4; 64],
        }
    }

    /// The governor names a host and moves or removes it; every member's
    /// copy reads what it named.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn the_governor_names_the_rooms_hosts() -> TestResult {
        let (mut peer, governor, _, nas, now) = crate::admission_tests::seeded();
        assert!(peer.governance().room_hosts().is_empty());
        let first = hint(&nas, "first");
        assert!(
            peer.governance_mut()
                .author_room_host(&first, &governor, now)?
        );
        assert!(
            !peer
                .governance_mut()
                .author_room_host(&first, &governor, now)?,
            "naming it as it is named changes nothing"
        );
        let moved = hint(&nas, "moved");
        assert!(
            peer.governance_mut()
                .author_room_host(&moved, &governor, now)?
        );
        assert_eq!(peer.governance().room_hosts(), [moved]);
        assert!(peer.governance_mut().author_room_host_removed(
            &nas.device_public_key,
            &governor,
            now
        )?);
        assert!(peer.governance().room_hosts().is_empty());
        Ok(())
    }

    /// A member's change to the room's hosts is refused: only the governor
    /// names them.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn a_member_cannot_name_a_host() -> TestResult {
        use automerge::transaction::Transactable as _;
        let (mut peer, _, _, member, now) = crate::admission_tests::seeded();
        let mut doc = peer.admitted_room().ok_or("room missing")?.clone();
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
        let hosts = doc.put_object(
            automerge::ROOT,
            super::ROOM_HOSTS_KEY,
            automerge::ObjType::Map,
        )?;
        doc.put(
            &hosts,
            hex::encode(member.device_public_key).as_str(),
            serde_json::to_string(&hint(&member, "mine"))?.as_str(),
        )?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let room = peer.room().to_owned();
        let refused = peer.receive_from(
            crate::TransportOrigin::RetainedRelayDelta(member.device_public_key),
            crate::NeighbourMsg::SyncResp { changes, room },
            now,
        );
        assert!(refused.is_err(), "{refused:?}");
        assert!(peer.governance().room_hosts().is_empty());
        Ok(())
    }
}
