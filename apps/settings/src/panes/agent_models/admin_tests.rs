//! Exercise the mounted controls' effect door with a portable asynchronous host.
use std::sync::atomic::{AtomicBool, Ordering};

use agent_provider_config::management::AdminFuture;
use ui::framework::testing::TestApp;

use super::*;
use crate::{SettingsShell, effects::NavigationEffect};
include!("../../../../../crates/agent/provider-settings-ui/src/fake_backend.rs");

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

fn row_for(effect: &AgentModelsEffect) -> String {
    let args = effect.arguments();
    args.get("provider")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || "settings.root".into(),
            |id| format!("settings.agent-provider:{id}"),
        )
}

fn invoke(
    app: &mut TestApp<SettingsShell>,
    effect: AgentModelsEffect,
) -> Result<(), ui::tilo_actions::DoorError> {
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

fn run(app: &mut TestApp<SettingsShell>, effect: AgentModelsEffect) {
    if matches!(
        effect,
        AgentModelsEffect::RemoveProvider { .. }
            | AgentModelsEffect::RemoveProviderKey { .. }
            | AgentModelsEffect::RemoveLegacyProviderKey { .. }
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

fn config(app: &mut TestApp<SettingsShell>) -> ProviderConfig {
    app.with_widget::<SettingsApp, _>(|app| {
        app.agent_models()
            .model()
            .borrow()
            .provider_config()
            .clone()
    })
    .unwrap()
}

fn key_buffer(app: &mut TestApp<SettingsShell>, id: &str) -> Arc<Mutex<String>> {
    app.with_widget::<SettingsApp, _>(|app| app.agent_models().model().borrow().key_buffer(id))
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
        AgentModelsEffect::AddProvider {
            protocol: "anthropic".into(),
        },
    );
    let id = config(&mut app).profiles[0].id.clone();
    app.with_widget::<SettingsApp, _>(|app| {
        let model = app.agent_models().model().borrow();
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
            AgentModelsEffect::SaveProviderKey {
                provider: id,
                key: None
            }
        )
        .is_err()
    );
    assert_eq!(*draft.lock_recover(), "private-key-draft");
    app.with_widget::<SettingsApp, _>(|app| {
        assert!(
            app.agent_models()
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
        let model = app.agent_models().model().borrow();
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
    app.with_widget::<SettingsApp, _>(|app| f(&app.agent_models().model().borrow().panel))
        .unwrap()
}

mod shared_contract {
    use super::*;
    // The native public effect adapter retains all existing ids and parameter shapes.
    type ProviderEffect = AgentModelsEffect;
    fn host_effect(effect: ProviderEffect) -> SettingsEffect {
        effect.into()
    }
    include!("../../../../../crates/agent/provider-settings-ui/src/mounted_tests.rs");
}
