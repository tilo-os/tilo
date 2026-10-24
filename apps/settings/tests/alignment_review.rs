//! Geometry regressions from the 2026-09-16 UI alignment review (unit A3).
//!
//! Every assertion reads the production widget tree after a real layout pass
//! — the same `sync → build_layout → compute_layout → apply_layout` cycle the
//! app runs — and compares rectangles against the theme's spacing scale, so a
//! spacer or padding regression shows up as a number, not as a screenshot
//! someone has to squint at.

#![allow(
    clippy::panic,
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

/// The settings row that holds the control `id`: its box, and whether its
/// control sits under its text (the row is too narrow for both side by
/// side). A row paints its label itself, from the box's leading edge.
fn row_of_control(root: &dyn Widget, id: &str) -> (Rect, bool) {
    let mut out = None;
    walk(root, &mut |widget| {
        let Some(row) = widget
            .as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::ListRow>())
        else {
            return;
        };
        let mut contains = false;
        walk(widget, &mut |child| contains |= child.test_id() == Some(id));
        if contains {
            out = Some((widget.get_rect(), row.trailing_wrapped()));
        }
    });
    out.unwrap_or_else(|| panic!("no settings row holds {id:?}"))
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
    // 1800 px is above the scaled breakpoint, so the sidebar stays docked.
    let large = config
        .app()
        .with_category(Category::Appearance)
        .with_width_hint(1800.0);
    let large = lay_out_scaled(large, (1800.0, 900.0), 2.0);
    let rows: Vec<_> = crate::support::nav_rows(&*large)
        .into_iter()
        .filter(|row| Category::ALL.iter().any(|c| c.label() == row.label))
        .collect();
    assert!(
        !rows.is_empty(),
        "the sidebar is still docked at 1800 px / 200 %"
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
    // Case `described_row_keeps_its_explanation_and_centres_its_control`.
    {
        let config = crate::support::ScratchConfig::new();
        let app = config.app().with_category(Category::Accessibility);
        let app = lay_out(app, WIDE);
        // The row speaks its label and the line explaining it together, so
        // the explanation belongs to its row, not to the next one.
        let mut described = None;
        let mut next = None;
        walk(&*app, &mut |w| {
            if w.name() != "ListRow" {
                return;
            }
            let node = w.accessibility();
            let name = node
                .as_ref()
                .and_then(|n| n.name.clone())
                .unwrap_or_default();
            let said = format!("{name} {}", node.and_then(|n| n.value).unwrap_or_default());
            if name.starts_with("High contrast") {
                described = Some((w.get_rect(), said));
            } else if name.starts_with("Reduce motion") && next.is_none() {
                next = Some(w.get_rect());
            }
        });
        let (row, name) = described.expect("the High contrast row");
        assert!(
            name.contains("Maximise contrast for low-vision comfort"),
            "the explanation is part of its row: {name}"
        );
        let next = next.expect("the Reduce motion row");
        assert!(
            next.y0 >= row.y1 - 0.5,
            "the next row starts below the explanation: {row:?} then {next:?}"
        );
        let toggles = by_widget_name(&*app, "Toggle");
        let row_toggle = toggles
            .iter()
            .find(|t| t.y0 >= row.y0 && t.y1 <= row.y1)
            .copied()
            .expect("the High contrast row carries a toggle");
        same_centre_line(
            row.center().y,
            row_toggle.center().y,
            "row vs toggle centre",
        );
    }

    // Case `hero_places_title_beside_its_icon`.
    {
        let hero = FrameHarness::new(
            ui::State::new(),
            settings::widgets::hero(ui::icons::Icon::Monitor, "Device name"),
            (600, 200),
        );
        let icon = by_widget_name(hero.root_widget(), "IconWidget")[0];
        let title = first_named(hero.root_widget(), "Device name");
        assert!(
            title.x0 >= icon.x1,
            "title {title:?} must sit beside the icon {icon:?}, not wrap beneath it"
        );
        same_centre_line(
            title.center().y,
            icon.center().y,
            "title vs icon centre line",
        );
    }
}

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
        let (row, wrapped) = row_of_control(&*app, "region.currency");
        let control = by_test_id(&*app, "region.currency");
        assert!(!wrapped, "wide: the currency chooser sits beside its label");
        same_centre_line(
            row.center().y,
            control.center().y,
            "inline row vs dropdown centre",
        );
        // …inside the toolkit's readable measure (720) rather than the full
        // detail width, so the pair never drifts ~600 px apart.
        let card_inner = control.x1 - row.x0;
        assert!(
            card_inner <= 720.0 + 0.5,
            "label to control span {card_inner} exceeds the readable measure"
        );
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

        // A narrow window is a maximum, not a fixed width: the chooser stays
        // inside its row and the window.
        let narrow = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(NARROW.0);
        let narrow = lay_out(narrow, NARROW);
        let control = by_test_id(&*narrow, "region.currency");
        let (row, _) = row_of_control(&*narrow, "region.currency");
        assert!(
            control.x1 <= row.x1 + 0.5 && control.x1 <= f64::from(NARROW.0),
            "narrow chooser {control:?} stays inside its row {row:?}"
        );
    }

    // Case `region_choice_rows_switch_layout_as_one_card`.
    // The Formats card lays all six choice rows out the same way, decided at
    // layout from the width it really gets: beneath their labels where the
    // widest label and chooser do not fit side by side (a phone at large
    // text), beside them once they do. No row is left inline while its
    // neighbours wrap.
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
        let phone = (320.0, 844.0);
        let app = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(phone.0);
        let app = lay_out(app, phone);
        for (id, _label) in rows {
            let control = by_test_id(&*app, id);
            let (row, wrapped) = row_of_control(&*app, id);
            assert!(
                wrapped,
                "phone: {id} {control:?} sits beneath its label in {row:?}"
            );
            assert!(
                control.x0 >= row.x0 - 0.5 && control.x1 <= row.x1 + 0.5,
                "phone: {id} {control:?} stays inside its row {row:?}"
            );
        }

        // A page wide enough for the chooser and a label (beside the docked
        // category list): every row sits inline.
        let medium = (900.0, 820.0);
        let app = config
            .app()
            .with_category(Category::RegionFormats)
            .with_width_hint(medium.0);
        let app = lay_out(app, medium);
        for (id, _label) in rows {
            let control = by_test_id(&*app, id);
            let (row, wrapped) = row_of_control(&*app, id);
            assert!(
                !wrapped,
                "medium: {id} {control:?} sits beside its label in {row:?}"
            );
            same_centre_line(
                control.center().y,
                row.center().y,
                &format!("medium: {id} shares its row's centre line"),
            );
        }
    }

    // Case `large_text_scales_the_shell_breakpoint_with_the_type`.
    // Large text needs the room a narrow window does: a 900 px window docks
    // the sidebar at 1× and collapses to the single content screen at 200 %,
    // where the breakpoint and the sidebar width have grown with the type.
    {
        let config = crate::support::ScratchConfig::new();
        let one_x = config
            .app()
            .with_category(Category::Appearance)
            .with_width_hint(900.0);
        let one_x = lay_out(one_x, (900.0, 900.0));
        assert!(!one_x.is_narrow(), "900 px docks the sidebar at 1×");
        assert!(
            !crate::support::nav_rows(&*one_x).is_empty(),
            "1×: the docked sidebar lists its categories"
        );

        let two_x = config
            .app()
            .with_category(Category::Appearance)
            .with_width_hint(900.0);
        let two_x = lay_out_scaled(two_x, (900.0, 900.0), 2.0);
        assert!(
            two_x.is_narrow(),
            "900 px is below the 200 % breakpoint, so the shell shows one screen"
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
        sp.rhythm.group, sp.s6,
        "the section gap is the group rhythm, the s6 step"
    );
    for (category, viewport) in [
        (Category::About, WIDE),
        (Category::Network, WIDE),
        (Category::Users, WIDE),
        (Category::RegionFormats, NARROW),
    ] {
        let app = config
            .app()
            .with_category(category)
            .with_width_hint(viewport.0);
        let app = lay_out(app, viewport);
        // Top-level sections only: a section nested inside another is that
        // section's own content, not a sibling.
        let mut sections: Vec<Rect> = by_widget_name(&*app, "Section")
            .into_iter()
            .filter(|r| r.width() > 0.0 && r.height() > 0.0)
            .collect();
        sections.sort_by(|a, b| a.y0.total_cmp(&b.y0));
        // A section that fills its parent exactly (an untitled card holding one
        // section) is the same slot, not a sibling.
        sections.dedup();
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
