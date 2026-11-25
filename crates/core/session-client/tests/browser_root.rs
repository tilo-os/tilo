//! A person with only a browser: the page's root certifies a native node as
//! the host of their workspaces, the node serves with that
//! certificate, and the page certifies its own device against it and syncs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host and page live until the test ends"
)]

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_server_client::DeviceSigner as _;
use tilo_server_client::certification::{IssuedRole, certify_with_root, issue_device_cert};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{Device, ready, start_host, start_peer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_root_certifies_its_node_and_then_itself() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let node_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let node_cert = issue_device_cert(
        &root,
        node_key.verifying_key().to_bytes(),
        "Company node",
        IssuedRole::Device,
    )
    .await
    .unwrap();
    assert!(node_cert.capabilities.can_own_workspaces);
    assert!(node_cert.capabilities.can_own_workspaces);
    assert!(!node_cert.capabilities.can_admit_devices);
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &Device {
            key: node_key,
            cert: node_cert.clone(),
        },
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    let browser = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (cert, server_cert) = certify_with_root(&root, &browser, "Browser", &node_cert)
        .await
        .unwrap();
    assert_eq!(server_cert, node_cert);
    assert_eq!(cert.capabilities, DeviceCapabilities::member());
    assert_eq!(cert.device_public_key, browser.device_public_key());
    let peer = start_peer(host.iroh(), cert, server_cert, browser);
    ready(&[&peer]).await;

    // Another person's root certifies a device of its own person, not one of Ada's: whether
    // that person is let in is for the workspaces they are a member of to decide.
    let (stranger, _) = UserIdentity::generate("Eve", 1);
    let other = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (theirs, _) = certify_with_root(&stranger, &other, "Browser", &node_cert)
        .await
        .unwrap();
    assert_eq!(theirs.user_public_key, stranger.device_public_key());
    assert_ne!(theirs.user_public_key, node_cert.user_public_key);
}
