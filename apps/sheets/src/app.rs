//! Root widget for the Sheets app. Owns the [`Grid`] and the surrounding
//! chrome — a format/action toolbar, a name-box + formula bar, and a
//! bottom sheet-tab strip + status line — all hand-drawn into the scene.
//!
//! Data flow: `sync` seeds the workbook into the CRDT on first run, reads
//! the active sheet's raw inputs, recalculates every formula via the
//! [`crate::formula`] engine, and hands the resulting values to the grid.
//! `handle_event` routes pointer/keyboard input to whichever region was
//! hit and applies the grid's [`GridCmd`]s back into the document.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use vello::Scene;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;
use ui::ColorCompat;

use crate::doc::SheetsDoc;
use crate::formula;
use crate::grid::{Grid, GridCmd};
use crate::model::{a1_to_addr, Addr, CellFormat};

const TOOLBAR_H: f64 = 50.0;
const FORMULA_H: f64 = 34.0;
const TABS_H: f64 = 34.0;
const NAME_BOX_W: f64 = 92.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum ToolbarAction {
    Sum,
    Format(CellFormat),
    Clear,
    Import,
    Export,
}

struct ToolButton {
    rect: Rect,
    label: String,
    action: ToolbarAction,
}

pub struct SheetsApp {
    grid: Grid,
    rect: Rect,
    toolbar_rect: Rect,
    formula_rect: Rect,
    grid_rect: Rect,
    tabs_rect: Rect,
    buttons: Vec<ToolButton>,
    tab_rects: Vec<(Rect, usize)>,
    add_tab_rect: Rect,
    formula_field_rect: Rect,
    sheet_names: Vec<String>,
    active_sheet: usize,
    status: String,
    seeded: bool,
    dirty: bool,
    needs_commit: bool,
    last_press: Option<(Instant, Addr)>,
    last_mouse: Point,
    current_active_format: CellFormat,
}

impl SheetsApp {
    pub fn new() -> Self {
        Self {
            grid: Grid::new(),
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            formula_rect: Rect::ZERO,
            grid_rect: Rect::ZERO,
            tabs_rect: Rect::ZERO,
            buttons: Vec::new(),
            tab_rects: Vec::new(),
            add_tab_rect: Rect::ZERO,
            formula_field_rect: Rect::ZERO,
            sheet_names: Vec::new(),
            active_sheet: 0,
            status: "Ready".into(),
            seeded: false,
            dirty: true,
            needs_commit: false,
            last_press: None,
            last_mouse: Point::ZERO,
            current_active_format: CellFormat::General,
        }
    }

    // --- document plumbing --------------------------------------------------

    fn seed_if_empty(&mut self, doc: &mut AutoCommit) {
        if !SheetsDoc::is_empty(doc) {
            return;
        }
        seed_workbook(doc);
        SheetsDoc::set_active_index(doc, 0);
        // Preselect a formula cell so the formula bar shows a formula in
        // the first frame / screenshot.
        if let Some(addr) = a1_to_addr("E2") {
            self.grid.select_only(addr);
        }
    }

    /// Read the active sheet, recalculate, and push the data into the grid.
    fn reload(&mut self, doc: &mut AutoCommit) {
        self.active_sheet = SheetsDoc::active_index(doc);
        self.sheet_names = SheetsDoc::sheet_names(doc);
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet);
        let values = formula::recalculate(&snap.raw);
        self.grid.set_data(snap.raw, snap.fmt, values);
        self.dirty = true;
    }

    fn apply_cmds(&mut self, doc: &mut AutoCommit, cmds: Vec<GridCmd>) {
        let mut mutated = false;
        for cmd in cmds {
            match cmd {
                GridCmd::SetCell { addr, raw } => {
                    SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, &raw);
                    mutated = true;
                }
                GridCmd::Commit => {
                    self.needs_commit = true;
                }
            }
        }
        if mutated {
            self.reload(doc);
        }
    }

    fn apply_format(&mut self, doc: &mut AutoCommit, fmt: CellFormat) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        for addr in self.grid.selected_cells() {
            SheetsDoc::set_cell_format(doc, self.active_sheet, addr, fmt);
        }
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Applied {} format to {}", fmt.label(), self.grid.selection_label());
    }

    /// AutoSum: scan upward from the active cell for a run of numeric
    /// cells and write `=SUM(top:above)` into the active cell.
    fn auto_sum(&mut self, doc: &mut AutoCommit) {
        let (c, r) = self.grid.active();
        if r == 0 {
            self.status = "Nothing above to sum".into();
            return;
        }
        // Walk up while the cell parses as a number.
        let mut top = r;
        let mut rr = r as i64 - 1;
        while rr >= 0 {
            let raw = SheetsDoc::cell_raw(doc, self.active_sheet, (c, rr as u32));
            let is_num = raw.trim().parse::<f64>().is_ok()
                || raw.starts_with('=');
            if raw.is_empty() || !is_num {
                break;
            }
            top = rr as u32;
            rr -= 1;
        }
        if top == r {
            self.status = "No numeric run above".into();
            return;
        }
        let formula = format!(
            "=SUM({}:{})",
            crate::model::addr_to_a1(c, top),
            crate::model::addr_to_a1(c, r - 1)
        );
        SheetsDoc::set_cell_raw(doc, self.active_sheet, (c, r), &formula);
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Inserted {formula}");
    }

    fn clear_selection(&mut self, doc: &mut AutoCommit) {
        for addr in self.grid.selected_cells() {
            SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, "");
        }
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Cleared {}", self.grid.selection_label());
    }

    fn import_demo_csv(&mut self, doc: &mut AutoCommit) {
        let idx = SheetsDoc::add_sheet(doc, "Imported");
        let parsed = crate::csv::parse_csv(DEMO_CSV);
        let count = parsed.len();
        for (addr, raw) in parsed {
            SheetsDoc::set_cell_raw(doc, idx, addr, &raw);
        }
        SheetsDoc::set_active_index(doc, idx);
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Imported {count} cells into a new sheet");
    }

    fn export_csv(&mut self, doc: &mut AutoCommit) {
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet);
        let values = formula::recalculate(&snap.raw);
        // Determine extent.
        let max_c = snap.raw.keys().map(|a| a.0).max().unwrap_or(0);
        let max_r = snap.raw.keys().map(|a| a.1).max().unwrap_or(0);
        let mut rows = Vec::new();
        for r in 0..=max_r {
            let mut row = Vec::new();
            for c in 0..=max_c {
                let cell = values.get(&(c, r));
                let fmt = snap.fmt.get(&(c, r)).copied().unwrap_or_default();
                let s = cell
                    .map(|v| crate::model::format_value(v, fmt))
                    .unwrap_or_default();
                row.push(s);
            }
            rows.push(row);
        }
        let csv = crate::csv::to_csv(&rows);
        let path = std::env::temp_dir().join(format!("sheets-{}.csv", sanitize(&snap.name)));
        match std::fs::write(&path, csv) {
            Ok(_) => self.status = format!("Exported {} to {}", snap.name, path.display()),
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    fn switch_sheet(&mut self, doc: &mut AutoCommit, idx: usize) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        SheetsDoc::set_active_index(doc, idx);
        self.grid.select_only((0, 0));
        self.reload(doc);
        self.status = format!("Sheet: {}", self.sheet_names.get(idx).cloned().unwrap_or_default());
    }

    fn add_sheet(&mut self, doc: &mut AutoCommit) {
        let n = SheetsDoc::sheet_count(doc);
        let idx = SheetsDoc::add_sheet(doc, &format!("Sheet{}", n + 1));
        SheetsDoc::set_active_index(doc, idx);
        self.grid.select_only((0, 0));
        self.needs_commit = true;
        self.reload(doc);
        self.status = "Added sheet".into();
    }

    // --- layout -------------------------------------------------------------

    fn relayout(&mut self) {
        let r = self.rect;
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);
        self.formula_rect =
            Rect::new(r.x0, self.toolbar_rect.y1, r.x1, self.toolbar_rect.y1 + FORMULA_H);
        self.tabs_rect = Rect::new(r.x0, r.y1 - TABS_H, r.x1, r.y1);
        self.grid_rect =
            Rect::new(r.x0, self.formula_rect.y1, r.x1, self.tabs_rect.y0);
        self.grid.rect = self.grid_rect;

        // Toolbar buttons (fixed widths so hit-testing needs no measuring).
        let specs: &[(&str, ToolbarAction, f64)] = &[
            ("\u{03A3} Sum", ToolbarAction::Sum, 66.0),
            ("General", ToolbarAction::Format(CellFormat::General), 68.0),
            ("Number", ToolbarAction::Format(CellFormat::Number), 66.0),
            ("Currency", ToolbarAction::Format(CellFormat::Currency), 74.0),
            ("Percent", ToolbarAction::Format(CellFormat::Percent), 68.0),
            ("Date", ToolbarAction::Format(CellFormat::Date), 52.0),
            ("Text", ToolbarAction::Format(CellFormat::Text), 52.0),
            ("Clear", ToolbarAction::Clear, 56.0),
            ("Import CSV", ToolbarAction::Import, 92.0),
            ("Export CSV", ToolbarAction::Export, 92.0),
        ];
        self.buttons.clear();
        let mut x = self.toolbar_rect.x0 + 12.0;
        let pad = 6.0;
        let bh = 30.0;
        let by = self.toolbar_rect.y0 + (TOOLBAR_H - bh) / 2.0;
        for (label, action, w) in specs {
            // Visual separators after Sum and after Text.
            if matches!(action, ToolbarAction::Format(CellFormat::General))
                || matches!(action, ToolbarAction::Clear)
            {
                x += 8.0;
            }
            let rect = Rect::new(x, by, x + w, by + bh);
            self.buttons.push(ToolButton {
                rect,
                label: label.to_string(),
                action: *action,
            });
            x += w + pad;
        }

        // Formula bar field (after the name box + fx label).
        let fx_w = 30.0;
        self.formula_field_rect = Rect::new(
            self.formula_rect.x0 + NAME_BOX_W + fx_w + 8.0,
            self.formula_rect.y0 + 4.0,
            self.formula_rect.x1 - 8.0,
            self.formula_rect.y1 - 4.0,
        );

        // Sheet tabs.
        self.tab_rects.clear();
        let mut tx = self.tabs_rect.x0 + 8.0;
        let ty = self.tabs_rect.y0 + 4.0;
        let th = TABS_H - 6.0;
        for (i, name) in self.sheet_names.iter().enumerate() {
            let w = tab_width(name);
            self.tab_rects.push((Rect::new(tx, ty, tx + w, ty + th), i));
            tx += w + 4.0;
        }
        self.add_tab_rect = Rect::new(tx, ty, tx + 30.0, ty + th);
    }

    // --- drawing ------------------------------------------------------------

    fn draw_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        active: bool,
        accent: bool,
    ) {
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.sm as f64);
        let bg = if active {
            colors.accent[5]
        } else if accent {
            colors.accent[5].with_alpha_factor(0.18)
        } else {
            colors.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            if active { colors.accent[5] } else { colors.border_standard },
            None,
            &shape,
        );
        let size = theme.tokens.typography.scale.sm;
        let (tw, _) = tr.measure(label, size, None);
        let tx = rect.center().x - tw as f64 / 2.0;
        let tyy = rect.y0 + (rect.height() - size as f64) / 2.0;
        let color = if active { Color::WHITE } else { colors.text_primary };
        tr.draw_text(scene, label, size, color, transform * Affine::translate((tx, tyy)), None);
    }

    fn draw_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        // Whole-window background.
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &self.rect);

        // Toolbar.
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &self.toolbar_rect);
        let active_fmt = self.active_format();
        for b in &self.buttons {
            let is_active = matches!(b.action, ToolbarAction::Format(f) if f == active_fmt);
            let accent = matches!(b.action, ToolbarAction::Sum);
            self.draw_chip(scene, transform, tr, theme, b.rect, &b.label, is_active, accent);
        }
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &Line::new((self.toolbar_rect.x0, self.toolbar_rect.y1), (self.toolbar_rect.x1, self.toolbar_rect.y1)),
        );

        // Formula bar.
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &self.formula_rect);
        // Name box.
        let name_box = Rect::new(
            self.formula_rect.x0 + 6.0,
            self.formula_rect.y0 + 4.0,
            self.formula_rect.x0 + NAME_BOX_W,
            self.formula_rect.y1 - 4.0,
        );
        let nb_shape = RoundedRect::from_rect(name_box, theme.tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &nb_shape);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &nb_shape);
        let sm = theme.tokens.typography.scale.sm;
        let label = self.grid.selection_label();
        tr.draw_text_weighted(
            scene,
            &label,
            sm,
            colors.text_primary,
            transform * Affine::translate((name_box.x0 + 8.0, name_box.y0 + (name_box.height() - sm as f64) / 2.0)),
            None,
            TextWeight::Semibold,
        );
        // fx glyph.
        tr.draw_text_styled(
            scene,
            "fx",
            theme.tokens.typography.scale.base,
            colors.text_dim,
            transform * Affine::translate((self.formula_rect.x0 + NAME_BOX_W + 8.0, self.formula_rect.y0 + 8.0)),
            None,
            TextWeight::Regular,
            true,
            false,
        );
        // Formula field.
        let ff_shape = RoundedRect::from_rect(self.formula_field_rect, theme.tokens.radii.sm as f64);
        let editing = self.grid.is_editing();
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &ff_shape);
        scene.stroke(
            &Stroke::new(if editing { 2.0 } else { 1.0 }),
            transform,
            if editing { colors.accent[5] } else { colors.border_standard },
            None,
            &ff_shape,
        );
        let content = self.grid.active_display_raw();
        if !content.is_empty() {
            let base = theme.tokens.typography.scale.base;
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &ff_shape);
            tr.draw_text(
                scene,
                &content,
                base,
                colors.text_primary,
                transform * Affine::translate((self.formula_field_rect.x0 + 8.0, self.formula_field_rect.y0 + (self.formula_field_rect.height() - base as f64) / 2.0)),
                None,
            );
            scene.pop_layer();
        }

        // Bottom strip: sheet tabs + status.
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &self.tabs_rect);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &Line::new((self.tabs_rect.x0, self.tabs_rect.y0), (self.tabs_rect.x1, self.tabs_rect.y0)),
        );
        for (rect, idx) in &self.tab_rects {
            let active = *idx == self.active_sheet;
            let shape = RoundedRect::from_rect(*rect, theme.tokens.radii.sm as f64);
            let bg = if active { colors.surfaces.base } else { colors.surfaces.raised };
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
            if active {
                // Accent underline for the active tab.
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &Rect::new(rect.x0 + 4.0, rect.y1 - 3.0, rect.x1 - 4.0, rect.y1),
                );
            }
            let name = self.sheet_names.get(*idx).cloned().unwrap_or_default();
            let (tw, _) = tr.measure(&name, sm, None);
            let tx = rect.center().x - tw as f64 / 2.0;
            let color = if active { colors.text_primary } else { colors.text_dim };
            tr.draw_text_weighted(
                scene,
                &name,
                sm,
                color,
                transform * Affine::translate((tx, rect.y0 + (rect.height() - sm as f64) / 2.0)),
                None,
                if active { TextWeight::Semibold } else { TextWeight::Regular },
            );
        }
        // Add-sheet button.
        let add_shape = RoundedRect::from_rect(self.add_tab_rect, theme.tokens.radii.sm as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &add_shape);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &add_shape);
        tr.draw_text(
            scene,
            "+",
            theme.tokens.typography.scale.lg,
            colors.text_dim,
            transform * Affine::translate((self.add_tab_rect.center().x - 4.0, self.add_tab_rect.y0 + 4.0)),
            None,
        );
        // Status text (right-aligned).
        let (sw, _) = tr.measure(&self.status, theme.tokens.typography.scale.xs, None);
        tr.draw_text(
            scene,
            &self.status,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform * Affine::translate((self.tabs_rect.x1 - sw as f64 - 12.0, self.tabs_rect.y0 + (TABS_H - theme.tokens.typography.scale.xs as f64) / 2.0)),
            None,
        );
    }

    /// Format chip to highlight: the format of the active cell, refreshed
    /// each `sync`.
    fn active_format(&self) -> CellFormat {
        self.current_active_format
    }
}

/// Approximate tab width from the sheet name (kept identical between
/// layout and draw so hit rects line up).
fn tab_width(name: &str) -> f64 {
    (name.chars().count() as f64 * 7.5 + 26.0).clamp(64.0, 180.0)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

impl Widget for SheetsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            self.seed_if_empty(state);
            self.seeded = true;
        }
        self.reload(state);
        self.current_active_format = self.format_of_active(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
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
        self.relayout();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.draw_chrome(scene, transform, text_renderer, theme);
        self.grid.draw(scene, transform, text_renderer, theme);
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                // Toolbar?
                if self.toolbar_rect.contains(p) {
                    if let Some(action) = self.button_at(p) {
                        self.run_action(state, action);
                        *handled = true;
                    }
                    return;
                }
                // Formula field click → begin edit of active cell.
                if self.formula_field_rect.contains(p) {
                    self.grid.begin_edit_in_place();
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // Sheet tabs?
                if self.tabs_rect.contains(p) {
                    if self.add_tab_rect.contains(p) {
                        self.add_sheet(state);
                        *handled = true;
                        return;
                    }
                    for (rect, idx) in self.tab_rects.clone() {
                        if rect.contains(p) {
                            self.switch_sheet(state, idx);
                            *handled = true;
                            return;
                        }
                    }
                    return;
                }
                // Grid: detect double-click.
                let now = Instant::now();
                let cell = self.cell_under(p);
                let double = matches!(
                    (self.last_press, cell),
                    (Some((t, a)), Some(c)) if a == c && now.duration_since(t).as_millis() < 350
                );
                let mut out = Vec::new();
                if double {
                    self.grid.double_click(p, &mut out);
                    self.last_press = None;
                } else {
                    let shift = modifiers.state().shift_key();
                    let consumed = self.grid.pointer_down(p, shift, &mut out);
                    if consumed {
                        self.last_press = cell.map(|c| (now, c));
                    }
                }
                self.apply_cmds(state, out);
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.grid.pointer_up();
                *handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
                if self.grid.pointer_move(self.last_mouse) {
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-(*x as f64) * 48.0, -(*y as f64) * 48.0),
                    MouseScrollDelta::PixelDelta(pos) => {
                        (-pos.x / scale_factor, -pos.y / scale_factor)
                    }
                };
                self.grid.on_scroll(dx, dy);
                self.dirty = true;
                *handled = true;
            }
            WindowEvent::KeyboardInput { .. } => {
                let mut out = Vec::new();
                if self.grid.key(event, modifiers, &mut out) {
                    *handled = true;
                }
                self.apply_cmds(state, out);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SheetsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.grid.dirty()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.grid.mark_drawn();
    }

    fn needs_commit(&self) -> bool {
        self.needs_commit
    }

    fn on_commit(&mut self) {
        self.needs_commit = false;
    }
}

impl SheetsApp {
    fn button_at(&self, p: Point) -> Option<ToolbarAction> {
        self.buttons.iter().find(|b| b.rect.contains(p)).map(|b| b.action)
    }

    fn cell_under(&self, p: Point) -> Option<Addr> {
        if self.grid_rect.contains(p) {
            // Reuse the grid's hit-test indirectly via a public helper.
            self.grid.cell_at_public(p)
        } else {
            None
        }
    }

    fn run_action(&mut self, doc: &mut AutoCommit, action: ToolbarAction) {
        match action {
            ToolbarAction::Sum => self.auto_sum(doc),
            ToolbarAction::Format(f) => self.apply_format(doc, f),
            ToolbarAction::Clear => self.clear_selection(doc),
            ToolbarAction::Import => self.import_demo_csv(doc),
            ToolbarAction::Export => self.export_csv(doc),
        }
        self.dirty = true;
    }

    fn format_of_active(&self, doc: &mut AutoCommit) -> CellFormat {
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet);
        snap.fmt.get(&self.grid.active()).copied().unwrap_or_default()
    }
}

/// Seed a realistic two-sheet workbook: a monthly budget with SUM /
/// AVERAGE / IF formulas, and a quarterly sales sheet with growth %.
fn seed_workbook(doc: &mut AutoCommit) {
    // ---- Sheet 1: Budget ----
    let budget = SheetsDoc::add_sheet(doc, "Budget");
    let set = |doc: &mut AutoCommit, a: &str, v: &str| {
        if let Some(addr) = a1_to_addr(a) {
            SheetsDoc::set_cell_raw(doc, budget, addr, v);
        }
    };
    let setf = |doc: &mut AutoCommit, a: &str, f: CellFormat| {
        if let Some(addr) = a1_to_addr(a) {
            SheetsDoc::set_cell_format(doc, budget, addr, f);
        }
    };
    set(doc, "A1", "Category");
    set(doc, "B1", "Jan");
    set(doc, "C1", "Feb");
    set(doc, "D1", "Mar");
    set(doc, "E1", "Q1 Total");
    set(doc, "F1", "Average");

    let rows: &[(&str, f64, f64, f64)] = &[
        ("Rent", 1200.0, 1200.0, 1200.0),
        ("Groceries", 420.0, 460.0, 510.0),
        ("Utilities", 180.0, 165.0, 200.0),
        ("Transport", 90.0, 110.0, 95.0),
        ("Entertainment", 150.0, 200.0, 175.0),
    ];
    for (i, (name, jan, feb, mar)) in rows.iter().enumerate() {
        let r = i + 2; // data starts on row 2
        set(doc, &format!("A{r}"), name);
        set(doc, &format!("B{r}"), &jan.to_string());
        set(doc, &format!("C{r}"), &feb.to_string());
        set(doc, &format!("D{r}"), &mar.to_string());
        set(doc, &format!("E{r}"), &format!("=SUM(B{r}:D{r})"));
        set(doc, &format!("F{r}"), &format!("=AVERAGE(B{r}:D{r})"));
    }
    // Totals row (row 7).
    set(doc, "A7", "Total");
    set(doc, "B7", "=SUM(B2:B6)");
    set(doc, "C7", "=SUM(C2:C6)");
    set(doc, "D7", "=SUM(D2:D6)");
    set(doc, "E7", "=SUM(E2:E6)");
    set(doc, "F7", "=AVERAGE(B7:D7)");
    // Budget check.
    set(doc, "A9", "Monthly budget");
    set(doc, "B9", "2600");
    set(doc, "A10", "Over budget?");
    set(doc, "B10", "=IF(B7>B9,\"OVER\",\"OK\")");
    set(doc, "A11", "Headroom");
    set(doc, "B11", "=B9-B7");

    // Currency on the money columns/rows.
    for r in 2..=7 {
        for col in ["B", "C", "D", "E"] {
            setf(doc, &format!("{col}{r}"), CellFormat::Currency);
        }
        setf(doc, &format!("F{r}"), CellFormat::Number);
    }
    setf(doc, "B9", CellFormat::Currency);
    setf(doc, "B11", CellFormat::Currency);

    // ---- Sheet 2: Sales ----
    let sales = SheetsDoc::add_sheet(doc, "Sales");
    let sset = |doc: &mut AutoCommit, a: &str, v: &str| {
        if let Some(addr) = a1_to_addr(a) {
            SheetsDoc::set_cell_raw(doc, sales, addr, v);
        }
    };
    let ssetf = |doc: &mut AutoCommit, a: &str, f: CellFormat| {
        if let Some(addr) = a1_to_addr(a) {
            SheetsDoc::set_cell_format(doc, sales, addr, f);
        }
    };
    sset(doc, "A1", "Region");
    sset(doc, "B1", "Q1");
    sset(doc, "C1", "Q2");
    sset(doc, "D1", "Q3");
    sset(doc, "E1", "Q4");
    sset(doc, "F1", "Year");
    sset(doc, "G1", "Growth");
    let sales_rows: &[(&str, f64, f64, f64, f64)] = &[
        ("North", 12000.0, 13500.0, 14200.0, 15800.0),
        ("South", 9800.0, 10200.0, 9900.0, 11500.0),
        ("East", 15200.0, 16100.0, 17400.0, 18900.0),
        ("West", 8400.0, 9100.0, 9700.0, 10800.0),
    ];
    for (i, (name, q1, q2, q3, q4)) in sales_rows.iter().enumerate() {
        let r = i + 2;
        sset(doc, &format!("A{r}"), name);
        sset(doc, &format!("B{r}"), &q1.to_string());
        sset(doc, &format!("C{r}"), &q2.to_string());
        sset(doc, &format!("D{r}"), &q3.to_string());
        sset(doc, &format!("E{r}"), &q4.to_string());
        sset(doc, &format!("F{r}"), &format!("=SUM(B{r}:E{r})"));
        sset(doc, &format!("G{r}"), &format!("=(E{r}-B{r})/B{r}"));
    }
    sset(doc, "A6", "Total");
    sset(doc, "B6", "=SUM(B2:B5)");
    sset(doc, "C6", "=SUM(C2:C5)");
    sset(doc, "D6", "=SUM(D2:D5)");
    sset(doc, "E6", "=SUM(E2:E5)");
    sset(doc, "F6", "=SUM(F2:F5)");
    for r in 2..=6 {
        for col in ["B", "C", "D", "E", "F"] {
            ssetf(doc, &format!("{col}{r}"), CellFormat::Currency);
        }
        ssetf(doc, &format!("G{r}"), CellFormat::Percent);
    }
}

/// Sample CSV used by the "Import CSV" toolbar action (no file picker in
/// headless mode, so the demo imports an embedded dataset).
const DEMO_CSV: &str = "Product,Units,Price,Revenue\n\
Widget,120,4.50,=B2*C2\n\
Gadget,85,12.00,=B3*C3\n\
Gizmo,200,2.25,=B4*C4\n\
Total,=SUM(B2:B4),,=SUM(D2:D4)\n";
