//! What Chat still needs of the session for a direct conversation: which pair
//! records this person shares, the announcement that makes a pair's record
//! theirs, and the one import of the old private store before it is emptied.
//!
//! The conversation itself is the pair's record, which Chat reads and writes
//! as the app's own resource and which never passes this door
//! (`apps/chat/src/direct/acceptance.rs`). The people of this workspace are
//! the person whose device runs the session, another device of theirs, and
//! someone else the workspace certified; the app reaches the door as the
//! certificate the broker attested on its connection.

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
    fixture_with(false)
}

/// The fixture, with the old private store a conversation when
/// `old_conversation`: the history a person's pair record imports.
fn fixture_with(old_conversation: bool) -> Fixture {
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
    hold_workspace(&state, &keys, old_conversation);
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
fn hold_workspace(state: &Path, keys: &Keys, old_conversation: bool) {
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
    if !old_conversation {
        return;
    }
    // The conversation the old private store holds, before Chat moved to the
    // pair's record: what a person's history imports from.
    let now = std::time::SystemTime::now();
    let authority = CallerAuthority::Person {
        caller: keys.cert.clone(),
    };
    owner
        .open_direct_conversation(workspace, &keys.peer_key(), &authority, now)
        .expect("the old store opens the conversation");
    for text in ["before the records", "and one more"] {
        owner
            .send_direct_message(
                workspace,
                &keys.peer_key(),
                &authority,
                tilo_peer_state::owner::DirectMessageSend {
                    invocation: uuid::Uuid::new_v4(),
                    text: text.to_owned(),
                },
                now,
            )
            .expect("the old store keeps the message");
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

/// A person's root public key as a pair's people are spelt, and as the pair's
/// id is named with.
fn root_hex(root: [u8; 32]) -> String {
    hex::encode(root)
}

/// What the pair door answered: the pairs before and after the announcement,
/// the refusal of a person this workspace never certified, and the same
/// question asked by the person's other device.
struct PairProof {
    before: serde_json::Value,
    refused: String,
    confirmed: serde_json::Value,
    after: serde_json::Value,
    theirs: serde_json::Value,
}

/// Chat asks which pair records it is in, announces the pair with the other
/// person, and asks again: once from this device, once from the other
/// person's.
fn announce_calls(socket: &PathBuf, keys: &Keys) -> PairProof {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    let peer = root_hex(keys.peer_cert.user_public_key);
    runtime.block_on(async {
        let client = connect(socket);
        let mine = client
            .join_channel(&channel(), &presentation(&keys.device, &keys.cert))
            .await
            .expect("the app the session launched joins the workspace's channel");
        let before = ask(&mine, serde_json::json!({"operation": "pairs"})).await;
        let refused = mine
            .person_chat(
                &serde_json::to_vec(&serde_json::json!({
                    "operation": "announce",
                    "public_key": "ab".repeat(32),
                }))
                .expect("a command is JSON"),
                &[],
            )
            .await
            .expect_err("the workspace never certified that person")
            .to_string();
        let confirmed = ask(
            &mine,
            serde_json::json!({"operation": "announce", "public_key": peer}),
        )
        .await;
        let after = ask(&mine, serde_json::json!({"operation": "pairs"})).await;
        // The person's other device sees the same pair: the proof is the
        // workspace's, not the announcing device's in memory.
        let theirs = connect(socket);
        let theirs = theirs
            .join_channel(&channel(), &presentation(&keys.phone, &keys.phone_cert))
            .await
            .expect("the person's other device joins the workspace's channel");
        let theirs = ask(&theirs, serde_json::json!({"operation": "pairs"})).await;
        PairProof {
            before,
            refused,
            confirmed,
            after,
            theirs,
        }
    })
}

#[test]
fn chat_announces_its_pair_and_the_workspace_proves_it() {
    let home = private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture();
    let mut session = session(&socket, &fixture);

    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = fixture.keys.clone();
    let apps = std::thread::spawn(move || {
        let _ = done.send(announce_calls(&plane, &keys));
    });
    let proof = until_the_apps_are_done(&mut session, &received);
    let ended = apps.join();
    let proof = match (proof, ended) {
        (Some(proof), Ok(())) => proof,
        (_, Err(panic)) => panic!("the app's calls failed: {panic:?}"),
        (None, Ok(())) => panic!("the app left without answering"),
    };

    assert_eq!(
        proof.before.as_array().map(Vec::len),
        Some(0),
        "no pair is proved before a device opens the conversation: {}",
        proof.before
    );
    assert!(
        proof
            .refused
            .contains("a conversation is opened with a person of this workspace"),
        "the workspace refuses a pair with a person it never certified: {}",
        proof.refused
    );
    assert_eq!(
        proof.confirmed,
        serde_json::json!({}),
        "announcing answers the pair's record may now be written"
    );
    let pair = collab_types::direct_pair_id(
        WORKSPACE,
        &root_hex(fixture.keys.root),
        &root_hex(fixture.keys.peer_cert.user_public_key),
    )
    .expect("the workspace names a pair of two people");
    let after = proof.after.as_array().expect("a list of pairs");
    assert_eq!(after.len(), 1, "the workspace proves one pair: {after:?}");
    assert_eq!(after[0]["pair"], pair);
    let mut people: Vec<&str> = after[0]["people"]
        .as_array()
        .expect("a pair's two people")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    people.sort_unstable();
    let mut expected = vec![
        root_hex(fixture.keys.root),
        root_hex(fixture.keys.peer_cert.user_public_key),
    ];
    expected.sort_unstable();
    assert_eq!(people, expected, "the pair is the two people of it");
    assert_eq!(
        proof.theirs, proof.after,
        "the person's other device reads the same pair record"
    );
}

/// What the import door answered: the conversation the old store holds, what
/// asking again past its end answers, and what is left after the migration
/// empties it.
struct ImportProof {
    announced: serde_json::Value,
    first: serde_json::Value,
    ended: serde_json::Value,
    kept: serde_json::Value,
    cleared: serde_json::Value,
    /// The old store after the clear: nothing more to import.
    emptied: serde_json::Value,
}

fn import_calls(socket: &PathBuf, keys: &Keys) -> ImportProof {
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
        // The pair's record is the two people's before the history moves into
        // it: importing writes there, so the workspace proves the pair first.
        let announced = ask(
            &mine,
            serde_json::json!({
                "operation": "announce",
                "public_key": root_hex(keys.peer_cert.user_public_key),
            }),
        )
        .await;
        let first = ask(
            &mine,
            serde_json::json!({
                "operation": "import",
                "public_key": peer,
                "after_sequence": 0,
            }),
        )
        .await;
        let ended = ask(
            &mine,
            serde_json::json!({
                "operation": "import",
                "public_key": peer,
                "after_sequence": first["next_sequence"],
            }),
        )
        .await;
        // Importing only reads: the old store still holds what it did.
        let kept = ask(
            &mine,
            serde_json::json!({
                "operation": "import",
                "public_key": peer,
                "after_sequence": 0,
            }),
        )
        .await;
        // The history is in the pair's record: the migration empties the old
        // store in one write, and nothing is left to import.
        let cleared = ask(&mine, serde_json::json!({"operation": "clear"})).await;
        let emptied = ask(
            &mine,
            serde_json::json!({
                "operation": "import",
                "public_key": peer,
                "after_sequence": 0,
            }),
        )
        .await;
        ImportProof {
            announced,
            first,
            ended,
            kept,
            cleared,
            emptied,
        }
    })
}

#[test]
fn chat_imports_the_conversation_the_old_store_holds() {
    let home = private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture_with(true);
    let mut session = session(&socket, &fixture);

    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = fixture.keys.clone();
    let apps = std::thread::spawn(move || {
        let _ = done.send(import_calls(&plane, &keys));
    });
    let proof = until_the_apps_are_done(&mut session, &received);
    let ended = apps.join();
    let proof = match (proof, ended) {
        (Some(proof), Ok(())) => proof,
        (_, Err(panic)) => panic!("the app's calls failed: {panic:?}"),
        (None, Ok(())) => panic!("the app left without answering"),
    };

    assert_eq!(
        proof.announced,
        serde_json::json!({}),
        "the pair the history moves into is proved first"
    );
    assert_eq!(proof.first["peer"], fixture.keys.peer_key());
    let messages = proof.first["messages"]
        .as_array()
        .expect("the old conversation's messages");
    assert_eq!(
        messages.len(),
        2,
        "the old store's conversation imports whole: {}",
        proof.first
    );
    assert_eq!(messages[0]["text"], "before the records");
    assert_eq!(messages[1]["text"], "and one more");
    assert_eq!(proof.first["has_more"], false);
    assert_eq!(proof.first["next_sequence"], 2);
    assert_eq!(
        proof.ended["messages"].as_array().map(Vec::len),
        Some(0),
        "past its end the old conversation answers nothing: {}",
        proof.ended
    );
    assert_eq!(
        proof.kept["messages"].as_array().map(Vec::len),
        Some(2),
        "importing never moves or repeats what the old store holds: {}",
        proof.kept
    );
    assert_eq!(
        proof.cleared,
        serde_json::json!({}),
        "the migration empties the old store once"
    );
    assert_eq!(
        proof.emptied["messages"].as_array().map(Vec::len),
        Some(0),
        "the emptied store has nothing more to import: {}",
        proof.emptied
    );
    assert_eq!(proof.emptied["has_more"], false);
}

/// What the app heard about the pairs it is in: the updates it read from the
/// channel's context, and whether one repeated, without the app ever asking
/// the owner for the list.
struct PairsWakeProof {
    /// Every pair list the context named, in the order it named them.
    named: Vec<Vec<String>>,
    /// Whether the context named the same list again after the change.
    repeated: bool,
}

/// Chat subscribes to the workspace's context and announces its pair: the
/// list of pairs it is in arrives on the subscription, and nothing asks for
/// it again.
fn pairs_wake_calls(socket: &PathBuf, keys: &Keys) -> PairsWakeProof {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    let pair = collab_types::direct_pair_id(
        WORKSPACE,
        &root_hex(keys.root),
        &root_hex(keys.peer_cert.user_public_key),
    )
    .expect("the workspace names a pair of two people");
    runtime.block_on(async move {
        let client = connect(socket);
        let mine = client
            .join_channel(&channel(), &presentation(&keys.device, &keys.cert))
            .await
            .expect("the app the session launched joins the workspace's channel");
        // The app follows the channel's context: who is here, and which pair
        // records this person's windows may open.
        let context = mine
            .subscribe_context()
            .await
            .expect("the channel's context");
        ask(
            &mine,
            serde_json::json!({
                "operation": "announce",
                "public_key": root_hex(keys.peer_cert.user_public_key),
            }),
        )
        .await;
        let mut named = Vec::new();
        loop {
            let next = context.read().await.expect("the context as it changed");
            let Some(pairs) = next.person_pairs else {
                continue;
            };
            let done = pairs.contains(&pair);
            named.push(pairs);
            if done {
                break;
            }
        }
        // Nothing repeats it: a change is told once, so a window builds its
        // list from what arrives and never from a clock.
        let repeated = tokio::time::timeout(std::time::Duration::from_millis(20), context.read())
            .await
            .is_ok_and(|next| {
                next.expect("the context as it changed")
                    .person_pairs
                    .is_some()
            });
        PairsWakeProof { named, repeated }
    })
}

#[test]
fn chat_learns_its_pair_from_the_workspaces_governance_change() {
    let home = private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture();
    let mut session = session(&socket, &fixture);

    let (done, received) = std::sync::mpsc::channel();
    let plane = socket;
    let keys = fixture.keys.clone();
    let apps = std::thread::spawn(move || {
        let _ = done.send(pairs_wake_calls(&plane, &keys));
    });
    let proof = until_the_apps_are_done(&mut session, &received);
    let ended = apps.join();
    let proof = match (proof, ended) {
        (Some(proof), Ok(())) => proof,
        (_, Err(panic)) => panic!("the app's calls failed: {panic:?}"),
        (None, Ok(())) => panic!("the app left without answering"),
    };

    let pair = collab_types::direct_pair_id(
        WORKSPACE,
        &root_hex(fixture.keys.root),
        &root_hex(fixture.keys.peer_cert.user_public_key),
    )
    .expect("the workspace names a pair of two people");
    assert_eq!(
        proof.named.last().map(Vec::len),
        Some(1),
        "the change names exactly the pair the governance admitted: {:?}",
        proof.named
    );
    assert_eq!(
        proof.named.last().map(|pairs| pairs[0].clone()),
        Some(pair.clone()),
        "the pair is the one the workspace proves: {:?}",
        proof.named
    );
    assert!(
        !proof.repeated,
        "the pair is told once, not on a clock: {:?}",
        proof.named
    );
}
