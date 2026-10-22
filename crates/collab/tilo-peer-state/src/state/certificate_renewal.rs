//! A member's renewed device certificate. A certificate that expires is
//! renewed by its person's root before it runs out; the room keeps the
//! device as the same member, with the same role and grants, once its
//! governor replaces the certificate it holds with the renewed one.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{DeviceCert, GovernedIdentity, ROOT, ReadDoc, SigningKey, SystemTime, err, now_ns};
use automerge::transaction::Transactable as _;
use tilo_user_identity::DeviceCapabilities;

impl PeerGovernance<'_> {
    /// The certificate `renewed` replaces: the one the room holds for the
    /// same device under the same person's root, issued before it. `None`
    /// when the room holds `renewed` itself. Whether the certificate it
    /// replaces has expired does not matter: the person's root vouches for
    /// the device again.
    ///
    /// # Errors
    ///
    /// The room holds no certificate for the device, holds one under
    /// another root or a newer one, or `renewed` grants more than it, does
    /// not verify at `now`, or names a revoked device.
    pub fn renewal_of(
        self,
        renewed: &DeviceCert,
        now: SystemTime,
    ) -> Result<Option<DeviceCert>, String> {
        let held = self
            .peer
            .core
            .certificates
            .get(&renewed.device_public_key)
            .map(|held| held.as_ref().clone())
            .ok_or("membership certificate is absent from governed state")?;
        if &held == renewed {
            return Ok(None);
        }
        if held.user_public_key != renewed.user_public_key {
            return Err("a renewed certificate comes from the root of the one it replaces".into());
        }
        if renewed.issued_at_ns <= held.issued_at_ns {
            return Err("the room holds a newer certificate for this device".into());
        }
        if !within(&renewed.capabilities, &held.capabilities) {
            return Err("a renewed certificate grants no more than the one it replaces".into());
        }
        tilo_user_identity::verify_device_cert_at(renewed, now_ns(now)).map_err(err)?;
        if self
            .peer
            .core
            .revocations
            .is_revoked(&renewed.device_public_key)
        {
            return Err("device is revoked".into());
        }
        Ok(Some(held))
    }
}

impl PeerGovernanceMut<'_> {
    /// Replace the certificate the room holds for `renewed`'s device with
    /// `renewed` ([`PeerGovernance::renewal_of`]), keeping everything else
    /// the room records of the device. Returns whether governance changed.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, `renewed`
    /// does not renew the device's certificate, or governance cannot be
    /// updated.
    pub fn renew_member_certificate(
        self,
        renewed: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let Some(held) = self.peer.governance().renewal_of(renewed, now)? else {
            return Ok(false);
        };
        let mut doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let rows = super::room::governance_of(Some(&doc)).identities;
        let before = doc.get_heads();
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("governed identities missing")?
            .1;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let mut changed = false;
        for (id, bytes) in rows {
            let mut identity: GovernedIdentity = serde_json::from_slice(&bytes).map_err(err)?;
            if identity.device_cert != held {
                continue;
            }
            identity.device_cert = renewed.clone();
            let encoded = serde_json::to_vec(&identity).map_err(err)?;
            doc.put(&identities, id, automerge::ScalarValue::Bytes(encoded))
                .map_err(err)?;
            changed = true;
        }
        if !changed {
            return Err("the renewed device has no governed identity".into());
        }
        doc.commit();
        self.peer.apply_governance_doc(doc, &before, key, now)?;
        Ok(true)
    }
}

/// Whether `renewed` renews `held`: a later certificate of the same device
/// from the same person's root, granting nothing `held` does not. Every
/// replica takes it in `held`'s place; its signature and validity are
/// checked where it is taken in.
pub(crate) fn renews(renewed: &DeviceCert, held: &DeviceCert) -> bool {
    renewed.device_public_key == held.device_public_key
        && renewed.user_public_key == held.user_public_key
        && renewed.issued_at_ns > held.issued_at_ns
        && within(&renewed.capabilities, &held.capabilities)
}

/// Whether `renewed` grants nothing `held` does not.
fn within(renewed: &DeviceCapabilities, held: &DeviceCapabilities) -> bool {
    let within = |renewed: bool, held: bool| !renewed || held;
    within(renewed.can_own_workspaces, held.can_own_workspaces)
        && within(renewed.can_admit_devices, held.can_admit_devices)
        && within(renewed.can_act_as_host, held.can_act_as_host)
        && within(
            renewed.can_read_all_workspaces,
            held.can_read_all_workspaces,
        )
        && within(renewed.can_administer_devices, held.can_administer_devices)
        && (renewed.foreign || !held.foreign)
}

#[cfg(test)]
mod tests;
