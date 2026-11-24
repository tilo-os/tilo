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

/// Ada's account.
const ADA: u32 = 1000;
/// An account nobody signed in.
const STRANGER: u32 = 1002;

const TWO_ACCOUNTS_TEST: &str = "device::services::second_person_tests::two_accounts_make_their_people_on_one_computer_and_each_certifies_as_itself";

/// One line of JSON in an MCP answer's text, when it is one.
fn answered_json(answer: &Value) -> Value {
    answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

/// `tilo_do` of `path` with `args`, asked on a connection of `uid`'s.
fn do_here(socket: &Path, uid: u32, path: &str, args: Value) -> Value {
    ask(
        socket,
        uid,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": path, "args": args},
        }}),
    )
}

/// Root signs `uid` in, saying nothing of a person it holds.
fn signed_in(socket: &Path, uid: u32) -> Value {
    ask(
        socket,
        tilo_api::daemon::admission::ROOT,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": "seats#signed-in", "args": {"uid": uid}},
        }}),
    )
}

fn who_here(socket: &Path, uid: u32) -> Value {
    ask(socket, uid, &json!({"native": "account#who"}))
}

/// Two accounts set up on one computer, each with no identity of its own: each makes its person
/// here through the one flow (`devices#found`), each certifies the key the daemon made it with
/// the root kept for that account alone, and each then acts as itself. A stranger reaches none
/// of it. One account's root signs nothing for the other.
#[test]
fn two_accounts_make_their_people_on_one_computer_and_each_certifies_as_itself() {
    let Some(root) = isolated_root(TWO_ACCOUNTS_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let device_keys = tilo_api::calls::device_keys();
    let device = DeviceOnly::new(
        tilo_device_identity::persist::load_device_key(&device_keys).unwrap(),
        tilo_device_identity::login::Place::here(device_keys).unwrap(),
    );
    let admission: &'static tilo_api::daemon::admission::Admission = Box::leak(Box::new(
        tilo_api::daemon::admission::Admission::new(DAEMON, true),
    ));
    let socket = root.join("native.sock");
    crate::daemon::serve_for_test(&daemon, device, test_peer, admission, &socket).unwrap();

    // Neither account was here before: signing in makes each a seat that may make its person.
    for uid in [ADA, BO] {
        let said = signed_in(&socket, uid);
        assert_ne!(said["result"]["isError"], true, "{said}");
        assert_eq!(
            answered_json(&said)["founding"],
            json!(true),
            "account {uid} that holds no identity is not told it may make one: {said}"
        );
        let before = who_here(&socket, uid);
        assert_eq!(before["founding"], json!(true), "{before}");
    }

    // Each makes its person through the daemon, the one way a person is made.
    let mut people = Vec::new();
    let mut acting_keys = Vec::new();
    for uid in [ADA, BO] {
        let made = do_here(&socket, uid, "devices#found", json!({}));
        assert_ne!(
            made["result"]["isError"], true,
            "account {uid} made no person: {made}"
        );
        let person = answered_json(&made)["person"]
            .as_str()
            .expect("a person is made")
            .to_owned();
        let who = who_here(&socket, uid);
        assert_eq!(who["founding"], Value::Null, "{who}");
        assert_eq!(
            who["certify"]["person"], person,
            "the daemon does not wait to certify the key it made account {uid}: {who}"
        );

        // Each certifies with the root the daemon keeps for its own account.
        let certified = do_here(&socket, uid, "seats#certified", json!({}));
        assert_ne!(
            certified["result"]["isError"], true,
            "account {uid} did not certify its key with its own root here: {certified}"
        );
        let after = who_here(&socket, uid);
        assert_eq!(after["person"], json!(person), "{after}");
        assert_eq!(after["holds_root"], json!(true), "{after}");
        let key = after["key"]
            .as_str()
            .expect("the account acts as a key")
            .to_owned();
        assert_ne!(key, hex::encode(daemon.cert.device_public_key), "{after}");
        people.push(person);
        acting_keys.push(key);
    }
    assert_ne!(people[0], people[1], "the two accounts are one person");
    assert_ne!(
        acting_keys[0], acting_keys[1],
        "the two accounts act as one key"
    );

    // A stranger reaches none of it: it may not make a person, and certifies no key.
    let refused = do_here(&socket, STRANGER, "devices#found", json!({}));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let refused = do_here(&socket, STRANGER, "seats#certified", json!({}));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let stranger = who_here(&socket, STRANGER);
    assert_eq!(stranger["person"], Value::Null, "{stranger}");
    assert_eq!(stranger["certify"], Value::Null, "{stranger}");
    assert_eq!(stranger["founding"], Value::Null, "{stranger}");

    // One account's root signs nothing for the other: Ada certifying the key Bert acts as is
    // refused, because this computer made Ada no such key.
    let bert_key = acting_keys[1].clone();
    let refused = ask(
        &socket,
        ADA,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": "seats#certified", "args": {"cert": bert_key}},
        }}),
    );
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
}
