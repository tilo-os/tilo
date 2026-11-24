//! This machine's device key, for the host only, while the identity it signs for still trusts it.
//!
//! The key answers only while this device holds a valid certificate from the identity a workspace
//! selected, held in that person's devices record and not revoked there, so a revocation takes
//! effect at the very next call. Apps never get the key: they sign through the
//! [`broker`](crate::broker).
//!
//! The person's devices are answered by the host that holds their record
//! ([`crate::sources::DeviceTrust`]): this crate keeps no list of its own.

use std::io;

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

/// This machine's device key and its certificate for the identity this device presents in
/// `workspace`.
///
/// # Errors
/// This device has no identity for the workspace, or its certificate is invalid, another
/// identity's or device's, not enrolled, or revoked.
pub fn current(workspace: &str) -> io::Result<(SigningKey, DeviceCert)> {
    let profile = crate::store::identity_for_workspace(workspace)?
        .ok_or_else(|| io::Error::other("this device has no identity to sign in with"))?;
    of_profile(&profile)
}

/// This machine's device key and the certificate its person's root signed for it, before any
/// host has listed the device: what the daemon that keeps the person's devices record starts as,
/// since it lists this device itself once it holds the record. A revoked device still gets
/// nothing; every other caller asks [`current`].
///
/// # Errors
/// As [`current`], except that the device need not be listed yet.
pub fn unlisted(workspace: &str) -> io::Result<(SigningKey, DeviceCert)> {
    let profile = crate::store::identity_for_workspace(workspace)?
        .ok_or_else(|| io::Error::other("this device has no identity to sign in with"))?;
    checked(&profile, false)
}

/// As [`current`], for the identity `profile` names.
pub(crate) fn of_profile(
    profile: &crate::store::IdentityProfile,
) -> io::Result<(SigningKey, DeviceCert)> {
    checked(profile, true)
}

/// The device key and certificate for `profile`, refused when revoked, and when not `listed`
/// where the person's devices record must already hold it.
fn checked(
    profile: &crate::store::IdentityProfile,
    listed: bool,
) -> io::Result<(SigningKey, DeviceCert)> {
    let paths = crate::provisioning::collaboration_identity_paths();
    let device = tilo_device_identity::persist::load_device_key(&paths)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let device_key = device.verifying_key().to_bytes();
    let cert_path = paths.device_cert_for_user(&profile.user_public_key);
    let cert = tilo_device_identity::persist::read_device_cert(&cert_path)
        .map_err(|error| io::Error::other(error.to_string()))?;
    tilo_user_identity::verify_device_cert(&cert)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if cert.user_public_key != profile.user_public_key || cert.device_public_key != device_key {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device certificate does not belong to the selected identity and device",
        ));
    }
    // The person's devices, as the host that holds their record answers: a revocation — of the
    // device or of the stand-in that issued it — is seen by the very next call.
    if !crate::certificates::admitted(&profile.user_public_key, &cert) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device has been revoked for the selected identity",
        ));
    }
    if listed && !crate::certificates::enrolled(&profile.user_public_key, &cert) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this device is not enrolled for the selected identity",
        ));
    }
    Ok((device, cert))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_answers_only_while_the_identity_trusts_this_device() {
        crate::sources::test_trust::install();
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let system_root = temp.path().join("system");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", &config)
            .set("TILO_IDENTITY_ROOT", &system_root)
            .set(crate::provisioning::FILE_SECRETS_ENV, "1");
        let identity_dir = crate::store::account_identity_dir(&config.join("tilo/identity"));
        let paths = tilo_device_identity::IdentityPaths::under(&system_root);
        let workspace = tilo_desktop_model::WorkspaceId::new().to_string();

        assert!(current(&workspace).is_err(), "no identity yet");
        crate::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            crate::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let (device, certificate) = current(&workspace).unwrap();
        assert_eq!(
            certificate.device_public_key,
            device.verifying_key().to_bytes()
        );

        // The person's devices record holds this device: their root certified it as it made them.
        let profile = crate::store::identity_for_workspace(&workspace)
            .unwrap()
            .unwrap();
        crate::sources::test_trust::enrol(
            profile.user_public_key,
            device.verifying_key().to_bytes(),
        );
        current(&workspace).unwrap();

        // Their root takes it back, on another of their devices.
        crate::sources::test_trust::revoke(
            profile.user_public_key,
            device.verifying_key().to_bytes(),
        );
        let error = current(&workspace).unwrap_err();
        assert!(error.to_string().contains("revoked"), "{error}");
    }
}
