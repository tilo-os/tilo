//! A person with only a browser: the page's root certifies a native node as
//! the host and governor of their workspaces, the node serves with that
//! certificate, and the page certifies its own device against it and syncs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host and page live until the test ends"
)]

mod support;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{
    CertifyError, DeviceSigner as _, Host, IssuedRole, certify_with_root, issue_device_cert,
};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{Device, ready, start_host, start_peer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_root_certifies_its_node_and_then_itself() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let node_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let node_cert = issue_device_cert(
        &root,
        node_key.verifying_key().to_bytes(),
        "Company node",
        IssuedRole::Node,
    )
    .await
    .unwrap();
    assert!(node_cert.capabilities.can_act_as_host);
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
    let (cert, host_cert) = certify_with_root(&*host.https(), &root, &browser, "Browser")
        .await
        .unwrap();
    assert_eq!(host_cert, node_cert);
    assert_eq!(cert.capabilities, DeviceCapabilities::member());
    assert_eq!(cert.device_public_key, browser.device_public_key());
    let peer = start_peer(Host::Https(host.https()), cert, host_cert, browser);
    ready(&[&peer]).await;

    // Another person's root does not certify a page for this node.
    let (stranger, _) = UserIdentity::generate("Eve", 1);
    let other = SigningKey::generate(&mut UnwrapErr(SysRng));
    assert!(matches!(
        certify_with_root(&*host.https(), &stranger, &other, "Browser").await,
        Err(CertifyError::Denied(_))
    ));
}
