//! A page that only watches a workspace, as its spectator, asks its records'
//! channels for access like any device and writes nothing. The workspace
//! refuses it by name, in a public workspace too: a key opens a channel for the
//! devices its record's readers name, and no route hands a page one. It holds
//! what members sealed and opens none of it, and the node never sees a key to
//! give.

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
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};

use support::{TestHost, admitted, enrol, start_peer, start_workspace_host, wait_for};

/// The refusal a watching page is answered with when it asks a record's
/// channel for access.
const SEALED: &str = "this workspace opens a record's channel only to the devices its readers name, not to those who only watch";

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
            background: false,
        },
        Arc::new(key),
        Arc::new(MemoryPeerStore::default()),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watching_page_is_refused_a_records_channel_and_opened_none_of_it() {
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
    // A public workspace: anyone may join it as a guest, and a watching page
    // still opens nothing its members sealed.
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
    let hello = DocumentMutation::Snapshot(b"hello from Alice".to_vec());
    alice_page.publish(route.clone(), &hello).unwrap();
    admitted(&alice_page).await;
    wait_for("Alice's note to be committed", || {
        let committed = alice_page.status().outgoing_pending == 0;
        async move { committed.then_some(()) }
    })
    .await;

    // The watching page asks the note's channel for access, and the workspace
    // refuses it by name.
    let paths = Paths::of(https.clone());
    let watcher = watching_page(&spectator, &host.cert, paths.clone()).await;
    watcher.join(workspace_id).unwrap();
    watcher.open_route(route.clone()).unwrap();
    watcher
        .publish(
            route.clone(),
            &DocumentMutation::Incremental(b"a spectator's edit".to_vec()),
        )
        .unwrap();
    let refusal = wait_for("the workspace to refuse the page's channel", || {
        let status = watcher.status();
        let refusal = status
            .failures
            .iter()
            .find(|failure| failure.message == SEALED)
            .map(|failure| failure.code);
        async move { refusal }
    })
    .await;
    assert_eq!(refusal, "permission-required");

    // The page holds what Alice sealed and opens none of it: nothing of it
    // reaches the page's apps, and its own edit reaches no one.
    let opened: Vec<_> = watcher
        .drain()
        .into_iter()
        .filter_map(|event| match event {
            PeerEvent::Document(document)
                if document.route == route || document.decrypted_payload.is_some() =>
            {
                Some(document.route.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        opened.is_empty(),
        "a watching page opened sealed records: {opened:?}"
    );
    let awaiting = watcher
        .status()
        .routes
        .get(&route)
        .map(|route| route.awaiting_keys);
    assert!(
        awaiting.is_some_and(|count| count > 0),
        "the page did not hold the sealed note: {awaiting:?}"
    );
    let again = DocumentMutation::Incremental(b"and again".to_vec());
    alice_page.publish(route.clone(), &again).unwrap();
    admitted(&alice_page).await;
    wait_for("Alice's second write to be committed", || {
        let committed = alice_page.status().outgoing_pending == 0;
        async move { committed.then_some(()) }
    })
    .await;
    assert!(
        watcher.drain().iter().all(
            |event| !matches!(event, PeerEvent::Document(document) if document.route == route)
        ),
        "a watching page read a sealed write"
    );
    assert!(
        alice_page.drain().iter().all(
            |event| !matches!(event, PeerEvent::Document(document) if document.route == route)
        ),
        "the watching page wrote to Alice"
    );
    // No route handed the page a record's keys, and it is listed as here
    // nowhere: the node holds no key to hand a page.
    assert!(
        paths.asked("/channel-keys").is_empty(),
        "the node was asked for a record's keys: {:?}",
        paths.asked("/channel-keys")
    );
    assert!(paths.asked("/here").is_empty());

    watcher.close();
    alice_page.close();
    relay.shutdown().await;
}
