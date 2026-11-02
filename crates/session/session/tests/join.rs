//! An app of a headless session joins its workspace's channel with the
//! device's certified presentation, as the person's own device does, and
//! another of their devices is in the same channel: each reads the change the
//! other wrote. A presentation this device's identity does not certify, and
//! one of another person, are refused.

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
use ed25519_dalek::Signer as _;
use runtime_collab::CollabClient;
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceId};
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Options, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// The app whose record the channel carries.
const APP: &str = "so.tilo.notes";
/// The record of `APP` the apps change.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";
/// The workspace the desk shows and the channel is in.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// The person's keys: the identity store's device, and a second device of
/// theirs the store enrolled when it was paired.
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
    // The environment the identity was provisioned in, restored before the
    // directories it names are removed.
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
    // The other of the person's devices, enrolled as a pairing enrolls one:
    // the token is this device's, the certificate is the identity's root's.
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

/// What an app presents to join `route`'s channel: the certificate, and the
/// device's proof that it is the certificate's.
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
    let workspace = RoomId::parse(WORKSPACE).expect("a workspace id");
    let id = WorkspaceId::of_room(workspace);
    let mut workspace = Workspace::in_workspace(id, workspace, "Desk");
    workspace.set_owner(principal(&person.root));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.id,
        ..DesktopTree::default()
    }
}

/// A session serving the apps it starts on `socket`, with the person's desk
/// and this device's identity.
fn session(socket: &Path, person: &Keys) -> Session {
    // The apps speak from this test's own process, so the launches the
    // session would have recorded for them name that process.
    let launches = std::sync::Arc::new(Launches::default());
    launches.started(std::process::id(), APP, WORKSPACE, true);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            wake: LoopWake::default(),
            action_plane: ActionPlane::At(socket.to_path_buf()),
            person: "Ada".to_owned(),
            launches,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(desk(person));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// The channel key of the app's record in the workspace.
fn channel() -> String {
    tilo_action_broker::route::CrdtRoute::record(APP, WORKSPACE, RECORD).channel_key()
}

/// One change of the record: `value` written under `key`, as an app writes it.
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

/// A signed change of the channel's record, authored by `device`.
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

/// The next change `stream` delivers that `device` wrote: the channel also
/// carries what this device wrote itself.
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

/// What `op` wrote under `key`.
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

/// A directory for the plane's socket that only this user reaches: the
/// broker refuses to serve one anybody else could open.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

/// The session's pass until the apps are done, as the session runs its loop.
fn until_the_apps_are_done<T>(session: &mut Session, received: &std::sync::mpsc::Receiver<T>) -> T {
    for _ in 0..4000 {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
        if let Ok(done) = received.try_recv() {
            return done;
        }
    }
    panic!("the apps never finished their calls");
}

/// An app connected to the session's plane as `APP` in `WORKSPACE`, as the
/// launcher hands it the connection.
fn connect(socket: &PathBuf) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
    launcher::services::write_collab_handoff(&mut stream, APP, WORKSPACE)
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

/// What the apps found: the changes each read from the other, and how the
/// identity store's refusals read.
struct Joined {
    first_reads: collab_types::SignedOp,
    second_reads: collab_types::SignedOp,
    uncertified: String,
    wrong_person: String,
}

/// An app on this device and one on the other of the person's devices, both
/// joining the workspace's channel with their own certificate, each writing
/// a change and reading the other's; then the presentations this device's
/// identity refuses.
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
        let this_device = connect(socket);
        let other_device = connect(socket);
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

        // A presentation this device's identity never certified: the
        // certificate is real, the proof is another key's.
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
        // A presentation of another person: their own root certified it, and
        // it is nobody this workspace's identity has heard of.
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
    let home = private_home();
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
