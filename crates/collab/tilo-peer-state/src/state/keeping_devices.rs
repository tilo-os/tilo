//! The hosts serving a workspace beside its owner, written by the workspace's
//! governance and read by every member.
//!
//! Each is a device of the owner's own person that serves the workspace as well
//! (a home relay the person chose for it), named by its own signed address, so a
//! member reaches the workspace while the owner's own device is away.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{ROOT, SystemTime, err};
use crate::join::TransportHint;
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc as _, Value};
use collab_types::signer::DeviceSigner;

/// Key of the workspace's hosts in its governance document: a map from each
/// host's device key (hex) to its signed address (JSON).
pub const WORKSPACE_KEEPING_DEVICES_KEY: &str = "keeping_devices_v1";

/// The hosts `doc` names, ordered by device key. An entry this build cannot
/// read, or one its host did not sign, is left out: whoever wrote the list,
/// only a host's own signature names where it answers.
#[must_use]
pub fn keeping_devices_of(doc: &automerge::AutoCommit) -> Vec<TransportHint> {
    let Ok(Some((Value::Object(ObjType::Map), hosts))) =
        doc.get(ROOT, WORKSPACE_KEEPING_DEVICES_KEY)
    else {
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
            (hex::encode(hint.host.device_public_key) == device
                && tilo_workspace_join::verify_transport_hint(&hint).is_ok())
            .then_some(hint)
        })
        .collect()
}

impl PeerGovernance<'_> {
    /// The hosts serving the workspace beside its governor, as its governance
    /// names them.
    #[must_use]
    pub fn keeping_devices(self) -> Vec<TransportHint> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map_or_else(Vec::new, keeping_devices_of)
    }
}

impl PeerGovernanceMut<'_> {
    /// Name `hint`'s host among the workspace's hosts at that address, authored
    /// by `key`; it replaces the host's earlier address. Naming it as it is
    /// named changes nothing, and an address its host did not sign is
    /// refused. Returns whether it changed the workspace.
    pub fn author_keeping_device(
        self,
        hint: &TransportHint,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        tilo_workspace_join::verify_transport_hint(hint)?;
        if self.peer.governance().keeping_devices().contains(hint) {
            return Ok(false);
        }
        let json = serde_json::to_string(hint).map_err(|error| error.to_string())?;
        self.write(key, now, |doc| {
            let hosts = match doc.get(ROOT, WORKSPACE_KEEPING_DEVICES_KEY).map_err(err)? {
                Some((Value::Object(ObjType::Map), hosts)) => hosts,
                _ => doc
                    .put_object(ROOT, WORKSPACE_KEEPING_DEVICES_KEY, ObjType::Map)
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

    /// Name the host `device` among the workspace's hosts no more, authored by
    /// the governor `key`. Returns whether it changed the workspace.
    pub fn author_keeping_device_removed(
        self,
        device: &[u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        if !self
            .peer
            .governance()
            .keeping_devices()
            .iter()
            .any(|hint| &hint.host.device_public_key == device)
        {
            return Ok(false);
        }
        self.write(key, now, |doc| {
            let Some((Value::Object(ObjType::Map), hosts)) =
                doc.get(ROOT, WORKSPACE_KEEPING_DEVICES_KEY).map_err(err)?
            else {
                return Ok(());
            };
            doc.delete(&hosts, hex::encode(device).as_str())
                .map_err(err)
        })
    }

    /// Apply `change` to the workspace's governance as the governor `key`,
    /// for this module and the workspace's grants beside it.
    pub(super) fn write(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
        change: impl FnOnce(&mut automerge::AutoCommit) -> Result<(), String>,
    ) -> Result<bool, String> {
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("missing workspace")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        change(&mut doc)?;
        doc.commit();
        self.peer.write_governance(doc, &before, key, now)
    }
}

#[cfg(test)]
mod tests {
    use crate::join::TransportHint;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// `host`'s address `at`, signed by its device `key`.
    fn hint(
        host: &tilo_user_identity::DeviceCert,
        key: &ed25519_dalek::SigningKey,
        at: &str,
    ) -> Result<TransportHint, String> {
        tilo_workspace_join::sign_transport_hint("iroh", at, host.clone(), [3; 32], key)
    }

    /// The governor names a host and moves or removes it; every member's
    /// copy reads what it named. An address its host did not sign is refused.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn the_governor_names_the_workspaces_hosts() -> TestResult {
        let (mut peer, governor, nas_key, nas, now) = crate::admission_tests::seeded();
        assert!(peer.governance().keeping_devices().is_empty());
        let first = hint(&nas, &nas_key, "first")?;
        let forged = TransportHint {
            opaque_json: "elsewhere".into(),
            ..first.clone()
        };
        assert!(
            peer.governance_mut()
                .author_keeping_device(&forged, &governor, now)
                .is_err(),
            "only the host signs where it answers"
        );
        assert!(
            peer.governance_mut()
                .author_keeping_device(&first, &governor, now)?
        );
        assert!(
            !peer
                .governance_mut()
                .author_keeping_device(&first, &governor, now)?,
            "naming it as it is named changes nothing"
        );
        let moved = hint(&nas, &nas_key, "moved")?;
        assert!(
            peer.governance_mut()
                .author_keeping_device(&moved, &governor, now)?
        );
        assert_eq!(peer.governance().keeping_devices(), [moved]);
        assert!(peer.governance_mut().author_keeping_device_removed(
            &nas.device_public_key,
            &governor,
            now
        )?);
        assert!(peer.governance().keeping_devices().is_empty());
        Ok(())
    }

    /// Any device of the owner names the workspace's hosts, not only the one the
    /// workspace was made on; every receiver leaves out an entry its host did
    /// not sign, whoever wrote it.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "the regression returns setup errors and asserts actual production outcomes"
    )]
    fn another_device_of_the_owner_names_a_host() -> TestResult {
        use automerge::ReadDoc as _;
        use automerge::transaction::Transactable as _;
        let (mut peer, _, member_key, member, now) = crate::admission_tests::seeded();
        assert_eq!(member.user_public_key, peer.owner());
        let signed = hint(&member, &member_key, "mine")?;
        let forged = TransportHint {
            opaque_json: "elsewhere".into(),
            ..signed.clone()
        };
        for (entry, named) in [(forged, 0), (signed, 1)] {
            let mut doc = peer
                .admitted_workspace()
                .ok_or("workspace missing")?
                .clone();
            let before = doc.get_heads();
            doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
            let hosts = match doc.get(automerge::ROOT, super::WORKSPACE_KEEPING_DEVICES_KEY)? {
                Some((automerge::Value::Object(automerge::ObjType::Map), hosts)) => hosts,
                _ => doc.put_object(
                    automerge::ROOT,
                    super::WORKSPACE_KEEPING_DEVICES_KEY,
                    automerge::ObjType::Map,
                )?,
            };
            doc.put(
                &hosts,
                hex::encode(member.device_public_key).as_str(),
                serde_json::to_string(&entry)?.as_str(),
            )?;
            doc.commit();
            let changes = doc
                .get_changes(&before)
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect();
            let workspace = peer.workspace_id().to_owned();
            peer.receive_from(
                crate::TransportOrigin::RetainedServerDelta(member.device_public_key),
                crate::NeighbourMsg::SyncResp { changes, workspace },
                now,
            )?;
            assert_eq!(peer.governance().keeping_devices().len(), named);
        }
        Ok(())
    }
}
