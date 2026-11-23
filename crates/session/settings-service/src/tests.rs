use serde_json::{Map, Value, json};
use tilo_actions::{ApiError, ConfirmationPolicy, ResourceSpec};
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

/// The settings service, as the session starts it for `person`.
fn own_person(person: &str) -> SettingsService {
    let mut service = SettingsService::new();
    service.own_person(person, &[]);
    service
}

fn services() -> Services {
    let mut services = Services::default();
    services.add(Box::new(own_person("ada")));
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
        Some(Err(ApiError::ConfirmationRequired))
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
        Some(Err(ApiError::ConfirmationRequired)),
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
fn a_change_is_a_step_the_person_takes_back_through_the_section() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    let section = "settings:settings.appearance";
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
            &setting_resource("appearance.high_contrast"),
            "switch",
            &Map::new(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read("settings:settings.appearance").unwrap()["high_contrast"],
        json!(true)
    );

    services
        .invoke(section, "undo", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read("settings:settings.appearance").unwrap()["high_contrast"],
        json!(false),
        "the step is taken back through the section's own history"
    );
    services
        .invoke(section, "redo", &Map::new())
        .unwrap()
        .unwrap();
    assert_eq!(
        services.read("settings:settings.appearance").unwrap()["high_contrast"],
        json!(true),
        "and made again"
    );
}

#[test]
fn someone_else_cannot_take_back_a_step_that_is_not_theirs() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut ada = own_person("ada");
    let mut bob = own_person("bob");
    bob.invoke(
        &setting_resource("appearance.high_contrast"),
        "switch",
        &Map::new(),
    )
    .expect("flipped");
    let section = "settings:settings.appearance";
    assert!(
        ada.invoke(section, "undo", &Map::new()).is_err(),
        "bob's step is bob's to take back"
    );
    assert_eq!(
        ada.read("settings:settings.appearance").unwrap()["high_contrast"],
        json!(true),
        "ada's undo changed nothing"
    );
    bob.invoke(section, "undo", &Map::new()).expect("undone");
    assert_eq!(
        bob.read("settings:settings.appearance").unwrap()["high_contrast"],
        json!(false)
    );
}

#[test]
fn a_change_made_elsewhere_is_published_on_the_next_poll() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut service = own_person("ada");
    service.poll();
    assert!(!service.poll(), "nothing changed");
    let mut other = own_person("bob");
    other
        .invoke(
            &setting_resource("appearance.high_contrast"),
            "switch",
            &Map::new(),
        )
        .unwrap();
    assert!(service.poll(), "the settings document moved");
}

#[test]
fn the_rotation_lock_is_an_action_of_the_display_section() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut services = services();
    services
        .invoke(
            "settings:settings.display",
            "set-rotation-lock",
            &arguments(json!({ "locked": true })),
        )
        .expect("published")
        .expect("locked");
    assert_eq!(
        services.read("settings:settings.display").unwrap()["rotation_lock"],
        json!(true)
    );
    assert!(
        settings_store::hardware::ROTATION_LOCK.get(),
        "the session's rotation reads the same setting"
    );
}

/// What this service publishes is the sections and the index over them, by the ids a host that
/// answers for them names: its sections as `settings:<kind>`, and nothing else's resources.
#[test]
fn the_settings_it_publishes_are_the_sections_and_the_index() {
    let _home = tilo_test_env::TestHome::new().unwrap();
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
            .any(|resource| resource.id.as_str() == "settings:settings.appearance"),
        "the appearance section is published"
    );
    assert!(!super::keeps("sound"), "the sound a session keeps is none");
    assert!(
        !super::keeps("settings.audio"),
        "a kind alone names no section's resource"
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
    configured();
    let services = services();
    let reading = services
        .read("settings:settings.agent-models")
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
    let resource = "settings:settings.agent-models";
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

/// The resource's surface, as a view of another language reads it: written
/// beside this crate for `tilolang-tilo` to publish, the way
/// `device-surface.json` is for the device. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_agent_models_surface_file_is_what_this_crate_publishes() {
    let published = super::agent_models::surface();
    let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": [published] }))
        .expect("a surface writes as JSON");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-models-surface.json");
    let committed = std::fs::read_to_string(&path).ok();
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        committed.as_deref(),
        Some(text.as_str()),
        "crates/session/settings-service/agent-models-surface.json is not what this crate \
         publishes; regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

/// The providers the person set up, with the catalogue they can add from and each key's
/// state: a profile's key is read as where it stands, never as what it is.
#[test]
fn the_providers_reading_lists_the_ones_set_up_and_the_ones_to_add() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    configured();
    let services = services();
    let reading = services
        .read("settings:settings.agent-provider-setup")
        .expect("the providers are published");
    let providers = reading["providers"].as_array().expect("providers");
    let local = providers
        .iter()
        .find(|provider| provider["id"] == json!("local"))
        .expect("the device's provider is listed");
    assert_eq!(local["label"], json!("This machine"));
    assert_eq!(local["protocol"], json!("openai-compatible"));
    assert_eq!(local["model"], json!("small"));
    assert_eq!(local["in_use"], json!(true));
    assert_eq!(local["location"], json!("this-device"));
    assert_eq!(
        local["key"],
        json!("not-needed"),
        "a local endpoint that authenticates no one needs no key"
    );
    let hosted = providers
        .iter()
        .find(|provider| provider["id"] == json!("hosted"))
        .expect("the hosted provider is listed");
    assert_eq!(hosted["in_use"], json!(false));
    let presets = reading["presets"].as_array().expect("presets");
    assert!(
        presets
            .iter()
            .any(|preset| preset["slug"] == json!("openai")),
        "a provider that takes a key can be added: {presets:?}"
    );
    assert!(
        !presets
            .iter()
            .any(|preset| preset["authentication"] == json!("sign-in")),
        "one set up only by signing in is not offered where signing in is not"
    );
}

/// Adding a provider from the catalogue gives it a fresh id and the preset's name; removing it
/// takes it away again.
#[test]
fn a_provider_added_from_the_catalogue_is_removed_again() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    configured();
    let mut services = services();
    let resource = "settings:settings.agent-provider-setup";
    services
        .invoke(
            resource,
            "add-provider",
            &arguments(json!({ "preset": "openai" })),
        )
        .unwrap()
        .unwrap();
    let added = {
        let config = agent_provider_config::ProviderConfig::load();
        config
            .profiles
            .iter()
            .find(|profile| profile.label == "OpenAI")
            .cloned()
            .expect("the provider was added")
    };
    assert!(
        services.read(resource).expect("the providers read")["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .any(|provider| provider["id"] == json!(added.id.clone())),
        "the reading lists the provider that was added"
    );
    services
        .invoke(
            resource,
            "remove-provider",
            &arguments(json!({ "provider": added.id })),
        )
        .unwrap()
        .unwrap();
    assert!(
        !services.read(resource).expect("the providers read")["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .any(|provider| provider["id"] == json!(added.id.clone())),
        "the provider is gone"
    );
}

/// A provider's key is written to this device's vault, its profile points at it, and the
/// reading says it is stored and never what it is; forgetting it takes both away.
#[test]
fn a_providers_key_is_kept_and_forgotten() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    configured();
    let mut services = services();
    let resource = "settings:settings.agent-provider-setup";
    services
        .invoke(
            resource,
            "add-provider",
            &arguments(json!({ "preset": "openai" })),
        )
        .unwrap()
        .unwrap();
    let profile = agent_provider_config::ProviderConfig::load()
        .profiles
        .into_iter()
        .find(|profile| profile.label == "OpenAI")
        .expect("the provider was added");
    assert!(
        provider_key(&profile.id, &profile.endpoint).is_none(),
        "it has no key yet"
    );
    services
        .invoke(
            resource,
            "set-key",
            &arguments(json!({ "provider": profile.id, "key": "sk-test" })),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        provider_key(&profile.id, &profile.endpoint).as_deref(),
        Some("sk-test"),
        "the key is in the vault"
    );
    let shown = services.read(resource).expect("the providers read")["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["id"] == json!(profile.id.clone()))
        .cloned()
        .expect("the provider is listed");
    assert_eq!(shown["key"], json!("stored"));
    assert_eq!(shown["authentication"], json!("api-key"));
    assert!(
        !shown.to_string().contains("sk-test"),
        "the key never reaches the reading: {shown}"
    );
    services
        .invoke(
            resource,
            "clear-key",
            &arguments(json!({ "provider": profile.id })),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        provider_key(&profile.id, &profile.endpoint),
        None,
        "the key is gone from the vault"
    );
    assert_eq!(
        services.read(resource).expect("the providers read")["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["id"] == json!(profile.id.clone()))
            .expect("the provider is listed")["key"],
        json!("missing"),
    );
}

/// The key kept for the provider `id` at `endpoint` in this device's vault.
fn provider_key(id: &str, endpoint: &str) -> Option<String> {
    let (reference, authorization) = accounts::provider_api_key(id, endpoint).ok()?;
    accounts::ServiceCredentials::open()
        .resolve(&reference, &authorization)
        .ok()
        .flatten()
        .map(|key| key.to_string())
}

/// Changing a provider this device does not keep, adding one only a sign-in sets up, and
/// changing anything for a session that has not said who sits here are all refused.
#[test]
fn changing_a_provider_this_device_does_not_keep_is_refused() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    configured();
    let mut services = services();
    let resource = "settings:settings.agent-provider-setup";
    let refusal = services
        .invoke(
            resource,
            "remove-provider",
            &arguments(json!({ "provider": "nobody" })),
        )
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(refusal, ApiError::InvalidArguments(_)),
        "{refusal:?}"
    );
    let refusal = services
        .invoke(
            resource,
            "add-provider",
            &arguments(json!({ "preset": "chatgpt" })),
        )
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(refusal, ApiError::InvalidArguments(_)),
        "one set up by signing in is not added here: {refusal:?}"
    );

    // A service started without saying who sits here has no one to put on a change.
    let mut unowned = Services::default();
    unowned.add(Box::new(SettingsService::new()));
    let refusal = unowned
        .invoke(
            resource,
            "set-provider-enabled",
            &arguments(json!({ "provider": "local", "on": false })),
        )
        .unwrap()
        .unwrap_err();
    assert!(matches!(refusal, ApiError::Failed(_)), "{refusal:?}");
}

/// The resource's surface, written beside this crate for `tilolang-tilo` to publish.
/// `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_agent_provider_setup_surface_file_is_what_this_crate_publishes() {
    let published = super::providers::surface();
    let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": [published] }))
        .expect("a surface writes as JSON");
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent-providers-surface.json");
    let committed = std::fs::read_to_string(&path).ok();
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        committed.as_deref(),
        Some(text.as_str()),
        "crates/session/settings-service/agent-providers-surface.json is not what this crate \
         publishes; regenerate it with UPDATE_SNAPSHOTS=1"
    );
}

/// The shortcuts table's reading, as a view of another language reads it: written beside
/// this crate for `tilolang-tilo` to publish. `UPDATE_SNAPSHOTS=1` writes it.
#[test]
fn the_shortcuts_surface_file_is_what_this_crate_publishes() {
    let published = super::shortcuts_table::surface();
    let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": [published] }))
        .expect("a surface writes as JSON");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shortcuts-surface.json");
    let committed = std::fs::read_to_string(&path).ok();
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|value| value == "1") {
        std::fs::write(&path, &text).expect("the surface file is written");
        return;
    }
    assert_eq!(
        committed.as_deref(),
        Some(text.as_str()),
        "crates/session/settings-service/shortcuts-surface.json is not what this crate \
         publishes; regenerate it with UPDATE_SNAPSHOTS=1"
    );
}
