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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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

/// The shell's hint asks the section's verb where its surface names it (`settings/control-hints`),
/// and the change it makes is the person's: a hint the person was shown counts as shown.
#[test]
fn the_shells_hint_verbs_are_published_where_its_surface_names_them() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = crate::in_memory::scoped();
    let mut service = own_person("ada");
    let at = crate::address("settings.control-hints").expect("the hints section is published");
    // A setting the person changed first: the service publishes its sections again after every
    // change it runs, and the hints must still be among them.
    let _ = service
        .invoke(
            &section("appearance"),
            "set-dark-mode",
            &arguments(json!({ "on": false })),
        )
        .expect("the appearance changes");
    let resources = service.resources();
    let published = resources
        .iter()
        .find(|resource| resource.id.as_str() == at)
        .unwrap_or_else(|| {
            let ids: Vec<&str> = resources.iter().map(|r| r.id.as_str()).collect();
            panic!("{at} is among the settings the service publishes: {ids:?}")
        });
    for verb in ["learn-control-hint", "note-control-hint-shown"] {
        assert!(
            published.actions.iter().any(|spec| spec.id == verb),
            "{at} publishes `{verb}`"
        );
        assert!(
            service
                .resolve(&tilo_actions::ActionAddress::new(at.as_str(), verb))
                .is_some(),
            "the service resolves `{at}#{verb}`"
        );
    }
    let _ = service
        .invoke(
            &at,
            "note-control-hint-shown",
            &arguments(json!({ "hint": "open-from-search" })),
        )
        .expect("the hint counts as shown");
    assert_eq!(
        service.read(&at).expect("published")["learned"]["shown"]["open-from-search"],
        json!(1)
    );
}

#[test]
fn a_change_is_a_step_the_person_takes_back_through_the_section() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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

/// The keyboard's settings are a system section's, kept at the app's address
/// (`settings/apps/so.tilo.osk`): an app declares no preferences there, and a write that would
/// be one is refused rather than share the section's record. The section still sets its own
/// fields at that address.
#[test]
fn a_system_sections_app_address_takes_no_app_preferences() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = crate::in_memory::scoped();
    let mut services = services();
    let resource = "settings/apps/so.tilo.osk";
    let refusal = services
        .invoke(
            resource,
            "set-font-size",
            &arguments(json!({ "value": 13 })),
        )
        .expect("the address is answered")
        .expect_err("an app's preference is not written where a system section stands");
    let said = refusal.to_string();
    assert!(said.contains("settings.osk-sources"), "{said}");
    assert!(said.contains(resource), "{said}");
    assert!(said.contains("so.tilo.osk"), "{said}");
    assert_eq!(
        services
            .read(resource)
            .and_then(|reading| reading.get("font_size").cloned()),
        None,
        "nothing wrote the app's preference there"
    );
    // The section's own setter still changes it.
    services
        .invoke(
            resource,
            "set-layouts",
            &arguments(json!({ "layouts": ["qwerty", "azerty"], "active": "azerty" })),
        )
        .expect("answered")
        .expect("set");
    assert_eq!(
        services.read(resource),
        Some(json!({ "layout": "azerty", "layouts": ["qwerty", "azerty"] }))
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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
    let _settings = crate::in_memory::scoped();
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

/// Every verb a section declares is published where the section stands, and runs there: the
/// shell's own hint call is the section's `learn-control-hint`.
#[test]
fn a_sections_declared_verb_is_published_and_runs() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = crate::in_memory::scoped();
    let mut services = services();
    let resource = section("control_hints");
    let published = services
        .resources()
        .into_iter()
        .find(|kept| kept.id.as_str() == resource)
        .expect("the control hints section is published");
    let actions: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(
        actions.contains(&"learn-control-hint"),
        "{resource} publishes its own verb: {actions:?}"
    );
    services
        .invoke(
            &resource,
            "learn-control-hint",
            &arguments(json!({ "hint": "open-from-search" })),
        )
        .expect("published")
        .expect("learned");
    assert_eq!(
        services.read(&resource).unwrap()["learned"]["known"],
        json!(["open-from-search"])
    );
}

/// Every verb a section declares beyond its fields' setters is published where the section
/// stands, as the section's surface declares it: a view's own verb of a setting reaches it, as
/// the setting's setters do. The shell's control hint call is one of these.
#[test]
fn every_declared_verb_of_a_section_is_published_where_the_section_stands() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settings = crate::in_memory::scoped();
    let (places, roots) = crate::surface();
    let resources = services().resources();
    let mut checked = 0;
    for (place, root) in places.iter().zip(&roots) {
        let address = root.published.replace(crate::DEVICE, DEVICE);
        let Some(resource) = resources
            .iter()
            .find(|resource| resource.id.as_str() == address)
        else {
            panic!(
                "{address}, which {} is published at, is one of the resources",
                place.kind
            );
        };
        for verb in &place.verbs {
            // A field's setter is published from the field; a direct content edit and a
            // migration are the record's history, not actions; undo and redo are the person's
            // steps, published beside the section.
            if verb.id.starts_with("set-")
                || tilo_actions::surface::CONTENT_VERBS.contains(&verb.id.as_str())
                || matches!(verb.id.as_str(), "undo" | "redo")
            {
                continue;
            }
            assert!(
                resource.actions.iter().any(|action| action.id == verb.id),
                "{address} publishes {}#{}, as its surface declares it",
                place.kind,
                verb.id
            );
            checked += 1;
        }
    }
    assert!(checked > 20, "{checked} declared verbs are published");
}

/// A painter module for a theme bundle: it declares the SDK's parameters and draws a triangle
/// whose apex is `dx` from the centre, so a changed module draws a visibly other frame. It is the
/// module the bundle's document names, compiled by the daemon into the device's store at install.
fn triangle_module(dx: f64) -> Vec<u8> {
    use ui::part_paint::{Brush, PaintedPart, ParamSpec, PartScene};

    let mut part = PartScene::new();
    let mut triangle = ui::kurbo::BezPath::new();
    triangle.move_to((60.0 + dx, 0.0));
    triangle.line_to((108.0, 40.0));
    triangle.line_to((12.0, 40.0));
    triangle.close_path();
    part.fill(
        triangle,
        Brush::Solid(ui::peniko::Color::from_rgb8(0xd0, 0x30, 0x20)),
    );
    let answer = tilo_theme_sdk::wire::paint_answer(Some(&PaintedPart::new(part)));
    let params = tilo_theme_sdk::wire::params_answer(&[
        ParamSpec::new("levels", Some(4.0), 2.0..=16.0),
        ParamSpec::new("depth", Some(0.14), 0.0..=1.0),
        ParamSpec::new("shadow", Some(3.0), 0.0..=10.0),
    ]);
    let data = |bytes: &[u8]| {
        let mut text = String::new();
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(text, "\\{byte:02x}");
        }
        text
    };
    let params = data(&params);
    let answer = data(&answer);
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (data (i32.const 8192) "{answer}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (result i64) (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {params_len})))
                    (else (i64.or (i64.shl (i64.const 8192) (i64.const 32)) (i64.const {answer_len}))))))"#,
        params_len = params.len() / 3,
        answer_len = answer.len() / 3,
    ))
    .expect("the module reads")
}

/// The theme bundle of the package `PACKAGE`: Bauhaus's document naming `SET`, and that set's
/// module beside it. The bytes are what the device's daemon compiles when the package arrives.
fn theme_bundle(dir: &std::path::Path) -> Vec<u8> {
    const DOCUMENT: &str = include_str!("../../../../ui/themes/bauhaus/theme.json");
    const SET: &str = "test.theme-triangle";
    std::fs::create_dir_all(dir).expect("the payload tree");
    let document = DOCUMENT.replace("\"bauhaus\"", &format!("{SET:?}"));
    std::fs::write(dir.join(ui::theme_bundle::DOCUMENT), document).expect("the document");
    let bytes = triangle_module(0.0);
    std::fs::write(dir.join(format!("{SET}.wasm")), &bytes).expect("the set's module");
    bytes
}

/// Lay the package `PACKAGE` out as an install leaves it: its payload tree under the device's
/// user store, a row in the store's registry, and the machine code the daemon compiled for the
/// set it carries in the store the launcher grants read-only.
fn install_theme_package() -> (std::path::PathBuf, Vec<u8>) {
    let store = package_store::layout::default_user_root().expect("the device's store");
    let payload = store.join("pkgs").join("theme-triangle");
    let bytes = theme_bundle(&payload);
    tilo_wasm_aot::Cache::at(
        tilo_paths::data_dir()
            .expect("the test home")
            .join("tilolang-compiled"),
    )
    .expect("the device compiles")
    .ensure(&bytes)
    .expect("the set compiles");
    let mut registry =
        package_store::Registry::load(&store.join("registry.toml")).expect("the registry reads");
    let mut row = package_store::InstalledPackage::detached(
        PACKAGE,
        String::new(),
        payload.clone(),
        payload.join("manifest.toml"),
    );
    "1.0.0".clone_into(&mut row.version);
    registry.upsert(row);
    registry
        .save(&store.join("registry.toml"))
        .expect("the registry is written");
    (payload, bytes)
}

const PACKAGE: &str = "so.tilo.theme.triangle";
const APPEARANCE: &str = "settings/appearance";

/// Applying an installed theme package: the appearance names it, its painters draw the frame on
/// the machine code the device compiled when the package arrived, and undo puts the previous
/// theme back.
#[test]
fn a_theme_package_is_applied_by_its_action_and_undo_returns_the_previous_theme() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let _settings = crate::in_memory::scoped();
    let (_payload, bytes) = install_theme_package();
    let mut services = services();

    // What the appearance says before: no package, and the theme its mode names.
    assert_eq!(
        services.read(&section("appearance")).expect("published")["theme_package"],
        Value::Null
    );
    let before = crate::prefs::AppearancePrefs::default()
        .user_document()
        .expect("the stock theme");

    // The store the daemon wrote holds the set's machine code before anything applies it: what
    // the install compiled, and the only thing the apply path reads.
    assert!(
        tilo_wasm_aot::kept_in(
            &tilo_paths::data_dir()
                .expect("the test home")
                .join("tilolang-compiled"),
            &bytes
        )
        .is_some(),
        "the install compiled the set into the device's store"
    );

    let lookups = tilo_wasm_aot::lookups();
    services
        .invoke(
            APPEARANCE,
            "apply-theme",
            &arguments(json!({ "package": PACKAGE })),
        )
        .expect("published")
        .expect("applied");
    assert_eq!(
        services.read(APPEARANCE).expect("published")["theme_package"],
        json!(PACKAGE),
        "the person's appearance names the package"
    );
    assert!(
        tilo_wasm_aot::lookups() > lookups,
        "applying read the device's compiled store rather than compiling on that path"
    );

    // The theme the appearance now draws is the bundle's, and the set it ships is the one the
    // toolkit draws each part with: the frame a session draws is in the session's own test.
    let prefs: crate::prefs::AppearancePrefs =
        serde_json::from_value(services.read(APPEARANCE).expect("published")).expect("the section");
    let applied = prefs.user_document().expect("the applied theme");
    assert_ne!(applied.id, before.id, "the applied theme is another one");
    let theme = ui::Theme::from_document(&applied).expect("the bundle's theme draws");
    assert_eq!(
        theme.finish(ui::tokens::SurfaceRole::Control).painter(),
        "test.theme-triangle",
        "the set the bundle names is the one that draws the parts"
    );

    // Undo takes back the person's step: the appearance names no package again, and the theme
    // the mode shows is what every app draws.
    services
        .invoke(APPEARANCE, "undo", &Map::new())
        .expect("published")
        .expect("undone");
    let section = services.read(APPEARANCE).expect("published");
    assert_eq!(section["theme_package"], Value::Null);
    let prefs: crate::prefs::AppearancePrefs =
        serde_json::from_value(section).expect("the section reads");
    assert_eq!(
        prefs.user_document().map(|document| document.id),
        Some(before.id),
        "undo returns the previous theme"
    );
}

/// A package this device does not have is refused, naming what is missing, and the appearance is
/// unchanged: only an installed bundle can be applied.
#[test]
fn a_package_that_is_not_installed_is_refused() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let _settings = crate::in_memory::scoped();
    let mut services = services();
    let refused = services
        .invoke(
            APPEARANCE,
            "apply-theme",
            &arguments(json!({ "package": "so.tilo.theme.missing" })),
        )
        .expect("published");
    let refused = refused.expect_err("a package that is not installed is refused");
    assert!(
        format!("{refused}").contains("so.tilo.theme.missing"),
        "the refusal names the package: {refused}"
    );
    assert_eq!(
        services.read(APPEARANCE).expect("published")["theme_package"],
        Value::Null
    );
}

/// The person's own theme, as the appearance's own verb makes one: its id.
fn own_theme(service: &mut SettingsService) -> String {
    service
        .invoke(
            APPEARANCE,
            "new-theme",
            &arguments(json!({ "id": "mine", "base": "paper-dark" })),
        )
        .expect("the appearance's own verb makes a theme");
    "mine".to_owned()
}

/// Whether the theme `id` of the appearance is dark, as the section reads now.
fn theme_is_dark(service: &SettingsService, id: &str) -> bool {
    let read = service.read(APPEARANCE).expect("the appearance reads");
    read["themes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|theme| theme["id"] == json!(id))
        .and_then(|theme| theme["value"]["dark"].as_bool())
        .unwrap_or_else(|| panic!("the theme {id} is one of the person's: {read}"))
}

/// A read-only field is written by the actions the section declares, never by a setter: a setter
/// that takes an edit inside `themes` cannot reach `themes.*.dark`, and the field keeps its value
/// where the write is refused.
#[test]
fn a_write_to_a_read_only_field_is_refused_and_the_field_keeps_its_value() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let _settings = crate::in_memory::scoped();
    let mut service = own_person("ada");
    let id = own_theme(&mut service);
    let dark = theme_is_dark(&service, &id);
    let write = service.invoke(
        APPEARANCE,
        "set-themes",
        &arguments(json!({
            "edit": {
                "path": [{ "item": id }, { "field": "dark" }],
                "op": "set",
                "value": !dark,
            }
        })),
    );
    let Err(refused) = write else {
        panic!("the theme's polarity is the section's own verb's to change")
    };
    assert!(
        format!("{refused}").contains("changed only by the record's actions"),
        "the refusal says the field is the section's actions' to change: {refused}"
    );
    assert_eq!(
        theme_is_dark(&service, &id),
        dark,
        "the read-only field keeps its value"
    );
}

/// The field's own declared verb still derives it: `switch-polarity` sets the theme's `dark`, and
/// the write is the section's own, not a setter's.
#[test]
fn a_sections_own_verb_changes_the_read_only_field_it_derives() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let _settings = crate::in_memory::scoped();
    let mut service = own_person("ada");
    let id = own_theme(&mut service);
    let dark = theme_is_dark(&service, &id);
    service
        .invoke(
            APPEARANCE,
            "switch-polarity",
            &arguments(json!({ "theme": id, "dark": !dark })),
        )
        .expect("the section's own verb switches the polarity");
    assert_eq!(theme_is_dark(&service, &id), !dark);
}
