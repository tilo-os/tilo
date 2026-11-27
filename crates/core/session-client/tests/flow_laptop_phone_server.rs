//! Flow 3 of `docs/architecture/flows.md`: a laptop, an Android phone and the person's own
//! server, with no tilo.sh at all.
//!
//! The server is a real Tilo host on this machine's loopback and every device reaches it
//! directly; each test is one step of the flow, and a step the system does not do yet fails on
//! the assertion that names what is missing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    dead_code,
    reason = "in a test these are the assertion, not a hazard; a flow's steps set up more than one reads; each device and its own computer and sessions are named alike"
)]

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_peer_state::CopyRole;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_client::https::HttpClient as _;
use tilo_server_protocol::WorkspaceId as ServerWorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{
    CREATE_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, WorkspaceLabels,
};
use tilo_session_client::Link;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::flow;
// The phone's calls block, as they do on the phone's own worker thread.
use tokio::task::block_in_place;

/// The phone's own part, as the phone's tests drive it: its home and the laptop that prints its
/// ticket.
#[path = "../../../../crates/desktop/phone/tests/support/mod.rs"]
mod phone;
use crate::support::{
    DaemonHost, Device, HostSettings, TestHost, commit_change, enrol, entry_grant, record_of,
    redeem, start_host_serving, start_peer, start_server_daemon, start_workspace_host_relaying,
};

/// The laptop, the Android phone and the person's server, with no tilo.sh: every address here
/// is this machine's loopback, and the host looks up no relay.
struct Rig {
    root: SigningKey,
    person: [u8; 32],
    server_device: Device,
    laptop: Device,
    phone: Device,
    host: DaemonHost,
    work: String,
    second: String,
    person_workspace: String,
    /// Each device's session in each workspace: a session enters one workspace through its
    /// entry, which is what hands it that workspace's records.
    laptop_session: Link,
    laptop_person: Link,
    phone_session: Link,
    phone_person: Link,
    /// The person's own computer, holding the workspaces they found themselves: it founds their
    /// own workspace and hands it to the server.
    desk: TestHost,
    _desk_state: tempfile::TempDir,
}

impl Rig {
    fn work_id(&self) -> ServerWorkspaceId {
        ServerWorkspaceId::parse(&self.work).unwrap()
    }

    fn home_id(&self) -> ServerWorkspaceId {
        ServerWorkspaceId::parse(&self.second).unwrap()
    }

    fn person_id(&self) -> ServerWorkspaceId {
        ServerWorkspaceId::parse(&self.person_workspace).unwrap()
    }
}

impl Rig {
    fn client(
        &self,
        session: &Link,
        device: &Device,
        label: &str,
    ) -> tilo_session_client::device::Device {
        flow::client(session, device, &self.server_device.cert, label)
    }
}

async fn rig() -> Rig {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let phone = enrol(&root, "android-phone", DeviceCapabilities::member());
    let person = root.verifying_key().to_bytes();
    // The person's own workspace is founded on their own computer, as founding is signed on the
    // founder's own device and never by a server, and that computer hands it to the server: the
    // server keeps the copy beside it, answers the person's list from it, and founds nothing.
    let desk = enrol(&root, "desk", DeviceCapabilities::primary());
    let desk_state = tempfile::tempdir().unwrap();
    let desk_host =
        start_workspace_host_relaying(&root, &desk, desk_state.path(), false, true).await;
    let desk_workspaces = desk_host.server.workspaces().unwrap();
    let person_workspace =
        tilo_workspace_service::workspaces::person_workspace::found(desk_workspaces)
            .await
            .unwrap();
    assert_eq!(person_workspace, person_workspace_id(&person));
    let host = start_server_daemon(&root, &desk_host, &desk, &person_workspace, "the-server");
    let server_device = host.device();
    desk_workspaces
        .attach_server(
            &person_workspace,
            server_device.cert.device_public_key,
            host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .expect("the server keeps the person's own workspace their computer hands it");
    let laptop_session = flow::session(&host, &laptop, &server_device.cert);
    let phone_session = flow::session(&host, &phone, &server_device.cert);
    for session in [&laptop_session, &phone_session] {
        flow::join(session, &person_workspace, "Ada").await;
    }
    // The person's workspaces are theirs: their laptop makes them on the server through its
    // published action, and every device of theirs takes part in each as a device of the
    // person whose workspace it is.
    let maker = flow::client(&laptop_session, &laptop, &server_device.cert, "laptop");
    let in_person = ServerWorkspaceId::parse(&person_workspace).unwrap();
    let work = flow::create_workspace(&maker, in_person, "Work").await;
    let home = flow::create_workspace(&maker, in_person, "Home").await;
    // Their own computer lists what the person has, as the device that holds their own
    // workspace does: each entry reaches the copy the server keeps.
    for (workspace, name) in [(&work, "Work"), (&home, "Home")] {
        assert!(
            desk_workspaces.list_for(&person, workspace, name).await,
            "the person's own computer lists {name} in their list"
        );
    }
    for session in [&laptop_session, &phone_session] {
        flow::join(session, &work, "Work").await;
        flow::join(session, &home, "Home").await;
    }
    let laptop_person = laptop_session.clone();
    let phone_person = phone_session.clone();
    Rig {
        root,
        person,
        server_device,
        laptop,
        phone,
        host,
        work,
        second: home,
        person_workspace,
        laptop_session,
        laptop_person,
        phone_session,
        phone_person,
        desk: desk_host,
        _desk_state: desk_state,
    }
}

/// The person signs in and controls all their workspaces on the server, and no request
/// leaves this machine's loopback for a tilo.sh or relay name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_signs_in_and_controls_all_workspaces_without_tilosh() {
    let f = Box::pin(rig()).await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");

    // Both workspaces of the person are listed, and the laptop's own device is theirs.
    let listed = laptop
        .person_workspaces(&WorkspaceLabels::new(), None)
        .await
        .expect("the server answers the person's list");
    let ids: Vec<&str> = listed
        .workspaces
        .iter()
        .map(|listed| listed.workspace.as_str())
        .collect();
    assert!(
        ids.contains(&f.work.as_str()) && ids.contains(&f.second.as_str()),
        "the laptop controls every workspace on the server: {ids:?}"
    );

    // It makes a workspace of its own through the host's published action.
    let made = flow::ask(
        &laptop,
        f.work_id(),
        HOST_ACTIONS_PROVIDER,
        HOST_WORKSPACES_RESOURCE,
        CREATE_WORKSPACE,
        json!({"name": "Garden"}),
    )
    .await;
    let made = made
        .unwrap_or_else(|error| panic!("the laptop makes a workspace on the server: {error:?}"));
    let garden = made["workspace"]
        .as_str()
        .expect("the new workspace's id")
        .to_owned();
    let listed = laptop
        .person_workspaces(&WorkspaceLabels::new(), None)
        .await
        .expect("the list grew by the workspace");
    assert!(
        listed
            .workspaces
            .iter()
            .any(|listed| listed.workspace == garden),
        "the workspace the laptop made is one of theirs they control: {listed:?}"
    );
}

/// Nothing of a server's reaches tilo.sh or a public relay: it is its devices' relay, and they
/// relay through the person's relays (number 0's by default) only while it is unreachable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_reaches_tilo_sh_or_a_public_relay() {
    // The person's relays stand in for number 0's: a loopback address nothing answers on.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_RELAY", "http://127.0.0.1:9/");
    let mut f = Box::pin(rig()).await;
    let addr = f.host.node_addr();
    let loopback = test_server::loopback_url();
    assert!(
        addr.relay_urls().all(|relay| relay.to_string() == loopback),
        "the server is reached through this machine's loopback relay alone: {addr:?}"
    );
    assert!(
        f.host.daemon.web().ip().is_loopback(),
        "the server's web UI listens on this machine's loopback: {}",
        f.host.daemon.web()
    );
    let origin = f.host.origin();
    assert!(
        !origin.contains("tilo.sh"),
        "nothing the server serves names tilo.sh: {origin}"
    );
    // Every address the devices dial is this machine's.
    assert!(
        f.host
            .iroh()
            .addr()
            .ip_addrs()
            .all(|addr| addr.ip().is_loopback()),
        "the laptop and the phone dial the server on this machine: {:?}",
        f.host.iroh().addr()
    );

    let fallback = |want: bool, why: &'static str| {
        let session = f.laptop_session.clone();
        crate::support::wait_for(why, move || {
            let status = session.status();
            let settled = status.fallback_relays == want
                && (want || status.phase == tilo_session_client::TransportPhase::Connected);
            async move { settled.then_some(()) }
        })
    };
    fallback(false, "no fallback relay while the server is reachable").await;
    f.host.daemon.stop();
    fallback(true, "the person's relays once the server is unreachable").await;
    tokio::task::block_in_place(|| f.host.daemon.start_again());
    fallback(false, "no fallback relay once the server answers again").await;
}

/// The server serves its own web UI by default, and a device setting turns it off.
///
/// There is no published action or setting that turns the server's web UI off yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_server_serves_its_web_ui_by_default_and_a_setting_turns_it_off() {
    let f = Box::pin(rig()).await;
    let web = crate::support::https::DevHttps::new(f.host.daemon.web(), f.person);

    // On by default: the server answers on its own address over TLS a device of the person's
    // trusts, which is how its pages and the page protocol are reached.
    let served = web.post_answer("/", Vec::new()).await;
    assert!(
        served.is_ok(),
        "the server serves its web UI on its own address by default: {:?}",
        served.err()
    );

    // A setting of the server's, changed by one of its person's devices, turns it off.
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    let turned_off = flow::ask(
        &laptop,
        f.work_id(),
        HOST_ACTIONS_PROVIDER,
        HOST_WORKSPACES_RESOURCE,
        "web.serve",
        json!({"serving": false}),
    )
    .await;
    assert!(
        turned_off.is_ok(),
        "the server has a published setting that stops it serving its web UI, which a \
         device of the person's turns off: {turned_off:?}"
    );
    assert!(
        web.post_answer("/", Vec::new()).await.is_err(),
        "the server's web UI is off once its setting says so"
    );
}

/// The phone joins like any other device: the person's laptop prints a ticket, and the phone's
/// own device (the daemon the Android app runs in its process, driven by the app's code) redeems
/// it, enters the person's workspace on their server, lists it, and reads what the laptop wrote.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_phone_joins_like_any_other_device() {
    // The phone's daemon reads its places from the process's environment: set before it starts.
    let _home = phone::Home::new();
    let f = Box::pin(rig()).await;
    // The server the person's ticket signed in answers their devices as the person: what the
    // workspace's host keeps for them is theirs to read, with no root of theirs on the server.
    let providers = flow::ask(
        &f.client(&f.laptop_session, &f.laptop, "laptop"),
        f.work_id(),
        tilo_workspace_records::models::PROVIDER,
        tilo_workspace_records::models::RESOURCE,
        "list",
        json!({}),
    )
    .await;
    assert!(
        providers.is_ok(),
        "the server answers the laptop's person: {providers:?}"
    );
    let entry = tilo_device_identity::login::Entry {
        workspace: f.work.clone(),
        workspace_name: Some("Work".into()),
        server_cert: f.server_device.cert.clone(),
        server: tilo_api::daemon::enter::reached_of(&f.host.node_addr()),
        site: f.host.origin(),
        host_origin: f.host.origin(),
    };
    let runtime = tokio::runtime::Handle::current();
    // The record of the person's devices, which the computer that founded their own workspace
    // holds and the server keeps a copy of.
    let workspaces = f.desk.server.workspaces().unwrap().clone();
    let laptop = block_in_place(|| phone::Issuing::start(&runtime, &f.root, entry, workspaces))
        .expect("the laptop's ticket and endpoint are made");
    let mut device = block_in_place(|| tilo_runtime::in_process::Device::start(&phone::profile()))
        .expect("the phone's daemon starts");
    let before = block_in_place(|| phone::shown(&device));
    assert!(
        !before.signed_in,
        "a phone nobody signed in to shows where a ticket goes: {before:?}"
    );
    block_in_place(|| device.log_in(&laptop.ticket(), "Pixel"))
        .expect("the phone redeems the ticket the person's laptop printed");
    let devices = block_in_place(|| {
        phone::until("the phone lists the person's workspace", || {
            let devices = device.devices().map_err(|error| format!("{error:#}"))?;
            if devices
                .workspaces
                .iter()
                .any(|workspace| workspace.key == f.work)
            {
                Ok(devices)
            } else {
                Err(format!("{devices:?}"))
            }
        })
    });
    assert!(
        devices.signed_in,
        "the ticket signed the phone in: {devices:?}"
    );
    assert!(
        devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == f.work && workspace.name == "Work"),
        "the phone names the workspace as the person does: {devices:?}"
    );

    // A file the laptop writes into the workspace's folder reaches the phone: a real entry of
    // the workspace's file tree, written as a computer's folder writes it, listed by the phone's
    // daemon as `tilo ls files` lists the workspace's folder.
    let put = flow::put_file(&f.laptop_session, &f.work, "notes.txt", b"from the laptop").await;
    assert!(
        put.is_ok(),
        "the laptop's file is one of the person's workspace's folder: {put:?}"
    );
    let calls = tilo_api::calls::Calls::open(tilo_api::calls::Reach::default())
        .await
        .expect("the phone's daemon is this process's computer");
    let the_file = |row: &serde_json::Value| row["id"] == "file:notes.txt";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let listed = loop {
        let answer = calls
            .list(Some(&f.work), Some("files"), None)
            .await
            .map_err(|error| format!("{error:#}"));
        let has_it = answer.as_ref().is_ok_and(|listed| {
            listed["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(the_file))
        });
        match answer {
            Ok(listed) if has_it => break listed,
            last if std::time::Instant::now() > deadline => {
                panic!("the phone lists the laptop's file in the workspace's folder: {last:?}")
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(200)).await,
        }
    };
    assert_eq!(
        listed["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| the_file(row))
            .count(),
        1,
        "the phone lists the one file the laptop put in the workspace's folder: {listed}"
    );
    block_in_place(|| device.stop()).expect("the phone's daemon stops");
    drop(laptop);
}

/// The laptop's person picks the 2D canvas as their shell, and it is their layout: the phone
/// shows the same canvas for the workspace.
///
/// The style is the person's: `show_style` on their shell `Viewing` record chooses the frames
/// style, and the layout is the session's `shell:layout`, whose `place` at x/y makes a
/// workspace a canvas. Neither is reachable from a workspace session in this fixture: the
/// layout's actions are the session's own, and the profile they write is on the person's
/// cluster, not the workspace host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_laptop_picks_the_canvas_shell_and_it_is_their_layout() {
    let f = Box::pin(rig()).await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    let placed = flow::ask(
        &laptop,
        f.work_id(),
        "so.tilo.session",
        "shell:layout",
        "place",
        json!({
            "workspace": f.work,
            "x": 0,
            "y": 0,
            "width": 640,
            "height": 480,
        }),
    )
    .await;
    assert!(
        placed.is_ok(),
        "the session's `shell:layout#place` with x/y makes the workspace's layout a canvas, \
         and `show_style` on the person's shell `Viewing` record chooses the frames style: this \
         fixture reaches neither, as the layout's actions are the session's own and the profile \
         is on the person's cluster, not the workspace host: {placed:?}"
    );
}

/// A chat turn writes a view: an agent asked for "Child mortality in Africa by country since
/// 2000" answers with a view of that data, backed by a dataset resource.
///
/// No action starts such a turn and no data path backs a view yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_chat_turn_writes_a_view_backed_by_a_dataset() {
    let f = Box::pin(rig()).await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    let answered = flow::ask(
        &laptop,
        f.work_id(),
        "so.tilo.chat",
        "chat",
        "start-turn",
        json!({"prompt": "Child mortality in Africa by country since 2000"}),
    )
    .await;
    let answered = answered.unwrap_or_else(|error| {
        panic!("a chat turn over the workspace's model writes a view: {error:?}")
    });
    assert!(
        answered.get("view").is_some(),
        "the turn answers with a view of the data it asked for: {answered}"
    );
    let resource = answered["view"]["dataset"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        !resource.is_empty(),
        "the view is backed by a dataset resource of the workspace: {answered}"
    );
    let route = DocumentRoute {
        app_id: resource
            .split_once('/')
            .map_or("so.tilo.datasets", |(app, _)| app)
            .to_owned(),
        cluster_id: f.work.clone(),
        document: resource
            .split_once('/')
            .map_or_else(|| resource.clone(), |(_, document)| document.to_owned()),
    };
    flow::open(&f.phone_session, &route);
    assert!(
        record_of(&f.phone_session, &route).is_some(),
        "the dataset the view reads reaches every device: {resource}"
    );
}

/// An alarm the person sets on the laptop through their agent rings on the phone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_alarm_set_on_laptop_through_the_agent_rings_on_the_phone() {
    let f = Box::pin(rig()).await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    let rung = flow::ask(
        &laptop,
        f.work_id(),
        "so.tilo.clocks",
        "clocks",
        "add-alarm",
        json!({"label": "Standup", "hour": 7, "minute": 0}),
    )
    .await;
    let set = rung.unwrap_or_else(|error| {
        panic!(
            "an alarm set on the laptop is a resource every device of the person's rings: {error:?}"
        )
    });
    assert!(
        set.get("resource").is_some(),
        "the alarm is one resource of the person's workspace, which the phone watches: {set}"
    );
}

/// Someone who has never used Tilo joins on the web through an invitation: they open one app
/// as a page, and the workspace's canvas as HTML.
///
/// The browser's first page is the static web entry's, on a host of its own; it hands a
/// workspace's page addresses (`/w/<workspace>/<app>`) to the server, as this test asks the
/// server for them. What the page then renders over its link is the holding device's
/// (`tilo-runtime`'s `key_sessions::pages`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_newcomer_joins_on_the_web_and_opens_an_app_as_a_page() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "the-server", DeviceCapabilities::member());
    let guest = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let guest_key = guest.verifying_key().to_bytes();
    let state = tempfile::tempdir().unwrap();
    let guests = [guest_key];
    let host = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            plain_listen: Some("127.0.0.1:0".parse().unwrap()),
            guests: &guests,
            ..HostSettings::default()
        },
    )
    .await;
    let workspace = host
        .server
        .workspaces()
        .unwrap()
        .create("Garden")
        .await
        .unwrap();

    // The newcomer's browser takes the invitation and opens one app of the workspace.
    let tab = enrol(&guest, "firefox", DeviceCapabilities::member());
    let invitation = entry_grant(
        &host,
        &root,
        &server_device,
        &workspace.workspace,
        guest_key,
    )
    .await;
    redeem(&host, invitation, &tab.cert, &tab.key).await;
    let page = start_peer(
        host.iroh(),
        tab.cert.clone(),
        server_device.cert.clone(),
        tab.key.clone(),
    );
    // The newcomer entered through a link, so it joins through its entry, not as one of the
    // workspace's own person's devices.
    flow::join_entered(&page, &workspace.workspace).await;
    // One app of the workspace, opened as a page: the person's view of its layout, which a
    // page writes and reads as `layout_views`' pages do.
    let person = hex::encode(guest_key);
    let view = DocumentRoute {
        app_id: flow::DESKTOP_APP.into(),
        cluster_id: workspace.workspace.clone(),
        document: tilo_desktop_model::person_view::resource_of(&person),
    };
    let mut doc = AutoCommit::new();
    tilo_desktop_model::person_view::PersonView::default()
        .write(&mut doc)
        .unwrap();
    commit_change(&mut doc);
    let opened =
        flow::try_written_entered(&page, &view, &DocumentMutation::Snapshot(doc.save())).await;
    assert!(
        opened.is_ok() && record_of(&page, &view).is_some(),
        "the newcomer opens an app of the workspace as a page: {opened:?}"
    );

    // And the workspace's canvas, as HTML a browser renders: the page the web entry forwards
    // to the server, whose script dials the server's page link and proves the newcomer's
    // device there.
    let origin = host.web.plain_origin.clone().unwrap();
    let canvas = tilo_server_protocol::page::PageAddress {
        workspace: workspace.workspace.clone(),
        app: tilo_server_protocol::page::DESKTOP.into(),
        show: None,
    };
    let (status, body) =
        flow::get(flow::address_of(&origin), &canvas.path()).unwrap_or((0, String::new()));
    assert!(
        status == 200
            && body.contains(&workspace.workspace)
            && body.contains(&host.web.link.origin)
            && body.contains(tilo_server_protocol::web::LINK_PAGE_PATH),
        "the workspace's 2D canvas is served as HTML to a browser that joined on the web, \
         dialling the server's page link (status {status}): {body}"
    );
}

/// Their brother uses the Android app only to sync his files through Tilo: his device reaches
/// the files, and nothing else of the workspace.
///
/// A device whose grant is limited to files is not offered yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_brothers_device_syncs_only_files() {
    let f = Box::pin(rig()).await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    // Their brother is a person of their own, with his own identity and phone.
    let (brother, _) = UserIdentity::generate("Bram", 1);
    // The workspace is shared with him through the one grant rule over addresses: a member
    // grant over `<workspace>/files/**`, so his phone holds the file tree there and opens no
    // other app.
    let shared = flow::ask_permissions(
        &laptop,
        f.work_id(),
        &f.work,
        "grant.set",
        json!({
            "person": hex::encode(brother.verifying_key().to_bytes()),
            "address": format!("{}/files/**", f.work),
            "role": "editor",
        }),
    )
    .await;
    assert!(
        shared.is_ok(),
        "the one grant rule over addresses grants the brother's own identity \
         `{workspace}/files/**` (workspace/<id>/grants#set), so his phone holds the \
         workspace's file tree and opens no other app (the phones lane lands it): {shared:?}",
        workspace = f.work
    );
}

/// Every device manages every other: from the phone, the laptop and the server are listed,
/// renamed, updated and configured.
///
/// A device of the person's manages another's name; its update and settings are the server's
/// alone (the owner of the identity lane takes the rest).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_every_device_manages_every_other() {
    let f = Box::pin(rig()).await;
    let phone = f.client(&f.phone_session, &f.phone, "android-phone");
    let laptop_key = flow::key_of(&f.laptop.cert);

    // The person's devices are a record of their own workspace, written by the computer that
    // certifies them, which founded it, and kept by the server with them.
    let workspaces = f.desk.server.workspaces().unwrap();
    for device in [&f.laptop, &f.phone, &f.server_device] {
        workspaces.keep_issued_device(&device.cert, "").await;
    }
    // The phone renames the laptop and the server there, as any record's action.
    for (key, name) in [
        (laptop_key.as_str(), "Ada's laptop"),
        (
            flow::key_of(&f.server_device.cert).as_str(),
            "Hallway server",
        ),
    ] {
        flow::ask(
            &phone,
            f.person_id(),
            "so.tilo.devices",
            "devices",
            "name-device",
            json!({"device": key, "name": name}),
        )
        .await
        .unwrap_or_else(|error| panic!("the phone names the device {key}: {error:?}"));
    }
    let record = tilo_workspace_service::workspaces::person_workspace::read::<
        tilo_workspace_service::workspaces::person_workspace::devices::Devices,
    >(
        workspaces,
        tilo_workspace_service::workspaces::person_workspace::devices::DEVICES,
    )
    .await
    .expect("the record of the person's devices reads");
    let named = record
        .devices
        .iter()
        .find(|(key, _)| key.as_str() == laptop_key)
        .and_then(|(_, device)| device.name.clone());
    assert_eq!(
        named.as_deref(),
        Some("Ada's laptop"),
        "the phone's name for the laptop is what every list shows"
    );

    // From the phone, the laptop is updated and its settings changed.
    let managed = flow::ask(
        &phone,
        f.work_id(),
        "so.tilo.devices",
        "devices",
        "update-device",
        json!({"device": laptop_key, "update": "now"}),
    )
    .await;
    let managed = managed.unwrap_or_else(|error| {
        panic!("the phone updates the laptop as it would the server: {error:?}")
    });
    assert!(
        managed.get("updating").is_some(),
        "the phone's update of the laptop is carried out by the laptop: {managed}"
    );
}

/// A newcomer's browser joins through the static web entry page while the server has no
/// domain name: the browser reaches the server peer-to-peer over iroh through a relay that
/// carries only sealed traffic, and the certificate, with no name set, comes from a local CA
/// under the person's root.
///
/// The entry page is the static web entry's, on a host of its own: it hands its page the line
/// the server keeps in its state (`web-host.json`, `TILO_WEB_HOME_HOST_INFO`), and the page
/// dials the server as that names it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_newcomer_browser_joins_through_the_static_entry_with_no_domain_name() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "the-server", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_serving(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        HostSettings {
            plain_listen: Some("127.0.0.1:0".parse().unwrap()),
            relayed: true,
            ..HostSettings::default()
        },
    )
    .await;
    assert!(
        host.server.node_addr().relay_urls().next().is_some(),
        "the server is reached through a relay, so a browser behind any network finds it"
    );
    // Found in the server's state as its deployment finds it there, wherever below it is kept.
    let body = kept_below(state.path(), "web-host.json").unwrap_or_else(|| {
        panic!("the server keeps what a static entry page hands a browser before it holds anything")
    });

    // The page hands the browser the server's own address, to reach it peer-to-peer: no site
    // name, and the relay it dials.
    let relay = host
        .server
        .node_addr()
        .relay_urls()
        .next()
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(
        !relay.is_empty() && body.contains(&relay),
        "the entry page hands the browser the server's relay address ({relay}), through which \
         the browser reaches the server itself: {body}"
    );
    assert!(
        !body.contains("tilo.sh"),
        "the entry page names no tilo.sh site: the browser reaches the server itself"
    );

    // With no domain name, the certificate comes from a local CA under the person's root: Ada's
    // devices trust it, and another person's do not.
    let adas = crate::support::https::DevHttps::new(host.web.addr, root.verifying_key().to_bytes());
    let answer = adas.post_answer("/", Vec::new()).await;
    assert!(
        answer.is_ok(),
        "Ada's device trusts the certificate her server's local CA issued: {:?}",
        answer.err()
    );
    let (bob, _) = UserIdentity::generate("Bob", 1);
    let bobs = crate::support::https::DevHttps::new(host.web.addr, bob.verifying_key().to_bytes());
    let refused = bobs
        .post_answer("/", Vec::new())
        .await
        .expect_err("another person's device does not trust Ada's local CA");
    assert!(
        refused.contains("UnknownIssuer"),
        "the certificate is refused for its issuer: {refused}"
    );
}

/// The text of the file `name` somewhere below `dir`, as a deployment finds what its server
/// keeps in its state.
fn kept_below(dir: &std::path::Path, name: &str) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries.filter_map(Result::ok).find_map(|entry| {
        let path = entry.path();
        if path.is_dir() {
            kept_below(&path, name)
        } else if entry.file_name() == name {
            std::fs::read_to_string(path).ok()
        } else {
            None
        }
    })
}
