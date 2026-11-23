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
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    dead_code,
    reason = "in a test these are the assertion, not a hazard; a flow's steps set up more than one reads"
)]

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId as ServerWorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{
    CREATE_WORKSPACE, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, PERSONAL_DEVICE_NAME,
    PersonalDevice, PersonalList, PersonalWorkspace, WorkspaceLabels,
};
use tilo_session_client::Link;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::flow;
use crate::support::{
    Device, HostSettings, TestHost, enrol, entry_grant, record_of, redeem, start_host_serving,
    start_peer,
};

/// The laptop, the Android phone and the person's server, with no tilo.sh: every address here
/// is this machine's loopback, and the host looks up no relay.
struct Rig {
    root: SigningKey,
    person: [u8; 32],
    server_device: Device,
    laptop: Device,
    phone: Device,
    host: TestHost,
    work: String,
    second: String,
    person_workspace: String,
    /// Each device's session in each workspace: a session enters one workspace through its
    /// entry, which is what hands it that workspace's records.
    laptop_session: Link,
    laptop_person: Link,
    phone_session: Link,
    phone_person: Link,
    _state: tempfile::TempDir,
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
    let server_device = enrol(&root, "the-server", DeviceCapabilities::member());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let phone = enrol(&root, "android-phone", DeviceCapabilities::member());
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
    let workspaces = host.server.workspaces().unwrap();
    let person = root.verifying_key().to_bytes();
    // The person's workspaces are theirs: the server keeps them for them, and every device of
    // theirs takes part in each as a device of the person whose workspace it is.
    let work = workspaces.create("Work").await.unwrap().workspace;
    let home = workspaces.create("Home").await.unwrap().workspace;
    // The person's own workspace, founded on their server as a server founds it.
    let person_workspace = tilo_server::servers::found(workspaces).await.unwrap();
    assert_eq!(person_workspace, person_workspace_id(&person));
    let laptop_session = flow::session(&host, &laptop, &server_device.cert);
    let phone_session = flow::session(&host, &phone, &server_device.cert);
    for session in [&laptop_session, &phone_session] {
        flow::join(session, &work, "Work").await;
        flow::join(session, &home, "Home").await;
        flow::join(session, &person_workspace, "Ada").await;
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
        _state: state,
    }
}

/// The list the laptop keeps for the person.
fn personal(f: &Rig) -> PersonalList {
    let workspace = |id: &str, name: &str| PersonalWorkspace {
        workspace: id.to_owned(),
        name: name.to_owned(),
        host: None,
        role: Some("owner".into()),
        labels: WorkspaceLabels::new(),
        made_at_ms: None,
        unattended: false,
        enter_at: None,
    };
    PersonalList {
        workspaces: vec![workspace(&f.work, "Work"), workspace(&f.second, "Home")],
        devices: vec![
            PersonalDevice {
                cert: f.laptop.cert.clone(),
                name: Some("Laptop".into()),
                revoked: false,
                browser: None,
                agent: None,
                last_active_ms: None,
                online: true,
            },
            PersonalDevice {
                cert: f.phone.cert.clone(),
                name: Some("Phone".into()),
                revoked: false,
                browser: None,
                agent: None,
                last_active_ms: None,
                online: true,
            },
            PersonalDevice {
                cert: f.server_device.cert.clone(),
                name: Some("The server".into()),
                revoked: false,
                browser: None,
                agent: None,
                last_active_ms: None,
                online: true,
            },
        ],
    }
}

/// The person signs in and controls all their workspaces on the server, and no request
/// leaves this machine's loopback for a tilo.sh or relay name.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_signs_in_and_controls_all_workspaces_without_tilosh() {
    let f = rig().await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    laptop
        .keep_personal(&personal(&f))
        .await
        .expect("the laptop keeps the person's list");

    // Both workspaces of the person are listed, and the laptop's own device is theirs.
    let listed = laptop
        .personal_list_in(f.work_id(), &WorkspaceLabels::new(), None)
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
        .personal_list_in(f.work_id(), &WorkspaceLabels::new(), None)
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

/// Nothing of a server's reaches tilo.sh or a public relay: its endpoint relays nowhere
/// public and resolves peers on no number 0 DNS, and the devices reach it on this machine.
///
/// The runtime's default endpoint still binds number 0's relays and DNS (the hosting lane
/// takes this step).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_reaches_tilo_sh_or_a_public_relay() {
    let f = rig().await;
    // The endpoint every device of the person's binds with, as the runtime binds it by
    // default.
    let relays = tilo_endpoint_reachability::relays();
    let named: Vec<String> = relays
        .relays
        .urls::<Vec<_>>()
        .iter()
        .map(ToString::to_string)
        .collect();
    let host = named
        .first()
        .map(|url| {
            url.split("://")
                .nth(1)
                .unwrap_or(url)
                .split(['/', '?'])
                .next()
                .unwrap_or(url)
                .to_owned()
        })
        .unwrap_or_default();
    assert!(
        !relays.n0_dns && named.is_empty(),
        "the server's endpoint reaches {host} (and number 0's DNS: {}) where nothing of \
         the person's should reach tilo.sh or a public relay: {named:?}",
        relays.n0_dns
    );

    let addr = f.host.server.node_addr();
    assert!(
        addr.relay_urls().next().is_none(),
        "the server reaches its devices directly, through no relay at all: {addr:?}"
    );
    assert!(
        f.host.web.addr.ip().is_loopback(),
        "the server's web UI listens on this machine's loopback: {}",
        f.host.web.addr
    );
    let origin = f
        .host
        .web
        .plain_origin
        .clone()
        .expect("the server's plain listener");
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
}

/// The server serves its own web UI by default, and a device setting turns it off.
///
/// There is no published action or setting that turns the server's web UI off yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_server_serves_its_web_ui_by_default_and_a_setting_turns_it_off() {
    let f = rig().await;
    let origin = f.host.web.plain_origin.clone().unwrap();
    let addr = flow::address_of(&origin);

    // On by default: the server answers HTTP on its own address, which is how its pages and
    // the page protocol are reached.
    let (status, body) = flow::post(addr, "/", &[]).expect("the server answers on its address");
    assert!(
        status > 0,
        "the server serves its web UI on its own address by default: {status} {body}"
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
    let (status, _) = flow::post(addr, "/", &[]).unwrap_or((0, String::new()));
    assert_eq!(
        status, 0,
        "the server's web UI is off once its setting says so"
    );
}

/// The phone joins like any other device, through the invitation a link brings it, and reads
/// what the laptop wrote.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_phone_joins_like_any_other_device() {
    let f = rig().await;
    // A device of the person's that has never been here joins their workspace like any other
    // of their devices; the Android app's own part is the phones lane's.
    let phone = enrol(&f.root, "pixel", DeviceCapabilities::member());
    let session = flow::session(&f.host, &phone, &f.server_device.cert);
    flow::join(&session, &f.work, "Work").await;

    // A file the laptop writes into the workspace's folder reaches the phone: a real entry of
    // the workspace's file tree, written as a computer's folder writes it.
    let put = flow::put_file(&f.laptop_session, &f.work, "notes.txt", b"from the laptop").await;
    assert!(
        put.is_ok(),
        "the laptop's file is one of the person's workspace's folder, read by the phone: {put:?}"
    );
    flow::folder_index(&session, &f.work, "the folder's index at the phone").await;
    let files = crate::support::wait_for("the folder's file at the phone", || async {
        let count = flow::file_count(&session, &f.work);
        (count > 0).then_some(count)
    })
    .await;
    assert_eq!(
        files, 1,
        "the phone reads the one file the laptop put in the workspace's folder"
    );
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
    let f = rig().await;
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
    let f = rig().await;
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
///
/// The Clocks port has no published action yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_alarm_set_on_laptop_through_the_agent_rings_on_the_phone() {
    let f = rig().await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "laptop");
    let rung = flow::ask(
        &laptop,
        f.work_id(),
        "so.tilo.clocks",
        "clocks",
        "set-alarm",
        json!({"at": "07:00", "label": "Standup", "ring": "every-device"}),
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
/// The page is a browser's; the canvas as HTML is not served yet.
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
    doc.commit();
    let opened =
        flow::try_written_entered(&page, &view, &DocumentMutation::Snapshot(doc.save())).await;
    assert!(
        opened.is_ok() && record_of(&page, &view).is_some(),
        "the newcomer opens an app of the workspace as a page: {opened:?}"
    );

    // And the workspace's canvas, as HTML a browser renders.
    let origin = host.web.plain_origin.clone().unwrap();
    let (status, body) =
        flow::get(flow::address_of(&origin), "/canvas").unwrap_or((0, String::new()));
    assert!(
        status == 200 && body.contains("garden"),
        "the workspace's 2D canvas is served as HTML to a browser that joined on the web \
         (status {status}): {body}"
    );
}

/// Their brother uses the Android app only to sync his files through Tilo: his device reaches
/// the files, and nothing else of the workspace.
///
/// A device whose grant is limited to files is not offered yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_brothers_device_syncs_only_files() {
    let f = rig().await;
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
    let f = rig().await;
    let phone = f.client(&f.phone_session, &f.phone, "android-phone");
    let laptop_key = flow::key_of(&f.laptop.cert);

    // The phone lists the laptop and the server and renames them.
    phone
        .keep_personal(&personal(&f))
        .await
        .expect("the phone keeps the person's list");
    for (key, name) in [
        (laptop_key.as_str(), "Ada's laptop"),
        (
            flow::key_of(&f.server_device.cert).as_str(),
            "Hallway server",
        ),
    ] {
        flow::ask(
            &phone,
            f.work_id(),
            HOST_ACTIONS_PROVIDER,
            HOST_WORKSPACES_RESOURCE,
            PERSONAL_DEVICE_NAME,
            json!({"device": key, "name": name}),
        )
        .await
        .unwrap_or_else(|error| panic!("the phone names the device {key}: {error:?}"));
    }
    let listed = phone
        .personal_list_in(f.work_id(), &WorkspaceLabels::new(), None)
        .await
        .expect("the list answers");
    let named = listed
        .devices
        .iter()
        .find(|listed| flow::key_of(&listed.cert) == laptop_key)
        .and_then(|listed| listed.name.clone());
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
/// The listener still serves a development self-signature (the hosting lane takes this step).
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
    let origin = host.web.plain_origin.clone().unwrap();
    let (status, body) = flow::get(flow::address_of(&origin), "/").unwrap_or((0, String::new()));
    assert_eq!(
        status, 200,
        "the server serves a static entry page a browser can open before it holds anything"
    );

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

    // With no domain name, the certificate comes from a local CA under the person's root,
    // not from a development self-signature.
    assert!(
        host.web.certificate_sha256.is_none(),
        "the listener's certificate is issued by a local CA under the person's root, not a \
         development self-signature: {:?}",
        host.web.certificate_sha256
    );
}
