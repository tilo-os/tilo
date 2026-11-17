//! A page opening its person's own record names it so: the workspace's host
//! knows whose record it is before anything of it is written, and another
//! member opening it is told it is not theirs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::Host;
use tilo_server_protocol::{DocumentAnswer, WorkspaceId};
use tilo_session_client::device::{Entering, entry_request};
use tilo_session_client::{Cmd, Event, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{enrol, enter_page, start_workspace_host_relaying, wait_for};

/// The app whose record each member keeps as their own, as its manifest declares.
const SETTINGS: &str = "so.tilo.settings";

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// A page of a fresh person entering `workspace` through its entry, and the person's user root key.
async fn arrive(
    host: Host,
    host_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> (Link, [u8; 32]) {
    let (root, key) = (new_key(), new_key());
    let (cert, entry) = entry_request(
        host_cert,
        workspace,
        &key,
        Entering {
            held: None,
            root: Some(&root),
            label: "Browser",
            name,
            token: None,
        },
    )
    .await
    .unwrap();
    let person = cert.user_public_key;
    let page = enter_page(
        host,
        cert,
        host_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    );
    (page, person)
}

/// What the workspace answered `page` for `route` once it opened it.
async fn answer(page: &Link, route: &DocumentRoute) -> DocumentAnswer {
    let events = page.events();
    page.send(Cmd::OpenDocumentAt(
        WorkspaceId::parse(&route.cluster_id).unwrap(),
        route.clone(),
    ))
    .unwrap();
    wait_for("the workspace answers the opened record", || {
        let answered = events.drain().into_iter().find_map(|event| match event {
            Event::DocumentOpened {
                route: Some(opened),
                answer,
                ..
            } if opened.document == route.document => Some(answer),
            _ => None,
        });
        async move { answered }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_page_names_its_own_record_and_another_member_is_told_it_is_not_theirs() {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_server_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let address = host.relay.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let page_host = || Host::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let workspace_id = WorkspaceId::parse(&lounge.workspace).unwrap();

    let (ada, ada_root) = arrive(page_host(), &host_device.cert, &lounge.workspace, "Ada").await;
    let (bo, _) = arrive(page_host(), &host_device.cert, &lounge.workspace, "Bo").await;
    ada.send(Cmd::Join(workspace_id)).unwrap();
    bo.send(Cmd::Join(workspace_id)).unwrap();
    let adas = DocumentRoute {
        app_id: SETTINGS.into(),
        cluster_id: lounge.workspace.clone(),
        document: collab_types::OwnResource::new("settings.root", ada_root)
            .unwrap()
            .document_id(),
    };

    // Nothing of Ada's settings is written: her page opening them is sent nothing, and says
    // they are hers.
    assert_eq!(
        answer(&ada, &adas).await,
        DocumentAnswer::Sent { frames: 0 },
        "her own record, of which nothing is written yet"
    );
    // Bo's page cannot name them, being nobody's but Ada's: the host knows whose they are from
    // her opening them, and tells Bo they are not his.
    assert_eq!(
        answer(&bo, &adas).await,
        DocumentAnswer::NotYours,
        "another member's own record, written or not"
    );
}
