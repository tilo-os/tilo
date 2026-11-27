//! Flow 2 of `docs/architecture/flows.md`: one person, four devices, used as one computer.
//!
//! The person's four devices are a Fedora desktop, an Orange Pi, a Snapdragon laptop and
//! tilo.sh, the always-on server. Each test is one step of the flow and drives the devices
//! through the published actions and views alone; a step the system does not do yet fails on
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

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_server_protocol::WorkspaceId as ServerWorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::WorkspaceLabels;
use tilo_session_client::Link;
use tilo_user_identity::{AgentScope, DeviceCapabilities, UserIdentity};
use tilo_workspace_records::models as model_providers;
use tilo_workspace_records::vault as workspace_vault;
use tilo_workspace_service::workspaces::OwnWorkspaceOffer;

use crate::support::flow;
use crate::support::{
    DaemonHost, Device, TestHost, enrol, start_server_daemon, start_workspace_host_relaying,
};

/// The person's four devices, entered into every workspace they have, with a session each
/// for the three that run a Tilo shell; the fourth is the server itself.
struct Four {
    root: SigningKey,
    person: [u8; 32],
    server_device: Device,
    desktop: Device,
    pi: Device,
    laptop: Device,
    host: DaemonHost,
    work: String,
    home: String,
    person_workspace: String,
    /// Each device's session in each workspace: a session enters one workspace through its
    /// entry, which is what hands it that workspace's records.
    desktop_session: Link,
    desktop_home: Link,
    desktop_person: Link,
    pi_session: Link,
    pi_person: Link,
    laptop_session: Link,
    laptop_person: Link,
    /// The person's own computer, which founded their own workspace and handed it to the server.
    desk: Device,
    desk_host: TestHost,
    _desk_state: tempfile::TempDir,
}

impl Four {
    fn work_id(&self) -> ServerWorkspaceId {
        ServerWorkspaceId::parse(&self.work).unwrap()
    }

    fn home_id(&self) -> ServerWorkspaceId {
        ServerWorkspaceId::parse(&self.home).unwrap()
    }

    fn person_id(&self) -> ServerWorkspaceId {
        ServerWorkspaceId::parse(&self.person_workspace).unwrap()
    }

    fn client(
        &self,
        session: &Link,
        device: &Device,
        label: &str,
    ) -> tilo_session_client::device::Device {
        flow::client(session, device, &self.server_device.cert, label)
    }
}

async fn four_devices() -> Four {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let desktop = enrol(&root, "fedora-desktop", DeviceCapabilities::primary());
    let pi = enrol(&root, "orange-pi", DeviceCapabilities::member());
    let laptop = enrol(&root, "snapdragon-laptop", DeviceCapabilities::member());
    let person = root.verifying_key().to_bytes();
    // The person's own workspace is founded on a computer of theirs, as founding is signed on
    // the founder's own device and never by a server, and that computer hands it to the server
    // and lists the workspaces they have: a server never writes the person's list.
    let desk = enrol(&root, "desk", DeviceCapabilities::primary());
    let desk_state = tempfile::tempdir().unwrap();
    let desk_host =
        start_workspace_host_relaying(&root, &desk, desk_state.path(), false, true).await;
    let desk_workspaces = desk_host.server.workspaces().unwrap();
    // As the person's computer keeps the workspaces they make on it as their own.
    desk_workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("desk"))
        .unwrap();
    let person_workspace =
        tilo_workspace_service::workspaces::person_workspace::found(desk_workspaces)
            .await
            .unwrap();
    assert_eq!(person_workspace, person_workspace_id(&person));
    let host = start_server_daemon(&root, &desk_host, &desk, &person_workspace, "tilo.sh");
    let server_device = host.device();
    desk_workspaces
        .attach_server(
            &person_workspace,
            server_device.cert.device_public_key,
            host.node_addr(),
            tilo_peer_state::CopyRole::KeyHolder,
        )
        .await
        .expect("the server keeps the person's own workspace their computer hands it");
    let desktop_session = flow::session(&host, &desktop, &server_device.cert);
    let pi_session = flow::session(&host, &pi, &server_device.cert);
    let laptop_session = flow::session(&host, &laptop, &server_device.cert);
    for session in [&desktop_session, &pi_session, &laptop_session] {
        flow::join(session, &person_workspace, "Ada").await;
    }
    // The person's workspaces are theirs: their desktop makes them on the server through its
    // published action, the server keeps them for them, and every device of theirs takes part
    // in each as a device of the person whose workspace it is.
    let maker = flow::client(
        &desktop_session,
        &desktop,
        &server_device.cert,
        "fedora-desktop",
    );
    let in_person = ServerWorkspaceId::parse(&person_workspace).unwrap();
    let work = flow::create_workspace(&maker, in_person, "Work").await;
    let home = flow::create_workspace(&maker, in_person, "Home").await;
    for (workspace, name) in [(&work, "Work"), (&home, "Home")] {
        assert!(
            desk_workspaces.list_for(&person, workspace, name).await,
            "the person's own computer lists {name} in their list"
        );
    }
    for session in [&desktop_session, &pi_session, &laptop_session] {
        flow::join(session, &work, "Work").await;
        flow::join(session, &home, "Home").await;
    }
    let (desktop_home, desktop_person) = (desktop_session.clone(), desktop_session.clone());
    let pi_person = pi_session.clone();
    let laptop_person = laptop_session.clone();
    Four {
        root,
        person,
        server_device,
        desktop,
        pi,
        laptop,
        host,
        work,
        home,
        person_workspace,
        desktop_session,
        desktop_home,
        desktop_person,
        pi_session,
        pi_person,
        laptop_session,
        laptop_person,
        desk,
        desk_host,
        _desk_state: desk_state,
    }
}

/// The keys of the person's devices, as the record of their own workspace holds them on the
/// computer that founded it, which every device of theirs holds too.
async fn recorded(f: &Four) -> Vec<String> {
    use tilo_workspace_service::workspaces::person_workspace::{self, devices};
    let workspaces = f.desk_host.server.workspaces().unwrap();
    person_workspace::read::<devices::Devices>(workspaces, devices::DEVICES)
        .await
        .expect("the record of the person's devices reads")
        .listed(&f.person)
        .into_iter()
        .map(|listed| listed.key)
        .collect()
}

/// All four devices use the person's identity and have all their workspaces, apps and
/// files: each lists both workspaces and all four devices, and reads the file the desktop
/// wrote and the shared desktop under which the apps' windows live.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_all_four_devices_see_every_workspace_app_and_file() {
    let f = Box::pin(four_devices()).await;

    // The server holds both of the person's workspaces at once.
    let desktop = f.client(&f.desktop_session, &f.desktop, "fedora-desktop");
    assert!(
        flow::server_serves(&desktop, f.work_id(), &f.work).await
            && flow::server_serves(&desktop, f.work_id(), &f.home).await,
        "the always-on server serves every workspace of the person"
    );

    // Each device their root certified is in the record of their devices.
    let devices = recorded(&f).await;
    for certified in [&f.desktop, &f.pi, &f.laptop, &f.server_device] {
        assert!(
            devices.contains(&flow::key_of(&certified.cert)),
            "the person's record holds every device of theirs: {devices:?}"
        );
    }

    // Every other device reads the same list: both workspaces.
    for (label, session, device) in [
        ("the Orange Pi", &f.pi_session, &f.pi),
        ("the laptop", &f.laptop_session, &f.laptop),
    ] {
        let client = f.client(session, device, &label.replace(' ', "-"));
        let listed = client
            .person_workspaces(&WorkspaceLabels::new(), None)
            .await
            .unwrap_or_else(|error| panic!("{label} reads the person's list: {error:?}"));
        let ids: Vec<&str> = listed
            .workspaces
            .iter()
            .map(|listed| listed.workspace.as_str())
            .collect();
        assert!(
            ids.contains(&f.work.as_str()) && ids.contains(&f.home.as_str()),
            "{label} sees every workspace of the person: {ids:?}"
        );
    }

    // A file the desktop writes into the workspace's folder is the file every other device
    // reads: a real entry of the workspace's file tree, written as a computer's folder writes
    // it, never an app's document.
    let put = flow::put_file(
        &f.desktop_session,
        &f.work,
        "welcome.txt",
        b"hello from the desktop",
    )
    .await;
    assert!(
        put.is_ok(),
        "the file the desktop writes is one of the workspace's folder, which every device of \
         theirs reads: {put:?}"
    );
    flow::folder_index(
        &f.desktop_session,
        &f.work,
        "the folder's index at the desktop",
    )
    .await;
    for (label, session) in [
        ("the Orange Pi", &f.pi_session),
        ("the laptop", &f.laptop_session),
    ] {
        flow::folder_index(session, &f.work, "the folder's index at the device").await;
        let files = crate::support::wait_for("the folder's file at the device", || async {
            let count = flow::file_count(session, &f.work);
            (count > 0).then_some(count)
        })
        .await;
        assert_eq!(
            files, 1,
            "{label} reads the one file the desktop put in the workspace's folder"
        );
    }

    // The workspace's installed apps are one list every device of the person reads.
    for (label, session, device) in [
        ("the Orange Pi", &f.pi_session, &f.pi),
        ("the laptop", &f.laptop_session, &f.laptop),
    ] {
        let list = flow::ask_permissions_child(
            &f.client(session, device, label),
            f.work_id(),
            &f.work,
            "apps",
            "list",
            json!({}),
        )
        .await;
        assert!(
            list.is_ok(),
            "{label} reads the workspace's installed app list at workspace/{}/apps#list: \
             {list:?}",
            f.work
        );
    }
}

/// The always-on server is one of the person's devices in the one device list: their other
/// devices list it, and manage it as theirs.
///
/// The server is kept in `so.tilo.servers` and not among the person's devices, so the other
/// devices' lists do not name it (the server OS lane adds it, with a ticket signed through
/// `devices#pending`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_always_on_server_is_one_of_the_persons_devices() {
    let f = Box::pin(four_devices()).await;
    let devices = recorded(&f).await;
    let server = flow::key_of(&f.server_device.cert);
    assert!(
        devices.contains(&server),
        "the always-on server ({server}) is listed among the person's devices, so every device \
         of theirs manages it as theirs; it is kept only in so.tilo.servers, so the list names \
         no server: {devices:?}"
    );
}

/// A workspace the person makes on their desktop, and one they make on their laptop, are both
/// held by their always-on server without asking: it is their server, and it takes their work
/// by default. Turning it off for one workspace through the server's entry drops it there.
///
/// The workspace's holders are read through the read every member may ask beside `copies.set`
/// (`workspace/<id>/servers#keeps`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_always_on_server_holds_all_workspaces_at_once() {
    let f = Box::pin(four_devices()).await;
    let desktop = f.client(&f.desktop_session, &f.desktop, "fedora-desktop");
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
    // The person makes a workspace on each of two devices: their computer that founded their
    // own workspace founds it for them, places it at their server and lists it.
    let mut made: Vec<(&str, String)> = Vec::new();
    for (who, device) in [("the desktop", &f.desktop), ("the laptop", &f.laptop)] {
        let session = crate::support::start_peer(
            f.desk_host.iroh(),
            device.cert.clone(),
            f.desk.cert.clone(),
            device.key.clone(),
        );
        let (listing, _) = flow::client(&session, device, &f.desk.cert, who)
            .own_workspace(
                serde_json::Map::from_iter([("name".to_owned(), json!("Ada's work"))]),
                tilo_actions::InvocationId::new(),
            )
            .await
            .unwrap_or_else(|error| panic!("{who} makes a workspace: {error:?}"));
        session.close();
        made.push((who, listing.workspace));
    }

    // Their server holds each without being asked: it is theirs, and takes their work by
    // default. The workspace's servers child publishes what keeps it, as `keeps` beside
    // `copies.set`.
    for (who, workspace) in &made {
        let keeps = flow::ask_permissions_child(
            &laptop,
            f.work_id(),
            workspace,
            "servers",
            "keeps",
            json!({}),
        )
        .await;
        let listed = keeps
            .as_ref()
            .map_or_else(|error| format!("{error:?}"), ToString::to_string);
        assert!(
            keeps.is_ok(),
            "no published read of a workspace's holders at workspace/{workspace}/servers#keeps \
             (the read beside `copies.set`), which the hosting lane publishes: the workspace \
             {who} made on the person's device is held by their server without asking: {listed}"
        );
        let server = flow::key_of(&f.server_device.cert);
        let kept = keeps
            .as_ref()
            .ok()
            .and_then(|keeps| keeps["keeps"].as_array())
            .is_some_and(|keeps| {
                keeps
                    .iter()
                    .any(|holder| holder["device"].as_str() == Some(server.as_str()))
            });
        assert!(
            kept,
            "the always-on server ({server}) is in `keeps` of the workspace {who} made on the \
             person's device, without being asked: {listed}"
        );
    }

    // Keeping off for the laptop's workspace, through the server's entry: the server lets it
    // go, and no member reads it among the workspace's keepers any more.
    let server = flow::key_of(&f.server_device.cert);
    let (_, laptops) = &made[1];
    keep(&f, &desktop, laptops, false).await;
    crate::support::wait_for("the server lets go of the workspace kept off", || async {
        (!flow::server_serves(&desktop, f.work_id(), laptops).await).then_some(())
    })
    .await;
    let keeps = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        flow::ask_permissions_child(&laptop, f.work_id(), laptops, "servers", "keeps", json!({})),
    )
    .await;
    let named = matches!(&keeps, Ok(Ok(keeps)) if keeps["keeps"]
        .as_array()
        .is_some_and(|keeps| keeps.iter().any(|holder| holder["device"].as_str() == Some(server.as_str()))));
    assert!(
        !named,
        "the server is no keeper of the workspace its entry turned keeping off for: {keeps:?}"
    );
    assert!(
        flow::server_serves(&desktop, f.work_id(), &made[0].1).await,
        "the server still keeps the desktop's workspace"
    );

    // Keeping on again: the server keeps it once more.
    keep(&f, &desktop, laptops, true).await;
    crate::support::wait_for("the server keeps the workspace again", || async {
        flow::server_serves(&desktop, f.work_id(), laptops)
            .await
            .then_some(())
    })
    .await;
}

/// Have the server keep `workspace`, or not, through its entry among the person's servers: the
/// ordinary action on the person's own workspace (`server.keep`), asked by `device`.
async fn keep(
    f: &Four,
    device: &tilo_session_client::device::Device,
    workspace: &str,
    keeps: bool,
) {
    flow::ask_permissions(
        device,
        f.person_id(),
        &f.person_workspace,
        "server.keep",
        json!({
            "server": flow::key_of(&f.server_device.cert),
            "workspace": workspace,
            "keeps": keeps,
        }),
    )
    .await
    .unwrap_or_else(|refusal| panic!("the server's entry takes the choice: {refusal:?}"));
}

/// Four monitors, two on the desktop and one each on the Pi and the laptop, are one
/// arrangement, and a window moved by its move action onto the Pi's monitor is shown there.
///
/// The arrangement is a resource every device reads; the move is not (the owner of the
/// desktop and session lane takes it, waiting on the session running as a daemon client).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_window_moved_by_its_move_action_shows_on_the_other_devices_monitor() {
    let f = Box::pin(four_devices()).await;
    let (desktop, pi, laptop) = (
        flow::key_of(&f.desktop.cert),
        flow::key_of(&f.pi.cert),
        flow::key_of(&f.laptop.cert),
    );
    let arrangement = flow::placements(&[
        (desktop.clone(), "eDP-1".into(), 0, 0),
        (desktop.clone(), "DP-1".into(), 1920, 0),
        (pi.clone(), "HDMI-A-1".into(), 3840, 0),
        (laptop.clone(), "eDP-1".into(), 5760, 0),
    ]);
    let arranged = flow::arrange_displays(&f.desktop_person, f.person, &arrangement).await;
    assert!(
        arranged.is_ok(),
        "the one four-monitor arrangement is a resource of the person's, written by their \
         desktop: {arranged:?}"
    );

    // Every device reads the one arrangement, with its own monitors in it.
    for (label, session, own) in [
        ("the desktop", &f.desktop_person, &desktop),
        ("the Orange Pi", &f.pi_person, &pi),
        ("the laptop", &f.laptop_person, &laptop),
    ] {
        flow::open(session, &flow::settings_route(f.person));
        let read = crate::support::wait_for("the display arrangement at the device", || async {
            flow::displays(session, f.person)
        })
        .await;
        let counted = flow::arranged_devices(&read);
        let expected = if own.as_str() == desktop.as_str() {
            2
        } else {
            1
        };
        assert_eq!(
            counted.get(own).copied().unwrap_or_default(),
            expected,
            "{label} reads the one four-monitor arrangement, its own monitors among them: {read}"
        );
    }

    // The desktop's session moves one of its windows onto the Pi's monitor.
    let desktop_client = f.client(&f.desktop_session, &f.desktop, "fedora-desktop");
    let moved = flow::ask(
        &desktop_client,
        f.work_id(),
        "so.tilo.session",
        "windows",
        "move",
        json!({"window": "work-1", "monitor": {"device": pi, "output": "HDMI-A-1"}}),
    )
    .await;
    assert!(
        moved.is_ok(),
        "the session publishes the windows and their move action, so a window moved onto \
         the Orange Pi's monitor is shown there; no session answers it on the desktop yet \
         (it must run as a daemon client): {moved:?}"
    );
}

/// The laptop falls asleep in the middle of an agent's turn, and the server finishes the
/// turn: the person's agents carry on while their devices are gone.
///
/// The laptop's message to the agent is what starts the turn (Chat's `write_to`), and the turn
/// runs on the server, where the workspace's agents run: the laptop holds none of it.
///
/// The server writes the message, but runs no app (the flows' server daemon finds no
/// `tilo-launcher`, so it reads no app's records), and the turn never starts there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_laptop_asleep_mid_turn_the_server_finishes_it() {
    let f = Box::pin(four_devices()).await;
    let fake = fake_model_providers::FakeProviders::start().await.unwrap();
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
    flow::run_work_on(&laptop, f.work_id(), &fake.openai_api_url(), "custom-model").await;
    flow::make_agent(&laptop, &f.work, "Scout", "scout").await;
    flow::write_to(
        &laptop,
        f.work_id(),
        f.person,
        "scout",
        "Write the standup notes.",
    )
    .await
    .unwrap_or_else(|error| panic!("the laptop writes to the person's agent: {error:?}"));

    // The laptop goes away mid-turn.
    f.laptop_session.close();

    // The turn ends on the server, and the Pi reads what the agent wrote.
    flow::chat_saying(
        &f.pi_session,
        &f.host,
        &f.work,
        "You said: Write the standup notes.",
    )
    .await;
}

/// Five model providers, kept as the workspace's, each answer a turn: Anthropic, Google
/// Vertex, Gemini, `OpenAI` signed in with OAuth, and a custom `OpenAI`-compatible provider,
/// against a fake provider on loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_five_model_providers_each_answer_a_turn() {
    let f = Box::pin(four_devices()).await;
    let fake = fake_model_providers::FakeProviders::start().await.unwrap();
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
    let work = f.work_id();

    let mut asked = Vec::new();
    for (name, protocol, endpoint, model) in [
        (
            "Anthropic",
            model_providers::Protocol::Anthropic,
            fake.anthropic_api_url(),
            "claude-sonnet-5-5",
        ),
        (
            "Google Vertex",
            model_providers::Protocol::OpenAiCompatible,
            fake.openai_api_url(),
            "vertex-model",
        ),
        (
            "Gemini",
            model_providers::Protocol::OpenAiCompatible,
            fake.openai_api_url(),
            "gemini-model",
        ),
        (
            "Custom OpenAI-compatible",
            model_providers::Protocol::OpenAiCompatible,
            fake.openai_api_url(),
            "custom-model",
        ),
    ] {
        asked.push(link_provider(&laptop, work, name, protocol, endpoint, model).await);
    }

    // OpenAI, signed in with OAuth as the person's plan.
    let started = ask_models(&laptop, work, model_providers::BEGIN_SIGN_IN, json!({}))
        .await
        .unwrap_or_else(|error| panic!("the OpenAI sign-in begins: {error:?}"));
    let code = started["code"].as_str().unwrap_or_default().to_owned();
    let sign_in = started["signIn"].as_str().unwrap_or_default().to_owned();
    assert!(fake.approve(&code), "the fake accepted the code {code}");
    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ask_models(
            &laptop,
            work,
            model_providers::WAIT_SIGN_IN,
            json!({"signIn": sign_in}),
        ),
    )
    .await;
    let unanswered = format!("{waited:?}");
    let signed_in = waited
        .ok()
        .and_then(Result::ok)
        .filter(|state| state["state"] != json!("waiting"))
        .unwrap_or_else(|| {
            panic!(
                "the person's OpenAI sign-in, signed in with OAuth at the fake's issuer, \
                 finishes so a turn is answered on their plan: {unanswered}"
            )
        });
    let provider = signed_in["provider"]["id"]
        .as_str()
        .expect("the signed-in provider's id")
        .to_owned();
    let model = signed_in["provider"]["defaultModel"]
        .as_str()
        .expect("the signed-in provider's model")
        .to_owned();
    ask_models(
        &laptop,
        work,
        model_providers::SET_ASSIGNMENT,
        json!({"kind": "request", "provider": provider, "model": model}),
    )
    .await
    .expect("the signed-in plan answers requests");
    asked.push((provider, model));

    // A turn through each provider is answered by the one chosen for it.
    for (provider, model) in asked {
        ask_models(
            &laptop,
            work,
            model_providers::SET_ASSIGNMENT,
            json!({"kind": "request", "provider": provider, "model": model}),
        )
        .await
        .unwrap_or_else(|error| panic!("{provider} answers a request: {error:?}"));
        let answered = ask_models(
            &laptop,
            work,
            model_providers::ASK,
            json!({
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Good morning"}]}],
                "tools": [],
                "tool_choice": "auto",
                "max_output_tokens": 64,
            }),
        )
        .await;
        let said = answered
            .as_ref()
            .map_or_else(|error| format!("{error:?}"), ToString::to_string);
        assert!(
            said.contains(model.as_str()),
            "the turn through provider {provider} ({model}) is answered: {said}"
        );
    }
}

/// Ask the workspace's model providers as `device`.
async fn ask_models(
    device: &tilo_session_client::device::Device,
    workspace: ServerWorkspaceId,
    action: &str,
    arguments: Value,
) -> Result<Value, tilo_session_client::device::DeviceRefusal> {
    flow::ask(
        device,
        workspace,
        model_providers::PROVIDER,
        model_providers::RESOURCE,
        action,
        arguments,
    )
    .await
}

/// Keep `name` as a workspace's provider offering `model`, choose it for requests, and answer
/// its id with the model.
async fn link_provider(
    device: &tilo_session_client::device::Device,
    workspace: ServerWorkspaceId,
    name: &str,
    protocol: model_providers::Protocol,
    endpoint: String,
    model: &str,
) -> (String, String) {
    let added = ask_models(
        device,
        workspace,
        model_providers::ADD_PROVIDER,
        json!({"name": name, "protocol": protocol, "endpoint": endpoint}),
    )
    .await
    .unwrap_or_else(|error| panic!("{name} is kept as the workspace's provider: {error:?}"));
    let id = added["id"].as_str().expect("the provider's id").to_owned();
    ask_models(
        device,
        workspace,
        model_providers::ADD_MODEL,
        json!({"provider": id, "model": model}),
    )
    .await
    .unwrap_or_else(|error| panic!("{name} offers {model}: {error:?}"));
    ask_models(
        device,
        workspace,
        model_providers::SET_KEY,
        json!({"provider": id, "key": "sk-fake-api-0123456789abcdef"}),
    )
    .await
    .unwrap_or_else(|error| panic!("{name} is reached with the key kept for it: {error:?}"));
    ask_models(
        device,
        workspace,
        model_providers::SET_ASSIGNMENT,
        json!({"kind": "request", "provider": id, "model": model}),
    )
    .await
    .unwrap_or_else(|error| panic!("{name} answers a request: {error:?}"));
    (id, model.to_owned())
}

/// An agent signs in to a site with a vault entry it may use but cannot read, then acts on the
/// page.
///
/// The vault is a workspace's and the agent holds a reference to the entry only; the closest
/// in-process path to acting on the page is a web call the host makes with the key struck out
/// of its answer. The hosted browser's page itself is proven in the KVM suite (the owner of
/// the agent engines lane takes that part of this step).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_agent_signs_in_with_a_vault_entry_it_cannot_read_and_acts_on_the_page() {
    let f = Box::pin(four_devices()).await;

    // The site the person has a key for: it answers whoever shows the key, and repeats what it
    // was sent, so a key a careless service handed back would be in the answer.
    let site = test_http::Responder::handle(|request| {
        let shown = request.header("authorization").unwrap_or("none").to_owned();
        Some(
            test_http::Reply::status(200)
                .header("content-type", "application/json")
                .body(format!(
                    r#"{{"report":"the quarter's numbers","seen":{shown:?}}}"#
                )),
        )
    });
    let service = site.url();
    let address = format!("{service}/vault-signin");
    let desktop = f.client(&f.desktop_session, &f.desktop, "fedora-desktop");

    // The key, kept in the workspace's vault by their device.
    let kept = flow::ask(
        &desktop,
        f.work_id(),
        workspace_vault::PROVIDER,
        workspace_vault::RESOURCE,
        workspace_vault::ADD,
        json!({
            "kind": "key",
            "belongsTo": [service],
            "secret": "sk-live-0123456789abcdef0123456789ab",
        }),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "the person keeps a key in the workspace's vault, which Passwords makes on a \
             member's device first, so an agent holds only a reference to it: {error:?}"
        )
    });
    let reference = kept["reference"]
        .as_str()
        .expect("the entry's reference")
        .to_owned();

    // An agent of theirs, certified for this workspace, joins as its own device.
    let workspace = *uuid::Uuid::parse_str(&f.work).unwrap().as_bytes();
    let agent = flow::agent_device(&f.root, "claude-code", AgentScope::Workspace(workspace));
    let agent_session = flow::session(&f.host, &agent, &f.server_device.cert);
    flow::join(&agent_session, &f.work, "Work").await;
    let agent_device = f.client(&agent_session, &agent, "claude-code");

    // It cannot be shown the secret, nor have it copied.
    for showing in [workspace_vault::REVEAL, workspace_vault::COPY] {
        let shown = flow::ask(
            &agent_device,
            f.work_id(),
            workspace_vault::PROVIDER,
            workspace_vault::RESOURCE,
            showing,
            json!({"item": reference}),
        )
        .await;
        assert!(
            shown.is_err(),
            "an agent is handed no secret of its person's vault, only a reference it may use: \
             {showing} {shown:?}"
        );
    }

    // It uses the entry on the site without holding the secret: the call is put to its person
    // first, as a call is theirs to allow each time, and it is sent once they said yes.
    let asked = flow::ask(
        &agent_device,
        f.work_id(),
        workspace_vault::PROVIDER,
        workspace_vault::RESOURCE,
        workspace_vault::WEB_CALL,
        json!({
            "url": address,
            "method": "GET",
            "key": reference,
            "reason": "Signing in to see the report.",
        }),
    )
    .await
    .expect_err("a web call waits for its person's yes");
    assert_eq!(
        asked.code,
        tilo_session_client::device::DeviceRefusal::AWAITING_APPROVAL,
        "{asked:?}"
    );
    let awaiting = asked.awaiting.expect("the call that waits is named");
    assert!(
        site.drain_requests().is_empty(),
        "nothing is sent before the person's yes"
    );
    desktop
        .ask_host(
            ServerWorkspaceId::parse(&awaiting.ask.workspace).expect("the ask's workspace"),
            tilo_actions::remote::RemoteActionTarget {
                provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    &awaiting.ask.record,
                    tilo_actions::remote::asked::RESOLVE,
                ),
            },
            serde_json::Map::from_iter([("resolution".into(), json!("approve"))]),
        )
        .await
        .expect("the person's yes is taken");
    let answered = agent_device
        .answer_of(&awaiting, std::time::Duration::from_secs(60))
        .await
        .unwrap_or_else(|error| panic!("the call the person allowed is answered: {error:?}"));
    let said = answered.to_string();
    assert!(
        said.contains("the quarter's numbers"),
        "the agent acts on the site with the vault entry it may use, and the host answers what \
         the site said: {said}"
    );
    assert!(
        !said.contains("sk-live-0123456789abcdef0123456789ab"),
        "the answer carries no secret the agent never held: {said}"
    );
    let sent = site.drain_requests();
    assert_eq!(
        sent.len(),
        1,
        "the call is sent once, with the key: {sent:?}"
    );
    assert_eq!(
        sent[0].header("authorization"),
        Some("Bearer sk-live-0123456789abcdef0123456789ab"),
        "the vault added the key on the way out"
    );
}

/// Ask the person's devices, through the one device link, for `action` with `arguments`: the
/// typed action a step of this flow needs on `so.tilo.devices/devices`.
async fn device_action(
    f: &Four,
    label: &str,
    action: &str,
    arguments: Value,
) -> Result<Value, tilo_session_client::device::DeviceRefusal> {
    let laptop = f.client(&f.laptop_session, &f.laptop, label);
    flow::ask(
        &laptop,
        f.person_id(),
        "so.tilo.devices",
        "devices",
        action,
        arguments,
    )
    .await
}

/// A gamepad plugged into one of the person's devices plays on another of them: its input is
/// forwarded over the one device link, as if it were plugged in there.
///
/// The person's devices offer no action to forward an input device (the owner of the target
/// flows end to end lane builds it as a typed action on the one device link).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_input_device_on_one_device_is_forwarded_to_another() {
    let f = Box::pin(four_devices()).await;
    let forwarded = device_action(
        &f,
        "snapdragon-laptop",
        "forward-input-device",
        json!({
            "device": flow::key_of(&f.pi.cert),
            "input": {"kind": "gamepad", "id": "usb-054c-0ce6"},
        }),
    )
    .await;
    assert!(
        forwarded.is_ok(),
        "the person's devices offer no action to forward an input device to another of their \
         devices: a gamepad on the desktop plays on the Pi; {forwarded:?}"
    );
}

/// A window open on one device is shown on another of the person's devices, the same app on
/// the same record, not a copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_window_is_shown_on_another_of_the_persons_devices() {
    let f = Box::pin(four_devices()).await;
    // The person's devices are a record of their own workspace, kept by the computer that
    // certifies them.
    let workspaces = f.desk_host.server.workspaces().unwrap();
    for device in [&f.laptop, &f.pi] {
        workspaces.keep_issued_device(&device.cert, "").await;
    }
    let shown = device_action(
        &f,
        "snapdragon-laptop",
        "show-window-on-device",
        json!({
            "from": flow::key_of(&f.laptop.cert),
            "device": flow::key_of(&f.pi.cert),
            "window": {"workspace": f.work, "app": "so.tilo.files", "record": "home/welcome.txt"},
        }),
    )
    .await;
    assert!(
        shown.is_ok(),
        "the person's devices offer no action to show a window of one device on another: the \
         laptop's Files window opens on the Pi's monitor; {shown:?}"
    );
}

/// A quick inline drop hands a file to a nearby device of the person's, apart from Nearby
/// Send's transfer of a whole record.
///
/// The person's devices offer no action for a nearby drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_nearby_drop_hands_a_file_to_a_nearby_device() {
    let f = Box::pin(four_devices()).await;
    let dropped = device_action(
        &f,
        "snapdragon-laptop",
        "nearby-drop",
        json!({
            "device": flow::key_of(&f.pi.cert),
            "file": {"workspace": f.work, "path": "home/welcome.txt"},
        }),
    )
    .await;
    assert!(
        dropped.is_ok(),
        "the person's devices offer no action for a quick inline drop of a file to a nearby \
         device, apart from Nearby Send's transfer: {dropped:?}"
    );
}

/// A device of the person's is added from a nearby one: the new device joins the fleet from
/// the device beside it, without a code typed in.
///
/// The person's devices offer no action to add a device from a nearby one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_device_is_added_from_a_nearby_device() {
    let f = Box::pin(four_devices()).await;
    let added = device_action(
        &f,
        "snapdragon-laptop",
        "add-nearby-device",
        json!({
            "device": flow::key_of(&f.pi.cert),
            "name": "Ada's tablet",
        }),
    )
    .await;
    assert!(
        added.is_ok(),
        "the person's devices offer no action to add a device from a nearby one: a new tablet \
         joins from the laptop beside it, with no code typed in; {added:?}"
    );
}

/// Bluetooth pairing is offered to the person's other devices: an accessory the laptop pairs
/// with is offered there, and one already bonded to the laptop keeps working.
///
/// The person's devices offer no action to offer a pairing to the person's other devices.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_bluetooth_accessory_is_offered_to_the_persons_other_devices() {
    let f = Box::pin(four_devices()).await;
    let offered = device_action(
        &f,
        "snapdragon-laptop",
        "offer-bluetooth-pairing",
        json!({
            "device": flow::key_of(&f.pi.cert),
            "accessory": {"kind": "headphones", "address": "00:11:22:33:44:55"},
        }),
    )
    .await;
    assert!(
        offered.is_ok(),
        "the person's devices offer no action to offer a Bluetooth accessory to the person's \
         other devices, while an accessory already bonded keeps working: {offered:?}"
    );
}
