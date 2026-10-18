//! Back follows visited screens, including decisions that share a page.
use super::*;
use crate::resource::ImagerShell;
use ui::framework::testing::TestApp;

type App = TestApp<ImagerShell>;

fn effect(app: &mut App, effect: InstallerPreviewEffect) {
    app.effect(&effect)
        .unwrap_or_else(|error| panic!("{} failed: {error:?}", effect.id()));
}

fn choose(app: &mut App, choice: &str) {
    effect(
        app,
        InstallerPreviewEffect::ChoosePreview {
            choice: choice.into(),
        },
    );
    effect(app, InstallerPreviewEffect::ContinuePreview);
}

fn state(app: &mut App) -> serde_json::Value {
    app.effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap()
}

fn preview() -> App {
    let mut app = App::launched(&["--installer-preview"]).unwrap();
    // Keep discovery and identity preparation out of navigation tests. Every
    // screen is still reached through the production action route, not a page fixture.
    app.with_widget::<InstallerPreview, _>(|view| {
        view.live = false;
        view.restart = restart_controller::Restart::default();
        view.storage.stage();
    })
    .unwrap();
    app
}

fn purpose(migration: &str) -> App {
    let mut app = preview();
    choose(&mut app, "computer");
    choose(&mut app, migration);
    choose(&mut app, "new");
    effect(
        &mut app,
        InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        },
    );
    effect(&mut app, InstallerPreviewEffect::ContinuePreview);
    for next in [
        InstallerPreviewEffect::SetPreviewPassword {
            value: "navigation example passphrase".into(),
        },
        InstallerPreviewEffect::SetPreviewConfirmation {
            value: "navigation example passphrase".into(),
        },
        InstallerPreviewEffect::ContinuePreview,
        InstallerPreviewEffect::SetPreviewDeviceName {
            value: "Ada's computer".into(),
        },
        InstallerPreviewEffect::ContinuePreview,
    ] {
        effect(&mut app, next);
    }
    assert_eq!(state(&mut app)["page"], "purpose");
    app
}

fn belongings() -> App {
    let mut app = purpose("migrate");
    choose(&mut app, "computer");
    assert_eq!(state(&mut app)["page"], "belongings");
    app
}

#[test]
fn back_from_choose_what_comes_over_returns_to_overview_and_keeps_choices() {
    let mut app = belongings();
    effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
    effect(
        &mut app,
        InstallerPreviewEffect::SetPreviewCategory {
            category: "flatpak".into(),
            included: false,
        },
    );
    let before = state(&mut app);
    assert_eq!(before["details"], true);
    app.with_widget::<InstallerPreview, _>(|view| view.live = true)
        .unwrap();
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    let overview = state(&mut app);
    assert_eq!(overview["page"], "belongings");
    assert_eq!(overview["details"], false);
    assert_eq!(overview["categories"], before["categories"]);
    assert_eq!(overview["browser_domains"], before["browser_domains"]);
    effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
    assert_eq!(state(&mut app)["categories"], before["categories"]);
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "purpose");
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "devices");
}

#[test]
fn closing_and_reopening_details_does_not_leave_phantom_back_stops() {
    let mut app = belongings();
    for _ in 0..3 {
        effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
        effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
    }
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "purpose");
}

#[cfg(target_os = "linux")]
#[test]
fn back_closes_source_and_app_disclosures_before_belongings_details() {
    let mut app = belongings();
    let source = tempfile::tempdir().unwrap();
    app.with_widget::<InstallerPreview, _>(|view| {
        view.live = true;
        view.belongings.home = Some(source.path().to_path_buf());
    })
    .unwrap();
    effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
    effect(
        &mut app,
        InstallerPreviewEffect::ToggleMigrationSourceLocation,
    );
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(!view.migration_source_location);
        assert!(view.journey.details);
        assert_eq!(view.journey.page, Page::Belongings);
    })
    .unwrap();
    effect(&mut app, InstallerPreviewEffect::ToggleFlatpakAccessDetails);
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(!view.flatpak_access_details);
        assert!(view.journey.details);
    })
    .unwrap();
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "belongings");
    assert_eq!(state(&mut app)["details"], false);
}

#[cfg(target_os = "linux")]
#[test]
fn account_back_steps_return_to_the_actual_belongings_screen() {
    let mut app = belongings();
    effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
    effect(&mut app, InstallerPreviewEffect::ContinuePreview);
    assert_eq!(state(&mut app)["page"], "accounts");
    app.with_widget::<InstallerPreview, _>(|view| view.live = true)
        .unwrap();
    for (step, selection) in [("add-account", None), ("choose-provider", Some("webdav"))] {
        effect(
            &mut app,
            InstallerPreviewEffect::AccountStep {
                step: step.into(),
                selection: selection.map(str::to_owned),
            },
        );
    }
    for (field, value) in [("server", "https://dav.example.com"), ("login", "ada")] {
        effect(
            &mut app,
            InstallerPreviewEffect::SetAccountField {
                field: field.into(),
                value: value.into(),
            },
        );
        effect(
            &mut app,
            InstallerPreviewEffect::AccountStep {
                step: "confirm".into(),
                selection: None,
            },
        );
    }
    app.with_widget::<InstallerPreview, _>(|view| {
        assert_eq!(view.accounts.navigation_step(), "dav-password");
    })
    .unwrap();
    effect(
        &mut app,
        InstallerPreviewEffect::SetAccountField {
            field: "password".into(),
            value: "private example credential".into(),
        },
    );
    for expected in ["dav-user", "dav-server", "providers", "overview"] {
        effect(&mut app, InstallerPreviewEffect::BackInPreview);
        assert_eq!(state(&mut app)["page"], "accounts");
        app.with_widget::<InstallerPreview, _>(|view| {
            assert_eq!(view.accounts.navigation_step(), expected);
        })
        .unwrap();
        assert!(
            !state(&mut app)
                .to_string()
                .contains("private example credential")
        );
    }
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "belongings");
    assert_eq!(state(&mut app)["details"], true);
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "belongings");
    assert_eq!(state(&mut app)["details"], false);
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "purpose");
}

#[cfg(target_os = "linux")]
#[test]
fn source_permission_back_retires_the_grant_without_leaving_details() {
    let mut app = belongings();
    effect(&mut app, InstallerPreviewEffect::TogglePreviewDetails);
    for category in ["flatpak", "accounts"] {
        effect(
            &mut app,
            InstallerPreviewEffect::SetPreviewCategory {
                category: category.into(),
                included: false,
            },
        );
    }
    let source = tempfile::tempdir().unwrap();
    let (reply, receiver) = std::sync::mpsc::channel();
    app.with_widget::<InstallerPreview, _>(|view| {
        view.live = true;
        view.belongings.home = Some(source.path().to_path_buf());
        view.migration_selection.permission_reply = Some(receiver);
    })
    .unwrap();
    effect(&mut app, InstallerPreviewEffect::ContinuePreview);
    app.with_widget::<InstallerPreview, _>(|view| {
        assert!(view.migration_selection.choosing());
        // A grant arriving at the navigation boundary must not outlive Back.
        reply
            .send(ui::PortalOutcome::Opened(source.path().to_path_buf()))
            .unwrap();
        view.apply(InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(view.journey.page, Page::Belongings);
        assert!(view.journey.details);
        assert!(!view.migration_selection.busy());
        assert!(view.migration_selection.reviewed_intent().is_none());
    })
    .unwrap();
    assert!(
        reply
            .send(ui::PortalOutcome::Opened(source.path().to_path_buf()))
            .is_err()
    );
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "belongings");
    assert_eq!(state(&mut app)["details"], false);
}

#[test]
fn accounts_reached_from_overview_return_to_overview_without_a_details_stop() {
    let mut app = belongings();
    effect(&mut app, InstallerPreviewEffect::ContinuePreview);
    assert_eq!(state(&mut app)["page"], "accounts");
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "belongings");
    assert_eq!(state(&mut app)["details"], false);
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "purpose");
}

#[test]
fn fresh_install_back_keeps_the_normal_os_history() {
    let mut app = purpose("fresh");
    choose(&mut app, "computer");
    assert_eq!(state(&mut app)["page"], "storage");
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "purpose");
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "devices");
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "password");
}

#[cfg(target_os = "linux")]
#[test]
fn desktop_review_back_still_returns_directly_to_destination() {
    let mut app = preview();
    choose(&mut app, "desktop");
    assert_eq!(state(&mut app)["page"], "review");
    effect(&mut app, InstallerPreviewEffect::BackInPreview);
    assert_eq!(state(&mut app)["page"], "destination");
    assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
}
