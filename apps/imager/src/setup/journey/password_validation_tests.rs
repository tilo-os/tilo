use super::visual_tests::control;
use super::*;
use crate::resource::GuidedInstallerShell;
use ui::framework::testing::TestApp;

fn fixture() -> TestApp<GuidedInstallerShell> {
    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = false;
        preview.restart = restart_controller::Restart::default();
        preview.error = None;
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.migrate = Some(false);
        preview.journey.identity = Some(Identity::New);
        preview.journey.page = Page::Password;
        preview.journey.password.clear();
        preview.journey.confirmation.clear();
        preview.dirty = true;
    })
    .unwrap();
    app.harness()
        .set_theme(Theme::dark().with_reduce_motion(true));
    app.settle();
    app
}

fn focus(app: &mut TestApp<GuidedInstallerShell>, id: &str) {
    app.harness()
        .perform_named(id, &ui::a11y::Action::Focus)
        .unwrap();
    app.settle();
}

fn check(
    app: &mut TestApp<GuidedInstallerShell>,
    password: bool,
    confirmation: bool,
    enabled: bool,
) {
    app.with_widget::<InstallerPreview, _>(|preview| {
        for (id, invalid) in [
            ("installer.password", password),
            ("installer.confirmation", confirmation),
        ] {
            let access = control(preview, id).unwrap().accessibility().unwrap();
            assert_eq!(access.state.invalid, invalid, "{id}");
            assert!(access.state.protected);
            assert!(access.value.is_none());
        }
        assert_eq!(preview.password_continue_enabled(), enabled);
        assert_eq!(preview.journey.page, Page::Password);
        assert!(
            control(preview, "installer.error").is_none(),
            "unexpected banner: {:?}; expected invalid={password}/{confirmation}",
            preview.error
        );
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn tab_blur_validates_short_password_and_corrections_update_both_fields() {
    let mut app = fixture();
    check(&mut app, false, false, false);
    focus(&mut app, "installer.password");
    app.harness().type_text("eightchr");
    app.settle();
    check(&mut app, false, false, false);
    app.harness().key("Tab");
    app.settle();
    check(&mut app, true, false, false);
    app.harness().type_text("different");
    app.settle();
    check(&mut app, true, true, false);
    // Semantic setters follow the same private edit path, with no public value setter.
    app.effect(&InstallerPreviewEffect::SetPreviewPassword {
        value: "walnut telescope amber meadow".into(),
    })
    .unwrap();
    check(&mut app, false, true, false);
    app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
        value: "walnut telescope amber meadow".into(),
    })
    .unwrap();
    check(&mut app, false, false, true);
    app.effect(&InstallerPreviewEffect::SetPreviewPassword {
        value: "short".into(),
    })
    .unwrap();
    check(&mut app, true, true, false);
}

#[test]
#[cfg(unix)]
fn enter_and_pointer_blur_validate_without_submitting_or_exposing_secrets() {
    let mut app = fixture();
    focus(&mut app, "installer.password");
    app.harness().type_text("eightchr");
    app.harness().key("Enter");
    app.settle();
    check(&mut app, true, false, false);
    app.with_widget::<InstallerPreview, _>(|preview| {
        assert!(
            control(preview, "installer.confirmation")
                .unwrap()
                .is_focused()
        );
        assert!(preview.error.is_none());
    })
    .unwrap();
    app.harness().type_text("eightchr");
    app.settle();
    check(&mut app, true, false, false);
    let point = app
        .with_widget::<InstallerPreview, _>(|preview| {
            control(preview, "installer.password")
                .unwrap()
                .get_rect()
                .center()
        })
        .unwrap();
    app.harness().click_at(point.x, point.y);
    app.settle();
    app.harness().type_text(" walnut meadow");
    app.settle();
    check(&mut app, false, true, false);
    focus(&mut app, "installer.confirmation");
    app.harness().key("End");
    app.harness().type_text(" walnut meadow");
    app.settle();
    check(&mut app, false, false, true);
    // Moving away from the corrected field must not resurrect stale errors.
    app.harness().key("Tab");
    app.settle();
    check(&mut app, false, false, true);
    // Edits in the middle survive validation-driven recomposition, including
    // the caret. If it jumps to the end these two corrections cannot match.
    focus(&mut app, "installer.password");
    app.harness().key("Home");
    app.harness().type_text("x");
    app.settle();
    app.harness().type_text("y");
    app.settle();
    check(&mut app, false, true, false);
    focus(&mut app, "installer.confirmation");
    app.harness().key("Home");
    app.harness().type_text("xy");
    app.settle();
    check(&mut app, false, false, true);
    let status = app.effect(&InstallerPreviewEffect::ReadPreview).unwrap();
    assert!(!status.value.unwrap().to_string().contains("eightchr"));
    let snapshot = app.harness().inspection_json().unwrap();
    assert!(!snapshot.contains("eightchr"));
    assert!(!snapshot.contains("walnut meadow"));
}

#[test]
#[cfg(unix)]
fn pointer_blur_shows_primary_error_before_any_submission() {
    let mut app = fixture();
    focus(&mut app, "installer.password");
    app.harness().type_text("eightchr");
    app.settle();
    let point = app
        .with_widget::<InstallerPreview, _>(|preview| {
            control(preview, "installer.confirmation")
                .unwrap()
                .get_rect()
                .center()
        })
        .unwrap();
    app.harness().click_at(point.x, point.y);
    app.settle();
    check(&mut app, true, false, false);
}
