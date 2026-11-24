//! A computer whose identity is missing or half-made: the daemon keeps answering its device's
//! own actions, and founding completes the person rather than the daemon ending.

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use crate::device::services::tests::isolated_root;

/// The person a half-made computer's certificate names.
const PERSON: [u8; 32] = [0x5a; 32];

const HALF_MADE_TEST: &str =
    "device::founding_tests::a_half_made_computer_stays_up_and_founds_its_person";

/// A certificate for this computer's device key was filed, and the account's identity record
/// never was: what a login or founding stopped partway leaves. The daemon answers at once, its
/// own actions among them, and takes the certificate's person up when founding is asked, so its
/// person's services run rather than it ending.
#[test]
fn a_half_made_computer_stays_up_and_founds_its_person() {
    let Some(profile) = isolated_root(HALF_MADE_TEST) else {
        return;
    };
    let environment = isolated_environment();
    let keys = tilo_api::calls::device_keys();
    // The isolated child starts with a person of its own; this test is about a computer whose
    // identity is not there, so it starts from none.
    reset_identity(&keys);
    let (key, _) = tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
    let person = SigningKey::from_bytes(&PERSON);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Half made".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).unwrap();
    let place = tilo_device_identity::login::Place::here(keys.clone()).unwrap();
    tilo_device_identity::login::file_certificate(&place, &cert).unwrap();
    assert!(
        tilo_identity::trusted_device::serving(&keys).is_err(),
        "an account with no identity record starts nothing"
    );

    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    // The daemon answers while it has no identity to serve: the device's own actions, and who
    // the account is, certificate and all.
    let fresh = ask(&socket, &json!({"native": "account#who"}));
    assert_eq!(fresh["claimed"], false, "{fresh}");
    assert_eq!(
        fresh["person"],
        hex::encode(person.verifying_key().to_bytes()),
        "the certificate still names a person: {fresh}"
    );

    let made = ask(&socket, &do_request("devices#found", json!({})));
    assert_ne!(
        made["result"]["isError"], true,
        "a half-made computer made no person: {made}"
    );
    assert_eq!(
        answered_json(&made)["person"],
        hex::encode(person.verifying_key().to_bytes()),
        "the person the certificate names is taken up, not a second person made over them"
    );

    // Its person's services run from then on, as the person the certificate names.
    running
        .wait_for_person(Duration::from_secs(30))
        .expect("the person's services run");
    let who = ask(&socket, &json!({"native": "account#who"}));
    assert_eq!(who["claimed"], true, "{who}");
    assert_eq!(
        who["person"],
        hex::encode(person.verifying_key().to_bytes()),
        "{who}"
    );
    assert_eq!(
        who["holds_root"], false,
        "the root is another device's: {who}"
    );

    // The account's identity is the person's, in the store its services read.
    let dir = tilo_identity::store::account_identity_dir_from_env().unwrap();
    let record =
        tilo_identity::store::resolve_active_identity(&dir).expect("the account's identity record");
    let metadata = tilo_identity::store::load_metadata_from_path(&record).unwrap();
    assert_eq!(metadata.group_public_key, cert.user_public_key);
    assert!(!metadata.single_device, "this computer keeps no root");
    running.stop().unwrap();
    drop(environment);
}

const EMPTY_TEST: &str =
    "device::founding_tests::a_computer_with_no_identity_at_all_stays_up_and_founds_its_person";

/// A computer nobody set up: no certificate and no identity. Its daemon answers its own actions,
/// `devices#found` makes the person, and its person's services run rather than it ending.
#[test]
fn a_computer_with_no_identity_at_all_stays_up_and_founds_its_person() {
    let Some(profile) = isolated_root(EMPTY_TEST) else {
        return;
    };
    let environment = isolated_environment();
    // The isolated child starts with a person of its own; this test is about a computer nobody
    // set up, so it starts from none at all.
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let _ = std::fs::remove_file(&keys.device_privkey);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let fresh = ask(&socket, &json!({"native": "account#who"}));
    assert_eq!(fresh["claimed"], false, "{fresh}");
    assert_eq!(fresh["person"], Value::Null, "{fresh}");

    let made = ask(
        &socket,
        &do_request("devices#found", json!({"name": "Ada"})),
    );
    assert_ne!(
        made["result"]["isError"], true,
        "no person was made: {made}"
    );
    let person = answered_json(&made)["person"]
        .as_str()
        .expect("a person is made")
        .to_owned();

    running
        .wait_for_person(Duration::from_secs(30))
        .expect("the person's services run");
    let who = ask(&socket, &json!({"native": "account#who"}));
    assert_eq!(who["claimed"], true, "{who}");
    assert_eq!(who["person"], person, "{who}");
    assert_eq!(
        who["holds_root"], true,
        "the person's root is kept here: {who}"
    );
    running.stop().unwrap();
    drop(environment);
}

/// A process of its own whose home and identity are the isolated child's, with nothing else
/// handed to the daemon at its start.
fn isolated_environment() -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_RUNTIME_PROFILE")
        .remove("CREDENTIALS_DIRECTORY");
    env
}

/// Remove the identity the isolated child was set up with: the account's identity record and
/// the certificates this device holds. The device key stays, as a computer's does.
fn reset_identity(keys: &tilo_device_identity::IdentityPaths) {
    if let Ok(dir) = tilo_identity::store::account_identity_dir_from_env() {
        let _ = std::fs::remove_dir_all(dir);
    }
    for path in [&keys.user_pubkey, &keys.device_cert] {
        let _ = std::fs::remove_file(path);
    }
    if let Some(certs) = keys.device_cert_for_user(&[0; 32]).parent() {
        let _ = std::fs::remove_dir_all(certs);
    }
}

/// One request on the daemon's native endpoint, and its answer.
fn ask(socket: &Path, request: &Value) -> Value {
    let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    writeln!(&stream, "{request}").unwrap();
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .expect("the daemon answers");
    serde_json::from_str(&line).unwrap()
}

/// `tilo_do` of `path` with `args`, as the daemon's native endpoint answers it.
fn do_request(path: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "tilo_do",
        "arguments": {"path": path, "args": args},
    }})
}

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
