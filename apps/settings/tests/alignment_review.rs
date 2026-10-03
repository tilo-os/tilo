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

/// The sidebar lists every destination as one selectable row at the toolkit's
/// row height, sharing one leading edge, and grows with large text.
#[test]
fn sidebar_rows_share_an_edge_and_the_toolkit_row_height() {
    // Case `rows_share_one_leading_edge_and_one_height_at_1x`.
    let config = crate::support::ScratchConfig::new();
    let metrics = ui::tokens::Metrics::default();
    let app = config.app().with_category(Category::Appearance);
    let app = lay_out(app, WIDE);
    let rows: Vec<_> = crate::support::nav_rows(&*app)
        .into_iter()
        .filter(|row| Category::ALL.iter().any(|c| c.label() == row.label))
        .collect();
    assert!(
        rows.len() >= Category::ALL.len() / 2,
        "the docked sidebar lists its categories ({} visible)",
        rows.len()
    );
    for row in &rows {
        close(
            row.rect.height(),
            f64::from(metrics.nav_row_height),
            "1× sidebar row height",
        );
        close(row.rect.x0, rows[0].rect.x0, "rows share one leading edge");
    }
    let selected: Vec<_> = rows.iter().filter(|row| row.selected).collect();
    assert_eq!(selected.len(), 1, "exactly one category is selected");
    assert_eq!(selected[0].label, Category::Appearance.label());

    // Case `rows_grow_with_large_text`.
    // 1600 px is above the scaled breakpoint, so the sidebar stays docked.
    let large = config
        .app()
        .with_category(Category::Appearance)
        .with_width_hint(1600.0);
    let large = lay_out_scaled(large, (1600.0, 900.0), 2.0);
    let rows: Vec<_> = crate::support::nav_rows(&*large)
        .into_iter()
        .filter(|row| Category::ALL.iter().any(|c| c.label() == row.label))
        .collect();
    assert!(
        !rows.is_empty(),
        "the sidebar is still docked at 1600 px / 200 %"
    );
    let shortest = rows
        .iter()
        .map(|r| r.rect.height())
        .fold(f64::MAX, f64::min);
    assert!(
        shortest > f64::from(metrics.nav_row_height),
        "at 200 % every row ({shortest}) must be taller than the 1× row"
    );
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
        let next_label = first_named(&*app, "Reduce transparency");
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
            .expect("the Reduce transparency row carries a toggle");
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

        // A window wide enough for the chooser and a label: still one screen
        // (below the docking breakpoint), but every row sits inline.
        let medium = (600.0, 820.0);
        let app = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(medium.0)
            .with_deterministic_hardware_for_shot();
        let app = lay_out(app, medium);
        for (id, label) in rows {
            let control = by_test_id(&*app, id);
            let label = first_named(&*app, label);
            assert!(
                control.x0 > label.x1,
                "medium: {id} {control:?} sits beside its label {label:?}"
            );
            same_centre_line(
                control.center().y,
                label.center().y,
                &format!("medium: {id} shares its label's centre line"),
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
            !crate::support::nav_rows(&*one_x).is_empty(),
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

/// One rhythm per pane (HIG §7b): every section sits exactly one `s6`, the
/// "before the next group" step the toolkit's section gap names, below the
/// section above it, on every pane and in both layout classes, and no two
/// sections ever touch.
#[test]
fn pane_sections_stack_on_the_s6_step_and_never_touch() {
    let config = crate::support::ScratchConfig::new();
    let sp = spacing();
    assert_eq!(
        settings::widgets::SECTION_GAP,
        sp.s6,
        "the section gap is the s6 step, not an authored pixel value"
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
        // Top-level sections only: a section nested inside another is that
        // section's own content, not a sibling.
        let mut sections: Vec<Rect> = by_widget_name(&*app, "Section")
            .into_iter()
            .filter(|r| r.width() > 0.0 && r.height() > 0.0)
            .collect();
        sections.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        let outer: Vec<Rect> = sections
            .iter()
            .copied()
            .filter(|c| {
                !sections
                    .iter()
                    .any(|o| o != c && o.contains(c.center()) && o.area() > c.area())
            })
            .collect();
        assert!(
            outer.len() >= 2,
            "{category:?} at {viewport:?}: need at least two sections to measure a rhythm, found {}",
            outer.len()
        );
        for pair in outer.windows(2) {
            let gap = pair[1].y0 - pair[0].y1;
            assert!(
                gap >= f64::from(sp.s1),
                "{category:?} at {viewport:?}: sections touch (gap {gap})"
            );
            // The column places section slots exactly `s6` apart; taffy rounds
            // each node's text-driven height and position to whole pixels on
            // its own, so the gap the eye sees is `s6` ± 1 px, never another
            // step.
            let step = f64::from(sp.s6);
            assert!(
                (gap - step).abs() <= 1.0,
                "{category:?} at {viewport:?}: section to section: expected {step} (± 1 px of \
                 text-height rounding), got {gap}"
            );
        }
    }
}
