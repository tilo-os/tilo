//! Mounted standalone host adapter for the shared provider contract.
use super::app::{ProviderSettingsApp, ProviderSurface};
use super::{ProviderEffect, ProviderPanel, definition, widgets};
use agent_provider_config::management::AdminFuture;
use agent_provider_config::management::{CredentialStatus, ProviderAdmin, ProviderSnapshot};
use agent_provider_config::protocol::{ErrorKind, ModelError};
use agent_provider_config::{ProviderConfig, ProviderProfile};
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
            protocol: "anthropic".into()
        })
        .is_err()
    );
    assert!(config(&mut app).profiles.is_empty());
    assert_private(&mut app);
    assert_eq!(definition().app_id(), "so.tilo.model-providers");
}

include!("fake_backend.rs");
include!("mounted_tests.rs");
