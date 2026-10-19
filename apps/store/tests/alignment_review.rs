//! Geometry regressions for the Store's pages.
//!
//! The Store composes its pages from shared `ui` widgets; these tests lay the
//! real `StoreApp` out through the same `sync → build_layout → compute_layout
//! → apply_layout` cycle the app runs and assert on the resulting rectangles
//! against the theme's spacing scale.

#![allow(
    clippy::panic,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use store::StoreApp;
use ui::headless::Framed;
use ui::widget::Widget;
use vello::kurbo::Rect;

fn lay_out(app: StoreApp, (w, h): (f32, f32)) -> Framed<StoreApp> {
    Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((w, h)),
        ui::Theme::dark(),
    )
}

fn walk(w: &dyn Widget, f: &mut dyn FnMut(&dyn Widget)) {
    f(w);
    for child in w.children() {
        walk(child, f);
    }
}

fn first_named(root: &dyn Widget, label: &str) -> Rect {
    let mut out = None;
    walk(root, &mut |w| {
        if out.is_none() && w.accessibility().and_then(|n| n.name).as_deref() == Some(label) {
            out = Some(w.get_rect());
        }
    });
    out.unwrap_or_else(|| panic!("no widget named {label:?}"))
}

fn by_test_id(root: &dyn Widget, id: &str) -> Rect {
    let mut out = None;
    walk(root, &mut |w| {
        if out.is_none() && w.test_id() == Some(id) {
            out = Some(w.get_rect());
        }
    });
    out.unwrap_or_else(|| panic!("no widget with test id {id:?}"))
}

fn by_widget_name(root: &dyn Widget, name: &str) -> Vec<Rect> {
    let mut out = Vec::new();
    walk(root, &mut |w| {
        if w.name() == name {
            out.push(w.get_rect());
        }
    });
    out
}

fn close(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() < 0.5,
        "{what}: expected {b}, got {a} (difference {})",
        a - b
    );
}

fn staged(scenario: &str) -> StoreApp {
    let mut app = store::fixture::seeded_app();
    app.stage_scene(Some(scenario));
    app
}

/// The catalogue's toolbar and its sections share one column and one centre line.
#[test]
fn catalogue_toolbar_and_sections_share_one_column() {
    let theme = ui::Theme::dark();
    let mut store = store::fixture::seeded_app();
    store.set_items(store::fixture::fixture_catalog());
    let app = lay_out(store, (1200.0, 800.0));
    let views = by_test_id(&*app, "store.views");
    let remotes = by_test_id(&*app, "store.remotes");
    let trust = by_test_id(&*app, "store.safety");
    // The actions stand in the window's header row, beside History.
    close(
        remotes.center().y,
        trust.center().y,
        "the actions share a centre line",
    );
    assert!(
        trust.y1 <= views.y0,
        "the actions sit in the header row above the catalogue"
    );

    // Every section starts on the views' leading edge.
    let sections = by_widget_name(&*app, "Section");
    assert!(!sections.is_empty(), "Discover shows its shelves");
    let trailing = sections[0].x1;
    for section in &sections {
        close(
            section.x0,
            views.x0,
            "sections start on the views' leading edge",
        );
        close(section.x1, trailing, "sections end on one trailing edge");
    }
    // The scroll bar's gutter stays free: no card reaches the window edge.
    let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
    for card in by_widget_name(&*app, "Card") {
        assert!(
            card.x1 <= 1200.0 - gutter,
            "card {card:?} must not reach the scroll bar gutter"
        );
    }
}

/// The catalogue is titled like every page, has no permanent status pill, and its actions fit a narrow window.
#[test]
fn catalogue_has_a_title_no_status_pill_and_actions_that_fit() {
    let app = lay_out(store::fixture::seeded_app(), (520.0, 820.0));
    let mut names = Vec::new();
    walk(&*app, &mut |w| {
        if let Some(name) = w.accessibility().and_then(|node| node.name) {
            names.push(name);
        }
    });
    assert!(
        names.iter().any(|name| name == "Discover"),
        "the header row names the page: {names:?}"
    );
    assert_eq!(
        names.iter().filter(|name| *name == "App Store").count(),
        1,
        "only the window itself carries the app's name: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("apps available")),
        "no persistent status pill: {names:?}"
    );
    for id in ["store.package.import", "store.remotes", "store.safety"] {
        let rect = by_test_id(&*app, id);
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= 520.0,
            "{id} {rect:?} must fit a 520 px window"
        );
    }
}

/// Secondary pages share one header composition: the window's title, then the body.
#[test]
fn secondary_pages_share_one_header_composition() {
    let remotes = lay_out(staged("remotes"), (1200.0, 800.0));
    let safety = lay_out(staged("moderation"), (1200.0, 800.0));

    let remotes_title = first_named(&*remotes, "Flatpak repositories");
    let safety_title = first_named(&*safety, "Trust and moderation");
    close(
        remotes_title.height(),
        safety_title.height(),
        "titles use one size",
    );
    close(
        remotes_title.y0,
        safety_title.y0,
        "titles start on one line",
    );
    let explanation = first_named(
        &*remotes,
        "Browse software from your configured repositories. Repositories supplied by this device stay available; repositories you add belong to your account.",
    );
    let form_title = first_named(&*remotes, "Add a repository");
    assert!(
        explanation.y0 >= remotes_title.y1,
        "repository scope explanation follows the shared header"
    );
    assert!(
        explanation.y1 < form_title.y0,
        "the complete explanation fits before the add form"
    );
    assert!(
        explanation.width() > 0.0 && explanation.height() > 0.0,
        "repository scope guidance is visible"
    );
}

/// An app's page stacks its identity at the token gap and lists its facts once.
#[test]
fn details_page_aligns_identity_and_facts() {
    let theme = ui::Theme::dark();
    let tight = f64::from(theme.tokens.spacing.s2);
    for width in [480.0, 1200.0] {
        let app = lay_out(staged("detail"), (width, 900.0));
        // The page's own heading, not the window's title of the same name.
        let title = by_test_id(&*app, "store.detail.name");
        let developer = by_test_id(&*app, "store.detail.developer");
        close(
            developer.y0 - title.y1,
            tight,
            "name to its developer is one tight step",
        );
        let facts = first_named(&*app, "Facts");
        let permissions = first_named(&*app, "What it can do");
        assert!(
            facts.y1 < permissions.y0,
            "sections stack in reading order without overlap"
        );
        assert_eq!(
            by_widget_name(&*app, "PropertyList").len(),
            1,
            "one property list, so every value starts on one edge"
        );
    }
}

/// A publisher's pitch and previews precede the package facts, while the
/// complete description stays readable in every shipped theme.
#[test]
fn publisher_presentation_precedes_package_facts() {
    for theme in [
        ui::Theme::dark(),
        ui::Theme::light(),
        ui::Theme::high_contrast(),
    ] {
        for width in [480.0, 1200.0] {
            let mut store = staged("detail");
            store.set_image_store(Some(store::fixture::artwork_store()));
            let app = Framed::new(
                ui::State::new(),
                store,
                ui::headless::viewport((width, 1600.0)),
                theme.clone(),
            );
            let pitch = by_test_id(&*app, "store.detail.tagline");
            let previews = first_named(&*app, "Screenshots");
            let description = by_test_id(&*app, "store.detail.description.0");
            let facts = first_named(&*app, "Facts");
            let first_preview = by_test_id(&*app, "store.screenshot.so.tilo.notes.0");
            assert!(first_preview.x0 >= 0.0 && first_preview.x1 <= width);
            for index in 0..3 {
                let preview = by_test_id(&*app, &format!("store.screenshot.so.tilo.notes.{index}"));
                // One horizontal gallery: every screenshot on one row, the
                // ones past the edge a scroll away.
                close(
                    preview.y0,
                    first_preview.y0,
                    "the screenshots share one row",
                );
                assert!(preview.x0 >= first_preview.x0);
            }
            assert!(pitch.y1 < previews.y0, "the pitch precedes artwork");
            assert!(previews.y1 < description.y0, "artwork precedes long copy");
            assert!(description.y1 < facts.y0, "copy precedes package facts");
            for rect in [pitch, description] {
                assert!(rect.width() > 0.0 && rect.height() > 0.0);
                assert!(rect.x0 >= 0.0 && rect.x1 <= width);
            }
        }
    }
}

/// Every page starts on the catalogue's leading edge, at any width, and the
/// sections of a reading page end on one trailing edge.
#[test]
fn pages_share_one_leading_edge_and_reading_pages_one_trailing_edge() {
    for width in [800.0, 1200.0] {
        // As tall as the pages, so neither scrolls and reserves a bar's gutter.
        let catalogue = lay_out(store::fixture::seeded_app(), (width, 1400.0));
        let edge = by_test_id(&*catalogue, "store.views").x0;
        for scenario in ["detail", "remotes", "moderation"] {
            let page = lay_out(staged(scenario), (width, 1400.0));
            let first = if scenario == "detail" {
                by_widget_name(&*page, "Avatar")[0]
            } else {
                by_widget_name(&*page, "Section")[0]
            };
            close(
                first.x0,
                edge,
                &format!("{scenario} starts on the catalogue's edge at {width}"),
            );
            let sections = by_widget_name(&*page, "Section");
            let trailing = sections[0].x1;
            for section in &sections {
                close(section.x1, trailing, "sections end on one trailing edge");
            }
        }
    }
}
