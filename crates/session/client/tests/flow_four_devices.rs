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

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{person_models, person_vault};
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId as ServerWorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_session::workspaces::{
    PersonalDevice, PersonalList, PersonalWorkspace, WorkspaceLabels,
};
use tilo_session_client::Link;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, UserIdentity};

use crate::support::flow;
use crate::support::{Device, TestHost, enrol, hears, record_of, start_workspace_host};

/// The person's four devices, entered into every workspace they have, with a session each
/// for the three that run a Tilo shell; the fourth is the server itself.
struct Four {
    root: SigningKey,
    person: [u8; 32],
    server_device: Device,
    desktop: Device,
    pi: Device,
    laptop: Device,
    host: TestHost,
    work: String,
    home: String,
    person_workspace: String,
    desktop_session: Link,
    pi_session: Link,
    laptop_session: Link,
    _state: tempfile::TempDir,
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
    let server_device = enrol(&root, "tilo.sh", DeviceCapabilities::member());
    let desktop = enrol(&root, "fedora-desktop", DeviceCapabilities::primary());
    let pi = enrol(&root, "orange-pi", DeviceCapabilities::member());
    let laptop = enrol(&root, "snapdragon-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&root, &server_device, state.path(), false).await;
    let person = root.verifying_key().to_bytes();
    // The person's workspaces are theirs: the first of their devices to take one founds it on
    // their host, and every other device of theirs joins it.
    let work = uuid::Uuid::new_v4().to_string();
    let home = uuid::Uuid::new_v4().to_string();
    let person_workspace = person_workspace_id(&person);
    let desktop_session = flow::session(&host, &desktop, &server_device.cert);
    let pi_session = flow::session(&host, &pi, &server_device.cert);
    let laptop_session = flow::session(&host, &laptop, &server_device.cert);
    // The desktop founds each of the person's workspaces on their host; the others take one
    // in once it exists, through the same placement, which enters them and hands them the
    // workspace.
    flow::join(&desktop_session, &work, "Work").await;
    flow::join(&desktop_session, &home, "Home").await;
    flow::join(&desktop_session, &person_workspace, "Servers").await;
    for session in [&pi_session, &laptop_session] {
        flow::join(session, &work, "Work").await;
        flow::join(session, &home, "Home").await;
        flow::join(session, &person_workspace, "Servers").await;
    }
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
        pi_session,
        laptop_session,
        _state: state,
    }
}

/// The list the desktop keeps for the person: their workspaces and their four devices.
fn personal(f: &Four) -> PersonalList {
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
    let device = |cert: &DeviceCert, name: &str| PersonalDevice {
        cert: cert.clone(),
        name: Some(name.to_owned()),
        revoked: false,
        browser: None,
        agent: None,
        last_active_ms: None,
        online: true,
    };
    PersonalList {
        workspaces: vec![workspace(&f.work, "Work"), workspace(&f.home, "Home")],
        devices: vec![
            device(&f.desktop.cert, "Fedora desktop"),
            device(&f.pi.cert, "Orange Pi"),
            device(&f.laptop.cert, "Laptop"),
            device(&f.server_device.cert, "tilo.sh"),
        ],
    }
}

/// All four devices use the person's identity and have all their workspaces, apps and
/// files: each lists both workspaces and all four devices, and reads the file the desktop
/// wrote and the shared desktop under which the apps' windows live.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_all_four_devices_see_every_workspace_app_and_file() {
    let f = four_devices().await;

    // The server holds both of the person's workspaces at once.
    let workspaces = f.host.server.workspaces().unwrap();
    assert!(
        workspaces.summary(&f.work).await.is_ok() && workspaces.summary(&f.home).await.is_ok(),
        "the always-on server governs every workspace of the person"
    );

    // The desktop's device says which workspaces and devices are the person's.
    let desktop = f.client(&f.desktop_session, &f.desktop, "fedora-desktop");
    desktop
        .keep_personal(&personal(&f))
        .await
        .expect("a device of the person's keeps their list on the host");

    // Every other device reads the same list: both workspaces and all four devices.
    for (label, session, device) in [
        ("the Orange Pi", &f.pi_session, &f.pi),
        ("the laptop", &f.laptop_session, &f.laptop),
    ] {
        let client = f.client(session, device, &label.replace(' ', "-"));
        let listed = client
            .personal_list_in(f.work_id(), &WorkspaceLabels::new(), None)
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
        let devices: Vec<String> = listed
            .devices
            .iter()
            .map(|listed| flow::key_of(&listed.cert))
            .collect();
        for certified in [&f.desktop, &f.pi, &f.laptop, &f.server_device] {
            assert!(
                devices.contains(&flow::key_of(&certified.cert)),
                "{label} sees every device of the person: {devices:?}"
            );
        }
    }

    // A file the desktop writes is the file every other device reads.
    let file = DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: f.work.clone(),
        document: "home/welcome".into(),
    };
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "text", "hello from the desktop").unwrap();
    doc.commit();
    let wrote = flow::try_written(
        &f.desktop_session,
        &file,
        &DocumentMutation::Snapshot(doc.save()),
    )
    .await;
    assert!(
        wrote.is_ok(),
        "the file the desktop writes is the workspace's, which every device of theirs reads: \
         {wrote:?}"
    );
    for (label, session) in [
        ("the Orange Pi", &f.pi_session),
        ("the laptop", &f.laptop_session),
    ] {
        flow::open(session, &file);
        hears(session, &file, "text").await;
        assert_eq!(
            crate::support::resource_of(session, &file)
                .and_then(|doc| doc.get(ROOT, "text").unwrap().unwrap().0.into_string().ok())
                .as_deref(),
            Some("hello from the desktop"),
            "{label} reads the file the desktop wrote"
        );
    }

    // The apps' windows live on the workspace's shared desktop, which every device shows.
    let desktop_route = DocumentRoute {
        app_id: flow::DESKTOP_APP.into(),
        cluster_id: f.work.clone(),
        document: flow::DESKTOP_DOCUMENT.into(),
    };
    let mut shared = AutoCommit::new();
    let layout = Layout {
        active: WorkspaceId::parse(&f.work).unwrap(),
        workspaces: vec![Workspace::new(WorkspaceId::parse(&f.work).unwrap(), "Work")],
        ..Layout::default()
    };
    tilo_desktop_model::shared_resource::write(
        &mut shared,
        &SharedDesktop {
            tree: layout,
            records: Vec::new(),
            authors: std::collections::BTreeMap::default(),
            opened_by: std::collections::BTreeMap::default(),
        },
    )
    .unwrap();
    shared.commit();
    let published = flow::try_written(
        &f.desktop_session,
        &desktop_route,
        &DocumentMutation::Snapshot(shared.save()),
    )
    .await;
    assert!(
        published.is_ok(),
        "the desktop's shared desktop, which holds every open app's window, is written by a \
         device of the person's: {published:?}"
    );
    for (label, session) in [
        ("the Orange Pi", &f.pi_session),
        ("the laptop", &f.laptop_session),
    ] {
        flow::open(session, &desktop_route);
        let seen = crate::support::wait_for("the shared desktop at the device", || async {
            record_of(session, &desktop_route)
                .and_then(|bytes| AutoCommit::load(&bytes).ok())
                .and_then(|doc| tilo_desktop_model::shared_resource::read(&doc, None))
                .filter(|desktop| !desktop.tree.workspaces.is_empty())
        })
        .await;
        assert_eq!(
            seen.tree.workspaces.len(),
            1,
            "{label} shows the workspace the desktop arranged, with its open apps"
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
    let f = four_devices().await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
    let listed = laptop
        .personal_list_in(f.work_id(), &WorkspaceLabels::new(), None)
        .await
        .expect("the server answers the person's list");
    let devices: Vec<String> = listed
        .devices
        .iter()
        .map(|listed| flow::key_of(&listed.cert))
        .collect();
    let server = flow::key_of(&f.server_device.cert);
    assert!(
        devices.contains(&server),
        "the always-on server ({server}) is listed among the person's devices, so every device \
         of theirs manages it as theirs; it is kept only in so.tilo.servers, so the list names \
         no server: {devices:?}"
    );
}

/// The always-on server holds all of the person's workspaces at once: every workspace is
/// served by it at the same time, not one at a time.
///
/// The hosting lane takes this step.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_always_on_server_holds_all_workspaces_at_once() {
    let f = four_devices().await;
    let workspaces = f.host.server.workspaces().unwrap();
    for (label, workspace) in [("Work", &f.work), ("Home", &f.home)] {
        let summary = workspaces.summary(workspace).await;
        assert!(
            summary.is_ok(),
            "the always-on server holds the person's {label} workspace while it holds the \
             others, not one at a time: {summary:?}"
        );
    }
    // And their other devices see every workspace it holds at once.
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
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
        ids.contains(&f.work.as_str()) && ids.contains(&f.home.as_str()),
        "the server holds every workspace of the person at once, and their other devices see \
         them: {ids:?}"
    );
}

/// Four monitors, two on the desktop and one each on the Pi and the laptop, are one
/// arrangement, and a window moved by its move action onto the Pi's monitor is shown there.
///
/// The arrangement is a resource every device reads; the move is not (the owner of the
/// desktop and session lane takes it, waiting on the session running as a daemon client).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_window_moved_by_its_move_action_shows_on_the_other_devices_monitor() {
    let f = four_devices().await;
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
    let arranged = flow::arrange_displays(&f.desktop_session, f.person, &arrangement).await;
    assert!(
        arranged.is_ok(),
        "the one four-monitor arrangement is a resource of the person's, written by their \
         desktop: {arranged:?}"
    );

    // Every device reads the one arrangement, with its own monitors in it.
    for (label, session, own) in [
        ("the desktop", &f.desktop_session, &desktop),
        ("the Orange Pi", &f.pi_session, &pi),
        ("the laptop", &f.laptop_session, &laptop),
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
/// No published action starts a turn the server carries on (the owner of the agent engines
/// lane takes this step).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_laptop_asleep_mid_turn_the_server_finishes_it() {
    let f = four_devices().await;
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
    let started = flow::ask(
        &laptop,
        f.work_id(),
        "so.tilo.chat",
        "chat",
        "start-turn",
        json!({"prompt": "Write the standup notes."}),
    )
    .await;
    let started = started.unwrap_or_else(|error| {
        panic!(
            "a published action starts an agent turn in the workspace so the server can finish \
             it when the laptop sleeps: {error:?}"
        )
    });
    let turn = started
        .get("turn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    // The laptop goes away mid-turn.
    f.laptop_session.close();

    // The turn ends on the server, with what the agent wrote.
    let turned = crate::support::wait_for("the server to finish the turn", || async {
        let device = f.client(&f.pi_session, &f.pi, "orange-pi");
        flow::ask(
            &device,
            f.work_id(),
            "so.tilo.chat",
            "chat",
            "turn",
            json!({ "turn": turn }),
        )
        .await
        .ok()
        .filter(|state| state.get("state").and_then(Value::as_str) == Some("done"))
    })
    .await;
    assert!(
        turned.get("answer").is_some(),
        "the server finishes the turn the laptop began and the Pi reads its answer: {turned}"
    );
}

/// Five model providers, kept as the workspace's, each answer a turn: Anthropic, Google
/// Vertex, Gemini, `OpenAI` signed in with OAuth, and a custom `OpenAI`-compatible provider,
/// against a fake provider on loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_five_model_providers_each_answer_a_turn() {
    let f = four_devices().await;
    let workspaces = f.host.server.workspaces().unwrap();
    workspaces.call_private_model_addresses(true);
    let fake = fake_model_providers::FakeProviders::start().await.unwrap();
    workspaces.sign_in_to_openai_at(tilo_server::workspaces::OpenAiSignIn {
        issuer: fake.issuer_url(),
        backend: fake.chatgpt_backend_url(),
        api: fake.openai_api_url(),
        ..Default::default()
    });
    let laptop = f.client(&f.laptop_session, &f.laptop, "snapdragon-laptop");
    let work = f.work_id();

    let mut asked = Vec::new();
    for (name, protocol, endpoint, model) in [
        (
            "Anthropic",
            person_models::Protocol::Anthropic,
            fake.anthropic_api_url(),
            "claude-sonnet-5-5",
        ),
        (
            "Google Vertex",
            person_models::Protocol::OpenAiCompatible,
            fake.openai_api_url(),
            "vertex-model",
        ),
        (
            "Gemini",
            person_models::Protocol::OpenAiCompatible,
            fake.openai_api_url(),
            "gemini-model",
        ),
        (
            "Custom OpenAI-compatible",
            person_models::Protocol::OpenAiCompatible,
            fake.openai_api_url(),
            "custom-model",
        ),
    ] {
        asked.push(link_provider(&laptop, work, name, protocol, endpoint, model).await);
    }

    // OpenAI, signed in with OAuth as the person's plan.
    let started = ask_models(&laptop, work, person_models::BEGIN_SIGN_IN, json!({}))
        .await
        .unwrap_or_else(|error| panic!("the OpenAI sign-in begins: {error:?}"));
    let code = started["code"].as_str().unwrap_or_default().to_owned();
    let sign_in = started["sign_in"].as_str().unwrap_or_default().to_owned();
    assert!(fake.approve(&code), "the fake accepted the code {code}");
    let waited = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        ask_models(
            &laptop,
            work,
            person_models::WAIT_SIGN_IN,
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
        person_models::SET_ASSIGNMENT,
        json!({"kind": "request", "provider": provider, "model": model}),
    )
    .await
    .expect("the signed-in plan answers requests");
    asked.push((provider, model));

    // A turn through each provider is answered by the one chosen for it.
    for (provider, model) in asked {
        let answered = ask_models(
            &laptop,
            work,
            person_models::ASK,
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
        person_models::PROVIDER,
        person_models::RESOURCE,
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
    protocol: person_models::Protocol,
    endpoint: String,
    model: &str,
) -> (String, String) {
    let added = ask_models(
        device,
        workspace,
        person_models::ADD_PROVIDER,
        json!({"name": name, "protocol": protocol, "endpoint": endpoint}),
    )
    .await
    .unwrap_or_else(|error| panic!("{name} is kept as the workspace's provider: {error:?}"));
    let id = added["id"].as_str().expect("the provider's id").to_owned();
    ask_models(
        device,
        workspace,
        person_models::ADD_MODEL,
        json!({"provider": id, "model": model}),
    )
    .await
    .unwrap_or_else(|error| panic!("{name} offers {model}: {error:?}"));
    ask_models(
        device,
        workspace,
        person_models::SET_ASSIGNMENT,
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
    let f = four_devices().await;
    let workspaces = f.host.server.workspaces().unwrap();
    workspaces.call_private_model_addresses(true);

    // A site the person has a login at, kept in the workspace's vault by their device.
    let site = "http://127.0.0.1:9/vault-signin";
    let kept = flow::ask(
        &f.client(&f.desktop_session, &f.desktop, "fedora-desktop"),
        f.work_id(),
        person_vault::PROVIDER,
        person_vault::RESOURCE,
        person_vault::ADD,
        json!({
            "kind": "login",
            "belongsTo": [site],
            "username": "ada@example.com",
            "secret": "hunter2",
        }),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "the person keeps a login in the workspace's vault, which Passwords makes on a \
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
    workspaces.enter(&f.work, &agent.cert).await.unwrap();
    let agent_session = flow::session(&f.host, &agent, &f.server_device.cert);
    flow::join(&agent_session, &f.work, "Work").await;
    let agent_device = f.client(&agent_session, &agent, "claude-code");

    // It cannot read the entry.
    let entries = flow::ask(
        &agent_device,
        f.work_id(),
        person_vault::PROVIDER,
        person_vault::RESOURCE,
        person_vault::ENTRIES,
        json!({}),
    )
    .await;
    assert!(
        entries.is_err(),
        "an agent reads no entry of its person's Passwords, only a reference it may use: {entries:?}"
    );

    // It uses the entry on the site without holding the secret.
    let answered = flow::ask(
        &agent_device,
        f.work_id(),
        person_vault::PROVIDER,
        person_vault::RESOURCE,
        person_vault::WEB_CALL,
        json!({
            "url": site,
            "method": "GET",
            "key": reference,
            "reason": "Signing in to see the report.",
        }),
    )
    .await;
    let said = answered
        .as_ref()
        .map_or_else(|error| format!("{error:?}"), ToString::to_string);
    assert!(
        answered.is_ok(),
        "the agent acts on the site with the vault entry it may use, and the host answers with \
         the key struck out: {said}"
    );
    assert!(
        !said.contains("hunter2"),
        "the answer carries no secret the agent never held: {said}"
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
    let f = four_devices().await;
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
///
/// The person's devices offer no action to show a window on another device.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flow_window_is_shown_on_another_of_the_persons_devices() {
    let f = four_devices().await;
    let shown = device_action(
        &f,
        "snapdragon-laptop",
        "show-window-on-device",
        json!({
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
    let f = four_devices().await;
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
    let f = four_devices().await;
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
    let f = four_devices().await;
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
