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

/// A person founded on this computer adds their own server: their own workspace names it, so the
/// person's part ends and starts again through it, in the same process. The part that ended is
/// gone (its services, links and stores let go), the one that starts shows the same folders,
/// and an agent's call is answered by it rather than refused as not started.
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
        std::process::exit(101);
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
    let (first, first_workspaces) = {
        let services = running.services(Duration::from_secs(120)).unwrap();
        (
            Arc::downgrade(&services),
            Arc::downgrade(services.serving()),
        )
    };

    // The person's server: a device their root certifies, keeping workspaces on this machine.
    let dir = tilo_identity::store::account_identity_dir_from_env().unwrap();
    let root = tilo_identity::store::load_from_path(
        tilo_identity::store::resolve_active_identity(&dir).expect("the founded person's root"),
    )
    .unwrap();
    let server_key = SigningKey::from_bytes(&[0x6e; 32]);
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
        .block_on(tilo_server::daemon::Server::start_with_relay_mode(
            tilo_server::ServerConfig {
                state_root: state.path().join("server"),
                ..Default::default()
            },
            tilo_server::daemon::ServerIdentity {
                user_public_key: root.group_public_key(),
                device_signing_key: server_key,
                device_cert: server_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        ))
        .unwrap();
    let line = serde_json::to_string(&tilo_server_protocol::web::PageHost {
        server_cert: server_cert.clone(),
        address: Some(tilo_server_protocol::web::PageHostAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
        origin: None,
    })
    .unwrap();

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
    // The part that ended let go of its services, and the workspace service they ran closed
    // its endpoint: nothing of it answers or reaches out as this device any more.
    let started = std::time::Instant::now();
    let closed = || {
        first_workspaces
            .upgrade()
            .is_none_or(|workspaces| workspaces.endpoint().is_closed())
    };
    while first.strong_count() > 0 || !closed() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the part that ended is still held ({} times) or its endpoint still open",
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
    let named = tilo_user_identity::canonical::canonical_bytes(&server_cert)
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
