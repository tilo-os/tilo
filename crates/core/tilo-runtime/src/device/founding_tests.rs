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
    // Filed as the login files it, in the process holding the key; the daemon takes it after.
    crate::device::services::tests::as_the_daemon(|| {
        tilo_device_identity::login::file_certificate(&place, &cert).unwrap();
    });
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

const SLOW_PLAIN_FOUNDING_TEST: &str = "device::founding_tests::a_founding_with_no_tickets_follows_a_person_part_that_starts_past_one_answer";

/// A founding that asks for no tickets, as a session that signed in with no person makes: its
/// person's part starts more than the ten seconds one answer is waited for, and the follow that
/// comes then reads the same call, answered with the person once they serve. It is never asked
/// again as a founding from the person the call just made, which a computer refuses.
#[test]
fn a_founding_with_no_tickets_follows_a_person_part_that_starts_past_one_answer() {
    let Some(profile) = isolated_root(SLOW_PLAIN_FOUNDING_TEST) else {
        return;
    };
    let mut environment = isolated_environment();
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
    let began = std::time::Instant::now();
    let answer = place
        .found(&tilo_api::daemon::Founding::default())
        .expect("the founding call is answered");
    let took = began.elapsed();
    let answer = answer.expect("the founding call answered as if this computer served a person");
    assert!(
        answer["person"]
            .as_str()
            .is_some_and(|person| person.len() == 64),
        "the founding call answered no person: {answer}"
    );
    assert!(
        took > Duration::from_secs(10),
        "the founding was answered before its person's part served: {took:?}"
    );
    running.stop().unwrap();
    drop(environment);
}

const FOLLOW_ACROSS_START_TEST: &str =
    "device::founding_tests::a_foundings_follow_is_answered_across_its_persons_part_starting";

/// A founding is answered as soon as the person exists, before their part of the daemon starts:
/// a call made right after it that only the person's part answers (listing their workspace)
/// waits for that part and is answered once it runs, never refused for having come early nor
/// told the action is unknown. The founding's own call still ends with the tickets it asked for.
#[test]
fn a_foundings_follow_is_answered_across_its_persons_part_starting() {
    let Some(profile) = isolated_root(FOLLOW_ACROSS_START_TEST) else {
        return;
    };
    let mut environment = isolated_environment();
    // The person's part starts a few seconds after founding: the next call is in flight as it
    // does.
    environment.set("TILO_TEST_HOLD_BETWEEN_PARTS_MS", "4000");
    let keys = tilo_api::calls::device_keys();
    reset_identity(&keys);
    let _ = std::fs::remove_file(&keys.device_privkey);
    let running = crate::daemon::Running::start(&profile).unwrap();
    let socket = running.native_socket().to_owned();
    let actions = tilo_device_identity::login::daemon::DeviceActions::at(socket);
    // The call `tilo login --new-identity --ticket server` makes.
    let mut call = json!({
        "path": "devices#found",
        "args": {"name": "Ada", "tickets": ["server"]},
        "wait_ms": 0,
    });
    // Answered at once: done, or under way while the person's part starts.
    let (answered, _, _) = actions
        .tool("tilo_do", &call)
        .expect("the founding call is answered");
    assert!(
        answered["error"].is_null()
            && ["ok", "pending"].contains(&answered["status"].as_str().unwrap_or_default()),
        "the founding was refused: {answered}"
    );

    // Right after founding answers, a call only the person's part answers, listing their
    // workspace: it waits for the part to start and is answered then.
    let (listed, said, failed) = actions
        .tool("tilo_ls", &json!({"path": ""}))
        .expect("a call made right after founding is answered");
    assert!(
        !failed && said.contains("devices"),
        "a call made right after founding is refused or unknown instead of waiting for the \
         person's part: {said} {listed}"
    );

    // And the founding's call ends with the tickets it asked for, followed by its id.
    let mut answer = answered;
    if let Some(invocation) = answer["invocation_id"].as_str() {
        call["invocation_id"] = json!(invocation);
    }
    call["wait_ms"] = json!(60_000);
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
    let person_key = crate::device::services::tests::as_the_daemon(|| {
        tilo_identity::provisioning::create_person(
            &founding_store,
            &keys,
            "Ada",
            "",
            tilo_identity::provisioning::IdentityStorage::Files,
        )
        .unwrap()
    });
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

/// A person founded on this computer logs it in at their own server: once it keeps their own
/// workspace, the person's part enters through it as it runs. An agent connected before is still
/// served, the same folders are shown, and a new agent's call is answered. The daemon started
/// afresh enters through the server too: the workspace names it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one person's first server, from founding to a call answered after the move, read top to bottom"
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
            "the test did not finish in {}s: the person's part did not enter through their \
             server",
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

    // The part that serves now: the one that serves after the move too.
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
    // It is taken up at once, and the person's part enters through the server as it runs.
    assert!(
        matches!(
            logged_in,
            tilo_actions::remote::ReceiptStatus::Accepted
                | tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person's computer logs in at their server: {logged_in:?}"
    );

    // The person's part keeps running, and enters through the server from now on: the agent
    // connected before the move is still served by it.
    writeln!(
        &connected,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    )
    .unwrap();
    let mut listed = String::new();
    let read = std::io::BufReader::new(&connected).read_line(&mut listed);
    assert!(
        matches!(read, Ok(n) if n > 0) && listed.contains("\"id\":2"),
        "the agent connected before the move is not served after it: {read:?} {listed}"
    );
    drop(connected);
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
    let now = running.services(Duration::from_secs(1)).unwrap();
    assert!(
        std::ptr::eq(Arc::as_ptr(&now), first.as_ptr()),
        "the person's part started again for the move"
    );
    drop(now);
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
    let PersonServer {
        server, serving, ..
    } = home;
    serving.block_on(server.shutdown());
    drop(environment);
}

/// The person's server: a device their root certifies, keeping workspaces on this machine, and
/// the line another device reaches it at.
struct PersonServer {
    line: String,
    cert: tilo_user_identity::DeviceCert,
    server: crate::device::keep::server::KeptServer,
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
        .block_on(crate::device::keep::server::KeptServer::with_relay_mode(
            tilo_server_protocol::config::ServerConfig {
                state_root: state.path().join("server"),
                ..Default::default()
            },
            crate::device::keep::server::ServerIdentity {
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

const SIGNS_A_BROWSER_IN_TEST: &str =
    "device::founding_tests::a_person_founded_here_signs_a_browser_in_through_their_server";

/// How long the whole of [`SIGNS_A_BROWSER_IN_TEST`] may take: past it the test fails and its
/// process ends, whatever step it is in, rather than hanging the run.
const SIGNS_A_BROWSER_IN_BOUND: Duration = Duration::from_secs(240);

/// A person founded on this computer, which holds their root, gains a server and enters through
/// it. A tab asks that server to be signed in as them and is shown a ticket, and
/// `tilo device add <ticket>` here approves it through the daemon as the command asks it: the
/// daemon reaches the server as the device it entered as, and the tab ends signed in with the
/// certificate the root here signed for it. A computer that founded its person is as logged in
/// as one a ticket made.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one browser's sign-in, from founding the person to the tab signed in, read top to bottom"
)]
fn a_person_founded_here_signs_a_browser_in_through_their_server() {
    use tilo_device_identity::login::{APPROVE, FOLLOW, ISSUE, PENDING, Tab, Ticket};

    let Some(profile) = isolated_root(SIGNS_A_BROWSER_IN_TEST) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(SIGNS_A_BROWSER_IN_BOUND);
        eprintln!(
            "the test did not finish in {}s: the browser was not signed in",
            SIGNS_A_BROWSER_IN_BOUND.as_secs()
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
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");

    // The person's server, and this computer logged in at it.
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
    // What `tilo device add` reads first: where this computer enters, through the server.
    let named = tilo_user_identity::canonical::canonical_bytes(&home.cert)
        .map(hex::encode)
        .unwrap();
    let started = std::time::Instant::now();
    let who = loop {
        wait_for_the_person_serving(&socket);
        let who = ask(&socket, &json!({"native": "account#who"}));
        if who["home"]["server_cert"] == json!(named) {
            break who;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the person's part does not enter through their server: {who}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        who["holds_root"], true,
        "this computer holds the root: {who}"
    );
    let workspace = who["home"]["workspace"].as_str().unwrap().to_owned();
    let person = who["person"].as_str().unwrap().to_owned();

    // A tab asks the server to be signed in as the person, and shows its ticket.
    let host = home.cert.device_public_key;
    let tab = tilo_session_client::session::TicketLink::new(tilo_server_client::ServerAddress::at(
        home.server.node_addr(),
    ));
    let tab_key = SigningKey::from_bytes(&[0x7a; 32]);
    let tab_ask = |action: &str, argument: Value| {
        let Value::Object(argument) = argument else {
            panic!("a ticket action is asked with its arguments");
        };
        home.serving
            .block_on(tab.ask(
                Arc::new(tab_key.clone()),
                tab_key.verifying_key().to_bytes(),
                host,
                action,
                argument,
                Duration::from_secs(20),
            ))
            .expect("the server answers the tab")
    };
    let minted = tab_ask(
        ISSUE,
        json!({"direction": "sign-in", "user": person, "label": "Firefox on Linux"}),
    )
    .expect("the tab asks to be signed in as the person");
    let shown = minted["ticket"].as_str().expect("a ticket to show");
    // Copied out of the sentence that shows it, with the comma after it.
    let ticket = Ticket::parse(&format!("{shown},"))
        .expect("the ticket reads")
        .encode();

    // `tilo device add <ticket>`, through the daemon as the command asks it: what waits, then
    // the approval the root here signs.
    let calls = |action: &'static str, args: Value| {
        home.serving.block_on(async {
            let client =
                tilo_api::calls::Calls::open_with(tilo_api::calls::Reach::default(), || {
                    tilo_files::native::socket::connect(&socket).map_err(|error| error.to_string())
                })
                .await?;
            client
                .act(
                    Some(&workspace),
                    "devices",
                    action,
                    &args,
                    "Sign a browser in",
                )
                .await
        })
    };
    let pending = calls(PENDING, json!({ "ticket": ticket }))
        .expect("the daemon reads what waits at the person's server");
    let waiting = &pending["result"];
    assert_eq!(
        waiting["device"],
        hex::encode(tab_key.verifying_key().to_bytes()),
        "the tab waits: {pending}"
    );
    let asked = Tab {
        device_public_key: hex::encode(tab_key.verifying_key().to_bytes()),
        label: waiting["label"].as_str().unwrap_or("a browser").to_owned(),
        key_for: None,
    };
    let approved = calls(
        APPROVE,
        json!({ "ticket": ticket, "tab": asked, "label": "this computer" }),
    )
    .expect("the daemon approves the tab");
    assert_eq!(
        approved["result"]["signed"], true,
        "the root here signed the tab: {approved}"
    );

    // The tab, following its ticket, is signed in as the person.
    let followed = tab_ask(FOLLOW, json!({ "id": minted["id"] })).expect("the tab follows");
    let certified: tilo_user_identity::DeviceCert = serde_json::from_str(
        followed["certified"]
            .as_str()
            .unwrap_or_else(|| panic!("the tab is signed in: {followed}")),
    )
    .unwrap();
    assert_eq!(
        certified.device_public_key,
        tab_key.verifying_key().to_bytes()
    );
    assert_eq!(hex::encode(certified.user_public_key), person);

    home.serving.block_on(tab.close());
    running.stop().unwrap();
    let PersonServer {
        server, serving, ..
    } = home;
    serving.block_on(server.shutdown());
    drop(environment);
}

const KEEPS_THE_PORT_TEST: &str =
    "device::founding_tests::a_part_that_starts_again_takes_the_port_the_part_that_ended_bound";

/// How long this test keeps the closing work of the part that ended busy: past every bound the
/// ending waits on and past the restart, as a folder that will not unmount does.
const BUSY_CLEANUP: Duration = Duration::from_secs(120);

/// How long the person's part may take to start again through their server: the bound the test
/// already had before the port was named.
const START_AGAIN_WITHIN: Duration = Duration::from_secs(90);

/// Another person's computer logs in here with a ticket, which makes this computer theirs: the
/// part that served ends, while the work closing behind it is still busy, and starts again as
/// them. The endpoint outlives the part that served on it: the part that starts takes the port
/// the part that ended served on, rather than binding it again after that cleanup let go of it.
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

    // Another person's computer issues a ticket, and this computer logs in with it as `tilo
    // login <ticket>` does: this computer is their device now, so the part that serves ends and
    // starts again while what that part still has to let go of is busy.
    let other = other_computer();
    let logged_in = device_call(
        &socket,
        tilo_device_identity::login::LOG_IN,
        json!({ "ticket": other.ticket, "agent": "tilo", "label": "this computer" }),
    );
    assert!(
        matches!(
            logged_in,
            tilo_actions::remote::ReceiptStatus::Accepted
                | tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "this computer logs in with the ticket: {logged_in:?}"
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
            "this computer's part did not start again while the part that ended was still closing"
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
    drop(environment);
}

/// Another person's computer, run by this process: the workspace it hosts, the device actions
/// that certify a computer its ticket lets in, and the runtime both run on. Its ticket is the one
/// this computer logs in with, as a computer of theirs.
struct OtherComputer {
    _workspaces: Arc<crate::device::workspaces::ServiceWorkspaces>,
    _state: tempfile::TempDir,
    _keys: tilo_device_identity::persist::DeviceKeyLock,
    _router: iroh::protocol::Router,
    _runtime: tokio::runtime::Runtime,
    ticket: String,
}

/// [`OtherComputer`], with its ticket issued: what a second computer of a person's serves.
fn other_computer() -> OtherComputer {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (workspaces, state, keys, router, ticket) = runtime.block_on(async {
        let (workspaces, state, key, cert, keys) =
            crate::device::workspaces::ServiceWorkspaces::test_of_person().await;
        // The workspace a computer of theirs enters is their own, at the id their key names.
        let workspace = tilo_desktop_model::WorkspaceId::from(collab_types::person_workspace(
            &cert.user_public_key,
        ));
        workspaces.found(workspace, "Home").await.unwrap();
        let tickets = crate::device::ticket::Tickets::new(
            (key, cert),
            workspaces.endpoint(),
            None,
            crate::device::ticket_tests::root_in(state.path().to_path_buf()),
            Some(crate::device::first_workspace::FirstWorkspace::founded(
                Arc::clone(&workspaces),
                workspace,
            )),
            Some(Arc::clone(&workspaces)),
            tilo_device_identity::login::Place::at(
                state.path().join("issuer"),
                tilo_device_identity::IdentityPaths::under(state.path().join("issuer-keys")),
            ),
            workspaces.copies().action_service(),
        );
        let router =
            crate::device::ticket_tests::serve(&workspaces, workspaces.copies().action_service());
        let printed = crate::device::ticket_tests::ask_device(
            &tickets,
            tilo_device_identity::login::ISSUE,
            json!({ "capability": "member" }),
        )
        .await
        .expect("the other computer issues a ticket");
        let ticket = printed["ticket"]
            .as_str()
            .unwrap_or_else(|| panic!("the ticket it issued: {printed}"))
            .to_owned();
        (workspaces, state, keys, router, ticket)
    });
    OtherComputer {
        _workspaces: workspaces,
        _state: state,
        _keys: keys,
        _router: router,
        _runtime: runtime,
        ticket,
    }
}

const ROOT_ONLY_AT_ONCE_TEST: &str = "device::founding_tests::a_person_names_their_servers_from_the_computer_they_were_just_founded_on";

/// Which servers keep a workspace changes only on the computer that keeps its person's root, and
/// the computer a person was just founded on is that one from their first call: naming a server
/// right after founding gets to the servers themselves, not to a refusal that the computer does
/// not keep the root.
#[test]
fn a_person_names_their_servers_from_the_computer_they_were_just_founded_on() {
    let Some(profile) = isolated_root(ROOT_ONLY_AT_ONCE_TEST) else {
        return;
    };
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
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");
    wait_for_the_person_part(&socket);
    let person = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(
            &tilo_identity::store::account_identity_dir_from_env().unwrap(),
        )
        .expect("the founded person's root"),
    )
    .unwrap()
    .group_public_key();
    let workspace = tilo_server_protocol::servers::person_workspace_id(&person);
    let answered = workspace_call(
        &socket,
        &workspace,
        "server.remove",
        json!({"server": "the server Ada never had"}),
        60_000,
    )
    .expect("the call is answered");
    let tilo_actions::remote::ReceiptStatus::Rejected { error } = &answered else {
        panic!("a server Ada never had is not taken out: {answered:?}");
    };
    let said = format!("{error:?}");
    assert!(
        said.contains("no server keeping the workspace goes by"),
        "the computer Ada was founded on reaches her servers: {said}"
    );
    running.stop().unwrap();
    drop(environment);
}

const MOVE_TEST: &str =
    "device::founding_tests::a_move_to_the_persons_server_keeps_their_part_and_its_calls";

/// How long the whole of [`MOVE_TEST`] may take: past it the test fails and its process ends,
/// whatever step it is in, rather than hanging the run.
const MOVE_BOUND: Duration = Duration::from_secs(240);

/// How long the call that spans the move runs.
const SPANNING: Duration = Duration::from_secs(20);

/// A call that names the person's server in their own workspace moves this computer to it, while
/// its person's part keeps running: a call that was running across the move hears its own
/// answer, a call asked during it is answered at once, and from then on the part enters through
/// the server the call named.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one move, from the call that spans it to the server entered through, read top to \
              bottom"
)]
fn a_move_to_the_persons_server_keeps_their_part_and_its_calls() {
    let Some(profile) = isolated_root(MOVE_TEST) else {
        return;
    };
    std::thread::spawn(|| {
        std::thread::sleep(MOVE_BOUND);
        eprintln!(
            "the test did not finish in {}s: a call across the move was not answered",
            MOVE_BOUND.as_secs()
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
    running
        .wait_for_person(Duration::from_secs(120))
        .expect("the person's services run");
    wait_for_the_person_part(&socket);
    let first = Arc::downgrade(&running.services(Duration::from_secs(120)).unwrap());

    // An agent's watch of this computer's location that runs past the move, on one connection:
    // each call is held until something changes or its wait runs out, and the next goes on from
    // the cursor it answered. Who is here changes with the move (the person's devices are then
    // present through their server), which answers the call it falls in; the watch goes on until
    // its whole time has run, and its last call is answered by running out its wait.
    let (agent, init) = agent_initialized(&socket);
    assert_eq!(
        init["result"]["serverInfo"]["name"], "tilo-runtime",
        "the person's part answers the agent: {init}"
    );
    let watch = move |id: u64, cursor: Option<&str>, wait: Duration| {
        let mut arguments = json!({"path": "location/Location",
                                   "wait_ms": u64::try_from(wait.as_millis()).unwrap()});
        if let Some(cursor) = cursor {
            arguments["cursor"] = json!(cursor);
        }
        let mut writing = &agent;
        writeln!(
            writing,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
                "name": "tilo_watch", "arguments": arguments}})
        )
        .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&agent)
            .read_line(&mut line)
            .expect("the watch is answered");
        let answered: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answered["id"], id, "{answered}");
        assert!(
            answered["result"].is_object() && answered["result"]["isError"] != true,
            "a watch across the move is answered as it would be without one: {answered}"
        );
        answered
    };
    let mut cursor =
        watch(2, None, Duration::from_secs(1))["result"]["structuredContent"]["cursor"]
            .as_str()
            .expect("a watch answers a cursor")
            .to_owned();
    let spanning = std::thread::spawn(move || {
        let asked = std::time::Instant::now();
        let mut answers = Vec::new();
        for id in 3.. {
            let left = SPANNING.saturating_sub(asked.elapsed());
            let answered = watch(id, Some(&cursor), left.max(Duration::from_millis(1)));
            let content = &answered["result"]["structuredContent"];
            cursor = content["cursor"].as_str().expect("a cursor").to_owned();
            let ran_out = content["events"].as_array().is_some_and(Vec::is_empty);
            answers.push(answered.to_string());
            if ran_out && asked.elapsed() >= SPANNING.saturating_sub(Duration::from_secs(1)) {
                break;
            }
        }
        (asked.elapsed(), answers)
    });

    // A server of the person's keeps their own workspace from now on.
    let (serving, _state, server, line) = the_persons_server(0x6c);
    let person = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(
            &tilo_identity::store::account_identity_dir_from_env().unwrap(),
        )
        .expect("the founded person's root"),
    )
    .unwrap()
    .group_public_key();
    let workspace = tilo_server_protocol::servers::person_workspace_id(&person);
    let named = workspace_call(
        &socket,
        &workspace,
        "server.add",
        json!({"host": &line}),
        60_000,
    )
    .expect("the call that named the person's server is answered");
    assert!(
        matches!(named, tilo_actions::remote::ReceiptStatus::Completed { .. }),
        "the person's server keeps their workspace: {named:?}"
    );

    // The part enters through the server the call named, as it runs.
    let placed: tilo_server_protocol::web::PageHost = serde_json::from_str(&line).unwrap();
    let server_cert = tilo_user_identity::canonical::canonical_bytes(&placed.server_cert)
        .map(hex::encode)
        .unwrap();
    let started = std::time::Instant::now();
    loop {
        let who = ask(&socket, &json!({"native": "account#who"}));
        if who["home"]["server_cert"] == server_cert {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the person's part does not enter through the server the call named: {who}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !spanning.is_finished(),
        "the watch across the move ended before the move was made"
    );

    // A call asked during the move is answered at once, by the same part.
    let asked = std::time::Instant::now();
    let (during, init) = agent_initialized(&socket);
    assert_eq!(
        init["result"]["serverInfo"]["name"], "tilo-runtime",
        "the person's part answers an agent during the move: {init}"
    );
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "a call during the move waits for nothing: {:?}",
        asked.elapsed()
    );
    drop(during);

    // The watch across the move is answered on its one connection until its whole time ran.
    let (took, answers) = spanning.join().unwrap();
    assert!(
        took >= SPANNING.saturating_sub(Duration::from_secs(1)),
        "the watch ran its whole time: {took:?}: {answers:?}"
    );

    // No part started again: the one that served before the move serves after it.
    let now = running.services(Duration::from_secs(1)).unwrap();
    assert!(
        std::ptr::eq(Arc::as_ptr(&now), first.as_ptr()),
        "the person's part started again for the move"
    );
    drop(now);
    running.stop().unwrap();
    serving.block_on(server.shutdown());
    drop(environment);
}

/// A server on this machine the person's root certifies, keeping workspaces: its runtime, its
/// state, the server, and the line that names it to a device placing a workspace on it.
fn the_persons_server(
    seed: u8,
) -> (
    tokio::runtime::Runtime,
    tempfile::TempDir,
    crate::device::keep::server::KeptServer,
    String,
) {
    let dir = tilo_identity::store::account_identity_dir_from_env().unwrap();
    let root = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(&dir).expect("the founded person's root"),
    )
    .unwrap();
    let server_key = SigningKey::from_bytes(&[seed; 32]);
    let mut server_cert = tilo_user_identity::DeviceCert {
        user_public_key: root.group_public_key(),
        device_public_key: server_key.verifying_key().to_bytes(),
        device_label: "home-server".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut server_cert, &root).unwrap();
    let serving = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let state = tempfile::tempdir().unwrap();
    let server = serving
        .block_on(crate::device::keep::server::KeptServer::with_relay_mode(
            tilo_server_protocol::config::ServerConfig {
                state_root: state.path().join("server"),
                ..Default::default()
            },
            crate::device::keep::server::ServerIdentity {
                user_public_key: root.group_public_key(),
                device_signing_key: server_key,
                device_cert: server_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        ))
        .unwrap();
    let line = serde_json::to_string(&tilo_server_protocol::web::PageHost {
        server_cert,
        address: Some(tilo_server_protocol::web::PageHostAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .unwrap();
    (serving, state, server, line)
}

/// The workspace's own `action` on its servers, called over the frame wire as a client of this
/// computer does, in the person's own workspace: its receipt, or none when the daemon closed the
/// connection without one.
fn workspace_call(
    socket: &Path,
    workspace: &str,
    action: &str,
    args: Value,
    wait_ms: u32,
) -> Option<tilo_actions::remote::ReceiptStatus> {
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
    };
    let Value::Object(argument) = args else {
        panic!("a workspace action is called with its arguments");
    };
    let request = RemoteActionMessage::Invoke(InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
            address: tilo_actions::remote::workspace_governance_address(workspace, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(wait_ms),
        appearance: None,
        signed: None,
    });
    let stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut writer = stream.try_clone().unwrap();
    let frame = RemoteActionFrame::new(request).in_workspace(Some(workspace.to_owned()));
    writer.write_all(&frame.encode().unwrap()).unwrap();
    writer.write_all(b"\n").unwrap();
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line).ok()?;
    match RemoteActionFrame::decode(line.trim_end().as_bytes())
        .ok()?
        .message
    {
        RemoteActionMessage::Receipt(receipt) => Some(receipt.status),
        other => panic!("a call is answered with a receipt, not {other:?}"),
    }
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
        asks: false,
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
    let _ = std::fs::remove_file(&keys.user_pubkey);
    if let Some(certs) = keys.device_cert_for_user(&[0; 32]).parent() {
        let _ = std::fs::remove_dir_all(certs);
    }
}

/// Wait until the person's part of the daemon serves: its status answers as theirs from then on,
/// where the device's own actions answer alone before. A computer founded here names no server
/// until one is placed on its workspace, so no device is waited for.
fn wait_for_the_person_part(socket: &Path) -> Value {
    let started = std::time::Instant::now();
    loop {
        let status = ask(
            socket,
            &json!({"jsonrpc": "2.0", "id": 1, "method": tilo_api::daemon::PEER_STATUS}),
        );
        if status["result"]["session"] != json!("starting") {
            return status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the person's part does not serve: {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
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

/// `tilo_do` of `path` with `args`, as the daemon's native endpoint answers it, waited on for as
/// long as a founding takes: it answers once the person's workspace is founded, as their
/// services start.
fn do_request(path: &str, args: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
        "name": "tilo_do",
        "arguments": {"path": path, "args": args, "wait_ms": FOUNDING_WAIT_MS},
    }})
}

/// How long a test waits on a call that founds a person: their services start before it answers.
const FOUNDING_WAIT_MS: u64 = 120_000;

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
