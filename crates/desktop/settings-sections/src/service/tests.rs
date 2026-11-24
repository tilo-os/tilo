use serde_json::{Map, Value, json};
use settings_store::index;
use tilo_actions::{ActionSpec, ApiError, ConfirmationPolicy, ResourceSpec};
use tilo_service::{Service, Services};

use super::SettingsService;
use super::tablet_choices;

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// This device's key, in lowercase hex, as the session names it.
const DEVICE: &str = "abababababababababababababababababababababababababababababababab";

/// Where the session publishes the section `section` on this device.
fn section(section: &str) -> String {
    settings_store::placement::address(section, None, DEVICE).id
}

/// The verb that changes the setting `id`, as its section publishes it.
fn verb(resources: &[ResourceSpec], id: &str) -> Option<(String, ActionSpec)> {
    let at = index::setting(id)?.at?;
    let section = resources
        .iter()
        .find(|resource| resource.id.as_str() == at.resource(DEVICE))?;
    let spec = section
        .actions
        .iter()
        .find(|spec| spec.id == at.action())?
        .clone();
    Some((at.resource(DEVICE), spec))
}

/// Settings of this test's own, kept in the person's and the device owner's workspaces in memory
/// with their history, which no other test sees: what the service reads and writes while the
/// scope lives.
fn held() -> settings_store::SourceScope {
    let settings = std::sync::Arc::new(settings_store::workspace::WorkspaceSettings::new());
    settings.attach(settings_store::in_memory::Workspaces::new());
    settings_store::scoped_source(Some(settings))
}

/// The settings service, as the session starts it for `person`.
fn own_person(person: &str) -> SettingsService {
    ui::framework::read_settings_on(DEVICE);
    let mut service = SettingsService::new();
    service.own_person(person, &[]);
    service
}

fn services() -> Services {
    let mut services = Services::default();
    services.add(Box::new(own_person("ada")));
    services
}

/// Every setting people look for by name that is changed where it stands is
/// its section's own verb, named as people name the setting, found by every
/// word they use for it, and said to be on its page; no section publishes an
/// action that sets any field at all.
#[test]
fn every_setting_is_its_sections_verb_named_as_people_name_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let resources = services().resources();
    let mut named = 0;
    for setting in index::SETTINGS {
        let Some(at) = setting.at else {
            continue;
        };
        let (_, spec) = verb(&resources, setting.id).unwrap_or_else(|| {
            panic!(
                "{} is changed by {}#{}, which is published",
                setting.id,
                at.resource(DEVICE),
                at.action()
            )
        });
        assert_eq!(
            spec.label,
            super::NAMES.t(&setting.name_key()),
            "{}",
            setting.id
        );
        assert_eq!(spec.keywords, setting.search_words(), "{}", setting.id);
        assert_eq!(
            spec.description,
            index::page(setting.page).map(|page| super::NAMES.t(&page.title_key())),
            "{}",
            setting.id
        );
        named += 1;
    }
    assert!(named > 20, "{named} settings are changed where they stand");
    for section in resources
        .iter()
        .filter(|resource| resource.id.as_str() != super::agent_models::RESOURCE)
    {
        for generic in ["field-edit", "apply", "group"] {
            assert!(
                section.actions.iter().all(|spec| spec.id != generic),
                "{} publishes `{generic}`",
                section.id.as_str()
            );
        }
    }
    let (resource, spec) = verb(&resources, "appearance.high_contrast").unwrap();
    assert_eq!(
        (resource.as_str(), spec.id.as_str()),
        ("settings/appearance", "set-high-contrast")
    );
}

#[test]
fn a_switch_says_whether_it_is_on_and_is_set_through_its_setter() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let (resource, switch) = verb(&services.resources(), "appearance.high_contrast")
        .expect("high contrast is changed where it stands");
    assert_eq!(switch.label, "High contrast");
    assert_eq!(switch.state.badge.as_deref(), Some("Off"));

    services
        .invoke(&resource, &switch.id, &arguments(json!({ "value": true })))
        .expect("published")
        .expect("set");
    assert_eq!(
        services.read(&section("appearance")).unwrap()["high_contrast"],
        json!(true)
    );
    let (_, switch) = verb(&services.resources(), "appearance.high_contrast").unwrap();
    assert_eq!(switch.state.badge.as_deref(), Some("On"));
}

#[test]
fn turning_off_a_switch_that_guards_the_lock_asks_first() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let (resource, switch) = verb(&services.resources(), "power.lock_on_sleep").unwrap();
    let off = arguments(json!({ "value": false }));
    assert_eq!(
        switch.confirmation_for_json(&off),
        ConfirmationPolicy::Confirm
    );
    assert_eq!(
        switch.confirmation_for_json(&arguments(json!({ "value": true }))),
        ConfirmationPolicy::None,
        "keeping the lock asks nothing"
    );
    assert_eq!(
        services.invoke(&resource, &switch.id, &off),
        Some(Err(ApiError::ConfirmationRequired))
    );
    services
        .invoke_confirmed(&resource, &switch.id, &off)
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(&section("power")).unwrap()["lock_on_sleep"],
        json!(false)
    );
}

#[test]
fn a_choice_names_each_value_and_says_which_it_holds() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let (resource, choice) = verb(&services.resources(), "appearance.style").unwrap();
    assert_eq!(choice.id, "set-mode");
    let value = choice
        .params()
        .iter()
        .find(|param| param.name == "value")
        .expect("the value it takes");
    for (value_name, label) in [
        ("light", "Light mode"),
        ("dark", "Dark mode"),
        ("automatic", "Light by day, dark by night"),
    ] {
        assert_eq!(value.choice_label(value_name), Some(label), "{value_name}");
    }
    services
        .invoke(
            &resource,
            &choice.id,
            &arguments(json!({ "value": "dark" })),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(&section("appearance")).unwrap()["mode"],
        json!("dark")
    );
    let (_, choice) = verb(&services.resources(), "appearance.style").unwrap();
    assert_eq!(choice.state.badge.as_deref(), Some("Dark mode"));
}

#[test]
fn a_timer_takes_a_value_and_a_longer_lock_asks_first() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let (resource, timer) = verb(&services.resources(), "power.lock_after").unwrap();
    assert_eq!(timer.id, "set-lock-minutes");
    services
        .invoke(&resource, &timer.id, &arguments(json!({ "value": 1 })))
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(&section("power")).unwrap()["lock_minutes"],
        json!(1)
    );
    let never = arguments(json!({ "value": 0 }));
    assert_eq!(
        services.invoke(&resource, &timer.id, &never),
        Some(Err(ApiError::ConfirmationRequired)),
        "never locking weakens the lock"
    );
    services
        .invoke_confirmed(&resource, &timer.id, &never)
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(&section("power")).unwrap()["lock_minutes"],
        json!(0)
    );
}

#[test]
fn the_keyboard_layout_is_chosen_through_its_sections_verb() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let (resource, layout) = verb(&services.resources(), "keyboard.layout").unwrap();
    assert_eq!(layout.id, "choose-layout");
    assert_eq!(layout.label, "Keyboard layout");
    services
        .invoke(&resource, &layout.id, &arguments(json!({ "layout": "de" })))
        .unwrap()
        .unwrap();
    let input = services.read(&section("input")).unwrap();
    assert_eq!(input["keyboard_layout"], json!("de"));
    assert!(
        input["keyboard_layouts"]
            .as_array()
            .is_some_and(|layouts| layouts.contains(&json!("de")))
    );
}

/// The keyboard sources are one value the section sets whole, and the source
/// typed with is kept while it is one of them: what a device whose session is
/// not running chooses them by.
#[test]
fn the_keyboard_sources_are_chosen_as_one_value() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let input = section("input");
    services
        .invoke(
            &input,
            "set-keyboard-layouts",
            &arguments(json!({ "value": ["fr", "de"] })),
        )
        .unwrap()
        .unwrap();
    let chosen = services.read(&input).unwrap();
    assert_eq!(chosen["keyboard_layouts"], json!(["fr", "de"]));
    assert_eq!(
        chosen["keyboard_layout"],
        json!("fr"),
        "the source typed with is one of the list; when it was not, the first is"
    );
    services
        .invoke(
            &input,
            "set-keyboard-layouts",
            &arguments(json!({ "value": ["de", "fr"] })),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(&input).unwrap()["keyboard_layout"],
        json!("fr"),
        "a list that still holds the source typed with keeps it"
    );
    services
        .invoke(
            &input,
            "set-keyboard-layout",
            &arguments(json!({ "value": "de" })),
        )
        .unwrap()
        .unwrap();
    let typed = services.read(&input).unwrap();
    assert_eq!(typed["keyboard_layout"], json!("de"));
    assert_eq!(
        typed["keyboard_layouts"],
        json!(["de", "fr"]),
        "the chosen source stays in the list it is one of"
    );
}

/// Set `appearance.high_contrast` on, as `service`'s person.
fn high_contrast_on(service: &mut SettingsService) {
    service
        .invoke(
            &section("appearance"),
            "set-high-contrast",
            &arguments(json!({ "value": true })),
        )
        .expect("set");
}

#[test]
fn a_change_is_a_step_the_person_takes_back_through_the_section() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let section = &section("appearance");
    let published = services
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == section)
        .expect("the appearance section is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(actions.contains(&"undo"), "a control can undo: {actions:?}");
    assert!(actions.contains(&"redo"), "a control can redo: {actions:?}");

    services
        .invoke(
            section,
            "set-high-contrast",
            &arguments(json!({ "value": true })),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(section).unwrap()["high_contrast"],
        json!(true)
    );

    services
        .invoke(section, "undo", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(section).unwrap()["high_contrast"],
        json!(false),
        "the step is taken back through the section's own history"
    );
    services
        .invoke(section, "redo", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read(section).unwrap()["high_contrast"],
        json!(true),
        "and made again"
    );
}

#[test]
fn someone_else_cannot_take_back_a_step_that_is_not_theirs() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut ada = own_person("ada");
    let mut bob = own_person("bob");
    high_contrast_on(&mut bob);
    let section = &section("appearance");
    assert!(
        ada.invoke(section, "undo", &Map::new()).is_err(),
        "bob's step is bob's to take back"
    );
    assert_eq!(
        ada.read(section).unwrap()["high_contrast"],
        json!(true),
        "ada's undo changed nothing"
    );
    bob.invoke(section, "undo", &Map::new()).expect("undone");
    assert_eq!(bob.read(section).unwrap()["high_contrast"], json!(false));
}

#[test]
fn a_change_made_elsewhere_is_published_on_the_next_poll() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut service = own_person("ada");
    service.poll();
    assert!(!service.poll(), "nothing changed");
    let mut other = own_person("bob");
    high_contrast_on(&mut other);
    assert!(service.poll(), "the settings document moved");
}

#[test]
fn the_rotation_lock_is_an_action_of_the_display_section() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    services
        .invoke(
            &section("display"),
            "set-rotation-lock",
            &arguments(json!({ "locked": true })),
        )
        .expect("published")
        .expect("locked");
    assert_eq!(
        services.read(&section("display")).unwrap()["rotation_lock"],
        json!(true)
    );
    assert!(
        settings_store::hardware::ROTATION_LOCK.get(),
        "the session's rotation reads the same setting"
    );
}

/// What this service publishes is the sections, each where its holder keeps it
/// (`settings/<section>`, `devices/<device>/settings/<section>`), and nothing else's resources.
#[test]
fn the_settings_it_publishes_are_the_sections() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let published = services().resources();
    for resource in &published {
        assert!(
            super::keeps(resource.id.as_str()),
            "{} is one of the settings",
            resource.id.as_str()
        );
    }
    assert!(
        published
            .iter()
            .any(|resource| resource.id.as_str() == section("appearance")),
        "the appearance section is published"
    );
    assert!(!super::keeps("sound"), "the sound a session keeps is none");
    assert!(
        !super::keeps("settings.audio"),
        "a kind alone names no section's resource"
    );
}

/// An app's own preferences stand where its holder keeps them (`settings/apps/<app>`), each set
/// by its own verb, the first time too, and setting one keeps the others it holds.
#[test]
fn an_apps_preferences_are_set_one_by_one_by_their_own_verbs() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let resource = settings_store::placement::address(
        settings_store::placement::APP_PERSONAL_SECTION,
        Some("so.tilo.ebook"),
        DEVICE,
    )
    .id;
    assert_eq!(resource, "settings/apps/so.tilo.ebook");
    for (action, value) in [("set-keep-open", json!(true)), ("set-page-size", json!(12))] {
        services
            .invoke(&resource, action, &arguments(json!({ "value": value })))
            .expect("answered")
            .expect("set");
    }
    assert_eq!(
        services.read(&resource),
        Some(json!({ "keep_open": true, "page_size": 12 }))
    );
    let published = services
        .resources()
        .into_iter()
        .find(|published| published.id.as_str() == resource)
        .expect("the app's preferences are published once it holds one");
    for setter in ["set-keep-open", "set-page-size"] {
        assert!(
            published.actions.iter().any(|spec| spec.id == setter),
            "{setter} is published"
        );
    }
    services
        .invoke(
            &resource,
            "set-keep-open",
            &arguments(json!({ "value": false })),
        )
        .expect("answered")
        .expect("set");
    assert_eq!(
        services.read(&resource),
        Some(json!({ "keep_open": false, "page_size": 12 }))
    );
}

/// Two providers the person set up: one on this device with two models, and
/// one hosted with one, the device's the one agents answer with.
fn configured() -> agent_provider_config::ProviderConfig {
    use agent_provider_config::{
        EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
    };
    let mut local = ProviderProfile::local_vllm();
    local.id = "local".into();
    local.label = "This machine".into();
    local.models = vec![ModelDescriptor::new("small"), ModelDescriptor::new("large")];
    local.default_model = Some("small".into());
    let mut hosted = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    hosted.id = "hosted".into();
    hosted.label = "Work".into();
    hosted.models = vec![ModelDescriptor::new("claude")];
    hosted.default_model = Some("claude".into());
    let _ = EndpointLocation::Hosted;
    let config = ProviderConfig {
        default_profile: Some(local.id.clone()),
        profiles: vec![local, hosted],
        ..ProviderConfig::default()
    };
    config.save().unwrap();
    ProviderConfig::load()
}

/// The agent-models resource reads the providers the person set up, each
/// with its models and the one agents use from it, and which provider and
/// model agents answer with now.
#[test]
fn the_agent_model_reading_lists_a_providers_models_and_the_one_in_use() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    configured();
    let services = services();
    let reading = services
        .read(super::agent_models::RESOURCE)
        .expect("the agent model choice is published");
    let providers = reading["providers"].as_array().expect("providers");
    let local = providers
        .iter()
        .find(|provider| provider["id"] == json!("local"))
        .expect("the device's provider is listed");
    let models: Vec<&str> = local["models"]
        .as_array()
        .expect("models")
        .iter()
        .map(|model| model["id"].as_str().expect("a model id"))
        .collect();
    assert_eq!(
        models,
        ["small", "large"],
        "every model it offers is listed"
    );
    assert_eq!(
        local["model"],
        json!("small"),
        "the model agents use from it"
    );
    assert_eq!(local["enabled"], json!(true));
    assert_eq!(local["label"], json!("This machine"));
    assert!(
        local.get("endpoint").is_none() && local.get("credential").is_none(),
        "no endpoint or credential is read: {local}"
    );
    assert_eq!(
        reading["in_use"],
        json!({ "provider": "local", "model": "small", "model_label": "small" }),
        "the provider and model agents answer with now"
    );
}

/// "Use model" makes that provider's model the one agents use and its
/// provider the one they answer with, so `ProviderSelection::from_config`
/// resolves to it; the change is one step the person takes back across both
/// provider sections.
#[test]
fn use_model_changes_what_agents_answer_with_and_is_undone_as_one_step() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    configured();
    let mut services = services();
    let resolve = || {
        // What `tilo_agent::providers::ProviderSelection::from_config` reads: the
        // active profile and its default model.
        let config = agent_provider_config::ProviderConfig::load();
        let profile = config
            .active_profile()
            .expect("agents answer with a provider");
        (profile.id.clone(), profile.default_model.clone())
    };
    assert_eq!(resolve(), ("local".into(), Some("small".into())));
    let resource = super::agent_models::RESOURCE;
    services
        .invoke(
            resource,
            "use-model",
            &arguments(json!({ "provider": "hosted", "model": "claude" })),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        resolve(),
        ("hosted".into(), Some("claude".into())),
        "agents answer with the provider and model the person chose"
    );
    assert_eq!(
        services.read(resource).unwrap()["in_use"],
        json!({ "provider": "hosted", "model": "claude", "model_label": "claude" })
    );
    services
        .invoke(resource, "undo-model", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        resolve(),
        ("local".into(), Some("small".into())),
        "the choice is taken back across the device's provider section and the person's"
    );
    services
        .invoke(resource, "redo-model", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        resolve(),
        ("hosted".into(), Some("claude".into())),
        "and made again"
    );
}

/// A pad's button takes any of the desktop's shortcuts, and each ring and strip keeps what it
/// does in every mode: the input section holds both, and reads them back as the choices its verbs
/// take.
#[test]
fn a_pads_buttons_and_dials_are_kept_by_the_choice_its_verb_takes() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = held();
    let mut services = services();
    let device = "056a:0374:Wacom Intuos Pro M Pad";
    let input = section("input_device");

    services
        .invoke(
            &input,
            "set-pad-button",
            &arguments(json!({
                "device": device,
                "button": 2,
                "action": "shortcut:focus-previous-window",
            })),
        )
        .unwrap()
        .unwrap();
    // The second mode of the first ring, counted from zero as the mapping is.
    services
        .invoke(
            &input,
            "set-pad-dial",
            &arguments(json!({
                "device": device,
                "dial": "ring",
                "index": 0,
                "mode": 1,
                "action": "volume",
            })),
        )
        .unwrap()
        .unwrap();

    let stored = services.read(&input).unwrap();
    let tablet = &stored["tablets"][device];
    assert_eq!(
        tablet["pad_buttons"]["2"],
        json!("focus_previous_window"),
        "the shortcut the catalogue names is the one the section keeps"
    );
    assert_eq!(
        tablet["pad_rings"]["0"],
        json!(["scroll", "volume"]),
        "the second mode holds what it was given, and the first keeps scrolling"
    );

    let choices = services.read(tablet_choices::RESOURCE).unwrap();
    let ids = &choices["tablets"][device];
    assert_eq!(
        ids["pad_buttons"]["2"],
        json!("shortcut:focus-previous-window"),
        "the pane names the choice by the catalogue's own id"
    );
    assert_eq!(ids["pad_rings"]["0"], json!(["scroll", "volume"]));

    let refused = services.invoke(
        &input,
        "set-pad-button",
        &arguments(json!({
            "device": device,
            "button": 0,
            "action": "shortcut:not-a-shortcut",
        })),
    );
    assert!(
        refused.is_some_and(|done| done.is_err()),
        "a shortcut the desktop does not have is refused, not stored"
    );
}
