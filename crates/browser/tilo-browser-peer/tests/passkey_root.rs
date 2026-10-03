//! A person with only a browser and a passkey: headless Chrome (a virtual
//! authenticator with PRF) creates their root with the production
//! `root-identity.mjs`; native code opens the same envelope with the
//! recovery key; the page's root certifies a node, which serves with that
//! certificate, and then the page's own device, which syncs with the node.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the browser, host and page live until the test ends"
)]

mod support;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::{
    DeviceSigner, Host, IssuedRole, SignError, SignFuture, certify_with_root, issue_device_cert,
};
use tilo_user_identity::DeviceCapabilities;
use tilo_user_identity::root_envelope::{RecoveryKey, RootEnvelope};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use support::{Device, ready, start_host, start_peer};

/// The root the page holds, signing over the driver's stdio.
struct PageRoot {
    public_key: [u8; 32],
    io: Arc<Mutex<(ChildStdin, Lines<BufReader<ChildStdout>>)>>,
}

impl DeviceSigner for PageRoot {
    fn device_public_key(&self) -> [u8; 32] {
        self.public_key
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        let io = self.io.clone();
        Box::pin(async move {
            let mut io = io.lock().await;
            io.0.write_all(format!("{}\n", hex::encode(message)).as_bytes())
                .await
                .map_err(|error| SignError(error.to_string()))?;
            let line = io.1.next_line().await.unwrap().expect("a signature");
            hex::decode(line)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| SignError("not a signature".into()))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_passkey_root_in_chrome_certifies_a_node_and_its_own_page() {
    let driver =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/passkey_root/driver.mjs");
    let mut browser = tokio::process::Command::new("node")
        .arg(driver)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let first = tilo_test_env::within("the page to create the identity", lines.next_line())
        .await
        .unwrap()
        .expect("the envelope line");
    let created: serde_json::Value = serde_json::from_str(&first).unwrap();
    let envelope: RootEnvelope = serde_json::from_value(created["envelope"].clone()).unwrap();
    assert_eq!(envelope.wraps.len(), 3);

    // Native code opens what Chrome sealed.
    let recovery = RecoveryKey::parse(created["recoveryKey"].as_str().unwrap()).unwrap();
    let opened = envelope.open_with_recovery(&recovery).unwrap();
    let public_key = envelope.public_key().unwrap();
    assert_eq!(opened.verifying_key().to_bytes(), public_key);

    let root = PageRoot {
        public_key,
        io: Arc::new(Mutex::new((browser.stdin.take().unwrap(), lines))),
    };
    let node_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let node_cert = issue_device_cert(
        &root,
        node_key.verifying_key().to_bytes(),
        "Company node",
        IssuedRole::Node,
    )
    .await
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &opened,
        &Device {
            key: node_key,
            cert: node_cert.clone(),
        },
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    let page_device = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (cert, host_cert) = certify_with_root(&*host.https(), &root, &page_device, "Browser")
        .await
        .unwrap();
    assert_eq!(host_cert, node_cert);
    assert_eq!(cert.capabilities, DeviceCapabilities::member());
    let peer = start_peer(Host::Https(host.https()), cert, host_cert, page_device);
    ready(&[&peer]).await;

    drop(root);
    let status = tilo_test_env::within("the browser to close", browser.wait())
        .await
        .unwrap();
    assert!(status.success(), "the page's expectations failed");
}
