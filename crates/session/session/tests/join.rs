//! An app of a headless session joins its workspace's channel with the device's certified
//! presentation, as the person's own device does, and another of their devices is in the same
//! channel: each reads the change the other wrote. A presentation this device's identity does not
//! certify, and one of another person, are refused.

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

mod common;

use std::path::{Path, PathBuf};

use automerge::transaction::Transactable as _;
use ed25519_dalek::Signer as _;
use runtime_collab::CollabClient;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, AppLists, Launches, Lists, Options, SearchOptions, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

/// The app whose record the channel carries.
const APP: &str = "so.tilo.notes";
/// The record of `APP` the apps change.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";
/// The workspace the desk shows and the channel is in.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// The person's keys: the identity store's device, and a second device of theirs the store enrolled
/// when it was paired.
#[derive(Clone)]
struct Keys {
    /// The public root of the person's identity, as the workspace's owner.
    root: [u8; 32],
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    other_device: ed25519_dalek::SigningKey,
    other_cert: DeviceCert,
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
    let profile = tilo_identity_host::registry::identity_for_workspace(WORKSPACE)
        .expect("the identity registry")
        .expect("the workspace is answered by this device's person");
    let device = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    // The other of the person's devices, enrolled as a pairing enrolls one: the token is this
    // device's, the certificate is the identity's root's.
    let store = DeviceFleetStore::from_env().expect("the device fleet");
    let now = tilo_clock::unix_now_ns();
    let token = store
        .mint_enrollment(
            &profile.id,
            tilo_user_identity::DeviceCapabilities::member(),
            now,
            now.saturating_add(3_600_000_000_000),
        )
        .expect("this device may enroll another of the person's");
    let other_device = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    let other_cert = store
        .complete_enrollment(
            &profile.id,
            &token,
            tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
            other_device.verifying_key().to_bytes(),
            "The phone",
            now,
        )
        .expect("the second device is enrolled");
    let keys = Keys {
        root: cert.user_public_key,
        device,
        cert,
        other_device,
        other_cert,
    };
    Person {
        keys,
        _env: env,
        _root: root,
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

/// What an app presents to join `route` 's channel: the certificate, and the device's proof that it
/// is the certificate's.
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

/// A desk showing the workspace, known by its id, owned by the person.
fn desk(person: &Keys) -> DesktopTree {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(&person.root));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// A session serving the apps it starts on `socket` , with the person's desk and this device's
/// identity.
fn session(socket: &Path, person: &Keys) -> Session {
    session_of(socket, person, &[APP], None)
}

/// A session serving `apps` as the apps it started, keeping its state — and so its broker's
/// admission journal — at `state` when it is given one.
fn session_of(socket: &Path, person: &Keys, apps: &[&str], state: Option<&Path>) -> Session {
    serving(socket, person, apps, state, Lists::default())
}

/// [`session_of`] for a device whose app lists are `lists` : what the action door lists as each
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
        action_plane: ActionPlane::At(socket.to_path_buf()),
        person: "Ada".to_owned(),
        launches,
        state: state.map(Path::to_path_buf),
        search: SearchOptions {
            lists,
            ..SearchOptions::default()
        },
        ..Options::default()
    });
    session.handle().replace_tree(desk(person));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// The channel key of `app` 's record in the workspace.
fn record_channel(app: &str) -> String {
    tilo_action_broker::route::CrdtRoute::record(app, WORKSPACE, RECORD).channel_key()
}

/// The channel key of the app's record in the workspace.
fn channel() -> String {
    record_channel(APP)
}

/// The app whose settings the keyboard shortcuts are.
const SETTINGS: &str = "so.tilo.settings";

/// The channel key of Settings' record in the workspace.
fn settings_channel() -> String {
    record_channel(SETTINGS)
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
    collab_types::DocumentMutation::Incremental(document.save_after(&[]))
        .encode()
        .expect("a mutation encodes")
}

/// A signed change of the channel's record, authored by `device` .
fn op(device: &ed25519_dalek::SigningKey, key: &str, value: &str, seq: u64) -> Vec<u8> {
    let author = collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
    let mut op = collab_types::SignedOp::new_unsigned(
        change(key, value),
        collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        collab_types::OpKind::Change,
        std::time::SystemTime::now(),
        seq,
    );
    collab_types::sign_op(&mut op, device).expect("the app signs its change");
    postcard::to_stdvec(&op).expect("an op encodes for the wire")
}

/// The next change `stream` delivers that `device` wrote: the channel also carries what this device
/// wrote itself.
async fn written_by(
    stream: &runtime_collab::OpStream,
    device: &ed25519_dalek::SigningKey,
) -> collab_types::SignedOp {
    loop {
        let event = stream.read().await.expect("the channel's changes");
        let op: collab_types::SignedOp =
            postcard::from_bytes(&event.op).expect("a signed change is on the wire");
        if *op.author.as_bytes() == device.verifying_key().to_bytes() {
            return op;
        }
    }
}

/// What `op` wrote under `key` .
fn written(op: &collab_types::SignedOp, key: &str) -> Option<String> {
    use automerge::ReadDoc as _;
    let collab_types::DocumentMutation::Incremental(bytes) =
        collab_types::DocumentMutation::decode(&op.payload).expect("a mutation decodes")
    else {
        panic!("a change carries an incremental mutation");
    };
    let mut document = automerge::AutoCommit::new();
    tilo_automerge::ApplyIncoming::load_incoming(&mut document, &bytes).expect("the change loads");
    document
        .get(automerge::ROOT, key)
        .expect("the record reads")
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok())
}

/// The session's pass until the apps are done, as the session runs its loop.
fn until_the_apps_are_done<T>(session: &mut Session, received: &std::sync::mpsc::Receiver<T>) -> T {
    common::wait_for(session, "the apps to finish their calls", |session| {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
        received.try_recv().ok()
    })
}

/// An app connected to the session's plane as `APP` in `WORKSPACE` , as the launcher hands it the
/// connection.
fn connect(socket: &PathBuf, app: &str) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
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
    handle: &runtime_collab::ChannelHandle,
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
fn settings_lists_shortcuts(
    socket: &PathBuf,
    person: &Keys,
) -> tilo_actions::remote::ReceiptStatus {
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionMessage, RemoteActionTarget,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let settings = connect(socket, SETTINGS);
        let handle = settings
            .join_channel(
                &settings_channel(),
                &presentation(&person.device, &person.cert),
            )
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
            }),
        )
        .await;
        if status.is_terminal() {
            return status;
        }
        answered(
            &handle,
            RemoteActionMessage::Observe {
                invocation,
                wait_ms: Some(5_000),
                seen: None,
            },
        )
        .await
    })
}

/// The settings `app` reads over the session's door, as the JSON document the session answered:
/// joined as the launch the session recorded for it.
fn settings_read(socket: &PathBuf, person: &Keys, app: &str) -> serde_json::Value {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, app);
        let handle = client
            .join_channel(
                &record_channel(app),
                &presentation(&person.device, &person.cert),
            )
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

/// What the apps found: the changes each read from the other, and how the identity store's refusals
/// read.
struct Joined {
    first_reads: collab_types::SignedOp,
    second_reads: collab_types::SignedOp,
    uncertified: String,
    wrong_person: String,
}

/// An app on this device and one on the other of the person's devices, both joining the workspace's
/// channel with their own certificate, each writing a change and reading the other's; then the
/// presentations this device's identity refuses.
fn apps(socket: &PathBuf, person: &Keys) -> Joined {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the apps");
    let (first, second) = (
        presentation(&person.device, &person.cert),
        presentation(&person.other_device, &person.other_cert),
    );
    runtime.block_on(async {
        let this_device = connect(socket, APP);
        let other_device = connect(socket, APP);
        let this_channel = this_device
            .join_channel(&channel(), &first)
            .await
            .expect("this device joins its workspace's channel");
        let other_channel = other_device
            .join_channel(&channel(), &second)
            .await
            .expect("the other device joins it too");
        let this_reads = this_channel
            .subscribe()
            .await
            .expect("the channel's changes");
        let other_reads = other_channel
            .subscribe()
            .await
            .expect("the channel's changes");
        this_channel
            .send(&op(&person.device, "from-this-device", "hello", 1))
            .await
            .expect("the session admits this device's change");
        let heard = written_by(&other_reads, &person.device).await;
        other_channel
            .send(&op(&person.other_device, "from-the-phone", "there", 1))
            .await
            .expect("the session admits the other device's change");
        let answered = written_by(&this_reads, &person.other_device).await;

        // A presentation this device's identity never certified: the certificate is real, the proof
        // is another key's.
        let uncertified = this_device
            .join_channel(
                &channel(),
                &presentation(
                    &ed25519_dalek::SigningKey::from_bytes(&[0x63; 32]),
                    &person.cert,
                ),
            )
            .await
            .err()
            .expect("a proof the certificate does not hold is refused");
        // A presentation of another person: their own root certified it, and it is nobody this
        // workspace's identity has heard of.
        let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[0x64; 32]);
        let stranger_device = ed25519_dalek::SigningKey::from_bytes(&[0x65; 32]);
        let mut stranger = DeviceCert {
            user_public_key: stranger_root.verifying_key().to_bytes(),
            device_public_key: stranger_device.verifying_key().to_bytes(),
            device_label: "Someone else's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut stranger, &stranger_root)
            .expect("their device is certified by their own root");
        let wrong_person = this_device
            .join_channel(&channel(), &presentation(&stranger_device, &stranger))
            .await
            .err()
            .expect("another person's device does not join this workspace");
        Joined {
            first_reads: answered,
            second_reads: heard,
            uncertified: uncertified.to_string(),
            wrong_person: wrong_person.to_string(),
        }
    })
}

#[test]
fn an_app_of_one_of_the_persons_devices_joins_the_channel_and_the_others_answer() {
    let home = common::private_home();
    let socket = home.path().join("collab.sock");
    let person = person();
    let mut session = session(&socket, &person.keys);

    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = person.keys.clone();
    let apps = std::thread::spawn(move || {
        let joined = apps(&plane, &keys);
        let _ = done.send(joined);
    });
    let joined = until_the_apps_are_done(&mut session, &received);
    apps.join().expect("the apps' thread");

    let heard = joined.first_reads;
    assert_eq!(
        heard.author.as_bytes(),
        &person.keys.other_device.verifying_key().to_bytes(),
        "this device reads the change the other wrote"
    );
    assert_eq!(
        written(&heard, "from-the-phone").as_deref(),
        Some("there"),
        "what the other device wrote"
    );
    let answered = joined.second_reads;
    assert_eq!(
        answered.author.as_bytes(),
        &person.keys.device.verifying_key().to_bytes(),
        "and the other device reads this one's"
    );
    assert_eq!(
        written(&answered, "from-this-device").as_deref(),
        Some("hello"),
        "what this device wrote"
    );
    assert!(
        joined.uncertified.contains("proof"),
        "a proof its certificate does not hold is refused: {}",
        joined.uncertified
    );
    assert!(
        joined.wrong_person.contains("identity") || joined.wrong_person.contains("enrolled"),
        "another person's device is refused: {}",
        joined.wrong_person
    );
}

/// A session that keeps its state keeps its broker's admission journal, and still admits the app
/// its launcher started: Settings joins its workspace's channel, and the shortcuts it lists are the
/// settings this device holds.
#[test]
fn settings_lists_the_desktop_shortcuts_over_the_sessions_door() {
    use tilo_actions::remote::ReceiptStatus;
    let home = common::private_home();
    let socket = home.path().join("collab.sock");
    let person = person();
    let state = home.path().join("state");
    std::fs::create_dir_all(&state).expect("a state directory");
    let mut session = session_of(&socket, &person.keys, &[SETTINGS], Some(&state));

    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = person.keys;
    std::thread::spawn(move || {
        let _ = done.send(settings_lists_shortcuts(&plane, &keys));
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
/// door, because the app lists say its manifest declares the system reach; an app that declares
/// none reads the shared sections only.
#[test]
fn the_settings_app_the_app_lists_allow_reaches_every_section() {
    let person = person();
    // A section the session keeps and no app reads as a shared one: what only the device's own
    // settings app may see.
    let mut power = serde_json::Map::new();
    power.insert("probe".to_owned(), serde_json::json!("system"));
    settings_store::update_section(settings_store::power::SECTION, power)
        .expect("the session keeps the power settings");

    let settings = settings_of(&person.keys, SETTINGS, &[(SETTINGS, true)]);
    let notes = settings_of(&person.keys, APP, &[(APP, false)]);
    assert_eq!(
        settings["power"]["probe"], "system",
        "the device's settings app reads the whole document: {settings}"
    );
    assert!(
        notes.get("power").is_none(),
        "an app whose manifest declares no system reach reads the shared sections only: {notes}"
    );
}

/// What `app` reads of the settings over the session's door, in a session whose app lists say of
/// each of `declared` whether its manifest asks for the device's system settings reach.
fn settings_of(person: &Keys, app: &str, declared: &[(&str, bool)]) -> serde_json::Value {
    use tilo_action_broker::services::LaunchableApplication;
    let home = common::private_home();
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
                    vec![settings_store::app_scope::SYSTEM_SETTINGS_CAPABILITY.to_owned()]
                } else {
                    Vec::new()
                },
            })
            .collect(),
        ..AppLists::default()
    };
    let mut session = serving(&socket, person, &[app], None, Lists::fixed(lists));
    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = person.clone();
    let called = app.to_owned();
    std::thread::spawn(move || {
        let _ = done.send(settings_read(&plane, &keys, &called));
    });
    until_the_apps_are_done(&mut session, &received)
}
