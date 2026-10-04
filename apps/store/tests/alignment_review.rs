//! Geometry regressions for the Store's pages.
//!
//! The Store composes its pages from shared `ui` widgets; these tests lay the
//! real `StoreApp` out through the same `sync → build_layout → compute_layout
//! → apply_layout` cycle the app runs and assert on the resulting rectangles
//! against the theme's spacing scale.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
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

fn last_named(root: &dyn Widget, label: &str) -> Rect {
    let mut out = None;
    walk(root, &mut |w| {
        if w.accessibility().and_then(|n| n.name).as_deref() == Some(label) {
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
    let mut app = StoreApp::new();
    store::shots::stage(&mut app, Some(scenario));
    app
}

/// The catalogue's toolbar and its sections share one column and one centre line.
#[test]
fn catalogue_toolbar_and_sections_share_one_column() {
    let theme = ui::Theme::dark();
    let app = lay_out(store::shots::seeded_app(), (1200.0, 800.0));
    let summary = first_named(&*app, "6 apps from 2 sources");
    let search = by_test_id(&*app, "search-open");
    let refresh = by_test_id(&*app, "store.refresh");
    close(
        summary.center().y,
        search.center().y,
        "the summary sits on the centre line of the actions",
    );
    close(
        search.center().y,
        refresh.center().y,
        "the actions share a centre line",
    );

    // Every section starts on the toolbar's leading edge and ends where the
    // toolbar's trailing action ends.
    let sections = by_widget_name(&*app, "Section");
    assert!(!sections.is_empty(), "the catalogue shows its groups");
    for section in &sections {
        close(
            section.x0,
            summary.x0,
            "sections start on the toolbar's leading edge",
        );
        close(
            section.x1,
            refresh.x1,
            "sections end on the toolbar's trailing edge",
        );
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

/// The catalogue has no title of its own, no permanent status pill, and its toolbar fits a narrow window.
#[test]
fn catalogue_has_no_title_no_status_pill_and_a_toolbar_that_fits() {
    let app = lay_out(store::shots::seeded_app(), (520.0, 820.0));
    let mut names = Vec::new();
    walk(&*app, &mut |w| {
        if let Some(name) = w.accessibility().and_then(|node| node.name) {
            names.push(name);
        }
    });
    assert!(
        !names.iter().any(|name| name == "Apps"),
        "no page title of its own: {names:?}"
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
    for id in [
        "search-open",
        "store.remotes",
        "store.safety",
        "store.refresh",
    ] {
        let rect = by_test_id(&*app, id);
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= 520.0,
            "{id} {rect:?} must fit a 520 px window"
        );
    }
}

/// Secondary pages share one header composition: back control, title, body.
#[test]
fn secondary_pages_share_one_header_composition() {
    let theme = ui::Theme::dark();
    let group = f64::from(theme.tokens.spacing.rhythm.group);
    let remotes = lay_out(staged("remotes"), (1200.0, 800.0));
    let safety = lay_out(staged("moderation"), (1200.0, 800.0));

    let remotes_title = first_named(&*remotes, "Flatpak remotes");
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
    let remotes_back = by_test_id(&*remotes, "store.remotes.back");
    let safety_back = by_test_id(&*safety, "store.safety.back");
    close(
        remotes_title.y0 - remotes_back.y1,
        group,
        "remotes: back control to title is one group step",
    );
    close(
        safety_title.y0 - safety_back.y1,
        group,
        "trust: back control to title is one group step",
    );
    close(
        remotes_back.x0,
        safety_back.x0,
        "back controls start on one edge",
    );
}

/// The details page stacks its identity at the token gap and aligns its facts with the hero.
#[test]
fn details_page_aligns_identity_and_facts() {
    let theme = ui::Theme::dark();
    let tight = f64::from(theme.tokens.spacing.s2);
    for width in [480.0, 1200.0] {
        let app = lay_out(staged("detail"), (width, 900.0));
        let title = last_named(&*app, "Notes");
        let source = first_named(&*app, "Tilo app, version 0.1.0");
        close(
            source.y0 - title.y1,
            tight,
            "name to its source line is one tight step",
        );
        let back = by_test_id(&*app, "store.detail.back");
        let icon = by_widget_name(&*app, "Avatar")[0];
        assert!(
            icon.y0 > back.y1,
            "the app icon {icon:?} stays below the back control {back:?}"
        );
        close(
            icon.x0,
            back.x0,
            "icon and back control start on the content edge",
        );
        let details = first_named(&*app, "Details");
        let trust = first_named(&*app, "Trust");
        assert!(
            details.y1 < trust.y0,
            "sections stack in reading order without overlap"
        );
        assert_eq!(
            by_widget_name(&*app, "PropertyList").len(),
            1,
            "one property list, so every value starts on one edge"
        );
    }
}

/// Every page starts on the catalogue's leading edge, at any width, and the
/// sections of a reading page end on one trailing edge.
#[test]
fn pages_share_one_leading_edge_and_reading_pages_one_trailing_edge() {
    for width in [800.0, 1200.0] {
        let catalogue = lay_out(store::shots::seeded_app(), (width, 800.0));
        let edge = first_named(&*catalogue, "6 apps from 2 sources").x0;
        for (scenario, title, back_id) in [
            ("detail", "Notes", "store.detail.back"),
            ("remotes", "Flatpak remotes", "store.remotes.back"),
            ("moderation", "Trust and moderation", "store.safety.back"),
        ] {
            let page = lay_out(staged(scenario), (width, 1400.0));
            let first = if scenario == "detail" {
                by_widget_name(&*page, "Avatar")[0]
            } else {
                first_named(&*page, title)
            };
            close(
                first.x0,
                edge,
                &format!("{scenario} starts on the catalogue's edge at {width}"),
            );
            let back = by_test_id(&*page, back_id);
            close(back.x0, edge, "the way back starts on the same edge");
            let sections = by_widget_name(&*page, "Section");
            let trailing = sections[0].x1;
            for section in &sections {
                close(section.x1, trailing, "sections end on one trailing edge");
            }
        }
    }
}
