//! The studio as a person and an agent meet it: the gallery previews every
//! preset, the editor follows every edit with live contrast findings, and a
//! theme that misses a floor cannot be applied.

#![allow(clippy::expect_used, reason = "a test that cannot open the app fails")]

use themes::{App, SCENARIO_THEME, Studio};
use ui::headless::SemanticExpectation;
use ui::prelude::{Id, TestApp};
use ui::theme::Preset;

#[test]
fn the_gallery_previews_every_preset() {
    let mut app =
        TestApp::<App>::capture(Some(themes::GALLERY_SCENARIO)).expect("the gallery capture opens");
    for preset in Preset::ALL {
        app.harness()
            .expect(
                &SemanticExpectation::named(format!("preview-{}", preset.id()))
                    .name_contains(preset.name()),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", preset.name()));
    }
}

#[test]
fn the_editor_measures_contrast_as_the_theme_changes_and_gates_apply() {
    let mut app = TestApp::<App>::capture(Some("editor")).expect("the editor capture opens");
    app.harness()
        .expect(
            &SemanticExpectation::named("contrast-findings")
                .name_contains("Every pairing meets its contrast floor"),
        )
        .expect("a preset copy is legible");
    app.harness()
        .expect(&SemanticExpectation::named("theme-apply").enabled(true))
        .expect("a legible theme can be applied");

    // The shared push shell returns to the destinations on a narrow window,
    // and opening the same theme changes only local presentation state.
    let before_navigation = app.state();
    app.harness()
        .relayout_at((390, 700))
        .expect("narrow layout settles");
    let back = app
        .harness()
        .a11y_node_named("Back")
        .expect("the content exposes shared back navigation");
    app.harness()
        .perform_a11y_action(back, &ui::a11y::Action::Activate)
        .expect("back activation reaches the shell");
    app.harness()
        .wait_for_visuals(ui::time::Duration::from_secs(2))
        .expect("the sidebar finishes entering before its destination is clicked");
    app.harness()
        .click_named(&format!("theme-{SCENARIO_THEME}"))
        .expect("the destination reopens its theme");
    app.harness()
        .wait_for_visuals(ui::time::Duration::from_secs(2))
        .expect("the editor finishes entering before its controls are inspected");
    app.harness()
        .expect(&SemanticExpectation::named("theme-apply").enabled(true))
        .expect("the editor is reachable after narrow navigation");
    assert_eq!(
        app.state(),
        before_navigation,
        "navigation never edits the shared theme"
    );

    app.invoke(Studio::SET_LIGHTNESS.with((
        Id::from_raw(SCENARIO_THEME),
        String::from("tertiary"),
        0.42,
    )))
    .expect("an admitted edit");
    app.harness()
        .expect(&SemanticExpectation::named("contrast-findings").name_contains("text_tertiary"))
        .expect("the failing pair is named");
    app.harness()
        .expect(&SemanticExpectation::named("theme-apply").enabled(false))
        .expect("an illegible theme cannot be applied");
    app.harness()
        .expect(&SemanticExpectation::named("theme-export").enabled(false))
        .expect("nor exported");
}

#[test]
fn agents_shape_a_theme_through_the_same_actions() {
    let mut app = TestApp::<App>::capture(Some("tinted")).expect("the tinted capture opens");
    let state = app.state();
    let theme = state
        .themes
        .get(&Id::from_raw(SCENARIO_THEME))
        .expect("the scenario wrote a theme");
    assert_eq!(theme.name, "Ember");
    assert!((theme.window_radius - 22.0).abs() < 1e-4);
    let document = theme.document(SCENARIO_THEME);
    assert_eq!(document.validate(), Ok(()));
    assert_eq!(document.findings(), Vec::new());
    assert!(
        app.invoke(Studio::SET_CHROMA.with((Id::from_raw(SCENARIO_THEME), 0.3)))
            .is_err(),
        "a role outside its band is refused"
    );
}
