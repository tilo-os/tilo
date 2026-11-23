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

/// A setting a laptop sets in its person's own record is taken in by the person's other devices:
/// its change names the person, and is judged as a change of theirs made on their own device,
/// which carries no invocation because nobody else asked for it.
#[test]
fn a_laptop_writes_its_own_settings_record_and_it_is_taken_in() {
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
    let change = doc
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_changes(&[])
        .into_iter()
        .next()
        .expect("a change");
    let mine = tilo_action_policy::PersonId::from_root(person);
    // What every receiving device asks of it: the claim its history entry makes ...
    let claim =
        collab_types::claim::ChangeClaim::of_change(change.hash().0, change.message(), Vec::new())
            .expect("the change is claimed as an edit of the record");
    assert_eq!(
        claim.invoker.as_ref().and_then(|acting| acting.person_id()),
        Some(mine.as_str()),
        "the laptop's write is its person's"
    );
    // ... and who it names as having asked for it.
    let context = change
        .message()
        .and_then(tilo_actions::ActionContext::from_change_message)
        .expect("the change names its action");
    let invoker = context
        .invoker
        .as_ref()
        .expect("the change names who set it");
    assert!(
        invoker.device.is_none(),
        "made on the person's own device, for nobody else"
    );
    assert_eq!(
        tilo_server_protocol::invocation::verify_recorded(
            &context,
            &persons_workspace(&person),
            &tilo_user_identity::RevocationList::new(),
            u64::MAX,
        ),
        Ok(None),
        "a person's own change carries no invocation and needs none"
    );
}
