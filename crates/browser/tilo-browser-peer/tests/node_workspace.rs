//! A company's workspace on its node, end to end. The founder has only a
//! browser: headless Chrome (a virtual authenticator with PRF) creates their
//! root behind a passkey with the production `root-identity.mjs`. A node is
//! provisioned with a device key and an install token and no certificate;
//! the root certifies it and the page installs the certificate through the
//! node's install route, as `tilo-relay run --web … --install-token-file`
//! serves it. With it the root certifies the session beside the node, which
//! runs the person's apps and agents with a key of its own: the node vouches
//! for that key, and the session becomes one of her devices, never a host. The node then starts as the founder's host and the governor of
//! their rooms: the page's own device connects, creates the company
//! workspace and writes a document, which leaves the page sealed and is
//! retained on the node only as ciphertext, under channel keys the node
//! stewards. A second person, invited by name, is admitted and reads the
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

// This binary re-execs as the automerge worker (the node's rooms use it).
tilo_automerge_isolate::test_worker!();

use std::path::Path;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use neighbour_wire::NeighbourMsg;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tilo_browser_peer::invite::{ask_to_join, ask_to_join_pre_admitted, guest_certificate, join};
use tilo_browser_peer::{
    DeviceSigner, DocumentRoute, Host, IssuedRole, PeerHandle, SignError, SignFuture,
    certify_with_root, issue_device_cert,
};
use tilo_relay::RelayConfig;
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay::web::install::{InstallListener, SessionDevice};
use tilo_relay::web::{TlsSource, WebConfig};
use tilo_relay_protocol::rooms::{
    JoinReply, JoinStep, PreAdmission, RoomNotice, RoomRole, RoomsReply, RoomsRequest,
};
use tilo_user_identity::{IdentityPaths, RevocationList, persist};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;

use support::https::DevHttps;
use support::{next_document, ready, start_peer, wait_for};

const INSTALL_TOKEN: &str = "provisioned-install-token-0123";
/// Text only the document's readers ever see.
const SECRET: &str = "Acme Q3 plan: ship the node";
/// How soon someone let in, or redeeming a pre-admission, reads the
/// room's documents.
const ADMITTED_TO_READING: std::time::Duration = std::time::Duration::from_secs(8);

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

async fn rooms(peer: &PeerHandle, request: RoomsRequest) -> RoomsReply {
    let (reply, answer) = tokio::sync::oneshot::channel();
    peer.rooms_then(
        request,
        Box::new(move |result| {
            let _ = reply.send(result);
        }),
    )
    .unwrap();
    tilo_test_env::within("the node answers /rooms", answer)
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

/// Whether any file under `dir` contains `needle`.
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
async fn a_company_node_governs_and_stewards_the_workspace_people_join() {
    let driver = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/node_workspace/driver.mjs");
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
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
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
            live_origin: None,
            limits: tilo_relay::web::WebLimits::default(),
        })
        .await
        .unwrap();
    let https = DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());

    // The page's own device, certified by the root against the node.
    let page_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (page_cert, host_cert) = certify_with_root(&*https, &root, &page_key, "Ada's browser")
        .await
        .unwrap();
    assert_eq!(host_cert, node_cert);
    let ada = start_peer(
        Host::Https(https.clone()),
        page_cert,
        host_cert.clone(),
        page_key,
    );
    ready(&[&ada]).await;

    // The company workspace, governed by the node, with Ada as its owner.
    let RoomsReply::Room(acme) = rooms(
        &ada,
        RoomsRequest::Create {
            name: "Acme".into(),
        },
    )
    .await
    else {
        panic!("no workspace");
    };
    let entered = rooms(
        &ada,
        RoomsRequest::Enter {
            room: acme.room.clone(),
        },
    )
    .await;
    assert!(matches!(entered, RoomsReply::Room(_)), "{entered:?}");
    ada.join(acme.workspace_id).unwrap();
    ready(&[&ada]).await;

    // A document, sealed before it leaves the page: the node stewards its
    // channel, so the page gets keys and the writes go out.
    let route = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: acme.room.clone(),
        record: "plan".into(),
    };
    let (snapshot, change) = plan();
    ada.publish(route.clone(), &DocumentMutation::Snapshot(snapshot.clone()))
        .unwrap();
    ada.publish(
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
        .room_records()
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
    let RoomsReply::Invitation {
        signed_invitation, ..
    } = rooms(
        &ada,
        RoomsRequest::Invite {
            room: acme.room.clone(),
            request_id: [1; 32],
            role: RoomRole::Editor,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: Some("Ada".into()),
        },
    )
    .await
    else {
        panic!("no invitation");
    };
    let offered = join(
        &*https,
        &JoinStep::Inspect {
            invitation: signed_invitation.clone(),
        },
    )
    .await
    .unwrap();
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
    let asked = ask_to_join(
        &*https,
        &signed_invitation,
        &bo_cert,
        &bo_key,
        [2; 32],
        "Bo",
    )
    .await
    .unwrap();
    assert_eq!(asked, JoinReply::Waiting);
    let RoomsReply::Requests(waiting) = rooms(
        &ada,
        RoomsRequest::Requests {
            room: acme.room.clone(),
        },
    )
    .await
    else {
        panic!("no requests");
    };
    let [bo_request] = waiting.as_slice() else {
        panic!("{waiting:?}");
    };
    assert_eq!(bo_request.name, "Bo");
    let let_in = std::time::Instant::now();
    let admitted = rooms(
        &ada,
        RoomsRequest::Admit {
            room: acme.room.clone(),
            request_id: bo_request.request_id,
        },
    )
    .await;
    assert!(matches!(admitted, RoomsReply::Room(_)), "{admitted:?}");
    let status = join(
        &*https,
        &JoinStep::Status {
            request_id: [2; 32],
            device_public_key: bo_cert.device_public_key,
        },
    )
    .await
    .unwrap();
    assert!(matches!(status, JoinReply::Admitted { .. }), "{status:?}");

    // Bo joins after everything was written and reads all of it.
    let bo = start_peer(
        Host::Https(https.clone()),
        bo_cert,
        host_cert.clone(),
        bo_key,
    );
    bo.join(acme.workspace_id).unwrap();
    let mut events = Vec::new();
    assert_eq!(
        next_document(&bo, &mut events).await,
        DocumentMutation::Snapshot(snapshot.clone())
    );
    assert_eq!(
        next_document(&bo, &mut events).await,
        DocumentMutation::Incremental(change)
    );
    assert!(
        let_in.elapsed() < ADMITTED_TO_READING,
        "Bo read the room {:?} after he was let in",
        let_in.elapsed()
    );

    // Cy is pre-admitted: Ada's root signs a grant for the digest of a
    // one-time token someone else hands Cy, and Cy is admitted the moment
    // she redeems the invitation with it. Ada finds a notice and removes
    // her again.
    let token = b"one-time token for cy@acme.example";
    let mut grant = PreAdmission {
        room: acme.room.clone(),
        role: RoomRole::Editor,
        token_sha256: Sha256::digest(token).into(),
        label: "cy@acme.example".into(),
        expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
        owner_user_public_key: root.public_key,
        signature: [0; 64],
    };
    grant.signature = root.sign(grant.digest().unwrap().to_vec()).await.unwrap();
    let RoomsReply::Invitation {
        signed_invitation, ..
    } = rooms(
        &ada,
        RoomsRequest::PreAdmit {
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
    let wrong = ask_to_join_pre_admitted(
        &*https,
        &signed_invitation,
        &cy_cert,
        &cy_key,
        [4; 32],
        "Cy",
        b"a guessed token",
    )
    .await
    .unwrap();
    assert!(matches!(wrong, JoinReply::Refused(_)), "{wrong:?}");
    let redeemed = std::time::Instant::now();
    let admitted = ask_to_join_pre_admitted(
        &*https,
        &signed_invitation,
        &cy_cert,
        &cy_key,
        [5; 32],
        "Cy",
        token,
    )
    .await
    .unwrap();
    assert!(
        matches!(&admitted, JoinReply::Admitted { name, role: RoomRole::Editor, .. } if name == "Acme"),
        "{admitted:?}"
    );
    let RoomsReply::Notices(notices) = rooms(
        &ada,
        RoomsRequest::Notices {
            room: acme.room.clone(),
        },
    )
    .await
    else {
        panic!("no notices");
    };
    assert!(
        matches!(notices.as_slice(), [RoomNotice::PreAdmitted { user_public_key, name, label, .. }]
            if *user_public_key == cy_cert.user_public_key && name == "Cy" && label == "cy@acme.example"),
        "{notices:?}"
    );
    let cy_device = cy_cert.device_public_key;
    let cy = start_peer(
        Host::Https(https.clone()),
        cy_cert,
        host_cert.clone(),
        cy_key,
    );
    cy.join(acme.workspace_id).unwrap();
    let mut events = Vec::new();
    assert_eq!(
        next_document(&cy, &mut events).await,
        DocumentMutation::Snapshot(snapshot.clone())
    );
    assert!(
        redeemed.elapsed() < ADMITTED_TO_READING,
        "Cy read the room {:?} after she redeemed the invitation",
        redeemed.elapsed()
    );
    let removed = rooms(
        &ada,
        RoomsRequest::Remove {
            room: acme.room.clone(),
            request_id: [6; 32],
            user_public_key: cy_root.verifying_key().to_bytes(),
        },
    )
    .await;
    assert!(matches!(removed, RoomsReply::Room(_)), "{removed:?}");
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
