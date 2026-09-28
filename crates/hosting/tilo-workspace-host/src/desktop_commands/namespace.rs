//! A member device claiming its own topology namespace in the room this host
//! governs: the registry entry that lets it publish its own windows (a
//! session's legacy windows), which this host then admits like any other
//! owned-content change and republishes to every member.
use super::*;
use tilo_peer_state::owner_commit::CommitBinding;
use tilo_user_identity::DeviceCert;

/// Write `salt` → `caller` into `doc`'s topology registry. A namespace is
/// never rebound, the governor's own is never given away, and a device holds
/// at most one.
pub(super) fn register(
    doc: &mut AutoCommit,
    caller: &DeviceCert,
    governor: &DeviceCert,
    salt: u16,
) -> Result<(), String> {
    use ui::automerge::ObjType;
    if salt == 0 {
        return Err("salt 0 names no topology namespace".into());
    }
    if caller.device_public_key == governor.device_public_key {
        return Err("the governor already owns its topology namespace".into());
    }
    let owners = match doc
        .get(
            ROOT,
            tilo_desktop_model::projection::ROOM_CONTENTS_OWNERS_KEY,
        )
        .map_err(|e| e.to_string())?
    {
        Some((_, owners)) => owners,
        None => doc
            .put_object(
                ROOT,
                tilo_desktop_model::projection::ROOM_CONTENTS_OWNERS_KEY,
                ObjType::Map,
            )
            .map_err(|e| e.to_string())?,
    };
    let key = format!("{salt:04x}");
    let device = hex::encode(caller.device_public_key);
    let registered = |doc: &AutoCommit, key: &str| -> Option<String> {
        doc.get(&owners, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
    };
    match registered(doc, &key) {
        Some(owner) if owner == device => {
            return Err("the topology namespace is already this device's".into());
        }
        Some(_) => return Err("the topology namespace belongs to another device".into()),
        None => {}
    }
    if doc
        .keys(&owners)
        .any(|other| registered(doc, &other).as_deref() == Some(device.as_str()))
    {
        return Err("this device already holds a topology namespace".into());
    }
    doc.put(&owners, key, device).map_err(|e| e.to_string())?;
    Ok(())
}

impl SessionOwner {
    /// Commit [`DesktopCommand::ClaimNamespace`] for `caller`, whose room
    /// write the caller policy already checked.
    pub(super) fn commit_namespace_claim(
        &mut self,
        request: &OwnerCommitRequest,
        binding: CommitBinding,
        caller: &DeviceCert,
        salt: u16,
        time: Option<&PolicyTime>,
    ) -> Result<OwnerCommitReceipt, DesktopCommandError> {
        let rejected = DesktopCommandError::Rejected;
        let mut doc = self
            .peer()
            .admitted_room()
            .ok_or_else(|| rejected("room governance missing".into()))?
            .clone();
        let base_heads = doc.get_heads();
        doc.set_actor(ui::automerge::ActorId::from(
            self.identity.native_cert.device_public_key.to_vec(),
        ));
        register(&mut doc, caller, &self.identity.native_cert, salt).map_err(rejected)?;
        doc.commit();
        let mut candidate = self.peer().clone();
        candidate
            .receive_from(
                tilo_peer_state::TransportOrigin::RelayDelta(
                    self.identity.native_cert.device_public_key,
                ),
                tilo_peer_state::NeighbourMsg::SyncResp {
                    room: request.room.clone(),
                    changes: doc
                        .get_changes(&base_heads)
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                },
                crate::owner::policy_fence::sample(time).map_err(rejected)?,
            )
            .map_err(rejected)?;
        let committed = self
            .commit_desktop_creation_candidate(candidate, None, &[], request, time)
            .map_err(
                |e| match e.downcast_ref::<crate::owner::OwnerPolicyRefused>() {
                    Some(refused) => DesktopCommandError::Rejected(refused.0.clone()),
                    None => DesktopCommandError::Uncertain(e.to_string()),
                },
            )?;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: committed.generation,
            aggregate_hash: hex::decode(committed.aggregate_hash)
                .map_err(|e| DesktopCommandError::Uncertain(e.to_string()))?
                .try_into()
                .map_err(|_| DesktopCommandError::Uncertain("invalid aggregate hash".into()))?,
        };
        receipt
            .validate_for(request)
            .map_err(DesktopCommandError::Uncertain)?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "in a test these are the assertion")]
mod tests {
    use super::*;

    fn cert(device: u8) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [device; 32],
            device_label: "device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        }
    }

    #[test]
    fn a_member_claims_one_free_namespace_and_nothing_is_ever_rebound() {
        let (governor, session, other) = (cert(9), cert(2), cert(3));
        let mut doc = AutoCommit::new();
        register(&mut doc, &session, &governor, 0x0042).unwrap();
        assert_eq!(
            tilo_desktop_model::projection::contents_owners_of(&doc).get(&0x0042),
            Some(&hex::encode([2; 32]))
        );
        let refusal = |doc: &mut AutoCommit, who: &DeviceCert, salt| {
            register(doc, who, &governor, salt).unwrap_err()
        };
        assert!(refusal(&mut doc, &session, 0x0042).contains("already this device's"));
        assert!(refusal(&mut doc, &other, 0x0042).contains("another device"));
        assert!(refusal(&mut doc, &session, 0x0043).contains("already holds"));
        assert!(refusal(&mut doc, &other, 0).contains("salt 0"));
        assert!(refusal(&mut doc, &governor, 0x0044).contains("governor"));
        register(&mut doc, &other, &governor, 0x0043).unwrap();
    }
}
