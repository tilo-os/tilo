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
//! 4. Both devices open a `WorkspaceDirectory` and exchange
//!    authenticated deltas.
//! 5. Convergence: both devices see the same list, and each side
//!    verifies the other's cert chains to the same user identity
//!    — the "matching identities get owner permissions" check.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
    WorkspaceDirectory, decode_enrollment_token, encode_enrollment_token, mint_enrollment_token,
    sign_device_cert, verify_device_cert_at, verify_enrollment_token,
};

/// The full enrollment and directory sync flow works, and a peer from a different user is rejected.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn enrollment_and_directory_sync_work_end_to_end_only_for_one_user() {
    // Phase: full_enrollment_and_directory_sync_flow.
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
        let (phone_sk, phone_dev) = DeviceIdentity::generate("Ada's phone", 1_500_000);
        let mut phone_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: phone_dev.public_key,
            device_label: phone_dev.label.clone(),
            capabilities: decoded.capabilities,
            issued_at_ns: 1_600_000,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut phone_cert, &root_sk).unwrap();

        // 6. Both devices open a directory + author distinct changes.
        let mut primary_dir = WorkspaceDirectory::new(user.public_key, primary_dev.public_key);
        let mut phone_dir = WorkspaceDirectory::new(user.public_key, phone_dev.public_key);
        primary_dir.add_workspace(1, "Notes").unwrap();
        primary_dir
            .set_member_devices(1, &[primary_dev.public_key, phone_dev.public_key])
            .unwrap();
        phone_dir.add_workspace(2, "Photos").unwrap();

        // 7. They exchange deltas. Both accept because:
        //    - Every delta's author cert chains to the same user identity.
        //    - No revocation is active.
        let primary_delta = primary_dir
            .sync_incremental(&primary_sk, 1_700_000)
            .unwrap();
        let phone_delta = phone_dir.sync_incremental(&phone_sk, 1_700_000).unwrap();
        let revs = RevocationList::new();

        phone_dir
            .apply_incremental(&primary_delta, &primary_cert, &revs, 1_750_000)
            .unwrap();
        primary_dir
            .apply_incremental(&phone_delta, &phone_cert, &revs, 1_750_000)
            .unwrap();

        // 8. Convergence: both see workspaces 1 and 2, with the shared
        //    member set on workspace 1.
        let primary_view = primary_dir.list_workspaces();
        let phone_view = phone_dir.list_workspaces();
        assert_eq!(primary_view.len(), 2);
        assert_eq!(phone_view.len(), 2);
        assert_eq!(primary_view, phone_view, "directories should converge");

        let ws1 = primary_view.iter().find(|w| w.id == 1).unwrap();
        assert!(ws1.member_devices.contains(&primary_dev.public_key));
        assert!(ws1.member_devices.contains(&phone_dev.public_key));

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

    // Phase: peer_from_different_user_is_rejected.
    {
        // Two independent users. A device certified by Bob cannot
        // apply deltas to Alice's directory even if its byte layout is
        // valid — the cert-vs-directory user-key guard rejects.
        let (alice_sk, alice) = UserIdentity::generate("Alice", 1);
        let (bob_sk, bob) = UserIdentity::generate("Bob", 2);

        let (a_dev_sk, a_dev) = DeviceIdentity::generate("Alice laptop", 100);
        let mut a_cert = DeviceCert {
            user_public_key: alice.public_key,
            device_public_key: a_dev.public_key,
            device_label: a_dev.label.clone(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut a_cert, &alice_sk).unwrap();

        let (b_dev_sk, b_dev) = DeviceIdentity::generate("Bob laptop", 100);
        let mut b_cert = DeviceCert {
            user_public_key: bob.public_key,
            device_public_key: b_dev.public_key,
            device_label: b_dev.label.clone(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut b_cert, &bob_sk).unwrap();

        let mut alice_dir = WorkspaceDirectory::new(alice.public_key, a_dev.public_key);
        alice_dir.add_workspace(1, "Personal").unwrap();
        let alice_delta = alice_dir.sync_incremental(&a_dev_sk, 300).unwrap();

        let mut bob_dir = WorkspaceDirectory::new(bob.public_key, b_dev.public_key);
        bob_dir.add_workspace(1, "Personal").unwrap();
        let bob_delta = bob_dir.sync_incremental(&b_dev_sk, 300).unwrap();

        // Bob presenting his cert against Alice's directory: rejected.
        let revs = RevocationList::new();
        assert!(
            alice_dir
                .apply_incremental(&bob_delta, &b_cert, &revs, 400)
                .is_err()
        );
        // Same in reverse.
        assert!(
            bob_dir
                .apply_incremental(&alice_delta, &a_cert, &revs, 400)
                .is_err()
        );
    }
}
