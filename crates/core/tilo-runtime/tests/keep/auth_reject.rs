//! Auth-side integration checks:
//!
//! 1. A device whose cert chains to a different user identity is
//!    refused when it dials its workspace's link — no link opens.
//! 2. A device its person revoked is refused the same way.
//!
//! Both cases exercise the wire handshake and the link's admission, not just
//! the pure auth-module tests — an implementation bug on the server side
//! (e.g. skipping the revocation check) would slip past the unit tests but
//! fail here.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};

use crate::keep::common::{Linked, TestUser, hold_workspace, spawn_server};
use tilo_server_protocol::WorkspaceId;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn foreign_and_revoked_devices_are_rejected_and_disconnected() {
    // Phase `peer_from_different_user_is_rejected`.
    {
        let alice = TestUser::fresh("Alice");
        let bob = TestUser::fresh("Bob");
        let alice_server_dev = alice.enrol("home-server", DeviceCapabilities::member());
        let alice_ws_member = alice.enrol("laptop", DeviceCapabilities::primary());
        let bob_intruder = bob.enrol("stranger-laptop", DeviceCapabilities::primary());

        let server = Box::pin(spawn_server(
            &alice,
            alice_server_dev,
            &[WorkspaceId::from_u128(1)],
            &[&alice_ws_member],
        ))
        .await;

        // Bob's device dials Alice's server for the workspace. The host
        // proves the device it admits, and Bob's cert chains to Bob's root,
        // not Alice's: no link opens.
        let refused = Linked::open(&server.server, &bob_intruder, WorkspaceId::from_u128(1)).await;
        assert!(refused.is_err(), "cross-user device must be rejected");
    }
    // Phase `revoked_device_is_rejected`.
    {
        let user = TestUser::fresh("Alice");
        let server_dev = user.enrol("home-server", DeviceCapabilities::member());
        let good_dev = user.enrol("laptop", DeviceCapabilities::primary());
        let bad_dev = user.enrol("stolen-phone", DeviceCapabilities::primary());

        let mut revs = RevocationList::new();
        let mut entry = RevocationEntry {
            user_public_key: user.user_pk,
            revoked_device_public_key: bad_dev.device_pubkey(),
            revoked_at_ns: 100,
            reason: RevocationReason::Stolen,
            signature: [0u8; 64],
        };
        sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
        revs.insert(entry).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let state_root = tmp.path().to_path_buf();
        let cfg = tilo_server_protocol::config::ServerConfig::loopback(state_root.clone());
        let identity = tilo_runtime::device::keep::server::ServerIdentity {
            user_public_key: user.user_pk,
            device_signing_key: server_dev.sk.clone(),
            device_cert: server_dev.cert.clone(),
            revocations: revs,
        };
        let server = crate::keep::common::start_server(cfg, identity)
            .await
            .unwrap();
        hold_workspace(&server, WorkspaceId::from_u128(9), &[&good_dev]).await;

        // The revoked device dials the same link — refused.
        let rejected = Linked::open(&server, &bad_dev, WorkspaceId::from_u128(9)).await;
        assert!(rejected.is_err(), "revoked device must be rejected");

        let admitted = Linked::open(&server, &good_dev, WorkspaceId::from_u128(9)).await;
        assert!(admitted.is_ok(), "good device should still link");
    }
}
