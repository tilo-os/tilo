//! A member's view of the workspace's layout is an ordinary record of the
//! workspace: its policy lets its person write it, and no other member.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::{arrive_under, new_key, person_of};
use ed25519_dalek::SigningKey;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::ServerAddress;
use tilo_server_protocol::{DocumentAnswer, WorkspaceId};
use tilo_session_client::{Cmd, Event, Link, Outbox as _};
use tilo_user_identity::UserIdentity;

use crate::support::{commit_change, enrol, start_workspace_host_relaying, wait_for};

/// A page of a fresh person entering `workspace` through its entry, and the person's user root key.
async fn arrive(
    host: ServerAddress,
    server_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> (Link, [u8; 32]) {
    let root = new_key();
    arrive_as(host, server_cert, workspace, name, &root).await
}

/// [`arrive`] for a device of `root`'s person: another browser of theirs.
async fn arrive_as(
    host: ServerAddress,
    server_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
    root: &SigningKey,
) -> (Link, [u8; 32]) {
    let page = arrive_under(host, root, server_cert, workspace, name).await;
    let person = person_of(&page);
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

/// The app whose document is a workspace's layout, and the person's view of it.
const DESKTOP: &str = tilo_desktop_model::ownership::DESKTOP_APP;

/// A lounge that admits each member who joins through its listing as `role`: its host, the
/// workspace's uuid, and where a page dials it.
async fn lounge(
    role: tilo_server_protocol::workspaces::Role,
) -> (
    crate::support::TestHost,
    crate::support::Device,
    String,
    ServerAddress,
    tempfile::TempDir,
) {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = crate::support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .found_entry(&lounge.workspace, &host_device.cert, |entry| {
            entry.listing = Some(tilo_action_policy::ListingPolicy {
                description: "Everyone".into(),
                default_role: role,
            });
        })
        .await
        .unwrap();
    let address = host.server.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let dialed = ServerAddress::through(iroh::EndpointAddr::new(address.id), relay_url);
    (host, host_device, lounge.workspace, dialed, state)
}

/// The route of `person`'s view of `workspace`'s layout.
fn view_route(workspace: &str, person: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: DESKTOP.into(),
        cluster_id: workspace.into(),
        document: tilo_desktop_model::person_view::resource_of(person),
    }
}

/// A view of the layout with `width` for group 21.
fn view(width: u32) -> tilo_desktop_model::person_view::PersonView {
    tilo_desktop_model::person_view::PersonView {
        widths: [(21, tilo_desktop_model::WindowWidth::Fraction(width))]
            .into_iter()
            .collect(),
        ..Default::default()
    }
}

/// Write `view` as `page`'s own view of `workspace`'s layout, and answer what the workspace said
/// of it.
fn publish_view(
    page: &Link,
    workspace: &str,
    person: &str,
    view: &tilo_desktop_model::person_view::PersonView,
) -> Result<(), String> {
    let mut doc = automerge::AutoCommit::new();
    view.write(&mut doc).map_err(|error| error.to_string())?;
    commit_change(&mut doc);
    page.write(
        view_route(workspace, person),
        &collab_types::DocumentMutation::Snapshot(doc.save()),
    )?;
    let receipt = page.barrier().map_err(|closed| closed.to_string())?;
    receipt.recv().map_err(|error| error.to_string())?
}

/// A layout whose one workspace holds one group, id 21, as a page shows it.
fn layout(workspace: &str) -> tilo_desktop_model::Layout {
    use tilo_desktop_model::{Layout, Strip, StripId, WindowGroup, Workspace, WorkspaceId};
    let id = WorkspaceId::parse(workspace).unwrap();
    let mut held = Workspace::new(id, "Lounge");
    let mut strip = Strip::new(StripId(10), "Main");
    strip.window_groups = vec![WindowGroup {
        id: 21,
        tabs: Vec::new(),
        active_tab: 0,
        width: tilo_desktop_model::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }];
    held.strips = vec![strip];
    Layout {
        active: id,
        workspaces: vec![held],
        ..Layout::default()
    }
}

/// A viewer writes their own view of the layout: the workspace takes it, whatever their role.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_viewer_writes_their_own_view_of_the_layout() {
    let (_host, host_device, workspace, dialed, _state) =
        lounge(tilo_server_protocol::workspaces::Role::Viewer).await;
    let (ada, ada_root) = arrive(dialed, &host_device.cert, &workspace, "Ada").await;
    let id = WorkspaceId::parse(&workspace).unwrap();
    ada.send(Cmd::Join(id)).unwrap();
    let person = hex::encode(ada_root);
    publish_view(&ada, &workspace, &person, &view(700))
        .expect("a viewer writes their own view of the layout");
    assert!(
        matches!(
            answer(&ada, &view_route(&workspace, &person)).await,
            DocumentAnswer::Sent { .. }
        ),
        "the workspace holds the view"
    );
}

/// A member who writes in the workspace writes no one's view of the layout but their own.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn another_member_writes_no_one_elses_view_of_the_layout() {
    let (_host, host_device, workspace, dialed, _state) =
        lounge(tilo_server_protocol::workspaces::Role::Editor).await;
    let (ada, ada_root) = arrive(dialed.clone(), &host_device.cert, &workspace, "Ada").await;
    let (bo, _) = arrive(dialed, &host_device.cert, &workspace, "Bo").await;
    let id = WorkspaceId::parse(&workspace).unwrap();
    ada.send(Cmd::Join(id)).unwrap();
    bo.send(Cmd::Join(id)).unwrap();
    let person = hex::encode(ada_root);
    publish_view(&ada, &workspace, &person, &view(700)).expect("Ada writes her own view");
    let refused = publish_view(&bo, &workspace, &person, &view(480))
        .expect_err("Bo writes no view but his own");
    assert!(
        refused.contains("not granted") || refused.contains("view"),
        "the refusal says whose the view is: {refused}"
    );
}

/// A viewer's widths, set in one of their browsers, show in their other one: the view is the
/// person's view, and every browser of theirs holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_viewers_widths_show_in_their_second_browser() {
    let (_host, host_device, workspace, dialed, _state) =
        lounge(tilo_server_protocol::workspaces::Role::Viewer).await;
    let root = new_key();
    let (laptop, person) = arrive_as(
        dialed.clone(),
        &host_device.cert,
        &workspace,
        "Laptop",
        &root,
    )
    .await;
    let (second, _) = arrive_as(dialed, &host_device.cert, &workspace, "Second", &root).await;
    let id = WorkspaceId::parse(&workspace).unwrap();
    laptop.send(Cmd::Join(id)).unwrap();
    second.send(Cmd::Join(id)).unwrap();
    let person = hex::encode(person);
    let route = view_route(&workspace, &person);
    // The second browser opens the person's view before the first sets a width on it.
    answer(&second, &route).await;

    publish_view(&laptop, &workspace, &person, &view(700))
        .expect("a viewer writes their own view of the layout");

    let kept = wait_for("the width on the person's other browser", || {
        let route = DocumentRoute {
            app_id: DESKTOP.into(),
            cluster_id: workspace.clone(),
            document: tilo_desktop_model::person_view::resource_of(&person),
        };
        let view = crate::support::record_of(&second, &route)
            .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
            .map(|doc| tilo_desktop_model::person_view::PersonView::read(&doc));
        async move { view.filter(|view| !view.widths.is_empty()) }
    })
    .await;
    let mut shown = layout(&workspace);
    assert!(kept.applied(&mut shown, id));
    assert_eq!(
        shown.workspaces[0].strips[0].window_groups[0].width,
        tilo_desktop_model::WindowWidth::Fraction(700),
        "the width the first browser set is the one the second shows"
    );
}
