use serde_json::{Map, Value, json};
use tilo_actions::{ConfirmationPolicy, DoorError, ResourceSpec};
use tilo_service::{Service, Services};

use super::{INDEX, SettingsService, setting_resource};

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// The setting `id` as the index publishes it.
fn setting(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources
        .iter()
        .find(|resource| resource.id.as_str() == INDEX)?
        .children
        .iter()
        .find(|child| child.id.as_str() == setting_resource(id))
        .cloned()
}

fn services() -> Services {
    let mut services = Services::default();
    services.add(Box::new(SettingsService::new("ada")));
    services
}

#[test]
fn a_switch_says_whether_it_is_on_and_flips_through_its_section() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    let published = setting(&services.resources(), "appearance.high_contrast")
        .expect("high contrast is a switch search changes");
    let switch = published.actions.first().expect("its switch");
    assert_eq!(published.label, "High contrast");
    assert!(switch.state.toggles);
    assert_eq!(switch.state.badge.as_deref(), Some("Off"));

    let resource = setting_resource("appearance.high_contrast");
    services
        .invoke(&resource, "switch", &Map::new())
        .expect("published")
        .expect("flipped");
    assert_eq!(
        services.read("settings:settings.appearance").unwrap()["high_contrast"],
        json!(true)
    );
    let published = setting(&services.resources(), "appearance.high_contrast").unwrap();
    assert_eq!(published.actions[0].state.badge.as_deref(), Some("On"));
}

#[test]
fn turning_off_a_switch_that_guards_the_lock_asks_first() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    let resource = setting_resource("power.lock_on_sleep");
    let published = setting(&services.resources(), "power.lock_on_sleep").unwrap();
    assert_eq!(
        published.actions[0].confirmation_for_json(&Map::new()),
        ConfirmationPolicy::Confirm
    );
    assert_eq!(
        services.invoke(&resource, "switch", &Map::new()),
        Some(Err(DoorError::ConfirmationRequired))
    );
    services
        .invoke_confirmed(&resource, "switch", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read("settings:settings.power").unwrap()["lock_on_sleep"],
        json!(false)
    );
}

#[test]
fn a_choice_offers_each_value_by_its_name() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    let published = setting(&services.resources(), "appearance.style").unwrap();
    let labels: Vec<&str> = published
        .children
        .iter()
        .map(|option| option.actions[0].label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["Light mode", "Dark mode", "Light by day, dark by night"]
    );
    services
        .invoke(
            &super::option_resource("appearance.style", "dark"),
            "choose",
            &Map::new(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read("settings:settings.appearance").unwrap()["mode"],
        json!("dark")
    );
    let published = setting(&services.resources(), "appearance.style").unwrap();
    let current: Vec<&str> = published
        .children
        .iter()
        .map(|option| &option.actions[0])
        .filter(|action| action.state.badge.is_some())
        .map(|action| action.label.as_str())
        .collect();
    assert_eq!(current, ["Dark mode"]);
}

#[test]
fn a_timer_takes_a_value_and_a_longer_lock_asks_first() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    let resource = setting_resource("power.lock_after");
    let five = arguments(json!({ "value": "1" }));
    services.invoke(&resource, "set", &five).unwrap().unwrap();
    assert_eq!(
        services.read("settings:settings.power").unwrap()["lock_minutes"],
        json!(1)
    );
    let never = arguments(json!({ "value": "0" }));
    assert_eq!(
        services.invoke(&resource, "set", &never),
        Some(Err(DoorError::ConfirmationRequired)),
        "never locking weakens the lock"
    );
    services
        .invoke_confirmed(&resource, "set", &never)
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read("settings:settings.power").unwrap()["lock_minutes"],
        json!(0)
    );
}

#[test]
fn the_keyboard_layout_is_chosen_through_its_sections_verb() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    let resource = setting_resource("keyboard.layout");
    services
        .invoke(&resource, "set", &arguments(json!({ "value": "de" })))
        .unwrap()
        .unwrap();
    let input = services.read("settings:settings.input").unwrap();
    assert_eq!(input["keyboard_layout"], json!("de"));
    assert!(
        input["keyboard_layouts"]
            .as_array()
            .is_some_and(|layouts| layouts.contains(&json!("de")))
    );
}

#[test]
fn a_change_made_elsewhere_is_published_on_the_next_poll() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut service = SettingsService::new("ada");
    service.poll();
    assert!(!service.poll(), "nothing changed");
    let mut other = SettingsService::new("bob");
    other
        .invoke(
            &setting_resource("appearance.high_contrast"),
            "switch",
            &Map::new(),
        )
        .unwrap();
    assert!(service.poll(), "the settings document moved");
}
