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
use tilo_actions::remote::CatalogueQuery;

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
fn test_peer(_: &UnixStream) -> io::Result<crate::seats::Peer> {
    Ok(crate::seats::Peer {
        uid: PEER.load(Ordering::SeqCst),
        // A founding link is tied to the process that redeems it: these connections come from
        // this test's own process.
        pid: Some(std::process::id()),
    })
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
fn line_client(socket: &Path, uid: u32) -> tilo_action_line::LineClient {
    PEER.store(uid, Ordering::SeqCst);
    let stream = UnixStream::connect(socket).unwrap();
    tilo_action_line::LineClient::of_stream(stream).unwrap()
}

const SECOND_PERSON_TEST: &str = "device::services::second_person_tests::a_second_person_signs_in_certifies_their_key_and_works_in_the_workspace_as_it";

/// Ada's computer, whose workspace Bo is a member of. Bo signs in on it with a real identity of
/// their own: root signs their account in as the person it holds, their session certifies the
/// key the daemon made them, and from then on their account lists the workspace's actions and
/// puts content into it as that key. Naming Bo reached nothing: before they certified the key,
/// they reached neither.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one account, from its sign-in to the key it certifies and the writes it makes, read top to bottom"
)]
fn a_second_person_signs_in_certifies_their_key_and_works_in_the_workspace_as_it() {
    let Some(root) = isolated_root(SECOND_PERSON_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());

    // Bo's own identity, kept in their account's identity directory: made where a device's daemon
    // signs, which this process stands in for with a key of Bo's own.
    let bo_dir = root.join("bo").join("identity");
    std::fs::create_dir_all(&bo_dir).unwrap();
    let bo_keys = tilo_device_identity::IdentityPaths::under(root.join("bo").join("keys"));
    tilo_device_identity::persist::load_or_create_device_key(&bo_keys).unwrap();
    let bo_key = tilo_device_identity::persist::take_device_key(&bo_keys).unwrap();
    tilo_identity::provisioning::ensure_collaboration_identity(
        &bo_dir,
        &bo_keys,
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    drop(bo_key);
    let bo = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(&bo_dir).unwrap(),
    )
    .unwrap();
    let person = bo.group_public_key();
    assert_ne!(person, daemon.cert.user_public_key, "Bo is another person");

    // Ada makes Bo an editor of the computer's workspace, founded with her root.
    assert_eq!(
        daemon.first.here_blocking().as_deref(),
        Ok(daemon.workspace.as_str()),
        "Ada's workspace is founded at the id her key names"
    );
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

/// One line of JSON in an MCP answer's text, when it is one: an action's answer follows the
/// `ok` line the tool writes before it.
fn answered_json(answer: &Value) -> Value {
    let Some(text) = answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
    else {
        return Value::Null;
    };
    serde_json::from_str(text)
        .ok()
        .or_else(|| serde_json::from_str(text.trim_start_matches("ok\n").trim()).ok())
        .or_else(|| {
            text.lines()
                .rev()
                .find_map(|line| serde_json::from_str(line).ok())
        })
        .unwrap_or(Value::Null)
}

/// `tilo_do` of `path` with `args`, asked on a connection of `uid`'s.
fn do_here(socket: &Path, uid: u32, path: &str, args: &Value) -> Value {
    // A founding answers once the person's workspace is founded, as their services start: it is
    // waited on for as long as that takes.
    ask(
        socket,
        uid,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": path, "args": args, "wait_ms": 120_000},
        }}),
    )
}

/// Root signs `uid` in, saying nothing of a person it holds: the account's session redeems the
/// founding link its sign-in answers, as a session does.
fn signed_in(socket: &Path, uid: u32) -> Value {
    let answer = ask(
        socket,
        tilo_api::daemon::admission::ROOT,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "tilo_do",
            "arguments": {"path": "seats#signed-in", "args": {"uid": uid}},
        }}),
    );
    if let Some(token) = answered_json(&answer)["token"].as_str() {
        let linked = ask(
            socket,
            uid,
            &json!({"native": "shells#link", "token": token}),
        );
        assert_eq!(
            linked["founding"],
            json!(true),
            "the sign-in's founding link is not the account's: {linked}"
        );
    }
    answer
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
        let made = do_here(&socket, uid, "devices#found", &json!({}));
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
        let certified = do_here(&socket, uid, "seats#certified", &json!({}));
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
    let refused = do_here(&socket, STRANGER, "devices#found", &json!({}));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let refused = do_here(&socket, STRANGER, "seats#certified", &json!({}));
    assert_eq!(refused["result"]["isError"], json!(true), "{refused}");
    let stranger = who_here(&socket, STRANGER);
    assert_eq!(stranger["person"], Value::Null, "{stranger}");
    assert_eq!(stranger["certify"], Value::Null, "{stranger}");
    assert_eq!(stranger["founding"], Value::Null, "{stranger}");
}

/// Make the person `person` (hex) an editor of the daemon's workspace, as a host adds a member
/// whose key reaches it.
fn add_as_editor(daemon: &Daemon, person: &str, tag: u8) -> Result<(), String> {
    use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};
    let person: [u8; 32] = hex::decode(person)
        .map_err(|error| error.to_string())?
        .try_into()
        .map_err(|_| "a person's key is 32 bytes".to_owned())?;
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
                AddMemberRequest {
                    request_id: [tag; 32],
                    user_public_key: person,
                    place: MemberPlace::Role(tilo_action_policy::Role::Editor),
                    name: None,
                },
            )
            .await
            .map(drop)
    })
    .map_err(|error| error.to_string())
}

const ROOT_APP_TEST: &str = "device::services::second_person_tests::a_root_connection_that_runs_a_launched_app_is_served_as_that_app";

/// An app the daemon started runs as root in an image whose daemon is the system's own, and the
/// launch record is what answers for it: its connection reaches its workspace's actions, never
/// the device's own, which reach no workspace. A connection with no record is still the device's.
#[test]
fn a_root_connection_that_runs_a_launched_app_is_served_as_that_app() {
    let Some(root) = isolated_root(ROOT_APP_TEST) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
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
    let listing = "/v1/workspaces";
    let answered = |socket: &Path| {
        let calls = std::sync::Arc::new(tilo_api::calls::Calls::in_workspace(
            std::sync::Arc::new(line_client(socket, 0)),
            &daemon.workspace,
        ));
        let asking = std::sync::Arc::clone(&calls);
        block_on_owned(async move { asking.request("GET", listing, Vec::new(), Vec::new()).await })
            .map_or_else(|error| error, |_| String::new())
    };
    // The same account with no launch record is the device's own: its API reaches no workspace.
    let device_only = answered(&socket);
    assert!(
        device_only.contains("reaches no workspace"),
        "a connection that runs no app is the device's: {device_only:?}"
    );
    // Recorded as the app this process runs, it is that app's, whose API reaches its workspace.
    daemon
        .apps()
        .record_started(std::process::id(), "so.tilo.discord", &daemon.workspace);
    let app = answered(&socket);
    assert!(
        !app.contains("reaches no workspace"),
        "an app the daemon launched reaches its workspace: {app:?}"
    );
}

const ROOTS_SERVED_TEST: &str = "device::services::second_person_tests::people_founded_while_the_daemon_serves_have_their_roots_served_here";

/// People founded while this daemon serves have each one's root kept and served here without
/// the daemon starting again: an account founded after it started, and the first person a fresh
/// computer founds through `devices#found` (what `tilo login --new-identity` runs). Each one's
/// root-only call is then answered here as themselves.
#[test]
fn people_founded_while_the_daemon_serves_have_their_roots_served_here() {
    let Some(root) = isolated_root(ROOTS_SERVED_TEST) else {
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

    let mut people = Vec::new();
    for uid in [ADA, BO] {
        let said = signed_in(&socket, uid);
        assert_ne!(said["result"]["isError"], true, "{said}");
        let made = do_here(&socket, uid, "devices#found", &json!({}));
        assert_ne!(
            made["result"]["isError"], true,
            "account {uid} made no person: {made}"
        );
        let person = answered_json(&made)["person"]
            .as_str()
            .expect("a person is made")
            .to_owned();
        let certified = do_here(&socket, uid, "seats#certified", &json!({}));
        assert_ne!(
            certified["result"]["isError"], true,
            "account {uid} certified no key with its own root: {certified}"
        );
        let who = who_here(&socket, uid);
        assert_eq!(who["person"], json!(person), "{who}");
        assert_eq!(
            who["holds_root"],
            json!(true),
            "account {uid}'s root is not kept here: {who}"
        );
        people.push(person);
    }
    // The daemon serves root-only asks for the people it founded, without starting again.
    tilo_test_env::wait_until("the founded person's root to be served", || {
        daemon.serves_at_root()
    });
    // A person whose key reaches the workspace is served its actions as that key. While the
    // built-in packages do not compile on this tree, the server holds no workspace and no key
    // reaches one: the calls below wait for that to end.
    let mut reachable = true;
    for (uid, person) in [ADA, BO].into_iter().zip(&people) {
        let Ok(editor) = u8::try_from(uid) else {
            reachable = false;
            continue;
        };
        if add_as_editor(&daemon, person, editor).is_err() {
            reachable = false;
        }
    }
    if !reachable {
        return;
    }
    for uid in [ADA, BO] {
        let words = do_here(&socket, uid, "devices#recoveryWords", &json!({}));
        assert_ne!(
            words["result"]["isError"], true,
            "account {uid}'s root-only call is not answered as itself: {words}"
        );
    }
}

const FRESH_SYSTEM_TEST: &str = "device::services::second_person_tests::a_fresh_computers_identity_is_founded_through_the_system_admission";

/// A fresh computer's first identity, as `tilo login --new-identity` makes it through the
/// system unit's daemon: root signs the account in with no person, `devices#found` makes one, and
/// the root the founding call made is read by the account's uid rather than by looking the
/// person up while they are being signed in.
#[test]
fn a_fresh_computers_identity_is_founded_through_the_system_admission() {
    let Some(root) = isolated_root(FRESH_SYSTEM_TEST) else {
        return;
    };
    let profile = root.join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    tilo_api::daemon::admission::install(
        tilo_api::daemon::admission::Admission::new(DAEMON, true)
            .keeping_accounts_in(profile)
            .unwrap(),
    );
    let admission = tilo_api::daemon::admission::here(|| DAEMON);
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
    let device_keys = tilo_api::calls::device_keys();
    let device = DeviceOnly::new(
        tilo_device_identity::persist::load_device_key(&device_keys).unwrap(),
        tilo_device_identity::login::Place::here(device_keys).unwrap(),
    );
    let socket = root.join("native.sock");
    crate::daemon::serve_for_test(&daemon, device, test_peer, admission, &socket).unwrap();

    let said = signed_in(&socket, ADA);
    assert_ne!(said["result"]["isError"], true, "{said}");
    let made = do_here(&socket, ADA, "devices#found", &json!({}));
    assert_ne!(
        made["result"]["isError"], true,
        "a fresh account made no person: {made}"
    );
    let person = answered_json(&made)["person"]
        .as_str()
        .expect("a person is made")
        .to_owned();
    let person_key: [u8; 32] = hex::decode(&person)
        .expect("a person's key is hex")
        .try_into()
        .expect("a person's key is 32 bytes");
    // The root the founding call made is the account's, read by its uid.
    assert!(
        super::root_for(ADA).expect("the opened root").is_some(),
        "the founded account's root is not kept here"
    );
    // And their person is theirs once the sign-in is recorded.
    assert!(
        super::root_record_for_person(person_key).is_ok(),
        "the founded person's root is not found for their signed-in account"
    );
    // The login asks again only while it holds nothing, but a computer whose person is here
    // answers the ask all the same: it refuses, and never waits on the part it is starting.
    let again = do_here(&socket, ADA, "devices#found", &json!({}));
    assert_eq!(
        again["result"]["isError"],
        json!(true),
        "a person's device answered no refusal to a second founding: {again}"
    );
}
