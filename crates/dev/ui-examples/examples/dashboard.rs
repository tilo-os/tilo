//! Interactive dashboard — a "very complex" example application
//! to eyeball the incremental-rendering work from the 2026-05-23
//! perf plan. Opens a single window with:
//!
//!   - 11 animating widgets in the header (3 `ProgressBars` +
//!     8 Spinners) — exercises `needs_animation_frame` + the
//!     gate.
//!   - 8 panels × 20 rows = ~160 cached leaves (Text + Button
//!     + Avatar + Badge in rotation) — exercises the per-widget
//!       `Scene::append` path.
//!   - A footer with extra Avatars / Badges + a `ToggleGroup` so
//!     a click target is always within reach.
//!
//! Run with:
//! ```
//! cargo run -p ui-examples --release --example ui-lab -- dashboard
//! ```
//!
//! Set `TILO_UI_PERF=1` to see per-frame phase timings printed
//! to stderr (the App's redraw cycle is instrumented in `app.rs`).

use std::fmt::Write as _;

use tilo_ui::i18n::I18nManager;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::{
    Avatar, Badge, Button, Column, ProgressBar, Row, ScrollAxis, Scrollable, Spinner, Text,
    ToggleGroup, Widget,
};
use tilo_ui::{App, State};

fn leak_label(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Rows-per-panel. Bumped up so the dashboard's total height
/// comfortably overflows the window — that's what makes the
/// outer `Scrollable` actually have content to scroll over.
const ROWS_PER_PANEL: usize = 40;

fn panel(title_key: &'static str, idx: usize) -> Box<dyn Widget> {
    // Each panel is its own intrinsic-height column so its
    // children stack at their natural sum-of-heights instead of
    // collapsing to the parent's percent-allocated zero inside a
    // Block layout. Without this the panel columns flatten to
    // ~0 px and the dashboard fits the window in one frame —
    // which would defeat the whole point of having a Scrollable.
    let mut col = Column::new()
        .with_intrinsic_height()
        .push(Box::new(Text::new(title_key).with_size(18.0)));
    for i in 0..ROWS_PER_PANEL {
        let row_idx = idx.saturating_mul(ROWS_PER_PANEL).saturating_add(i);
        let label_key = leak_label(&format!("dash_lbl_{row_idx}"));
        match i % 3 {
            0 => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Button::new(
                            leak_label(&format!("dash_btn_{row_idx}")),
                            leak_label(&format!("dash_btnlbl_{row_idx}")),
                            None,
                        ))),
                ));
            }
            1 => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Badge::count(
                            u32::try_from(i).unwrap_or(u32::MAX).saturating_mul(3),
                        ))),
                ));
            }
            _ => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Avatar::from_initials(&format!("U{i}"), 24.0))),
                ));
            }
        }
    }
    Box::new(col)
}

pub fn run(_args: &[String]) {
    let wake_scope = tilo_ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    I18nManager::init("en-US");
    let mut ftl = String::new();
    ftl.push_str("dash_title = Tilo Desktop Dashboard\n");
    for i in 0..8 {
        let _ = writeln!(ftl, "dash_p{i} = Panel {i}");
    }
    for row_idx in 0..(8 * ROWS_PER_PANEL) {
        let _ = writeln!(ftl, "dash_lbl_{row_idx} = Metric {row_idx}");
        let _ = writeln!(ftl, "dash_btn_{row_idx} = btn_{row_idx}");
        let _ = writeln!(ftl, "dash_btnlbl_{row_idx} = Open");
    }
    I18nManager::add_resource("en-US", ftl);

    let state = State::new();

    let header = Row::new()
        .push(Box::new(Text::new("dash_title").with_size(24.0)))
        .push(Box::new(ToggleGroup::new(
            vec![
                Segment::new("Day"),
                Segment::new("Week"),
                Segment::new("Month"),
            ],
            1,
        )))
        .push(Box::new(ProgressBar::new(0.4)))
        .push(Box::new(ProgressBar::indeterminate()))
        .push(Box::new(ProgressBar::indeterminate()))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)));

    let titles = [
        "dash_p0", "dash_p1", "dash_p2", "dash_p3", "dash_p4", "dash_p5", "dash_p6", "dash_p7",
    ];
    let mut panels = Row::new();
    for (i, t) in titles.iter().enumerate() {
        panels = panels.push(panel(t, i));
    }

    let footer = Row::new()
        .push(Box::new(Text::new("dash_title")))
        .push(Box::new(Avatar::from_initials("DB", 28.0)))
        .push(Box::new(Avatar::from_initials("QD", 28.0)))
        .push(Box::new(Avatar::from_initials("AL", 28.0)))
        .push(Box::new(Avatar::from_initials("PB", 28.0)))
        .push(Box::new(Badge::count(42)))
        .push(Box::new(Badge::count(7)))
        .push(Box::new(Badge::count(120)));

    // Vertical-only scrolling. The outer Column uses
    // `with_intrinsic_height` so taffy gives it the sum of its
    // children's heights instead of clamping to viewport; the
    // panels themselves stack to ~40 rows × 8 panels ≈ 1300 px
    // each, comfortably over a 1080 p screen.
    let root: Box<dyn Widget> = Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(header))
                .push(Box::new(panels))
                .push(Box::new(footer)),
        )),
    );

    let app = App::from_scoped_parts(wake_scope, state, root);
    app.run();
}
