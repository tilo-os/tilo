//! The studio as a person and an agent meet it: the gallery previews every
//! preset, the editor follows every edit with live contrast findings, and a
//! theme that misses a floor cannot be applied.

#![allow(
    clippy::cast_possible_truncation,
    reason = "column positions are rounded to whole pixels to count them"
)]

use themes::studio::Rung;
use themes::{App, SCENARIO_THEME, Studio};
use ui::headless::SemanticExpectation;
use ui::prelude::{Id, Invoke, TestApp};
use ui::theme::Preset;
use ui::vello::kurbo::Rect;
use ui::widget::{CardWidth, Widget, card_columns};

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

fn widgets(root: &dyn Widget, name: &str, out: &mut Vec<Rect>) {
    if root.name() == name {
        out.push(root.get_rect());
    }
    for child in root.children() {
        widgets(child, name, out);
    }
}

fn named(app: &mut TestApp<App>, name: &str) -> Vec<Rect> {
    let mut out = Vec::new();
    widgets(app.harness().root_widget(), name, &mut out);
    out
}

/// The accessible names of everything drawn inside `bounds`.
fn names_within(root: &dyn Widget, bounds: Rect, out: &mut Vec<String>) {
    let rect = root.get_rect();
    if rect.x0 >= bounds.x0 - 0.5
        && rect.x1 <= bounds.x1 + 0.5
        && rect.y0 >= bounds.y0 - 0.5
        && rect.y1 <= bounds.y1 + 0.5
        && let Some(name) = root.accessibility().and_then(|node| node.name)
    {
        out.push(name);
    }
    for child in root.children() {
        names_within(child, bounds, out);
    }
}

#[allow(
    clippy::expect_used,
    reason = "a test fixture: a gallery that cannot stage fails the test"
)]
fn gallery_at(width: u32) -> TestApp<App> {
    TestApp::<App>::capture_at(Some(themes::GALLERY_SCENARIO), (width, 2400))
        .expect("the scenario stages")
}

/// Each preset is one card: its preview, its name and its actions are all
/// inside it, and inside no other card.
#[test]
fn a_preset_is_one_card_holding_its_preview_name_and_actions() {
    let mut app = gallery_at(1680);
    let cards = named(&mut app, "Card");
    assert_eq!(cards.len(), Preset::ALL.len(), "one card per preset");
    let targets = app.harness().targets();
    for preset in Preset::ALL {
        let parts =
            ["preview", "use", "customize"].map(|part| targets[&format!("{part}-{}", preset.id())]);
        let holding: Vec<&Rect> = cards
            .iter()
            .filter(|card| parts.iter().all(|part| card.union(*part) == **card))
            .collect();
        assert_eq!(
            holding.len(),
            1,
            "{}: {parts:?} in {cards:?}",
            preset.name()
        );
        let mut names = Vec::new();
        names_within(app.harness().root_widget(), *holding[0], &mut names);
        assert!(
            names.iter().any(|name| name == preset.name()),
            "{} names its own card: {names:?}",
            preset.name()
        );
        assert!(
            parts[1].y0 >= parts[0].y1 && parts[2].y0 >= parts[0].y1,
            "{}: the actions sit under the preview",
            preset.name()
        );
    }
}

#[test]
fn the_preset_grid_columns_follow_the_width_and_its_tiles_resize() {
    let mut counts = Vec::new();
    let mut widths_at = Vec::new();
    for width in [420, 1100, 1680, 1720, 2560] {
        let mut app = gallery_at(width);
        let theme = app.harness().theme().clone();
        let min = CardWidth::Wide.min_width(&theme);
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let grid = named(&mut app, "CardGrid")[0];
        let cards = named(&mut app, "Card");
        let mut columns: Vec<i64> = cards.iter().map(|card| card.x0.round() as i64).collect();
        columns.sort_unstable();
        columns.dedup();
        assert_eq!(
            columns.len(),
            card_columns(grid.width(), min, gap).min(cards.len()),
            "at {width}: as many columns as {min} px tiles fit in {}",
            grid.width()
        );
        let first_row: Vec<&Rect> = cards
            .iter()
            .filter(|card| (card.y0 - cards[0].y0).abs() < 0.5)
            .collect();
        let row = first_row
            .iter()
            .fold(*first_row[0], |row, card| row.union(**card));
        assert!(
            (row.x0 - grid.x0).abs() < 1.0 && (row.x1 - grid.x1).abs() < 1.0,
            "at {width}: the tiles stretch to fill the row {row:?} of {grid:?}"
        );
        let targets = app.harness().targets();
        for preset in Preset::ALL {
            let preview = targets[&format!("preview-{}", preset.id())];
            let (cw, ch) = themes::draw::CANVAS;
            assert!(
                (preview.height() - preview.width() * ch / cw).abs() < 1.0,
                "at {width}: {}'s preview {preview:?} keeps the miniature's proportions",
                preset.name()
            );
        }
        counts.push(columns.len());
        widths_at.push(cards[0].width());
    }
    assert_eq!(counts[0], 1, "a narrow window shows one column");
    assert!(
        counts.windows(2).all(|pair| pair[1] >= pair[0]),
        "a wider window never shows fewer columns: {counts:?}"
    );
    assert!(
        counts[4] > counts[1],
        "a wide window shows more columns: {counts:?}"
    );
    if counts[2] == counts[3] {
        assert!(
            widths_at[3] > widths_at[2],
            "40 px more window widens every tile: {widths_at:?}"
        );
    } else {
        assert!(
            widths_at[3] < widths_at[2],
            "a new column narrows the tiles"
        );
    }
}

/// The preset the system shows says so where the others offer "Use".
#[test]
fn the_preset_in_use_says_in_use() {
    let mut app = gallery_at(1680);
    app.harness()
        .expect(
            &SemanticExpectation::named("use-dark")
                .name_contains("In use")
                .enabled(false),
        )
        .expect("with nothing chosen, the system shows Dark");
    app.harness()
        .expect(
            &SemanticExpectation::named("use-light")
                .name_contains("Use")
                .enabled(true),
        )
        .expect("every other preset can be used");
    app.harness()
        .expect(&SemanticExpectation::named("use-dark").enabled(false))
        .expect("still in use");
    let mut selected = Vec::new();
    selected_cards(app.harness().root_widget(), &mut selected);
    assert_eq!(
        selected,
        vec![Preset::Dark.name().to_owned()],
        "the card of the preset in use is the selected one, and only it"
    );
}

/// The names of the cards announced as selected.
fn selected_cards(root: &dyn Widget, out: &mut Vec<String>) {
    if root.name() == "Card"
        && let Some(node) = root.accessibility()
        && node.state.selected
    {
        out.push(node.name.unwrap_or_default());
    }
    for child in root.children() {
        selected_cards(child, out);
    }
}

/// The sidebar is the toolkit's navigation list, not a column of buttons.
#[test]
fn the_sidebar_is_the_navigation_list() {
    let mut app = gallery_at(1680);
    app.harness()
        .expect(&SemanticExpectation::named("themes-nav"))
        .expect("the destinations are a navigation list");
    assert!(
        !app.harness().targets().contains_key("themes-gallery"),
        "no button stands in for a destination"
    );
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
    assert_eq!(
        bar_enabled(&mut app, "theme-apply"),
        Some(true),
        "a legible theme can be applied"
    );

    let theme = Studio::paths().themes.item(&Id::from_raw(SCENARIO_THEME));
    app.invoke(Invoke::edit(&Rung::Tertiary.path(&theme).set(0.42)).expect("an edit"))
        .expect("an admitted edit");
    app.harness()
        .expect(&SemanticExpectation::named("contrast-findings").name_contains("text_tertiary"))
        .expect("the failing pair is named");
    assert_eq!(
        bar_enabled(&mut app, "theme-apply"),
        Some(false),
        "an illegible theme cannot be applied"
    );
    assert_eq!(
        bar_enabled(&mut app, "theme-export"),
        Some(false),
        "nor exported"
    );
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

/// Whether the window bar's action `id` acts, if the bar holds it.
fn bar_enabled(app: &mut TestApp<App>, id: &str) -> Option<bool> {
    app.settle();
    ui::window_toolbar::declared()?
        .actions
        .into_iter()
        .find(|action| action.id == id)
        .map(|action| action.enabled)
}

/// Open the navigation list's Presets destination.
#[allow(
    clippy::expect_used,
    reason = "a test fixture: a sidebar without its Presets row fails the test"
)]
fn leave_for_presets(app: &mut TestApp<App>) {
    let row = app
        .with_widget(|nav: &mut ui::prelude::NavList| nav.row_rect("presets"))
        .flatten()
        .expect("the Presets row is drawn");
    let center = row.center();
    app.harness().click_at(center.x, center.y);
    app.harness().relayout();
    app.harness()
        .expect(&SemanticExpectation::named(format!(
            "preview-{}",
            Preset::Dark.id()
        )))
        .expect("the presets are shown");
}

/// Customizing a preset opens an editor showing its copy, but only an edit
/// writes the theme: one opened and left untouched leaves nothing under
/// "Your themes", and the first edit writes exactly one, carrying that edit.
#[test]
fn customizing_writes_the_theme_on_its_first_edit() {
    let mut app = gallery_at(1680);
    let customize = format!("customize-{}", Preset::DarkGraphite.id());
    app.harness()
        .click_named(&customize)
        .expect("the preset offers Customize");
    app.harness().relayout();
    app.harness()
        .expect(&SemanticExpectation::named("contrast-findings").name_contains("Every pairing"))
        .expect("the editor shows the preset's copy");
    assert!(app.state().themes.is_empty(), "opening writes nothing");
    leave_for_presets(&mut app);
    assert!(
        app.state().themes.is_empty(),
        "a copy left untouched leaves nothing behind"
    );

    app.harness()
        .click_named(&customize)
        .expect("the preset offers Customize");
    app.harness().relayout();
    app.harness()
        .click_named("theme-name")
        .expect("the name field is drawn");
    app.harness().type_text("X");
    app.harness().relayout();
    let state = app.state();
    assert_eq!(state.themes.len(), 1, "the first edit writes one theme");
    let (_, theme) = state.themes.iter().next().expect("one theme");
    assert!(
        theme.name.contains('X') && theme.name.contains(Preset::DarkGraphite.name()),
        "the theme is the preset's copy carrying the edit: {:?}",
        theme.name
    );
    leave_for_presets(&mut app);
    assert_eq!(app.state().themes.len(), 1, "a written theme stays");
}
