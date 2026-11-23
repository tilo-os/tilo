//! Enrolling a fresh image with the setup machine's own identity: the
//! machine's person approves the image's new device on the spot, without a
//! network exchange. The image's accounts own their files, so this exists
//! where a prepared image is Unix; elsewhere it is refused.

use std::path::Path;

use tilo_device_identity::IdentityPaths;

/// The setup machine's identity and the fresh, private handoff tree.
pub struct LocalEnrollment<'a> {
    pub target_root: &'a Path,
    pub username: &'a str,
    pub local_identity_dir: &'a Path,
    pub local_system: &'a IdentityPaths,
    pub local_personal_devices: &'a Path,
}

/// Enroll the image `enrollment` names for the person called `display_name`,
/// as the device `device_label`.
///
/// # Errors
/// What the enrollment refused, or that this machine prepares no image a
/// local identity enrolls.
#[cfg(unix)]
pub fn enroll(
    enrollment: &LocalEnrollment<'_>,
    display_name: &str,
    device_label: &str,
) -> Result<(), String> {
    tilo_enrollment::install::install_from_local_identity(
        &tilo_enrollment::install::OfflineInstall {
            target_root: enrollment.target_root,
            username: enrollment.username,
            local_identity_dir: enrollment.local_identity_dir,
            local_system: enrollment.local_system,
            local_personal_devices: enrollment.local_personal_devices,
        },
        display_name,
        device_label,
    )
    .map(drop)
    .map_err(|error| format!("{error:#}"))
}

/// See the Unix `enroll`: no image this machine prepares is enrolled with
/// its identity.
///
/// # Errors
/// Always.
#[cfg(not(unix))]
pub fn enroll(
    _enrollment: &LocalEnrollment<'_>,
    _display_name: &str,
    _device_label: &str,
) -> Result<(), String> {
    Err("local identity enrollment is unavailable on this machine".into())
}
