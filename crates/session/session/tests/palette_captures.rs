//! A line typed into the palette with no window of the app open runs that
//! app's capture: Tasks publishes one action that takes what a person writes,
//! the session's search lists it through its action host, and choosing the row
//! delivers the sentence as the action's own line — starting the app when it
//! is not running.
//!
//! The app speaks exactly as Tasks does: one launcher-attested connection to
//! the session's plane, certified as the workspace's member, its surface
//! published on the channel it joined, and the invocations the session
//! addresses back to it read on that same connection.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::Signer as _;
use runtime_collab::{CollabClient, Invocation};
use tilo_actions::ResourceSpec;
use tilo_actions::remote::InvokeArgument;
use tilo_desktop::desk_verbs::{DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
use tilo_session::apps::Apps;
use tilo_session::{ActionPlane, AppActions, Launches, Options, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

/// The app whose capture this is.
const TASKS: &str = "so.tilo.lang.tasks";
/// The workspace the session holds, as the desk names it.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
/// The line a person writes into the palette.
const LINE: &str = "Call the landlord fri 10am";

/// This device's identity and the workspace it hosts, where the session reads them.
struct Fixture {
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    /// The state directory the session keeps its workspaces under.
    state: PathBuf,
    // The identity's environment and files, restored and removed last: the session and the app read
    // them while they run.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let state = root.path().join("state");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(FILE_SECRETS_ENV, "1");
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let _profile = tilo_identity_host::registry::identity_for_workspace(WORKSPACE)
        .expect("the identity registry")
        .expect("the workspace is answered by this device's person");
    let device = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    hold_workspace(&state, &device, &cert);
    Fixture {
        device,
        cert,
        state,
        _env: env,
        _root: root,
    }
}

/// The workspace's owner, as this device holds it in `state` : what the session opens when it
/// starts, with this device the one governing the workspace.
fn hold_workspace(state: &Path, device: &ed25519_dalek::SigningKey, cert: &DeviceCert) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
        .expect("an owner for this device's workspace");
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let governor = testing::enroll_interactive_governor_in(&owner, device, cert, WORKSPACE)
        .expect("this device governs the workspace it hosts");
    // The governor's checkpoint now names this device: the host admits it as the publication a
    // host's own workspace makes.
    let host = governor.workspace_host().device_public_key;
    let frames: Vec<Vec<u8>> = governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect();
    for frame in frames {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                std::time::SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The person `root` certifies, as a workspace names its owner.
fn principal(root: &[u8; 32]) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(*root),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("{other:?} is not a person"),
    }
}

/// A desk showing the workspace, known by its id, owned by the person.
fn desk(fixture: &Fixture) -> DesktopTree {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(&fixture.cert.user_public_key));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// What the app presents to join the workspace's channel: the certificate, and the device's proof
/// that it is the certificate's.
fn presentation(device: &ed25519_dalek::SigningKey, cert: &DeviceCert) -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: WORKSPACE.to_owned(),
        device_certificate: serde_json::to_vec(cert).expect("a certificate is JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                WORKSPACE,
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("a presentation encodes")
}

/// A connection to the session's plane as `app` in the workspace, as the launcher hands an app one.
fn connect(socket: &Path) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
    launcher::services::write_collab_handoff(&mut stream, TASKS, WORKSPACE)
        .expect("the launcher names the app on the connection");
    stream
        .set_nonblocking(true)
        .expect("the connection is ready for the runtime");
    let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
    let (reader, writer) = tokio::io::split(stream);
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);
    client
}

/// The channel key of Tasks' own route in the workspace.
fn channel() -> String {
    tilo_action_broker::route::CrdtRoute::app(TASKS, WORKSPACE).channel_key()
}

/// The session serving the apps it starts on `socket` , holding the fixture's workspace from its
/// state and showing its desk: the session an installed session is, with the apps it launches
/// started through.
fn session(socket: &Path, fixture: &Fixture) -> (Session, Rc<Apps>) {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process: what the broker admits the connection by.
    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), TASKS, WORKSPACE, true);
    let apps = Rc::new(Apps::new(
        tilo_app_launch::SessionEnvironment::from_process(),
        None,
    ));
    let mut session = crate::common::headless(Options {
        collab: false,
        action_plane: ActionPlane::At(socket.to_path_buf()),
        person: principal(&fixture.cert.user_public_key),
        launches,
        state: Some(fixture.state.clone()),
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        ..Options::default()
    });
    session.handle().replace_tree(desk(fixture));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    (session, apps)
}

/// Tasks' capture, as the app publishes it: its root, with the one action that takes the line a
/// person types (`title` is that parameter).
fn capture() -> Vec<ResourceSpec> {
    let capture = tilo_actions::ActionSpec::prompted_param(
        "quick_add",
        "New task",
        tilo_actions::ActionParam::required("title", tilo_actions::ParamType::Text, "What to do")
            .taking_entry(),
        LINE,
        tilo_actions::ExecutionLocus::ClientLocal,
        |text| tilo_actions::EncodedAction {
            type_id: "so.tilo.lang.tasks.quick_add".into(),
            payload: text.as_bytes().to_vec(),
        },
    );
    vec![ResourceSpec::new("tasks", "tl.Tasks.tasks", "Tasks").with_action(capture)]
}

/// What the app publishes: the capture as it crosses the wire, which carries no execution locus of
/// its own.
fn surface() -> Vec<runtime_collab::ExportedAction> {
    tilo_actions::door::survey(&capture())
        .iter()
        .map(runtime_collab::ExportedAction::from)
        .collect()
}

/// The app's side: join the workspace as Tasks, publish its capture, and hand back the first
/// invocation the session addresses to it.
fn tasks(
    socket: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    seen: &std::sync::mpsc::Sender<Invocation>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket);
        let mine = client
            .join_channel(&channel(), &presentation(device, cert))
            .await
            .expect("the app the session launched joins the workspace's channel");
        let invocations = mine
            .subscribe_stable_invocations()
            .await
            .expect("the app listens for the invocations it runs");
        mine.publish_actions(&surface())
            .await
            .expect("Tasks publishes the capture it runs");
        let run = invocations
            .read()
            .await
            .expect("the session addresses the capture to the app");
        let _ = seen.send(run);
    });
}

fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// The open search's rows, as the desk publishes them.
fn rows(session: &Session) -> Vec<ResourceSpec> {
    find(&session.resources(), SEARCH_RESOURCE)
        .map(|search| search.children)
        .unwrap_or_default()
}

fn invoke(session: &mut Session, resource: &str, action: &str, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

/// A line typed into the palette with no Tasks window open runs Tasks' capture: the row is the
/// app's action, and the sentence is its line.
#[test]
fn a_line_typed_into_the_palette_runs_an_app_s_capture_with_no_window_open() {
    let _settled = ui::motion::testing::settled();
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture();
    let (mut session, apps) = session(&socket, &fixture);
    let (seen, delivered) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || {
        tasks(&socket, &fixture.device, &fixture.cert, &seen);
    });
    // Tasks is up and has published its capture: the session's action host lists what the app
    // offers, read from the broker the app published on.
    crate::common::wait_for(&mut session, "Tasks publishes its capture", |_| {
        apps.published(WORKSPACE)
            .iter()
            .any(|published| published.app == TASKS)
            .then_some(())
    });

    invoke(
        &mut session,
        DESK_RESOURCE,
        "search",
        serde_json::json!({ "query": LINE }),
    );
    let capture =
        crate::common::wait_for(&mut session, "the capture answers the line", |session| {
            rows(session)
                .into_iter()
                .find(|row| row.kind == SEARCH_ROW_KIND && row.label == "New task")
        });
    invoke(
        &mut session,
        capture.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );

    // The session hands the app exactly the line the person wrote.
    let run = crate::common::wait_for(&mut session, "the capture reaches Tasks", |_| {
        delivered.try_recv().ok()
    });
    if let Err(panic) = app.join() {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(run.resource, "tasks", "the capture acts on Tasks' root");
    assert_eq!(run.action, "quick_add", "the action the row names");
    let argument = InvokeArgument::from_slot(run.argument.as_deref());
    let InvokeArgument::Json(arguments) = argument else {
        panic!("the capture takes its line as the action's parameter: {argument:?}");
    };
    assert_eq!(
        arguments["title"], LINE,
        "what was typed is the line itself, word for word"
    );
}
