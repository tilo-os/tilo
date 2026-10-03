//! Whether a certified device may act for the identity a workspace selected.

use tilo_user_identity::DeviceCert;

/// `cert` belongs to the identity selected for `room` and its device is not
/// revoked.
///
/// # Errors
///
/// Returns a message when no identity is selected, the stores cannot be read,
/// or the certificate's identity or device does not qualify.
pub fn authorize(room: &str, cert: &DeviceCert) -> Result<(), String> {
    let profile = crate::registry::selected_identity_profile(room)
        .map_err(|e| e.to_string())?
        .ok_or("workspace identity unavailable")?;
    let fleet = crate::device_fleet::DeviceFleetStore::from_env()
        .and_then(|store| store.load(&profile.id))
        .map_err(|e| e.to_string())?;
    if cert.user_public_key != profile.user_public_key || fleet.is_revoked(&cert.device_public_key)
    {
        return Err("device revoked or identity mismatched".into());
    }
    Ok(())
}

/// [`authorize`], and the device is enrolled in the identity's fleet.
///
/// # Errors
///
/// Returns any [`authorize`] error, or a message for an unenrolled device.
pub fn authorize_join(room: &str, cert: &DeviceCert) -> Result<(), String> {
    authorize(room, cert)?;
    let profile = crate::registry::selected_identity_profile(room)
        .map_err(|e| e.to_string())?
        .ok_or("workspace identity unavailable")?;
    let fleet = crate::device_fleet::DeviceFleetStore::from_env()
        .and_then(|store| store.load(&profile.id))
        .map_err(|e| e.to_string())?;
    if !fleet
        .certificates
        .iter()
        .any(|known| known.device_public_key == cert.device_public_key)
    {
        return Err("certified collaboration device is not enrolled".into());
    }
    Ok(())
}
