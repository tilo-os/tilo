//! Geometry regressions from the 2026-09-16 UI alignment review (unit A3).
//!
//! Every assertion reads the production widget tree after a real layout pass
//! — the same `sync → build_layout → compute_layout → apply_layout` cycle the
//! app runs — and compares rectangles against the theme's spacing scale, so a
//! spacer or padding regression shows up as a number, not as a screenshot
//! someone has to squint at.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::SettingsApp;
use settings::categories::Category;
use settings::sidebar::SHELL_FIELD_INSET;
use settings::widgets::spacing;
use ui::widget::Widget;
use vello::kurbo::Rect;

use crate::composition_balance::lay_out_at;

const WIDE: (f32, f32) = (1200.0, 760.0);
const NARROW: (f32, f32) = (440.0, 820.0);

fn lay_out(widget: &mut dyn Widget, (w, h): (f32, f32)) {
    lay_out_at(widget, w, h);
}

fn walk(w: &dyn Widget, f: &mut dyn FnMut(&dyn Widget)) {
    f(w);
    for child in w.children() {
        walk(child, f);
    }
}

/// Rects of every widget whose accessible name is exactly `label`.
fn named(root: &dyn Widget, label: &str) -> Vec<Rect> {
    let mut out = Vec::new();
    walk(root, &mut |w| {
        if w.accessibility().and_then(|n| n.name).as_deref() == Some(label) {
            out.push(w.get_rect());
        }
    });
    out
}

fn first_named(root: &dyn Widget, label: &str) -> Rect {
    *named(root, label)
        .first()
        .unwrap_or_else(|| panic!("no widget named {label:?}"))
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

#[test]
fn sidebar_pill_and_section_headers_share_the_shell_search_edge() {
    let sp = spacing();
    let mut app = SettingsApp::new();
    lay_out(&mut app, WIDE);
    let search = by_test_id(&app, "settings.search");
    let rows = by_widget_name(&app, "SidebarRow");
    assert_eq!(rows.len(), Category::ALL.len());
    for row in &rows {
        // The selection pill is inset `s2` from the row; it must land on the
        // search field's left edge, so field and highlight read as one column.
        close(
            row.x0 + f64::from(sp.s2),
            search.x0,
            "pill left edge vs search field",
        );
        close(
            row.x1 - f64::from(sp.s2),
            search.x1,
            "pill right edge vs search field",
        );
    }
    // Section headers start on the pill's content edge (the icon column).
    let content_left = rows[0].x0 + f64::from(sp.s2) + f64::from(sp.s3);
    for header in ["CONNECTIVITY", "DEVICES", "SYSTEM"] {
        close(
            first_named(&app, header).x0,
            content_left,
            &format!("{header} header x vs row content"),
        );
    }
}

#[test]
fn sidebar_rhythm_uses_three_spacing_steps() {
    let sp = spacing();
    let mut app = SettingsApp::new();
    lay_out(&mut app, WIDE);
    let search = by_test_id(&app, "settings.search");
    let connectivity = first_named(&app, "CONNECTIVITY");
    let devices = first_named(&app, "DEVICES");
    let network = by_test_id(&app, "Network");
    let bluetooth = by_test_id(&app, "Bluetooth");
    let devices_sync = by_test_id(&app, "Devices & Sync");

    // The list starts `s2` below the shell's search band (the band pads the
    // field by the same inset the pill aligns to).
    close(
        connectivity.y0 - search.y1,
        f64::from(SHELL_FIELD_INSET + sp.s2),
        "first header below the search band",
    );
    // A header hugs its rows (`s2`); related rows sit `s1` apart; sections
    // are `s6` apart — the header is always nearer its own rows than the
    // section above it.
    close(network.y0 - connectivity.y1, f64::from(sp.s2), "header to first row");
    close(bluetooth.y0 - network.y1, f64::from(sp.s1), "row to row");
    close(devices.y0 - bluetooth.y1, f64::from(sp.s6), "section gap");
    close(devices_sync.y0 - devices.y1, f64::from(sp.s2), "second header to its row");
    assert!(
        devices.y0 - bluetooth.y1 > network.y0 - connectivity.y1,
        "a header must be closer to its rows than to the previous section"
    );
}

#[test]
fn described_row_caption_hugs_its_label_not_the_next_row() {
    let sp = spacing();
    let mut app = SettingsApp::with_selected(Category::Appearance);
    lay_out(&mut app, WIDE);
    let label = first_named(&app, "High contrast");
    let caption = first_named(&app, "Maximise contrast for low-vision comfort");
    let next_label = first_named(&app, "Reduce motion");
    let toggles = by_widget_name(&app, "Toggle");
    let row_toggle = toggles
        .iter()
        .find(|t| t.y0 <= label.center().y && label.center().y <= t.y1)
        .copied()
        .expect("the High contrast row carries a toggle");
    let next_toggle = toggles
        .iter()
        .find(|t| t.y0 <= next_label.center().y && next_label.center().y <= t.y1)
        .copied()
        .expect("the Reduce motion row carries a toggle");
    // The toggle is the tallest thing in the row, so its edges are the row's.
    close(caption.y0 - row_toggle.y1, f64::from(sp.s1), "caption below its row");
    close(
        next_toggle.y0 - caption.y1,
        f64::from(settings::widgets::ROW_GAP),
        "next row below the caption",
    );
    assert!(
        next_toggle.y0 - caption.y1 > caption.y0 - row_toggle.y1,
        "the caption must belong to its label, not float towards the next row"
    );
    // Label and toggle share one centre line.
    close(label.center().y, row_toggle.center().y, "label vs toggle centre");
}

#[test]
fn hero_places_title_beside_its_icon_with_a_caption_gap() {
    let sp = spacing();
    let mut hero = settings::widgets::hero(ui::icons::Icon::Monitor, "Device name", "qOS 1.0");
    lay_out(hero.as_mut(), (600.0, 200.0));
    let icon = by_widget_name(hero.as_ref(), "IconWidget")[0];
    let title = first_named(hero.as_ref(), "Device name");
    let subtitle = first_named(hero.as_ref(), "qOS 1.0");
    assert!(
        title.x0 >= icon.x1,
        "title {title:?} must sit beside the icon {icon:?}, not wrap beneath it"
    );
    close(subtitle.x0, title.x0, "subtitle shares the title's left edge");
    close(subtitle.y0 - title.y1, f64::from(sp.s1), "title to subtitle");
    assert!(
        title.y0 < icon.y1 && subtitle.y1 > icon.y0,
        "text block {title:?}/{subtitle:?} overlaps the icon band {icon:?}"
    );
}

#[test]
fn region_pane_keeps_a_readable_form_measure_and_stays_fluid_when_narrow() {
    let mut app = SettingsApp::with_selected(Category::RegionFormats);
    lay_out(&mut app, WIDE);
    let label = first_named(&app, "Currency");
    let control = by_test_id(&app, "region.currency");
    // Label and trailing chooser stay on one centre line…
    close(label.center().y, control.center().y, "label vs dropdown centre");
    // …inside the toolkit's readable form measure (720) rather than the full
    // detail width, so the pair no longer drifts ~600 px apart.
    let card_inner = control.x1 - label.x0;
    assert!(
        card_inner <= 720.0,
        "label→control span {card_inner} exceeds the readable measure"
    );
    assert!(
        control.x0 - label.x1 < 480.0,
        "label/control gap {} is still the full-width drift",
        control.x0 - label.x1
    );
    // Every chooser in the card shares the same trailing edge.
    for id in [
        "region.date",
        "region.time",
        "region.measurement",
        "region.number_style",
        "region.first_day",
    ] {
        close(by_test_id(&app, id).x1, control.x1, &format!("{id} trailing edge"));
    }

    // A narrow window is a maximum, not a fixed width: the card still fills it.
    let mut narrow = SettingsApp::with_selected(Category::RegionFormats)
        .with_width_hint(NARROW.0)
        .with_deterministic_hardware_for_shot();
    lay_out(&mut narrow, NARROW);
    let control = by_test_id(&narrow, "region.currency");
    assert!(
        control.x1 > f64::from(NARROW.0) - 60.0,
        "narrow chooser {control:?} must reach the card's trailing edge"
    );
}
