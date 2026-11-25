//! A computer whose identity is missing or half-made: the daemon keeps answering its device's
//! own actions, and founding completes the person rather than the daemon ending.

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::sync::Arc;
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
    // Where a login of this person's device said it enters, kept beside the certificate: the
    // daemon starts again from it, and asks as the person's own account hear it.
    let server = iroh::SecretKey::from_bytes(&[0x3c; 32]);
    tilo_device_identity::login::file_entry(
        &place,
        &tilo_device_identity::login::Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key),
            workspace_name: Some("Ada's".into()),
            agent: String::new(),
            cert: cert.clone(),
            server_cert: cert.clone(),
            server_address: Some(tilo_device_identity::login::ServerAddress {
                endpoint_addr: iroh::EndpointAddr::new(server.public()),
                relay_url: None,
                web: None,
            }),
        },
    )
    .unwrap();
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

    let made = ask(&socket, &do_request("devices#found", &json!({})));
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
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");
    // The person's part serves the device a moment after their services run: the daemon's own
    // status names it from then on, and `account#who` answers where it enters.
    wait_for_the_person_serving(&socket);
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
    assert_eq!(
        who["home"]["workspace"],
        tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key),
        "the daemon's own account is told where it enters, from the entry the login left: {who}"
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
        &do_request("devices#found", &json!({"name": "Ada"})),
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
        .wait_for_person(Duration::from_secs(120))
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

const PLAIN_CONNECTION_TEST: &str =
    "device::founding_tests::a_plain_connection_to_a_founding_daemon_prints_a_device_ticket";

/// The same computer, founded while its daemon serves, reached the way `tilo` and the SDKs reach
/// it: a plain connection rather than the tools above. It is the person the daemon serves, so
/// `tilo device add` has its ticket printed for it rather than refused for an agent the
/// workspace keeps none of.
#[test]
fn a_plain_connection_to_a_founding_daemon_prints_a_device_ticket() {
    let Some(profile) = isolated_root(PLAIN_CONNECTION_TEST) else {
        return;
    };
    let environment = isolated_environment();
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let _ = std::fs::remove_file(&keys.device_privkey);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let made = ask(
        &socket,
        &do_request("devices#found", &json!({"name": "Ada"})),
    );
    assert_ne!(
        made["result"]["isError"], true,
        "no person was made: {made}"
    );
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");

    let began = std::time::Instant::now();
    let printed = tilo_rt::app().handle().block_on(async {
        let calls = tilo_api::calls::Calls::connect(Some(socket.as_path()), None).await?;
        // The list first, on the same connection: a plain connection acts where the account
        // enters, named by no workspace.
        calls.founded_workspace().await?;
        calls
            .start(
                None,
                tilo_device_identity::login::RESOURCE,
                tilo_device_identity::login::ISSUE,
                &json!({"wait": false}),
                "Add a device",
            )
            .await
    });
    let printed = printed.unwrap_or_else(|error| {
        panic!(
            "the person's own connection printed no ticket after {:?}: {error:#}",
            began.elapsed()
        )
    });
    assert!(
        printed["result"]["ticket"]
            .as_str()
            .is_some_and(|ticket| !ticket.is_empty()),
        "the daemon answered with no ticket: {printed}"
    );
    running.stop().unwrap();
    drop(environment);
}

const FOLLOWED_FOUNDING_TEST: &str =
    "device::founding_tests::a_founding_call_asked_for_tickets_is_followed_after_its_person_serves";

/// The call `tilo login --new-identity --ticket server` makes: founding with tickets, followed
/// by its id once the person's services run. The person's services answer where the call is,
/// though their catalogue never publishes founding.
#[test]
fn a_founding_call_asked_for_tickets_is_followed_after_its_person_serves() {
    let Some(profile) = isolated_root(FOLLOWED_FOUNDING_TEST) else {
        return;
    };
    let environment = isolated_environment();
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let _ = std::fs::remove_file(&keys.device_privkey);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    // What the founder asks for. The call is the one a typed caller makes: it names its own
    // invocation, so it can be followed where it is however far it got.
    let asked = json!({ "name": "Ada", "tickets": ["server"] });
    let invocation = tilo_actions::InvocationId::new().0.to_string();
    // The answer may come at once or, as the founder reads it, say the call stands under way
    // (its tickets are issued once the person's services run): either is a call to follow.
    let made = ask(
        &socket,
        &followed_request("devices#found", &asked, Some(&invocation), 0),
    );
    assert!(
        made.get("error").is_none(),
        "the daemon did not answer: {made}"
    );

    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");
    // The follow, as the founder makes it: the same call by its id, now that the person's
    // services answer this device instead of its own actions.
    let followed = ask(
        &socket,
        &followed_request("devices#found", &asked, Some(&invocation), 60_000),
    );
    assert_ne!(
        followed["result"]["isError"], true,
        "the founding call was not followed: {followed}"
    );
    let followed = &followed["result"]["structuredContent"];
    assert_eq!(
        followed["status"], "ok",
        "the founding call did not end: {followed}"
    );
    let tickets = followed["result"]["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("the founding call answered no tickets: {followed}"));
    assert!(
        tickets.iter().any(|ticket| {
            ticket["kind"] == "server"
                && ticket["ticket"]
                    .as_str()
                    .is_some_and(|ticket| !ticket.is_empty())
        }),
        "the founding call answered no server ticket: {followed}"
    );
    // The device the person was founded on lists itself among their devices: a server handed
    // their workspace admits every change it makes by that record, and this device keeps it
    // whether it holds their workspace itself or hands it to the server.
    let person = followed["result"]["person"]
        .as_str()
        .and_then(|person| hex::decode(person).ok())
        .and_then(|person| <[u8; 32]>::try_from(person).ok())
        .expect("the founding names the person");
    let own = tilo_device_identity::persist::load_device_key(&keys)
        .expect("the founder's device key")
        .verifying_key()
        .to_bytes();
    let daemon = running
        .services(Duration::from_secs(120))
        .expect("the person's services");
    let copies = daemon.workspaces().clone();
    tilo_rt::app().handle().block_on(tilo_test_env::within(
        "the founding device to list itself in the person's devices",
        async move {
            loop {
                let listed = tilo_workspace_service::workspaces::person_workspace::devices::of(
                    &copies, &person,
                )
                .await
                .listed(&person)
                .iter()
                .any(|entry| entry.cert.device_public_key == own);
                if listed {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        },
    ));
    running.stop().unwrap();
    drop(environment);
}

const SLOW_FOUNDING_TEST: &str =
    "device::founding_tests::a_founding_call_follows_a_person_part_that_starts_past_one_answer";

/// A founding call that asks for tickets is answered at once, and followed by the stages the
/// daemon reports: its person's part may start more than the ten seconds one answer is waited
/// for, and the call is still answered the tickets it asked for.
#[test]
fn a_founding_call_follows_a_person_part_that_starts_past_one_answer() {
    let Some(profile) = isolated_root(SLOW_FOUNDING_TEST) else {
        return;
    };
    let mut environment = isolated_environment();
    // The person's part starts after more than the ten seconds one answer waits for.
    environment.set("TILO_TEST_HOLD_BETWEEN_PARTS_MS", "11000");
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let _ = std::fs::remove_file(&keys.device_privkey);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let place = tilo_api::daemon::DaemonPlace::at(
        socket.parent().expect("the native socket's directory"),
        &profile,
    );
    let founding = tilo_api::daemon::Founding {
        name: Some("Ada".to_owned()),
        tickets: vec![tilo_api::daemon::TicketKind::Server],
    };
    let began = std::time::Instant::now();
    let answer = place
        .found(&founding)
        .expect("the founding call is answered");
    let took = began.elapsed();
    assert!(
        took > Duration::from_secs(10),
        "the person's part was not slowed past one answer: {took:?}"
    );
    let answer = answer.expect("a person is founded where this computer has none");
    let tickets = answer["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("the founding call answered no tickets: {answer}"));
    assert!(
        tickets.iter().any(|ticket| {
            ticket["kind"] == "server"
                && ticket["ticket"]
                    .as_str()
                    .is_some_and(|ticket| !ticket.is_empty())
        }),
        "the founding call answered no server ticket: {answer}"
    );
    running.stop().unwrap();
    drop(environment);
}

const FOLLOW_ACROSS_START_TEST: &str =
    "device::founding_tests::a_foundings_follow_is_answered_across_its_persons_part_starting";

/// The follow `tilo login --new-identity` keeps making is answered before its person's part
/// starts and after: the stages are read where the device that took the call keeps them, so the
/// part's own admission never holds the founder's deadline.
#[test]
fn a_foundings_follow_is_answered_across_its_persons_part_starting() {
    let Some(profile) = isolated_root(FOLLOW_ACROSS_START_TEST) else {
        return;
    };
    let mut environment = isolated_environment();
    // The person's part starts a few seconds after founding: a follow is in flight as it does.
    environment.set("TILO_TEST_HOLD_BETWEEN_PARTS_MS", "4000");
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let _ = std::fs::remove_file(&keys.device_privkey);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let actions = tilo_device_identity::login::daemon::DeviceActions::at(socket);
    // The call `tilo login --new-identity --ticket server` makes, answered with it under way.
    let mut call = json!({
        "path": "devices#found",
        "args": {"name": "Ada", "tickets": ["server"]},
        "wait_ms": 0,
    });
    let (answered, _, _) = actions
        .tool("tilo_do", &call)
        .expect("the founding call is answered");
    assert_eq!(answered["status"], "pending", "{answered}");
    let invocation = answered["invocation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the founding call names nothing to follow: {answered}"))
        .to_owned();

    // A follow while the person's part has not started: the stages the daemon reports.
    call["invocation_id"] = json!(invocation);
    call["wait_ms"] = json!(5_000);
    let (followed, _, _) = actions
        .tool("tilo_do", &call)
        .expect("a follow before the person's part starts is answered");
    assert_eq!(followed["status"], "pending", "{followed}");
    assert!(
        followed["waiting_for"].as_str().is_some(),
        "the follow reports the stage the call is in: {followed}"
    );

    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's part starts");

    // A follow after the person's part starts: answered at once, never held by the part's own
    // admission of the session that would answer it.
    call["wait_ms"] = json!(60_000);
    let began = std::time::Instant::now();
    let (followed, _, failed) = actions
        .tool("tilo_do", &call)
        .expect("a follow after the person's part started is answered");
    assert!(!failed, "the follow was refused: {followed}");
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "the follow waited on the person's part: {:?}, {followed}",
        began.elapsed()
    );

    // And the call ends with the tickets the founding asked for.
    let mut answer = followed;
    for _ in 0..20 {
        if answer["status"] != "pending" {
            break;
        }
        let (next, _, _) = actions
            .tool("tilo_do", &call)
            .expect("the follow is answered");
        answer = next;
    }
    assert_eq!(answer["status"], "ok", "{answer}");
    let tickets = answer["result"]["tickets"]
        .as_array()
        .unwrap_or_else(|| panic!("the founding call answered no tickets: {answer}"));
    assert!(
        tickets.iter().any(|ticket| {
            ticket["kind"] == "server"
                && ticket["ticket"]
                    .as_str()
                    .is_some_and(|ticket| !ticket.is_empty())
        }),
        "the founding call answered no server ticket: {answer}"
    );
    running.stop().unwrap();
    drop(environment);
}

/// A daemon that reports a stage and then stops answering is told as one, naming the stage it
/// reached: a caller waits for each report, never for ever, and never fails without saying
/// where the call was.
#[test]
fn a_founding_call_that_stops_reporting_names_its_last_stage() {
    let dir = tempfile::tempdir().unwrap();
    let place = tilo_api::daemon::DaemonPlace::at(dir.path(), dir.path());
    let socket = place.native();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let invocation = tilo_actions::InvocationId::new().0.to_string();
    let pending = |stage: Option<&str>| {
        let mut typed = json!({"status": "pending", "invocation_id": invocation});
        if let Some(stage) = stage {
            typed["waiting_for"] = json!(stage);
        }
        json!({"jsonrpc": "2.0", "id": 1, "result": {
            "content": [{"type": "text", "text": ""}],
            "structuredContent": typed,
        }})
        .to_string()
    };
    let taken = pending(None);
    let staged = pending(Some("starting the person part"));
    let server = std::thread::spawn(move || {
        // The request, then the stage it reached, then none: a daemon that stopped reporting.
        for answer in [Some(taken), Some(staged), None] {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(answer) = answer {
                writeln!(reader.get_mut(), "{answer}").unwrap();
            } else {
                // The connection stays open, as a daemon that stopped answering keeps it, until
                // the caller gives up and closes it.
                let mut after = String::new();
                let _ = reader.read_line(&mut after);
            }
        }
    });
    let founding = tilo_api::daemon::Founding {
        name: None,
        tickets: vec![tilo_api::daemon::TicketKind::Device],
    };
    let began = std::time::Instant::now();
    let refused = place
        .found(&founding)
        .expect_err("a daemon that stops reporting fails");
    assert!(
        began.elapsed() >= Duration::from_secs(9),
        "the call failed before the daemon's answer was waited for: {refused}"
    );
    assert!(
        refused.contains("starting the person part"),
        "the failure names no stage the daemon reached: {refused}"
    );
    server.join().unwrap();
}

const TWO_STORES_TEST: &str =
    "device::founding_tests::a_second_accounts_store_naming_the_same_person_is_never_picked";

/// The id an account's public record of `person` is kept under.
fn public_record_id(person: [u8; 32]) -> String {
    format!(
        "identity-{}",
        hex::encode(person).chars().take(55).collect::<String>()
    )
}

/// Keep an account's public record of `person` under `id` in its store, and make it the one
/// the account presents.
fn write_public_record(store: &Path, id: &str, person: [u8; 32]) {
    let record = tilo_identity::store::public_member_record("Ada", [88, 101, 242], person).unwrap();
    tilo_files::native::atomic::write_with_mode(&store.join(format!("{id}.json")), &record, 0o600)
        .unwrap();
    tilo_identity::store::set_active_identity(store, id).unwrap();
}

/// Two identity stores on this computer name the same person. The one the profile records as the
/// account the device was set up from is the one the device starts as; the other is never taken
/// for it, and a profile that records no such account, or whose account names someone else, is
/// refused with what to run rather than guessed.
#[test]
fn a_second_accounts_store_naming_the_same_person_is_never_picked() {
    let Some(profile) = isolated_root(TWO_STORES_TEST) else {
        return;
    };
    let environment = isolated_environment();
    let keys = tilo_api::calls::device_keys();
    // The isolated child starts with a person of its own; this test sets its own stores.
    reset_identity(&keys);
    let store = |uid: u32| {
        tilo_identity::store::identity_dir_from_env()
            .unwrap()
            .join("accounts")
            .join(uid.to_string())
    };
    // The account the device was set up from: its root made the person and certified the device.
    let founding = 7_001;
    let founding_store = store(founding);
    std::fs::create_dir_all(&founding_store).unwrap();
    let person_key = tilo_identity::provisioning::create_person(
        &founding_store,
        &keys,
        "Ada",
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    // A second account enrolled for the same person: its store is a public record naming them,
    // as an account that joined from another device keeps.
    let second = 7_002;
    let second_store = store(second);
    tilo_identity::provisioning::prepare_identity_directory(&second_store).unwrap();
    let record_id = public_record_id(person_key);
    write_public_record(&second_store, &record_id, person_key);
    let names = |dir: &Path| {
        tilo_identity::store::resolve_active_identity(dir)
            .and_then(|record| tilo_identity::store::load_metadata_from_path(&record).ok())
            .is_some_and(|metadata| metadata.group_public_key == person_key)
    };
    assert!(names(&founding_store), "the founding account names them");
    assert!(
        names(&second_store),
        "the second account names them too, so only the record tells them apart"
    );

    // The profile records the founding account as the account the device was set up from.
    let record = tilo_api::daemon::admission::person_record(&profile, founding);
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(
        &record,
        json!({"person": hex::encode(person_key), "device": true}).to_string(),
    )
    .unwrap();

    // The daemon's own account names nobody: the recorded account is the one it starts as, and
    // never the second account whose store names the same person.
    let own = 7_003;
    let picked = crate::device::services::device_identity_dir(&profile, own, &keys).unwrap();
    assert_eq!(picked, founding_store, "the recorded account is the one");
    assert_ne!(picked, second_store, "the second account is never picked");

    // The daemon's own account is the first answer when its store names the person.
    assert_eq!(
        crate::device::services::device_identity_dir(&profile, founding, &keys).unwrap(),
        founding_store
    );

    // A profile that records no account is refused rather than guessed.
    std::fs::remove_file(&record).unwrap();
    let error = crate::device::services::device_identity_dir(&profile, own, &keys).unwrap_err();
    assert!(
        error.to_string().contains("--new-identity"),
        "the refusal says what to run: {error}"
    );

    // A recorded account whose store names another person is refused too.
    let other = 7_004;
    let other_store = store(other);
    tilo_identity::provisioning::prepare_identity_directory(&other_store).unwrap();
    let other_person = [9u8; 32];
    let id = public_record_id(other_person);
    write_public_record(&other_store, &id, other_person);
    let other_record = tilo_api::daemon::admission::person_record(&profile, other);
    std::fs::create_dir_all(other_record.parent().unwrap()).unwrap();
    std::fs::write(
        &other_record,
        json!({"person": hex::encode(other_person), "device": true}).to_string(),
    )
    .unwrap();
    let error = crate::device::services::device_identity_dir(&profile, own, &keys).unwrap_err();
    assert!(
        error.to_string().contains("another person"),
        "the refusal says what is wrong: {error}"
    );

    // A recorded account whose identity record is not on the computer is refused with that.
    std::fs::remove_dir_all(&other_store).unwrap();
    let error = crate::device::services::device_identity_dir(&profile, own, &keys).unwrap_err();
    assert!(
        error.to_string().contains("no identity record"),
        "the refusal says what is missing: {error}"
    );
    drop(environment);
}

const GAINS_A_SERVER_TEST: &str =
    "device::founding_tests::a_person_gaining_a_server_is_served_through_it_at_once";

/// How long the whole of [`GAINS_A_SERVER_TEST`] may take: past it the test fails and its
/// process ends, whatever step it is in, rather than hanging the run.
const GAINS_A_SERVER_BOUND: Duration = Duration::from_secs(240);

/// A person founded on this computer adds their own server: once it keeps their own workspace,
/// the person's part ends and starts again through it, in the same process. The part that ended
/// is gone (its services, links and stores let go), the one that starts shows the same folders,
/// and an agent's call is answered by it rather than refused as not started. The daemon started
/// afresh enters through the server too: the hand-over finished before the part ended, so the
/// workspace still names it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one person's first server, from founding to a call answered after the restart, read top to bottom"
)]
fn a_person_gaining_a_server_is_served_through_it_at_once() {
    let Some(profile) = isolated_root(GAINS_A_SERVER_TEST) else {
        return;
    };
    // A step that hangs fails the test and ends its process: a wedged daemon's stop on unwinding
    // would hang too.
    std::thread::spawn(|| {
        std::thread::sleep(GAINS_A_SERVER_BOUND);
        eprintln!(
            "the test did not finish in {}s: the person's part did not start again through \
             their server",
            GAINS_A_SERVER_BOUND.as_secs()
        );
        // SAFETY: `_exit` ends this process at once, as the test's bound means: no destructor
        // of the step that hung runs, and 101 is the status a failed test ends with.
        unsafe { libc::_exit(101) };
    });
    let environment = isolated_environment();
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let made = ask(
        &socket,
        &do_request("devices#found", &json!({"name": "Ada"})),
    );
    assert_ne!(
        made["result"]["isError"], true,
        "no person was made: {made}"
    );
    // The person's part serves the device, which enters no server yet: it was founded here.
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");
    let before = ask(
        &socket,
        &json!({"jsonrpc": "2.0", "id": 1, "method": tilo_api::daemon::PEER_STATUS}),
    );
    let mounted: Vec<Value> = before["result"]["mounted"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    // The part that serves now, by what it holds: none of it may outlive the restart.
    let first = Arc::downgrade(&running.services(Duration::from_secs(120)).unwrap());

    // The person's server: a device their root certifies, keeping workspaces on this machine.
    let home = person_server();
    let line = home.line.clone();

    // An agent's `tilo mcp` connected before the move, and stays connected.
    let (connected, init) = agent_initialized(&socket);
    assert_eq!(
        init["result"]["serverInfo"]["name"], "tilo-runtime",
        "the person's part answers the agent: {init}"
    );

    // The person logs this computer in at it: their own workspace names it from now on.
    // The device's own login action, on the frame wire every client reaches it with.
    let logged_in = device_call(
        &socket,
        tilo_device_identity::login::LOG_IN,
        json!({ "server": line }),
    );
    // It is taken up at once, and runs on as the person's part ends and starts again through
    // the server: what it did shows below, as the part that starts serves the person.
    assert!(
        matches!(
            logged_in,
            tilo_actions::remote::ReceiptStatus::Accepted
                | tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person's computer logs in at their server: {logged_in:?}"
    );

    // The person's part starts again, as a part of its own.
    let started = std::time::Instant::now();
    let again = loop {
        if let Ok(services) = running.services(Duration::from_secs(1))
            && !std::ptr::eq(Arc::as_ptr(&services), first.as_ptr())
        {
            break Arc::downgrade(&services);
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the person's part did not start again through their server"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    // The agent connected to the part that ended is told it ended: its connection closes, and
    // its `tilo mcp` connects again, to the part that started.
    let mut rest = String::new();
    let read = std::io::BufReader::new(&connected).read_line(&mut rest);
    assert!(
        matches!(read, Ok(0)),
        "the connection of the part that ended is still served: {read:?} {rest}"
    );
    drop(connected);
    // The part that ended let go of its services: nothing of it answers or reaches out as this
    // device any more. The endpoint they ran on outlives it, taken by the part that started.
    let started = std::time::Instant::now();
    while first.strong_count() > 0 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the part that ended is still held ({} times)",
            first.strong_count(),
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // It serves the device through the server, with every folder it showed before shown again.
    let mut status = wait_for_the_person_serving(&socket);
    let started = std::time::Instant::now();
    let shown = |status: &Value| {
        let now = status["result"]["mounted"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        mounted.iter().all(|folder| now.contains(folder))
    };
    while !shown(&status) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the folders were not shown again: {status}, before {mounted:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
        status = wait_for_the_person_serving(&socket);
    }
    let who = ask(&socket, &json!({"native": "account#who"}));
    let named = tilo_user_identity::canonical::canonical_bytes(&home.cert)
        .map(hex::encode)
        .unwrap();
    assert_eq!(
        who["home"]["server_cert"], named,
        "the person's part enters through their server: {who}"
    );

    // An agent's call is answered by the part that started, not refused as not started.
    let (agent, init) = agent_initialized(&socket);
    assert_eq!(
        init["result"]["serverInfo"]["name"], "tilo-runtime",
        "the person's part answers the agent: {init}"
    );
    drop(agent);
    assert!(again.strong_count() > 0, "the part that started still runs");
    running.stop().unwrap();

    // The daemon started afresh, the person's own workspace naming their server from its
    // start: the person's part serves through it, and an agent's call is answered.
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run after a fresh start");
    wait_for_the_person_serving(&socket);
    let who = ask(&socket, &json!({"native": "account#who"}));
    assert_eq!(
        who["home"]["server_cert"], named,
        "a fresh start enters through the person's server: {who}"
    );
    let (agent, init) = agent_initialized(&socket);
    assert_eq!(
        init["result"]["serverInfo"]["name"], "tilo-runtime",
        "the person's part started afresh answers the agent: {init}"
    );
    drop(agent);
    running.stop().unwrap();
    let PersonServer { server, serving, .. } = home;
    serving.block_on(server.shutdown());
    drop(environment);
}

/// The person's server: a device their root certifies, keeping workspaces on this machine, and
/// the line another device reaches it at.
struct PersonServer {
    line: String,
    cert: tilo_user_identity::DeviceCert,
    server: tilo_server::daemon::Server,
    serving: tokio::runtime::Runtime,
    /// Where the server keeps what it is given, for as long as the test runs.
    _state: tempfile::TempDir,
}

/// A server of the person this computer founded, running in this process.
fn person_server() -> PersonServer {
    let dir = tilo_identity::store::account_identity_dir_from_env().unwrap();
    let root = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(&dir).expect("the founded person's root"),
    )
    .unwrap();
    let server_key = SigningKey::from_bytes(&[0x6e; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.group_public_key(),
        device_public_key: server_key.verifying_key().to_bytes(),
        device_label: "home-server".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    let serving = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let server = serving
        .block_on(tilo_server::daemon::Server::start_with_relay_mode(
            tilo_server::ServerConfig {
                state_root: state.path().join("server"),
                ..Default::default()
            },
            tilo_server::daemon::ServerIdentity {
                user_public_key: root.group_public_key(),
                device_signing_key: server_key,
                device_cert: cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        ))
        .unwrap();
    let line = serde_json::to_string(&tilo_server_protocol::web::PageHost {
        server_cert: cert.clone(),
        address: Some(tilo_server_protocol::web::PageHostAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .unwrap();
    PersonServer {
        line,
        cert,
        server,
        serving,
        _state: state,
    }
}

const KEEPS_THE_PORT_TEST: &str =
    "device::founding_tests::a_part_that_starts_again_takes_the_port_the_part_that_ended_bound";

/// How long this test keeps the closing work of the part that ended busy: past every bound the
/// ending waits on and past the restart, as a folder that will not unmount does.
const BUSY_CLEANUP: Duration = Duration::from_secs(120);

/// How long the person's part may take to start again through their server: the bound the test
/// already had before the port was named.
const START_AGAIN_WITHIN: Duration = Duration::from_secs(90);

/// The person's part ends and starts again through their server in the same process, while the
/// work closing behind it is still busy. The endpoint outlives the part that served on it: the
/// part that starts takes the port the part that ended served on, rather than binding it again
/// after that cleanup let go of it.
#[test]
fn a_part_that_starts_again_takes_the_port_the_part_that_ended_bound() {
    let Some(profile) = isolated_root(KEEPS_THE_PORT_TEST) else {
        return;
    };
    let port = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut environment = isolated_environment();
    environment
        .set(crate::device::services::PORT_ENV, port.to_string())
        .set(
            "TILO_TEST_BUSY_CLEANUP_MS",
            BUSY_CLEANUP.as_millis().to_string(),
        );
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let made = ask(
        &socket,
        &do_request("devices#found", &json!({"name": "Ada"})),
    );
    assert_ne!(
        made["result"]["isError"], true,
        "no person was made: {made}"
    );
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");
    // The part that serves is the one reached at the port this computer names.
    let (first, first_endpoint) = {
        let services = running.services(Duration::from_secs(120)).unwrap();
        let endpoint = services.serving().endpoint();
        let sockets = endpoint.bound_sockets();
        assert!(
            sockets.iter().any(|at| at.port() == port),
            "the part that serves bound {sockets:?}, not the port {port} this computer names"
        );
        (Arc::downgrade(&services), endpoint)
    };

    // The person logs this computer in at their server: their own workspace names it from now
    // on, and the part starts again through it.
    let home = person_server();
    let logged_in = device_call(
        &socket,
        tilo_device_identity::login::LOG_IN,
        json!({ "server": home.line }),
    );
    assert!(
        matches!(
            logged_in,
            tilo_actions::remote::ReceiptStatus::Accepted
                | tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person's computer logs in at their server: {logged_in:?}"
    );

    // The part that starts takes the endpoint the part that ended served on, while what that
    // part is still closing is busy: the port is never bound again.
    let started = std::time::Instant::now();
    let again = loop {
        if let Ok(services) = running.services(Duration::from_secs(1))
            && !std::ptr::eq(Arc::as_ptr(&services), first.as_ptr())
        {
            break services;
        }
        assert!(
            started.elapsed() < START_AGAIN_WITHIN,
            "the person's part did not start again while the part that ended was still closing"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let sockets = again.serving().endpoint().bound_sockets();
    assert!(
        sockets.iter().any(|at| at.port() == port),
        "the part that started bound {sockets:?}, not the port {port} the part that ended bound"
    );
    // The endpoint outlived the part that served on it, and the part that started took it
    // rather than binding the port again: the work closing behind the part that ended held
    // nothing of what the part that starts is reached at.
    assert!(
        !first_endpoint.is_closed(),
        "the endpoint the part that ended served on was closed rather than handed on"
    );
    assert!(
        first_endpoint
            .bound_sockets()
            .iter()
            .any(|at| at.port() == port),
        "the part that served was reached at another port than the one this computer names"
    );
    wait_for_the_person_serving(&socket);
    running.stop().unwrap();
    let PersonServer { server, serving, .. } = home;
    serving.block_on(server.shutdown());
    drop(environment);
}

/// The device's own `action` with `args`, called over the frame wire every client reaches it
/// with, and the receipt's status.
fn device_call(socket: &Path, action: &str, args: Value) -> tilo_actions::remote::ReceiptStatus {
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
    };
    let Value::Object(argument) = args else {
        panic!("a device action is called with its arguments");
    };
    let request = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_device_identity::login::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_device_identity::login::RESOURCE,
                action,
            ),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(55_000),
        appearance: None,
        signed: None,
    });
    let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    writer
        .write_all(&RemoteActionFrame::new(request).encode().unwrap())
        .unwrap();
    writer.write_all(b"\n").unwrap();
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .expect("the daemon answers");
    match RemoteActionFrame::decode(line.trim_end().as_bytes())
        .expect("the answer is a frame")
        .message
    {
        RemoteActionMessage::Receipt(receipt) => receipt.status,
        other => panic!("a call is answered with a receipt, not {other:?}"),
    }
}

/// An agent's `tilo mcp` connection to the person's part, as the native endpoint hands it over,
/// and the answer to its `initialize`.
fn agent_initialized(socket: &Path) -> (std::os::unix::net::UnixStream, Value) {
    let agent = std::os::unix::net::UnixStream::connect(socket).unwrap();
    agent
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    writeln!(&agent, "{}", tilo_api::daemon::AGENT_LINE).unwrap();
    writeln!(
        &agent,
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "Claude Code", "version": "1"}}})
    )
    .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(&agent)
        .read_line(&mut line)
        .expect("the agent's call is answered");
    (agent, serde_json::from_str(&line).unwrap())
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

/// Wait until the daemon's own status names the device its person's part serves: from that
/// moment `account#who` answers where this computer enters its person's workspaces.
fn wait_for_the_person_serving(socket: &Path) -> Value {
    let started = std::time::Instant::now();
    loop {
        let status = ask(
            socket,
            &json!({"jsonrpc": "2.0", "id": 1, "method": tilo_api::daemon::PEER_STATUS}),
        );
        if status["result"]["device"] != Value::Null {
            return status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the person's part did not take this device up: {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
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
fn do_request(path: &str, args: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "tilo_do",
        "arguments": {"path": path, "args": args},
    }})
}

/// [`do_request`] as `tilo login --new-identity` makes it: the answer typed, the call waited on
/// `wait_ms`, and the call `following` named asked again where it is.
fn followed_request(path: &str, args: &Value, following: Option<&str>, wait_ms: u64) -> Value {
    let mut arguments = json!({"path": path, "args": args, "wait_ms": wait_ms});
    if let Some(invocation) = following {
        arguments["invocation_id"] = json!(invocation);
    }
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "tilo_do",
        "arguments": arguments,
        "_meta": {"so.tilo.mcp": {"structured": true}},
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
