//! What a press, a wheel or a key does: the targets the last paint recorded, the cell menu and its rows, the annotation cards, the palette and chrome clicks, and the clipboard chords a keyboard sends.

use super::*;

impl SheetsApp {
    pub(super) fn annotation_click_from_target(&self, target: &str) -> Option<AnnotationClick> {
        let mut parts = target.strip_prefix("sheets.annotation:")?.split(':');
        let (Some(kind), Some(id), Some(operation), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        match (kind, operation) {
            ("chart", "open") if self.sheet_charts.iter().any(|chart| chart.id == id) => self
                .sheet_charts
                .iter()
                .find(|chart| chart.id == id)
                .map(|chart| AnnotationClick::Open(chart.start)),
            ("chart", "manage") if self.sheet_charts.iter().any(|chart| chart.id == id) => {
                Some(AnnotationClick::ManageChart(id.to_string()))
            }
            ("chart", "edit") if self.sheet_charts.iter().any(|chart| chart.id == id) => {
                Some(AnnotationClick::EditChart(id.to_string()))
            }
            ("chart", "delete") if self.sheet_charts.iter().any(|chart| chart.id == id) => {
                Some(AnnotationClick::DeleteChart(id.to_string()))
            }
            ("chart", "cells") if self.sheet_charts.iter().any(|chart| chart.id == id) => {
                Some(AnnotationClick::ChartCells(id.to_string()))
            }
            ("chart", code) if self.sheet_charts.iter().any(|chart| chart.id == id) => {
                ChartKind::parse(code)
                    .map(|kind| AnnotationClick::SetChartKind(id.to_string(), kind))
            }
            ("comment", "open") => self
                .sheet_comments
                .iter()
                .find(|thread| thread.id == id)
                .map(|thread| AnnotationClick::Open(thread.start)),
            ("comment", "reply") if self.sheet_comments.iter().any(|thread| thread.id == id) => {
                Some(AnnotationClick::Reply(id.to_string()))
            }
            ("comment", "edit") if self.sheet_comments.iter().any(|thread| thread.id == id) => {
                Some(AnnotationClick::EditComment(id.to_string()))
            }
            ("comment", "delete") if self.sheet_comments.iter().any(|thread| thread.id == id) => {
                Some(AnnotationClick::DeleteComment(id.to_string()))
            }
            ("comment", "toggle") => self
                .sheet_comments
                .iter()
                .find(|thread| thread.id == id)
                .map(|thread| AnnotationClick::ToggleComment(id.to_string(), !thread.resolved)),
            _ => None,
        }
    }

    pub(super) fn apply_annotation_click(
        &mut self,
        action: AnnotationClick,
        state: &mut Edits<'_>,
    ) {
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        match action {
            AnnotationClick::ManageChart(id) => {
                self.managed_chart = (self.managed_chart.as_ref() != Some(&id)).then_some(id);
            }
            AnnotationClick::Open(addr) => {
                self.grid.select_only(addr);
                self.status = format!("Opened {}", crate::model::addr_to_a1(addr.0, addr.1));
            }
            AnnotationClick::EditChart(id) => {
                let Some(chart) = self.sheet_charts.iter().find(|chart| chart.id == id) else {
                    self.status = "Chart no longer exists".into();
                    return;
                };
                let title = chart.title.clone();
                if self.ask_annotation(&format!("chart:{id}"), "edit-chart", title) {
                    self.status = "Editing chart".into();
                }
            }
            AnnotationClick::SetChartKind(id, kind) => {
                let path = Workbook::paths()
                    .sheets
                    .item(&sheet)
                    .charts
                    .item(&Id::from_raw(id))
                    .kind;
                if let Ok(invoke) = Invoke::edit(&path.set(kind))
                    && self.submit(state, invoke)
                {
                    self.status = format!("{} chart", kind.label());
                }
            }
            AnnotationClick::ChartCells(id) => {
                let Some(chart) = self.sheet_charts.iter().find(|chart| chart.id == id) else {
                    self.status = "Chart no longer exists".into();
                    return;
                };
                let range = format!(
                    "{}:{}",
                    crate::model::addr_to_a1(chart.start.0, chart.start.1),
                    crate::model::addr_to_a1(chart.end.0, chart.end.1),
                );
                if self.ask_annotation(&format!("chart:{id}"), "chart-cells", range) {
                    self.status = "Choosing the chart's cells".into();
                }
            }
            AnnotationClick::DeleteChart(id) => {
                self.submit(
                    state,
                    Workbook::DELETE_CHART.with(verbs::DeleteChart {
                        sheet,
                        chart: Id::from_raw(id),
                    }),
                );
                self.status = "Deleted chart".into();
            }
            AnnotationClick::Reply(id) => {
                if self.ask_annotation(&format!("comment:{id}"), "reply", String::new()) {
                    self.status = "Replying to comment".into();
                }
            }
            AnnotationClick::EditComment(id) => {
                let Some(thread) = self.sheet_comments.iter().find(|thread| thread.id == id) else {
                    self.status = "Comment no longer exists".into();
                    return;
                };
                let body = thread.body.clone();
                if self.ask_annotation(&format!("comment:{id}"), "edit-comment", body) {
                    self.status = "Editing comment".into();
                }
            }
            AnnotationClick::ToggleComment(id, resolved) => {
                let path = Workbook::paths().comments.item(&Id::from_raw(id)).resolved;
                if let Ok(invoke) = Invoke::edit(&path.set(resolved)) {
                    self.submit(state, invoke);
                }
                self.status = if resolved {
                    "Resolved comment"
                } else {
                    "Reopened comment"
                }
                .into();
            }
            AnnotationClick::DeleteComment(id) => {
                self.submit(
                    state,
                    Workbook::DELETE_COMMENT.with(verbs::DeleteComment {
                        comment: Id::from_raw(id),
                    }),
                );
                self.status = "Deleted comment".into();
            }
        }
        self.reload();
        self.pending.redraw = true;
    }

    /// Ask in the palette for what the published action `action` of the
    /// annotation `place` (`chart:<id>`, `comment:<id>`) takes, starting
    /// from `prefill`; the palette invokes it as Ctrl+K does.
    pub(super) fn ask_annotation(&mut self, place: &str, action: &str, prefill: String) -> bool {
        let resources = self.published();
        let Some(flat) = tilo_actions::flatten(&resources)
            .into_iter()
            .find(|flat| flat.action.id == action && flat.resource_id.0.ends_with(place))
        else {
            self.status = "That is no longer there".into();
            return false;
        };
        let entry = CommandEntry::from_flat_action(&flat);
        self.palette.open_action_argument(entry, Some(prefill));
        self.pending.redraw = true;
        true
    }

    /// Run the row the open cell menu reported, if any.
    pub(super) fn drain_cell_menu(&mut self, doc: &mut Edits<'_>) {
        if let Some(choice) = self.cell_menu.take_selection() {
            let row = self.cell_menu_rows.get(choice).copied().flatten();
            if let Some(row) = row {
                self.run_cell_menu_row(doc, row);
            }
            self.cell_menu_target = None;
        }
        if !self.cell_menu.is_open() {
            self.cell_menu_target = None;
        }
        self.pending.redraw = true;
    }

    pub(super) fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let before = self.last_mouse;
        self.last_mouse = Point::new(x / scale_factor, y / scale_factor);
        // The painted buttons and tabs light up under the pointer, so a move
        // into, within or out of the bars repaints them.
        let on_bar = |p: Point| self.tabs_rect.contains(p) || self.annotation_hit(p);
        if on_bar(before) || on_bar(self.last_mouse) {
            self.pending.redraw = true;
        }
        // A drag that began in the formula bar extends the text
        // selection there, not the grid selection.
        if self.formula_bar_selecting {
            let idx = self.formula_caret_at_x(self.last_mouse.x);
            self.grid.set_edit_caret_shown(idx, true);
            self.pending.redraw = true;
            *handled = true;
            return;
        }
        if self.settings_panel != SettingsPanel::None {
            self.pending.redraw = true;
        }
        if self.grid.pointer_move(self.last_mouse) {
            self.pending.redraw = true;
            *handled = true;
        }
    }

    /// Clipboard + select-all chords while a cell/formula-bar edit owns the
    /// keyboard, operating on the edit buffer's text selection (so Ctrl+V
    /// pastes text instead of inserting the letter 'v'). Returns whether the
    /// chord was handled.
    pub(super) fn handle_edit_clipboard_chord(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        if !(ke.state.is_pressed() && ctrl && self.grid.is_editing()) {
            return false;
        }
        let winit::keyboard::Key::Character(c) = &ke.logical_key else {
            return false;
        };
        match c.as_str() {
            "a" | "A" => {
                self.grid.select_all_edit();
                *handled = true;
                self.pending.redraw = true;
            }
            "c" | "C" => {
                if let Some(t) = self.grid.edit_selected_text() {
                    ui::clipboard_copy(&t);
                }
                *handled = true;
            }
            "x" | "X" => {
                if let Some(t) = self.grid.edit_cut_text() {
                    ui::clipboard_copy(&t);
                }
                *handled = true;
                self.pending.redraw = true;
            }
            "v" | "V" => {
                if let Some(t) = ui::clipboard_paste() {
                    self.grid.edit_paste_text(&t);
                }
                *handled = true;
                self.pending.redraw = true;
            }
            _ => return false,
        }
        true
    }

    /// Ctrl+C/X/V/A/D/R while the grid selection (not a cell edit) owns the
    /// keyboard. Returns whether the chord was handled.
    pub(super) fn handle_grid_clipboard_chord(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        state: &mut Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        if !(ke.state.is_pressed() && ctrl && !self.grid.is_editing()) {
            return false;
        }
        let winit::keyboard::Key::Character(c) = &ke.logical_key else {
            return false;
        };
        match c.as_str() {
            "c" | "C" => {
                self.copy_selection(false);
                *handled = true;
            }
            "x" | "X" => {
                self.copy_selection(true);
                *handled = true;
            }
            "v" | "V" => {
                self.paste(state);
                *handled = true;
                self.pending.redraw = true;
            }
            "a" | "A" => {
                self.grid.select_used_range();
                *handled = true;
                self.pending.redraw = true;
            }
            "b" | "B" => {
                self.toggle_bold(state);
                *handled = true;
                self.pending.redraw = true;
            }
            "d" | "D" => {
                self.fill_down(state);
                *handled = true;
                self.pending.redraw = true;
            }
            "r" | "R" => {
                self.fill_right(state);
                *handled = true;
                self.pending.redraw = true;
            }
            _ => return false,
        }
        true
    }

    /// The command palette (when open) owns the keyboard: arrows, Enter, Esc
    /// and typing are consumed by it and never reach the grid behind. On
    /// Enter it emits an outcome we dispatch here. Otherwise: Ctrl+K opens
    /// the palette, Escape closes an overlay, clipboard chords route to either
    /// the grid selection or an in-progress edit's text selection, and
    /// anything left falls through to the grid's own key handling. Inline
    /// editors are routed before this method by [`Self::route_inline_editor`].
    pub(super) fn handle_keyboard_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut Edits<'_>,
        handled: &mut bool,
        ctx: &mut PaletteRouteCtx<'_>,
    ) {
        if self.palette.is_open() {
            self.palette.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor,
                    modifiers: &modifiers,
                    state: state.reborrow(),
                    path: ctx.path,
                    handled,
                    focus_target: ctx.focus_target,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
            );
            self.pending.redraw = true;
            *handled = true;
            return;
        }
        // Familiar spreadsheet navigation stays on the keyboard, whatever
        // the window bar has room for.
        if let WindowEvent::KeyboardInput { event: ke, .. } = event {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            let shift = modifiers.state().shift_key();
            if ke.state.is_pressed()
                && ctrl
                && let winit::keyboard::Key::Character(c) = &ke.logical_key
            {
                if c.as_str().eq_ignore_ascii_case("f") && !shift {
                    self.open_find(ui::window_toolbar::SearchOpening::Find);
                    *handled = true;
                    return;
                }
                if c.as_str().eq_ignore_ascii_case("h") {
                    self.open_find(ui::window_toolbar::SearchOpening::Replace);
                    *handled = true;
                    return;
                }
            }
            if ke.state.is_pressed()
                && ctrl
                && !self.grid.is_editing()
                && let winit::keyboard::Key::Character(c) = &ke.logical_key
            {
                if c.as_str().eq_ignore_ascii_case("g") {
                    self.begin_name_box_edit();
                    *handled = true;
                    return;
                }
                if modifiers.state().shift_key() && c.as_str().eq_ignore_ascii_case("f") {
                    self.toggle_formula_view();
                    *handled = true;
                    return;
                }
                // Ctrl+K opens the command palette.
                if c.as_str().eq_ignore_ascii_case("k") {
                    self.open_command_palette();
                    *handled = true;
                    return;
                }
            }
        }

        // Escape closes any open overlay.
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state.is_pressed()
            && matches!(
                ke.logical_key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            )
            && self.settings_panel != SettingsPanel::None
        {
            self.settings_panel = SettingsPanel::None;
            self.pending.redraw = true;
            *handled = true;
            return;
        }

        if self.handle_grid_clipboard_chord(event, modifiers, state, handled) {
            return;
        }
        if self.handle_edit_clipboard_chord(event, modifiers, handled) {
            return;
        }

        let mut out = Vec::new();
        if self.grid.key(event, &modifiers, &mut out) {
            *handled = true;
        }
        self.apply_cmds(state, out);
        self.pending.redraw = true;
    }

    /// Left-press: routes to whichever modal surface is open (palette,
    /// rename, name box, tab menu, settings panel, dropdown menu), then the
    /// menu bar / name box / fx / formula field / sheet tabs, and finally the
    /// grid body, in priority order.
    pub(super) fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        state: &mut Edits<'_>,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let p = self.last_mouse;
        if self.try_palette_click(p, state, path, handled) {
            return;
        }
        if self.try_settings_panel_click(p, state, handled) {
            return;
        }
        if self.try_chrome_click(p, modifiers, state, handled) {
            return;
        }
        if self.try_annotation_click(p, state, handled) {
            return;
        }
        self.try_grid_click(p, modifiers, state, handled);
    }

    pub(super) fn handle_mouse_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (-f64::from(*x) * 48.0, -f64::from(*y) * 48.0),
            MouseScrollDelta::PixelDelta(pos) => (-pos.x / scale_factor, -pos.y / scale_factor),
        };
        self.grid.on_scroll(dx, dy);
        self.pending.redraw = true;
        *handled = true;
    }

    pub(super) fn handle_right_press(&mut self, handled: &mut bool) {
        let p = self.last_mouse;
        // Right-click on a sheet tab opens its context menu.
        if self.tabs_rect.contains(p)
            && let Some(idx) = self
                .tab_rects
                .iter()
                .find(|(r, _)| r.contains(p))
                .map(|(_, i)| *i)
        {
            self.open_tab_menu(idx);
            self.pending.redraw = true;
            *handled = true;
            return;
        }
        // Right-click on a cell opens the cell menu. Pressing a cell outside
        // the selection makes it the selection first, as a right-press does
        // in every spreadsheet; a press inside one keeps the range so a
        // multi-row delete is one action.
        if let Some(addr) = self.cell_under(p) {
            if !self.grid.selected_cells().contains(&addr) {
                self.grid.select_only(addr);
            }
            self.open_cell_menu(p);
            self.pending.redraw = true;
            *handled = true;
        }
    }

    /// Open the cell context menu at `p`, naming the range and the count each
    /// structural edit will act on.
    pub(super) fn open_cell_menu(&mut self, p: Point) {
        self.close_overlays();
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let rows = span(r0, r1);
        let cols = span(c0, c1);
        let plural = |n: u32, one: &str, many: &str| {
            if n == 1 {
                one.to_owned()
            } else {
                format!("{n} {many}")
            }
        };
        let first_col = crate::model::col_to_letters(c0);
        let last_col = crate::model::col_to_letters(c1);
        let all_bold = self
            .grid
            .selected_cells()
            .iter()
            .all(|addr| self.grid.cell_is_bold(*addr));
        let mut context = ContextMenu::new();
        let mut rows_out: Vec<Option<CellMenuRow>> = Vec::new();
        macro_rules! row {
            ($label:expr, $item:expr) => {{
                context.push(Row::selection($label));
                rows_out.push(Some($item));
            }};
        }
        macro_rules! destructive {
            ($label:expr, $item:expr) => {{
                context.push(Row::destructive_selection($label));
                rows_out.push(Some($item));
            }};
        }
        macro_rules! separator {
            () => {{
                context.push(Row::separator());
                rows_out.push(None);
            }};
        }
        row!("Cut", CellMenuRow::Cut);
        row!("Copy", CellMenuRow::Copy);
        row!("Paste", CellMenuRow::Paste);
        separator!();
        row!(
            format!("Insert {} above", plural(rows, "row", "rows")),
            CellMenuRow::InsertRowsAbove
        );
        row!(
            format!("Insert {} below", plural(rows, "row", "rows")),
            CellMenuRow::InsertRowsBelow
        );
        destructive!(
            format!("Delete {}", plural(rows, "row", "rows")),
            CellMenuRow::DeleteRows
        );
        separator!();
        row!(
            format!(
                "Insert {} left of {}",
                plural(cols, "column", "columns"),
                first_col
            ),
            CellMenuRow::InsertColumnsLeft
        );
        row!(
            format!(
                "Insert {} right of {}",
                plural(cols, "column", "columns"),
                last_col
            ),
            CellMenuRow::InsertColumnsRight
        );
        destructive!(
            format!("Delete {}", plural(cols, "column", "columns")),
            CellMenuRow::DeleteColumns
        );
        separator!();
        row!("Clear contents", CellMenuRow::ClearContents);
        context.push(Row::selection(if all_bold { "Regular" } else { "Bold" }));
        rows_out.push(Some(CellMenuRow::Bold));
        let currency = self.default_currency();
        row!(
            "Format as currency",
            CellMenuRow::Format(CellFormat::Currency(currency))
        );
        row!(
            "Format as percent",
            CellMenuRow::Format(CellFormat::Percent)
        );
        row!("Clear formatting", CellMenuRow::ClearFormat);
        context.set_align(ui::widget::PanelAlign::Leading);
        context.open_at_clamped([p.x as f32, p.y as f32], self.float_bounds());
        self.cell_menu = context;
        self.cell_menu_rows = rows_out;
        self.cell_menu_target = self.cell_under(p);
        self.pending.redraw = true;
    }

    /// Route every input kind through the active shared inline editor before
    /// the spreadsheet's grid or chrome can see it. Both editors are modal in
    /// Sheets: a tab rename blur-commits, while a Name-box blur cancels.
    pub(super) fn route_inline_editor(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut Edits<'_>,
        handled: &mut bool,
        ctx: &mut PaletteRouteCtx<'_>,
    ) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        if self.tab_rename.is_some() {
            let outcome = {
                let Some(rename) = self.tab_rename.as_mut() else {
                    return false;
                };
                rename.editor.apply_layout_external(rename.rect);
                rename.editor.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor,
                        modifiers: &modifiers,
                        state: state.reborrow(),
                        path: ctx.path,
                        handled,
                        focus_target: ctx.focus_target,
                        node_id: ctx.node_id,
                        taffy: ctx.taffy,
                    },
                );
                rename.editor.take_outcome()
            };
            self.resolve_tab_rename_outcome(state, outcome);
            self.pending.redraw = true;
            // The previous inline editor was modal too: clicking away commits
            // but does not also activate the newly clicked spreadsheet target.
            *handled = true;
            return true;
        }

        if self.name_box_edit.is_some() {
            if matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent {
                        logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
            ) || matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } if !self.name_box_rect.contains(self.last_mouse)
            ) {
                self.name_box_edit = None;
                self.pending.redraw = true;
                *handled = true;
                return true;
            }

            let submitted = {
                let Some(input) = self.name_box_edit.as_mut() else {
                    return false;
                };
                input.apply_layout_external(Rect::new(
                    self.name_box_rect.x0 + 8.0,
                    self.name_box_rect.y0 + 2.0,
                    self.name_box_rect.x1 - 4.0,
                    self.name_box_rect.y1 - 2.0,
                ));
                input.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor,
                        modifiers: &modifiers,
                        state: state.reborrow(),
                        path: ctx.path,
                        handled,
                        focus_target: ctx.focus_target,
                        node_id: ctx.node_id,
                        taffy: ctx.taffy,
                    },
                );
                input.take_submit()
            };
            if submitted {
                self.commit_name_box();
            }
            self.pending.redraw = true;
            *handled = true;
            return true;
        }

        false
    }

    /// One cell-menu command. Structural edits reach the workbook as a single
    /// verb with the range's count or rows, so undo takes the whole change
    /// back in one step; cell contents and formats go through the app's
    /// grouped write path, which is also one step.
    pub(super) fn run_cell_menu_row(&mut self, doc: &mut Edits<'_>, row: CellMenuRow) {
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        match row {
            CellMenuRow::Cut => {
                self.copy_selection(true);
                self.status = "Cut selection".into();
            }
            CellMenuRow::Copy => {
                self.copy_selection(false);
                self.status = "Copied selection".into();
            }
            CellMenuRow::Paste => {
                self.paste(doc);
            }
            CellMenuRow::InsertRowsAbove => {
                let count = span(r0, r1);
                self.submit(
                    doc,
                    Workbook::INSERT_ROWS.with(verbs::InsertRows {
                        sheet,
                        at: r0.saturating_add(1),
                        count: Some(count),
                    }),
                );
                self.reload();
                self.status = format!(
                    "Inserted {} above row {}",
                    if count == 1 {
                        "row".to_owned()
                    } else {
                        format!("{count} rows")
                    },
                    r0.saturating_add(1)
                );
            }
            CellMenuRow::InsertRowsBelow => {
                let count = span(r0, r1);
                let at = r1.saturating_add(2);
                self.submit(
                    doc,
                    Workbook::INSERT_ROWS.with(verbs::InsertRows {
                        sheet,
                        at,
                        count: Some(count),
                    }),
                );
                self.reload();
                self.status = format!(
                    "Inserted {} below row {}",
                    if count == 1 {
                        "row".to_owned()
                    } else {
                        format!("{count} rows")
                    },
                    r1.saturating_add(1)
                );
            }
            CellMenuRow::DeleteRows => {
                let rows: Vec<u32> = (r0..=r1).map(|r| r.saturating_add(1)).collect();
                let count = rows.len();
                self.submit(
                    doc,
                    Workbook::DELETE_ROWS.with(verbs::DeleteRows { sheet, rows }),
                );
                self.reload();
                self.status = format!(
                    "Deleted {}",
                    if count == 1 {
                        "row".to_owned()
                    } else {
                        format!("{count} rows")
                    }
                );
            }
            CellMenuRow::InsertColumnsLeft => {
                let count = span(c0, c1);
                let at = crate::model::col_to_letters(c0);
                self.submit(
                    doc,
                    Workbook::INSERT_COLUMNS.with(verbs::InsertColumns {
                        sheet,
                        at: at.clone(),
                        count: Some(count),
                    }),
                );
                self.reload();
                self.status = format!("Inserted {count} column(s) left of {at}");
            }
            CellMenuRow::InsertColumnsRight => {
                let count = span(c0, c1);
                let at = crate::model::col_to_letters(c1.saturating_add(1));
                self.submit(
                    doc,
                    Workbook::INSERT_COLUMNS.with(verbs::InsertColumns {
                        sheet,
                        at: at.clone(),
                        count: Some(count),
                    }),
                );
                self.reload();
                self.status = format!("Inserted {count} column(s) right of {at}");
            }
            CellMenuRow::DeleteColumns => {
                let columns: Vec<String> = (c0..=c1).map(crate::model::col_to_letters).collect();
                let count = columns.len();
                self.submit(
                    doc,
                    Workbook::DELETE_COLUMNS.with(verbs::DeleteColumns { sheet, columns }),
                );
                self.reload();
                self.status = format!(
                    "Deleted {}",
                    if count == 1 {
                        "column".to_owned()
                    } else {
                        format!("{count} columns")
                    }
                );
            }
            CellMenuRow::ClearContents => {
                let writes: Vec<(Addr, String, Option<CellFormat>)> = self
                    .grid
                    .selected_cells()
                    .into_iter()
                    .map(|addr| (addr, String::new(), None))
                    .collect();
                let count = writes.len();
                self.write_raw(doc, writes);
                self.reload();
                self.status = format!("Cleared {count} cell(s)");
            }
            CellMenuRow::Bold => self.toggle_bold(doc),
            CellMenuRow::Format(format) => self.apply_format(doc, format),
            CellMenuRow::ClearFormat => self.clear_format(doc),
        }
        self.pending.redraw = true;
    }

    pub(super) fn try_annotation_click(
        &mut self,
        p: Point,
        state: &mut Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        let Some(action) = self
            .annotation_clicks
            .iter()
            .find(|(rect, _)| rect.contains(p))
            .map(|(_, action)| action.clone())
        else {
            return false;
        };
        self.apply_annotation_click(action, state);
        *handled = true;
        true
    }

    /// Chrome hit-tests below the modal/menu checks: name box, "fx", formula
    /// field, and the sheet-tab strip.
    pub(super) fn try_chrome_click(
        &mut self,
        p: Point,
        modifiers: Modifiers,
        state: &mut Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        // Name box → open a Go-To entry seeded with the active cell.
        if self.name_box_rect.contains(p) {
            self.begin_name_box_edit();
            *handled = true;
            return true;
        }
        // "fx" → focus the active cell for a formula entry (seed `=`).
        if self.fx_rect.contains(p) {
            self.start_formula_entry();
            *handled = true;
            return true;
        }

        // Formula field click → edit the active cell, positioning the
        // caret where the user clicked (shift-click extends a selection).
        if self.formula_field_rect.contains(p) {
            let shift = modifiers.state().shift_key();
            if !self.grid.is_editing() {
                self.grid.begin_edit_in_place();
            }
            let idx = self.formula_caret_at_x(p.x);
            self.grid.set_edit_caret_shown(idx, shift);
            self.formula_bar_selecting = true;
            self.pending.redraw = true;
            *handled = true;
            return true;
        }
        // Sheet tabs?
        if self.tabs_rect.contains(p) {
            if self.add_tab_rect.contains(p) {
                self.add_sheet(state);
                *handled = true;
                return true;
            }
            for (rect, idx) in self.tab_rects.clone() {
                if rect.contains(p) {
                    self.switch_sheet(state, idx);
                    *handled = true;
                    return true;
                }
            }
            return true;
        }
        false
    }

    /// Fallback: click lands on the grid body. Detects double-click (skipped
    /// while a formula is being edited, since grid clicks there are
    /// reference insertions in point mode) and drives the resulting
    /// pointer-down / selection.
    pub(super) fn try_grid_click(
        &mut self,
        p: Point,
        modifiers: Modifiers,
        state: &mut Edits<'_>,
        handled: &mut bool,
    ) {
        let now = ui::time::now();
        let cell = self.cell_under(p);
        let double = !self.grid.editing_formula()
            && matches!(
                (self.last_press, cell),
                (Some((t, a)), Some(c)) if a == c && now.duration_since(t).as_millis() < 350
            );
        let mut out = Vec::new();
        if double {
            self.grid.double_click(p, &mut out);
            self.last_press = None;
        } else {
            let shift = modifiers.state().shift_key();
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            let consumed = self.grid.pointer_down(p, shift, ctrl, &mut out);
            if consumed {
                self.last_press = cell.map(|c| (now, c));
            }
        }
        self.apply_cmds(state, out);
        self.pending.redraw = true;
        *handled = true;
    }

    /// The command palette is modal while open: a click on a row runs it, a
    /// click elsewhere dismisses it.
    pub(super) fn try_palette_click(
        &mut self,
        p: Point,
        state: &mut Edits<'_>,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if !self.palette.is_open() {
            return false;
        }
        if let Some(i) = self.palette.row_at(p) {
            self.palette.set_cursor(i);
            self.palette.activate_selected(state, path);
        } else if !Widget::get_rect(&self.palette).contains(p) {
            self.palette.close();
        }
        self.pending.redraw = true;
        *handled = true;
        true
    }
}
