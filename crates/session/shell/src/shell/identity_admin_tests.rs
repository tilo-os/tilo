use super::*;
use crate::shell::search::action_api::ArgValue;
use std::sync::Mutex;
use std::sync::mpsc;

/// An identity store provisioned the way first boot does, in files, with
/// the identity its workspace selects.
struct Provisioned {
    _root: tempfile::TempDir,
    store: Store,
    identity: IdentityProfile,
}

fn provisioned() -> Provisioned {
    let root = tempfile::tempdir().expect("temp root");
    let dir = root.path().join("config/tilo/identity");
    let device = tilo_user_identity::IdentityPaths::under(root.path().join("system"));
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &dir,
        &device,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("provision");
    let registry = crate::identity_registry::IdentityRegistryStore::at(&dir)
        .load()
        .expect("registry");
    let id = registry.default_identity.clone().expect("default identity");
    let identity = registry.identities.get(&id).expect("profile").clone();
    Provisioned {
        store: Store {
            dir,
            device,
            root: root.path().join("system"),
        },
        _root: root,
        identity,
    }
}

fn args(step: &str, supplied: &[(&str, ArgValue)]) -> ActionArgs {
    ActionArgs::bind(
        params(step),
        supplied
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone())),
    )
    .expect("arguments bind")
}

/// What a step answered at once.
fn answered(started: Result<Started, String>) -> serde_json::Value {
    match started.expect("the step runs") {
        Started::Answered(value) => value,
        Started::Later(_) => panic!("the step answers at once"),
    }
}

/// The admin whose relay commands are recorded and answered with `stdout`.
fn recording(stdout: &'static str) -> (IdentityAdmin, Arc<Mutex<Vec<HostCommand>>>) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&commands);
    let admin = IdentityAdmin::with_runner(Arc::new(move |command, _| {
        recorded.lock().expect("commands").push(command.clone());
        Ok(Finished {
            success: true,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }));
    (admin, commands)
}

#[test]
fn identity_steps_list_change_and_protect_the_store_without_answering_a_secret() {
    // The temporary root is bound, not dropped with `..`: it holds the store.
    let Provisioned {
        _root,
        store,
        identity,
    } = provisioned();
    let (admin, commands) = recording("");

    let listed = answered(admin.start(LIST, &ActionArgs::none(), &identity, &store));
    let entries = listed.as_array().expect("a list");
    assert_eq!(entries.len(), 1, "{listed}");
    let entry = &entries[0];
    assert_eq!(entry["active"], true);
    assert_eq!(entry["public_key"], hex::encode(identity.user_public_key));
    let summary: runtime_identity::IdentitySummary =
        serde_json::from_value(entry.clone()).expect("Settings reads the answer back");
    let text = listed.to_string();
    assert!(
        !text.contains("secret") && !text.contains("record_path"),
        "no secret or record path leaves the host: {text}"
    );

    let id = ArgValue::Text(summary.id.clone());
    let yes = ArgValue::Boolean(true);
    assert!(
        admin
            .start(
                ACTIVATE,
                &args(
                    ACTIVATE,
                    &[("id", id.clone()), ("confirm", ArgValue::Boolean(false))]
                ),
                &identity,
                &store,
            )
            .is_err(),
        "using an identity needs the person's confirmation"
    );
    assert_eq!(
        answered(admin.start(
            ACTIVATE,
            &args(ACTIVATE, &[("id", id.clone()), ("confirm", yes.clone())]),
            &identity,
            &store
        )),
        serde_json::json!({ "id": summary.id, "active": true })
    );
    let protected = answered(admin.start(
        PROTECT,
        &args(PROTECT, &[("id", id.clone()), ("confirm", yes)]),
        &identity,
        &store,
    ));
    assert_eq!(protected["id"], summary.id.as_str());

    // Removing needs the person's confirmation, and the store keeps its
    // last identity whatever the caller confirmed.
    let unconfirmed = admin.start(
        REMOVE,
        &args(
            REMOVE,
            &[("id", id.clone()), ("confirm", ArgValue::Boolean(false))],
        ),
        &identity,
        &store,
    );
    assert!(
        matches!(&unconfirmed, Err(reason) if reason.contains("confirmation")),
        "{:?}",
        unconfirmed.err()
    );
    assert!(
        admin
            .start(
                REMOVE,
                &args(REMOVE, &[("id", id), ("confirm", ArgValue::Boolean(true))]),
                &identity,
                &store,
            )
            .is_err(),
        "the active, final identity stays"
    );
    assert_eq!(
        runtime_identity::list_identities(&store.dir)
            .expect("listed")
            .len(),
        1
    );
    assert!(
        commands.lock().expect("commands").is_empty(),
        "no step that stays on this device starts a process"
    );
}

/// Enroll a phone under `identity` in `store`; its key, in hex.
fn enroll_phone(store: &Store, identity: &IdentityProfile) -> String {
    let fleets = store.fleets();
    let issued_at_ns = fleets.load(&identity.id).expect("fleet").certificates[0].issued_at_ns;
    let token = fleets
        .mint_enrollment(
            &identity.id,
            tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns,
            issued_at_ns.saturating_add(5_000),
        )
        .expect("token");
    let phone = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    fleets
        .complete_enrollment(
            &identity.id,
            &token,
            tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
            phone.verifying_key().to_bytes(),
            "Phone",
            issued_at_ns,
        )
        .expect("enrolled");
    hex::encode(phone.verifying_key().to_bytes())
}

#[test]
fn devices_are_listed_and_revoked_by_the_host_root_key() {
    // The temporary root is bound, not dropped with `..`: it holds the store.
    let Provisioned {
        _root,
        store,
        identity,
    } = provisioned();
    let admin = IdentityAdmin::with_runner(Arc::new(|_, _| Err("no process runs".into())));

    let devices: Vec<FleetDevice> = serde_json::from_value(answered(admin.start(
        DEVICES,
        &ActionArgs::none(),
        &identity,
        &store,
    )))
    .expect("devices");
    assert_eq!(devices.len(), 1, "{devices:?}");
    assert!(devices[0].current && !devices[0].revoked, "{devices:?}");

    // Enroll a phone, then revoke it through the step.
    let phone_hex = enroll_phone(&store, &identity);

    let device = ArgValue::Text(phone_hex.clone());
    assert!(
        admin
            .start(
                REVOKE,
                &args(
                    REVOKE,
                    &[
                        ("device", device.clone()),
                        ("confirm", ArgValue::Boolean(false))
                    ]
                ),
                &identity,
                &store,
            )
            .is_err(),
        "revocation needs the person's confirmation"
    );
    assert!(
        admin
            .start(
                REVOKE,
                &args(
                    REVOKE,
                    &[
                        ("device", device.clone()),
                        ("reason", ArgValue::Text("misplaced".into())),
                        ("confirm", ArgValue::Boolean(true)),
                    ],
                ),
                &identity,
                &store,
            )
            .is_err(),
        "an unknown reason is refused"
    );
    assert_eq!(
        answered(admin.start(
            REVOKE,
            &args(
                REVOKE,
                &[
                    ("device", device),
                    ("reason", ArgValue::Text("lost".into())),
                    ("confirm", ArgValue::Boolean(true)),
                ],
            ),
            &identity,
            &store,
        )),
        serde_json::json!({ "device": phone_hex, "revoked": true })
    );
    let devices: Vec<FleetDevice> = serde_json::from_value(answered(admin.start(
        DEVICES,
        &ActionArgs::none(),
        &identity,
        &store,
    )))
    .expect("devices");
    let phone = devices
        .iter()
        .find(|device| device.key_hex == phone_hex)
        .expect("the phone is listed");
    assert!(phone.revoked && !phone.current, "{devices:?}");
    assert_eq!(phone.label, "Phone");
}

#[test]
fn home_server_removal_runs_on_the_host_with_its_own_identity_directory() {
    let store = Store {
        dir: PathBuf::from("/home/person/.config/tilo/identity"),
        device: tilo_user_identity::IdentityPaths::under("/var/lib/tilo"),
        root: PathBuf::from("/var/lib/tilo"),
    };
    let identity = IdentityProfile {
        id: "00".repeat(32),
        display_name: String::new(),
        record_path: PathBuf::from("/nonexistent"),
        user_public_key: [0; 32],
    };
    let (admin, commands) = recording("{\"decommissioned\":true,\"files_ready\":false}\n");
    let device = "AA".repeat(32);
    let service = "bb".repeat(32);

    let unconfirmed = admin.start(
        HOME_PEER_REMOVE,
        &args(
            HOME_PEER_REMOVE,
            &[
                ("device", ArgValue::Text(device.clone())),
                ("service", ArgValue::Text(service.clone())),
                ("confirm", ArgValue::Boolean(false)),
            ],
        ),
        &identity,
        &store,
    );
    assert!(unconfirmed.is_err());
    assert!(commands.lock().expect("commands").is_empty(), "nothing ran");

    let Started::Later(answer) = admin
        .start(
            HOME_PEER_REMOVE,
            &args(
                HOME_PEER_REMOVE,
                &[
                    ("device", ArgValue::Text(device)),
                    ("service", ArgValue::Text(service.clone())),
                    ("confirm", ArgValue::Boolean(true)),
                ],
            ),
            &identity,
            &store,
        )
        .expect("starts")
    else {
        panic!("removal reaches the home server later");
    };
    let answer = answer
        .wait_timeout(Duration::from_secs(10))
        .ok()
        .flatten()
        .expect("answered")
        .expect("removed");
    assert_eq!(answer["decommissioned"], true);

    let Started::Later(retried) = admin
        .start(HOME_PEER_RETRY, &ActionArgs::none(), &identity, &store)
        .expect("starts")
    else {
        panic!("retrying reaches the home servers later");
    };
    let (done, wait) = mpsc::channel();
    retried.then(move |answer| done.send(answer).expect("sent"));
    assert!(
        wait.recv_timeout(Duration::from_secs(10))
            .expect("answered")
            .is_some()
    );

    let recorded = commands.lock().expect("commands").clone();
    let expected = |command: &str, tail: &[&str]| HostCommand {
        program: PathBuf::from(RELAY_ADMIN),
        args: [
            command,
            "--identity-root",
            "/var/lib/tilo",
            "--identity-dir",
            "/home/person/.config/tilo/identity",
        ]
        .iter()
        .chain(tail)
        .map(OsString::from)
        .collect(),
        timeout: HOME_PEER_TIMEOUT,
    };
    assert_eq!(
        recorded,
        [
            expected(
                "home-peer-remove",
                &["--device", &"aa".repeat(32), "--service", &service]
            ),
            expected("home-peer-retry-removals", &[]),
        ]
    );
}
