//! The studio as a person and an agent meet it: the gallery previews every
//! preset, the editor follows every edit with live contrast findings, and a
//! theme that misses a floor cannot be applied.

use themes::studio::Rung;
use themes::{App, SCENARIO_THEME, Studio};
use ui::headless::SemanticExpectation;
use ui::prelude::{Id, Invoke, TestApp};
use ui::theme::Preset;

#[test]
fn the_gallery_previews_every_preset() {
    let mut app =
        TestApp::<App>::capture(Some(themes::GALLERY_SCENARIO)).expect("the scenario stages");
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
    let mut app = TestApp::<App>::capture(Some("editor")).expect("the scenario stages");
    app.harness()
        .expect(
            &SemanticExpectation::named("contrast-findings")
                .name_contains("Every pairing meets its contrast floor"),
        )
        .expect("a preset copy is legible");
    app.harness()
        .expect(&SemanticExpectation::named("theme-apply").enabled(true))
        .expect("a legible theme can be applied");

    let theme = Studio::paths().themes.item(&Id::from_raw(SCENARIO_THEME));
    app.invoke(Invoke::edit(&Rung::Tertiary.path(&theme).set(0.42)).expect("an edit"))
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
    let mut app = TestApp::<App>::capture(Some("tinted")).expect("the scenario stages");
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
    let paths = Studio::paths().themes.item(&Id::from_raw(SCENARIO_THEME));
    assert!(
        app.call(
            "field-edit",
            serde_json::json!({ "edit": paths.chroma.set(0.3) })
        )
        .is_err(),
        "a role outside its band is refused, whoever asks"
    );
    app.call(
        "field-edit",
        serde_json::json!({ "edit": paths.density.set(1.3) }),
    )
    .expect("an agent edits one role in one call");
    assert!((app.state().themes[&Id::from_raw(SCENARIO_THEME)].density - 1.3).abs() < 1e-6);
    assert!(app.undo(), "the agent's edit is one undo step");
    assert!((app.state().themes[&Id::from_raw(SCENARIO_THEME)].density - 1.2).abs() < 1e-6);
}

#[test]
fn history_reads_each_role_that_changed() {
    let mut app = TestApp::<App>::capture(Some("editor")).expect("the scenario stages");
    let id = Id::from_raw(SCENARIO_THEME);
    let theme = Studio::paths().themes.item(&id);
    for edit in [
        theme.density.set(1.25),
        Rung::Content.path(&theme).set(0.24),
        theme.tint.set(Some(40.0)),
    ] {
        app.invoke(Invoke::edit(&edit).expect("an edit"))
            .expect("an admitted edit");
    }
    app.invoke(Studio::SWITCH_POLARITY.with((id, false)))
        .expect("switch to light");
    let catalogue = tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| entry.what.is_some())
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences.get(sentences.len().saturating_sub(4)..),
        Some(
            [
                "Set the density of Ember to 1.25",
                "Set the window lightness of Ember to 0.24",
                "Tinted Ember with hue 40°",
                "Made Ember light",
            ]
            .map(String::from)
            .as_slice()
        )
    );
}
