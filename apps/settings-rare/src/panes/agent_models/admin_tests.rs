use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::management::AdminFuture;
use ui::framework::testing::TestApp;

use super::{AgentModelsEffect, PaneCx, ProviderPanel, SettingsApp, SettingsEffect};
use crate::panes::model::PaneModel;
use crate::{SettingsShell, effects::NavigationEffect};
use agent_provider_config::management::{CredentialStatus, ProviderAdmin, ProviderSnapshot};
use agent_provider_config::protocol::{ErrorKind, ModelError};
use agent_provider_config::{ProviderConfig, ProviderProfile};
use provider_settings_ui::ProviderEffect;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use ui::lock::MutexRecover as _;
use ui::tilo_resource::Effect;
include!("../../../../../crates/sdk/provider-settings-ui/src/fake_backend.rs");

fn mount(backend: Arc<FakeAdminBackend>) -> TestApp<SettingsShell> {
    let mut app = TestApp::<SettingsShell>::for_person_with_lent(
        "provider-administrator",
        ui::models::ProviderAdministration::new(backend),
    )
    .unwrap();
    app.effect(&SettingsEffect::from(NavigationEffect::OpenPane {
        pane: "agent_models".into(),
    }))
    .unwrap();
    app.settle();
    app
}

fn row_for<E: Effect>(effect: &E) -> String {
    /// The provider an effect acts on, when it names one.
    #[derive(serde::Deserialize)]
    struct OnProvider {
        provider: Option<String>,
    }
    serde_json::from_value::<OnProvider>(serde_json::Value::Object(effect.arguments()))
        .ok()
        .and_then(|on| on.provider)
        .map_or_else(
            || "settings.root".into(),
            |id| format!("settings.agent-provider:{id}"),
        )
}

fn invoke<E>(app: &mut TestApp<SettingsShell>, effect: E) -> Result<(), ui::tilo_actions::DoorError>
where
    E: Effect,
    SettingsEffect: From<E>,
{
    let row = row_for(&effect);
    let effect = SettingsEffect::from(effect);
    let mut arguments = effect.arguments();
    if row != "settings.root" {
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

fn run<E>(app: &mut TestApp<SettingsShell>, effect: E)
where
    E: Effect,
    SettingsEffect: From<E>,
{
    // The removals the host asks about before carrying them out.
    if matches!(
        effect.id(),
        "remove-provider" | "remove-provider-key" | "remove-legacy-provider-key"
    ) {
        let row = row_for(&effect);
        let effect = SettingsEffect::from(effect);
        app.call_confirmed_on(&row, effect.id(), serde_json::json!({}))
            .unwrap();
        app.settle();
    } else {
        invoke(app, effect).unwrap();
    }
}

fn start<E: Effect>(app: &mut TestApp<SettingsShell>, effect: &E) {
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: app.harness().component_instance_id().unwrap(),
        address: effect.address_on(row_for(effect)),
    };
    let mut arguments = effect.arguments();
    arguments.remove("provider");
    app.harness()
        .start_bound_job_for(None, &address, &arguments, true)
        .unwrap();
}

fn config(app: &mut TestApp<SettingsShell>) -> ProviderConfig {
    app.with_widget::<SettingsApp, _>(|app| {
        crate::panes::agent_models::area(app)
            .unwrap()
            .model()
            .borrow()
            .provider_config()
            .clone()
    })
    .unwrap()
}

fn key_buffer(app: &mut TestApp<SettingsShell>, id: &str) -> Arc<Mutex<String>> {
    app.with_widget::<SettingsApp, _>(|app| {
        crate::panes::agent_models::area(app)
            .unwrap()
            .model()
            .borrow()
            .key_buffer(id)
    })
    .unwrap()
}

#[test]
fn mounted_without_administration_edits_only_honestly_labelled_offline_metadata() {
    let mut app = TestApp::<SettingsShell>::for_person("provider-administrator").unwrap();
    app.effect(&SettingsEffect::from(NavigationEffect::OpenPane {
        pane: "agent_models".into(),
    }))
    .unwrap();
    run(
        &mut app,
        ProviderEffect::AddProvider {
            preset: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    app.with_widget::<SettingsApp, _>(|app| {
        let model = crate::panes::agent_models::area(app)
            .unwrap()
            .model()
            .borrow();
        assert_eq!(model.providers.settings().get().config.profiles.len(), 1);
        assert!(
            model
                .notice
                .as_ref()
                .unwrap()
                .contains("Backend settings are unchanged")
        );
    })
    .unwrap();
    let draft = key_buffer(&mut app, &id);
    *draft.lock_recover() = "private-key-draft".into();
    assert!(
        invoke(
            &mut app,
            ProviderEffect::SaveProviderKey {
                provider: id,
                key: None
            }
        )
        .is_err()
    );
    assert_eq!(*draft.lock_recover(), "private-key-draft");
    app.with_widget::<SettingsApp, _>(|app| {
        assert!(
            crate::panes::agent_models::area(app)
                .unwrap()
                .model()
                .borrow()
                .notice
                .as_ref()
                .unwrap()
                .contains("unavailable")
        );
    })
    .unwrap();
}

fn assert_private(app: &mut TestApp<SettingsShell>) {
    app.with_widget::<SettingsApp, _>(|app| {
        let model = crate::panes::agent_models::area(app)
            .unwrap()
            .model()
            .borrow();
        assert!(
            model.providers.settings().get().config.profiles.is_empty(),
            "backend metadata must not be duplicated into the local settings channel"
        );
        let shared = serde_json::to_string(&model.providers.settings().get()).unwrap();
        assert!(!shared.contains("private-key-draft"));
        assert!(
            !crate::widgets::announced_text(model.compose().as_ref())
                .join(" ")
                .contains("private-key-draft")
        );
    })
    .unwrap();
    let document = app
        .read_document(|doc| serde_json::to_string(&ui::automerge::AutoSerde::from(doc)).unwrap());
    assert!(!document.contains("private-key-draft"));
    assert!(!format!("{:?}", app.history()).contains("private-key-draft"));
}

fn with_panel<T>(
    app: &mut TestApp<SettingsShell>,
    f: impl FnOnce(&ProviderPanel<PaneCx>) -> T,
) -> T {
    app.with_widget::<SettingsApp, _>(|app| {
        f(&crate::panes::agent_models::area(app)
            .unwrap()
            .model()
            .borrow()
            .panel)
    })
    .unwrap()
}

#[test]
fn mounted_vertex_save_serializes_and_reloads_after_failure_and_conflict() {
    for conflict in [false, true] {
        let backend = Arc::new(FakeAdminBackend::default());
        let profile = ProviderProfile::google_vertex("project-a", "global", "gemini-2.5-pro");
        let id = profile.id.clone();
        backend.state.lock_recover().config.upsert(profile);
        let mut app = mount(backend.clone());
        let draft = app
            .with_widget::<SettingsApp, _>(|app| {
                let pane = crate::panes::agent_models::area(app)
                    .unwrap()
                    .model()
                    .borrow();
                pane.vertex_fields(&pane.provider_config().profiles[0])
            })
            .unwrap();
        draft.project.set("project-b".into());
        let effect = AgentModelsEffect::SetVertexProvider {
            provider: Some(id.clone()),
            project: "project-b".into(),
            location: Some("global".into()),
            model: Some("gemini-2.5-pro".into()),
        };
        backend.delay.store(true, Ordering::SeqCst);
        if conflict {
            backend.state.lock_recover().revision += 1;
        } else {
            backend.state.lock_recover().fail_next = true;
        }
        start(&mut app, &effect);
        with_panel(&mut app, |panel| assert!(panel.admin_pending.is_some()));
        assert!(
            invoke(
                &mut app,
                ProviderEffect::SetProviderEnabled {
                    provider: id.clone(),
                    on: false
                }
            )
            .is_err()
        );
        backend.delay.store(false, Ordering::SeqCst);
        app.settle();
        with_panel(&mut app, |panel| {
            assert!(panel.admin_pending.is_none());
            assert_eq!(
                panel.admin_snapshot.as_ref().unwrap().revision,
                backend.state.lock_recover().revision.to_string()
            );
        });
        assert_eq!(draft.project.get(), "project-b");
        assert_eq!(
            config(&mut app).profiles[0]
                .sign_in
                .as_ref()
                .unwrap()
                .billing_project
                .as_deref(),
            Some("project-a")
        );
        invoke(&mut app, effect).unwrap();
        assert_eq!(
            config(&mut app).profiles[0]
                .sign_in
                .as_ref()
                .unwrap()
                .billing_project
                .as_deref(),
            Some("project-b")
        );
    }
}

mod shared_contract {
    use super::{
        CredentialStatus, FakeAdminBackend, Ordering, ProviderProfile, SettingsEffect,
        assert_private, config, invoke, key_buffer, mount, run, with_panel,
    };
    // The provider panel's own typed vocabulary, answered by this area.
    use provider_settings_ui::ProviderEffect;
    use provider_settings_ui::widgets;
    use std::sync::Arc;
    use ui::lock::MutexRecover as _;
    fn host_effect(effect: ProviderEffect) -> SettingsEffect {
        effect.into()
    }
    include!("../../../../../crates/sdk/provider-settings-ui/src/mounted_tests.rs");
}

/// A provider effect invoked through Settings' public union — by its
/// published id and arguments, as the palette, an agent or a script does —
/// reaches the area that hosts the provider panel.
#[test]
fn a_published_provider_effect_reaches_the_provider_area() {
    let backend = Arc::new(FakeAdminBackend::default());
    let mut app = mount(backend.clone());
    let effect = SettingsEffect::decode(
        "add-provider",
        &serde_json::Map::from_iter([("preset".to_owned(), serde_json::json!("anthropic"))]),
    )
    .expect("the provider vocabulary is part of Settings");
    assert_eq!(effect.id(), "add-provider");
    app.effect(&effect).expect("the hosted area answers it");
    app.settle();
    assert_eq!(config(&mut app).profiles.len(), 1);
    assert_eq!(
        backend.state.lock_recover().config.profiles[0].label,
        "Anthropic",
        "the host's administration capability carried the change"
    );
}

/// The operations that need the host's credential custody are refused
/// offline with the reason the pane gives, never simulated here.
#[test]
fn key_custody_effects_are_denied_offline_and_say_why() {
    use crate::i18n::t;
    let mut app = TestApp::<SettingsShell>::for_person("provider-administrator").unwrap();
    app.effect(&SettingsEffect::from(NavigationEffect::OpenPane {
        pane: "agent_models".into(),
    }))
    .unwrap();
    for effect in [
        ProviderEffect::RefreshProviders,
        ProviderEffect::ListProviderModels {
            provider: "gone".into(),
        },
        ProviderEffect::SaveProviderKey {
            provider: "gone".into(),
            key: None,
        },
        ProviderEffect::RemoveProviderKey {
            provider: "gone".into(),
        },
        ProviderEffect::RemoveLegacyProviderKey {
            provider: "gone".into(),
        },
        ProviderEffect::MigrateProviderKey {
            provider: "gone".into(),
            original_endpoint: String::new(),
        },
    ] {
        let effect = SettingsEffect::from(effect);
        assert!(
            app.effect(&effect).is_err(),
            "{} needs the host's custody",
            effect.id()
        );
        let offline = t("agent-provider-offline");
        app.with_widget::<SettingsApp, _>(|app| {
            assert_eq!(
                crate::panes::agent_models::area(app)
                    .unwrap()
                    .model()
                    .borrow()
                    .notice
                    .as_deref(),
                Some(offline.as_str()),
                "{} says why it was refused",
                effect.id()
            );
        })
        .unwrap();
    }
}
