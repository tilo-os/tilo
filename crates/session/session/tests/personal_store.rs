//! The session keeps the person's own records in their store, and keeps that store open to their
//! other devices: a contact an app adds in Settings' own record lands in the store this device
//! serves, beside the session's state.
//! The store's link itself is answered by the session's endpoint on the pass it binds, so a session
//! that runs keeps one for as long as it does; this test starts a real session and drives the app
//! that writes.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

// A host admits its members' records through an Automerge worker: this binary is that worker, as it
// is the session's own.
tilo_automerge_isolate::test_worker!();

mod common;

use std::path::{Path, PathBuf};

use automerge::transaction::Transactable as _;
use ed25519_dalek::Signer as _;
use runtime_collab::CollabClient;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Options, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

/// Contacts, and the record kind of the person's it keeps: an identity they added, which is theirs
/// alone until their other devices hear of it.
const CONTACTS: &str = "so.tilo.contacts";
const KIND: &str = "tilo.contacts.identities";

/// What Contacts declares of the person's own records, as its package ships.
const SURFACE: &[u8] = b"[[resources]]\nkind = \"tilo.contacts.identities\"\nlabel = \"Contact identities\"\nversion = 2\nperson = true\nfields = []\n";

/// The person's keys: this device's, as the identity store provisioned them.
struct Person {
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    // The environment the identity was provisioned in, restored before the directories it names are
    // removed.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
    _home: tempfile::TempDir,
}

fn person() -> Person {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let home = tempfile::tempdir().expect("a home for this machine's session");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set("FILE_SECRETS_ENV", FILE_SECRETS_ENV)
        .set("HOME", home.path());
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    Person {
        device,
        cert,
        _env: env,
        _root: root,
        _home: home,
    }
}

/// The person's own workspace, held in `state` with this device the one governing it: what the
/// session opens when it starts, so the person's records reach their store as any workspace's
/// records do.
fn hold_workspace(
    state: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    cluster: &str,
) {
    let store_file = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(
        store_file
            .parent()
            .expect("the state's workspace directory"),
    )
    .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&store_file))
        .expect("an owner for this device's workspace");
    let workspace = WorkspaceId::parse(cluster).expect("a workspace id");
    let governor = testing::enroll_interactive_governor_in(&owner, device, cert, cluster)
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
                None,
                std::time::SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
    // The record the app writes, created empty as this device's own, before any app joins it: what
    // the host of a workspace does when one of its records' apps first opens it.
    let route = owner::ApplicationRecord::declared(CONTACTS, &record(), KIND, &[KIND.to_owned()])
        .expect("Contacts declares this record of the person's");
    owner
        .prepare_application_record(
            workspace,
            cert,
            device,
            &route,
            std::time::SystemTime::now(),
        )
        .expect("this device creates the record it hosts");
}

/// The cluster the person's own records route on, which their store carries.
fn cluster(person: &Person) -> String {
    collab_types::personal_cluster(&person.cert.user_public_key).to_string()
}

/// The record of Contacts' the app writes: one of the person's own.
fn record() -> String {
    collab_types::person_record_id(KIND)
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

/// A desk showing the person's own workspace, known by its id and owned by them.
fn desk(person: &Person, cluster: &str) -> DesktopTree {
    let workspace = WorkspaceId::parse(cluster).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(&person.cert.user_public_key));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// What an app presents to join `cluster` 's channel: the certificate, and the device's proof that
/// it is the certificate's.
fn presentation(device: &ed25519_dalek::SigningKey, cert: &DeviceCert, cluster: &str) -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: cluster.to_owned(),
        device_certificate: serde_json::to_vec(cert).expect("a certificate is JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(cluster))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("a presentation encodes")
}

/// A directory for the plane's socket that only this user reaches: the broker refuses to serve one
/// anybody else could open.
fn private_socket() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    let socket = root.path().join("collab.sock");
    (root, socket)
}

/// A session serving Contacts on `socket` , on this computer's certified device, with the person's
/// own workspace on its desk and its state at `state` .
fn session(socket: &Path, person: &Person, cluster: &str, state: &Path) -> Session {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = std::sync::Arc::new(Launches::default());
    launches.started(std::process::id(), CONTACTS, cluster, true);
    let mut session = crate::common::headless(Options {
        collab: true,
        wake: LoopWake::default(),
        action_plane: ActionPlane::At(socket.to_path_buf()),
        person: "Ada".to_owned(),
        launches,
        state: Some(state.to_path_buf()),
        ..Options::default()
    });
    session.handle().replace_tree(desk(person, cluster));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// The channel key of the person's own record.
fn channel(cluster: &str) -> String {
    tilo_action_broker::route::CrdtRoute::record(CONTACTS, cluster, record()).channel_key()
}

/// A signed change of the record, authored by `device` , at the sequence a document signer takes.
fn op(device: &ed25519_dalek::SigningKey, cluster: &str, key: &str, value: &str) -> Vec<u8> {
    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, key, value)
        .expect("the record takes the key");
    document.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Native.change_message()),
    );
    // An app's first write of a record it created is the record whole, as every participant's is.
    let mutation = collab_types::DocumentMutation::Snapshot(document.save())
        .encode()
        .expect("a mutation encodes");
    let author = collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes());
    let now = std::time::SystemTime::now();
    let mut op = collab_types::SignedOp::new_unsigned(
        mutation,
        collab_types::document_artifact_id(CONTACTS, cluster, &record()),
        author,
        collab_types::document_attestation_id(CONTACTS, cluster, &record()),
        collab_types::OpKind::Change,
        now,
        collab_types::next_unordered_document_sequence(now),
    );
    collab_types::sign_op(&mut op, device).expect("the app signs its change");
    postcard::to_stdvec(&op).expect("an op encodes for the wire")
}

/// An app connected to the session's plane as Contacts in the person's own workspace, as the
/// launcher hands it the connection.
fn connect(socket: &Path, cluster: &str) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
    launcher::services::write_collab_handoff(&mut stream, CONTACTS, cluster)
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

/// The person adds a contact in Contacts, on this device.
fn add_contact(socket: &Path, person: &Person, cluster: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, cluster);
        let handle = client
            .join_channel(
                &channel(cluster),
                &presentation(&person.device, &person.cert, cluster),
            )
            .await
            .expect("Contacts joins the person's own record");
        handle
            .send(&op(&person.device, cluster, "name", "Ada"))
            .await
            .expect("the session admits the contact");
    });
}

/// Where the session keeps the person's own documents.
fn store_path(state: &Path) -> PathBuf {
    state.join("tilo").join("personal").join("documents")
}

#[test]
fn a_contact_added_here_is_kept_in_the_persons_own_store() {
    let person = person();
    let cluster = cluster(&person);
    // Contacts declares the kind of the record the app writes, as its package ships it: what the
    // session admits the join against.
    tilo_action_broker::register_package_surface(CONTACTS, SURFACE);
    let (_socket_root, socket) = private_socket();
    let state = tempfile::tempdir().expect("a state for this session");
    hold_workspace(state.path(), &person.device, &person.cert, &cluster);
    let mut session = session(&socket, &person, &cluster, state.path());

    add_contact(&socket, &person, &cluster);

    // The record reaches the person's own store, written beside the session's state as it changes:
    // what this device serves their other devices.
    let path = store_path(state.path());
    let bytes = common::wait_for(&mut session, "the person's store", |session| {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(5))
            .expect("step");
        std::fs::read(&path).ok()
    });
    let store =
        tilo_peer_state::PersonalStore::restore(&bytes, &person.cert, std::time::SystemTime::now())
            .expect("the store this session keeps is this device's to open");
    assert_eq!(
        store.store_key(),
        cluster,
        "it is the store of the person whose records route on their own cluster"
    );
    assert!(
        store.holds_document(CONTACTS, &record()),
        "the contact the app added is in the person's own store, for their other devices"
    );
}
