//! A company's workspace on its node, end to end. The founder has only a
//! browser: headless Chrome (a virtual authenticator with PRF) creates their
//! root behind a passkey with the production `identity.mjs`. A node is
//! provisioned with a device key and an install token and no certificate;
//! the root certifies it and the page installs the certificate through the
//! node's install route, as `tilo-relay run --web … --install-token-file`
//! serves it. With it the root certifies the session beside the node, which
//! runs the person's apps and agents with a key of its own: the node vouches
//! for that key, and the session becomes one of her devices, never a host. The node then starts as the founder's host and the governor of
//! their workspaces: the page's own device connects, creates the company
//! workspace and writes a document, which leaves the page sealed and is
//! retained on the node only as ciphertext, under channel keys the node
//! holds. A second person, invited by name, is admitted and reads the
//! document's whole history. A third, whom the founder pre-admitted for a
//! token someone else vouched with, is admitted without the founder, who
//! finds a notice and removes her.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the browser, node and pages live until the test ends"
)]

mod support;

// This binary re-execs as the automerge worker (the node's workspaces use it).
tilo_automerge_isolate::test_worker!();

use std::path::Path;
use std::sync::Arc;
use tilo_automerge::ApplyIncoming as _;
use tilo_session_client::{Cmd, Link, Outbox};
use tilo_test_env::SpawnUnderEnvLockAsync as _;

use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use neighbour_wire::NeighbourMsg;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay::RelayConfig;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::install::{InstallListener, SessionDevice};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay_client::https::invitation as invitation_offer;
use tilo_relay_client::invite::guest_certificate;
use tilo_relay_client::{
    DeviceSigner, Host, IssuedRole, SignError, SignFuture, certify_with_root, issue_device_cert,
};
use tilo_relay_protocol::workspaces::{
    JoinReply, PreAdmission, WorkspaceNotice, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_user_identity::{IdentityPaths, RevocationList, persist};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use support::https::DevHttps;
use support::{ready, start_peer, wait_for};

const INSTALL_TOKEN: &str = "provisioned-install-token-0123";
/// Text only the document's readers ever see.
const SECRET: &str = "Acme Q3 plan: ship the node";

/// The root the page holds, answering requests over the driver's stdio.
struct PageRoot {
    public_key: [u8; 32],
    io: Arc<Mutex<(ChildStdin, Lines<BufReader<ChildStdout>>)>>,
}

impl PageRoot {
    async fn call(&self, request: Value) -> Value {
        let mut io = self.io.lock().await;
        io.0.write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let line = io.1.next_line().await.unwrap().expect("an answer");
        serde_json::from_str(&line).unwrap()
    }
}

impl DeviceSigner for PageRoot {
    fn device_public_key(&self) -> [u8; 32] {
        self.public_key
    }

    fn sign(&self, message: Vec<u8>) -> SignFuture<'_> {
        Box::pin(async move {
            let answer = self.call(json!({"sign": hex::encode(message)})).await;
            answer["signature"]
                .as_str()
                .and_then(|signature| hex::decode(signature).ok())
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| SignError(answer.to_string()))
        })
    }
}

fn dev_tls(state: &Path) -> TlsSource {
    TlsSource::Development {
        hosts: vec!["localhost".into(), "127.0.0.1".into()],
        store: Some(state.join("web-dev-identity")),
    }
}

async fn workspaces(peer: &Link, request: WorkspacesRequest) -> WorkspacesReply {
    let answer = peer.ask_workspaces(request).unwrap();
    tilo_test_env::within("the node answers its administration route", answer)
        .await
        .unwrap()
        .unwrap()
}

/// The document's first state and a later change to it.
fn plan() -> (Vec<u8>, Vec<u8>) {
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "title", SECRET).unwrap();
    let snapshot = doc.save();
    doc.put(automerge::ROOT, "owner", "Ada").unwrap();
    (snapshot, doc.save_incremental())
}

fn holds(dir: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        if path.is_dir() {
            holds(&path, needle)
        } else {
            std::fs::read(&path)
                .unwrap_or_default()
                .windows(needle.len())
                .any(|window| window == needle)
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_company_node_governs_and_seals_the_workspace_people_join() {
    let driver =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/page-driver.mjs");
    // The runtime module the driver serves is resolved before it starts, so
    // the time this test gives the page to create its identity is the page's
    // own.
    let bundle = support::page_bundle();
    let mut browser = tokio::process::Command::new("node")
        .arg(driver)
        .arg("node")
        .env("TILO_PAGE_BUNDLE", &bundle)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn_locked()
        .unwrap();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let first = tilo_test_env::within("the page to create the identity", lines.next_line())
        .await
        .unwrap()
        .expect("the identity line");
    let created: Value = serde_json::from_str(&first).unwrap();
    let page_origin = created["origin"].as_str().unwrap().to_owned();
    let root = PageRoot {
        public_key: hex::decode(created["userPublicKey"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
        io: Arc::new(Mutex::new((browser.stdin.take().unwrap(), lines))),
    };

    // The node: a device key, an install token and no certificate yet. It
    // serves only the install route, to the console's origin.
    let node = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(node.path().join("identity"));
    let state = node.path().join("state");
    let (device, _) = tilo_user_identity::load_or_create_device_key(&paths).unwrap();
    let device = device.verifying_key().to_bytes();
    let session_paths = IdentityPaths::under(node.path().join("session-identity"));
    let (session, _) = tilo_user_identity::load_or_create_device_key(&session_paths).unwrap();
    let session = session.verifying_key().to_bytes();
    let session_cert_out = node.path().join("session.cert");
    let install = InstallListener::bind_with_session(
        &WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: dev_tls(&state),
            allowed_origins: vec![page_origin],
            plain_listen: Some("127.0.0.1:0".parse().unwrap()),
            public_origin: None,
            fetch_proxy: None,
            door: None,
            link: None,
        },
        paths.clone(),
        INSTALL_TOKEN,
        Some(SessionDevice {
            public_key: session,
            cert_out: session_cert_out.clone(),
            user_key_out: None,
        }),
    )
    .unwrap();
    let install_origin = format!("http://{}", install.plain_addr.unwrap());

    // The node names the session beside it, vouched for by the key the
    // page got out of band.
    let info = root
        .call(json!({"info": {
            "origin": install_origin, "devicePublicKey": hex::encode(device), "installToken": INSTALL_TOKEN,
        }}))
        .await;
    assert_eq!(info["sessionPublicKey"], hex::encode(session), "{info}");

    // The page's root certifies the node, and the page installs it in the
    // canonical encoding `certifyDevice` hands it.
    let node_cert = issue_device_cert(&root, device, "Acme node", IssuedRole::Node)
        .await
        .unwrap();
    let certificate =
        hex::encode(tilo_user_identity::canonical::canonical_bytes(&node_cert).unwrap());
    let refused = root
        .call(json!({"install": {
            "origin": install_origin, "certificate": certificate, "installToken": "not-the-token-at-all",
        }}))
        .await;
    assert!(refused["error"].is_string(), "{refused}");
    let session_cert = issue_device_cert(&root, session, "Apps on Acme node", IssuedRole::Session)
        .await
        .unwrap();
    let installed = root
        .call(json!({"install": {
            "origin": install_origin,
            "certificate": certificate,
            "sessionCertificate": hex::encode(
                tilo_user_identity::canonical::canonical_bytes(&session_cert).unwrap()
            ),
            "installToken": INSTALL_TOKEN,
        }}))
        .await;
    assert_eq!(
        installed["device_public_key"],
        hex::encode(device),
        "{installed}"
    );
    assert_eq!(
        tilo_test_env::within("the node takes its certificate", install.installed())
            .await
            .unwrap(),
        node_cert
    );
    let delivered = tilo_user_identity::persist::read_device_cert(&session_cert_out).unwrap();
    assert_eq!(delivered, session_cert);
    assert_eq!(delivered.user_public_key, root.public_key);
    assert!(!delivered.capabilities.can_act_as_host && !delivered.capabilities.can_admit_devices);

    // It starts as `tilo-relay run` then does: the person's host, on the
    // identity it was installed with.
    let relay = Relay::start(
        RelayConfig {
            state_root: state.clone(),
            ..RelayConfig::default()
        },
        RelayIdentity {
            user_public_key: persist::read_public_key(&paths.user_pubkey).unwrap(),
            device_signing_key: persist::read_device_key(&paths.device_privkey).unwrap(),
            device_cert: persist::read_device_cert(&paths.device_cert).unwrap(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let web = relay
        .serve_web(WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: dev_tls(&state),
            allowed_origins: Vec::new(),
            plain_listen: None,
            public_origin: None,
            fetch_proxy: None,
            door: None,
            link: None,
        })
        .await
        .unwrap();
    let https = DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
    let host = Host::at(relay.node_addr());

    // The page's own device, certified by the root against the node.
    let page_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (page_cert, host_cert) = certify_with_root(&*https, &root, &page_key, "Ada's browser")
        .await
        .unwrap();
    assert_eq!(host_cert, node_cert);
    let ada = start_peer(host.clone(), page_cert, host_cert.clone(), page_key);
    ready(&[&ada]).await;

    // The company workspace, governed by the node, with Ada as its owner.
    let WorkspacesReply::Workspace(acme) = workspaces(
        &ada,
        WorkspacesRequest::Create {
            name: "Acme".into(),
        },
    )
    .await
    else {
        panic!("no workspace");
    };
    let entered = workspaces(
        &ada,
        WorkspacesRequest::Enter {
            workspace: acme.workspace.clone(),
        },
    )
    .await;
    assert!(
        matches!(entered, WorkspacesReply::Workspace(_)),
        "{entered:?}"
    );
    ada.send(Cmd::Join(acme.workspace_id)).unwrap();
    ready(&[&ada]).await;

    // A document, sealed before it leaves the page: the node takes part in
    // its channel, so the page gets keys and the writes go out.
    let route = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: acme.workspace.clone(),
        record: "plan".into(),
    };
    let (snapshot, change) = plan();
    ada.write(route.clone(), &DocumentMutation::Snapshot(snapshot.clone()))
        .unwrap();
    ada.write(
        route.clone(),
        &DocumentMutation::Incremental(change.clone()),
    )
    .unwrap();
    let durable = ada.barrier().unwrap();
    wait_for("the writes to be durable on the node", || async {
        durable.try_recv().ok()
    })
    .await
    .unwrap();

    // The node holds the document only as ciphertext.
    let writes: Vec<_> = relay
        .workspace_records()
        .retained(acme.workspace_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(
            |retained| match postcard::from_bytes::<NeighbourMsg>(&retained.body) {
                Ok(NeighbourMsg::AppDocument {
                    record_id,
                    envelope,
                    ..
                }) if record_id == "plan" => Some(envelope),
                _ => None,
            },
        )
        .collect();
    assert!(!writes.is_empty(), "the node retained the document");
    assert!(
        writes.iter().all(|envelope| envelope.encryption.is_some()),
        "every retained write is sealed"
    );
    assert!(
        !holds(&state, SECRET.as_bytes()),
        "no file on the node holds the document's text"
    );

    // Bo is invited; the invitation names who sent it.
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = workspaces(
        &ada,
        WorkspacesRequest::Invite {
            workspace: acme.workspace.clone(),
            request_id: [1; 32],
            role: WorkspaceRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: Some("Ada".into()),
            wait_list_entry: None,
            recipient: None,
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let offered = invitation_offer(&*https, &signed_invitation).await.unwrap();
    let JoinReply::Invitation {
        name,
        inviter: Some(inviter),
        ..
    } = offered
    else {
        panic!("{offered:?}");
    };
    assert_eq!(name, "Acme");
    assert_eq!(inviter.name, "Ada");
    assert_eq!(inviter.user_public_key, root.public_key);

    let bo_root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bo_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let bo_cert = guest_certificate(
        &bo_root,
        bo_key.verifying_key().to_bytes(),
        "Bo's browser",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    // Bo's session carries the invitation in its first message, and joins after everything was
    // written and reads all of it.
    let (bo, asked) = enters(
        host.clone(),
        &host_cert,
        (&bo_cert, &bo_key),
        &signed_invitation,
        None,
        "Bo",
    )
    .await;
    let asked = asked.unwrap();
    assert_eq!(
        asked.name, "Acme",
        "the invitation admits the person who holds it"
    );
    assert_eq!(asked.role, WorkspaceRole::Editor);
    bo.send(Cmd::Join(acme.workspace_id)).unwrap();
    let mut written = automerge::AutoCommit::load(&snapshot).unwrap();
    written.load_incoming(&change).unwrap();
    support::holds(&bo, &route, &written).await;

    // Cy is pre-admitted: Ada's root signs a grant for the digest of a
    // one-time token someone else hands Cy, and Cy is admitted the moment
    // she redeems the invitation with it. Ada finds a notice and removes
    // her again.
    let token = b"one-time token for cy@acme.example";
    let mut grant = PreAdmission {
        workspace: acme.workspace.clone(),
        role: WorkspaceRole::Editor,
        token_sha256: Sha256::digest(token).into(),
        label: "cy@acme.example".into(),
        expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
        owner_user_public_key: root.public_key,
        signature: [0; 64],
    };
    grant.signature = root.sign(grant.digest().unwrap().to_vec()).await.unwrap();
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = workspaces(
        &ada,
        WorkspacesRequest::PreAdmit {
            request_id: [3; 32],
            grant,
            inviter_name: Some("Ada".into()),
        },
    )
    .await
    else {
        panic!("no pre-admitted invitation");
    };
    let cy_root = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cy_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cy_cert = guest_certificate(
        &cy_root,
        cy_key.verifying_key().to_bytes(),
        "Cy's browser",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let (guessing, wrong) = enters(
        host.clone(),
        &host_cert,
        (&cy_cert, &cy_key),
        &signed_invitation,
        Some(b"a guessed token"),
        "Cy",
    )
    .await;
    assert_eq!(wrong.unwrap_err().code, "not-admitted");
    guessing.close();
    let (cy_page, admitted) = enters(
        host.clone(),
        &host_cert,
        (&cy_cert, &cy_key),
        &signed_invitation,
        Some(token),
        "Cy",
    )
    .await;
    let admitted = admitted.unwrap();
    assert_eq!(admitted.name, "Acme");
    assert_eq!(admitted.role, WorkspaceRole::Editor);
    cy_page.close();
    let WorkspacesReply::Notices(notices) = workspaces(
        &ada,
        WorkspacesRequest::Notices {
            workspace: acme.workspace.clone(),
        },
    )
    .await
    else {
        panic!("no notices");
    };
    assert!(
        matches!(notices.as_slice(), [WorkspaceNotice::PreAdmitted { user_public_key, name, label, .. }]
            if *user_public_key == cy_cert.user_public_key && name == "Cy" && label == "cy@acme.example"),
        "{notices:?}"
    );
    let cy_device = cy_cert.device_public_key;
    let cy = start_peer(host.clone(), cy_cert, host_cert.clone(), cy_key);
    cy.send(Cmd::Join(acme.workspace_id)).unwrap();
    support::holds(
        &cy,
        &route,
        &automerge::AutoCommit::load(&snapshot).unwrap(),
    )
    .await;
    let removed = workspaces(
        &ada,
        WorkspacesRequest::Remove {
            workspace: acme.workspace.clone(),
            request_id: [6; 32],
            user_public_key: cy_root.verifying_key().to_bytes(),
        },
    )
    .await;
    assert!(
        matches!(removed, WorkspacesReply::Workspace(_)),
        "{removed:?}"
    );
    wait_for("the node to end Cy's subscription", || async {
        (!relay.is_subscribed(acme.workspace_id, &cy_device).await).then_some(())
    })
    .await;

    drop(root);
    let status = tilo_test_env::within("the browser to close", browser.wait())
        .await
        .unwrap();
    assert!(status.success(), "the page's expectations failed");
    relay.shutdown().await;
}

/// A session of `cert`'s device whose first message carries `invitation` (with the pre-admission
/// `token` where there is one), and what the workspace answered it.
async fn enters(
    host: tilo_relay_client::Host,
    host_cert: &tilo_user_identity::DeviceCert,
    (cert, key): (&tilo_user_identity::DeviceCert, &SigningKey),
    invitation: &[u8],
    token: Option<&[u8]>,
    name: &str,
) -> (
    tilo_session_client::Link,
    Result<tilo_relay_client::login::Admission, tilo_session_client::device::HostRefusal>,
) {
    let held = tilo_session_client::device::Held {
        cert: cert.clone(),
        host_cert: Some(host_cert.clone()),
    };
    let (_, entering) = tilo_session_client::device::invitation_entry(
        host_cert,
        invitation,
        key,
        tilo_session_client::device::Entering {
            held: Some(&held),
            root: None,
            label: name,
            name,
            token,
        },
    )
    .await
    .unwrap();
    let peer = tilo_session_client::start_session(
        tilo_session_client::PeerConfig {
            host,
            cert: cert.clone(),
            host_cert: host_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(entering),
        },
        std::sync::Arc::new(key.clone()),
        std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
    );
    let answered = tilo_session_client::device::entered(&peer).await;
    (peer, answered)
}
