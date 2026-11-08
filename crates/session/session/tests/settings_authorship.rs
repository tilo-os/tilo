//! Who a setting changed in a session belongs to: the person at the device,
//! by the id their workspaces name them by — the root key this device's
//! certificate carries — and never a placeholder the session was started
//! with. Their undo history shows them, including what was changed here
//! before the device had an identity.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use serde_json::{Map, Value, json};
use tilo_session::{Options, Session};
use tilo_settings_service::{SettingsService, setting_resource};

const HIGH_CONTRAST: &str = "appearance.high_contrast";
/// The appearance section's field the setting holds.
const FIELD: &str = "high_contrast";
const SECTION: &str = "settings:settings.appearance";

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own. Returns the root's directory — kept alive for the session — and
/// the person's key.
fn this_computers_person(home: &mut tilo_test_env::TestHome) -> (tempfile::TempDir, [u8; 32]) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    paths
        .ensure_directories()
        .expect("this computer's identity");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    home.env().set("TILO_IDENTITY_ROOT", root.path());
    (root, cert.user_public_key)
}

/// A session hosting the person's settings service, as the session starts it: the service is made
/// without knowing who sits here, and the session tells it as it starts it.
fn session() -> Session {
    crate::common::headless(Options {
        services: vec![Box::new(SettingsService::new())],
        collab: false,
        ..Options::default()
    })
}

/// The settings document's history, oldest first.
fn changes() -> Vec<Value> {
    settings_store::local()
        .expect("the person's settings")
        .change_log()
}

/// Whether the person's high contrast is on as the session reads it.
fn high_contrast(session: &Session) -> bool {
    session
        .read(SECTION)
        .and_then(|section| section[FIELD].as_bool())
        .expect("the appearance section is published")
}

/// What the person did after `before` : the changes added to the history since it was `before`
/// entries long.
fn changes_since(before: usize) -> Vec<Value> {
    let mut all = changes();
    all.drain(..before.min(all.len()));
    all
}

#[test]
fn a_setting_changed_in_a_session_is_the_persons_and_their_history_shows_it() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (_identity, person) = this_computers_person(&mut home);
    let person = hex::encode(person);
    let mut session = session();
    session.step(std::time::Duration::from_millis(1)).unwrap();

    let before = changes().len();
    session
        .invoke(&setting_resource(HIGH_CONTRAST), "switch", &Map::new())
        .expect("the switch is published");
    assert!(high_contrast(&session), "the switch turned it on");

    let changed = changes_since(before);
    let change = changed
        .iter()
        .find(|change| change["leaf"] == json!(FIELD))
        .expect("the change is in the settings document's history");
    assert_eq!(
        change["principal"],
        json!(person),
        "the person at the device made it, by the id their workspaces know them by"
    );
    assert_eq!(change["action"], json!("settings.appearance/field-edit"));

    // Their own undo finds the step: it is theirs, and taking it back is theirs to do.
    let before = changes().len();
    session
        .invoke(SECTION, "undo", &Map::new())
        .expect("their step is taken back");
    assert!(!high_contrast(&session), "the switch is off again");
    let undone = changes_since(before);
    assert!(
        undone.iter().any(
            |change| change["action"] == json!("settings.appearance/undo")
                && change["principal"] == json!(person)
        ),
        "the undo is the person's too: {undone:?}"
    );
}

#[test]
fn what_was_changed_before_the_device_knew_them_is_still_theirs_to_take_back() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (_identity, person) = this_computers_person(&mut home);
    let person = hex::encode(person);

    // A session from before this computer had an identity changed the setting as the placeholder it
    // was started with.
    let mut earlier_session = SettingsService::new();
    earlier_session.seated("person", &[]);
    let mut services = tilo_service::Services::default();
    services.add(Box::new(earlier_session));
    services
        .invoke(&setting_resource(HIGH_CONTRAST), "switch", &Map::new())
        .expect("the switch is published")
        .expect("flipped");
    assert_eq!(
        services.read(SECTION).expect("published")[FIELD],
        json!(true),
        "it was on before this session"
    );

    let mut session = session();
    session.step(std::time::Duration::from_millis(1)).unwrap();
    assert!(high_contrast(&session), "the session reads it on");

    let before = changes().len();
    session
        .invoke(SECTION, "undo", &Map::new())
        .expect("the step is read as theirs");
    assert!(
        !high_contrast(&session),
        "the change made before the device knew them is taken back"
    );
    let undone = changes_since(before);
    assert!(
        undone.iter().any(
            |change| change["action"] == json!("settings.appearance/undo")
                && change["principal"] == json!(person)
        ),
        "the undo is the person's: {undone:?}"
    );
}
