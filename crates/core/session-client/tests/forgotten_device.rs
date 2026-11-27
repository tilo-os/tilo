//! A host that forgets a guest: a host whose state was reset (its identity
//! and address kept) no longer has the workspace the guest is in, so it turns
//! the guest away there, and the page stops with `not-admitted` for the
//! person to join again.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the test holds its host for the length of a step"
)]

use tilo_session_client::Outbox as _;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{
    HostSettings, arrive_under, enrol, restart_host_relayed, start_host_serving, wait_for,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_host_refuses_its_forgotten_guest() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let (guest_root, _) = UserIdentity::generate("Guest", 1);
    let state = tempfile::tempdir().unwrap();
    let host = start_host_serving(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            guests: &[guest_root.verifying_key().to_bytes()],
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    // The guest is in a workspace of the host's: its entry is open, and the guest's page
    // enters through it and joins.
    let workspaces = host.server.workspaces().unwrap();
    let club = workspaces.create("Club").await.unwrap();
    workspaces
        .publish(
            &club.workspace,
            &host_device.cert,
            tilo_server_protocol::directory::Listing {
                description: "The club".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let peer = arrive_under(
        host.iroh(),
        &guest_root,
        &host_device.cert,
        &club.workspace,
        "Guest",
    )
    .await;
    peer.send(tilo_session_client::Cmd::Join(club.workspace_id))
        .unwrap();
    crate::support::admitted_to(&peer, club.workspace_id).await;

    // The host's state is reset: the same identity and web certificate, its
    // ports the kernel's to pick again, and nothing of the guest or its workspace.
    drop(host.web);
    host.server.shutdown().await;
    let reset = tempfile::tempdir().unwrap();
    std::fs::copy(
        state.path().join("web-certificate"),
        reset.path().join("web-certificate"),
    )
    .unwrap();
    let host = restart_host_relayed(&root, &host_device, reset.path()).await;
    let failed = wait_for("the reset host to refuse the guest", || {
        let snapshot = peer.snapshot();
        async move { (snapshot.phase == "failed").then_some(snapshot) }
    })
    .await;
    assert_eq!(failed.error_code, Some("not-admitted"));
    // Nothing was carried for the guest: the reset host keeps no workspace for it.
    assert!(
        host.server.workspaces().unwrap().ids().await.is_empty(),
        "the reset host keeps nothing for the guest it refused"
    );
    peer.close();
    host.server.shutdown().await;
}
