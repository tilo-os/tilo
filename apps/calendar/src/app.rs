//! `CalendarApp` — the root widget. Owns the in-memory [`Model`], the
//! current view + selection, and a single [`BlockEditor`] used for the
//! rich-text **notes** attached to the selected event/task (the same
//! widget the Notes app is built on).
//!
//! Layout is hand-positioned (like the Notes `MainPanel`) rather than
//! delegated to taffy, because the month grid / week timeline / day
//! timeline are bespoke Vello canvases. The only taffy-registered child
//! is the notes editor, positioned absolutely inside the detail panel.
//!
//! Five-pass `Widget` contract:
//! - `sync`   — seed the CRDT on first frame, bind the notes editor to
//!              the selected item's block list, run the editor's sync.
//! - `build_layout` — register the editor node, return a full-bleed root.
//! - `apply_layout` — compute region rects, place the editor.
//! - `draw`   — paint sidebar + toolbar + the active view + detail panel,
//!              rebuilding the click hit-map.
//! - `handle_event` — route clicks against the hit-map, keyboard
//!              shortcuts, and forward to the editor when notes are active.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::BlockEditor;

use crate::date;
use crate::doc::CalDoc;
use crate::ics;
use crate::model::{seed_demo, Event, Id, ListSel, Model, Palette, Priority, Recurrence, View};

const SIDEBAR_W: f64 = 252.0;
const TOPBAR_H: f64 = 60.0;
const DETAIL_W: f64 = 392.0;
const DETAIL_PAD: f64 = 22.0;
const NOTES_W: f64 = DETAIL_W - 2.0 * DETAIL_PAD;
const NOTES_TOP_OFFSET: f64 = 296.0;
/// First/last hours shown in the week/day timelines.
const DAY_START: i64 = 7;
const DAY_END: i64 = 22;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sel {
    Event(Id),
    Task(Id),
}

/// A click target produced while drawing and resolved on the next press.
#[derive(Clone)]
enum Action {
    View(View),
    Today,
    Prev,
    Next,
    PickDate(NaiveDate),
    OpenDay(NaiveDate),
    OpenEvent(Id),
    ToggleTask(Id),
    List(ListSel),
    CloseDetail,
    Export,
    Import,
    NewEvent,
    /// Step the visible month by ±1 from the mini-month navigator.
    ShiftMonth(i32),
    CycleColor,
    CycleRecurrence,
    CycleReminder,
    ToggleSelectedTask,
}

pub struct CalendarApp {
    model: Model,
    today: NaiveDate,
    anchor: NaiveDate,
    view: View,
    list_sel: ListSel,
    selection: Option<Sel>,
    detail_open: bool,

    notes_editor: BlockEditor,
    bound: Option<Sel>,
    seeded: bool,

    rect: Rect,
    detail_rect: Rect,
    notes_origin: Point,
    editor_node: Option<NodeId>,

    hits: Vec<(Rect, Action)>,
    last_mouse: Point,
    status: Option<String>,
    dirty: bool,
}

impl CalendarApp {
    pub fn new() -> Self {
        let today = Local::now().date_naive();
        let model = seed_demo(today);
        // Default selection: the design-review event, so the first frame
        // shows the detail panel with real rich-text notes.
        let selection = model
            .events
            .iter()
            .find(|e| e.title.starts_with("Design review"))
            .or_else(|| model.events.first())
            .map(|e| Sel::Event(e.id));

        Self {
            model,
            today,
            anchor: today,
            view: View::Agenda,
            list_sel: ListSel::Today,
            selection,
            detail_open: selection.is_some(),
            notes_editor: BlockEditor::new(),
            bound: None,
            seeded: false,
            rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            notes_origin: Point::new(-10000.0, -10000.0),
            editor_node: None,
            hits: Vec::new(),
            last_mouse: Point::ZERO,
            status: None,
            dirty: true,
        }
    }

    // --- CRDT plumbing ---------------------------------------------------

    fn notes_list_for(&self, doc: &mut AutoCommit, sel: Sel) -> Option<ObjId> {
        match sel {
            Sel::Event(id) => CalDoc::event_notes(doc, id),
            Sel::Task(id) => CalDoc::task_notes(doc, id),
        }
    }

    fn seed_demo_notes(&self, doc: &mut AutoCommit) {
        // Curated notes so a freshly-seeded calendar shows real
        // documents, not empty editors, on first paint.
        let curated: [(&str, &[&str]); 2] = [
            (
                "Design review",
                &[
                    "# Agenda",
                    "Walk the month grid + week timeline.",
                    "Open questions:",
                    "Repeat glyph on recurring chips?",
                    "Confirm all-day strip height.",
                ],
            ),
            (
                "Team standup",
                &[
                    "# Standup notes",
                    "Yesterday: shipped ICS export.",
                    "Today: month-grid hit testing.",
                    "Blockers: none.",
                ],
            ),
        ];
        for (needle, lines) in curated {
            if let Some(e) = self.model.events.iter().find(|e| e.title.starts_with(needle)) {
                if let Some(list) = CalDoc::event_notes(doc, e.id) {
                    CalDoc::seed_notes_if_empty(doc, &list, lines);
                }
            }
        }
    }

    fn ensure_bound(&mut self, doc: &mut AutoCommit) {
        let sel = match self.selection {
            Some(s) if self.detail_open => s,
            _ => {
                if self.bound.is_some() {
                    self.notes_editor = BlockEditor::new();
                    self.bound = None;
                }
                return;
            }
        };
        if self.bound == Some(sel) {
            return;
        }
        if let Some(list) = self.notes_list_for(doc, sel) {
            let mut ed = BlockEditor::new();
            ed.bind_blocks(doc, list);
            self.notes_editor = ed;
            self.bound = Some(sel);
            self.dirty = true;
        }
    }

    // --- mutations -------------------------------------------------------

    fn select_event(&mut self, id: Id) {
        self.selection = Some(Sel::Event(id));
        self.detail_open = true;
        self.dirty = true;
    }

    fn toggle_task(&mut self, id: Id, doc: &mut AutoCommit) {
        if let Some(t) = self.model.task_mut(id) {
            t.done = !t.done;
            let snap = t.clone();
            CalDoc::update_task(doc, &snap);
            self.dirty = true;
        }
    }

    fn new_event(&mut self, doc: &mut AutoCommit) {
        let start = self
            .anchor
            .and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap());
        let ev = Event {
            id: Uuid::new_v4(),
            title: "New event".into(),
            start,
            end: start + Duration::hours(1),
            all_day: false,
            location: String::new(),
            color: self.model.next_event_order_color(),
            recurrence: Recurrence::None,
            reminder_min: Some(10),
            project: None,
        };
        let id = ev.id;
        CalDoc::update_event(doc, &ev);
        if let Some(list) = CalDoc::event_notes(doc, id) {
            CalDoc::seed_notes_if_empty(doc, &list, &["Add details…"]);
        }
        self.model.events.push(ev);
        self.model.events.sort_by_key(|e| e.start);
        self.select_event(id);
        self.status = Some("New event created".into());
    }

    fn export_ics(&mut self) {
        let ics = ics::export(&self.model);
        let path = std::env::temp_dir().join(ics::suggested_filename(self.anchor));
        match std::fs::write(&path, ics) {
            Ok(_) => {
                self.status = Some(format!(
                    "Exported {} events to {}",
                    self.model.events.len(),
                    path.display()
                ));
            }
            Err(e) => self.status = Some(format!("Export failed: {e}")),
        }
        self.dirty = true;
    }

    /// Import the bundled sample `.ics` (anchored near today) through the
    /// real [`ics::parse`] path, merging new events/tasks into the model
    /// and the CRDT. De-duped by (title, start) for events and by title
    /// for tasks so a repeated import is close to idempotent.
    fn import_ics(&mut self, doc: &mut AutoCommit) {
        let payload = ics::sample_ics(self.today);
        let (events, tasks) = ics::parse(&payload);
        let mut added_e = 0usize;
        for mut e in events {
            if self
                .model
                .events
                .iter()
                .any(|x| x.title == e.title && x.start == e.start)
            {
                continue;
            }
            e.id = Uuid::new_v4();
            CalDoc::update_event(doc, &e);
            if let Some(list) = CalDoc::event_notes(doc, e.id) {
                CalDoc::seed_notes_if_empty(doc, &list, &["Imported from .ics"]);
            }
            self.model.events.push(e);
            added_e += 1;
        }
        let mut added_t = 0usize;
        for mut t in tasks {
            if self.model.tasks.iter().any(|x| x.title == t.title) {
                continue;
            }
            t.id = Uuid::new_v4();
            CalDoc::update_task(doc, &t);
            self.model.tasks.push(t);
            added_t += 1;
        }
        self.model.events.sort_by_key(|e| e.start);
        self.status = Some(if added_e == 0 && added_t == 0 {
            "Sample .ics already imported".into()
        } else {
            format!("Imported {added_e} events · {added_t} tasks from sample.ics")
        });
        self.dirty = true;
    }

    fn cycle_event<F: Fn(&mut Event)>(&mut self, doc: &mut AutoCommit, f: F) {
        if let Some(Sel::Event(id)) = self.selection {
            if let Some(e) = self.model.event_mut(id) {
                f(e);
                let snap = e.clone();
                CalDoc::update_event(doc, &snap);
                self.dirty = true;
            }
        }
    }

    fn move_anchor(&mut self, days: i64) {
        self.anchor += Duration::days(days);
        self.dirty = true;
    }

    fn step_period(&mut self, dir: i32) {
        self.anchor = match self.view {
            View::Month => date::add_months(self.anchor, dir),
            View::Week => self.anchor + Duration::days(7 * dir as i64),
            View::Day => self.anchor + Duration::days(dir as i64),
            View::Agenda => self.anchor + Duration::days(7 * dir as i64),
        };
        self.dirty = true;
    }

    fn apply_action(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::View(v) => {
                self.view = v;
            }
            Action::Today => self.anchor = self.today,
            Action::Prev => self.step_period(-1),
            Action::Next => self.step_period(1),
            Action::PickDate(d) => self.anchor = d,
            Action::OpenDay(d) => {
                self.anchor = d;
                self.view = View::Day;
            }
            Action::OpenEvent(id) => self.select_event(id),
            Action::ToggleTask(id) => self.toggle_task(id, doc),
            Action::List(s) => {
                self.list_sel = s;
            }
            Action::CloseDetail => {
                self.detail_open = false;
                self.selection = None;
            }
            Action::Export => self.export_ics(),
            Action::Import => self.import_ics(doc),
            Action::ShiftMonth(n) => self.anchor = date::add_months(self.anchor, n),
            Action::NewEvent => self.new_event(doc),
            Action::CycleColor => {
                self.cycle_event(doc, |e| {
                    e.color = Palette::from_index(e.color.index() + 1);
                });
            }
            Action::CycleRecurrence => {
                self.cycle_event(doc, |e| e.recurrence = e.recurrence.cycle());
            }
            Action::CycleReminder => {
                self.cycle_event(doc, |e| {
                    e.reminder_min = match e.reminder_min {
                        None => Some(10),
                        Some(10) => Some(30),
                        Some(30) => Some(60),
                        _ => None,
                    };
                });
            }
            Action::ToggleSelectedTask => {
                if let Some(Sel::Task(id)) = self.selection {
                    self.toggle_task(id, doc);
                }
            }
        }
        self.dirty = true;
    }

    // ====================================================================
    // Drawing — each helper takes `&self` and returns its click targets.
    // ====================================================================

    fn draw_sidebar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let r = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + SIDEBAR_W, self.rect.y1);
        fill_rect(scene, t, c.surfaces.raised, r);
        stroke_line(
            scene,
            t,
            c.border_standard,
            Point::new(r.x1, r.y0),
            Point::new(r.x1, r.y1),
            1.0,
        );

        let pad = 18.0;
        let mut y = r.y0 + 22.0;
        // App wordmark.
        tr.draw_text(
            scene,
            "Calendar",
            20.0,
            c.text_primary,
            t * Affine::translate((r.x0 + pad, y)),
            None,
        );
        // small accent dot
        scene.fill(
            Fill::NonZero,
            t,
            Palette::Indigo.color(theme),
            None,
            &Circle::new(Point::new(r.x0 + pad + 100.0, y + 9.0), 4.0),
        );
        y += 40.0;

        // Export / Import .ics buttons (also bound to `e` / `i`).
        let bw = (r.width() - 24.0 - 8.0) / 2.0;
        let exp = Rect::new(r.x0 + 12.0, y, r.x0 + 12.0 + bw, y + 32.0);
        draw_icon_button(scene, t, theme, exp, exp.contains(self.last_mouse));
        draw_download(scene, t, c.neutral[8], Point::new(exp.x0 + 18.0, exp.center().y));
        tr.draw_text(
            scene,
            "Export",
            13.0,
            c.neutral[8],
            t * Affine::translate((exp.x0 + 32.0, exp.center().y - 8.0)),
            None,
        );
        hits.push((exp, Action::Export));

        let imp = Rect::new(exp.x1 + 8.0, y, r.x1 - 12.0, y + 32.0);
        draw_icon_button(scene, t, theme, imp, imp.contains(self.last_mouse));
        draw_upload(scene, t, c.neutral[8], Point::new(imp.x0 + 18.0, imp.center().y));
        tr.draw_text(
            scene,
            "Import",
            13.0,
            c.neutral[8],
            t * Affine::translate((imp.x0 + 32.0, imp.center().y - 8.0)),
            None,
        );
        hits.push((imp, Action::Import));
        y += 46.0;

        // Smart lists.
        let smart = [
            ("Inbox", ListSel::Inbox, c.semantic.info),
            ("Today", ListSel::Today, Palette::Indigo.color(theme)),
            ("Upcoming", ListSel::Upcoming, c.semantic.warning),
            ("Completed", ListSel::Completed, c.semantic.success),
        ];
        for (label, sel, accent) in smart {
            let row = Rect::new(r.x0 + 10.0, y, r.x1 - 10.0, y + 34.0);
            let active = self.list_sel == sel;
            if active {
                fill_round(scene, t, c.accent[5].with_alpha(0.18), row, 8.0);
            } else if row.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, row, 8.0);
            }
            // bullet
            scene.fill(
                Fill::NonZero,
                t,
                accent,
                None,
                &Circle::new(Point::new(row.x0 + 16.0, row.center().y), 4.5),
            );
            let txt_color = if active { c.text_primary } else { c.neutral[8] };
            tr.draw_text(
                scene,
                label,
                14.0,
                txt_color,
                t * Affine::translate((row.x0 + 32.0, row.center().y - 8.0)),
                None,
            );
            let count = self.model.open_count(&sel, self.today);
            if count > 0 {
                draw_count_badge(scene, t, tr, theme, row.x1 - 18.0, row.center().y, count);
            }
            hits.push((row, Action::List(sel)));
            y += 38.0;
        }

        y += 14.0;
        // Section heading.
        tr.draw_text(
            scene,
            "PROJECTS",
            11.0,
            c.text_dim,
            t * Affine::translate((r.x0 + pad, y)),
            None,
        );
        y += 26.0;
        for p in &self.model.projects {
            let row = Rect::new(r.x0 + 10.0, y, r.x1 - 10.0, y + 34.0);
            let active = self.list_sel == ListSel::Project(p.id);
            if active {
                fill_round(scene, t, c.accent[5].with_alpha(0.18), row, 8.0);
            } else if row.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, row, 8.0);
            }
            // colour tag
            fill_round(
                scene,
                t,
                p.color.color(theme),
                Rect::new(row.x0 + 12.0, row.center().y - 5.0, row.x0 + 22.0, row.center().y + 5.0),
                3.0,
            );
            tr.draw_text(
                scene,
                &p.name,
                14.0,
                if active { c.text_primary } else { c.neutral[8] },
                t * Affine::translate((row.x0 + 32.0, row.center().y - 8.0)),
                None,
            );
            let count = self
                .model
                .tasks
                .iter()
                .filter(|tk| tk.project == Some(p.id) && !tk.done && tk.parent.is_none())
                .count();
            if count > 0 {
                draw_count_badge(scene, t, tr, theme, row.x1 - 18.0, row.center().y, count);
            }
            hits.push((row, Action::List(ListSel::Project(p.id))));
            y += 38.0;
        }

        // Mini upcoming-task rail at the bottom of the sidebar.
        let list_title = match &self.list_sel {
            ListSel::Inbox => "Inbox",
            ListSel::Today => "Today",
            ListSel::Upcoming => "Upcoming",
            ListSel::Completed => "Completed",
            ListSel::Project(_) => "Tasks",
        };
        y += 10.0;
        stroke_line(scene, t, c.border_standard, Point::new(r.x0 + pad, y), Point::new(r.x1 - pad, y), 1.0);
        y += 14.0;
        tr.draw_text(
            scene,
            list_title,
            11.0,
            c.text_dim,
            t * Affine::translate((r.x0 + pad, y)),
            None,
        );
        y += 24.0;
        // Reserve the bottom of the sidebar for the mini-month navigator.
        let mini_top = r.y1 - 196.0;
        let tasks = self.model.tasks_for(&self.list_sel, self.today);
        for tk in tasks.iter().take(4) {
            if y > mini_top - 34.0 {
                break;
            }
            let row = Rect::new(r.x0 + 12.0, y, r.x1 - 12.0, y + 30.0);
            if row.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, row, 6.0);
            }
            let box_rect = Rect::new(row.x0 + 4.0, row.center().y - 8.0, row.x0 + 20.0, row.center().y + 8.0);
            draw_checkbox(scene, t, theme, box_rect, tk.done);
            hits.push((box_rect, Action::ToggleTask(tk.id)));
            let txt_color = if tk.done { c.text_dim } else { c.neutral[8] };
            tr.draw_text(
                scene,
                &elide(&tk.title, 22),
                13.0,
                txt_color,
                t * Affine::translate((row.x0 + 28.0, row.center().y - 7.0)),
                None,
            );
            if tk.done {
                let w = tr.measure(&elide(&tk.title, 22), 13.0, None).0 as f64;
                stroke_line(
                    scene,
                    t,
                    c.text_dim,
                    Point::new(row.x0 + 28.0, row.center().y),
                    Point::new(row.x0 + 28.0 + w, row.center().y),
                    1.0,
                );
            }
            // priority flag
            if tk.priority != Priority::None {
                tr.draw_text(
                    scene,
                    tk.priority.flag(),
                    10.0,
                    tk.priority.color(theme),
                    t * Affine::translate((row.x1 - 22.0, row.center().y - 6.0)),
                    None,
                );
            }
            y += 32.0;
        }

        // Mini-month navigator pinned to the sidebar bottom.
        stroke_line(
            scene,
            t,
            c.border_standard,
            Point::new(r.x0 + pad, mini_top - 8.0),
            Point::new(r.x1 - pad, mini_top - 8.0),
            1.0,
        );
        let mini_region = Rect::new(r.x0, mini_top, r.x1, r.y1 - 12.0);
        hits.extend(self.draw_mini_month(scene, t, tr, theme, mini_region));

        hits
    }

    /// Compact month navigator drawn in the sidebar: a 6×7 day grid with
    /// today highlighted, the anchor ringed, a dot under days that have
    /// events, and prev/next month chevrons. Clicking a day re-anchors the
    /// active view; the chevrons step the visible month.
    fn draw_mini_month(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        region: Rect,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let pad = 18.0;

        tr.draw_text(
            scene,
            &date::month_title(self.anchor),
            13.0,
            c.text_primary,
            t * Affine::translate((region.x0 + pad, region.y0)),
            None,
        );
        let pc = Rect::new(region.x1 - pad - 46.0, region.y0 - 4.0, region.x1 - pad - 24.0, region.y0 + 18.0);
        let nc = Rect::new(region.x1 - pad - 22.0, region.y0 - 4.0, region.x1 - pad, region.y0 + 18.0);
        if pc.contains(self.last_mouse) {
            fill_round(scene, t, c.surface_white_5, pc, 6.0);
        }
        if nc.contains(self.last_mouse) {
            fill_round(scene, t, c.surface_white_5, nc, 6.0);
        }
        draw_chevron(scene, t, c.neutral[7], pc.center(), true);
        draw_chevron(scene, t, c.neutral[7], nc.center(), false);
        hits.push((pc, Action::ShiftMonth(-1)));
        hits.push((nc, Action::ShiftMonth(1)));

        let grid_x0 = region.x0 + pad;
        let grid_w = region.width() - 2.0 * pad;
        let cw = grid_w / 7.0;
        let head_y = region.y0 + 24.0;
        for (i, lbl) in date::WEEKDAY_NARROW.iter().enumerate() {
            center_text(
                scene,
                tr,
                lbl,
                10.0,
                c.text_dim,
                Rect::new(grid_x0 + cw * i as f64, head_y, grid_x0 + cw * (i as f64 + 1.0), head_y + 14.0),
                t,
            );
        }

        let grid = date::month_grid(self.anchor);
        let cell_h = 22.0;
        let top = head_y + 18.0;
        for (idx, day) in grid.iter().enumerate() {
            let row = (idx / 7) as f64;
            let col = (idx % 7) as f64;
            let cell = Rect::new(
                grid_x0 + cw * col,
                top + cell_h * row,
                grid_x0 + cw * (col + 1.0),
                top + cell_h * (row + 1.0),
            );
            let in_month = day.month() == self.anchor.month();
            let is_today = *day == self.today;
            let is_anchor = *day == self.anchor;
            let has_ev = self.model.events.iter().any(|e| e.occurs_on(*day));

            if is_today {
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(cell.center(), 10.0));
            } else if is_anchor {
                stroke_round(scene, t, c.accent[5], cell.inflate(-3.0, -2.0), 7.0, 1.2);
            } else if cell.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, cell.inflate(-3.0, -2.0), 7.0);
            }
            let num_col = if is_today {
                Color::WHITE
            } else if !in_month {
                c.neutral[4]
            } else {
                c.neutral[8]
            };
            center_text(scene, tr, &format!("{}", day.day()), 11.0, num_col, cell, t);
            if has_ev && !is_today {
                let dot = Point::new(cell.center().x, cell.y1 - 2.5);
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(dot, 1.6));
            }
            hits.push((cell, Action::PickDate(*day)));
        }

        hits
    }

    fn period_title(&self) -> String {
        match self.view {
            View::Month => date::month_title(self.anchor),
            View::Week => {
                let w = date::week_days(self.anchor);
                format!(
                    "{} – {}",
                    date::fmt_day_long(w[0]),
                    date::fmt_day_long(w[6])
                )
            }
            View::Day => date::fmt_day_long(self.anchor),
            View::Agenda => "Upcoming".into(),
        }
    }

    fn draw_topbar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content_x0: f64,
        right_edge: f64,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let r = Rect::new(content_x0, self.rect.y0, right_edge, self.rect.y0 + TOPBAR_H);
        fill_rect(scene, t, c.surfaces.base, r);
        stroke_line(scene, t, c.border_standard, Point::new(r.x0, r.y1), Point::new(r.x1, r.y1), 1.0);

        let cy = r.center().y;
        // prev / next chevrons
        let prev = Rect::new(r.x0 + 16.0, cy - 15.0, r.x0 + 46.0, cy + 15.0);
        let next = Rect::new(r.x0 + 50.0, cy - 15.0, r.x0 + 80.0, cy + 15.0);
        draw_icon_button(scene, t, theme, prev, prev.contains(self.last_mouse));
        draw_icon_button(scene, t, theme, next, next.contains(self.last_mouse));
        draw_chevron(scene, t, c.neutral[8], prev.center(), true);
        draw_chevron(scene, t, c.neutral[8], next.center(), false);
        hits.push((prev, Action::Prev));
        hits.push((next, Action::Next));

        // Today pill
        let today_btn = Rect::new(r.x0 + 90.0, cy - 15.0, r.x0 + 150.0, cy + 15.0);
        draw_icon_button(scene, t, theme, today_btn, today_btn.contains(self.last_mouse));
        center_text(scene, tr, "Today", 13.0, c.neutral[8], today_btn, t);
        hits.push((today_btn, Action::Today));

        // Right cluster: segmented view control + New button.
        let seg_w = 64.0;
        let seg_total = seg_w * 4.0;
        let seg_x1 = right_edge - 14.0;
        let seg_x0 = seg_x1 - seg_total;
        let seg = Rect::new(seg_x0, cy - 16.0, seg_x1, cy + 16.0);
        fill_round(scene, t, c.surfaces.raised, seg, 9.0);
        stroke_round(scene, t, c.border_standard, seg, 9.0, 1.0);
        for (i, v) in View::ALL.iter().enumerate() {
            let cell = Rect::new(seg_x0 + seg_w * i as f64, seg.y0, seg_x0 + seg_w * (i as f64 + 1.0), seg.y1);
            if self.view == *v {
                fill_round(scene, t, c.accent[5], cell.inflate(-3.0, -3.0), 7.0);
            } else if cell.contains(self.last_mouse) {
                fill_round(scene, t, c.surface_white_5, cell.inflate(-3.0, -3.0), 7.0);
            }
            let col = if self.view == *v { Color::WHITE } else { c.neutral[7] };
            center_text(scene, tr, v.label(), 13.0, col, cell, t);
            hits.push((cell, Action::View(*v)));
        }

        let new_btn = Rect::new(seg_x0 - 78.0, cy - 16.0, seg_x0 - 12.0, cy + 16.0);
        fill_round(scene, t, c.accent[5], new_btn, 9.0);
        draw_plus(scene, t, Color::WHITE, Point::new(new_btn.x0 + 16.0, cy));
        tr.draw_text(
            scene,
            "New",
            13.0,
            Color::WHITE,
            t * Affine::translate((new_btn.x0 + 28.0, cy - 8.0)),
            None,
        );
        hits.push((new_btn, Action::NewEvent));

        // Period title — fills the gap, clipped so a long week range never
        // collides with the New button.
        let title_x = r.x0 + 168.0;
        let title_clip = Rect::new(title_x - 2.0, r.y0, new_btn.x0 - 12.0, r.y1);
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &title_clip);
        tr.draw_text(
            scene,
            &self.period_title(),
            19.0,
            c.text_primary,
            t * Affine::translate((title_x, cy - 11.0)),
            None,
        );
        scene.pop_layer();

        hits
    }

    fn view_rect(&self, content_x0: f64, right_edge: f64) -> Rect {
        Rect::new(content_x0, self.rect.y0 + TOPBAR_H, right_edge, self.rect.y1)
    }

    fn draw_month(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, t, c.surfaces.base, area);

        let header_h = 28.0;
        let cols = 7;
        let rows = 6;
        let cell_w = area.width() / cols as f64;
        let grid_top = area.y0 + header_h;
        let cell_h = (area.height() - header_h) / rows as f64;

        // Weekday header.
        for (i, lbl) in date::WEEKDAY_SHORT.iter().enumerate() {
            let weekend = i >= 5;
            let col = if weekend { c.text_dim } else { c.neutral[7] };
            let x = area.x0 + cell_w * i as f64;
            tr.draw_text(
                scene,
                lbl,
                12.0,
                col,
                t * Affine::translate((x + 10.0, area.y0 + 7.0)),
                None,
            );
        }

        let grid = date::month_grid(self.anchor);
        for (idx, day) in grid.iter().enumerate() {
            let row = idx / cols;
            let col = idx % cols;
            let cell = Rect::new(
                area.x0 + cell_w * col as f64,
                grid_top + cell_h * row as f64,
                area.x0 + cell_w * (col as f64 + 1.0),
                grid_top + cell_h * (row as f64 + 1.0),
            );
            // grid lines
            stroke_round(scene, t, c.border_standard.with_alpha(0.5), cell, 0.0, 0.5);

            let in_month = day.month() == self.anchor.month();
            let is_today = *day == self.today;
            let is_anchor = *day == self.anchor;
            let weekend = day.weekday().num_days_from_monday() >= 5;

            if is_anchor && !is_today {
                fill_rect(scene, t, c.surface_white_5, cell);
            }
            if weekend && in_month {
                fill_rect(scene, t, c.neutral[1].with_alpha(0.35), cell);
            }

            // date number, with a today pill.
            let num = format!("{}", day.day());
            let num_color = if !in_month {
                c.neutral[4]
            } else if weekend {
                c.text_dim
            } else {
                c.neutral[9]
            };
            if is_today {
                let pill = Circle::new(Point::new(cell.x0 + 18.0, cell.y0 + 17.0), 12.0);
                scene.fill(Fill::NonZero, t, c.accent[5], None, &pill);
                center_text(
                    scene,
                    tr,
                    &num,
                    12.0,
                    Color::WHITE,
                    Rect::new(cell.x0 + 6.0, cell.y0 + 5.0, cell.x0 + 30.0, cell.y0 + 29.0),
                    t,
                );
            } else {
                tr.draw_text(
                    scene,
                    &num,
                    12.0,
                    num_color,
                    t * Affine::translate((cell.x0 + 8.0, cell.y0 + 6.0)),
                    None,
                );
            }

            // Event chips (clip to cell).
            scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &cell);
            let evs: Vec<(Id, String, Palette, bool, NaiveDateTime)> = self
                .model
                .events_on(*day)
                .iter()
                .map(|e| (e.id, e.title.clone(), e.color, e.all_day, e.start))
                .collect();
            let max_chips = ((cell_h - 30.0) / 20.0).floor().max(0.0) as usize;
            let mut cy = cell.y0 + 30.0;
            for (i, (id, title, color, all_day, start)) in evs.iter().enumerate() {
                if i >= max_chips.max(1).min(3) {
                    let more = evs.len() - i;
                    tr.draw_text(
                        scene,
                        &format!("+{more} more"),
                        10.0,
                        c.text_dim,
                        t * Affine::translate((cell.x0 + 8.0, cy)),
                        None,
                    );
                    break;
                }
                let chip = Rect::new(cell.x0 + 6.0, cy, cell.x1 - 6.0, cy + 17.0);
                let col = color.color(theme);
                if *all_day {
                    fill_round(scene, t, col, chip, 4.0);
                    tr.draw_text(
                        scene,
                        &elide(title, 16),
                        10.0,
                        Color::WHITE,
                        t * Affine::translate((chip.x0 + 6.0, chip.y0 + 3.0)),
                        None,
                    );
                } else {
                    fill_round(scene, t, col.with_alpha(0.18), chip, 4.0);
                    scene.fill(
                        Fill::NonZero,
                        t,
                        col,
                        None,
                        &Circle::new(Point::new(chip.x0 + 8.0, chip.center().y), 3.0),
                    );
                    let label = format!("{} {}", date::fmt_time(start), elide(title, 14));
                    tr.draw_text(
                        scene,
                        &label,
                        10.0,
                        c.neutral[9],
                        t * Affine::translate((chip.x0 + 16.0, chip.y0 + 3.0)),
                        None,
                    );
                }
                hits.push((chip, Action::OpenEvent(*id)));
                cy += 20.0;
            }
            scene.pop_layer();

            hits.push((Rect::new(cell.x0, cell.y0, cell.x1, cell.y0 + 28.0), Action::OpenDay(*day)));
            hits.push((cell, Action::PickDate(*day)));
        }
        hits
    }

    fn draw_timeline(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        days: &[NaiveDate],
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, t, c.surfaces.base, area);

        let gutter = 56.0;
        let head_h = 56.0;
        let allday_h = 26.0;
        let grid_x0 = area.x0 + gutter;
        let col_w = (area.width() - gutter) / days.len() as f64;
        let hours = (DAY_END - DAY_START) as f64;
        let body_top = area.y0 + head_h + allday_h;
        let body_h = (area.y1 - body_top - 8.0).max(40.0);
        let hour_h = body_h / hours;

        // Day headers + all-day strip.
        for (i, day) in days.iter().enumerate() {
            let x0 = grid_x0 + col_w * i as f64;
            let head = Rect::new(x0, area.y0, x0 + col_w, area.y0 + head_h);
            let is_today = *day == self.today;
            if is_today {
                fill_rect(scene, t, c.accent[5].with_alpha(0.10), head);
            }
            tr.draw_text(
                scene,
                date::WEEKDAY_SHORT[day.weekday().num_days_from_monday() as usize],
                11.0,
                if i >= 5 { c.text_dim } else { c.neutral[7] },
                t * Affine::translate((x0 + 10.0, area.y0 + 8.0)),
                None,
            );
            let num = format!("{}", day.day());
            let ncol = if is_today { c.accent[5] } else { c.neutral[9] };
            tr.draw_text(
                scene,
                &num,
                20.0,
                ncol,
                t * Affine::translate((x0 + 10.0, area.y0 + 24.0)),
                None,
            );
            hits.push((head, Action::OpenDay(*day)));

            // all-day chips
            let allday: Vec<(Id, String, Palette)> = self
                .model
                .events_on(*day)
                .iter()
                .filter(|e| e.all_day)
                .map(|e| (e.id, e.title.clone(), e.color))
                .collect();
            let strip = Rect::new(x0 + 2.0, area.y0 + head_h + 2.0, x0 + col_w - 2.0, area.y0 + head_h + allday_h - 2.0);
            if !allday.is_empty() {
                let (id, title, color) = &allday[0];
                fill_round(scene, t, color.color(theme), strip, 4.0);
                tr.draw_text(
                    scene,
                    &elide(title, 18),
                    10.0,
                    Color::WHITE,
                    t * Affine::translate((strip.x0 + 6.0, strip.y0 + 4.0)),
                    None,
                );
                hits.push((strip, Action::OpenEvent(*id)));
            }
        }

        // Hour gridlines + labels.
        for h in 0..=(hours as i64) {
            let y = body_top + hour_h * h as f64;
            stroke_line(scene, t, c.border_standard.with_alpha(0.5), Point::new(grid_x0, y), Point::new(area.x1, y), 0.5);
            if h < hours as i64 {
                tr.draw_text(
                    scene,
                    &format!("{:02}:00", DAY_START + h),
                    10.0,
                    c.text_dim,
                    t * Affine::translate((area.x0 + 12.0, y + 2.0)),
                    None,
                );
            }
        }
        // vertical column separators
        for i in 0..=days.len() {
            let x = grid_x0 + col_w * i as f64;
            stroke_line(scene, t, c.border_standard.with_alpha(0.5), Point::new(x, body_top), Point::new(x, area.y1 - 8.0), 0.5);
        }

        // "now" line if today is in range.
        let now = Local::now().naive_local();
        if let Some(i) = days.iter().position(|d| *d == self.today) {
            let frac = ((now.hour() as i64 - DAY_START) as f64 + now.minute() as f64 / 60.0) / hours;
            if (0.0..=1.0).contains(&frac) {
                let y = body_top + body_h * frac;
                let x0 = grid_x0 + col_w * i as f64;
                stroke_line(scene, t, c.semantic.danger, Point::new(x0, y), Point::new(x0 + col_w, y), 2.0);
                scene.fill(Fill::NonZero, t, c.semantic.danger, None, &Circle::new(Point::new(x0, y), 3.5));
            }
        }

        // Timed event blocks.
        for (i, day) in days.iter().enumerate() {
            let x0 = grid_x0 + col_w * i as f64;
            let timed: Vec<(Id, String, String, Palette, NaiveDateTime, NaiveDateTime)> = self
                .model
                .events_on(*day)
                .iter()
                .filter(|e| !e.all_day)
                .map(|e| (e.id, e.title.clone(), e.location.clone(), e.color, e.start, e.end))
                .collect();
            for (id, title, loc, color, start, end) in timed {
                let s = (start.hour() as i64 - DAY_START) as f64 + start.minute() as f64 / 60.0;
                let e = (end.hour() as i64 - DAY_START) as f64 + end.minute() as f64 / 60.0;
                let top = body_top + (s.max(0.0)) * hour_h;
                let bottom = body_top + (e.min(hours)) * hour_h;
                let block = Rect::new(x0 + 3.0, top, x0 + col_w - 3.0, (bottom).max(top + 16.0));
                let col = color.color(theme);
                fill_round(scene, t, col.with_alpha(0.22), block, 6.0);
                fill_round(scene, t, col, Rect::new(block.x0, block.y0, block.x0 + 3.0, block.y1), 2.0);
                scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &block);
                tr.draw_text(
                    scene,
                    &title,
                    11.0,
                    c.neutral[9],
                    t * Affine::translate((block.x0 + 10.0, block.y0 + 4.0)),
                    Some((block.width() - 14.0) as f32),
                );
                if block.height() > 34.0 {
                    let sub = format!("{}–{} {}", date::fmt_time(&start), date::fmt_time(&end), loc);
                    tr.draw_text(
                        scene,
                        &sub,
                        10.0,
                        c.text_dim,
                        t * Affine::translate((block.x0 + 10.0, block.y0 + 18.0)),
                        Some((block.width() - 14.0) as f32),
                    );
                }
                scene.pop_layer();
                hits.push((block, Action::OpenEvent(id)));
            }
        }

        hits
    }

    fn draw_agenda(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, t, c.surfaces.base, area);

        let from = self.anchor;
        let to = self.anchor + Duration::days(21);
        let mut y = area.y0 + 18.0;
        let x0 = area.x0 + 24.0;

        let mut day = from;
        while day <= to && y < area.y1 - 20.0 {
            let evs = self.model.events_on(day);
            let tasks: Vec<_> = self
                .model
                .tasks
                .iter()
                .filter(|tk| tk.due == Some(day) && tk.parent.is_none())
                .collect();
            if evs.is_empty() && tasks.is_empty() {
                day += Duration::days(1);
                continue;
            }

            // Date heading.
            let is_today = day == self.today;
            let head_col = if is_today { c.accent[5] } else { c.neutral[8] };
            tr.draw_text(
                scene,
                &date::relative_label(day, self.today),
                15.0,
                head_col,
                t * Affine::translate((x0, y)),
                None,
            );
            tr.draw_text(
                scene,
                &date::fmt_day_long(day),
                12.0,
                c.text_dim,
                t * Affine::translate((x0 + 120.0, y + 2.0)),
                None,
            );
            y += 26.0;

            for e in &evs {
                if y > area.y1 - 18.0 {
                    break;
                }
                let row = Rect::new(x0, y, area.x1 - 24.0, y + 30.0);
                if row.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, row, 6.0);
                }
                let col = e.color.color(theme);
                fill_round(scene, t, col, Rect::new(row.x0, row.y0 + 4.0, row.x0 + 4.0, row.y1 - 4.0), 2.0);
                let time = if e.all_day { "All day".to_string() } else { date::fmt_time(&e.start) };
                tr.draw_text(scene, &time, 12.0, c.text_dim, t * Affine::translate((row.x0 + 16.0, y + 7.0)), None);
                tr.draw_text(
                    scene,
                    &e.title,
                    13.0,
                    c.neutral[9],
                    t * Affine::translate((row.x0 + 90.0, y + 7.0)),
                    Some((row.width() - 220.0) as f32),
                );
                if !e.location.is_empty() {
                    tr.draw_text(
                        scene,
                        &e.location,
                        12.0,
                        c.text_dim,
                        t * Affine::translate((row.x1 - 130.0, y + 7.0)),
                        Some(126.0),
                    );
                }
                hits.push((row, Action::OpenEvent(e.id)));
                y += 32.0;
            }
            for tk in &tasks {
                if y > area.y1 - 18.0 {
                    break;
                }
                let row = Rect::new(x0, y, area.x1 - 24.0, y + 30.0);
                if row.contains(self.last_mouse) {
                    fill_round(scene, t, c.surface_white_5, row, 6.0);
                }
                let box_rect = Rect::new(row.x0 + 8.0, row.center().y - 8.0, row.x0 + 24.0, row.center().y + 8.0);
                draw_checkbox(scene, t, theme, box_rect, tk.done);
                hits.push((box_rect, Action::ToggleTask(tk.id)));
                let col = if tk.done { c.text_dim } else { c.neutral[9] };
                tr.draw_text(
                    scene,
                    &tk.title,
                    13.0,
                    col,
                    t * Affine::translate((row.x0 + 34.0, y + 7.0)),
                    Some((row.width() - 160.0) as f32),
                );
                if tk.done {
                    let w = tr.measure(&tk.title, 13.0, None).0 as f64;
                    stroke_line(scene, t, c.text_dim, Point::new(row.x0 + 34.0, row.center().y), Point::new((row.x0 + 34.0 + w).min(row.x1 - 120.0), row.center().y), 1.0);
                }
                if tk.priority != Priority::None {
                    tr.draw_text(scene, tk.priority.flag(), 11.0, tk.priority.color(theme), t * Affine::translate((row.x1 - 30.0, y + 7.0)), None);
                }
                y += 32.0;
            }
            y += 12.0;
            day += Duration::days(1);
        }

        if y <= area.y0 + 18.0 {
            center_text(scene, tr, "Nothing scheduled in the next three weeks", 14.0, c.text_dim, area, t);
        }
        hits
    }

    fn draw_detail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<(Rect, Action)> {
        let c = &theme.tokens.colors;
        let mut hits = Vec::new();
        let r = self.detail_rect;
        fill_rect(scene, t, c.surfaces.raised, r);
        stroke_line(scene, t, c.border_standard, Point::new(r.x0, r.y0), Point::new(r.x0, r.y1), 1.0);

        let x = r.x0 + DETAIL_PAD;
        let w = NOTES_W;
        let mut y = r.y0 + 18.0;

        // close button
        let close = Rect::new(r.x1 - 36.0, r.y0 + 14.0, r.x1 - 14.0, r.y0 + 36.0);
        draw_icon_button(scene, t, theme, close, close.contains(self.last_mouse));
        draw_cross(scene, t, c.neutral[8], close.center());
        hits.push((close, Action::CloseDetail));

        match self.selection {
            Some(Sel::Event(id)) => {
                if let Some(e) = self.model.event(id) {
                    // colour dot + kind label
                    scene.fill(Fill::NonZero, t, e.color.color(theme), None, &Circle::new(Point::new(x + 6.0, y + 8.0), 6.0));
                    tr.draw_text(scene, "EVENT", 11.0, c.text_dim, t * Affine::translate((x + 20.0, y + 2.0)), None);
                    y += 26.0;
                    tr.draw_text(scene, &e.title, 22.0, c.text_primary, t * Affine::translate((x, y)), Some(w as f32));
                    y += 40.0;

                    // when
                    let when = if e.all_day {
                        format!("{} · All day", date::fmt_day_long(e.start.date()))
                    } else {
                        format!(
                            "{} · {}–{}",
                            date::fmt_day_long(e.start.date()),
                            date::fmt_time(&e.start),
                            date::fmt_time(&e.end)
                        )
                    };
                    field_row(scene, tr, theme, t, x, y, w, "When", &when);
                    y += 34.0;
                    if !e.location.is_empty() {
                        field_row(scene, tr, theme, t, x, y, w, "Where", &e.location);
                        y += 34.0;
                    }
                    if let Some(p) = e.project.and_then(|pid| self.model.project(pid)) {
                        field_row(scene, tr, theme, t, x, y, w, "Calendar", &p.name);
                        y += 34.0;
                    }

                    // editable chips: recurrence / reminder / colour
                    let rec_label = if e.recurrence == Recurrence::None {
                        "No repeat".to_string()
                    } else {
                        e.recurrence.short().to_string()
                    };
                    let rec_chip = chip(scene, t, tr, theme, x, y, &format!("↻ {rec_label}"), false);
                    hits.push((rec_chip, Action::CycleRecurrence));
                    let rem_label = match e.reminder_min {
                        None => "No reminder".to_string(),
                        Some(m) if m >= 60 => format!("⏰ {}h before", m / 60),
                        Some(m) => format!("⏰ {m}m before"),
                    };
                    let rem_chip = chip(scene, t, tr, theme, rec_chip.x1 + 8.0, y, &rem_label, false);
                    hits.push((rem_chip, Action::CycleReminder));
                    y += 38.0;
                    let col_chip = chip(scene, t, tr, theme, x, y, &format!("● {}", e.color.name()), false);
                    // recolour the bullet
                    scene.fill(Fill::NonZero, t, e.color.color(theme), None, &Circle::new(Point::new(col_chip.x0 + 12.0, col_chip.center().y), 4.0));
                    hits.push((col_chip, Action::CycleColor));
                }
            }
            Some(Sel::Task(id)) => {
                if let Some(tk) = self.model.task(id) {
                    let box_rect = Rect::new(x, y, x + 18.0, y + 18.0);
                    draw_checkbox(scene, t, theme, box_rect, tk.done);
                    hits.push((box_rect, Action::ToggleSelectedTask));
                    tr.draw_text(scene, "TASK", 11.0, c.text_dim, t * Affine::translate((x + 28.0, y + 2.0)), None);
                    y += 26.0;
                    let title_col = if tk.done { c.text_dim } else { c.text_primary };
                    tr.draw_text(scene, &tk.title, 21.0, title_col, t * Affine::translate((x, y)), Some(w as f32));
                    y += 40.0;
                    if let Some(due) = tk.due {
                        field_row(scene, tr, theme, t, x, y, w, "Due", &date::fmt_day_long(due));
                        y += 34.0;
                    }
                    field_row(scene, tr, theme, t, x, y, w, "Priority", tk.priority.label());
                    y += 34.0;
                    if let Some(p) = tk.project.and_then(|pid| self.model.project(pid)) {
                        field_row(scene, tr, theme, t, x, y, w, "Project", &p.name);
                        y += 34.0;
                    }
                    let subs = self.model.subtasks_of(id);
                    if !subs.is_empty() {
                        tr.draw_text(scene, "Subtasks", 12.0, c.text_dim, t * Affine::translate((x, y)), None);
                        y += 22.0;
                        for s in subs {
                            let box_rect = Rect::new(x, y, x + 16.0, y + 16.0);
                            draw_checkbox(scene, t, theme, box_rect, s.done);
                            hits.push((box_rect, Action::ToggleTask(s.id)));
                            tr.draw_text(scene, &s.title, 13.0, c.neutral[8], t * Affine::translate((x + 26.0, y)), None);
                            y += 24.0;
                        }
                    }
                }
            }
            None => {}
        }

        // Notes header (the editor itself is drawn separately in draw()).
        let notes_label_y = r.y0 + NOTES_TOP_OFFSET - 26.0;
        tr.draw_text(scene, "NOTES", 11.0, c.text_dim, t * Affine::translate((x, notes_label_y)), None);
        stroke_line(scene, t, c.border_standard, Point::new(x, notes_label_y + 18.0), Point::new(x + w, notes_label_y + 18.0), 1.0);

        hits
    }

    fn draw_status(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, content_x0: f64) {
        if let Some(msg) = &self.status {
            let c = &theme.tokens.colors;
            let wmeas = tr.measure(msg, 12.0, None).0 as f64;
            let pill = Rect::new(content_x0 + 20.0, self.rect.y1 - 44.0, content_x0 + 44.0 + wmeas, self.rect.y1 - 16.0);
            fill_round(scene, t, c.neutral[2], pill, 8.0);
            stroke_round(scene, t, c.border_standard, pill, 8.0, 1.0);
            tr.draw_text(scene, msg, 12.0, c.neutral[9], t * Affine::translate((pill.x0 + 12.0, pill.center().y - 7.0)), None);
        }
    }
}

impl Default for CalendarApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CalendarApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            if CalDoc::is_empty(state) {
                CalDoc::seed(state, &self.model);
                self.seed_demo_notes(state);
            } else {
                // Adopt an existing shared calendar.
                self.model = CalDoc::read_model(state);
            }
            self.seeded = true;
        }
        self.ensure_bound(state);
        self.notes_editor.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let editor_node = self.notes_editor.build_layout(taffy, theme, node_map);
        if let Ok(existing) = taffy.style(editor_node).cloned() {
            let _ = taffy.set_style(
                editor_node,
                Style {
                    position: taffy::Position::Absolute,
                    size: Size {
                        width: Dimension::length(NOTES_W as f32),
                        height: Dimension::auto(),
                    },
                    inset: taffy::Rect {
                        left: length(0.0),
                        top: length(0.0),
                        right: taffy::LengthPercentageAuto::auto(),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    },
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);

        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[editor_node],
            )
            .unwrap();
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height() {
            self.dirty = true;
        }
        self.rect = new_rect;

        // Detail panel rect + notes editor placement.
        if self.detail_open {
            self.detail_rect = Rect::new(self.rect.x1 - DETAIL_W, self.rect.y0, self.rect.x1, self.rect.y1);
            self.notes_origin = Point::new(self.detail_rect.x0 + DETAIL_PAD, self.detail_rect.y0 + NOTES_TOP_OFFSET);
        } else {
            self.detail_rect = Rect::ZERO;
            self.notes_origin = Point::new(-10000.0, -10000.0);
        }
        if let Some(n) = self.editor_node {
            self.notes_editor.apply_layout(taffy, n, self.notes_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let t = transform;
        let c = &theme.tokens.colors;
        fill_rect(scene, t, c.surfaces.base, self.rect);

        let content_x0 = self.rect.x0 + SIDEBAR_W;
        let right_edge = if self.detail_open { self.rect.x1 - DETAIL_W } else { self.rect.x1 };

        let mut hits = Vec::new();
        hits.extend(self.draw_sidebar(scene, t, text_renderer, theme));
        hits.extend(self.draw_topbar(scene, t, text_renderer, theme, content_x0, right_edge));

        let area = self.view_rect(content_x0, right_edge);
        match self.view {
            View::Month => hits.extend(self.draw_month(scene, t, text_renderer, theme, area)),
            View::Week => {
                let days = date::week_days(self.anchor);
                hits.extend(self.draw_timeline(scene, t, text_renderer, theme, area, &days));
            }
            View::Day => {
                let days = [self.anchor];
                hits.extend(self.draw_timeline(scene, t, text_renderer, theme, area, &days));
            }
            View::Agenda => hits.extend(self.draw_agenda(scene, t, text_renderer, theme, area)),
        }

        self.draw_status(scene, t, text_renderer, theme, content_x0);

        if self.detail_open {
            hits.extend(self.draw_detail(scene, t, text_renderer, theme));
            // Notes editor — clipped to the notes region below the header.
            // Clip to the editor's own column (not the whole panel) so a
            // long single-line note block scrolls/cuts at the notes
            // padding rather than bleeding to the window edge.
            let clip = Rect::new(
                self.detail_rect.x0,
                self.notes_origin.y - 4.0,
                self.notes_origin.x + NOTES_W + 4.0,
                self.detail_rect.y1 - 12.0,
            );
            scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &clip);
            self.notes_editor.draw(scene, t, text_renderer, theme, presence);
            scene.pop_layer();
            self.notes_editor.draw_overlay(scene, t, text_renderer, theme);
        }

        self.hits = hits;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track the pointer in logical coordinates.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.dirty = true;
        }

        let editor_active = self.detail_open;
        let in_notes = editor_active
            && self.last_mouse.x >= self.detail_rect.x0
            && self.last_mouse.y >= self.notes_origin.y - 4.0;

        // Keyboard.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
            ..
        } = event
        {
            // Escape closes the detail panel.
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                if self.detail_open {
                    self.detail_open = false;
                    self.selection = None;
                    self.notes_editor.set_focus(false);
                    *handled = true;
                    self.dirty = true;
                    return;
                }
            }
            // When notes are focused, let the editor own the keystrokes.
            if !(editor_active && self.notes_editor.is_focused()) {
                if let Key::Character(ch) = logical_key {
                    let key = ch.to_lowercase();
                    let act = match key.as_str() {
                        "m" => Some(Action::View(View::Month)),
                        "w" => Some(Action::View(View::Week)),
                        "d" => Some(Action::View(View::Day)),
                        "a" => Some(Action::View(View::Agenda)),
                        "t" => Some(Action::Today),
                        "n" => Some(Action::NewEvent),
                        "e" => Some(Action::Export),
                        "i" => Some(Action::Import),
                        "[" => Some(Action::Prev),
                        "]" => Some(Action::Next),
                        _ => None,
                    };
                    if let Some(a) = act {
                        self.apply_action(a, state);
                        *handled = true;
                        return;
                    }
                }
                if let Key::Named(named) = logical_key {
                    let days = match named {
                        NamedKey::ArrowLeft => Some(-1),
                        NamedKey::ArrowRight => Some(1),
                        NamedKey::ArrowUp => Some(-7),
                        NamedKey::ArrowDown => Some(7),
                        _ => None,
                    };
                    if let Some(d) = days {
                        self.move_anchor(d);
                        *handled = true;
                        return;
                    }
                }
            }
        }

        // Mouse click resolution.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
            // Clicks inside the notes region go to the editor (focus + caret).
            if in_notes {
                if let Some(n) = self.editor_node {
                    self.notes_editor.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
                }
                self.dirty = true;
                return;
            }
            // Otherwise resolve against the hit-map (topmost last).
            let p = self.last_mouse;
            let mut chosen: Option<Action> = None;
            for (rect, action) in self.hits.iter().rev() {
                if rect.contains(p) {
                    chosen = Some(action.clone());
                    break;
                }
            }
            if let Some(a) = chosen {
                self.notes_editor.set_focus(false);
                self.apply_action(a, state);
                *handled = true;
                return;
            }
        }

        // Forward remaining events (key typing, mouse move) to the editor
        // when the notes panel is active so editing works.
        if editor_active {
            if let Some(n) = self.editor_node {
                self.notes_editor.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
            }
        }
        self.dirty = true;
    }

    fn name(&self) -> &str {
        "CalendarApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.notes_editor.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.notes_editor.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.notes_editor.take_structure_dirty()
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty || self.notes_editor.needs_redraw()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

// ========================================================================
// Drawing primitives (free functions).
// ========================================================================

fn fill_rect(scene: &mut Scene, t: Affine, color: Color, r: Rect) {
    scene.fill(Fill::NonZero, t, color, None, &r);
}

fn fill_round(scene: &mut Scene, t: Affine, color: Color, r: Rect, radius: f64) {
    scene.fill(Fill::NonZero, t, color, None, &RoundedRect::from_rect(r, radius));
}

fn stroke_round(scene: &mut Scene, t: Affine, color: Color, r: Rect, radius: f64, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &RoundedRect::from_rect(r, radius));
}

fn stroke_line(scene: &mut Scene, t: Affine, color: Color, a: Point, b: Point, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &Line::new(a, b));
}

fn center_text(scene: &mut Scene, tr: &mut TextRenderer, text: &str, size: f32, color: Color, r: Rect, t: Affine) {
    let w = tr.measure(text, size, None).0 as f64;
    let x = r.center().x - w / 2.0;
    let y = r.center().y - size as f64 / 2.0;
    tr.draw_text(scene, text, size, color, t * Affine::translate((x, y)), None);
}

fn draw_icon_button(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, hover: bool) {
    let c = &theme.tokens.colors;
    let bg = if hover { c.surface_white_10 } else { c.surfaces.raised };
    fill_round(scene, t, bg, r, 8.0);
    stroke_round(scene, t, c.border_standard, r, 8.0, 1.0);
}

fn draw_chevron(scene: &mut Scene, t: Affine, color: Color, center: Point, left: bool) {
    // `left == true` draws a "<" (points left); false draws ">".
    let dx = if left { -4.0 } else { 4.0 };
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - dx, center.y - 5.0));
    p.line_to(Point::new(center.x + dx, center.y));
    p.line_to(Point::new(center.x - dx, center.y + 5.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
}

fn draw_plus(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y), Point::new(center.x + 5.0, center.y), 1.8);
    stroke_line(scene, t, color, Point::new(center.x, center.y - 5.0), Point::new(center.x, center.y + 5.0), 1.8);
}

fn draw_cross(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y - 5.0), Point::new(center.x + 5.0, center.y + 5.0), 1.6);
    stroke_line(scene, t, color, Point::new(center.x + 5.0, center.y - 5.0), Point::new(center.x - 5.0, center.y + 5.0), 1.6);
}

fn draw_check(scene: &mut Scene, t: Affine, color: Color, r: Rect) {
    let mut p = BezPath::new();
    p.move_to(Point::new(r.x0 + r.width() * 0.22, r.center().y));
    p.line_to(Point::new(r.x0 + r.width() * 0.42, r.y1 - r.height() * 0.28));
    p.line_to(Point::new(r.x1 - r.width() * 0.20, r.y0 + r.height() * 0.28));
    scene.stroke(&Stroke::new(1.8), t, color, None, &p);
}

fn draw_checkbox(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, done: bool) {
    let c = &theme.tokens.colors;
    if done {
        fill_round(scene, t, c.accent[5], r, 4.0);
        draw_check(scene, t, Color::WHITE, r);
    } else {
        stroke_round(scene, t, c.neutral[5], r, 4.0, 1.6);
    }
}

fn draw_count_badge(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, cx: f64, cy: f64, n: usize) {
    let c = &theme.tokens.colors;
    let s = format!("{n}");
    let w = (tr.measure(&s, 11.0, None).0 as f64 + 12.0).max(20.0);
    let r = Rect::new(cx - w / 2.0, cy - 9.0, cx + w / 2.0, cy + 9.0);
    fill_round(scene, t, c.neutral[3], r, 9.0);
    center_text(scene, tr, &s, 11.0, c.neutral[9], r, t);
}

#[allow(clippy::too_many_arguments)]
fn field_row(scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, t: Affine, x: f64, y: f64, w: f64, label: &str, value: &str) {
    let c = &theme.tokens.colors;
    tr.draw_text(scene, label, 11.0, c.text_dim, t * Affine::translate((x, y)), None);
    tr.draw_text(scene, value, 14.0, c.neutral[9], t * Affine::translate((x + 84.0, y - 2.0)), Some((w - 84.0) as f32));
}

fn draw_download(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // Down-arrow into a tray.
    stroke_line(scene, t, color, Point::new(center.x, center.y - 6.0), Point::new(center.x, center.y + 2.0), 1.6);
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 4.0, center.y - 1.0));
    p.line_to(Point::new(center.x, center.y + 3.0));
    p.line_to(Point::new(center.x + 4.0, center.y - 1.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y + 6.0), Point::new(center.x + 5.0, center.y + 6.0), 1.6);
}

fn draw_upload(scene: &mut Scene, t: Affine, color: Color, center: Point) {
    // Up-arrow rising out of a tray (mirror of `draw_download`).
    stroke_line(scene, t, color, Point::new(center.x, center.y - 5.0), Point::new(center.x, center.y + 3.0), 1.6);
    let mut p = BezPath::new();
    p.move_to(Point::new(center.x - 4.0, center.y - 1.0));
    p.line_to(Point::new(center.x, center.y - 5.0));
    p.line_to(Point::new(center.x + 4.0, center.y - 1.0));
    scene.stroke(&Stroke::new(1.6), t, color, None, &p);
    stroke_line(scene, t, color, Point::new(center.x - 5.0, center.y + 6.0), Point::new(center.x + 5.0, center.y + 6.0), 1.6);
}

/// Draw a small pill chip and return its rect.
fn chip(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, label: &str, filled: bool) -> Rect {
    let c = &theme.tokens.colors;
    let tw = tr.measure(label, 12.0, None).0 as f64;
    let r = Rect::new(x, y, x + tw + 24.0, y + 28.0);
    if filled {
        fill_round(scene, t, c.accent[5], r, 8.0);
    } else {
        fill_round(scene, t, c.neutral[2], r, 8.0);
        stroke_round(scene, t, c.border_standard, r, 8.0, 1.0);
    }
    tr.draw_text(scene, label, 12.0, c.neutral[9], t * Affine::translate((r.x0 + 12.0, r.center().y - 7.0)), None);
    r
}

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_app_seeds_model_and_selection() {
        let app = CalendarApp::new();
        assert!(!app.model.events.is_empty());
        assert!(!app.model.tasks.is_empty());
        assert!(app.selection.is_some());
        assert!(app.detail_open);
    }

    #[test]
    fn sync_seeds_crdt_and_binds_editor() {
        let mut app = CalendarApp::new();
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut state, &root);
        assert!(app.seeded);
        // Selected event's notes are bound.
        assert!(app.bound.is_some());
        assert!(!CalDoc::is_empty(&state));
    }

    #[test]
    fn switching_view_changes_state() {
        let mut app = CalendarApp::new();
        let mut state = AutoCommit::new();
        app.apply_action(Action::View(View::Week), &mut state);
        assert_eq!(app.view, View::Week);
        app.apply_action(Action::View(View::Agenda), &mut state);
        assert_eq!(app.view, View::Agenda);
    }

    #[test]
    fn toggling_a_task_flips_done_and_mirrors_to_crdt() {
        let mut app = CalendarApp::new();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let id = app.model.tasks.iter().find(|t| !t.done).unwrap().id;
        let before = app.model.task(id).unwrap().done;
        app.apply_action(Action::ToggleTask(id), &mut state);
        assert_ne!(app.model.task(id).unwrap().done, before);
    }

    #[test]
    fn new_event_appends_and_selects() {
        let mut app = CalendarApp::new();
        let mut state = AutoCommit::new();
        app.sync(&mut state, &automerge::ROOT);
        let before = app.model.events.len();
        app.apply_action(Action::NewEvent, &mut state);
        assert_eq!(app.model.events.len(), before + 1);
        assert!(matches!(app.selection, Some(Sel::Event(_))));
    }

    #[test]
    fn stepping_month_changes_anchor_month() {
        let mut app = CalendarApp::new();
        let mut state = AutoCommit::new();
        let m0 = app.anchor.month();
        app.apply_action(Action::Next, &mut state);
        // Month view by default → Next advances one month.
        assert_ne!(app.anchor.month(), m0);
    }
}
