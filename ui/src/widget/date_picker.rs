//! `DatePicker` widget — month-grid calendar picker.
//!
//! Renders a header (prev-month chevron, "Month YYYY" label, next-month
//! chevron) above a 7-column day grid. The first column is **Monday** —
//! this is the locale assumption pinned by
//! `date_picker_first_day_of_week_is_monday` so a future locale change
//! must touch both the test and `first_visible_col`.
//!
//! State is owned by the widget itself, not the CRDT, because the
//! selected date and the visible month are presentation concerns
//! (analogous to scroll position) — the consumer wires `on_change` to
//! propagate the chosen `NaiveDate` into whatever Automerge document
//! they manage.

use automerge::{AutoCommit, ObjId};
use chrono::{Datelike, NaiveDate, Local};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// Number of weeks rendered. Six rows covers every possible month layout
/// (a 31-day month starting on Sunday is the worst case — 6 rows · 7 cols
/// = 42 cells, leaving room for the spill-in/spill-out days from
/// neighbouring months).
const GRID_ROWS: usize = 6;
const GRID_COLS: usize = 7;

/// Weekday headings rendered above the grid. Pinned to a Monday-first
/// locale by the `date_picker_first_day_of_week_is_monday` test.
const WEEKDAY_LABELS: [&str; GRID_COLS] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

pub struct DatePicker {
    selected: NaiveDate,
    month_view: (i32, u32),
    on_change: Option<Box<dyn FnMut(NaiveDate)>>,
    rect: Rect,
    day_rects: Vec<(NaiveDate, Rect)>,
    prev_button_rect: Rect,
    next_button_rect: Rect,
    last_cursor: Point,
}

impl DatePicker {
    pub fn new(initial: NaiveDate) -> Self {
        Self {
            selected: initial,
            month_view: (initial.year(), initial.month()),
            on_change: None,
            rect: Rect::ZERO,
            day_rects: Vec::new(),
            prev_button_rect: Rect::ZERO,
            next_button_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
        }
    }

    pub fn selected(&self) -> NaiveDate {
        self.selected
    }

    pub fn month_view(&self) -> (i32, u32) {
        self.month_view
    }

    pub fn on_change<F: FnMut(NaiveDate) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    /// Advance the visible month by one. Wraps December → January of the
    /// next year. Does **not** move the selected date — the user must
    /// click a day to change selection.
    pub fn advance_month(&mut self) {
        let (y, m) = self.month_view;
        self.month_view = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    }

    /// Retreat the visible month by one. Wraps January → December of the
    /// previous year.
    pub fn retreat_month(&mut self) {
        let (y, m) = self.month_view;
        self.month_view = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    }

    /// Column index (0..=6) of the first-of-month for the currently
    /// visible month, where Monday = 0 and Sunday = 6. Tests pin the
    /// locale assumption against this helper so the test failure
    /// message is unambiguous if a future locale change forgets to
    /// update the weekday header row.
    pub fn first_visible_col(&self) -> usize {
        let (y, m) = self.month_view;
        // `from_ymd_opt(_, _, 1)` only fails on invalid year ranges
        // (i32::MIN…). For any realistic year this is `Some`; fall back
        // to column 0 if it ever isn't, so the picker still renders.
        NaiveDate::from_ymd_opt(y, m, 1)
            .map(|d| d.weekday().num_days_from_monday() as usize)
            .unwrap_or(0)
    }

    /// The date that fills cell `(row, col)` in the visible grid. Days
    /// before the first-of-month spill in from the previous month;
    /// days past the end spill into the next month.
    fn cell_date(&self, row: usize, col: usize) -> Option<NaiveDate> {
        let (y, m) = self.month_view;
        let first = NaiveDate::from_ymd_opt(y, m, 1)?;
        let offset = (row * GRID_COLS + col) as i64 - self.first_visible_col() as i64;
        first.checked_add_signed(chrono::Duration::days(offset))
    }
}

const HEADER_HEIGHT: f64 = 32.0;
const WEEKDAY_HEIGHT: f64 = 20.0;
const CELL_SIZE: f64 = 32.0;
const CHEVRON_WIDTH: f64 = 32.0;

impl Widget for DatePicker {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let width = CELL_SIZE * GRID_COLS as f64;
        let height = HEADER_HEIGHT + WEEKDAY_HEIGHT + CELL_SIZE * GRID_ROWS as f64;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(width as f32),
                    height: Dimension::length(height as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );

        // Header chevrons sit at the left and right edges of the header.
        self.prev_button_rect = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + CHEVRON_WIDTH,
            self.rect.y0 + HEADER_HEIGHT,
        );
        self.next_button_rect = Rect::new(
            self.rect.x1 - CHEVRON_WIDTH,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + HEADER_HEIGHT,
        );

        // Build the per-day hit-rects for the visible grid. Cell widths
        // are derived from the host rect so the grid stretches/shrinks
        // gracefully if layout assigns a different width than the
        // build_layout default.
        let grid_top = self.rect.y0 + HEADER_HEIGHT + WEEKDAY_HEIGHT;
        let cell_w = self.rect.width() / GRID_COLS as f64;
        let cell_h = (self.rect.y1 - grid_top) / GRID_ROWS as f64;
        self.day_rects.clear();
        for row in 0..GRID_ROWS {
            for col in 0..GRID_COLS {
                if let Some(date) = self.cell_date(row, col) {
                    let x0 = self.rect.x0 + col as f64 * cell_w;
                    let y0 = grid_top + row as f64 * cell_h;
                    let cell_rect = Rect::new(x0, y0, x0 + cell_w, y0 + cell_h);
                    self.day_rects.push((date, cell_rect));
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let text_size = tokens.typography.scale.sm;
        let text_color = tokens.colors.text_primary;
        let dim_color = tokens.colors.text_dim;

        // Surface fill so the picker is visually grouped on any backdrop.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );

        // Header label: "Month YYYY".
        let (y, m) = self.month_view;
        let month_name = month_name_en(m);
        let header_label = format!("{} {}", month_name, y);
        let (label_w, _) = text_renderer.measure(&header_label, text_size, None);
        let label_x = self.rect.center().x - (label_w as f64) / 2.0;
        let label_y = self.rect.y0 + (HEADER_HEIGHT - text_size as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            &header_label,
            text_size,
            text_color,
            transform * Affine::translate((label_x, label_y)),
            None,
        );

        // Chevrons — drawn as text glyphs so we don't need a vector path.
        let (lw, _) = text_renderer.measure("\u{2039}", text_size, None);
        let (rw, _) = text_renderer.measure("\u{203A}", text_size, None);
        text_renderer.draw_text(
            scene,
            "\u{2039}",
            text_size,
            text_color,
            transform
                * Affine::translate((
                    self.prev_button_rect.center().x - (lw as f64) / 2.0,
                    self.prev_button_rect.y0
                        + (HEADER_HEIGHT - text_size as f64) / 2.0,
                )),
            None,
        );
        text_renderer.draw_text(
            scene,
            "\u{203A}",
            text_size,
            text_color,
            transform
                * Affine::translate((
                    self.next_button_rect.center().x - (rw as f64) / 2.0,
                    self.next_button_rect.y0
                        + (HEADER_HEIGHT - text_size as f64) / 2.0,
                )),
            None,
        );

        // Weekday header row (Mo Tu We Th Fr Sa Su).
        let cell_w = self.rect.width() / GRID_COLS as f64;
        let weekday_y = self.rect.y0
            + HEADER_HEIGHT
            + (WEEKDAY_HEIGHT - text_size as f64) / 2.0;
        for (col, label) in WEEKDAY_LABELS.iter().enumerate() {
            let (lw, _) = text_renderer.measure(label, text_size, None);
            let cx = self.rect.x0 + col as f64 * cell_w + cell_w / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                text_size,
                dim_color,
                transform * Affine::translate((cx - (lw as f64) / 2.0, weekday_y)),
                None,
            );
        }

        // Today (for the "1 px ring" highlight).
        let today = Local::now().date_naive();
        let (vy, vm) = self.month_view;

        // Day cells.
        for (date, cell_rect) in &self.day_rects {
            let is_in_view_month = date.year() == vy && date.month() == vm;
            let is_selected = *date == self.selected;
            let is_today = *date == today && is_in_view_month;

            // Selected pill — filled rounded rect under the day text.
            if is_selected {
                let pad = cell_rect.height().min(cell_rect.width()) * 0.1;
                let inner = Rect::new(
                    cell_rect.x0 + pad,
                    cell_rect.y0 + pad,
                    cell_rect.x1 - pad,
                    cell_rect.y1 - pad,
                );
                let shape = RoundedRect::from_rect(inner, tokens.radii.sm as f64);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[ACCENT_PRIMARY_IDX],
                    None,
                    &shape,
                );
            }

            // Today ring — drawn on top of the cell (and below the text)
            // so it's visible even when the day is selected.
            if is_today {
                let pad = cell_rect.height().min(cell_rect.width()) * 0.1;
                let inner = Rect::new(
                    cell_rect.x0 + pad,
                    cell_rect.y0 + pad,
                    cell_rect.x1 - pad,
                    cell_rect.y1 - pad,
                );
                let shape = RoundedRect::from_rect(inner, tokens.radii.sm as f64);
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &shape,
                );
            }

            // Day number text — centred, colour depends on
            // selected / in-view-month / out-of-view-month.
            let day_str = date.day().to_string();
            let color = if is_selected {
                Color::WHITE
            } else if is_in_view_month {
                text_color
            } else {
                dim_color
            };
            let (tw, _) = text_renderer.measure(&day_str, text_size, None);
            let tx = cell_rect.center().x - (tw as f64) / 2.0;
            let ty = cell_rect.y0 + (cell_rect.height() - text_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                &day_str,
                text_size,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                if self.prev_button_rect.contains(pt) {
                    self.retreat_month();
                    *handled = true;
                    return;
                }
                if self.next_button_rect.contains(pt) {
                    self.advance_month();
                    *handled = true;
                    return;
                }
                // Day cell hit — clone first so we can mutate `selected`
                // and call the callback without borrowing `self.day_rects`.
                let hit = self
                    .day_rects
                    .iter()
                    .find(|(_, r)| r.contains(pt))
                    .map(|(d, _)| *d);
                if let Some(date) = hit {
                    self.selected = date;
                    // If the click was on a spill-in/out day, slide the
                    // visible month to match the new selection — matches
                    // every common picker UX.
                    self.month_view = (date.year(), date.month());
                    if let Some(cb) = self.on_change.as_mut() {
                        cb(date);
                    }
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "DatePicker" }
    fn test_id(&self) -> Option<&str> { Some("date_picker") }
    fn get_rect(&self) -> Rect { self.rect }
    fn is_focusable(&self) -> bool { true }
}

fn month_name_en(m: u32) -> &'static str {
    match m {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use taffy::prelude::TaffyMaxContent;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, MouseButton};

    fn modifiers() -> winit::event::Modifiers {
        winit::event::Modifiers::default()
    }

    fn fake_device_id() -> DeviceId {
        // `DeviceId::dummy()` is only available behind a winit unstable
        // feature; the regular winit constructor isn't pub. We construct
        // one via `unsafe { std::mem::zeroed() }` because the value is
        // never inspected by `handle_event` for these widgets.
        unsafe { std::mem::zeroed() }
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: fake_device_id(),
            position: PhysicalPosition::new(x, y),
        }
    }

    fn mouse_pressed() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn date_picker_initial_state_shows_selected_month() {
        let dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
        assert_eq!(dp.month_view(), (2024, 6));
    }

    #[test]
    fn date_picker_next_month_advances() {
        let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
        dp.advance_month();
        assert_eq!(dp.month_view(), (2024, 7));

        // Advance from December → January of the next year.
        let mut dp_dec = DatePicker::new(NaiveDate::from_ymd_opt(2024, 12, 1).unwrap());
        dp_dec.advance_month();
        assert_eq!(dp_dec.month_view(), (2025, 1));
    }

    #[test]
    fn date_picker_clicking_day_updates_selection() {
        // Pin the visible month so day_rects are deterministic.
        let initial = NaiveDate::from_ymd_opt(2024, 6, 1).unwrap();

        // Capture on_change emissions.
        let captured: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));
        let captured_cb = captured.clone();
        let mut dp = DatePicker::new(initial).on_change(move |d| {
            *captured_cb.borrow_mut() = Some(d);
        });

        // Drive apply_layout manually — build a 1-node taffy tree, run
        // build_layout, then apply_layout at origin (0,0).
        let theme = crate::theme::Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = dp.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(node, taffy::Size::MAX_CONTENT)
            .unwrap();
        dp.apply_layout(&taffy, node, Point::ZERO);

        // Find the cell rect for June 7 2024 and click its centre.
        let target = NaiveDate::from_ymd_opt(2024, 6, 7).unwrap();
        let cell = dp
            .day_rects
            .iter()
            .find(|(d, _)| *d == target)
            .map(|(_, r)| *r)
            .expect("June 7 2024 cell exists in the visible grid");

        let pt = cell.center();
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let mods = modifiers();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;

        dp.handle_event(
            &cursor_moved(pt.x, pt.y),
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        dp.handle_event(
            &mouse_pressed(),
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );

        assert_eq!(dp.selected(), target);
        assert_eq!(*captured.borrow(), Some(target));
    }

    #[test]
    fn date_picker_first_day_of_week_is_monday() {
        let dp = DatePicker::new(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let first_col: usize = dp.first_visible_col();
        assert_eq!(first_col, 0_usize);
    }
}
