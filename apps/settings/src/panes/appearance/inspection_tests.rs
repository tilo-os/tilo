use super::*;
use crate::effects::SettingsEffect;
use crate::resource::SettingsShell;
use ui::prelude::TestApp;
use ui::tilo_resource::Effect as _;

fn read(app: &mut TestApp<SettingsShell>) -> serde_json::Value {
    let action = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|action| action.address.action == "inspect-appearance")
        .expect("appearance preferences have a published read door");
    app.call_on(
        &action.address.resource,
        &action.address.action,
        serde_json::json!({}),
    )
    .unwrap()
    .unwrap()
}

fn set(app: &mut TestApp<SettingsShell>, effect: AppearanceEffect) {
    app.effect(&SettingsEffect::from(effect)).unwrap();
}

#[test]
fn appearance_inspection_reads_actual_automatic_preferences_and_restores_public_changes() {
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    for effect in [
        AppearanceEffect::SetTheme {
            theme: "Automatic".into(),
        },
        AppearanceEffect::SetDayNightSchedule {
            schedule: "custom".into(),
        },
        AppearanceEffect::SetDayStart {
            time: "06:30".into(),
        },
        AppearanceEffect::SetNightStart {
            time: "21:00".into(),
        },
    ] {
        set(&mut app, effect);
    }
    let original = read(&mut app);
    let preferences = app.settings::<AppearancePrefs>().get();
    assert_eq!(original["schema"], "tilo.settings.appearance.v1");
    assert_eq!(original["theme"], "Automatic");
    assert_eq!(original["mode"], "automatic");
    assert_eq!(original["schedule"], "custom");
    assert_eq!(original["sunrise"], "06:30");
    assert_eq!(original["sunset"], "21:00");
    assert_eq!(app.settings::<AppearancePrefs>().get(), preferences);
    set(
        &mut app,
        AppearanceEffect::SetTheme {
            theme: "Light".into(),
        },
    );
    assert_eq!(read(&mut app)["theme"], "Light");
    set(
        &mut app,
        AppearanceEffect::SetTheme {
            theme: original["theme"].as_str().unwrap().into(),
        },
    );
    assert_eq!(read(&mut app), original);
    assert_eq!(app.settings::<AppearancePrefs>().get(), preferences);
}

#[test]
fn appearance_inspection_is_read_only_and_refuses_foreign_device_invocation() {
    let mut app = TestApp::<SettingsShell>::with_role(ui::permissions::Role::Viewer).unwrap();
    let original = read(&mut app);
    let action = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|action| action.address.action == "inspect-appearance")
        .unwrap();
    assert_eq!(action.contract.kind, ui::tilo_actions::ActionKind::Read);
    assert!(
        action
            .contract
            .tags
            .iter()
            .any(|tag| tag == ui::tilo_actions::LOCAL_DEVICE_ONLY)
    );
    let refused = app.call_by(
        ui::tilo_actions::ActionInvoker::actor("unrelated-collaborator"),
        &action.address.resource,
        &action.address.action,
        serde_json::json!({}),
    );
    assert!(
        matches!(refused, Err(ui::tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "private_view"),
        "{refused:?}"
    );
    let setting = SettingsEffect::from(AppearanceEffect::SetTheme {
        theme: "Dark".into(),
    });
    assert!(
        app.call_on(
            &action.address.resource,
            setting.id(),
            serde_json::json!({"theme":"Dark"})
        )
        .is_err()
    );
    assert_eq!(read(&mut app), original);
}

#[test]
fn appearance_inspection_follows_current_shared_custom_choices_without_exporting_theme_documents() {
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let _ = read(&mut app);
    app.settings::<AppearancePrefs>()
        .update(|prefs| {
            prefs.mode = "automatic".into();
            prefs.dark_theme = "custom:private-theme".into();
            prefs.light_theme = "light".into();
            prefs.custom_themes = vec![crate::prefs::CustomTheme(
                serde_json::json!({"private_document_marker":"never-export"}),
            )];
            prefs.schedule = "custom".into();
            prefs.sunrise = "07:00".into();
            prefs.sunset = "22:00".into();
        })
        .unwrap();
    let current = app.settings::<AppearancePrefs>().get();
    let inspected = read(&mut app);
    assert_eq!(inspected["mode"], "automatic");
    assert_eq!(inspected["dark_theme"], "custom:private-theme");
    assert_eq!(inspected["sunrise"], "07:00");
    assert_eq!(inspected["sunset"], "22:00");
    assert!(inspected.get("custom_themes").is_none());
    assert!(!inspected.to_string().contains("never-export"));
    assert_eq!(app.settings::<AppearancePrefs>().get(), current);
    // Reversible accessibility changes preserve custom references/documents.
    set(
        &mut app,
        AppearanceEffect::SetHighContrast {
            on: !current.high_contrast,
        },
    );
    assert_eq!(read(&mut app)["high_contrast"], !current.high_contrast);
    set(
        &mut app,
        AppearanceEffect::SetHighContrast {
            on: current.high_contrast,
        },
    );
    assert_eq!(read(&mut app), inspected);
    assert_eq!(app.settings::<AppearancePrefs>().get(), current);
}
