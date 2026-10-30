use super::app::{ProviderSettingsApp, ProviderSurface};
use super::{ProviderEffect, ProviderPanel, definition, widgets};
use agent_provider_config::management::AdminFuture;
use agent_provider_config::management::{CredentialStatus, ProviderAdmin, ProviderSnapshot};
use agent_provider_config::protocol::{ErrorKind, ModelError};
use agent_provider_config::{EndpointLocation, Interaction, ProviderConfig, ProviderProfile};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use ui::framework::testing::TestApp;
use ui::lock::MutexRecover as _;
use ui::tilo_resource::Effect;

fn mount(backend: Arc<FakeAdminBackend>) -> TestApp<ProviderSettingsApp> {
    let mut app = TestApp::for_person_with_lent(
        "provider-administrator",
        ui::models::ProviderAdministration::new(backend),
    )
    .unwrap();
    app.settle();
    app
}
fn host_effect(effect: ProviderEffect) -> ProviderEffect {
    effect
}
fn row_for(effect: &ProviderEffect) -> String {
    effect
        .arguments()
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || "model-providers.root".into(),
            |id| format!("settings.agent-provider:{id}"),
        )
}
fn invoke(
    app: &mut TestApp<ProviderSettingsApp>,
    effect: ProviderEffect,
) -> Result<(), ui::tilo_actions::DoorError> {
    let effect = host_effect(effect);
    let row = row_for(&effect);
    let mut arguments = effect.arguments();
    if row != "model-providers.root" {
        arguments.remove("provider");
        arguments.remove("on");
        if arguments.get("key").is_some_and(serde_json::Value::is_null) {
            arguments.remove("key");
        }
    }
    app.call_by(
        ui::tilo_actions::ActionInvoker::actor("provider-administrator"),
        &row,
        effect.id(),
        serde_json::Value::Object(arguments),
    )
    .map(|_| ())
}
fn run(app: &mut TestApp<ProviderSettingsApp>, effect: ProviderEffect) {
    if matches!(
        effect,
        ProviderEffect::RemoveProvider { .. }
            | ProviderEffect::RemoveProviderKey { .. }
            | ProviderEffect::RemoveLegacyProviderKey { .. }
    ) {
        app.call_confirmed_on(&row_for(&effect), effect.id(), serde_json::json!({}))
            .unwrap();
        app.settle();
    } else {
        invoke(app, effect).unwrap();
    }
}
fn with_panel<T>(
    app: &mut TestApp<ProviderSettingsApp>,
    f: impl FnOnce(&ProviderPanel<super::app::Bindings>) -> T,
) -> T {
    app.with_widget::<ProviderSurface, _>(|surface| f(&surface.panel))
        .unwrap()
}
fn config(app: &mut TestApp<ProviderSettingsApp>) -> ProviderConfig {
    with_panel(app, |panel| panel.provider_config().clone())
}
fn key_buffer(app: &mut TestApp<ProviderSettingsApp>, id: &str) -> Arc<Mutex<String>> {
    with_panel(app, |panel| panel.key_buffer(id))
}
fn assert_private(app: &mut TestApp<ProviderSettingsApp>) {
    with_panel(app, |panel| {
        assert!(
            panel.offline_config.profiles.is_empty(),
            "backend metadata must not be duplicated into local settings"
        );
        assert!(
            !serde_json::to_string(&panel.offline_config)
                .unwrap()
                .contains("private-key-draft")
        );
        assert!(
            !widgets::announced_text(widgets::page(panel.sections()).as_ref())
                .join(" ")
                .contains("private-key-draft")
        );
    });
    let document = app
        .read_document(|doc| serde_json::to_string(&ui::automerge::AutoSerde::from(doc)).unwrap());
    assert!(!document.contains("private-key-draft"));
    assert!(!format!("{:?}", app.history()).contains("private-key-draft"));
}
#[test]
fn standalone_without_authority_starts_empty_and_rejects_mutation() {
    let mut app = TestApp::<ProviderSettingsApp>::for_person("provider-administrator").unwrap();
    app.settle();
    assert!(config(&mut app).profiles.is_empty());
    assert!(
        app.effect(&ProviderEffect::AddProvider {
            preset: "anthropic".into()
        })
        .is_err()
    );
    assert!(config(&mut app).profiles.is_empty());
    assert_private(&mut app);
    assert_eq!(definition().app_id(), "so.tilo.model-providers");
}
#[test]
fn without_authority_the_window_says_who_sets_providers_up() {
    let mut app = TestApp::<ProviderSettingsApp>::for_person("provider-administrator").unwrap();
    app.settle();
    let tree = app.harness().inspection_json().unwrap();
    assert!(
        tree.contains("Model providers are set up by an administrator"),
        "{tree}"
    );
    assert!(tree.contains("every app that uses AI"), "{tree}");
    assert!(
        !tree.contains("Hosted and compatible providers"),
        "a section with nothing in it is not offered"
    );
}

/// An agent manages providers through the actions alone: it reads the
/// catalogue, and adds two accounts of one provider and an
/// Anthropic-compatible one.
#[test]
fn agents_list_presets_and_add_providers_through_actions() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    let agent = || ui::tilo_actions::ActionInvoker::actor("provider-administrator");
    let listed = app
        .call_by(
            agent(),
            "model-providers.root",
            ProviderEffect::LIST_PRESETS.id(),
            serde_json::json!({}),
        )
        .unwrap()
        .value
        .unwrap();
    let presets = listed["presets"].as_array().unwrap();
    let slugs: Vec<_> = presets
        .iter()
        .map(|preset| preset["preset"].as_str().unwrap())
        .collect();
    assert!(slugs.len() >= 40, "{slugs:?}");
    for slug in [
        "openrouter",
        "zai-anthropic",
        "anthropic-compatible",
        "vllm",
    ] {
        assert!(slugs.contains(&slug), "{slug}");
    }
    // A sign-in runs where a host offers it, not from this window.
    assert!(!slugs.contains(&"chatgpt"));
    let zai = presets
        .iter()
        .find(|preset| preset["preset"] == "zai-anthropic")
        .unwrap();
    assert_eq!(zai["protocol"], "anthropic");
    assert_eq!(zai["authentication"], "api-key");
    assert!(zai["docs"].as_str().unwrap().starts_with("https://"));

    for preset in ["openrouter", "openrouter", "zai-anthropic"] {
        app.call_by(
            agent(),
            "model-providers.root",
            ProviderEffect::ADD_PROVIDER.id(),
            serde_json::json!({ "preset": preset }),
        )
        .unwrap();
        app.settle();
    }
    let profiles = backend.state.lock_recover().config.profiles.clone();
    assert_eq!(profiles.len(), 3);
    assert_eq!(profiles[0].label, profiles[1].label);
    assert_ne!(profiles[0].id, profiles[1].id);
    assert_eq!(
        profiles[2].key_header,
        Some(agent_provider_config::KeyHeader::Bearer)
    );
    assert!(
        app.call_by(
            agent(),
            "model-providers.root",
            ProviderEffect::ADD_PROVIDER.id(),
            serde_json::json!({ "preset": "chatgpt" }),
        )
        .is_err()
    );
}

/// A generic provider pointed at a server on this device or the person's
/// network is that, not a hosted provider held to TLS; an internet host
/// stays hosted.
#[test]
fn a_typed_endpoint_takes_the_location_its_address_has() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            preset: "anthropic-compatible".into(),
        },
    );
    app.settle();
    let id = config(&mut app).profiles[0].id.clone();
    for (endpoint, location) in [
        (
            "http://127.0.0.1:8443/anthropic",
            EndpointLocation::ThisDevice,
        ),
        ("http://192.168.1.20:8000", EndpointLocation::PrivateNetwork),
        ("https://api.z.ai/api/anthropic", EndpointLocation::Hosted),
    ] {
        run(
            &mut app,
            ProviderEffect::SetProviderDetails {
                provider: id.clone(),
                label: "Mine".into(),
                endpoint: endpoint.into(),
                model: None,
            },
        );
        app.settle();
        let profile = backend.state.lock_recover().config.profiles[0].clone();
        assert_eq!(
            (profile.endpoint.as_str(), profile.location),
            (endpoint, location)
        );
        // A key sent over plain http on the person's network is said to be.
        let warned = with_panel(&mut app, |panel| {
            widgets::announced_text(widgets::page(panel.sections()).as_ref())
                .contains(&crate::i18n::t("agent-key-unencrypted"))
        });
        assert_eq!(
            warned,
            location == EndpointLocation::PrivateNetwork,
            "{endpoint}"
        );
    }
}

/// Test `provider`'s connection through its row's action, and require the
/// answer to be the result the action declares.
fn list_models_through_the_door(
    app: &mut TestApp<ProviderSettingsApp>,
    provider: &str,
) -> ui::tilo_actions::ActionOutput {
    let row = format!("settings.agent-provider:{provider}");
    let listed = app
        .call_by(
            ui::tilo_actions::ActionInvoker::actor("provider-administrator"),
            &row,
            ProviderEffect::LIST_PROVIDER_MODELS.id(),
            serde_json::json!({}),
        )
        .unwrap();
    let declared = with_panel(app, |panel| {
        panel
            .rows()
            .into_iter()
            .find(|candidate| candidate.id() == row)
            .unwrap()
            .into_resource()
            .actions
            .into_iter()
            .find(|spec| spec.id == ProviderEffect::LIST_PROVIDER_MODELS.id())
            .unwrap()
            .contract
            .result
            .clone()
    });
    declared.validate(&listed).unwrap();
    listed
}

/// A model named by id in a window that never listed its provider takes
/// what the provider's list says it can do, not only its family's.
#[test]
fn a_model_set_by_id_takes_its_capabilities_from_the_providers_list() {
    let backend = Arc::new(FakeAdminBackend::default());
    let listing = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent/provider-config/tests/fixtures/openrouter-models.json"
    ))
    .unwrap();
    let mut profile = agent_provider_config::presets::Preset::OPENROUTER.profile();
    profile.id = "openrouter-a".into();
    {
        let mut state = backend.state.lock_recover();
        state.listing = Some(agent_provider_config::transform::parse_models(&listing).unwrap());
        state.config.upsert(profile);
        state
            .keys
            .insert("openrouter-a".into(), CredentialStatus::Configured);
    }
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: "openrouter-a".into(),
            model: "moondream/moondream3-preview".into(),
        },
    );
    app.settle();
    let taken = backend.state.lock_recover().config.profiles[0].models[0].clone();
    // The family table does not know it; the list says it sees and takes
    // no tools.
    assert!(taken.vision && !taken.tool_calling, "{taken:?}");
    assert_eq!(
        taken.capabilities_from,
        agent_provider_config::CapabilitySource::Provider
    );
    // A model the provider does not list falls back to its family.
    run(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: "openrouter-a".into(),
            model: "vendor/gpt-4o-house".into(),
        },
    );
    app.settle();
    let typed = backend.state.lock_recover().config.profiles[0]
        .models
        .iter()
        .find(|model| model.id == "vendor/gpt-4o-house")
        .cloned()
        .unwrap();
    assert_eq!(
        typed.capabilities_from,
        agent_provider_config::CapabilitySource::Family
    );
}

/// Use model, through its action, shows the model in the form's Model ID
/// field; a field the person is editing keeps their draft.
#[test]
fn the_model_field_follows_the_model_chosen_unless_the_person_edits_it() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    app.harness().resize((1800, 6000)).unwrap();
    run(
        &mut app,
        ProviderEffect::AddProvider {
            preset: "openrouter".into(),
        },
    );
    app.settle();
    let id = config(&mut app).profiles[0].id.clone();
    let model_field = |app: &mut TestApp<ProviderSettingsApp>| {
        with_panel(app, |panel| {
            let profile = panel.provider_config().profiles[0].clone();
            let field = panel.field_buffers(&profile).model;
            let value = field.lock_recover().clone();
            value
        })
    };
    assert_eq!(model_field(&mut app), "");
    run(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: id.clone(),
            model: "vendor/model-a".into(),
        },
    );
    app.settle();
    assert_eq!(model_field(&mut app), "vendor/model-a");
    let shown = app.harness().inspection_json().unwrap();
    assert!(shown.contains("vendor/model-a"), "the field shows it");
    // The person types a model of their own; a later choice leaves it.
    with_panel(&mut app, |panel| {
        let profile = panel.provider_config().profiles[0].clone();
        *panel.field_buffers(&profile).model.lock_recover() = "typed-by-hand".into();
    });
    run(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: id,
            model: "vendor/model-b".into(),
        },
    );
    app.settle();
    assert_eq!(model_field(&mut app), "typed-by-hand");
}

/// An agent says what a model can do, through the action on its provider,
/// and is refused a model that calls no tools for an agent.
#[test]
fn agents_set_model_capabilities_and_cannot_plan_with_a_model_without_tools() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    run(
        &mut app,
        ProviderEffect::AddProvider {
            preset: "openrouter".into(),
        },
    );
    app.settle();
    // The key is written through its action, and is never read back.
    let provider = config(&mut app).profiles[0].id.clone();
    run(
        &mut app,
        ProviderEffect::SaveProviderKey {
            provider: provider.clone(),
            key: Some("private-key-draft".into()),
        },
    );
    app.settle();
    assert_private(&mut app);
    // The provider lists its models, answered as the action declares.
    let listed = list_models_through_the_door(&mut app, &provider);
    assert_eq!(listed.value.as_ref().unwrap()[0]["id"], "listed-a");
    app.settle();
    // One is taken, and is said to call no tools.
    run(
        &mut app,
        ProviderEffect::SetModelCapabilities {
            provider: provider.clone(),
            model: "listed-a".into(),
            vision: Some(true),
            tool_calling: Some(false),
        },
    );
    app.settle();
    let model = backend.state.lock_recover().config.profiles[0].models[0].clone();
    assert_eq!(model.id, "listed-a");
    assert_eq!(model.context_window, Some(200_000));
    assert!(model.vision && !model.tool_calling);
    assert_eq!(
        model.capabilities_from,
        agent_provider_config::CapabilitySource::Person
    );
    let refused = app.effect(&ProviderEffect::SetAgentModel {
        agent: "writer".into(),
        provider: Some(provider.clone()),
        model: Some("listed-a".into()),
    });
    assert_eq!(
        refused.err().map(|error| error.to_string()),
        Some(crate::i18n::t("agent-model-no-tools"))
    );
    run(
        &mut app,
        ProviderEffect::SetModelCapabilities {
            provider: provider.clone(),
            model: "listed-a".into(),
            vision: None,
            tool_calling: Some(true),
        },
    );
    app.settle();
    app.effect(&ProviderEffect::SetAgentModel {
        agent: "writer".into(),
        provider: Some(provider.clone()),
        model: Some("listed-a".into()),
    })
    .unwrap();
    app.settle();
    let config = backend.state.lock_recover().config.clone();
    assert_eq!(config.agent_models["writer"].model, "listed-a");
    // While writer plans with it, the model cannot be said to call no tools.
    let refused = invoke(
        &mut app,
        ProviderEffect::SetModelCapabilities {
            provider: provider.clone(),
            model: "listed-a".into(),
            vision: None,
            tool_calling: Some(false),
        },
    );
    let refused = format!("{:?}", refused.unwrap_err());
    assert!(refused.contains("writer"), "{refused}");
    // Giving the model back to its family table forgets the person's answer.
    run(
        &mut app,
        ProviderEffect::SetModelCapabilities {
            provider,
            model: "listed-a".into(),
            vision: None,
            tool_calling: None,
        },
    );
    app.settle();
    let model = backend.state.lock_recover().config.profiles[0].models[0].clone();
    assert!(!model.capabilities_set_by_person());
}

/// Pictures of the panel for review, rendered headless (lavapipe) into
/// `target/provider-shots`: the catalogue, narrowed by its filter, and a
/// provider's form with the models it listed and what each can do.
#[test]
#[ignore = "writes review pictures; run on demand"]
fn review_pictures_of_the_catalogue_and_a_providers_form() {
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target"),
        std::path::PathBuf::from,
    );
    let out = target.join("provider-shots");
    std::fs::create_dir_all(&out).unwrap();
    let backend = Arc::new(FakeAdminBackend::default());
    let listing = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agent/provider-config/tests/fixtures/openrouter-models.json"
    ))
    .unwrap();
    backend.state.lock_recover().listing =
        Some(agent_provider_config::transform::parse_models(&listing).unwrap());
    let mut app = mount(backend.clone());
    for (theme, name) in [
        (ui::theme::Theme::dark(), "dark"),
        (ui::theme::Theme::light(), "light"),
    ] {
        app.harness().set_theme(theme);
        app.harness().resize((1280, 2400)).unwrap();
        app.settle();
        app.harness()
            .render_png(&out.join(format!("catalogue-{name}.png")));
    }
    app.harness().set_theme(ui::theme::Theme::dark());
    app.harness()
        .click_named("agent-models-add-provider")
        .unwrap();
    app.harness().type_text("anthropic");
    app.settle();
    app.harness()
        .render_png(&out.join("catalogue-filtered-dark.png"));
    for preset in ["zai-anthropic", "openrouter"] {
        run(
            &mut app,
            ProviderEffect::AddProvider {
                preset: preset.into(),
            },
        );
        app.settle();
    }
    let id = config(&mut app).profiles.last().unwrap().id.clone();
    backend
        .state
        .lock_recover()
        .keys
        .insert(id.clone(), CredentialStatus::Configured);
    run(&mut app, ProviderEffect::RefreshProviders);
    app.settle();
    run(
        &mut app,
        ProviderEffect::ListProviderModels {
            provider: id.clone(),
        },
    );
    app.settle();
    run(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: id.clone(),
            model: "anthropic/claude-sonnet-4.5".into(),
        },
    );
    app.settle();
    run(
        &mut app,
        ProviderEffect::SetProviderModel {
            provider: id,
            model: "moondream/moondream3-preview".into(),
        },
    );
    app.settle();
    for (theme, name) in [
        (ui::theme::Theme::dark(), "dark"),
        (ui::theme::Theme::light(), "light"),
    ] {
        app.harness().set_theme(theme);
        app.harness().resize((1280, 3200)).unwrap();
        app.settle();
        app.harness().resize((1280, 3200)).unwrap();
        app.settle();
        app.harness()
            .render_png(&out.join(format!("form-{name}.png")));
    }
    app.harness().resize((420, 3200)).unwrap();
    app.settle();
    app.harness()
        .render_png(&out.join("form-compact-light.png"));
}

include!("fake_backend.rs");
include!("mounted_tests.rs");
