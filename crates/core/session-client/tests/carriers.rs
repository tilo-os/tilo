//! Every feature of a device's session, over each way a device reaches its
//! server: directly, as a native device does, and only through the server's
//! packet relay, as a page does (it holds no UDP socket). Each scenario runs
//! once per [`Carrier`] against a real server running its own packet relay, so a
//! feature that works over one carrier and not the other fails here.
//!
//! The device under test reaches the server over the carrier, and so does
//! whatever approves it (a root-holding laptop, or a page holding the
//! person's root); the person's other devices that answer it (the one that
//! writes back) dial directly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until the test ends"
)]

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_session_client::{CallFrame, Cmd, Event, Events, InCall, Link, Outbox, TransportPhase};

use collab_types::presence::Presence;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::{Carrier, ServerAddress};
use tilo_session_client::MemoryPeerStore;
use tilo_session_client::session::TicketLink;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, UserIdentity,
    sign_revocation_entry,
};

use crate::support::{
    Device, HostSettings, TestHost, WORKSPACE, enrol, holds, in_workspace, place, ready, route,
    say, start_host_serving, start_page, start_peer, wait_for,
};

/// Each scenario, once per carrier.
macro_rules! over_each_carrier {
    ($($scenario:ident),* $(,)?) => {
        mod direct {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $scenario() {
                    super::$scenario(tilo_server_client::Carrier::Direct).await;
                }
            )*
        }
        mod relay {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $scenario() {
                    super::$scenario(tilo_server_client::Carrier::Relay).await;
                }
            )*
        }
        /// A page's way: the browser's transport, to the host's listener.
        mod webtransport {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $scenario() {
                    super::$scenario(tilo_server_client::Carrier::WebTransport).await;
                }
            )*
        }
    };
}

over_each_carrier!(
    a_root_holding_device_certifies_a_new_one,
    the_persons_page_signs_a_new_device_in,
    a_links_ticket_is_refused_once_used_and_once_the_page_showing_it_is_removed,
    documents_sync_both_ways_until_the_device_is_revoked,
    a_removed_device_is_told_that_it_was_removed,
    the_workspace_admits_the_device_and_its_live_session_carries_it,
    the_device_takes_part_in_a_call,
);

/// Ada's host, running its own packet relay, with her laptop (a root holder)
/// enrolled.
pub(crate) struct Ada {
    pub(crate) root: SigningKey,
    pub(crate) host_device: Device,
    pub(crate) laptop: Device,
    pub(crate) host: TestHost,
    /// The host's state, kept for the test's lifetime: the host serves from it.
    _state: tempfile::TempDir,
}

impl Ada {
    pub(crate) async fn start() -> Self {
        let (root, _) = UserIdentity::generate("Ada", 1);
        let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
        let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
        let state = crate::support::scratch_dir();
        let host = start_host_serving(
            &root,
            &host_device,
            state.path(),
            "127.0.0.1:0".parse().unwrap(),
            HostSettings {
                relayed: true,
                ..HostSettings::default()
            },
        )
        .await;
        Self {
            root,
            host_device,
            laptop,
            host,
            _state: state,
        }
    }

    /// A new device of Ada's, reaching the host as `host` says, signed in by her root: the tab
    /// asks the host for a sign-in ticket naming her, over the link it reaches the host's actions
    /// on, and her laptop, dialling the host the same way, lists it and signs it in.
    async fn certified(&self, host: &ServerAddress) -> (SigningKey, DeviceCert, DeviceCert) {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let laptop = TicketLink::new(host.clone());
        let signed = self
            .signed_in(
                host,
                &key,
                "Browser on Ada's layout",
                (&laptop, &self.laptop),
            )
            .await;
        laptop.close().await;
        (key, signed.0, signed.1)
    }

    /// The tab whose key is `key`, listed as `label`, reaching the host as `host` says, signed
    /// in by `by`, a device of Ada's that holds her root, over its own ticket link: its
    /// certificate and the certificate of the host it entered through.
    async fn signed_in(
        &self,
        host: &ServerAddress,
        key: &SigningKey,
        label: &str,
        (link, by): (&TicketLink, &Device),
    ) -> (DeviceCert, DeviceCert) {
        let user = self.root.verifying_key().to_bytes();
        let tab = TicketLink::new(host.clone());
        let minted = self
            .ask(
                &tab,
                key,
                "issue",
                json!({"direction": "sign-in", "user": hex::encode(user), "label": label}),
            )
            .await
            .unwrap();
        let device = hex::encode(key.verifying_key().to_bytes());
        let listed = self
            .ask(link, &by.key, "pending", json!({ "as": by.cert }))
            .await
            .unwrap();
        let waiting = listed["waiting"]
            .as_array()
            .unwrap()
            .iter()
            .find(|waiting| waiting["device"] == device.as_str())
            .expect("the tab waits for Ada's root")
            .clone();
        assert_eq!(waiting["label"], label);
        let tab_key = tilo_device_identity::login::Tab {
            device_public_key: device.clone(),
            label: label.to_owned(),
            agent: None,
        };
        let mut cert = tab_key
            .certificate(user, tilo_clock::unix_now_ns())
            .unwrap();
        tilo_user_identity::sign_device_cert(&mut cert, &self.root).unwrap();
        self.ask(
            link,
            &by.key,
            "approve",
            json!({"secret": waiting["secret"], "cert": cert, "as": by.cert, "label": by.cert.device_label}),
        )
        .await
        .unwrap();
        let followed = self
            .ask(&tab, key, "follow", json!({ "id": minted["id"] }))
            .await
            .unwrap();
        tab.close().await;
        assert_eq!(followed["claimed"]["user"], hex::encode(user));
        let certified: DeviceCert =
            serde_json::from_str(followed["certified"].as_str().unwrap()).unwrap();
        let host_cert: DeviceCert =
            serde_json::from_str(followed["hostCert"].as_str().unwrap()).unwrap();
        assert_eq!(
            certified, cert,
            "the tab reads the certificate the root signed"
        );
        (certified, host_cert)
    }

    /// Call a ticket action over `link` as the key `key`.
    async fn ask(
        &self,
        link: &TicketLink,
        key: &SigningKey,
        action: &str,
        argument: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let serde_json::Value::Object(argument) = argument else {
            unreachable!("an argument is an object")
        };
        link.ask(
            Arc::new(key.clone()),
            key.verifying_key().to_bytes(),
            self.host_device.cert.device_public_key,
            action,
            argument,
            std::time::Duration::from_secs(20),
        )
        .await
        .unwrap()
    }

    /// A link's login ticket, as `page` (a device of Ada's whose page holds her root) shows one
    /// for a browser entering the workspace: the id the page follows it by, and the ticket.
    async fn show_link(
        &self,
        shown: &TicketLink,
        page: &Device,
    ) -> (serde_json::Value, tilo_device_identity::login::Ticket) {
        let user = hex::encode(self.root.verifying_key().to_bytes());
        let minted = self
            .ask(
                shown,
                &page.key,
                "issue",
                json!({"user": user, "workspace": WORKSPACE, "browser": true}),
            )
            .await
            .expect("the page asks for a link's ticket");
        let ticket =
            tilo_device_identity::login::Ticket::parse(minted["ticket"].as_str().unwrap()).unwrap();
        (minted["id"].clone(), ticket)
    }

    /// `page` approves the tab that logged in with the ticket it follows by `id`: the root signs
    /// the certificate the host built, and the page signs the question with its own key.
    async fn approve_asked(&self, shown: &TicketLink, page: &Device, id: &serde_json::Value) {
        use ed25519_dalek::Signer as _;
        let asked: tilo_device_identity::login::Asked = wait_for("the tab to ask", || async move {
            let status = self
                .ask(shown, &page.key, "follow", json!({ "id": id }))
                .await
                .unwrap();
            status
                .get("asked")
                .map(|asked| serde_json::from_value(asked.clone()).unwrap())
        })
        .await;
        let mut cert = asked.cert.clone();
        tilo_user_identity::sign_device_cert(&mut cert, &self.root).unwrap();
        let approved = tilo_device_identity::login::Approved {
            cert,
            approver: page.cert.clone(),
            signature: page.key.sign(&asked.digest).to_bytes(),
        };
        self.ask(
            shown,
            &page.key,
            "answer",
            json!({"id": id, "approved": approved}),
        )
        .await
        .expect("the page's answer is taken");
    }

    /// Ada's laptop, in the workspace over a direct session.
    pub(crate) async fn laptop_peer(&self, server_cert: &DeviceCert) -> Link {
        place(&self.host.server, &self.laptop.cert).await;
        let peer = start_peer(
            self.host.over(Carrier::Direct),
            self.laptop.cert.clone(),
            server_cert.clone(),
            self.laptop.key.clone(),
        );
        peer.join_placed(WORKSPACE, "Workspace").unwrap();
        peer
    }
}

/// A page's session over `carrier`: its device key stays held, so it
/// dials from an endpoint its device delegated.
fn page(
    host: &TestHost,
    carrier: Carrier,
    key: SigningKey,
    cert: &DeviceCert,
    server_cert: &DeviceCert,
) -> Link {
    start_page(
        host.over(carrier),
        cert.clone(),
        server_cert.clone(),
        key,
        Arc::new(MemoryPeerStore::default()),
    )
}

async fn a_root_holding_device_certifies_a_new_one(carrier: Carrier) {
    let ada = Ada::start().await;
    let (_, cert, server_cert) = ada.certified(&ada.host.over(carrier)).await;
    assert_eq!(
        server_cert.device_public_key,
        ada.host_device.cert.device_public_key
    );
    assert_eq!(cert.user_public_key, ada.root.verifying_key().to_bytes());
    assert!(!cert.capabilities.can_admit_devices);
    ada.host.server.shutdown().await;
}

/// What a page holding the person's root runs: it lists the device waiting to be signed in as
/// one of theirs, and signs it in, over the link it keeps to call its host's ticket actions on.
async fn the_persons_page_signs_a_new_device_in(carrier: Carrier) {
    let ada = Ada::start().await;
    let host = ada.host.over(carrier);
    let (page_key, page_cert, _) = ada.certified(&host).await;
    let page = Device {
        key: page_key,
        cert: page_cert,
    };
    let kept = TicketLink::new(host.clone());
    let phone = SigningKey::generate(&mut UnwrapErr(SysRng));
    let (cert, server_cert) = ada.signed_in(&host, &phone, "Phone", (&kept, &page)).await;
    assert_eq!(cert.device_public_key, phone.verifying_key().to_bytes());
    assert_eq!(cert.user_public_key, ada.root.verifying_key().to_bytes());
    assert!(!cert.capabilities.can_admit_devices);
    assert_eq!(
        server_cert.device_public_key,
        ada.host_device.cert.device_public_key
    );
    let listed = ada
        .ask(&kept, &page.key, "pending", json!({ "as": page.cert }))
        .await
        .unwrap();
    assert_eq!(
        listed["waiting"],
        json!([]),
        "a signed-in tab waits no more"
    );
    kept.close().await;
    ada.host.server.shutdown().await;
}

/// A link's login ticket, as a page holding Ada's root shows one, logs in one tab: used again it
/// is refused, in words a person reads. And a page Ada removed from her devices approves nothing:
/// the tab that logs in with the link it showed before is turned away, saying why, though the
/// removed page still answers.
async fn a_links_ticket_is_refused_once_used_and_once_the_page_showing_it_is_removed(
    carrier: Carrier,
) {
    let ada = Ada::start().await;
    let host = ada.host.over(carrier);
    let (page_key, page_cert, _) = ada.certified(&host).await;
    let page = Device {
        key: page_key,
        cert: page_cert,
    };
    let shown = TicketLink::new(host.clone());

    let (id, ticket) = ada.show_link(&shown, &page).await;
    let first = tokio::spawn(log_in_with(
        host.clone(),
        ada.host_device.cert.device_public_key,
        ticket.clone(),
    ));
    ada.approve_asked(&shown, &page, &id).await;
    let answer: tilo_device_identity::login::CertifyAnswer =
        serde_json::from_value(first.await.unwrap().expect("the first tab logs in")).unwrap();
    assert_eq!(
        answer.cert.user_public_key,
        ada.root.verifying_key().to_bytes()
    );
    let again = log_in_with(host.clone(), ada.host_device.cert.device_public_key, ticket)
        .await
        .expect_err("a link's ticket logs in one tab");
    assert_eq!(again, "this ticket was used already: ask for a new one");

    // Ada removes the page from her devices, on her laptop; the link it showed before stays out,
    // though the removed page still answers, not knowing.
    let (id, ticket) = ada.show_link(&shown, &page).await;
    let laptop = ada.laptop_peer(&ada.host_device.cert).await;
    let removal = tilo_server_client::revoke_device(
        &ada.root,
        page.cert.device_public_key,
        tilo_clock::unix_now_ns(),
        RevocationReason::Retired,
    )
    .await
    .unwrap();
    laptop.send(Cmd::Revoke(removal.clone())).unwrap();
    wait_for("the server to take the removal in", || async {
        ada.host
            .server
            .workspaces()
            .unwrap()
            .state(WORKSPACE)
            .await?
            .revocations()
            .into_iter()
            .find(|held| held == &removal)
    })
    .await;
    let late = tokio::spawn(log_in_with(
        host.clone(),
        ada.host_device.cert.device_public_key,
        ticket,
    ));
    ada.approve_asked(&shown, &page, &id).await;
    let refused = late
        .await
        .unwrap()
        .expect_err("a removed page's approval lets nobody in");
    assert!(
        refused.contains("was removed from its person's devices"),
        "{refused}"
    );
    shown.close().await;
    laptop.close();
    ada.host.server.shutdown().await;
}

/// A tab, with a key of its own and a link of its own to the host `host` (its device key
/// `host_key`), logging in with a link's `ticket`: the host's answer, once the page that showed
/// the link answered.
async fn log_in_with(
    host: ServerAddress,
    host_key: [u8; 32],
    ticket: tilo_device_identity::login::Ticket,
) -> Result<serde_json::Value, String> {
    let tab = TicketLink::new(host);
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let serde_json::Value::Object(argument) = json!({
        "secret": ticket.secret_hex(),
        "device_public_key": hex::encode(key.verifying_key().to_bytes()),
        "agent": "Safari on iPhone",
    }) else {
        unreachable!("an argument is an object")
    };
    let answer = tab
        .ask(
            Arc::new(key.clone()),
            key.verifying_key().to_bytes(),
            host_key,
            "certify",
            argument,
            std::time::Duration::from_secs(20),
        )
        .await
        .unwrap();
    tab.close().await;
    answer
}

async fn documents_sync_both_ways_until_the_device_is_revoked(carrier: Carrier) {
    let ada = Ada::start().await;
    let (key, cert, server_cert) = ada.certified(&ada.host.over(carrier)).await;
    let page = page(&ada.host, carrier, key, &cert, &server_cert);
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    let laptop = ada.laptop_peer(&server_cert).await;
    ready(&[&page, &laptop]).await;

    // An acknowledged commit from the page reaches the laptop, and the
    // laptop's reply reaches the page.
    let mut note = automerge::AutoCommit::new();
    page.write(route(), &say(&mut note, "written on the page"))
        .unwrap();
    // Behind a lock: the receipt itself is not shared between threads, and
    // the wait holds it across its sleeps.
    let receipt = std::sync::Mutex::new(page.barrier().unwrap());
    wait_for("the commit to be durable", || {
        let durable = receipt.lock().unwrap().try_recv().ok();
        async move { durable }
    })
    .await
    .unwrap();
    let mut on_laptop = holds(&laptop, &route(), &note).await;
    laptop
        .write(route(), &say(&mut on_laptop, "typed on the laptop"))
        .unwrap();
    holds(&page, &route(), &on_laptop).await;
    assert_eq!(page.snapshot().synchronization, "acknowledged");

    // Revoked, the page is told so at its next hello, over every carrier, and stops there:
    // its server says why it turns the device away, where it used to close the hello with
    // nothing said and the page gave up as `not-admitted` after three tries. Ada's root signs
    // the revocation and her laptop's session announces it, as the web page's removal does.
    let mut entry = RevocationEntry {
        user_public_key: ada.root.verifying_key().to_bytes(),
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &ada.root).unwrap();
    laptop.send(Cmd::Revoke(entry)).unwrap();
    let failed = wait_for("the page to be refused", || {
        let snapshot = page.snapshot();
        async move { (snapshot.phase == "failed").then_some(snapshot) }
    })
    .await;
    assert_eq!(failed.error_code, Some("device-revoked"));
    assert_eq!(page.status().phase, TransportPhase::Failed);
    page.close();
    laptop.close();
    ada.host.server.shutdown().await;
}

/// A person removes one of their devices the way Settings does (their root signs the removal,
/// a session of theirs announces it), and the device is told so. Its server used to close the
/// removed device's hello with nothing said: the device read a server that was away, tried
/// again, and its page waited on "Can't reach …". Loaded again, the page's first hello is now
/// refused with its reason and its session stops there with `device-revoked`, the code its
/// page words as "This browser was removed from your devices"; the page that was open hears
/// the same when it asks again.
async fn a_removed_device_is_told_that_it_was_removed(carrier: Carrier) {
    let ada = Ada::start().await;
    let (key, cert, server_cert) = ada.certified(&ada.host.over(carrier)).await;
    let open = page(&ada.host, carrier, key.clone(), &cert, &server_cert);
    open.join_placed(WORKSPACE, "Workspace").unwrap();
    let laptop = ada.laptop_peer(&server_cert).await;
    ready(&[&open, &laptop]).await;

    // Ada removes the browser from her devices, on her laptop.
    let removal = tilo_server_client::revoke_device(
        &ada.root,
        cert.device_public_key,
        tilo_clock::unix_now_ns(),
        RevocationReason::Retired,
    )
    .await
    .unwrap();
    laptop.send(Cmd::Revoke(removal.clone())).unwrap();
    wait_for("the server to take the removal in", || async {
        ada.host
            .server
            .workspaces()
            .unwrap()
            .state(WORKSPACE)
            .await?
            .revocations()
            .into_iter()
            .find(|held| held == &removal)
    })
    .await;

    // The browser's page is loaded again: a new session of the same device. Its first hello
    // is refused and the refusal says why, so the session stops there, told once, and at no
    // moment reads as waiting for a server that is away.
    let loaded = page(&ada.host, carrier, key, &cert, &server_cert);
    loaded.join_placed(WORKSPACE, "Workspace").unwrap();
    let told = wait_for("the page loaded again to hear its server's answer", || {
        let (snapshot, status) = (loaded.snapshot(), loaded.status());
        async move {
            assert_ne!(
                status.phase,
                TransportPhase::Offline,
                "a removed device read its server as away: {:?}",
                status.failures
            );
            (snapshot.phase == "failed").then_some(snapshot)
        }
    })
    .await;
    assert_eq!(
        told.error_code,
        Some("device-revoked"),
        "a removed device was not told that it was removed: {:?}",
        told.error
    );
    assert_eq!(loaded.status().attempt, 0, "it asked once");
    loaded.close();

    // The page that was open when the removal came asks its server again, and hears the same.
    open.send(Cmd::Reconnect).unwrap();
    wait_for("the open page to hear that it was removed", || {
        let snapshot = open.snapshot();
        async move {
            (snapshot.phase == "failed" && snapshot.error_code == Some("device-revoked"))
                .then_some(())
        }
    })
    .await;
    open.close();

    // The laptop is the person's as before.
    assert_eq!(laptop.status().phase, TransportPhase::Connected);
    laptop.close();
    ada.host.server.shutdown().await;
}

/// The workspace scope's record: its channel keys seal the live session's tracks.
pub(crate) fn live_session_route() -> DocumentRoute {
    DocumentRoute {
        app_id: collab_types::scope::MEDIA_APP.to_owned(),
        cluster_id: WORKSPACE.to_owned(),
        document: collab_types::scope::WORKSPACE_RESOURCE.to_owned(),
    }
}

/// The scope's keys, as the workspace's members derive them.
pub(crate) fn live_session_keys() -> ContentKeys {
    KeyGroup::create(
        collab_types::WorkspaceId::new(),
        MemberKey::generate("ada").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

async fn the_workspace_admits_the_device_and_its_live_session_carries_it(carrier: Carrier) {
    let ada = Ada::start().await;
    let (key, cert, server_cert) = ada.certified(&ada.host.over(carrier)).await;
    place(&ada.host.server, &cert).await;
    let keys = live_session_keys();

    // The workspace's governance admits the page.
    let page = page(&ada.host, carrier, key, &cert, &server_cert);
    page.send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&page, crate::support::workspace()).await;
    let laptop = ada.laptop_peer(&server_cert).await;
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;

    // The page says where it is on the workspace's live session, and the laptop
    // hears it, opened with the scope's keys.
    let events = laptop.events();
    let said = Presence::pointing(WORKSPACE, 7, 0.25, 0.75);
    let (from, track, body) = wait_for("the laptop to hear the page on its live session", || {
        page.send(Cmd::Presence(
            crate::support::workspace(),
            vec![said.clone()],
        ))
        .unwrap();
        let events = events.drain();
        async move {
            events.into_iter().find_map(|event| match event {
                Event::LiveSession {
                    ws: workspace_id,
                    from,
                    track,
                    body,
                    ..
                } if workspace_id == crate::support::workspace() => Some((from, track, body)),
                _ => None,
            })
        }
    })
    .await;
    assert_eq!(from, cert.device_public_key);
    assert_eq!(track, tilo_live::live::Track::Presence);
    assert_eq!(tilo_live::presence::decode(&body).unwrap(), said);
    assert_eq!(page.live_note(), None, "the page reached its live session");

    page.close();
    laptop.close();
    ada.host.server.shutdown().await;
}

/// One of Ada's devices in the workspace's calls: its session, and the presence board it keeps of
/// what everyone else says, as a desktop's or a page's client keeps it.
pub(crate) struct Caller {
    pub(crate) link: Link,
    events: Events,
    pub(crate) device: [u8; 32],
    board: tilo_live::presence::Board,
    pub(crate) own: tilo_live::presence::Own,
    pub(crate) heard: Vec<Event>,
}

impl Caller {
    pub(crate) fn new(link: Link, device: [u8; 32], person: &[u8; 32]) -> Self {
        Self {
            events: link.events(),
            link,
            device,
            board: tilo_live::presence::Board::new(WORKSPACE, device),
            own: tilo_live::presence::Own {
                label: "Ada".into(),
                user: Some(hex::encode(person)),
                ..tilo_live::presence::Own::default()
            },
            heard: Vec::new(),
        }
    }

    /// Say this device is in `call` (or in none) on the workspace's presence.
    pub(crate) fn say(&mut self, call: Option<InCall>) {
        self.own.call = call;
        self.board.say(&self.own);
        let frames = self.board.flush(now_ms());
        self.send(frames);
    }

    /// Say what this device's presence owes now, as a desktop's or a page's client does on its
    /// clock: what changed, and all of it again every few seconds, since a frame said before the
    /// other devices were on the live session reached nobody.
    pub(crate) fn restate(&mut self) {
        let frames = self.board.due(now_ms()).unwrap_or_default();
        self.send(frames);
    }

    fn send(&self, frames: Vec<(u32, collab_types::presence::Presence)>) {
        if frames.is_empty() {
            return;
        }
        self.link
            .send(Cmd::Presence(
                crate::support::workspace(),
                frames.into_iter().map(|(_, frame)| frame).collect(),
            ))
            .unwrap();
    }

    /// Take what arrived: presence onto the board, the rest kept to look at.
    pub(crate) fn listen(&mut self) {
        for event in self.events.drain() {
            match event {
                Event::LiveSession {
                    from,
                    track: tilo_live::live::Track::Presence,
                    body,
                    ..
                } => {
                    self.board.heard(from, &body, now_ms());
                }
                event => self.heard.push(event),
            }
        }
    }

    /// The workspace's voice channels as this device lists them.
    pub(crate) fn channels(&self) -> Vec<tilo_live::calls::Channel> {
        tilo_live::calls::channels(
            &self.board.calls(),
            self.own.call.as_ref().map(|call| tilo_live::calls::Own {
                device: self.device,
                person: self.own.user.as_deref(),
                call,
            }),
            &self
                .link
                .workspace_devices(WORKSPACE)
                .into_iter()
                .filter_map(|(device, person)| {
                    let mut key = [0; 32];
                    hex::decode_to_slice(device, &mut key).ok()?;
                    Some((key, person))
                })
                .collect(),
            &self.link.call_rules(WORKSPACE),
        )
    }

    /// Whether this device heard `from`'s voice.
    fn heard_voice_of(&self, from: [u8; 32]) -> bool {
        self.heard
            .iter()
            .any(|event| matches!(event, Event::CallAudio { from: heard, .. } if *heard == from))
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0
}

/// A call in `channel`, as a device says it is in it.
pub(crate) fn in_call(channel: &str, camera: bool, muted: bool) -> InCall {
    InCall {
        call: collab_types::presence::CallPresence {
            channel: channel.into(),
            name: "Standup".into(),
            bridge: None,
        },
        camera,
        screen: false,
        muted,
        deafened: false,
    }
}

/// Ada's page and laptop meet in a voice channel by saying so in the workspace's presence: each
/// lists the other in it and hears the other's voice, the laptop sees the page turn its camera on
/// and mute, and once the page leaves the laptop is in the channel alone. A device of Ada's
/// without the workspace's channel keys opens none of the call's voice.
async fn the_device_takes_part_in_a_call(carrier: Carrier) {
    let ada = Ada::start().await;
    let person = ada.root.verifying_key().to_bytes();
    let keys = live_session_keys();
    let (key, cert, server_cert) = ada.certified(&ada.host.over(carrier)).await;
    place(&ada.host.server, &cert).await;
    let browser = page(&ada.host, carrier, key, &cert, &server_cert);
    browser
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    browser.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&browser, crate::support::workspace()).await;
    let laptop = ada.laptop_peer(&server_cert).await;
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;
    let (key, outsider_cert, _) = ada.certified(&ada.host.over(carrier)).await;
    place(&ada.host.server, &outsider_cert).await;
    let outsider = page(&ada.host, carrier, key, &outsider_cert, &server_cert);
    outsider
        .send(Cmd::InstallGroupKeys(
            live_session_route(),
            live_session_keys(),
        ))
        .unwrap();
    outsider.join_placed(WORKSPACE, "Workspace").unwrap();
    in_workspace(&outsider, crate::support::workspace()).await;
    let outsiders = outsider.events();

    let mut page = Caller::new(browser, cert.device_public_key, &person);
    let mut desk = Caller::new(laptop, ada.laptop.cert.device_public_key, &person);
    page.say(Some(in_call("standup", false, false)));
    desk.say(Some(in_call("standup", false, false)));
    let both = [page.device, desk.device];
    let in_standup = |caller: &Caller| {
        let channels = caller.channels();
        let [standup] = channels.as_slice() else {
            return None;
        };
        let mut devices: Vec<_> = standup.callers.iter().map(|one| one.device).collect();
        devices.sort_unstable();
        let mut expected = both;
        expected.sort_unstable();
        (standup.channel == "standup" && devices == expected).then(|| standup.clone())
    };
    let voice = |caller: &Caller| {
        caller
            .link
            .send(Cmd::CallAudio(
                crate::support::workspace(),
                vec![CallFrame {
                    timestamp_48k: 960,
                    opus: vec![0xf8, 0xff, 0xfe],
                }],
            ))
            .unwrap();
    };
    let standup = wait_for(
        "each to list the other in the channel and hear them",
        || {
            for caller in [&mut page, &mut desk] {
                caller.listen();
                caller.restate();
                voice(caller);
            }
            let met = in_standup(&page)
                .and_then(|_| in_standup(&desk))
                .filter(|_| page.heard_voice_of(desk.device) && desk.heard_voice_of(page.device));
            async move { met }
        },
    )
    .await;
    assert_eq!(standup.name, "Standup");
    assert_eq!(
        standup.people(),
        [hex::encode(person)],
        "one person on two devices"
    );

    page.say(Some(in_call("standup", true, true)));
    wait_for(
        "the laptop to see the page's camera on and microphone off",
        || {
            desk.listen();
            page.restate();
            let seen = desk.channels().iter().any(|channel| {
                channel.callers.iter().any(|caller| {
                    caller.device == page.device
                        && caller.camera
                        && caller.muted
                        && !caller.deafened
                })
            });
            async move { seen.then_some(()) }
        },
    )
    .await;

    page.say(None);
    wait_for("the laptop to be in the channel alone", || {
        desk.listen();
        page.restate();
        let channels = desk.channels();
        let alone = matches!(
            channels.as_slice(),
            [standup] if matches!(standup.callers.as_slice(), [only] if only.device == desk.device)
        );
        async move { alone.then_some(()) }
    })
    .await;

    let opened: Vec<Event> = outsiders
        .drain()
        .into_iter()
        .filter(|event| {
            matches!(event, Event::CallAudio { .. })
                || matches!(
                    event,
                    Event::LiveSession { from, .. } if both.contains(from)
                )
        })
        .collect();
    assert!(
        opened.is_empty(),
        "a device without the channel's keys opens none of the call: {opened:?}"
    );

    page.link.close();
    desk.link.close();
    outsider.close();
    ada.host.server.shutdown().await;
}

impl Ada {
    /// A page's way to Ada's server through a port that counts who sends to
    /// it. Every WebTransport session a device opens is a QUIC connection
    /// from a socket of its own, so the count is the sessions the device
    /// made: what a browser's network process holds a socket for.
    async fn counted(&self) -> (ServerAddress, Arc<AtomicUsize>) {
        use tokio::net::UdpSocket;
        let served = &self.host;
        let listens = served.web.addr;
        let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let mut link = served.web.link.clone();
        link.origin = format!("https://127.0.0.1:{}", front.local_addr().unwrap().port());
        let sessions = Arc::new(AtomicUsize::new(0));
        tokio::spawn({
            let sessions = Arc::clone(&sessions);
            async move {
                let mut backs =
                    std::collections::HashMap::<std::net::SocketAddr, Arc<UdpSocket>>::new();
                let mut packet = vec![0u8; 65_536];
                while let Ok((len, from)) = front.recv_from(&mut packet).await {
                    let back = if let Some(back) = backs.get(&from) {
                        Arc::clone(back)
                    } else {
                        let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
                        back.connect(listens).await.unwrap();
                        sessions.fetch_add(1, Ordering::SeqCst);
                        backs.insert(from, Arc::clone(&back));
                        tokio::spawn({
                            let (front, back) = (Arc::clone(&front), Arc::clone(&back));
                            async move {
                                let mut packet = vec![0u8; 65_536];
                                while let Ok(len) = back.recv(&mut packet).await {
                                    let sent =
                                        front.send_to(packet.get(..len).unwrap(), from).await;
                                    if sent.is_err() {
                                        break;
                                    }
                                }
                            }
                        });
                        back
                    };
                    let _ = back.send(packet.get(..len).unwrap()).await;
                }
            }
        });
        (
            served
                .iroh()
                .with_web(Some(link))
                .over(Carrier::WebTransport),
            sessions,
        )
    }

    /// Ada's server stops.
    async fn stop(self) {
        self.host.server.shutdown().await;
    }
}

/// More asks than a session holds streams open at once (256): each ask ends
/// its stream, or the link would take no more after those.
const ASKS: usize = 300;

/// What a page holding its person's root asks its server every few seconds while it is open
/// (who waits to be signed in): every ask goes over the one link the page keeps. A browser holds
/// a socket for every WebTransport session a page makes, and `WebKit` holds it after the session
/// closed for as long as its network process lives, so a link for each ask filled that process
/// until the browser could open nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_ask_of_a_page_goes_over_the_one_link_it_keeps() {
    let ada = Ada::start().await;
    let (reach, sessions) = ada.counted().await;
    let kept = TicketLink::new(reach);
    for _ in 0..ASKS {
        let listed = ada
            .ask(
                &kept,
                &ada.laptop.key,
                "pending",
                json!({ "as": ada.laptop.cert }),
            )
            .await
            .unwrap();
        assert_eq!(listed["waiting"], json!([]));
    }
    assert_eq!(
        sessions.load(Ordering::SeqCst),
        1,
        "three hundred asks made one WebTransport session"
    );
    kept.close().await;
    ada.stop().await;
}

/// Ask Ada's host, over `link`, to moderate the call in `channel` as `arguments` say (`operation`
/// and its own arguments), and answer what the call allows then.
pub(crate) fn moderate(
    link: &Link,
    host: [u8; 32],
    channel: &str,
    arguments: &serde_json::Value,
) -> serde_json::Value {
    use tilo_actions::remote::{
        Caller as ActionCaller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    };
    let mut arguments = arguments.as_object().cloned().unwrap();
    arguments.insert("channel".into(), channel.into());
    let api = tilo_session_actions::SessionActions::new(link, crate::support::workspace(), host);
    let receipt = tokio::task::block_in_place(|| {
        api.invoke(
            &ActionCaller::local_person("test"),
            InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget::workspace_governance(WORKSPACE, "call"),
                argument: InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .wait_timeout(std::time::Duration::from_secs(30))
    });
    match receipt {
        Ok(Some(Ok(receipt))) => match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap(),
            status => panic!("the host did not moderate the call: {status:?}"),
        },
        Ok(Some(Err(error))) => panic!("the host refused: {error:?}"),
        _ => panic!("the host did not answer"),
    }
}

/// Whether `caller` lists exactly `devices` in the standup's call.
fn lists(caller: &Caller, devices: &[[u8; 32]]) -> bool {
    let channels = caller.channels();
    let listed: std::collections::BTreeSet<[u8; 32]> = channels
        .iter()
        .filter(|channel| channel.channel == "standup")
        .flat_map(|channel| channel.callers.iter().map(|one| one.device))
        .collect();
    listed == devices.iter().copied().collect()
}

/// Wait until `one` and `other` each list `devices` in the standup's call and, when `heard`, each
/// hears the voice of the other.
async fn until_listed(
    what: &str,
    (one, other): (&mut Caller, &mut Caller),
    devices: &[[u8; 32]],
    heard: bool,
) {
    for caller in [&mut *one, &mut *other] {
        caller.heard.clear();
    }
    let met = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        wait_for(what, || {
            for caller in [&mut *one, &mut *other] {
                caller.listen();
                caller.restate();
                caller
                    .link
                    .send(Cmd::CallAudio(
                        crate::support::workspace(),
                        vec![CallFrame {
                            timestamp_48k: 960,
                            opus: vec![0xf8, 0xff, 0xfe],
                        }],
                    ))
                    .unwrap();
            }
            let done = lists(one, devices)
                && lists(other, devices)
                && (!heard
                    || (one.heard_voice_of(other.device) && other.heard_voice_of(one.device)));
            async move { done.then_some(()) }
        }),
    )
    .await;
    assert!(
        met.is_ok(),
        "timed out waiting for {what}: one lists {:?} and heard the other: {}; the other lists {:?} \
         and heard the first: {}",
        one.channels(),
        one.heard_voice_of(other.device),
        other.channels(),
        other.heard_voice_of(one.device),
    );
}

/// Neither `one` nor `other` hears the other's voice while both keep speaking for a moment: the
/// call carries none of it.
async fn unheard((one, other): (&mut Caller, &mut Caller)) {
    for caller in [&mut *one, &mut *other] {
        caller.listen();
        caller.heard.clear();
    }
    let heard = tokio::time::timeout(
        std::time::Duration::from_millis(1_500),
        wait_for("either to hear the other", || {
            for caller in [&mut *one, &mut *other] {
                caller.listen();
                caller
                    .link
                    .send(Cmd::CallAudio(
                        crate::support::workspace(),
                        vec![CallFrame {
                            timestamp_48k: 960,
                            opus: vec![0xf8, 0xff, 0xfe],
                        }],
                    ))
                    .unwrap();
            }
            let heard = one.heard_voice_of(other.device) || other.heard_voice_of(one.device);
            async move { heard.then_some(()) }
        }),
    )
    .await;
    assert!(heard.is_err(), "the call carries nobody's voice");
}

/// Ada owns the workspace and moderates its calls through its published `call` action. She and
/// Bo meet in the standup; when she ends it both drop out of it and neither hears the other, and
/// restarting it brings both back. Bo, denied the call, is listed by nobody and heard by nobody
/// however often his presence says he is in it, and hears nothing of it; let back in, he is in
/// it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_workspace_moderates_its_calls() {
    let ada = Ada::start().await;
    let host = ada.host_device.cert.device_public_key;
    let keys = live_session_keys();
    let laptop = ada.laptop_peer(&ada.host_device.cert).await;
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;
    let (bo_root, _) = UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "browser", DeviceCapabilities::member());
    let bo = bo_root.verifying_key().to_bytes();
    crate::support::redeem(
        &ada.host,
        crate::support::entry_grant(&ada.host, &ada.root, &ada.host_device, WORKSPACE, bo).await,
        &bo_browser.cert,
        &bo_browser.key,
    )
    .await;
    let browser = start_peer(
        ada.host.iroh(),
        bo_browser.cert.clone(),
        ada.host_device.cert.clone(),
        bo_browser.key.clone(),
    );
    browser
        .send(Cmd::InstallGroupKeys(live_session_route(), keys))
        .unwrap();
    browser
        .send(Cmd::Join(crate::support::workspace()))
        .unwrap();
    in_workspace(&browser, crate::support::workspace()).await;

    let mut desk = Caller::new(
        laptop,
        ada.laptop.cert.device_public_key,
        &ada.root.verifying_key().to_bytes(),
    );
    let mut page = Caller::new(browser, bo_browser.cert.device_public_key, &bo);
    desk.say(Some(in_call("standup", false, false)));
    page.say(Some(in_call("standup", false, false)));
    let both = [desk.device, page.device];
    until_listed(
        "Ada and Bo to meet in the standup",
        (&mut desk, &mut page),
        &both,
        true,
    )
    .await;

    let ended = moderate(&desk.link, host, "standup", &json!({"operation": "end"}));
    assert_eq!(ended["call"]["ended"], true);
    until_listed(
        "the ended standup to list nobody",
        (&mut desk, &mut page),
        &[],
        false,
    )
    .await;
    unheard((&mut desk, &mut page)).await;

    moderate(
        &desk.link,
        host,
        "standup",
        &json!({"operation": "restart"}),
    );
    until_listed(
        "the restarted standup to carry both",
        (&mut desk, &mut page),
        &both,
        true,
    )
    .await;

    moderate(
        &desk.link,
        host,
        "standup",
        &json!({"operation": "deny", "person": hex::encode(bo)}),
    );
    // Bo says again that he is in the call: still nobody lists him or hears him.
    page.say(Some(in_call("standup", true, false)));
    let ada_alone = [desk.device];
    until_listed(
        "the standup to leave Bo out",
        (&mut desk, &mut page),
        &ada_alone,
        false,
    )
    .await;
    unheard((&mut desk, &mut page)).await;

    moderate(
        &desk.link,
        host,
        "standup",
        &json!({"operation": "allow", "person": hex::encode(bo)}),
    );
    until_listed(
        "Bo to be back in the standup",
        (&mut desk, &mut page),
        &both,
        true,
    )
    .await;

    desk.link.close();
    page.link.close();
    ada.host.server.shutdown().await;
}

/// A device's live session as a client modified to ignore the workspace's call rules holds it: it
/// joins the host's live origin with the workspace's keys, is told no rule, holds `keys` as the
/// standup's keys, and so sends and opens every call's media it can get. Its presence says it is
/// in the standup.
async fn ignoring_the_rules(
    ada: &Ada,
    device: &crate::support::Device,
    roster: &[[u8; 32]],
    keys: &ContentKeys,
) -> tilo_live::live_session::LiveSession {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.key.to_bytes()))
        .bind()
        .await
        .unwrap();
    let scope = tilo_live::scope::Scope::workspace(WORKSPACE);
    let grant = tilo_live::grant::Native::new(
        *crate::support::workspace().as_uuid().as_bytes(),
        &scope.id(),
        keys.epoch(),
        serde_json::to_vec(&device.cert).unwrap(),
        Vec::new(),
    )
    .unwrap()
    .encode();
    let live_session = tilo_live::live_session::LiveSession::join(
        Arc::new(tilo_live::native::Iroh::new(
            endpoint,
            ada.host.server.node_addr(),
        )),
        &grant,
        tilo_live::live_session::Join {
            scope,
            device: device.cert.device_public_key,
            keys: keys.clone().into(),
            roster: tilo_live::roster::Roster::new(roster.iter().copied()),
            publish: true,
            presence: true,
        },
    )
    .await
    .unwrap();
    claim(&live_session, 1);
    // Keys of the call it holds, whatever the call's rule now says: the live origin is what holds
    // it to the rule.
    live_session.set_call_keys("standup", 0, keys.clone());
    live_session
}

/// Say on `live_session`'s presence (at `sequence`) that its device is in the standup.
fn claim(live_session: &tilo_live::live_session::LiveSession, sequence: u32) {
    let said = Presence::desktop(
        WORKSPACE,
        collab_types::presence::Payload::Availability {
            state: "available".into(),
            call: Some(in_call("standup", false, false).encode()),
            camera: false,
            screen: false,
        },
    );
    tilo_live::presence::say(live_session, &[(sequence, said)]).unwrap();
}

/// Speak one voice frame on each of `devices` (as far as anyone asks for it), and count what each
/// heard of the other's voice since.
fn speak_and_hear(
    devices: [&tilo_live::live_session::LiveSession; 2],
    keys: [[u8; 32]; 2],
    sequence: &mut u32,
) -> [usize; 2] {
    *sequence = sequence.wrapping_add(1);
    let frame = tilo_live::voice::VoiceObject {
        sequence: u64::from(*sequence),
        timestamp: u64::from(*sequence).saturating_mul(u64::from(tilo_live::voice::FRAME_SAMPLES)),
        duration: tilo_live::voice::FRAME_SAMPLES,
        speaker: None,
        payload: vec![0xf8, 0xff, 0xfe],
    }
    .encode()
    .unwrap();
    for device in devices {
        // Nobody asking for it, or the device's own call refusing it, is what is tested here.
        let _ = device.publish(tilo_live::live::Track::Audio, *sequence, &frame);
    }
    let mut heard = [0; 2];
    for (index, device) in devices.iter().enumerate() {
        let other = keys[1 - index];
        while let Some(object) = device.try_recv() {
            if object.track == tilo_live::live::Track::Audio && object.sender == other {
                heard[index] += 1;
            }
        }
    }
    heard
}

/// Wait until each of `devices` hears the other's voice.
async fn until_both_heard(
    what: &str,
    devices: [&tilo_live::live_session::LiveSession; 2],
    keys: [[u8; 32]; 2],
    sequence: &mut u32,
) {
    let mut heard = [0; 2];
    wait_for(what, || {
        let now = speak_and_hear(devices, keys, sequence);
        heard = [heard[0] + now[0], heard[1] + now[1]];
        let done = heard[0] > 0 && heard[1] > 0;
        async move { done.then_some(()) }
    })
    .await;
}

/// Wait until neither of `devices` hears the other's voice for a second and a half while both
/// keep speaking: the live origin carries none of it between them.
async fn until_unheard(
    what: &str,
    devices: [&tilo_live::live_session::LiveSession; 2],
    keys: [[u8; 32]; 2],
    sequence: &mut u32,
) {
    let mut quiet_since = std::time::Instant::now();
    wait_for(what, || {
        if speak_and_hear(devices, keys, sequence) != [0, 0] {
            quiet_since = std::time::Instant::now();
        }
        let done = quiet_since.elapsed() >= std::time::Duration::from_millis(1_500);
        async move { done.then_some(()) }
    })
    .await;
}

/// The host's live origin holds every call to the workspace's rule for it, whatever a device's
/// client does: Ada's tablet and Bo's browser both run a client that ignores the rules and holds
/// the workspace's keys. Once Ada denies Bo the standup, the origin carries none of his voice to
/// her and none of hers to him, whether his presence says he is in the call or not. Let back in,
/// both hear each other again; once she ends the call the origin carries nobody's voice, until
/// she restarts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_live_origin_holds_a_call_to_its_rule_whatever_a_client_does() {
    let ada = Ada::start().await;
    let host = ada.host_device.cert.device_public_key;
    let keys = live_session_keys();
    let laptop = ada.laptop_peer(&ada.host_device.cert).await;
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;
    let tablet = enrol(&ada.root, "tablet", DeviceCapabilities::member());
    place(&ada.host.server, &tablet.cert).await;
    let (bo_root, _) = UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "browser", DeviceCapabilities::member());
    let bo = bo_root.verifying_key().to_bytes();
    crate::support::redeem(
        &ada.host,
        crate::support::entry_grant(&ada.host, &ada.root, &ada.host_device, WORKSPACE, bo).await,
        &bo_browser.cert,
        &bo_browser.key,
    )
    .await;

    let devices = [
        tablet.cert.device_public_key,
        bo_browser.cert.device_public_key,
    ];
    let ada_tablet = ignoring_the_rules(&ada, &tablet, &devices, &keys).await;
    let bo_page = ignoring_the_rules(&ada, &bo_browser, &devices, &keys).await;
    let both = [&ada_tablet, &bo_page];
    let mut sequence = 0;
    until_both_heard(
        "Ada and Bo to hear each other in the standup",
        both,
        devices,
        &mut sequence,
    )
    .await;

    moderate(
        &laptop,
        host,
        "standup",
        &json!({"operation": "deny", "person": hex::encode(bo)}),
    );
    until_unheard(
        "the live origin to carry nothing between Ada and the denied Bo",
        both,
        devices,
        &mut sequence,
    )
    .await;
    // His presence no longer says he is in the call, and his client still sends its voice: the
    // origin carries none of it either way.
    tilo_live::presence::say(
        &bo_page,
        &[(
            2,
            Presence::desktop(
                WORKSPACE,
                collab_types::presence::Payload::Status {
                    text: "elsewhere".into(),
                    automatic: false,
                },
            ),
        )],
    )
    .unwrap();
    until_unheard(
        "the live origin to carry nothing of Bo claiming no call",
        both,
        devices,
        &mut sequence,
    )
    .await;
    claim(&bo_page, 3);
    until_unheard(
        "the live origin to carry nothing of Bo claiming the call",
        both,
        devices,
        &mut sequence,
    )
    .await;

    moderate(
        &laptop,
        host,
        "standup",
        &json!({"operation": "allow", "person": hex::encode(bo)}),
    );
    until_both_heard(
        "Bo let back in to hear Ada and be heard",
        both,
        devices,
        &mut sequence,
    )
    .await;

    let ended = moderate(&laptop, host, "standup", &json!({"operation": "end"}));
    assert_eq!(ended["call"]["ended"], true);
    until_unheard(
        "the live origin to carry nothing of the ended call",
        both,
        devices,
        &mut sequence,
    )
    .await;

    moderate(&laptop, host, "standup", &json!({"operation": "restart"}));
    until_both_heard(
        "the restarted call to carry both again",
        both,
        devices,
        &mut sequence,
    )
    .await;

    laptop.close();
    ada.host.server.shutdown().await;
}

/// Ada and Bo talk in the standup while Ada denies someone else the call: its media keys move on
/// to the rule's next generation, handed to each of them through the workspace's key groups, and
/// neither goes without the other's voice for longer than one key exchange takes (the longest a
/// device waits for a fresh group's genesis, [`tilo_workspace_keys::founding_wait`]).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_calls_keys_move_on_within_one_key_exchange() {
    let ada = Ada::start().await;
    let host = ada.host_device.cert.device_public_key;
    let keys = live_session_keys();
    let laptop = ada.laptop_peer(&ada.host_device.cert).await;
    laptop
        .send(Cmd::InstallGroupKeys(live_session_route(), keys.clone()))
        .unwrap();
    in_workspace(&laptop, crate::support::workspace()).await;
    let (bo_root, _) = UserIdentity::generate("Bo", 1);
    let bo_browser = enrol(&bo_root, "browser", DeviceCapabilities::member());
    let bo = bo_root.verifying_key().to_bytes();
    crate::support::redeem(
        &ada.host,
        crate::support::entry_grant(&ada.host, &ada.root, &ada.host_device, WORKSPACE, bo).await,
        &bo_browser.cert,
        &bo_browser.key,
    )
    .await;
    let browser = start_peer(
        ada.host.iroh(),
        bo_browser.cert.clone(),
        ada.host_device.cert.clone(),
        bo_browser.key.clone(),
    );
    browser
        .send(Cmd::InstallGroupKeys(live_session_route(), keys))
        .unwrap();
    browser
        .send(Cmd::Join(crate::support::workspace()))
        .unwrap();
    in_workspace(&browser, crate::support::workspace()).await;

    let mut desk = Caller::new(
        laptop,
        ada.laptop.cert.device_public_key,
        &ada.root.verifying_key().to_bytes(),
    );
    let mut page = Caller::new(browser, bo_browser.cert.device_public_key, &bo);
    desk.say(Some(in_call("standup", false, false)));
    page.say(Some(in_call("standup", false, false)));
    let both = [desk.device, page.device];
    until_listed(
        "Ada and Bo to meet in the standup",
        (&mut desk, &mut page),
        &both,
        true,
    )
    .await;

    // Both keep speaking, a frame every 20 ms, and the rule changes once each hears the other.
    let deadline = tilo_test_env::hang_deadline();
    let (ada_device, bo_device) = (desk.device, page.device);
    let mut last = [None::<std::time::Instant>; 2];
    while last.iter().any(Option::is_none) {
        assert!(
            std::time::Instant::now() < deadline,
            "Ada and Bo never heard each other"
        );
        for (index, (speaker, other)) in [(&mut desk, bo_device), (&mut page, ada_device)]
            .into_iter()
            .enumerate()
        {
            if speak_once(speaker, other) {
                last[index] = Some(std::time::Instant::now());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let mut last = last.map(Option::unwrap);
    let moderation = tokio::task::spawn_blocking({
        let link = desk.link.clone();
        move || {
            moderate(
                &link,
                host,
                "standup",
                &json!({"operation": "deny", "person": hex::encode([0xc7; 32])}),
            )
        }
    });
    let mut longest = [std::time::Duration::ZERO; 2];
    let mut moved_at: Option<std::time::Instant> = None;
    let mut heard_since = [false; 2];
    loop {
        let now = std::time::Instant::now();
        assert!(now < deadline, "the call's new keys never arrived");
        for (index, (speaker, other)) in [(&mut desk, bo_device), (&mut page, ada_device)]
            .into_iter()
            .enumerate()
        {
            if speak_once(speaker, other) {
                longest[index] = longest[index].max(now.duration_since(last[index]));
                last[index] = now;
                if moved_at.is_some() {
                    heard_since[index] = true;
                }
            }
        }
        let moved = [&desk, &page].iter().all(|caller| {
            caller
                .link
                .call_rules(WORKSPACE)
                .get("standup")
                .is_some_and(|rule| rule.generation == 1)
        });
        if moved && moved_at.is_none() {
            moved_at = Some(now);
        }
        if moved_at.is_some_and(|at| now.duration_since(at) > std::time::Duration::from_secs(1))
            && heard_since == [true, true]
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    moderation.await.unwrap();
    let gap = longest[0].max(longest[1]);
    eprintln!(
        "the longest either went without the other's voice while the call's keys moved on: {} ms \
         (Ada {} ms, Bo {} ms)",
        gap.as_millis(),
        longest[0].as_millis(),
        longest[1].as_millis()
    );
    assert!(
        gap < tilo_workspace_keys::founding_wait(),
        "the call went silent for {gap:?} while its keys moved on"
    );

    desk.link.close();
    page.link.close();
    ada.host.server.shutdown().await;
}

/// `speaker` says one frame of the call, and whether it heard `other`'s voice since it last
/// spoke.
fn speak_once(speaker: &mut Caller, other: [u8; 32]) -> bool {
    speaker.heard.clear();
    speaker.listen();
    speaker.restate();
    speaker
        .link
        .send(Cmd::CallAudio(
            crate::support::workspace(),
            vec![CallFrame {
                timestamp_48k: 960,
                opus: vec![0xf8, 0xff, 0xfe],
            }],
        ))
        .unwrap();
    speaker.heard_voice_of(other)
}
