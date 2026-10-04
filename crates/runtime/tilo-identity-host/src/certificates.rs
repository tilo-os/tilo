//! Whether a certified device may act for the identity a workspace selected.

use tilo_user_identity::{Authority, ChainPolicy, DeviceCert, verify_chain};

use crate::device_fleet::{DeviceFleet, DeviceFleetStore};

/// `cert` lets its device act for the identity selected for `room` now: the
/// identity's root signed it, it is inside its validity window, that root has
/// not revoked the device, and it grants [`Authority::Person`].
///
/// # Errors
///
/// Returns a message when no identity is selected, the stores cannot be read,
/// or any link of the chain fails.
pub fn authorize(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let (user, fleet) = selected_fleet(room)?;
    check(cert, &user, &fleet, crate::device_fleet::now_ns(), false)
}

/// [`authorize`], and the device is enrolled in the identity's fleet.
///
/// # Errors
///
/// Returns any [`authorize`] error, or a message for an unenrolled device.
pub fn authorize_join(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let (user, fleet) = selected_fleet(room)?;
    check(cert, &user, &fleet, crate::device_fleet::now_ns(), true)
}

fn selected_fleet(room: &str) -> Result<([u8; 32], DeviceFleet), String> {
    let profile = crate::registry::selected_identity_profile(room)
        .map_err(|e| e.to_string())?
        .ok_or("workspace identity unavailable")?;
    let fleet = DeviceFleetStore::from_env()
        .and_then(|store| store.load(&profile.id))
        .map_err(|e| e.to_string())?;
    Ok((profile.user_public_key, fleet))
}

fn check(
    cert: &DeviceCert,
    user_public_key: &[u8; 32],
    fleet: &DeviceFleet,
    now_ns: u64,
    require_enrolled: bool,
) -> Result<(), String> {
    let revocations = fleet.revocation_list();
    verify_chain(
        cert,
        &ChainPolicy {
            user_public_key,
            authority: Authority::Person,
            revocations: &revocations,
            now_ns,
        },
    )
    .map_err(|error| format!("device refused for this workspace's identity: {error}"))?;
    if require_enrolled
        && !fleet
            .certificates
            .iter()
            .any(|known| known.device_public_key == cert.device_public_key)
    {
        return Err("certified collaboration device is not enrolled".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, UserIdentity, sign_device_cert,
        sign_revocation_entry,
    };

    pub(crate) const NOW: u64 = 1_000_000;

    pub(crate) fn certify(
        root: &ed25519_dalek::SigningKey,
        device: &ed25519_dalek::SigningKey,
        capabilities: DeviceCapabilities,
        expires_at_ns: Option<u64>,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Board B".into(),
            capabilities,
            issued_at_ns: 100,
            expires_at_ns,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    pub(crate) fn revoke(root: &ed25519_dalek::SigningKey, device: [u8; 32]) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: NOW * 10,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    #[test]
    fn only_a_current_personal_certificate_of_the_selected_identity_is_authorized() {
        let (root, user) = UserIdentity::generate("Ada", 1);
        let (other_root, _) = UserIdentity::generate("Mallory", 1);
        let device = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let joined = certify(&root, &device, DeviceCapabilities::joined(), None);
        let mut fleet = DeviceFleet::new(hex::encode(user.public_key));
        let user = user.public_key;

        // Valid, but not yet enrolled: authorize passes, join does not.
        check(&joined, &user, &fleet, NOW, false).unwrap();
        assert!(check(&joined, &user, &fleet, NOW, true).is_err());
        fleet.certificates.push(joined.clone());
        check(&joined, &user, &fleet, NOW, true).unwrap();

        let expired = certify(&root, &device, DeviceCapabilities::joined(), Some(NOW));
        assert!(check(&expired, &user, &fleet, NOW, false).is_err());
        let host = certify(&root, &device, DeviceCapabilities::host(), None);
        assert!(check(&host, &user, &fleet, NOW, false).is_err());
        let foreign = certify(&other_root, &device, DeviceCapabilities::joined(), None);
        assert!(check(&foreign, &user, &fleet, NOW, false).is_err());
        let mut forged = joined.clone();
        forged.capabilities.can_admit_devices = true;
        assert!(check(&forged, &user, &fleet, NOW, false).is_err());

        // Another identity's revocation does not count; the person's own
        // does, whatever its timestamp.
        fleet
            .revocations
            .push(revoke(&other_root, joined.device_public_key));
        check(&joined, &user, &fleet, NOW, true).unwrap();
        fleet
            .revocations
            .push(revoke(&root, joined.device_public_key));
        let refusal = check(&joined, &user, &fleet, NOW, true).unwrap_err();
        assert!(refusal.contains("revoked"), "{refusal}");
    }
}
