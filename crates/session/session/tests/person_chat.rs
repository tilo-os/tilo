//! Chat's private conversations, as the session serves them to the app: the
//! workspace's owner lists the other people here, opens the conversation with
//! one of them, sends, and tells the app when the conversation changed, so a
//! message written by another of the person's devices arrives without the app
//! asking again.
//!
//! The people of this workspace are the person whose device runs the session,
//! another device of theirs, and someone else the workspace certified. The
//! bodies of the conversation live only in the owner's private person state,
//! and the app reaches them as the certificate the broker attested on its
//! connection.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the apps it serves are done"
)]

use std::path::{Path, PathBuf};

use ed25519_dalek::Signer as _;
use runtime_collab::CollabClient;
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::caller_authority::CallerAuthority;
use tilo_peer_state::owner::{self, testing};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Lists, Options, SearchOptions, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

// A host admits its members' records through an Automerge worker: this
// binary is that worker, as it is the session's own.
tilo_automerge_isolate::test_worker!();

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// The app whose private conversations these are.
const CHAT: &str = "so.tilo.chat";
/// Chat's record in the workspace: the route its app connection joins.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000cc";
/// The workspace the session holds, as the desk names it.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// The person's keys: this device, another of their devices, and the other
/// person the workspace certified.
#[derive(Clone)]
struct Keys {
    /// The public root of the person's identity, as the workspace names its
    /// owner.
    root: [u8; 32],
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    /// Another of the person's devices, holding this person's key as their
    /// phone does.
    phone: ed25519_dalek::SigningKey,
    phone_cert: DeviceCert,
    /// The other person of the workspace: the device of theirs that enrolled,
    /// and their certificate.
    peer: ed25519_dalek::SigningKey,
    peer_cert: DeviceCert,
}

impl Keys {
    /// The other person, as the workspace and the conversation name a person.
    fn peer_key(&self) -> String {
        person(self.peer_cert.user_public_key)
    }

    /// This person, as the messages they sent name them.
    fn root_key(&self) -> String {
        person(self.root)
    }
}

/// A person's public root, as their conversations name them: the owner's own
/// spelling of a person, which is what Chat lists and sends to.
fn person(root: [u8; 32]) -> String {
    blake3::Hash::from_bytes(root).to_hex().to_string()
}

/// A person, their identity provisioned where this test's session reads it,
/// and the workspace they host in its state with the other people in it.
struct Fixture {
    keys: Keys,
    /// The state directory the session keeps its workspaces under.
    state: PathBuf,
    // The identity's environment and files, restored and removed last: the
    // session and the apps read them while they run.
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
    let profile = tilo_identity_host::registry::identity_for_workspace(WORKSPACE)
        .expect("the identity registry")
        .expect("the workspace is answered by this device's person");
    let device = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    // The person's other device, enrolled as a pairing enrolls one.
    let now = tilo_clock::unix_now_ns();
    let fleet = DeviceFleetStore::from_env().expect("the device fleet");
    let token = fleet
        .mint_enrollment(
            &profile.id,
            tilo_user_identity::DeviceCapabilities::member(),
            now,
            now.saturating_add(3_600_000_000_000),
        )
        .expect("this device may enroll another of the person's");
    let phone = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    let phone_cert = fleet
        .complete_enrollment(
            &profile.id,
            &token,
            tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
            phone.verifying_key().to_bytes(),
            "The phone",
            now,
        )
        .expect("the second device is enrolled");
    // Someone else: their own root, and a device of theirs the workspace
    // certified.
    let peer_root = ed25519_dalek::SigningKey::from_bytes(&[0x71; 32]);
    let peer = ed25519_dalek::SigningKey::from_bytes(&[0x72; 32]);
    let peer_cert = testing::certificate(&peer_root, &peer, "Bo's phone");
    let keys = Keys {
        root: cert.user_public_key,
        device,
        cert,
        phone,
        phone_cert,
        peer,
        peer_cert,
    };
    hold_workspace(&state, &keys);
    Fixture {
        keys,
        state,
        _env: env,
        _root: root,
    }
}

/// The workspace's host admits `certificate`'s device as an editor, redeeming
/// the invitation it issues for it: what pairing another of the person's
/// devices does, and what the other person's device did to be here.
fn admit(
    governor: &mut testing::governor::TestGovernor,
    key: &ed25519_dalek::SigningKey,
    certificate: &DeviceCert,
) -> Result<(), String> {
    use tilo_peer_state::join;
    let now = std::time::SystemTime::now();
    let ns = testing::ns(now)?;
    let hint = governor.hint(iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&governor.workspace_host().device_public_key)
            .map_err(|error| error.to_string())?,
    ));
    let invitation = governor.issue(
        vec![hint],
        join::MemberRole::Editor,
        Some(certificate.user_public_key),
        Some(certificate.device_label.clone()),
        ns.saturating_add(60_000_000_000),
    );
    let mut request_id = [0_u8; 32];
    request_id[..8].copy_from_slice(&certificate.device_public_key[..8]);
    let answered = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id,
        }),
    );
    let join::JoinTransportResponse::Challenge(challenge) = answered else {
        return Err(format!("the host refuses the invitation: {answered:?}"));
    };
    let answered = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Complete(join::JoinCompleteRequest {
            proof: join::sign_join_proof(challenge.challenge, key)?,
        }),
    );
    if !matches!(answered, join::JoinTransportResponse::Complete(_)) {
        return Err(format!("the host refuses the proof: {answered:?}"));
    }
    Ok(())
}

/// The workspace's owner, as this device holds it in `state`, with the people
/// above admitted: what the session opens when it starts. The conversation
/// bodies stay in this file, and no workspace record holds them.
fn hold_workspace(state: &Path, keys: &Keys) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
        .expect("an owner for this device's workspace");
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut governor =
        testing::enroll_interactive_governor_in(&owner, &keys.device, &keys.cert, WORKSPACE)
            .expect("this device governs the workspace it hosts");
    admit(&mut governor, &keys.phone, &keys.phone_cert).expect("the person's other device joins");
    admit(&mut governor, &keys.peer, &keys.peer_cert).expect("the other person joins");
    // The governor's checkpoint now names both of them: the host admits it as
    // the publication a host's own workspace makes.
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
    let people = owner
        .direct_contacts(
            workspace,
            &CallerAuthority::Person {
                caller: keys.cert.clone(),
            },
            std::time::SystemTime::now(),
        )
        .expect("the owner lists the workspace's people");
    assert!(
        people
            .iter()
            .any(|contact| contact.public_key == keys.peer_key()),
        "the other person is a contact before the session starts: {people:?}"
    );
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
fn desk(keys: &Keys) -> DesktopTree {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(&keys.root));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// What an app presents to join the workspace's channel: the certificate, and
/// the device's proof that it is the certificate's.
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

/// The session serving the apps it starts on `socket`, holding the fixture's
/// workspace from its state and showing its desk.
fn session(socket: &Path, fixture: &Fixture) -> Session {
    // The apps speak from this test's own process, so the launches the
    // session would have recorded for them name that process.
    let launches = std::sync::Arc::new(Launches::default());
    launches.started(std::process::id(), CHAT, WORKSPACE, true);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            wake: LoopWake::default(),
            action_plane: ActionPlane::At(socket.to_path_buf()),
            person: "Ada".to_owned(),
            launches,
            state: Some(fixture.state.clone()),
            search: SearchOptions {
                lists: Lists::default(),
                ..SearchOptions::default()
            },
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(desk(&fixture.keys));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
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

/// The session's pass until the apps are done, as the session runs its loop:
/// what they answered, or `None` when they left without one (their own
/// thread's ending tells why).
fn until_the_apps_are_done<T>(
    session: &mut Session,
    received: &std::sync::mpsc::Receiver<T>,
) -> Option<T> {
    use std::sync::mpsc::TryRecvError;
    for _ in 0..4000 {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
        match received.try_recv() {
            Ok(done) => return Some(done),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
    }
    panic!("the apps never finished their calls");
}

/// A connection to the session's plane as Chat in the workspace, as the
/// launcher hands an app one.
fn connect(socket: &PathBuf) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
    launcher::services::write_collab_handoff(&mut stream, CHAT, WORKSPACE)
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

/// The channel key of Chat's record in the workspace.
fn channel() -> String {
    tilo_action_broker::route::CrdtRoute::record(CHAT, WORKSPACE, RECORD).channel_key()
}

/// Ask the session `command` as Chat, and read its answer.
async fn ask(
    handle: &runtime_collab::ChannelHandle,
    command: serde_json::Value,
) -> serde_json::Value {
    let bytes = serde_json::to_vec(&command).expect("a command is JSON");
    let reply = handle
        .person_chat(&bytes, &[])
        .await
        .expect("the session serves Chat's person chat");
    serde_json::from_slice(&reply).expect("the session answers JSON")
}

/// What Chat found, and what it was told while it waited.
struct Proof {
    contacts: serde_json::Value,
    opened: serde_json::Value,
    sent: serde_json::Value,
    refused: String,
    /// The conversation before the other device wrote.
    first_lane: serde_json::Value,
    /// What the conversation's lane gave next: its generation, and the
    /// conversation.
    arrived: (u64, serde_json::Value),
    read_again: serde_json::Value,
}

/// Chat's own calls: read the contacts, open the conversation with the other
/// person, send them a message, and wait on the conversation while another of
/// the person's devices writes in it.
fn chat_calls(socket: &PathBuf, keys: &Keys) -> Proof {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    let peer = keys.peer_key();
    runtime.block_on(async {
        let client = connect(socket);
        let mine = client
            .join_channel(&channel(), &presentation(&keys.device, &keys.cert))
            .await
            .expect("the app the session launched joins the workspace's channel");
        let contacts = ask(&mine, serde_json::json!({"operation": "contacts"})).await;
        let opened = ask(
            &mine,
            serde_json::json!({"operation": "open", "public_key": peer}),
        )
        .await;
        let sent = ask(
            &mine,
            serde_json::json!({
                "operation": "send",
                "public_key": peer,
                "text": "hello Bo",
                "invocation": uuid::Uuid::new_v4(),
            }),
        )
        .await;
        // A person this workspace never certified is not someone the owner
        // opens a conversation with.
        let refused = mine
            .person_chat(
                &serde_json::to_vec(&serde_json::json!({
                    "operation": "open",
                    "public_key": "ab".repeat(32),
                }))
                .expect("a command is JSON"),
                &[],
            )
            .await
            .err()
            .expect("the owner refuses a person of no certified device here")
            .to_string();
        // The conversation's own lane, held before the other device writes:
        // what arrives on it is what the app shows next, without asking.
        let lane = mine
            .subscribe_person_chat(&peer, &[])
            .await
            .expect("the conversation's change lane");
        let (_, body) = lane.read().await.expect("the conversation as it stands");
        let first_lane: serde_json::Value =
            serde_json::from_slice(&body).expect("the conversation is JSON");

        // Another of the person's devices, as the broker admits it, writes in
        // the same conversation.
        let phone = connect(socket);
        let theirs = phone
            .join_channel(&channel(), &presentation(&keys.phone, &keys.phone_cert))
            .await
            .expect("the person's other device joins the workspace's channel");
        ask(
            &theirs,
            serde_json::json!({
                "operation": "send",
                "public_key": peer,
                "text": "from the phone",
                "invocation": uuid::Uuid::new_v4(),
            }),
        )
        .await;

        // The lane wakes on the write: the app reads the new conversation
        // without ever asking whether it changed.
        let (generation, body) = lane.read().await.expect("the conversation as it changed");
        let live: serde_json::Value =
            serde_json::from_slice(&body).expect("the conversation is JSON");
        let read_again = ask(
            &mine,
            serde_json::json!({"operation": "read", "public_key": peer}),
        )
        .await;
        Proof {
            contacts,
            opened,
            sent,
            refused,
            first_lane,
            arrived: (generation, live),
            read_again,
        }
    })
}

#[test]
fn chat_lists_the_other_person_opens_sends_and_hears_a_new_message_live() {
    let home = private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture();
    let mut session = session(&socket, &fixture);

    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = fixture.keys.clone();
    let apps = std::thread::spawn(move || {
        let proof = chat_calls(&plane, &keys);
        let _ = done.send(proof);
    });
    let proof = until_the_apps_are_done(&mut session, &received);
    let ended = apps.join();
    let proof = match (proof, ended) {
        (Some(proof), Ok(())) => proof,
        (_, Err(panic)) => panic!("the app's calls failed: {panic:?}"),
        (None, Ok(())) => panic!("the app left without answering"),
    };

    let peer = &proof.opened["peer"];
    assert_eq!(
        peer,
        &serde_json::Value::String(fixture.keys.peer_key()),
        "the conversation is with the person Chat opened it with"
    );
    let contacts = proof.contacts.as_array().expect("a list of contacts");
    let contact = contacts
        .iter()
        .find(|contact| contact["public_key"] == *peer)
        .expect("the other person is in the workspace's contacts");
    assert_eq!(
        contact["conversation"], false,
        "no conversation with them held a message before Chat wrote one"
    );
    assert!(
        contacts
            .iter()
            .all(|contact| contact["public_key"] != fixture.keys.root_key()),
        "a person is not their own contact"
    );
    assert!(
        proof.opened["messages"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "a conversation opens empty: {}",
        proof.opened
    );
    let sent = proof.sent["messages"]
        .as_array()
        .expect("the conversation's messages");
    assert_eq!(sent.len(), 1, "the message the app sent is there");
    assert_eq!(sent[0]["author"], fixture.keys.root_key());
    assert_eq!(sent[0]["text"], "hello Bo");
    assert!(
        proof
            .refused
            .contains("not another current certified person"),
        "the session refuses a person the workspace never certified: {}",
        proof.refused
    );

    assert_eq!(
        proof.first_lane["messages"].as_array().map(Vec::len),
        Some(1),
        "the lane's first read is the conversation so far"
    );
    let (generation, arrived) = &proof.arrived;
    assert!(
        *generation > 1,
        "the lane reports the conversation's new state: {generation}"
    );
    let messages = arrived["messages"]
        .as_array()
        .expect("the conversation's messages");
    assert_eq!(
        messages.len(),
        2,
        "the other device's message arrived on the lane: {arrived}"
    );
    assert_eq!(messages[1]["text"], "from the phone");
    assert_eq!(messages[1]["author"], fixture.keys.root_key());
    assert_eq!(
        proof.read_again["messages"].as_array().map(Vec::len),
        Some(2),
        "a read after the lane woke holds the message too"
    );
}
