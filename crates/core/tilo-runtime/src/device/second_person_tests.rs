//! A second person on one computer, end to end: root signs their account in as the person it
//! holds, their session certifies the key the daemon made them with their own identity, and their
//! calls on the daemon's native endpoint read and write the computer's workspace as that key.

use super::*;
use crate::device::services::tests::isolated_root;
use crate::device::unclaimed::DeviceOnly;
use serde_json::{Value, json};
use std::io::{BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use tilo_actions::remote::{CatalogueQuery, ContentPuts as _};

/// The account the daemon runs as: the system's.
const DAEMON: u32 = 990;
/// Bo's account.
const BO: u32 = 1001;

/// The account a test's next connection comes from, as the daemon reads it.
static PEER: AtomicU32 = AtomicU32::new(DAEMON);

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the kernel's answer, which can fail"
)]
fn test_peer(_: &UnixStream) -> io::Result<u32> {
    Ok(PEER.load(Ordering::SeqCst))
}

/// One request on a connection of `uid`'s, and its answer.
fn ask(socket: &Path, uid: u32, request: &Value) -> Value {
    PEER.store(uid, Ordering::SeqCst);
    let stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    writeln!(&stream, "{request}").unwrap();
    let mut line = String::new();
    io::BufReader::new(stream)
        .read_line(&mut line)
        .expect("the daemon answers");
    serde_json::from_str(&line).unwrap()
}

/// An action frame client on a connection of `uid`'s.
fn line_client(socket: &Path, uid: u32) -> tilo_actions::remote::LineClient {
    PEER.store(uid, Ordering::SeqCst);
    let stream = UnixStream::connect(socket).unwrap();
    tilo_actions::remote::LineClient::of_stream(stream).unwrap()
}

const SECOND_PERSON_TEST: &str = "device::services::second_person_tests::a_second_person_signs_in_certifies_their_key_and_works_in_the_workspace_as_it";

/// Ada's computer, whose workspace Bo is a member of. Bo signs in on it with a real identity of
/// their own: root signs their account in as the person it holds, their session certifies the
/// key the daemon made them, and from then on their account lists the workspace's actions and
/// puts content into it as that key. Naming Bo reached nothing: before they certified the key,
/// they reached neither.
#[test]
fn a_second_person_signs_in_certifies_their_key_and_works_in_the_workspace_as_it() {
    let Some(root) = isolated_root(SECOND_PERSON_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());

    // Bo's own identity, kept in their account's identity directory.
    let bo_dir = root.join("bo").join("identity");
    std::fs::create_dir_all(&bo_dir).unwrap();
    tilo_identity::provisioning::ensure_collaboration_identity(
        &bo_dir,
        &tilo_device_identity::IdentityPaths::under(root.join("bo").join("keys")),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let bo = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(&bo_dir).unwrap(),
    )
    .unwrap();
    let person = bo.group_public_key();
    assert_ne!(person, daemon.cert.user_public_key, "Bo is another person");

    // Ada makes Bo an editor of the computer's workspace.
    let (workspaces, workspace, cert) = (
        Arc::clone(&daemon.workspaces),
        daemon.workspace.clone(),
        daemon.cert.clone(),
    );
    block_on_owned(async move {
        workspaces
            .copies()
            .add_member(
                &workspace,
                &cert,
                tilo_workspace_service::workspaces::AddMemberRequest {
                    request_id: [7; 32],
                    user_public_key: person,
                    place: tilo_workspace_service::workspaces::MemberPlace::Role(
                        tilo_action_policy::Role::Editor,
                    ),
                    name: Some("Bo".into()),
                },
            )
            .await
            .map(drop)
    })
    .unwrap();

    // The daemon's native endpoint, under the system unit.
    let keys = tilo_api::calls::device_keys();
    let device = DeviceOnly::new(
        tilo_device_identity::persist::load_device_key(&keys).unwrap(),
        tilo_device_identity::login::Place::here(keys).unwrap(),
    );
    let admission: &'static tilo_api::daemon::admission::Admission = Box::leak(Box::new(
        tilo_api::daemon::admission::Admission::new(DAEMON, true),
    ));
    let socket = root.join("native.sock");
    crate::daemon::serve_for_test(&daemon, device, test_peer, admission, &socket).unwrap();
    let anyone = Caller::local_system();
    let puts = |uid: u32| {
        line_client(&socket, uid)
            .content_puts()
            .map(|puts| puts.put_many(&anyone, &[format!("from {uid}").into_bytes()]))
    };

    // Root signs Bo in as pam does, naming the person their account holds.
    let signed_in = ask(
        &socket,
        tilo_api::daemon::admission::ROOT,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": "seats#signed-in", "args": {
                "person": hex::encode(person),
                "uid": BO,
            }},
        }}),
    );
    assert_ne!(signed_in["result"]["isError"], true, "{signed_in}");

    // Until their session certified the key, Bo reaches none of the workspace.
    let who = ask(&socket, BO, &json!({"native": "account#who"}));
    assert_eq!(who["person"], Value::Null, "{who}");
    assert_eq!(who["certify"]["person"], hex::encode(person), "{who}");
    assert!(
        !matches!(puts(BO), Some(Ok(_))),
        "an uncertified key put content into the workspace"
    );

    // Bo's session certifies it with their identity.
    PEER.store(BO, Ordering::SeqCst);
    assert!(
        tilo_api::daemon::who::certify_key_at(&socket, &bo).unwrap(),
        "there was a key to certify"
    );
    let who = ask(&socket, BO, &json!({"native": "account#who"}));
    assert_eq!(who["person"], hex::encode(person), "{who}");
    let key = who["key"].as_str().expect("Bo acts as a key").to_owned();
    assert_ne!(key, hex::encode(daemon.cert.device_public_key));

    // Bo reads the workspace's actions and puts content into it, as their key.
    let listed = line_client(&socket, BO)
        .query(&anyone, CatalogueQuery::default())
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the catalogue was answered")
        .expect("Bo's key reads the workspace's catalogue");
    assert!(!listed.providers.is_empty(), "Bo lists no provider");
    let put = puts(BO)
        .expect("Bo's key reaches the workspace's content")
        .expect("Bo's key puts content into the workspace");
    let have = line_client(&socket, BO)
        .content_puts()
        .expect("Bo's key reaches the workspace's content")
        .have(&anyone, &put)
        .unwrap();
    assert_eq!(have, [true], "what Bo put is in the workspace");

    // Bo leaves: their key reaches the workspace no more.
    let left = ask(
        &socket,
        tilo_api::daemon::admission::ROOT,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do", "arguments": {"path": "seats#left", "args": {"uid": BO}},
        }}),
    );
    assert_ne!(left["result"]["isError"], true, "{left}");
    assert!(
        !matches!(puts(BO), Some(Ok(_))),
        "a key put content after its account left"
    );
}
