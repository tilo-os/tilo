//! A local host for browser runs of the runtime bundle: `tilo-server` serving
//! HTTPS with a certificate its local CA issues, a throwaway user root, and an
//! approver that signs in every browser that asks to be one of the root's devices.
//! Never use it outside development: anyone who reaches it is admitted.
//!
//! ```sh
//! cargo run -p tilo-session-client --example dev_host -- 127.0.0.1:8443 http://127.0.0.1:8765
//! ```
//!
//! Prints one JSON line (`origin`, `userPublicKey`, `certificateSha256`,
//! `workspace`) for the page's `connectPeer`, then serves until interrupted. The
//! page's browser must accept the HTTPS certificate, which its throwaway root's
//! CA issued: trust it once in the browser that opens it, or pin it — a browser given
//! `--ignore-certificate-errors-spki-list=<base64 SHA-256 of its SPKI>` loads
//! that certificate and no other. Clients pin it by its hash.
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
use tilo_runtime::device::keep::server::{KeptServer, ServerIdentity};
use tilo_runtime::device::keep::web::{TlsSource, WebConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

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
        issuer: None,
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
    let (host_key, server_cert) = enrol(&root, "dev host", DeviceCapabilities::member());

    let cfg = tilo_server_protocol::config::ServerConfig::default();
    let server = KeptServer::start_founding(
        cfg,
        state.path().to_path_buf(),
        ServerIdentity {
            user_public_key: user,
            device_signing_key: host_key,
            device_cert: server_cert,
            revocations: RevocationList::new(),
        },
        tilo_workspace_service::service::RegistriesFile::seeded(),
    )
    .await
    .expect("start the server");
    let web = server
        .serve_web(WebConfig {
            listen,
            tls: TlsSource::Local {
                hosts: vec!["localhost".into(), listen.ip().to_string()],
                store: None,
            },
            allowed_origins,
            plain_listen: None,
            public_origin: None,
            fetch_proxy: None,
            site: None,
        })
        .await
        .expect("serve browsers");
    println!(
        "{}",
        serde_json::json!({
            "origin": web.origin,
            "userPublicKey": hex::encode(user),
            "certificateSha256": web.link.certificate_sha256.map(hex::encode),
            "workspace": WORKSPACE,
        })
    );

    // Every tab that asks to sign in as one of the root's devices is signed in.
    let tickets = server
        .workspaces()
        .expect("the host serves its workspaces")
        .action_service()
        .tickets();
    loop {
        for (secret, asked) in tickets.waiting_for(user) {
            let tab = tilo_device_identity::login::Tab {
                device_public_key: asked.device.clone(),
                label: asked.label.clone(),
                key_for: None,
            };
            let Ok(mut cert) = tab.certificate(user, tilo_clock::unix_now_ns()) else {
                continue;
            };
            sign_device_cert(&mut cert, &root).expect("sign a browser's certificate");
            if tickets
                .approve(user, &secret, cert, Some("dev host"))
                .is_ok()
            {
                println!("{}", serde_json::json!({"certified": asked.label}));
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
