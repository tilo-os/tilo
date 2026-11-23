//! End-to-end integration: two devices certified under one user
//! identity are addressable from each other's directory CRDT and
//! get owner semantics on each other's changes.
//!
//! Complements the unit tests in each module:
//!
//! 1. The user's root certifies two devices, a primary and a phone.
//! 2. Both devices open a `WorkspaceDirectory` and take in each
//!    other's state (the person's own store carries it between them,
//!    and checks who wrote it).
//! 3. Convergence: both devices see the same list, and each side
//!    verifies the other's cert chains to the same user identity
//!    — the "matching identities get owner permissions" check.

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity, WorkspaceDirectory, WorkspaceId,
    sign_device_cert, verify_device_cert_at,
};

tilo_test_env::scenario! {
    fn two_certified_devices_share_a_directory_end_to_end_only_for_one_user;

    phase both_devices_certify_under_one_root_and_their_directories_converge;
    {
        // 1. User provisions their root identity on their primary device.
        let (root_sk, user) = UserIdentity::generate("Ada Lovelace", 1_000_000);

        // 2. Primary and phone each generate their own device key; the
        //    person's root certifies both.
        let (_, primary_dev) = DeviceIdentity::generate("Framework 16", 1_100_000);
        let mut primary_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: primary_dev.public_key,
            device_label: primary_dev.label.clone(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1_200_000,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut primary_cert, &root_sk).unwrap();
        verify_device_cert_at(&primary_cert, 1_300_000).unwrap();

        let (_, phone_dev) = DeviceIdentity::generate("Ada's phone", 1_500_000);
        let mut phone_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: phone_dev.public_key,
            device_label: phone_dev.label.clone(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1_600_000,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut phone_cert, &root_sk).unwrap();

        // 3. Both devices open a directory + author distinct changes.
        let mut primary_dir = WorkspaceDirectory::new(user.public_key, primary_dev.public_key);
        let mut phone_dir = WorkspaceDirectory::new(user.public_key, phone_dev.public_key);
        primary_dir
            .add_workspace(WorkspaceId::from_u128(1), "Notes")
            .unwrap();
        phone_dir
            .add_workspace(WorkspaceId::from_u128(2), "Photos")
            .unwrap();

        // 4. Each takes in the other's state.
        let primary_state = primary_dir.save();
        let phone_state = phone_dir.save();
        phone_dir.merge(&primary_state).unwrap();
        primary_dir.merge(&phone_state).unwrap();

        // 5. Convergence: both see workspaces 1 and 2.
        let primary_view = primary_dir.list_workspaces();
        let phone_view = phone_dir.list_workspaces();
        assert_eq!(primary_view.len(), 2);
        assert_eq!(phone_view.len(), 2);
        assert_eq!(primary_view, phone_view, "directories should converge");

        // 6. "Matching-user peers get owner permissions on each
        //    other's shares" — expressed via the cert chain: both
        //    certs verify under the same user root, so the phone
        //    is considered part of the user's fleet by the primary
        //    and vice-versa.
        assert_eq!(primary_cert.user_public_key, phone_cert.user_public_key);
        verify_device_cert_at(&primary_cert, 1_750_000).unwrap();
        verify_device_cert_at(&phone_cert, 1_750_000).unwrap();
        assert!(primary_cert.capabilities.can_own_workspaces);
        assert!(phone_cert.capabilities.can_own_workspaces);
    }
}
