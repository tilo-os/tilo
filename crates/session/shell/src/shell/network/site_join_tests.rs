//! A computer joins a site's workspace as its person: the site names its
//! home workspace and admission door in `entry.json`, the door pre-admits
//! the person, and the session redeems the door's invitation through the
//! host's workspace join exchange over HTTPS, then holds the room over the
//! host's HTTPS routes. A browser that joined through the same door shares
//! its records with the computer through the host.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::transaction::Transactable as _;
use collab_types::DocumentMutation;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::Digest as _;
use tilo_browser_peer::{DocumentRoute, Host, MemoryPeerStore, PeerConfig, PeerHandle};
use tilo_relay_protocol::rooms::{JoinReply, PreAdmission, RoomRole};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::hosted_sites::{self, HostHttp};
use super::site_join::SiteJoin;
use super::{HostIdentity, Reach, ShellNetwork};
use crate::shell::state::{RoomId, WorkspaceTree};

const CHAT: &str = "so.tilo.chat";

/// 32 random bytes.
fn random() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

pub(super) fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

pub(super) fn certify(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// Wait until `probe` answers, within the shared hang bound.
pub(super) fn until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A community workspace as a site runs it: one governed room on a host
/// that serves only its HTTPS routes, and the site's entry and door in
/// front of it.
pub(super) struct Site {
    pub runtime: tokio::runtime::Runtime,
    pub relay: Option<tilo_relay::daemon::Relay>,
    pub web: Option<tilo_relay::web::WebHandle>,
    pub host_cert: DeviceCert,
    pub room: String,
    pub workspace_id: u64,
    /// Serves the site's entry and door while the site runs.
    _door: test_http::Responder,
    /// The site's entry: where `entry.json` is.
    pub entry: url::Url,
    /// What the door answers with: the host behind it now.
    behind: Arc<Mutex<Behind>>,
    /// Whether the door admits people (else the site is invitation-only).
    pub open: Arc<std::sync::atomic::AtomicBool>,
}

/// The host a site's door admits people to.
#[derive(Clone)]
struct Behind {
    rooms: tilo_relay::rooms::Rooms,
    host_cert: DeviceCert,
    room: String,
    origin: String,
}

/// The site's owner, whose root certifies its host and signs its door's
/// pre-admissions.
fn owner() -> SigningKey {
    key(40)
}

/// Start a host with device key `seed` under `dir`, governing a Lounge.
fn start_host(
    runtime: &tokio::runtime::Runtime,
    dir: &std::path::Path,
    seed: u8,
) -> (
    tilo_relay::daemon::Relay,
    tilo_relay::web::WebHandle,
    DeviceCert,
    tilo_relay::rooms::RoomSummary,
) {
    let root = owner();
    let host_key = key(seed);
    let host_cert = certify(
        &root,
        &host_key,
        "Lounge room host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let (relay, web) = runtime.block_on(async {
        let relay = tilo_relay::daemon::Relay::start(
            tilo_relay::RelayConfig {
                state_root: dir.join(format!("host-{seed}")),
                ..tilo_relay::RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let web = relay
            .serve_web(tilo_relay::web::WebConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                tls: tilo_relay::web::TlsSource::Development {
                    hosts: vec!["localhost".into()],
                    store: None,
                },
                allowed_origins: Vec::new(),
                plain_listen: Some("127.0.0.1:0".parse().unwrap()),
                public_origin: None,
                live_origin: None,
                limits: tilo_relay::web::WebLimits::default(),
                country: None,
            })
            .await
            .unwrap();
        (relay, web)
    });
    let lounge = runtime
        .block_on(relay.rooms().unwrap().create("Lounge"))
        .unwrap();
    (relay, web, host_cert, lounge)
}

fn json(status: u16, value: &serde_json::Value) -> Option<test_http::Reply> {
    Some(
        test_http::Reply::status(status)
            .header("content-type", "application/json")
            .body(value.to_string()),
    )
}

/// The door's `/join`: pre-admit the person the request names to the room
/// behind it, as the room service does.
fn door_join(
    handle: &tokio::runtime::Handle,
    behind: &Behind,
    request: &test_http::Request,
) -> Option<test_http::Reply> {
    let asked: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
    let user: [u8; 32] = hex::decode(asked["userPublicKey"].as_str()?)
        .ok()?
        .try_into()
        .ok()?;
    let root = owner();
    let token = hex::encode(random());
    let mut grant = PreAdmission {
        room: behind.room.clone(),
        role: RoomRole::Editor,
        token_sha256: sha2::Sha256::digest(token.as_bytes()).into(),
        label: asked["displayName"].as_str().unwrap_or("Guest").into(),
        expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
        owner_user_public_key: root.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = root.sign(&grant.digest().unwrap()).to_bytes();
    let issued = handle
        .block_on(behind.rooms.pre_admit(
            &behind.host_cert,
            tilo_relay::rooms::PreAdmitRequest {
                request_id: random(),
                grant,
                recipient_user_public_key: Some(user),
                inviter_name: Some("Tilo".into()),
            },
        ))
        .unwrap();
    json(
        200,
        &serde_json::json!({
            "invitation": tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation),
            "admissionToken": token,
            "origin": behind.origin,
        }),
    )
}

impl Site {
    pub(super) fn start(dir: &std::path::Path) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (relay, web, host_cert, lounge) = start_host(&runtime, dir, 41);
        let behind = Arc::new(Mutex::new(Behind {
            rooms: relay.rooms().unwrap().clone(),
            host_cert: host_cert.clone(),
            room: lounge.room.clone(),
            origin: web.plain_origin.clone().unwrap(),
        }));
        let open = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let door = {
            let handle = runtime.handle().clone();
            let behind = Arc::clone(&behind);
            let open = Arc::clone(&open);
            test_http::Responder::handle(move |request| {
                let behind = behind.lock().unwrap().clone();
                match (request.method.as_str(), request.target.as_str()) {
                    ("GET", "/entry.json") => {
                        let mut home = serde_json::json!({
                            "name": "Lounge",
                            "host": {"origin": behind.origin},
                        });
                        if open.load(std::sync::atomic::Ordering::SeqCst) {
                            home["admission"] = "door/".into();
                        }
                        json(200, &serde_json::json!({ "home": home }))
                    }
                    ("POST", "/door/join") => door_join(&handle, &behind, request),
                    _ => Some(test_http::Reply::status(404)),
                }
            })
        };
        let entry = url::Url::parse(&format!("{}/", door.url())).unwrap();
        Self {
            runtime,
            relay: Some(relay),
            web: Some(web),
            host_cert,
            room: lounge.room,
            workspace_id: lounge.workspace_id,
            _door: door,
            entry,
            behind,
            open,
        }
    }

    /// Set the host up again behind the same site: a new device and a new
    /// room, with nothing kept from the host before.
    pub(super) fn reset(&mut self, dir: &std::path::Path, seed: u8) {
        drop(self.web.take());
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
        let (relay, web, host_cert, lounge) = start_host(&self.runtime, dir, seed);
        *self.behind.lock().unwrap() = Behind {
            rooms: relay.rooms().unwrap().clone(),
            host_cert: host_cert.clone(),
            room: lounge.room.clone(),
            origin: web.plain_origin.clone().unwrap(),
        };
        self.relay = Some(relay);
        self.web = Some(web);
        self.host_cert = host_cert;
        self.room = lounge.room;
        self.workspace_id = lounge.workspace_id;
    }

    pub(super) fn relay(&self) -> &tilo_relay::daemon::Relay {
        self.relay.as_ref().unwrap()
    }

    fn origin(&self) -> String {
        self.behind.lock().unwrap().origin.clone()
    }

    /// The host's routes, as a page reaches them.
    pub(super) fn http(&self) -> Arc<HostHttp> {
        HostHttp::new(&self.origin()).unwrap()
    }

    /// Let the device `cert` names into the room through the door, as a page
    /// does: the door pre-admits its person and the device redeems the
    /// invitation as itself.
    pub(super) fn admit_device(&self, cert: &DeviceCert, device: &SigningKey, name: &str) {
        let workspace = hosted_sites::SiteWorkspace {
            entry: self.entry.clone(),
            name: "Lounge".into(),
            origin: self.origin(),
            admission: Some(self.entry.join("door/").unwrap()),
        };
        let answer = self
            .runtime
            .block_on(hosted_sites::admit(&workspace, cert.user_public_key, name))
            .unwrap();
        let invitation = base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            &answer.invitation,
        )
        .unwrap();
        let reply = self
            .runtime
            .block_on(tilo_browser_peer::invite::ask_to_join_pre_admitted(
                self.http().as_ref(),
                &invitation,
                cert,
                device,
                random(),
                name,
                answer.admission_token.as_bytes(),
            ))
            .unwrap();
        assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
    }

    /// Whether `user` is a member of the room by its governance.
    pub(super) fn member(&self, user: [u8; 32]) -> bool {
        self.runtime
            .block_on(self.relay().rooms().unwrap().summary(&self.room))
            .unwrap()
            .members
            .iter()
            .any(|member| member.user_public_key == user && !member.removed)
    }

    /// A browser of the person `root` that joins through the door, as a
    /// page does, and holds the room.
    pub(super) fn browser(&self, root: &SigningKey, device: SigningKey, name: &str) -> PeerHandle {
        let cert = certify(root, &device, name, DeviceCapabilities::member());
        self.admit_device(&cert, &device, name);
        let http = self.http();
        let _runtime = self.runtime.enter();
        let peer = PeerHandle::start(
            PeerConfig {
                host: Host::Https(http),
                cert,
                host_cert: self.host_cert.clone(),
                revocations: RevocationList::new(),
                watching: false,
            },
            Arc::new(device),
            Arc::new(MemoryPeerStore::default()),
        );
        peer.join(self.workspace_id).unwrap();
        peer
    }

    pub(super) fn shutdown(mut self) {
        drop(self.web.take());
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
    }
}

/// A computer of the person `root`: its session device is certified by
/// that root, as one of the person's devices.
pub(super) fn computer(
    dir: &std::path::Path,
    root: SigningKey,
    label: &'static str,
) -> (ShellNetwork, crate::shell::state::WorkspaceTreeHandle) {
    // A computer that ran before starts from what it saved, as a session
    // restarting does.
    let state = dir.join("state.json");
    let mut tree = if state.exists() {
        crate::shell::persist::load_tree_from(&state).unwrap()
    } else {
        WorkspaceTree::bootstrap_default()
    };
    tree.persist_path = Some(state);
    let tree = tree.shared();
    let net = ShellNetwork::spawn_with(
        tree.clone(),
        &dir.join("device-private/keypair.bin"),
        &dir.join("peer-sync"),
        Reach::Loopback,
        move |secret, _| {
            let key = SigningKey::from_bytes(&secret.to_bytes());
            let cert = certify(&root, &key, label, DeviceCapabilities::member());
            Some(HostIdentity {
                device_cert: cert,
                device_signing_key: key,
                workspace_device_certs: std::collections::HashMap::new(),
                workspace_revocations: std::collections::HashMap::new(),
                identity_paths: None,
            })
        },
    )
    .unwrap();
    (net, tree)
}

/// Tick `net` until its tree holds `room` under its native owner.
pub(super) fn held(
    net: &ShellNetwork,
    tree: &crate::shell::state::WorkspaceTreeHandle,
    room: RoomId,
) {
    until("the computer to hold the site's workspace", || {
        net.tick();
        let tree = tree.read();
        (tree.requires_native_owner(room)
            && tree
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == room))
        .then_some(())
    });
}

fn document(text: &str) -> Vec<u8> {
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "text", text).unwrap();
    doc.save()
}

#[test]
#[serial_test::serial]
fn a_computer_joins_a_sites_workspace_through_its_door_and_shares_it_with_a_browser() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let ada = key(1);
    let (net, tree) = computer(&dir.path().join("computer"), ada.clone(), "Ada's computer");

    let joined = net.join_site(&site.entry).unwrap();
    let room = RoomId::parse(&site.room).unwrap();
    assert!(
        matches!(joined, SiteJoin::Enrolling { room: named, .. } if named == room),
        "{joined:?}"
    );
    held(&net, &tree, room);
    assert!(
        site.member(ada.verifying_key().to_bytes()),
        "the room's governance lists the computer's person"
    );
    assert_eq!(
        tree.read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .map(|workspace| workspace.name.clone())
            .as_deref(),
        Some("Lounge"),
        "the workspace is named as the site names it"
    );
    assert_eq!(
        net.join_site(&site.entry).unwrap(),
        SiteJoin::Held { room },
        "a workspace the computer holds is not joined again"
    );

    // Grace's browser joins through the same door and writes in the chat;
    // the computer receives it through the host.
    let grace = key(2);
    let browser = site.browser(&grace, key(3), "Grace");
    let inbound = net.inbound_documents_handle();
    browser
        .publish(
            DocumentRoute {
                app_id: CHAT.into(),
                cluster_id: site.room.clone(),
                record: "general".into(),
            },
            &DocumentMutation::Snapshot(document("hello from the browser")),
        )
        .unwrap();
    until("the browser's record to reach the computer", || {
        net.tick();
        inbound
            .lock()
            .unwrap()
            .iter()
            .any(|document| document.app_id == CHAT && document.record_id == "general")
            .then_some(())
    });

    browser.close();
    drop(net);
    site.shutdown();
}

#[test]
#[serial_test::serial]
fn a_link_to_a_sites_workspace_joins_it_and_the_join_field_takes_the_site() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let (net, tree) = computer(&dir.path().join("computer"), key(1), "Ada's computer");
    let net = Arc::new(net);
    let room = RoomId::parse(&site.room).unwrap();

    // A chat link to the Lounge, opened on the computer: joined first.
    let link = format!(
        "{}app/#/@localhost:9/w/{}/so.tilo.chat",
        site.entry, site.room
    );
    let joining = {
        let net = Arc::clone(&net);
        std::thread::spawn(move || net.join_link(&link))
    };
    until("the link's workspace to be joined", || {
        net.tick();
        joining.is_finished().then_some(())
    });
    assert_eq!(joining.join().unwrap(), Ok(room));
    held(&net, &tree, room);

    // A link to another room on that site is not let in by its door.
    let elsewhere = format!(
        "{}#/@localhost:9/w/{}/so.tilo.chat",
        site.entry,
        RoomId::new()
    );
    assert!(net.join_link(&elsewhere).is_err());

    // The join field takes the site as typed.
    let local = tree.read().desktop.active;
    assert_eq!(
        net.join_from_field(local, site.entry.as_str()),
        Ok(crate::shell::peer_sync_overlay::PeerSyncResponse::Message(
            room.to_string()
        )),
        "the site's workspace is held already"
    );
    drop(net);
    site.shutdown();
}

#[test]
#[serial_test::serial]
fn an_invitation_only_site_says_so() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    site.open.store(false, std::sync::atomic::Ordering::SeqCst);
    let (net, _tree) = computer(&dir.path().join("computer"), key(1), "Ada's computer");
    assert_eq!(
        net.join_site(&site.entry),
        Err(hosted_sites::SiteError::InvitationOnly("Lounge".into()))
    );
    drop(net);
    site.shutdown();
}
