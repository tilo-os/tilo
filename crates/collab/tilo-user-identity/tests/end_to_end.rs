//! End-to-end integration: enrol two devices under one user
//! identity; both are addressable from each other's directory
//! CRDT; both get owner semantics on each other's changes.
//!
//! Complements the unit tests in each module by walking the flow
//! the shell + firstboot code will implement:
//!
//! 1. User A's primary device mints an `EnrollmentToken` authorising
//!    a new phone with `member` capabilities.
//! 2. The phone generates its own device keypair.
//! 3. The token is decoded and verified; a `DeviceCert` is signed by
//!    the primary and handed to the phone.
//! 4. Both devices open a `WorkspaceDirectory` and take in each
//!    other's state (the person's own store carries it between them,
//!    and checks who wrote it).
//! 5. Convergence: both devices see the same list, and each side
//!    verifies the other's cert chains to the same user identity
//!    — the "matching identities get owner permissions" check.

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity, WorkspaceDirectory, WorkspaceId,
    decode_enrollment_token, encode_enrollment_token, mint_enrollment_token, sign_device_cert,
    verify_device_cert_at, verify_enrollment_token,
};

tilo_test_env::scenario! {
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn enrollment_and_directory_sync_work_end_to_end_only_for_one_user;

    phase full_enrollment_and_directory_sync_flow;
    {
        // 1. User provisions their root identity on their primary device.
        let (root_sk, user) = UserIdentity::generate("Ada Lovelace", 1_000_000);

        // 2. Primary device generates its own device key.
        let (primary_sk, primary_dev) = DeviceIdentity::generate("Framework 16", 1_100_000);
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

        // 3. Primary mints an enrollment token authorising a phone.
        let token = mint_enrollment_token(
            user.public_key,
            DeviceCapabilities::member(),
            1_300_000,
            1_800_000, // expires in the future
            [0xAA; 16],
            vec![],
            &primary_sk,
        )
        .unwrap();

        // 4. Encode → out-of-band hand-off (QR, spoken, mDNS) → decode.
        let wire = encode_enrollment_token(&token).unwrap();
        let decoded = decode_enrollment_token(&wire).unwrap();
        assert_eq!(token, decoded);
        verify_enrollment_token(&decoded, 1_400_000).unwrap();

        // 5. Phone generates its own device key + submits its pubkey
        //    back to the primary. The primary signs a DeviceCert.
        let (_, phone_dev) = DeviceIdentity::generate("Ada's phone", 1_500_000);
        let mut phone_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: phone_dev.public_key,
            device_label: phone_dev.label.clone(),
            capabilities: decoded.capabilities,
            issued_at_ns: 1_600_000,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut phone_cert, &root_sk).unwrap();

        // 6. Both devices open a directory + author distinct changes.
        let mut primary_dir = WorkspaceDirectory::new(user.public_key, primary_dev.public_key);
        let mut phone_dir = WorkspaceDirectory::new(user.public_key, phone_dev.public_key);
        primary_dir
            .add_workspace(WorkspaceId::from_u128(1), "Notes")
            .unwrap();
        phone_dir
            .add_workspace(WorkspaceId::from_u128(2), "Photos")
            .unwrap();

        // 7. Each takes in the other's state.
        let primary_state = primary_dir.save();
        let phone_state = phone_dir.save();
        phone_dir.merge(&primary_state).unwrap();
        primary_dir.merge(&phone_state).unwrap();

        // 8. Convergence: both see workspaces 1 and 2.
        let primary_view = primary_dir.list_workspaces();
        let phone_view = phone_dir.list_workspaces();
        assert_eq!(primary_view.len(), 2);
        assert_eq!(phone_view.len(), 2);
        assert_eq!(primary_view, phone_view, "directories should converge");

        // 9. "Matching-user peers get owner permissions on each
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
