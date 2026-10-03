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
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.stage(Some(themes::GALLERY_SCENARIO)).expect("staged");
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
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.stage(Some("editor")).expect("staged");
    app.harness()
        .expect(
            &SemanticExpectation::named("contrast-findings")
                .name_contains("Every pairing meets its contrast floor"),
        )
        .expect("a preset copy is legible");
    app.harness()
        .expect(&SemanticExpectation::named("theme-apply").enabled(true))
        .expect("a legible theme can be applied");

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
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.stage(Some("tinted")).expect("staged");
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
