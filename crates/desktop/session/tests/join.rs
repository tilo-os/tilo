//! The apps a headless session launched open their workspace's record as the device's person,
//! each on its own connection: each reads, from the device's store, the change the other wrote.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the apps it serves are done"
)]

use std::path::{Path, PathBuf};

use automerge::transaction::Transactable as _;
use runtime_collab::CollabClient;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_service::LoopWake;
use tilo_session::{AppLists, AppSocket, Launches, Lists, Options, Session};

use crate::common::held::{WORKSPACE, actor, opened};

/// The app whose record the channel carries.
const APP: &str = "so.tilo.notes";
/// The record of `APP` the apps change.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";

/// The person's keys, as the identity store holds them for this device.
#[derive(Clone)]
struct Keys {
    /// The public root of the person's identity, as the workspace's owner.
    root: [u8; 32],
}

/// The person's keys, and the identity they were provisioned in.
struct Person {
    keys: Keys,
    // The environment the identity was provisioned in, restored before the directories it names are
    // removed.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn person() -> Person {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let mut env = tilo_test_env::ScopedEnv::new();
    // This device's settings documents live in the document store under the
    // data home: a fresh one per person keeps a joined device out of another
    // case's settings.
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("XDG_DATA_HOME", root.path().join("data"))
        .set("TILO_IDENTITY_ROOT", &system)
        .set(FILE_SECRETS_ENV, "1");
    // The account's own store below the identity directory, where every reader looks, and the
    // person's devices a certificate is checked against before anything signs with it.
    tilo_identity::sources::test_trust::install();
    let personal = tilo_identity::store::account_identity_dir(&xdg.join("tilo/identity"));
    let paths = IdentityPaths::under(&system);
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    let keys = Keys {
        root: cert.user_public_key,
    };
    Person {
        keys,
        _env: env,
        _root: root,
    }
}

/// A shell showing the workspace, known by its id, owned by the person.
fn layout(person: &Keys) -> Layout {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut workspace = Workspace::new(workspace, "Session");
    workspace.policy.set_owner(actor(&person.root));
    Layout {
        workspaces: vec![workspace.clone()],
        active: workspace.id(),
        ..Layout::default()
    }
}

/// A session serving the apps it starts on `socket` , with the person's shell and this device's
/// identity.
fn session(socket: &Path, person: &Keys) -> Session {
    session_of(socket, person, &[APP], None)
}

/// A session serving `apps` as the apps it started, keeping its state — and so its broker's
/// admission journal — at `state` when it is given one.
fn session_of(socket: &Path, person: &Keys, apps: &[&str], state: Option<&Path>) -> Session {
    serving(socket, person, apps, state, Lists::default())
}

/// [`session_of`] for a device whose app lists are `lists` : what the action API lists as each
/// app's offers and declared capabilities.
fn serving(
    socket: &Path,
    person: &Keys,
    apps: &[&str],
    state: Option<&Path>,
    lists: Lists,
) -> Session {
    // The apps speak from this test's own process, so the launches the session would have recorded
    // for them name that process.
    let launches = std::sync::Arc::new(Launches::default());
    for app in apps {
        launches.started(std::process::id(), app, WORKSPACE, true);
    }
    let mut session = crate::common::headless(Options {
        collab: false,
        wake: LoopWake::default(),
        app_socket: AppSocket::At(socket.to_path_buf()),
        person: "Ada".to_owned(),
        launches,
        state: state.map(Path::to_path_buf),
        lists,
        ..Options::default()
    });
    session.handle().replace_tree(layout(person));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// The channel key of `app` 's record in the workspace.
fn document_channel(app: &str) -> String {
    tilo_workspace_service::apps::route::CrdtRoute::record(app, WORKSPACE, RECORD).channel_key()
}

/// The channel key of the app's record in the workspace.
fn channel() -> String {
    document_channel(APP)
}

/// The app whose settings the keyboard shortcuts are.
const SETTINGS: &str = "so.tilo.settings";

/// The channel key of Settings' record in the workspace.
fn settings_channel() -> String {
    document_channel(SETTINGS)
}

/// One change of the record: `value` written under `key` , as an app writes it.
fn change(key: &str, value: &str) -> Vec<u8> {
    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, key, value)
        .expect("the record takes the key");
    document.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Native.change_message()),
    );
    document.save_after(&[])
}

/// The device's store's answer to `command` about the channel's record.
async fn ask(
    channel: &runtime_collab::RecordHandle,
    command: &collab_types::store::wire::Command,
) -> collab_types::store::wire::Reply {
    let command = serde_json::to_vec(command).expect("a store command encodes");
    let reply = channel
        .resource_command(&command)
        .await
        .expect("the store answers the app");
    serde_json::from_slice(&reply).expect("the store's reply reads")
}

/// Hand the store the app's own change writing `value` under `key` .
async fn write(channel: &runtime_collab::RecordHandle, key: &str, value: &str) {
    let reply = ask(
        channel,
        &collab_types::store::wire::Command::RecordChange {
            changes: collab_types::store::wire::Bytes(change(key, value)),
            more: false,
            commit: None,
            record: None,
        },
    )
    .await;
    assert!(
        matches!(reply, collab_types::store::wire::Reply::Written),
        "the session takes the app's change: {reply:?}"
    );
}

/// The record as the store holds it, every change of it read.
async fn record(channel: &runtime_collab::RecordHandle) -> automerge::AutoCommit {
    let collab_types::store::wire::Reply::RecordChanges {
        changes,
        next: None,
        ..
    } = ask(
        channel,
        &collab_types::store::wire::Command::RecordChanges {
            after: Vec::new(),
            from: 0,
            record: None,
        },
    )
    .await
    else {
        panic!("the store answers a small record's read with all its changes at once");
    };
    let mut document = automerge::AutoCommit::new();
    tilo_automerge::ApplyIncoming::load_incoming(&mut document, &changes.0)
        .expect("the changes load");
    document
}

/// What `document` holds under `key` .
fn written(document: &automerge::AutoCommit, key: &str) -> Option<String> {
    use automerge::ReadDoc as _;
    document
        .get(automerge::ROOT, key)
        .expect("the record reads")
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok())
}

/// The session's pass until the apps are done, as the session runs its loop.
fn until_the_apps_are_done<T>(session: &mut Session, received: &std::sync::mpsc::Receiver<T>) -> T {
    crate::common::wait_for(session, "the apps to finish their calls", |session| {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
        received.try_recv().ok()
    })
}

/// An app connected to the session's app wire as `APP` in `WORKSPACE` , as the launcher hands it the
/// connection.
fn connect(socket: &PathBuf, app: &str) -> CollabClient {
    let mut stream =
        std::os::unix::net::UnixStream::connect(socket).expect("the app wire's socket");
    launcher::services::write_collab_handoff(&mut stream, app, WORKSPACE)
        .expect("the launcher names the app on the connection");
    stream
        .set_nonblocking(true)
        .expect("the connection is ready for the runtime");
    let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
    let (reader, writer) = stream.into_split();
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);
    client
}

/// What the session answers to `message` on `handle` , as the status it leaves the invocation in.
async fn answered(
    handle: &runtime_collab::RecordHandle,
    message: tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::ReceiptStatus {
    use tilo_actions::remote::{ReceiptStatus, RemoteActionFrame, RemoteActionMessage};
    let frame = RemoteActionFrame::new(message)
        .encode()
        .expect("a remote action encodes");
    let answer = handle
        .remote_action(&frame)
        .await
        .expect("the session answers the app");
    match RemoteActionFrame::decode(&answer)
        .expect("the answer decodes")
        .message
    {
        RemoteActionMessage::Receipt(receipt) => receipt.status,
        RemoteActionMessage::Update {
            state: Some(state), ..
        } => match state.update {
            tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
                ReceiptStatus::Completed { output }
            }
            other => panic!("the invocation never succeeded: {other:?}"),
        },
        other => panic!("the session answered {other:?}"),
    }
}

/// Settings, the app the session launched, joins its workspace's channel and lists the desktop's
/// shortcuts through the session's services: the answer it gets back from the settings it holds.
fn settings_lists_shortcuts(socket: &PathBuf) -> tilo_actions::remote::ReceiptStatus {
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let settings = connect(socket, SETTINGS);
        let handle = opened(&settings, &settings_channel())
            .await
            .expect("the app the session launched joins its workspace's channel");
        let invocation = tilo_actions::InvocationId::new();
        let status = answered(
            &handle,
            RemoteActionMessage::Invoke(InvokeRequest {
                invocation,
                target: RemoteActionTarget {
                    provider: tilo_actions::remote::workspace_provider(WORKSPACE),
                    address: tilo_actions::ActionAddress::new(
                        tilo_sheets::keyboard_shortcuts::RESOURCE,
                        tilo_sheets::keyboard_shortcuts::LIST_ACTION,
                    ),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                on_behalf_of: None,
                revision: None,
                wait_ms: Some(5_000),
                appearance: None,
                signed: None,
            }),
        )
        .await;
        if status.is_terminal() {
            return status;
        }
        answered(
            &handle,
            RemoteActionMessage::Observe {
                correlation: 0,
                invocation,
                wait_ms: Some(5_000),
                seen: None,
            },
        )
        .await
    })
}

/// The settings `app` reads over the session's API, as the JSON document the session answered:
/// joined as the launch the session recorded for it.
fn settings_read(socket: &PathBuf, app: &str) -> serde_json::Value {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, app);
        let handle = opened(&client, &document_channel(app))
            .await
            .expect("the app the session launched joins its workspace's channel");
        let view = handle
            .settings()
            .read(0)
            .await
            .expect("the session answers the app's settings");
        serde_json::from_slice(&view.document_json).expect("the settings view is JSON")
    })
}

/// What the apps found: the changes each read from the other.
struct Joined {
    first_reads: automerge::AutoCommit,
    second_reads: automerge::AutoCommit,
}

/// Two apps of this device, each on its own connection, both opening the workspace's record as
/// the device's person, each writing a change and reading the other's.
fn apps(socket: &PathBuf) -> Joined {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the apps");
    runtime.block_on(async {
        let first = connect(socket, APP);
        let second = connect(socket, APP);
        let first_record = opened(&first, &channel())
            .await
            .expect("an app the session launched opens its workspace's record");
        let second_record = opened(&second, &channel()).await.expect("so does another");
        write(&first_record, "from-the-first", "hello").await;
        let heard = record(&second_record).await;
        write(&second_record, "from-the-second", "there").await;
        let answered = record(&first_record).await;

        Joined {
            first_reads: answered,
            second_reads: heard,
        }
    })
}

#[test]
fn two_apps_of_the_device_read_each_others_changes_of_its_workspaces_record() {
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let person = person();
    let mut session = session(&socket, &person.keys);

    let (done, received) = std::sync::mpsc::channel();
    let app_wire = socket;
    let apps = std::thread::spawn(move || {
        let joined = apps(&app_wire);
        let _ = done.send(joined);
    });
    let joined = until_the_apps_are_done(&mut session, &received);
    apps.join().expect("the apps' thread");

    let heard = joined.first_reads;
    assert_eq!(
        written(&heard, "from-the-second").as_deref(),
        Some("there"),
        "the first app reads the change the second wrote"
    );
    let answered = joined.second_reads;
    assert_eq!(
        written(&answered, "from-the-first").as_deref(),
        Some("hello"),
        "and the second reads the first's"
    );
}

/// A session that keeps its state keeps its broker's admission journal, and still admits the app
/// its launcher started: Settings joins its workspace's channel, and the shortcuts it lists are the
/// settings this device holds.
#[test]
fn settings_lists_the_desktop_shortcuts_over_the_sessions_api() {
    use tilo_actions::remote::ReceiptStatus;
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let person = person();
    let state = home.path().join("state");
    std::fs::create_dir_all(&state).expect("a state directory");
    let mut session = session_of(&socket, &person.keys, &[SETTINGS], Some(&state));

    let (done, received) = std::sync::mpsc::channel();
    let app_wire = socket;
    std::thread::spawn(move || {
        let _ = done.send(settings_lists_shortcuts(&app_wire));
    });
    let status = until_the_apps_are_done(&mut session, &received);

    let ReceiptStatus::Completed { output } = status else {
        panic!("Settings lists the desktop's shortcuts: {status:?}");
    };
    let value = output.value.expect("the list answers with its rows");
    let rows = value["shortcuts"]
        .as_array()
        .expect("the catalogue answers with its rows");
    assert!(!rows.is_empty(), "the desktop has shortcuts to list");
    assert!(
        rows.iter().any(|row| row["id"] == "open-terminal"
            && row["defaults"]
                .as_array()
                .is_some_and(|defaults| defaults.iter().any(|chord| chord == "Ctrl+Alt+T"))),
        "the desktop's own shortcuts are listed: {rows:?}"
    );
}

/// The device's settings app reaches every section of the settings document over the session's
/// API, because the app lists say its manifest declares the system reach; an app that declares
/// none reads the shared sections only.
#[test]
fn the_settings_app_the_app_lists_allow_reaches_every_section() {
    let person = person();
    // A section the session keeps and no app reads as a shared one: what only the device's own
    // settings app may see. It is set on a device whose session is not running, as `tilo` sets it
    // there: kept for the session that follows, and left at once for what the device's processes
    // read. A device whose session never ran has not named its key yet, so its sections stand
    // where the settings outside the session make them: `devices//settings/power`.
    settings_store::outside::set_on_this_device(
        &settings_store::placement::address(settings_store::power::SECTION, None, "").id,
        &settings_store::workspace::verb("blank_minutes"),
        &serde_json::json!(7),
    )
    .expect("the session keeps the power settings");

    let settings = settings_of(&person.keys, SETTINGS, &[(SETTINGS, true)]);
    let notes = settings_of(&person.keys, APP, &[(APP, false)]);
    assert_eq!(
        settings["power"]["blank_minutes"], 7,
        "the device's settings app reads the whole document: {settings}"
    );
    assert!(
        notes.get("power").is_none(),
        "an app whose manifest declares no system reach reads the shared sections only: {notes}"
    );
}

/// What `app` reads of the settings over the session's API, in a session whose app lists say of
/// each of `declared` whether its manifest asks for the device's system settings reach.
fn settings_of(person: &Keys, app: &str, declared: &[(&str, bool)]) -> serde_json::Value {
    use tilo_workspace_service::apps::services::LaunchableApplication;
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let lists = AppLists {
        launchable: declared
            .iter()
            .map(|(package_id, asks)| LaunchableApplication {
                package_id: (*package_id).to_owned(),
                display_name: (*package_id).to_owned(),
                unconfined: false,
                offers: Vec::new(),
                capabilities: if *asks {
                    vec![settings_store::workspace::SET_EVERY_SETTING.to_owned()]
                } else {
                    Vec::new()
                },
                surface: None,
            })
            .collect(),
        ..AppLists::default()
    };
    let mut session = serving(&socket, person, &[app], None, Lists::fixed(lists));
    let (done, received) = std::sync::mpsc::channel();
    let app_wire = socket;
    let called = app.to_owned();
    std::thread::spawn(move || {
        let _ = done.send(settings_read(&app_wire, &called));
    });
    until_the_apps_are_done(&mut session, &received)
}
