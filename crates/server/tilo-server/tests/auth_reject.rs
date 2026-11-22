//! Auth-side integration checks:
//!
//! 1. A peer whose cert chains to a different user identity is
//!    rejected during the handshake — `ServerClient::connect`
//!    surfaces the error, no session is opened.
//! 2. A peer whose device was revoked is rejected.
//!
//! Both cases exercise the wire handshake, not just the pure
//! auth-module tests — an implementation bug on the server side
//! (e.g. skipping the revocation check) would slip past the unit
//! tests but fail here.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use tilo_server::ServerClient;
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};

use crate::common::{TestUser, client_endpoint, spawn_server};
use tilo_server::WorkspaceId;

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
            &[&alice_ws_member.cert],
        ))
        .await;
        let server_addr = server.server.node_addr();

        // Bob's device tries to connect. Handshake must fail — the
        // policy is AllowMatchingUser and Bob's cert chains to Bob's
        // root, not Alice's.
        let ep = client_endpoint().await;
        let res = ServerClient::connect(
            ep,
            server_addr,
            bob.user_pk,
            bob_intruder.cert,
            bob_intruder.sk,
            RevocationList::new(),
        )
        .await;
        assert!(res.is_err(), "cross-user client must be rejected");
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
        let cfg = tilo_server::ServerConfig::loopback(state_root.clone());
        let identity = tilo_server::daemon::ServerIdentity {
            user_public_key: user.user_pk,
            device_signing_key: server_dev.sk.clone(),
            device_cert: server_dev.cert.clone(),
            revocations: revs,
        };
        let server = crate::common::start_server(cfg, identity).await.unwrap();
        crate::common::hold_workspace(&server, WorkspaceId::from_u128(9), &[&good_dev.cert]).await;
        let addr = server.node_addr();

        // The revoked device tries to connect — must fail.
        let ep = client_endpoint().await;
        let rejected = ServerClient::connect(
            ep,
            addr.clone(),
            user.user_pk,
            bad_dev.cert.clone(),
            bad_dev.sk.clone(),
            RevocationList::new(),
        )
        .await;
        assert!(rejected.is_err(), "revoked device must be rejected");

        let ep2 = client_endpoint().await;
        let res_ok = ServerClient::connect(
            ep2,
            addr,
            user.user_pk,
            good_dev.cert,
            good_dev.sk,
            RevocationList::new(),
        )
        .await;
        assert!(res_ok.is_ok(), "good device should still connect");
    }
    // Phase `newly_persisted_revocation_closes_an_existing_session`.
    {
        let user = TestUser::fresh("Alice");
        let server_dev = user.enrol("home-server", DeviceCapabilities::member());
        let device = user.enrol("lost-phone", DeviceCapabilities::member());
        let device_key = device.device_pubkey();
        let state = tempfile::tempdir().unwrap();
        let server = crate::common::start_server(
            tilo_server::ServerConfig::loopback(state.path().to_path_buf()),
            tilo_server::daemon::ServerIdentity {
                user_public_key: user.user_pk,
                device_signing_key: server_dev.sk,
                device_cert: server_dev.cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let client = ServerClient::connect(
            client_endpoint().await,
            server.node_addr(),
            user.user_pk,
            device.cert,
            device.sk,
            RevocationList::new(),
        )
        .await
        .unwrap();
        client
            .send(tilo_server::Envelope::Subscribe {
                workspace_id: WorkspaceId::from_u128(33),
            })
            .unwrap();
        let _ = tilo_test_env::within("a message on client", client.recv()).await;

        let mut entry = RevocationEntry {
            user_public_key: user.user_pk,
            revoked_device_public_key: device_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
        tilo_server::workspace_files::save_revocations(state.path(), &[entry]).unwrap();

        assert!(
            tilo_test_env::within("client to close", client.recv())
                .await
                .is_none()
        );
    }
}

/// The server follows its revocation log with a watch rather than a poll, and
/// an admitted session re-checks as soon as the live list changes. A
/// revocation written the way `tilo-server-admin` writes it therefore ends a
/// subscribed member's session well inside one second; it used to wait for a
/// one-second log poll and then a one-second session audit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persisted_revocation_closes_a_member_session_within_a_second() {
    let user = TestUser::fresh("Alice");
    let server_dev = user.enrol("home-server", DeviceCapabilities::member());
    let device = user.enrol("lost-phone", DeviceCapabilities::member());
    let device_key = device.device_pubkey();
    let state = tempfile::tempdir().unwrap();
    let server = crate::common::start_server(
        tilo_server::ServerConfig::loopback(state.path().to_path_buf()),
        tilo_server::daemon::ServerIdentity {
            user_public_key: user.user_pk,
            device_signing_key: server_dev.sk,
            device_cert: server_dev.cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    crate::common::hold_workspace(&server, WorkspaceId::from_u128(34), &[&device.cert]).await;
    let client = ServerClient::connect(
        client_endpoint().await,
        server.node_addr(),
        user.user_pk,
        device.cert,
        device.sk,
        RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(tilo_server::Envelope::Subscribe {
            workspace_id: WorkspaceId::from_u128(34),
        })
        .unwrap();
    assert!(matches!(
        crate::common::next(&client, |env| matches!(
            env,
            tilo_server::Envelope::Ready { .. }
        ))
        .await,
        tilo_server::Envelope::Ready { .. }
    ));

    let mut entry = RevocationEntry {
        user_public_key: user.user_pk,
        revoked_device_public_key: device_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
    tilo_server::workspace_files::save_revocations(state.path(), &[entry]).unwrap();

    let closed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while client.recv().await.is_some() {}
    })
    .await;
    assert!(
        closed.is_ok(),
        "a persisted revocation must end the session within a second"
    );
}
