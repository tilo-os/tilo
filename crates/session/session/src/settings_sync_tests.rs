#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use super::*;

#[test]
fn only_the_persons_sections_are_offered() {
    let document: Map<String, Value> = serde_json::from_value(serde_json::json!({
        "appearance": {"mode": "dark", "wallpaper": "dunes"},
        "app_personal": {"so.tilo.monitor": {"confirm_kill": true}},
        "app_device_local": {"so.tilo.monitor": {"device": 1}},
        "power": {"lock_minutes": 1},
    }))
    .unwrap();
    let offered = personal_leaves(&document);
    assert_eq!(
        offered.keys().collect::<Vec<_>>(),
        [
            "tilo.app_personal.so.tilo.monitor",
            "tilo.appearance.mode",
            "tilo.appearance.wallpaper",
        ],
        "this device's sections stay on it"
    );
    assert_eq!(
        leaf_of(&document, "tilo.app_personal.so.tilo.monitor"),
        offered.get("tilo.app_personal.so.tilo.monitor"),
        "an app's entry is one value, as it travels"
    );
}

/// A setting a laptop sets is its person's: the record's change names them, so another of their
/// devices takes it in instead of refusing it for naming nobody.
#[test]
fn a_settings_write_names_the_person_that_set_it() {
    let person = [7_u8; 32];
    let doc = Arc::new(Mutex::new(AutoCommit::new()));
    let record = Record {
        doc: Arc::clone(&doc),
        wake: Arc::new(|| {}),
        person,
    };
    settings_store::RoamingSettings::write(
        &record,
        &[(
            "appearance".to_owned(),
            "mode".to_owned(),
            serde_json::json!("dark"),
        )],
        &settings_store::ChangeOrigin::default(),
    )
    .unwrap();
    let doc = doc.lock().unwrap_or_else(PoisonError::into_inner);
    let change = doc.get_changes(&[]).into_iter().next().expect("a change");
    let named = change.message().expect("the change names what it is");
    let context = tilo_actions::ActionContext::from_change_message(named)
        .expect("the change names its action");
    assert_eq!(
        context
            .invoker
            .expect("the change names who set it")
            .acting
            .person_id(),
        Some(tilo_action_policy::PersonId::from_root(person).as_str()),
        "the laptop's write is its person's"
    );
}
