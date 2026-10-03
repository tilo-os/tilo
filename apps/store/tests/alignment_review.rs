//! Geometry regressions from the 2026-09-16 UI alignment review (unit A3).
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
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::Rect;

fn spacing() -> ui::tokens::Spacing {
    ui::tokens::Spacing::default()
}

fn lay_out(mut app: StoreApp, (w, h): (f32, f32)) -> Framed<StoreApp> {
    app.set_layout_width_hint(f64::from(w));
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

/// Browse summary, remotes heading, compact hero and secondary headers share one composition.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn browse_and_secondary_pages_share_one_composition() {
    // Case `browse_summary_shares_the_pickers_centre_line`.
    {
        let app = store::shots::seeded_app();
        let app = lay_out(app, (1200.0, 800.0));
        let source = by_test_id(&*app, "store.source-filter");
        let category = by_test_id(&*app, "store.category-filter");
        let summary = first_named(&*app, "6 apps from 2 configured sources");
        close(
            source.center().y,
            category.center().y,
            "pickers share a centre line",
        );
        close(
            summary.center().y,
            source.center().y,
            "summary text centred on the pickers, not on the label+picker stack",
        );
        // The labels row and the controls row resolve to the same column edges.
        close(
            first_named(&*app, "Source").x0,
            source.x0,
            "Source label over its picker",
        );
        close(
            first_named(&*app, "Category").x0,
            category.x0,
            "Category label over its picker",
        );
        assert!(
            summary.x0 > category.x1,
            "summary {summary:?} sits in its own third column after {category:?}"
        );
    }

    // Case `remotes_heading_hugs_its_first_card`.
    {
        let sp = spacing();
        let mut app = store::shots::seeded_app();
        app.show_remotes_for_shot();
        let app = lay_out(app, (1200.0, 800.0));
        let heading = first_named(&*app, "Configured remotes");
        let remote_title = first_named(&*app, "Flathub");
        // The card is the outermost container surface around the remote's title
        // that starts below the heading (inner flex wrappers sit inside its
        // padding).
        #[expect(
            clippy::suspicious_operation_groupings,
            reason = "edge-by-edge containment test: each side compares like with like"
        )]
        let card = by_widget_name(&*app, "Container")
            .into_iter()
            .filter(|c| {
                c.y0 > heading.y1
                    && c.x0 <= remote_title.x0
                    && c.x1 >= remote_title.x1
                    && c.y0 <= remote_title.y0
                    && c.y1 >= remote_title.y1
            })
            .max_by(|a, b| a.area().partial_cmp(&b.area()).unwrap())
            .expect("a card surface holds the configured remote");
        close(
            card.y0 - heading.y1,
            f64::from(sp.s2),
            "heading to its first card",
        );
        // The form card above and the heading are one page step apart, which is
        // larger than the heading's own gap to its cards.
        let form_title = first_named(&*app, "Add a remote");
        #[expect(
            clippy::suspicious_operation_groupings,
            reason = "edge-by-edge containment test: each side compares like with like"
        )]
        let form_card = by_widget_name(&*app, "Container")
            .into_iter()
            .filter(|c| {
                c.x0 <= form_title.x0
                    && c.x1 >= form_title.x1
                    && c.y0 <= form_title.y0
                    && c.y1 >= form_title.y1
                    && c.y1 < heading.y0
            })
            .max_by(|a, b| a.area().partial_cmp(&b.area()).unwrap())
            .expect("a card surface holds the add-remote form");
        close(
            heading.y0 - form_card.y1,
            f64::from(sp.s3),
            "form card to list heading",
        );
    }

    // Case `compact_detail_hero_keeps_one_section_gap_between_artwork_and_title`.
    {
        let sp = spacing();
        let mut app = store::shots::seeded_app();
        app.focus_detail_for_shot("so.tilo.notes");
        let app = lay_out(app, (480.0, 900.0));
        let artwork = by_widget_name(&*app, "StoreArtwork")[0];
        let title = first_named(&*app, "Notes");
        close(
            title.y0 - artwork.y1,
            f64::from(sp.s6),
            "artwork to title (no spacer stacked on the column gap)",
        );
        let back = by_test_id(&*app, "store.detail.back");
        assert!(
            artwork.y0 > back.y1,
            "artwork {artwork:?} stays below the back control {back:?}"
        );
    }

    // Case `secondary_pages_share_one_header_composition`.
    {
        let sp = spacing();
        let mut remotes = store::shots::seeded_app();
        remotes.show_remotes_for_shot();
        let remotes = lay_out(remotes, (1200.0, 800.0));
        let mut safety = store::shots::seeded_app();
        safety.show_safety_for_shot();
        let safety = lay_out(safety, (1200.0, 800.0));

        let remotes_title = first_named(&*remotes, "Flatpak remotes");
        let safety_title = first_named(&*safety, "Trust & moderation");
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
            f64::from(sp.s3),
            "remotes: back control to title",
        );
        close(
            safety_title.y0 - safety_back.y1,
            f64::from(sp.s3),
            "safety: back control to title",
        );
        let remotes_subtitle = first_named(
            &*remotes,
            "Repositories are machine-scoped and every change runs through the package worker.",
        );
        close(
            remotes_subtitle.y0 - remotes_title.y1,
            f64::from(sp.s2),
            "title to explanation",
        );
    }
}

// ---- round two (2026-09-17) ----

/// The status pill never covers card actions and its landing zone follows text scale.
#[test]
fn status_pill_stays_off_card_actions_at_every_text_scale() {
    // Case `status_pill_never_floats_over_a_card_action`.
    // The floating status pill — and the hover band that reveals it — lives at
    // the catalogue's bottom-left, on the cards' own leading edge. Every card
    // puts its actions at its trailing edge, so at no window size does the pill
    // sit on an Install, Open or Details button (round one's T7: at 800 × 700
    // the bottom-right pill covered the last card's Install).
    {
        let theme = ui::Theme::dark();
        let sp = spacing();
        for (w, h) in [
            (1200.0, 800.0),
            (1366.0, 768.0),
            (800.0, 700.0),
            (480.0, 900.0),
        ] {
            let mut app = store::shots::seeded_app();
            app.arm_status_bubble_for_shots();
            let app = lay_out(app, (w, h));
            let mut tr = TextRenderer::new();
            let pill = app.status_bubble_rect(&theme, &mut tr);
            close(
                pill.y1,
                f64::from(h) - f64::from(sp.s4),
                &format!("{w}×{h}: pill sits one s4 above the bottom edge"),
            );
            assert!(
                pill.x0 >= 0.0 && pill.width() > 0.0,
                "{w}×{h}: pill {pill:?} is inside the window"
            );
            let buttons = by_widget_name(&*app, "Button");
            assert!(
                !buttons.is_empty(),
                "{w}×{h}: the catalogue shows card actions"
            );
            for button in buttons {
                let overlap = pill.intersect(button);
                assert!(
                    overlap.width() <= 0.0 || overlap.height() <= 0.0,
                    "{w}×{h}: pill {pill:?} overlaps a button {button:?}"
                );
            }
            // On the catalogue's leading edge — shared with the masthead's own
            // leading control on a compact window — never inside the card column.
            let leftmost_button_x0 = by_widget_name(&*app, "Button")
                .iter()
                .map(|b| b.x0)
                .fold(f64::MAX, f64::min);
            assert!(
                pill.x0 <= leftmost_button_x0 + 0.5,
                "{w}×{h}: pill {pill:?} starts at or before every button ({leftmost_button_x0})"
            );
        }
    }

    // Case `status_pill_landing_zone_follows_the_text_scale`.
    // The landing zone under the last row is the pill's own band (its height
    // plus an `s4` above and below), so it follows the text scale instead of
    // restating one number.
    {
        let sp = spacing();
        let one = StoreApp::status_bubble_band(&ui::Theme::dark());
        let two = StoreApp::status_bubble_band(&ui::Theme::dark().with_text_scale(2.0));
        assert!(
            one >= 30.0 + 2.0 * sp.s4,
            "1× band ({one}) reserves the pill floor plus two margins"
        );
        assert!(
            two > one,
            "200 % band ({two}) is taller than the 1× band ({one})"
        );
    }
}
