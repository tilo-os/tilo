//! A local host for browser runs of the runtime bundle: `tilo-relay` serving
//! HTTPS + WebTransport with a development certificate, a throwaway user
//! root, and an approver that certifies every browser device that asks.
//! Never use it outside development: anyone who reaches it is admitted.
//!
//! ```sh
//! cargo run -p tilo-browser-peer --example dev_host -- 127.0.0.1:8443 http://127.0.0.1:8765
//! ```
//!
//! Prints one JSON line (`origin`, `userPublicKey`, `certificateSha256`,
//! `room`) for the page's `connectPeer`, then serves until interrupted. The
//! page's browser must accept the self-signed HTTPS certificate (trust it
//! once, or start a test browser with `--ignore-certificate-errors`);
//! WebTransport pins it by hash.
#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures' locks for the length of a step"
)]
#![allow(
    clippy::print_stdout,
    clippy::expect_used,
    reason = "a development tool: it reports on stdout and a failed setup is fatal"
)]

use std::net::SocketAddr;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay::certify::ApproverDecision;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay_client::Approver;
use tilo_user_identity::{
    CertificationPolicy, DeviceCapabilities, DeviceCert, RevocationList, issue_device_cert,
    sign_device_cert,
};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).expect("sign a device certificate");
    (key, cert)
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let listen: SocketAddr = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:8443".into())
        .parse()
        .expect("listen address");
    let allowed_origins: Vec<String> = args.collect();
    let state = tempfile::tempdir().expect("state directory");
    let root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let user = root.verifying_key().to_bytes();
    let (host_key, host_cert) = enrol(&root, "dev host", DeviceCapabilities::host());
    let (approver_key, approver_cert) = enrol(&root, "dev approver", DeviceCapabilities::primary());

    let cfg = tilo_relay::RelayConfig {
        state_root: state.path().to_path_buf(),
        ..Default::default()
    };
    let relay = Relay::start(
        cfg,
        RelayIdentity {
            user_public_key: user,
            device_signing_key: host_key,
            device_cert: host_cert,
            revocations: RevocationList::new(),
        },
    )
    .await
    .expect("start the relay");
    let web = relay
        .serve_web(WebConfig {
            listen,
            tls: TlsSource::Development {
                hosts: vec!["localhost".into(), listen.ip().to_string()],
                store: None,
            },
            allowed_origins,
            plain_listen: None,
            public_origin: None,
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
            country: None,
            fetch_proxy: None,
        })
        .await
        .expect("serve browsers");
    println!(
        "{}",
        serde_json::json!({
            "origin": web.origin,
            "userPublicKey": hex::encode(user),
            "certificateSha256": web.certificate_sha256,
            "room": ROOM,
        })
    );

    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .expect("approver endpoint");
    let mut approver = Approver::connect(
        &endpoint,
        relay.node_addr(),
        approver_cert,
        &approver_key,
        &RevocationList::new(),
    )
    .await
    .expect("connect the approver");
    loop {
        for pending in approver
            .list_pending()
            .await
            .expect("list pending requests")
        {
            let decision = issue_device_cert(
                &pending.request,
                &root,
                &CertificationPolicy::browser(),
                &RevocationList::new(),
                tilo_clock::unix_now_ns(),
            )
            .map_or(ApproverDecision::Deny, ApproverDecision::Issue);
            println!(
                "{}",
                serde_json::json!({"certified": pending.request.device_label, "pairingCode": pending.pairing_code})
            );
            approver
                .decide(pending.request_digest, decision)
                .await
                .expect("answer a request");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
