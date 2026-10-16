//! Alignment contracts exercised on Theme Studio's admitted resource view.
//! The shared `NavShell`, `Form` and wrapping `Row` widgets now own the geometry that the
//! earlier customizer painted. Tests observe their production layout.

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

#[test]
fn narrow_selector_and_customizer_controls_align_to_shared_metrics() {
    let mut app = frame("editor", (480, 2400), 1.0);
    let toggle = rect(&mut app, "nav-shell-sidebar");
    assert!(toggle.width() > 0.0 && toggle.height() > 0.0);
    let preview = rect(&mut app, "theme-preview");
    let gutter = f64::from(app.harness().theme().tokens.spacing.s4);
    assert!(
        preview.x0 >= gutter && preview.x1 <= 480.0 - gutter,
        "preview {preview:?} retains content gutters"
    );
    assert!(
        preview.y0 >= toggle.y1,
        "content starts below shared chrome"
    );

    let action_ids = [
        "theme-apply",
        "theme-export",
        "theme-duplicate",
        "theme-reset",
        "theme-delete",
    ];
    let actions: Vec<_> = action_ids.iter().map(|id| rect(&mut app, id)).collect();
    for (i, action) in actions.iter().enumerate() {
        assert!(
            action.width() > 0.0 && action.x0 >= 0.0 && action.x1 <= 480.0,
            "{} remains inside the window: {action:?}",
            action_ids[i]
        );
        near(
            action.height(),
            actions[0].height(),
            "actions share control height",
        );
        for other in &actions[i + 1..] {
            assert!(
                disjoint(*action, *other),
                "actions overlap: {action:?} / {other:?}"
            );
        }
    }
    assert!(
        actions.iter().any(|r| r.y0 > actions[0].y0),
        "narrow action row wraps instead of clipping"
    );
    let dropdowns = named(&mut app, "Dropdown");
    assert!(
        !dropdowns.is_empty(),
        "production choices are shared dropdowns"
    );
    for dropdown in dropdowns {
        assert!(dropdown.width() > 0.0);
        near(
            dropdown.height(),
            actions[0].height(),
            "choices and actions share control height",
        );
    }
    app.harness()
        .expect(&SemanticExpectation::named("nav-shell-sidebar").enabled(true))
        .unwrap();
    app.harness().click_named("nav-shell-sidebar").unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("themes-gallery").enabled(true))
        .unwrap();
    let shown = rect(&mut app, "nav-shell-sidebar");
    near(toggle.x0, shown.x0, "sidebar control keeps its slot");
    near(
        toggle.width(),
        shown.width(),
        "sidebar control keeps its size",
    );
}

#[test]
fn gallery_cards_wrap_to_available_width_with_measured_gaps() {
    for width in [480, 1280] {
        let mut app = frame("gallery", (width, 2400), 1.0);
        let gap = f64::from(app.harness().theme().tokens.spacing.s2);
        let cards: Vec<_> = Preset::ALL
            .iter()
            .map(|preset| rect(&mut app, &format!("preset-{}", preset.id())))
            .collect();
        assert!(cards.len() >= 3, "all preset choices must be observed");
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
                        other.y0 - card.y1 >= gap,
                        "related cards need the theme's gap"
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
    let mut two = frame("editor", (480, 4000), 2.0);
    let toggle = rect(&mut two, "nav-shell-sidebar");
    let preview = rect(&mut two, "theme-preview");
    assert!(
        preview.y0 >= toggle.y1,
        "preview remains below scaled shared chrome"
    );
    let ids = [
        "theme-apply",
        "theme-export",
        "theme-duplicate",
        "theme-reset",
        "theme-delete",
    ];
    let actions: Vec<_> = ids.iter().map(|id| rect(&mut two, id)).collect();
    for (i, action) in actions.iter().enumerate() {
        assert!(
            disjoint(toggle, *action),
            "action must not cover the sidebar control"
        );
        assert!(action.x0 >= 0.0 && action.x1 <= 480.0);
        assert!(
            action.height() > rect(&mut one, ids[i]).height(),
            "large type increases its real control height"
        );
        for other in &actions[i + 1..] {
            assert!(disjoint(*action, *other));
        }
    }
    let name = rect(&mut two, "theme-name");
    assert!(
        name.y0 >= actions.iter().map(|r| r.y1).fold(0.0, f64::max),
        "form starts below wrapped actions"
    );
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
