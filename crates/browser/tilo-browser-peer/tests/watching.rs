//! A page that only watches a public room, as its spectator: it reads what
//! the members seal in the room's records with the keys the room's steward
//! hands it, and writes nothing, not even a request for a channel's access.
//! In a room that is not public it gets no keys and opens nothing.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

// This binary re-execs as the automerge worker (support is shared with crates that lack the dep).
tilo_automerge_isolate::test_worker!();

use std::sync::{Arc, Mutex};

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_browser_peer::invite::{ask_to_join, guest_certificate};
use tilo_browser_peer::{
    DocumentRoute, Host, HttpClient, HttpsLink, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle,
};
use tilo_relay_client::https as carrier;
use tilo_relay_protocol::rooms::{JoinReply, Room, RoomRole, RoomsReply, RoomsRequest};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use support::{TestHost, admitted, enrol, next_document, start_peer, start_room_host, wait_for};

async fn rooms(link: &HttpsLink, request: RoomsRequest) -> Room {
    match carrier::rooms(link.client().as_ref(), link.session(), &request)
        .await
        .unwrap()
    {
        RoomsReply::Room(room) => room,
        other => panic!("expected a room, got {other:?}"),
    }
}

/// An HTTPS client that keeps every path the page posted, with the answer's
/// status.
struct Paths {
    inner: Arc<dyn HttpClient>,
    posted: Mutex<Vec<(String, u16)>>,
}

impl Paths {
    fn asked(&self, path: &str) -> Vec<u16> {
        self.posted
            .lock()
            .unwrap()
            .iter()
            .filter(|(posted, _)| posted.ends_with(path))
            .map(|(_, status)| *status)
            .collect()
    }
}

impl HttpClient for Paths {
    fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> tilo_browser_peer::HttpFuture<'_> {
        let path = path.to_owned();
        let answer = self.inner.post(&path, session, body);
        Box::pin(async move {
            let answer = answer.await;
            if let Ok((status, _)) = &answer {
                self.posted.lock().unwrap().push((path, *status));
            }
            answer
        })
    }
}

/// A watching page of the spectator person `root`, posting through `paths`.
async fn watching_page(root: &SigningKey, host: &DeviceCert, paths: Arc<Paths>) -> PeerHandle {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let cert = guest_certificate(
        root,
        key.verifying_key().to_bytes(),
        "Watching visitor",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    PeerHandle::start(
        PeerConfig {
            host: Host::Https(paths),
            cert,
            host_cert: host.clone(),
            revocations: RevocationList::new(),
            watching: true,
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watching_page_opens_a_public_rooms_records_and_writes_nothing() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    // A room service's node, which stewards its rooms' channels.
    let host = enrol(
        &alice,
        "rooms",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let page = enrol(&alice, "alice-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web } = start_room_host(&alice, &host, state.path(), false).await;
    let https: Arc<dyn HttpClient> =
        support::https::DevHttps::new(web.addr, web.certificate_sha256.as_ref().unwrap());
    let owner = HttpsLink::connect(
        https.clone(),
        &page.cert,
        host.cert.device_public_key,
        &RevocationList::new(),
        &page.key,
    )
    .await
    .unwrap();
    let created = rooms(
        &owner,
        RoomsRequest::Create {
            name: "Plaza".into(),
        },
    )
    .await;
    rooms(
        &owner,
        RoomsRequest::Enter {
            room: created.room.clone(),
        },
    )
    .await;
    let workspace_id = created.workspace_id;

    // The spectator person is a viewer of the room, as its host makes it.
    let RoomsReply::Invitation {
        signed_invitation, ..
    } = carrier::rooms(
        owner.client().as_ref(),
        owner.session(),
        &RoomsRequest::Invite {
            room: created.room.clone(),
            request_id: [1; 32],
            role: RoomRole::Viewer,
            expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
            inviter_name: None,
            wait_list_entry: None,
        },
    )
    .await
    .unwrap()
    else {
        panic!("no invitation");
    };
    let spectator = SigningKey::generate(&mut UnwrapErr(SysRng));
    let joined_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let joined = guest_certificate(
        &spectator,
        joined_key.verifying_key().to_bytes(),
        "Watching visitors",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    assert_eq!(
        ask_to_join(
            https.as_ref(),
            &signed_invitation,
            &joined,
            &joined_key,
            [2; 32],
            "Watching visitors",
        )
        .await
        .unwrap(),
        JoinReply::Waiting
    );
    rooms(
        &owner,
        RoomsRequest::Admit {
            room: created.room.clone(),
            request_id: [2; 32],
        },
    )
    .await;
    let spectators = |public| tilo_relay::Spectators {
        root: spectator.verifying_key().to_bytes(),
        seats: 4,
        public,
    };
    relay
        .admit_spectators(workspace_id, spectators(true))
        .await
        .unwrap();

    // Alice writes in a note, sealed with its channel's keys.
    let route = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: created.room.clone(),
        record: "note-1".into(),
    };
    let alice_page = start_peer(
        Host::Https(https.clone()),
        page.cert.clone(),
        host.cert.clone(),
        page.key.clone(),
    );
    alice_page.join(workspace_id).unwrap();
    let hello = DocumentMutation::Snapshot(b"hello from Alice".to_vec());
    alice_page.publish(route.clone(), &hello).unwrap();
    admitted(&alice_page).await;
    wait_for("Alice's note to be committed", || {
        let committed = alice_page.status().outgoing_pending == 0;
        async move { committed.then_some(()) }
    })
    .await;

    // A watching page opens it with the keys the steward hands it.
    let paths = Arc::new(Paths {
        inner: https.clone(),
        posted: Mutex::default(),
    });
    let watcher = watching_page(&spectator, &host.cert, paths.clone()).await;
    watcher.join(workspace_id).unwrap();
    // What a page does as a member, it does not do watching.
    watcher.live_join(workspace_id).unwrap();
    watcher.open_route(route.clone()).unwrap();
    watcher
        .publish(
            route.clone(),
            &DocumentMutation::Incremental(b"a spectator's edit".to_vec()),
        )
        .unwrap();
    let mut pending = Vec::new();
    assert_eq!(next_document(&watcher, &mut pending).await, hello);
    assert_eq!(paths.asked("/channel-keys").first(), Some(&200));
    assert!(paths.asked("/here").is_empty());

    // Alice's next write reaches the page live; nothing of the page's
    // reached her or was refused it.
    let again = DocumentMutation::Incremental(b"and again".to_vec());
    alice_page.publish(route.clone(), &again).unwrap();
    assert_eq!(next_document(&watcher, &mut pending).await, again);
    let status = watcher.status();
    assert!(status.failures.is_empty(), "{:?}", status.failures);
    assert_eq!(status.outgoing_pending, 0);
    assert!(
        alice_page
            .drain()
            .iter()
            .all(|event| !matches!(event, PeerEvent::Document(_))),
        "the watching page wrote to Alice"
    );

    // In a room that is not public a watching page reads the room but none
    // of what its members seal.
    relay
        .admit_spectators(workspace_id, spectators(false))
        .await
        .unwrap();
    let paths = Arc::new(Paths {
        inner: https.clone(),
        posted: Mutex::default(),
    });
    let sealed_off = watching_page(&spectator, &host.cert, paths.clone()).await;
    sealed_off.join(workspace_id).unwrap();
    wait_for("the page to hold Alice's sealed note", || {
        let held = sealed_off
            .status()
            .routes
            .get(&route)
            .is_some_and(|route| route.awaiting_keys > 0)
            && !paths.asked("/channel-keys").is_empty();
        async move { held.then_some(()) }
    })
    .await;
    assert_eq!(paths.asked("/channel-keys"), vec![403]);
    assert!(
        sealed_off
            .drain()
            .iter()
            .all(|event| !matches!(event, PeerEvent::Document(_))),
        "a page opened a private room's note"
    );

    sealed_off.close();
    watcher.close();
    alice_page.close();
    relay.shutdown().await;
}
