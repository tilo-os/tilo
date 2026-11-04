//! A page that only watches a public workspace reads what its members seal: the
//! workspace records it as one that only watches, each public record's channel
//! adds its key package to the group, and the group's own Welcome gives it the
//! keys. It writes nothing, and what a grant keeps from some of the room stays
//! sealed from it. The node is never asked for a record's keys.

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
    Host, HttpClient, HttpsLink, MemoryPeerStore, PeerConfig, PeerEvent, PeerHandle,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::https as carrier;
use tilo_relay_protocol::workspaces::{
    JoinReply, Workspace, WorkspaceRole, WorkspacesReply, WorkspacesRequest,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use support::{TestHost, admitted, enrol, start_peer, start_workspace_host, wait_for};

async fn workspaces(link: &HttpsLink, request: WorkspacesRequest) -> Workspace {
    match carrier::workspaces(link.client().as_ref(), link.session(), &request)
        .await
        .unwrap()
    {
        WorkspacesReply::Workspace(workspace) => workspace,
        other => panic!("expected a workspace, got {other:?}"),
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

    fn of(inner: Arc<dyn HttpClient>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            posted: Mutex::default(),
        })
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

/// A watching page of the spectator person `root`, posting through `paths`:
/// the certificate the workspace's host signs for a visitor, an hour long and
/// carrying no capabilities.
fn watching_page(root: &SigningKey, host: &DeviceCert, paths: Arc<Paths>) -> PeerHandle {
    let key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Watching visitor".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    PeerHandle::start(
        PeerConfig {
            host: Host::Https(paths),
            cert,
            host_cert: host.clone(),
            revocations: RevocationList::new(),
            watching: true,
            background: false,
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    )
}

/// Whether `page` takes part in `route`'s channel with its keys.
fn takes_part(page: &PeerHandle, route: &DocumentRoute) -> bool {
    let note = format!("route {}/{} ", route.app_id, route.record);
    page.status()
        .channels
        .lines()
        .any(|line| line.starts_with(&note) && line.contains(": ready;"))
}

/// The events `page` holds for `route`, opening none of the rest.
fn drained_for(page: &PeerHandle, route: &DocumentRoute) -> Vec<PeerEvent> {
    page.drain()
        .into_iter()
        .filter(|event| match event {
            PeerEvent::Document(document) => document.route == *route,
            _ => false,
        })
        .collect()
}

/// Every file under `root`, with its path.
fn files_under(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                found.push((path, bytes));
            }
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watching_page_reads_a_public_workspaces_sealed_records_and_writes_nothing() {
    let alice = SigningKey::generate(&mut UnwrapErr(SysRng));
    // A workspace service's node, which takes part in its workspaces' channels.
    let host = enrol(
        &alice,
        "workspaces",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let page = enrol(&alice, "alice-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let TestHost { relay, web, .. } =
        start_workspace_host(&alice, &host, state.path(), false).await;
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
    let created = workspaces(
        &owner,
        WorkspacesRequest::Create {
            name: "Plaza".into(),
        },
    )
    .await;
    workspaces(
        &owner,
        WorkspacesRequest::Enter {
            workspace: created.workspace.clone(),
        },
    )
    .await;
    let workspace_id = created.workspace_id;

    // The spectator person is a viewer of the workspace, as its host makes it.
    let WorkspacesReply::Invitation {
        signed_invitation, ..
    } = carrier::workspaces(
        owner.client().as_ref(),
        owner.session(),
        &WorkspacesRequest::Invite {
            workspace: created.workspace.clone(),
            request_id: [1; 32],
            role: WorkspaceRole::Viewer,
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
    workspaces(
        &owner,
        WorkspacesRequest::Admit {
            workspace: created.workspace.clone(),
            request_id: [2; 32],
        },
    )
    .await;
    // A public workspace: anyone may join it as a guest.
    relay
        .admit_spectators(
            workspace_id,
            tilo_relay::Spectators {
                root: spectator.verifying_key().to_bytes(),
                seats: 4,
                public: true,
            },
        )
        .await
        .unwrap();

    // Alice writes in a note, sealed with its channel's keys.
    let route = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: created.workspace.clone(),
        record: "note-1".into(),
    };
    let alice_page = start_peer(
        Host::Https(https.clone()),
        page.cert.clone(),
        host.cert.clone(),
        page.key.clone(),
    );
    alice_page.join(workspace_id).unwrap();
    let started = DocumentMutation::Snapshot(b"the note is open".to_vec());
    alice_page.publish(route.clone(), &started).unwrap();
    admitted(&alice_page).await;
    wait_for("Alice's first write to be committed", || {
        let committed = alice_page.status().outgoing_pending == 0;
        async move { committed.then_some(()) }
    })
    .await;

    // The watching page takes part in the note's channel: the workspace records
    // it as one that only watches, the channel adds its key package to the
    // group, and the group's own Welcome hands it the keys.
    let paths = Paths::of(https.clone());
    let watcher = watching_page(&spectator, &host.cert, paths.clone());
    let watcher_device = watcher.status().device;
    watcher.join(workspace_id).unwrap();
    watcher.open_route(route.clone()).unwrap();
    wait_for("the page to join the note's channel", || {
        let joined = takes_part(&watcher, &route);
        async move { joined.then_some(()) }
    })
    .await;

    // It writes nothing: the page answers its own write without sealing or
    // sending it, so nothing it makes reaches Alice, or anyone.
    let _ = alice_page.drain();
    watcher
        .publish(
            route.clone(),
            &DocumentMutation::Incremental(b"a spectator's edit".to_vec()),
        )
        .unwrap();

    // Alice's next word is sealed with the channel's keys, and the page opens
    // it with the keys the group gave it.
    let hello = DocumentMutation::Snapshot(b"hello from Alice".to_vec());
    alice_page.publish(route.clone(), &hello).unwrap();
    let opened = wait_for("the page to open Alice's sealed note", || {
        let events = drained_for(&watcher, &route);
        async move {
            events.into_iter().find_map(|event| match event {
                PeerEvent::Document(document)
                    if matches!(
                        document.mutation(),
                        Some(DocumentMutation::Snapshot(bytes)) if bytes == b"hello from Alice"
                    ) =>
                {
                    Some((document.author, document.envelope.encryption.is_some()))
                }
                _ => None,
            })
        }
    })
    .await;
    assert_eq!(opened, (page.cert.device_public_key, true));
    assert!(
        drained_for(&alice_page, &route).iter().all(
            |event| !matches!(event, PeerEvent::Document(document) if document.author == watcher_device)
        ),
        "a watching page wrote into a member's record"
    );
    assert!(drained_for(&watcher, &route).iter().all(
        |event| !matches!(event, PeerEvent::Document(document) if document.author == watcher_device)
    ));

    // A record Alice's own person alone reads stays sealed from the page: its
    // channel has no keys for a visitor however it asks.
    let personal = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: created.workspace.clone(),
        record: collab_types::member_record_id(
            &hex::encode(alice.verifying_key().to_bytes()),
            "notes",
        ),
    };
    watcher.open_route(personal.clone()).unwrap();
    alice_page.open_route(personal.clone()).unwrap();
    alice_page
        .publish(
            personal.clone(),
            &DocumentMutation::Snapshot(b"Alice's own note starts".to_vec()),
        )
        .unwrap();
    wait_for("Alice's page to take part in her own record", || {
        let joined = takes_part(&alice_page, &personal);
        async move { joined.then_some(()) }
    })
    .await;
    alice_page
        .publish(
            personal.clone(),
            &DocumentMutation::Snapshot(b"Alice's own note".to_vec()),
        )
        .unwrap();
    wait_for("the page to hold Alice's sealed own note", || {
        let held = watcher
            .status()
            .routes
            .get(&personal)
            .is_some_and(|route| route.awaiting_keys > 0);
        async move { held.then_some(()) }
    })
    .await;
    assert!(
        drained_for(&watcher, &personal)
            .iter()
            .all(|event| !matches!(
                event,
                PeerEvent::Document(document)
                    if matches!(
                        document.mutation(),
                        Some(DocumentMutation::Snapshot(bytes)) if bytes == b"Alice's own note"
                    )
            )),
        "a watching page opened a record kept from it"
    );
    assert!(
        !takes_part(&watcher, &personal),
        "a watching page joined a channel kept from it: {}",
        watcher.status().channels
    );

    // The node's own state holds both records sealed: it kept the frames and
    // nothing that opens them. The page's keys came from the group it had
    // joined, and it never asked the node for a record's.
    for (path, bytes) in files_under(state.path()) {
        for plaintext in [b"hello from Alice".as_slice(), b"Alice's own note"] {
            assert!(
                !bytes
                    .windows(plaintext.len())
                    .any(|window| window == plaintext),
                "the node kept a record in the clear: {}",
                path.display()
            );
        }
    }
    assert!(
        paths.asked("/channel-keys").is_empty(),
        "the node was asked for a record's keys: {:?}",
        paths.asked("/channel-keys")
    );
    assert!(paths.asked("/here").is_empty());
    assert_ne!(watcher_device, [0; 32]);

    watcher.close();
    alice_page.close();
    relay.shutdown().await;
}
