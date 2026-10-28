#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use themes::{App, SCENARIO_THEME, Studio};
use ui::headless::SemanticExpectation;
use ui::prelude::{Id, TestApp};
use ui::theme::Preset;
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

fn frame(scenario: &str, viewport: (u32, u32), scale: f32) -> TestApp<App> {
    let mut app = TestApp::<App>::capture_at(Some(scenario), viewport).unwrap();
    app.harness()
        .relayout_themed(viewport, ui::Theme::dark().with_text_scale(scale))
        .unwrap();
    app
}

fn rect(app: &mut TestApp<App>, id: &str) -> Rect {
    app.harness()
        .targets()
        .get(id)
        .copied()
        .unwrap_or_else(|| panic!("no production target {id}"))
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

fn disjoint(a: Rect, b: Rect) -> bool {
    let overlap = a.intersect(b);
    overlap.width() <= 0.0 || overlap.height() <= 0.0
}

fn near(a: f64, b: f64, why: &str) {
    assert!((a - b).abs() < 0.5, "{why}: {a} vs {b}");
}

/// The theme's commands, in the window bar's order.
const COMMANDS: [&str; 6] = [
    "theme-apply",
    "theme-export",
    "theme-import",
    "theme-duplicate",
    "theme-reset",
    "theme-delete",
];

fn bar(app: &mut TestApp<App>) -> ui::window_toolbar::WindowToolbar {
    app.settle();
    ui::window_toolbar::declared().unwrap_or_else(|| panic!("the window declares its slots"))
}

#[test]
fn narrow_selector_and_customizer_controls_align_to_shared_metrics() {
    // The narrowest window that keeps the theme list docked beside the editor.
    let width = 600.0;
    let mut app = frame("editor", (600, 2400), 1.0);
    let preview = rect(&mut app, "theme-preview");
    let gutter = f64::from(app.harness().theme().tokens.spacing.s4);
    assert!(
        preview.x0 >= gutter && preview.x1 <= width - gutter,
        "preview {preview:?} retains content gutters"
    );

    let toolbar = bar(&mut app);
    let ids: Vec<&str> = toolbar
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert_eq!(ids, COMMANDS, "the theme's commands are the bar's");
    assert_eq!(
        toolbar.location.map(|location| location.label).as_deref(),
        Some("Ember"),
        "the bar names the theme"
    );
    for id in COMMANDS {
        assert!(
            !app.harness().targets().contains_key(id),
            "the editor draws no {id} of its own"
        );
    }

    let dropdowns = named(&mut app, "Dropdown");
    assert!(
        !dropdowns.is_empty(),
        "production choices are shared dropdowns"
    );
    let height = dropdowns[0].height();
    for dropdown in dropdowns {
        assert!(dropdown.width() > 0.0);
        near(dropdown.height(), height, "choices share control height");
    }
    // The list is docked at this width: the bar's toggle hides it and shows
    // it again. Whether the list is shown is read from the bar, not from the
    // list itself, which stays addressable while it slides away.
    assert_eq!(
        toolbar.sidebar,
        Some(true),
        "the bar holds the sidebar toggle, showing the list shown"
    );
    app.harness()
        .expect(&SemanticExpectation::named("themes-nav").enabled(true))
        .unwrap();
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::ToggleSidebar);
    assert_eq!(
        bar(&mut app).sidebar,
        Some(false),
        "the toggle hid the list"
    );
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::ToggleSidebar);
    assert_eq!(bar(&mut app).sidebar, Some(true), "and shows it again");
    app.harness()
        .expect(&SemanticExpectation::named("themes-nav").enabled(true))
        .unwrap();
}

#[test]
fn gallery_cards_wrap_to_available_width_with_measured_gaps() {
    for width in [480, 1280] {
        let mut app = frame("gallery", (width, 2400), 1.0);
        let gap = f64::from(app.harness().theme().tokens.spacing.rhythm.sibling);
        let cards = named(&mut app, "Card");
        assert_eq!(cards.len(), Preset::ALL.len(), "every preset is a card");
        for (i, card) in cards.iter().enumerate() {
            assert!(
                card.width() > 0.0
                    && card.height() > 0.0
                    && card.x0 >= 0.0
                    && card.x1 <= f64::from(width),
                "preset card clipped: {card:?}"
            );
            for other in &cards[i + 1..] {
                assert!(
                    disjoint(*card, *other),
                    "preset cards overlap: {card:?} / {other:?}"
                );
                if (card.x0 - other.x0).abs() < 0.5 {
                    assert!(
                        other.y0 - card.y1 >= gap - 0.5,
                        "stacked cards keep the grid's gap"
                    );
                }
            }
        }
        if width == 480 {
            for pair in cards.windows(2) {
                near(
                    pair[0].x0,
                    pair[1].x0,
                    "stacked cards share their left inset",
                );
                near(
                    pair[0].x1,
                    pair[1].x1,
                    "stacked cards share their right inset",
                );
                assert!(pair[1].y0 > pair[0].y1);
            }
        }
    }
}

#[test]
fn large_text_reflows_controls_and_the_form_without_covering_chrome() {
    let mut one = frame("editor", (480, 4000), 1.0);
    // At double text the list no longer fits beside the editor: the editor
    // is a screen of its own, the top-level page, and the window bar's
    // sidebar toggle reaches the list.
    let mut two = frame("editor", (480, 4000), 2.0);
    assert!(
        bar(&mut two).sidebar.is_some(),
        "the list's toggle is the window bar's"
    );
    let preview = rect(&mut two, "theme-preview");
    assert!(
        preview.y0 >= 0.0 && preview.x0 >= 0.0 && preview.x1 <= 480.0,
        "preview stays inside the window: {preview:?}"
    );
    let name = rect(&mut two, "theme-name");
    assert!(
        name.height() > rect(&mut one, "theme-name").height(),
        "large type increases the field's real height"
    );
    assert!(name.y0 >= preview.y1, "the form starts below the preview");
    let one_form = named(&mut one, "Form");
    let two_form = named(&mut two, "Form");
    assert_eq!(one_form.len(), 1, "the production editor hosts one form");
    assert_eq!(two_form.len(), 1);
    assert!(
        two_form[0].height() > one_form[0].height() + 100.0,
        "the editor reflows from measured large text"
    );
    let face = rect(&mut two, "theme-face");
    assert!(face.height() > rect(&mut one, "theme-face").height());
    assert!(
        face.y0 >= name.y1,
        "the face editor cannot cover the name editor"
    );
}

#[test]
fn reset_preserves_identity_and_restores_the_preset_through_the_public_action() {
    let mut app = frame("tinted", (1280, 2400), 1.0);
    let id = Id::from_raw(SCENARIO_THEME);
    let before = app
        .state()
        .themes
        .get(&id)
        .unwrap()
        .document(SCENARIO_THEME);
    app.invoke(Studio::RESET.with(id.clone())).unwrap();
    let state = app.state();
    let theme = state.themes.get(&id).unwrap();
    assert_eq!(theme.name, "Ember");
    let after = theme.document(SCENARIO_THEME);
    assert_ne!(before, after, "reset changes the edited properties");
    assert_eq!(
        after.shape,
        Preset::find("dark-graphite").unwrap().document().shape
    );
    assert!(after.findings().is_empty());
    assert!(app.undo());
    assert_eq!(
        app.state()
            .themes
            .get(&id)
            .unwrap()
            .document(SCENARIO_THEME),
        before,
        "reset remains one reversible resource action"
    );
}
