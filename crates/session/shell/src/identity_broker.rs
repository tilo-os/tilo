//! The identity broker over this session's workspace tree and channel service.

use crate::shell::channel_encryption::ChannelKeyProvider;
use crate::shell::state::{RoomId, Workspace, WorkspaceTreeHandle};
use std::{io, path::PathBuf, sync::Arc};
use tilo_identity_host::broker::WorkspaceMembership;
pub use tilo_identity_host::broker::{
    IdentityBrokerHandle, LocalDeviceIdentity, trusted_current_device_public,
};

/// Start the identity broker at `path`, authorizing against `tree` and
/// handing out route keys from `channels` once the collaboration broker
/// installs its channel service.
///
/// # Errors
///
/// Returns an error if binding the unix socket at `path` or putting it into
/// non-blocking mode fails, or if the listener thread cannot be spawned.
pub fn spawn(
    path: PathBuf,
    tree: WorkspaceTreeHandle,
    channels: ChannelKeyProvider,
) -> io::Result<IdentityBrokerHandle> {
    tilo_identity_host::broker::spawn(path, Arc::new(Membership(tree)), Arc::new(channels))
}

struct Membership(WorkspaceTreeHandle);

impl WorkspaceMembership for Membership {
    fn with_workspace(&self, room: &RoomId, read: &mut dyn FnMut(&Workspace)) {
        let tree = self.0.read();
        if let Some(workspace) = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| &workspace.room == room)
        {
            read(workspace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::collab_broker_capnp::run_isolated_native_consent_test;

    fn provision_native_consent_identity() -> (
        PathBuf,
        tilo_user_identity::IdentityPaths,
        crate::identity_registry::IdentityProfile,
        RoomId,
    ) {
        let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("child config"));
        let identity_dir = config.join("tilo/identity");
        let identity_root =
            PathBuf::from(std::env::var_os("TILO_IDENTITY_ROOT").expect("child identity root"));
        let paths = tilo_user_identity::IdentityPaths::under(identity_root);
        crate::firstboot::ensure_collaboration_identity(&identity_dir, &paths)
            .expect("provision certified identity");
        let room = RoomId::new();
        let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
            .expect("identity registry")
            .expect("selected identity");
        (identity_dir, paths, profile, room)
    }

    #[test]
    fn native_consent_public_identity_matches_selected_certified_device() {
        const TEST: &str = "identity_broker::tests::native_consent_public_identity_matches_selected_certified_device";
        run_isolated_native_consent_test(TEST, || {
            let (_identity_dir, paths, profile, room) = provision_native_consent_identity();
            let device = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
                .expect("persisted device key");
            let misleading = std::env::var("TILO_USER_ID").expect("misleading child identity");

            let public = trusted_current_device_public(&room.to_string())
                .expect("certified public local identity");

            assert_eq!(public.profile_id, profile.id);
            assert_eq!(public.principal_id, profile.principal_id());
            assert_ne!(public.profile_id, misleading);
            assert_ne!(public.principal_id, misleading);
            assert_eq!(public.user_root_public_key, profile.user_public_key);
            assert_eq!(public.device_public_key, device.verifying_key().to_bytes());
        });
    }

    #[test]
    fn native_consent_public_identity_rejects_invalid_or_missing_room_selection() {
        const TEST: &str = "identity_broker::tests::native_consent_public_identity_rejects_invalid_or_missing_room_selection";
        run_isolated_native_consent_test(TEST, || {
            assert_eq!(
                std::env::var("TILO_USER_ID").expect("misleading child identity"),
                "environment-owner-must-not-select-identity"
            );
            let invalid = trusted_current_device_public("not-a-room")
                .expect_err("an invalid room must fail before identity lookup");
            assert_eq!(invalid.kind(), std::io::ErrorKind::InvalidInput);

            let nil = trusted_current_device_public(&RoomId::nil().to_string())
                .expect_err("the unminted room sentinel is not admitted");
            assert_eq!(nil.kind(), std::io::ErrorKind::InvalidInput);

            let missing = trusted_current_device_public(&RoomId::new().to_string())
                .expect_err("a room with no selected identity must fail closed");
            assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        });
    }

    #[test]
    fn native_consent_public_identity_rejects_mismatch_unenrolled_and_revoked_device() {
        const TEST: &str = "identity_broker::tests::native_consent_public_identity_rejects_mismatch_unenrolled_and_revoked_device";
        run_isolated_native_consent_test(TEST, || {
            let (identity_dir, paths, profile, room) = provision_native_consent_identity();
            let cert_path = paths.device_cert_for_user(&profile.user_public_key);
            let original_cert = tilo_user_identity::persist::read_device_cert(&cert_path)
                .expect("original device certificate");
            let identity = runtime_identity::load_from_path(profile.record_path.clone())
                .expect("selected signing identity");

            std::fs::write(&cert_path, b"malformed-device-certificate")
                .expect("install malformed certificate bytes");
            assert!(
                trusted_current_device_public(&room.to_string()).is_err(),
                "malformed certificate bytes must fail the real decoder"
            );
            tilo_user_identity::persist::write_device_cert(&cert_path, &original_cert)
                .expect("restore after malformed certificate");

            let mut invalid_signature = original_cert.clone();
            invalid_signature.signature[0] ^= 0x80;
            tilo_user_identity::persist::write_device_cert(&cert_path, &invalid_signature)
                .expect("install invalid-signature certificate");
            assert!(
                trusted_current_device_public(&room.to_string()).is_err(),
                "a canonically encoded certificate with an invalid signature must fail"
            );
            tilo_user_identity::persist::write_device_cert(&cert_path, &original_cert)
                .expect("restore after invalid-signature certificate");

            let wrong_identity_dir = identity_dir.join("wrong-root");
            crate::firstboot::ensure_collaboration_identity(&wrong_identity_dir, &paths)
                .expect("provision independent wrong-root identity");
            let wrong_registry =
                crate::identity_registry::IdentityRegistryStore::at(&wrong_identity_dir)
                    .load()
                    .expect("load wrong-root registry");
            let wrong_id = wrong_registry
                .default_identity
                .expect("wrong-root default identity");
            let wrong_profile = wrong_registry
                .identities
                .get(&wrong_id)
                .expect("wrong-root profile");
            let wrong_root_cert = tilo_user_identity::persist::read_device_cert(
                &paths.device_cert_for_user(&wrong_profile.user_public_key),
            )
            .expect("valid wrong-root device certificate");
            tilo_user_identity::verify_device_cert(&wrong_root_cert)
                .expect("wrong-root certificate is independently valid");
            assert_ne!(wrong_root_cert.user_public_key, profile.user_public_key);
            tilo_user_identity::persist::write_device_cert(&cert_path, &wrong_root_cert)
                .expect("install valid wrong-root certificate");
            let wrong_root = trusted_current_device_public(&room.to_string())
                .expect_err("a valid certificate for another selected root must fail");
            assert_eq!(wrong_root.kind(), std::io::ErrorKind::PermissionDenied);
            tilo_user_identity::persist::write_device_cert(&cert_path, &original_cert)
                .expect("restore after wrong-root certificate");

            let wrong_device = ed25519_dalek::SigningKey::from_bytes(&[0x5a; 32]);
            let mut wrong_cert = tilo_user_identity::DeviceCert {
                device_public_key: wrong_device.verifying_key().to_bytes(),
                signature: [0; 64],
                ..original_cert.clone()
            };
            let digest = tilo_user_identity::device_cert_signing_digest(&wrong_cert)
                .expect("wrong-device certificate digest");
            wrong_cert.signature = identity.sign(&digest).expect("sign wrong-device cert");
            tilo_user_identity::persist::write_device_cert(&cert_path, &wrong_cert)
                .expect("install wrong-device certificate");
            let mismatch = trusted_current_device_public(&room.to_string())
                .expect_err("a valid certificate for another device must fail");
            assert_eq!(mismatch.kind(), std::io::ErrorKind::PermissionDenied);

            tilo_user_identity::persist::write_device_cert(&cert_path, &original_cert)
                .expect("restore current-device certificate");
            let fleet_path = identity_dir
                .join("fleets")
                .join(format!("{}.json", profile.id));
            std::fs::remove_file(&fleet_path).expect("remove fleet enrollment");
            let unenrolled = trusted_current_device_public(&room.to_string())
                .expect_err("a valid but unenrolled device must fail");
            assert_eq!(unenrolled.kind(), std::io::ErrorKind::PermissionDenied);

            let fleet = crate::device_fleet::DeviceFleetStore::at(&identity_dir, paths.clone());
            fleet
                .register_current_device(&profile.id)
                .expect("restore current-device enrollment");
            let device_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
                .expect("current device key")
                .verifying_key()
                .to_bytes();
            fleet
                .revoke(
                    &profile.id,
                    device_key,
                    tilo_user_identity::RevocationReason::Compromised,
                    1,
                )
                .expect("revoke current device");
            let revoked = trusted_current_device_public(&room.to_string())
                .expect_err("a revoked device must fail");
            assert_eq!(revoked.kind(), std::io::ErrorKind::PermissionDenied);
        });
    }
}
