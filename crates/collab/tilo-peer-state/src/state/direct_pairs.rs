//! A pair record's two people: who reads `direct/<pair id>`, and the one write
//! that makes it theirs ([`crate::workspace::direct_pair`]).
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    DeviceCert, NeighbourMsg, ROOT, ReadDoc, SigningKey, StatePeer, SystemTime, Transactable,
    TransportOrigin, err, now_ns,
};
use crate::workspace::direct_pair::{DIRECT_PAIRS_KEY, DirectPair};

impl StatePeer {
    /// The two people the record `record` is shared between, when it is a pair
    /// record the workspace holds a proof of that lasts now: `None` for every
    /// other record, and for a pair record whose proof no longer holds.
    /// [`Self::check_document_read`] and [`Self::authorize_document`] decide
    /// [`collab_types::direct_pair`] records by it alone.
    #[must_use]
    pub fn direct_pair(&self, record: &str, now: SystemTime) -> Option<[String; 2]> {
        let pair = collab_types::direct_pair(record)?;
        let doc = self.core.workspace_doc.as_deref()?;
        let (root, map) = doc.get(ROOT, DIRECT_PAIRS_KEY).ok().flatten()?;
        if !matches!(root, automerge::Value::Object(_)) {
            return None;
        }
        let (value, _) = doc.get(&map, pair).ok().flatten()?;
        let automerge::Value::Scalar(value) = value else {
            return None;
        };
        let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
            return None;
        };
        let bytes = bytes.clone();
        let announcement: DirectPair = serde_json::from_slice(&bytes).ok()?;
        crate::workspace::direct_pair::proven_pair(
            &self.core.workspace,
            pair,
            &announcement,
            &self.core.revocations,
            now_ns(now),
        )
    }
}

impl PeerGovernance<'_> {
    /// Every pair this workspace holds a lasting proof of now, by pair id.
    #[must_use]
    pub fn direct_pairs(self, now: SystemTime) -> std::collections::BTreeMap<String, [String; 2]> {
        let Some(doc) = self.peer.core.workspace_doc.as_deref() else {
            return std::collections::BTreeMap::new();
        };
        crate::workspace::direct_pair::direct_pairs_of(doc)
            .into_iter()
            .filter_map(|(pair, bytes)| {
                let announcement: DirectPair = serde_json::from_slice(&bytes).ok()?;
                let people = crate::workspace::direct_pair::proven_pair(
                    &self.peer.core.workspace,
                    &pair,
                    &announcement,
                    &self.peer.core.revocations,
                    now_ns(now),
                )?;
                Some((pair, people))
            })
            .collect()
    }
}

impl PeerGovernanceMut<'_> {
    /// Record the pair this device's person shares with `other` in this
    /// workspace, signed by `key`, that device's own: from then on the pair's
    /// record ([`collab_types::direct_record`]) is read and written by the two
    /// people's devices and by no one else's.
    ///
    /// Answers whether the workspace changed: a pair it already holds stays as
    /// it is, so the first device to open a conversation names it for both.
    ///
    /// # Errors
    ///
    /// A caller that is not current admitted evidence or may not write here, a
    /// key that is not the caller's, or a full workspace.
    pub fn author_direct_pair(
        self,
        caller: &DeviceCert,
        other: &[u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if key.verifying_key().to_bytes() != caller.device_public_key {
            return Err("a pair is signed by its caller's own device".into());
        }
        if self
            .peer
            .certificate(&caller.device_public_key, now)
            .map_err(|error| error.to_string())?
            != caller
        {
            return Err("pair caller is not exact current admitted evidence".into());
        }
        self.peer.require_member_writer(key, now)?;
        let people = [hex::encode(caller.user_public_key), hex::encode(other)];
        let (_, announcement) = DirectPair::signed(&self.peer.core.workspace, people, caller, key)?;
        self.admit_direct_pair(&announcement, now)
    }

    /// Record `announcement`, a pair of this workspace's people one of their
    /// devices signed, as this workspace holds it: for a device whose key
    /// signs where it is kept (a page, whose custody holds it), and for a host
    /// the device asked to record what it signed. The proof is asked first, so
    /// a workspace records a pair only from a device of one of the two people.
    ///
    /// Answers whether the workspace changed: a pair it already holds stays as
    /// it is.
    ///
    /// # Errors
    ///
    /// A proof that does not hold, a signing device that is not exact current
    /// admitted evidence or may not write here, or a full workspace.
    pub fn admit_direct_pair(
        self,
        announcement: &DirectPair,
        now: SystemTime,
    ) -> Result<bool, String> {
        let device: DeviceCert = serde_json::from_value(announcement.proof.device_cert.clone())
            .map_err(|_| "a pair is proven by a device of one of its people".to_owned())?;
        let pair = collab_types::direct_pair_id(
            &self.peer.core.workspace,
            &announcement.people[0],
            &announcement.people[1],
        )
        .ok_or("a pair is two distinct people of this workspace")?;
        crate::workspace::direct_pair::proven_pair(
            &self.peer.core.workspace,
            &pair,
            announcement,
            &self.peer.core.revocations,
            now_ns(now),
        )
        .ok_or("a pair is proven by a device of one of its people")?;
        if self
            .peer
            .certificate(&device.device_public_key, now)
            .map_err(|error| error.to_string())?
            != &device
        {
            return Err("pair caller is not exact current admitted evidence".into());
        }
        self.peer
            .require_member_device(&device.device_public_key, now)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_for(&device.device_public_key));
        let map = match doc.get(ROOT, DIRECT_PAIRS_KEY).map_err(err)? {
            Some((automerge::Value::Object(automerge::ObjType::Map), map)) => map,
            Some(_) => return Err("the workspace's pairs are malformed".into()),
            None => doc
                .put_object(ROOT, DIRECT_PAIRS_KEY, automerge::ObjType::Map)
                .map_err(err)?,
        };
        if doc.get(&map, pair.as_str()).map_err(err)?.is_some() {
            return Ok(false);
        }
        if doc.length(&map) >= crate::workspace::direct_pair::MAX_DIRECT_PAIRS {
            return Err("the workspace holds as many pairs as it can".into());
        }
        doc.put(
            &map,
            pair.as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(announcement).map_err(err)?),
        )
        .map_err(err)?;
        doc.commit();
        let changes = doc
            .get_changes(&before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let workspace = self.peer.core.workspace.clone();
        self.peer.receive_from(
            TransportOrigin::RelayDelta(device.device_public_key),
            NeighbourMsg::SyncResp {
                room: workspace,
                changes,
            },
            now,
        )?;
        Ok(true)
    }
}

#[cfg(test)]
#[path = "direct_pairs_tests.rs"]
mod tests;
