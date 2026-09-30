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

use settings::categories::Category;
use settings::sidebar::SHELL_FIELD_INSET;
use settings::widgets::spacing;
use ui::headless::{FrameHarness, Framed};
use ui::widget::Widget;
use vello::kurbo::Rect;

use crate::composition_balance::lay_out_at;

/// The production layout cycle under a large-text preference: the theme
/// carries `text_scale` and the renderer is told the same scale, exactly as
/// the live host, the shared launcher and the headless harness do.
fn lay_out_scaled<T: Widget + 'static>(
    widget: T,
    (w, h): (f32, f32),
    text_scale: f32,
) -> Framed<T> {
    Framed::new(
        ui::State::new(),
        widget,
        ui::headless::viewport((w, h)),
        ui::Theme::dark().with_text_scale(text_scale),
    )
}

const WIDE: (f32, f32) = (1200.0, 760.0);
const NARROW: (f32, f32) = (440.0, 820.0);

fn lay_out<T: Widget + 'static>(widget: T, (w, h): (f32, f32)) -> Framed<T> {
    lay_out_at(widget, w, h)
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

/// Two centre lines agree to within the layout engine's resolution. Taffy
/// rounds every node to whole pixels, so an odd-height line box centred in
/// an even-height row lands its centre exactly half a pixel off the row's:
/// that half pixel is the engine's rounding step, not a misalignment, and is
/// the largest difference this accepts. Used only where a text line box is
/// compared against a control's centre; every other measure keeps [`close`].
fn same_centre_line(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() <= 0.5,
        "{what}: expected {b}, got {a} (difference {})",
        a - b
    );
}

/// Sidebar edge, spacing rhythm and pointer targets.
#[test]
fn sidebar_rows_share_the_search_edge_rhythm_and_targets() {
    // Case `sidebar_pill_and_section_headers_share_the_shell_search_edge`.
    {
        let config = crate::support::ScratchConfig::new();
        let sp = spacing();
        let app = config.app();
        let app = lay_out(app, WIDE);
        let search = by_test_id(&*app, "settings.search");
        let rows = by_widget_name(&*app, "SidebarRow");
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
                first_named(&*app, header).x0,
                content_left,
                &format!("{header} header x vs row content"),
            );
        }
    }

    // Case `sidebar_rhythm_uses_three_spacing_steps`.
    {
        let config = crate::support::ScratchConfig::new();
        let sp = spacing();
        let app = config.app();
        let app = lay_out(app, WIDE);
        let search = by_test_id(&*app, "settings.search");
        let connectivity = first_named(&*app, "CONNECTIVITY");
        let devices = first_named(&*app, "DEVICES");
        let network = by_test_id(&*app, "Network");
        let bluetooth = by_test_id(&*app, "Bluetooth");
        let devices_sync = by_test_id(&*app, "Devices & Sync");

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
        close(
            network.y0 - connectivity.y1,
            f64::from(sp.s2),
            "header to first row",
        );
        close(bluetooth.y0 - network.y1, f64::from(sp.s1), "row to row");
        close(devices.y0 - bluetooth.y1, f64::from(sp.s6), "section gap");
        close(
            devices_sync.y0 - devices.y1,
            f64::from(sp.s2),
            "second header to its row",
        );
        assert!(
            devices.y0 - bluetooth.y1 > network.y0 - connectivity.y1,
            "a header must be closer to its rows than to the previous section"
        );
    }

    // Case `sidebar_rows_seat_the_pointer_target_at_1x_and_grow_with_large_text`.
    // A sidebar row seats the toolkit's minimum pointer target plus one `s1`
    // above and below it at 1× — exactly, so the pill/search-field alignment
    // round one established is untouched — and grows from its measured label
    // line under large text instead of letting a 28 px label overflow a 32 px
    // pill.
    {
        let config = crate::support::ScratchConfig::new();
        let metrics = ui::tokens::Metrics::default();
        let expected = f64::from(metrics.min_pointer_target + 2.0 * spacing().s1);
        let app = config.app().with_category(Category::Appearance);
        let app = lay_out(app, WIDE);
        let rows = by_widget_name(&*app, "SidebarRow");
        assert!(!rows.is_empty(), "the docked sidebar lists its categories");
        for row in &rows {
            close(row.height(), expected, "1× sidebar row height");
        }

        // 1600 px is above the scaled breakpoint, so the sidebar stays docked.
        let large = config
            .app()
            .with_category(Category::Appearance)
            .with_width_hint(1600.0);
        let large = lay_out_scaled(large, (1600.0, 900.0), 2.0);
        let rows = by_widget_name(&*large, "SidebarRow");
        assert!(
            !rows.is_empty(),
            "the sidebar is still docked at 1600 px / 200 %"
        );
        let shortest = rows.iter().map(Rect::height).fold(f64::MAX, f64::min);
        let tallest = rows.iter().map(Rect::height).fold(0.0, f64::max);
        assert!(
            shortest > expected,
            "at 200 % every row ({shortest}) must be taller than the 1× row ({expected})"
        );
        // Every label fits one 28 px line in the 528 px sidebar, so every row
        // seats exactly one line: a row measured against a narrower width than
        // it is drawn at would wrap "Device Privacy & Security" and stand two
        // lines tall around a single drawn line.
        assert!(
            tallest < 2.0 * expected,
            "at 200 % rows seat one line ({tallest} must be under {})",
            2.0 * expected
        );
    }
}

/// Row captions hug their label and the hero title sits beside its icon.
#[test]
fn row_captions_and_hero_follow_the_spacing_steps() {
    // Case `described_row_caption_hugs_its_label_not_the_next_row`.
    {
        let config = crate::support::ScratchConfig::new();
        let sp = spacing();
        let app = config.app().with_category(Category::Appearance);
        let app = lay_out(app, WIDE);
        let label = first_named(&*app, "High contrast");
        let caption = first_named(&*app, "Maximise contrast for low-vision comfort");
        let next_label = first_named(&*app, "Reduce motion");
        let toggles = by_widget_name(&*app, "Toggle");
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
        close(
            caption.y0 - row_toggle.y1,
            f64::from(sp.s1),
            "caption below its row",
        );
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
        close(
            label.center().y,
            row_toggle.center().y,
            "label vs toggle centre",
        );
    }

    // Case `hero_places_title_beside_its_icon_with_a_caption_gap`.
    {
        let sp = spacing();
        let hero = FrameHarness::new(
            ui::State::new(),
            settings::widgets::hero(ui::icons::Icon::Monitor, "Device name", "Tilo 1.0"),
            (600, 200),
        );
        let icon = by_widget_name(hero.root_widget(), "IconWidget")[0];
        let title = first_named(hero.root_widget(), "Device name");
        let subtitle = first_named(hero.root_widget(), "Tilo 1.0");
        assert!(
            title.x0 >= icon.x1,
            "title {title:?} must sit beside the icon {icon:?}, not wrap beneath it"
        );
        close(
            subtitle.x0,
            title.x0,
            "subtitle shares the title's left edge",
        );
        close(
            subtitle.y0 - title.y1,
            f64::from(sp.s1),
            "title to subtitle",
        );
        assert!(
            title.y0 < icon.y1 && subtitle.y1 > icon.y0,
            "text block {title:?}/{subtitle:?} overlaps the icon band {icon:?}"
        );
    }
}

/// Region form measure, choice rows and the large-text breakpoint.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn region_form_and_large_text_breakpoint_stay_fluid() {
    // Case `region_pane_keeps_a_readable_form_measure_and_stays_fluid_when_narrow`.
    {
        let config = crate::support::ScratchConfig::new();
        let app = config.app().with_category(Category::RegionFormats);
        let app = lay_out(app, WIDE);
        let label = first_named(&*app, "Currency");
        let control = by_test_id(&*app, "region.currency");
        // Label and trailing chooser stay on one centre line (to the engine's
        // half-pixel: the 13 px label's line box is odd-height since F2)…
        same_centre_line(
            label.center().y,
            control.center().y,
            "label vs dropdown centre",
        );
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
            close(
                by_test_id(&*app, id).x1,
                control.x1,
                &format!("{id} trailing edge"),
            );
        }

        // A narrow window is a maximum, not a fixed width: the card still fills it.
        let narrow = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(NARROW.0)
            .with_deterministic_hardware_for_shot();
        let narrow = lay_out(narrow, NARROW);
        let control = by_test_id(&*narrow, "region.currency");
        assert!(
            control.x1 > f64::from(NARROW.0) - 60.0,
            "narrow chooser {control:?} must reach the card's trailing edge"
        );
    }

    // Case `region_choice_rows_switch_layout_as_one_card`.
    // The Formats card composes all six choice rows the same way: beneath their
    // labels on a phone, beside them once the card can seat label and chooser on
    // one line. No row is left inline while its neighbours wrap.
    {
        let config = crate::support::ScratchConfig::new();
        let rows = [
            ("region.currency", "Currency"),
            ("region.date", "Date format"),
            ("region.time", "Time format"),
            ("region.measurement", "Measurement"),
            ("region.number_style", "Number format"),
            ("region.first_day", "First day of week"),
        ];
        let phone = (390.0, 844.0);
        let app = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(phone.0)
            .with_deterministic_hardware_for_shot();
        let app = lay_out(app, phone);
        let sp = spacing();
        for (id, label) in rows {
            let control = by_test_id(&*app, id);
            let label = first_named(&*app, label);
            assert!(
                control.y0 >= label.y1,
                "phone: {id} {control:?} sits beneath its label {label:?}"
            );
            close(
                control.x0,
                label.x0,
                &format!("phone: {id} starts on its label's left edge"),
            );
            // The chooser hugs its label by the caption gap, not the row gap.
            // Taffy rounds each sibling's position to whole pixels on its own, so
            // the gap between two adjacent edges can land one pixel either side of
            // the token; anything past that is a real gap defect.
            let gap = control.y0 - label.y1;
            assert!(
                gap >= 0.0 && gap <= f64::from(sp.s1) + 1.0,
                "phone: {id} caption gap {gap} must be the s1 step ({}) within one rounding pixel",
                sp.s1
            );
        }

        let app = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(NARROW.0)
            .with_deterministic_hardware_for_shot();
        let app = lay_out(app, NARROW);
        for (id, label) in rows {
            let control = by_test_id(&*app, id);
            let label = first_named(&*app, label);
            assert!(
                control.x0 > label.x1,
                "narrow: {id} {control:?} sits beside its label {label:?}"
            );
            same_centre_line(
                control.center().y,
                label.center().y,
                &format!("narrow: {id} shares its label's centre line"),
            );
        }
    }

    // Case `large_text_scales_the_shell_breakpoint_with_the_type`.
    // Large text needs the room a narrow window does: an 800 px window docks
    // the sidebar at 1× and collapses to the single content screen at 200 %,
    // where the breakpoint and the sidebar width have grown with the type.
    {
        let config = crate::support::ScratchConfig::new();
        let one_x = config
            .app()
            .with_category(Category::Appearance)
            .with_width_hint(800.0);
        let one_x = lay_out(one_x, (800.0, 900.0));
        assert!(!one_x.is_narrow(), "800 px docks the sidebar at 1×");
        assert!(
            !by_widget_name(&*one_x, "SidebarRow").is_empty(),
            "1×: the docked sidebar lists its categories"
        );

        let two_x = config
            .app()
            .with_category(Category::Appearance)
            .with_width_hint(800.0);
        let two_x = lay_out_scaled(two_x, (800.0, 900.0), 2.0);
        assert!(
            two_x.is_narrow(),
            "800 px is below the 200 % breakpoint, so the shell shows one screen"
        );
    }
}

// ---- round two (2026-09-17) ----

/// One rhythm per pane (HIG §7b): every card sits exactly one `s6` — the
/// "before the next group" step, the value [`settings::widgets::CARD_GAP`]
/// now names — below the card above it, on every pane and in both layout
/// classes, and no two cards ever touch. The heading block sits `s4` above
/// the first card. The earlier 20 px card gap belonged to no relationship on
/// the scale; a pane that mixes an off-scale gap with `s4`/`s6` reads as two
/// rhythms.
#[test]
fn pane_cards_stack_on_the_s6_step_and_never_touch() {
    let config = crate::support::ScratchConfig::new();
    let sp = spacing();
    assert_eq!(
        settings::widgets::CARD_GAP,
        sp.s6,
        "the card gap is the s6 step, not an authored pixel value"
    );
    for (category, viewport) in [
        (Category::About, WIDE),
        (Category::Network, WIDE),
        (Category::Users, WIDE),
        (Category::RegionFormats, NARROW),
    ] {
        let mut app = config
            .app()
            .with_category(category)
            .with_width_hint(viewport.0)
            .with_deterministic_hardware_for_shot();
        if viewport == NARROW {
            app = app.with_narrow_content_shot();
        }
        let app = lay_out(app, viewport);
        // Top-level cards only: a card nested inside another card (a hero row
        // inside a section) is that section's own content, not a sibling.
        let mut cards: Vec<Rect> = by_widget_name(&*app, "Card")
            .into_iter()
            .filter(|r| r.width() > 0.0 && r.height() > 0.0)
            .collect();
        cards.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        let outer: Vec<Rect> = cards
            .iter()
            .copied()
            .filter(|c| {
                !cards
                    .iter()
                    .any(|o| o != c && o.contains(c.center()) && o.area() > c.area())
            })
            .collect();
        assert!(
            outer.len() >= 2,
            "{category:?} at {viewport:?}: need at least two cards to measure a rhythm, found {}",
            outer.len()
        );
        for pair in outer.windows(2) {
            let gap = pair[1].y0 - pair[0].y1;
            assert!(
                gap >= f64::from(sp.s1),
                "{category:?} at {viewport:?}: cards touch (gap {gap})"
            );
            // The column places card slots exactly `s6` apart. A `Card`
            // reports its inner surface's rect, and taffy rounds each node's
            // text-driven height and position to whole pixels on its own, so
            // the surface can stand one pixel off its slot either way; the
            // gap the eye sees is `s6` ± 1 px, never another step.
            let step = f64::from(sp.s6);
            assert!(
                (gap - step).abs() <= 1.0,
                "{category:?} at {viewport:?}: card to card: expected {step} (± 1 px of \
                 text-height rounding), got {gap}"
            );
        }
        // The subtitle is the heading block's last line, so its bottom is the
        // block's bottom; the same whole-pixel rounding of a text line box
        // applies to it.
        let subtitle = first_named(&*app, &category.subtitle());
        let heading_gap = outer[0].y0 - subtitle.y1;
        assert!(
            (heading_gap - f64::from(sp.s4)).abs() <= 1.0,
            "{category:?} at {viewport:?}: heading block to first card: expected {} (± 1 px), got {heading_gap}",
            sp.s4
        );
    }
}
