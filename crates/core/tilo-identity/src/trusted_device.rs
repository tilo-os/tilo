//! This machine's device key, for its daemon only, while the identity it signs for still trusts it.
//!
//! The key answers only while this device holds a valid certificate from the identity a workspace
//! selected, held in that person's devices record and not revoked there, so a revocation takes
//! effect at the very next call. Apps never get the key: they sign through the
//! [`broker`](crate::broker).
//!
//! The device's daemon is the one process that has the key: it took it
//! ([`tilo_device_identity::persist::take_device_key`]), and every answer here that carries the key
//! is the one it took. Another program gets the certificate alone ([`certificate`]), checked
//! against the key the daemon says it signs with, and signs by asking the daemon
//! ([`device_signer`]). Nothing here opens the key file.
//!
//! The person's devices are answered by the host that holds their record
//! ([`crate::sources::DeviceTrust`]): this crate keeps no list of its own.

use std::io;
use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use tilo_device_identity::IdentityPaths;
use tilo_device_identity::login::daemon::{DeviceActions, DeviceRefusal};
use tilo_user_identity::DeviceCert;

/// This device's public key: the key this process took, in the device's daemon, else the one the
/// daemon says it signs with.
///
/// # Errors
/// The device's daemon is not running, or does not answer.
pub fn device_public_key(paths: &IdentityPaths) -> io::Result<[u8; 32]> {
    tilo_device_identity::persist::device_public_key(paths).map_err(|error| match error {
        tilo_device_identity::PersistError::Io(error) => error,
        error => io::Error::other(error),
    })
}

/// What signs as this device: the key this process took, in the device's daemon, else the daemon
/// asked to sign ([`crate::daemon_signer::DaemonSigner`]).
///
/// # Errors
/// The device's daemon is not running, or does not answer.
pub fn device_signer(paths: &IdentityPaths) -> io::Result<Arc<dyn DeviceSigner>> {
    if let Ok(key) = tilo_device_identity::persist::taken_device_key(paths) {
        return Ok(Arc::new(key));
    }
    let actions = DeviceActions::here(paths.clone()).map_err(io::Error::other)?;
    Ok(Arc::new(
        crate::daemon_signer::DaemonSigner::of(actions).map_err(refused)?,
    ))
}

/// The daemon's refusal as an error of the call that asked it: one the daemon gave because it is
/// not running says so.
fn refused(refusal: DeviceRefusal) -> io::Error {
    let kind = if refusal.not_running() {
        io::ErrorKind::NotConnected
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, refusal)
}

/// This machine's device key and its certificate for the identity this device presents in
/// `workspace`, in the device's daemon: the key it took.
///
/// # Errors
/// This process is not the device's daemon, this device has no identity for the workspace, or
/// its certificate is invalid, another identity's or device's, not enrolled, or revoked.
pub fn current(workspace: &str) -> io::Result<(SigningKey, DeviceCert)> {
    of_profile(&profile_for(workspace)?)
}

/// This device's certificate for the identity it presents in `workspace`, as [`current`] checks
/// it, for a program that is not the device's daemon: the key it names is the one the daemon
/// signs with.
///
/// # Errors
/// As [`current`], and when the device's daemon is not running.
pub fn certificate(workspace: &str) -> io::Result<DeviceCert> {
    let paths = crate::provisioning::collaboration_identity_paths();
    certified_in(
        &profile_for(workspace)?,
        &paths,
        true,
        device_public_key(&paths)?,
    )
}

/// The identity this device presents in `workspace`.
fn profile_for(workspace: &str) -> io::Result<crate::store::IdentityProfile> {
    crate::store::identity_for_workspace(workspace)?
        .ok_or_else(|| io::Error::other("this device has no identity to sign in with"))
}

/// This machine's device key and the certificate its person's root signed for it, before any
/// host has listed the device: what the daemon that keeps the person's devices record starts as,
/// since it lists this device itself once it holds the record. A revoked device still gets
/// nothing; every other caller asks [`current`].
///
/// # Errors
/// As [`current`], except that the device need not be listed yet.
pub fn unlisted(workspace: &str) -> io::Result<(SigningKey, DeviceCert)> {
    checked(&profile_for(workspace)?, false)
}

/// [`unlisted`]'s certificate, for a program that is not the device's daemon, as [`certificate`]
/// checks it.
///
/// # Errors
/// As [`unlisted`], and when the device's daemon is not running.
pub fn unlisted_certificate(workspace: &str) -> io::Result<DeviceCert> {
    let paths = crate::provisioning::collaboration_identity_paths();
    certified_in(
        &profile_for(workspace)?,
        &paths,
        false,
        device_public_key(&paths)?,
    )
}

/// As [`current`], for the identity `profile` names.
pub(crate) fn of_profile(
    profile: &crate::store::IdentityProfile,
) -> io::Result<(SigningKey, DeviceCert)> {
    checked(profile, true)
}

/// This computer's identity as its person's services start it, with this device's key and
/// certificates kept in `paths`: the person the identity store at `identity_dir` names, and the
/// certificate this device holds for them that names its key.
///
/// One answer for the daemon that decides whether to serve its person's services and for the
/// services that start from that decision: a certificate on disk that no identity names, or one
/// for a person this device cannot act as, starts nothing.
///
/// # Errors
/// The account keeps no identity, the identity names a person this device has no certificate for,
/// or the certificate is another device's, does not verify, or is revoked.
pub fn serving_in(
    identity_dir: &std::path::Path,
    paths: &IdentityPaths,
) -> io::Result<(SigningKey, DeviceCert)> {
    // The device's own certificate names the person this computer presents; without one, the
    // identity the account selected stands.
    let certificate = tilo_device_identity::persist::read_device_cert(&paths.device_cert).ok();
    let store = crate::store::IdentityRegistryStore::at(identity_dir);
    let registry = store.load_reconciled_cached()?;
    let profile = registry
        .for_workspace("", certificate.as_ref())
        .ok_or_else(|| io::Error::other("this device has no identity to sign in with"))?;
    checked_in(&profile, paths, false)
}

/// [`serving_in`] for the identity store of the account this process runs as.
///
/// # Errors
/// As [`serving_in`], and when this host keeps no Tilo profile.
pub fn serving(paths: &IdentityPaths) -> io::Result<(SigningKey, DeviceCert)> {
    serving_in(&crate::store::account_identity_dir_from_env()?, paths)
}

/// The device key and certificate for `profile`, refused when revoked, and when not `listed`
/// where the person's devices record must already hold it.
fn checked(
    profile: &crate::store::IdentityProfile,
    listed: bool,
) -> io::Result<(SigningKey, DeviceCert)> {
    checked_in(
        profile,
        &crate::provisioning::collaboration_identity_paths(),
        listed,
    )
}

/// [`checked`] with this device's key and certificates kept in `paths`: the key this process took
/// as the device's daemon.
fn checked_in(
    profile: &crate::store::IdentityProfile,
    paths: &IdentityPaths,
    listed: bool,
) -> io::Result<(SigningKey, DeviceCert)> {
    let device = tilo_device_identity::persist::taken_device_key(paths)
        .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
    let cert = certified_in(profile, paths, listed, device.verifying_key().to_bytes())?;
    Ok((device, cert))
}

/// This device's certificate for `profile`, kept in `paths`, naming `device_key`: refused when
/// revoked, and when not `listed` in the person's devices record where it must be already.
fn certified_in(
    profile: &crate::store::IdentityProfile,
    paths: &IdentityPaths,
    listed: bool,
    device_key: [u8; 32],
) -> io::Result<DeviceCert> {
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
    Ok(cert)
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

        // This test is the device's daemon: it took the device's key.
        tilo_device_identity::persist::load_or_create_device_key(&paths).unwrap();
        let _taken = tilo_device_identity::persist::take_device_key(&paths).unwrap();
        assert!(current(&workspace).is_err(), "no identity yet");
        crate::provisioning::ensure_collaboration_identity(
            &identity_dir,
            &paths,
            crate::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let (device, certificate) = current(&workspace).unwrap();
        assert_eq!(super::certificate(&workspace).unwrap(), certificate);
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
