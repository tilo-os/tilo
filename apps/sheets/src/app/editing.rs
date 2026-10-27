//! The commands the chrome hands back: the name box, the formula entry, bold and formula view, a tab's rename, the find bar and the menu rows each action names.

use super::*;

impl SheetsApp {
    // --- name box (Go-To) + fx formula entry -------------------------------

    /// Open the Name box for a Go-To entry, seeded with the active cell's
    /// reference and fully selected (Excel-style: the first keystroke replaces
    /// it). Closes any other overlay so the box owns input.
    pub(super) fn begin_name_box_edit(&mut self) {
        self.close_overlays();
        let (c, r) = self.grid.active();
        let reference = crate::model::addr_to_a1(c, r);
        let mut input = TextInput::local()
            .with_initial_value(&reference)
            .with_char_filter(reference_box_char)
            .with_plain_chrome()
            .with_text_role(TextRole::Body)
            .with_enter_submits(true)
            .with_id("sheet_name_box");
        input.set_focus(true);
        input.select_all();
        self.name_box_edit = Some(input);
        self.pending.redraw = true;
    }

    pub(super) fn begin_tab_rename(&mut self, idx: usize) {
        let name = self.sheet_names.get(idx).cloned().unwrap_or_default();
        let rect = self.tab_rename_rect(idx);
        let mut editor = EditableLabel::new(&name)
            .with_text_role(TextRole::Label)
            .with_id("sheet_tab_rename");
        editor.begin_edit();
        self.tab_menu_target = None;
        self.tab_rename = Some(TabRename {
            idx,
            rect,
            text_w: 0.0,
            editor,
        });
        self.pending.redraw = true;
    }

    pub(super) fn cell_under(&self, p: Point) -> Option<Addr> {
        if self.grid_rect.contains(p) {
            self.grid.cell_at_public(p)
        } else {
            None
        }
    }

    /// Commit a Name box entry: parse it as a single reference or a range and
    /// move the selection there (scrolling it into view), or report an invalid
    /// reference without changing the selection.
    pub(super) fn commit_name_box(&mut self) {
        let Some(input) = self.name_box_edit.take() else {
            return;
        };
        let text = input.value().trim();
        if text.is_empty() {
            self.pending.redraw = true;
            return;
        }
        match parse_ref_target(text) {
            Some((anchor, active)) => {
                if anchor == active {
                    self.grid.select_only(anchor);
                } else {
                    self.grid.select_range_public(anchor, active);
                }
                self.status = format!("Went to {}", self.grid.selection_label());
            }
            None => {
                self.status = format!("Invalid cell reference: {text}");
            }
        }
        self.pending.redraw = true;
    }

    /// Map a layout-space x within the formula bar to the nearest character
    /// boundary (caret index), using the per-character offsets cached on the
    /// last draw. Falls back to `0` before the first frame.
    pub(super) fn formula_caret_at_x(&self, x: f64) -> usize {
        let mut best = 0usize;
        let mut best_d = f64::INFINITY;
        for (i, &bx) in self.formula_char_x.iter().enumerate() {
            let d = (bx - x).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }

    // --- find and replace --------------------------------------------------

    /// Open the window bar's search on the workbook: on its query for Find,
    /// on its replacement for Replace. The bar hosts the field at every
    /// width, with match case, whole word and the replacement.
    pub(super) fn open_find(&mut self, opening: ui::window_toolbar::SearchOpening) {
        self.close_overlays();
        ui::window_toolbar::open_search(opening);
    }

    pub(super) fn resolve_tab_rename_outcome(&mut self, doc: &mut Edits<'_>, outcome: EditOutcome) {
        match outcome {
            EditOutcome::Committed(name) => {
                let Some(rename) = self.tab_rename.take() else {
                    return;
                };
                let trimmed = name.trim();
                let unchanged = self
                    .sheet_names
                    .get(rename.idx)
                    .is_some_and(|current| current == trimmed);
                if !trimmed.is_empty() && !unchanged {
                    if let Some(id) = self.sheet_ids.get(rename.idx).cloned() {
                        let name = Workbook::paths().sheets.item(&id).name;
                        if let Ok(invoke) = Invoke::edit(&name.set(Atom(trimmed.to_owned()))) {
                            self.submit(doc, invoke);
                        }
                    }
                    self.reload();
                    self.status = format!("Renamed sheet to {trimmed}");
                }
            }
            EditOutcome::Cancelled => {
                self.tab_rename = None;
            }
            EditOutcome::None => return,
        }
        self.pending.redraw = true;
    }

    pub(super) fn run_menu_item(&mut self, doc: &mut Edits<'_>, item: &MenuItem) {
        match item {
            MenuItem::NewSheet => self.add_sheet(doc),
            MenuItem::ImportCsv => {
                self.invoke_effect(&actions::SheetsEffect::ImportCsv { path: None });
            }
            MenuItem::ExportCsv => {
                self.invoke_effect(&actions::SheetsEffect::ExportCsv { path: None });
            }
            MenuItem::ImportXlsx => {
                self.invoke_effect(&actions::SheetsEffect::ImportXlsx { path: None });
            }
            MenuItem::ExportXlsx => {
                self.invoke_effect(&actions::SheetsEffect::ExportXlsx { path: None });
            }
            MenuItem::ImportOds => {
                self.invoke_effect(&actions::SheetsEffect::ImportOds { path: None });
            }
            MenuItem::ExportOds => {
                self.invoke_effect(&actions::SheetsEffect::ExportOds { path: None });
            }
            MenuItem::SetFormat(f) => self.apply_format(doc, *f),
            MenuItem::ClearFormat => self.clear_format(doc),
            MenuItem::OpenDocSettings => self.open_doc_settings(),
            MenuItem::AutoSum => self.auto_sum(doc),
            MenuItem::ToggleFormulaView => self.toggle_formula_view(),
            MenuItem::ToggleBold => self.toggle_bold(doc),
        }
        self.pending.redraw = true;
    }

    /// Execute a tab context-menu action against sheet `idx`.
    pub(super) fn run_tab_action(&mut self, doc: &mut Edits<'_>, idx: usize, act: TabAction) {
        let Some(id) = self.sheet_ids.get(idx).cloned() else {
            return;
        };
        let sheets = Workbook::paths().sheets;
        match act {
            TabAction::Rename => self.begin_tab_rename(idx),
            TabAction::Duplicate => {
                if self.submit(
                    doc,
                    Workbook::DUPLICATE_SHEET.with(verbs::DuplicateSheet { sheet: id }),
                ) {
                    // The duplicate lands immediately after the source.
                    self.active_sheet = idx + 1;
                    self.grid.select_only((0, 0));
                    self.reload();
                    self.status = "Duplicated sheet".into();
                }
            }
            TabAction::InsertRight => {
                if self.submit(
                    doc,
                    Workbook::NEW_SHEET.with(verbs::NewSheet {
                        new: crate::resource::SheetSpec {
                            after: Some(id),
                            ..crate::resource::SheetSpec::default()
                        },
                    }),
                ) {
                    self.active_sheet = idx + 1;
                    self.grid.select_only((0, 0));
                    self.reload();
                    self.status = "Inserted sheet".into();
                }
            }
            TabAction::MoveLeft => {
                if idx > 0
                    && let Ok(invoke) = Invoke::edit(&sheets.move_to(&id, idx - 1))
                    && self.submit(doc, invoke)
                {
                    self.active_sheet = idx - 1;
                    self.reload();
                    self.status = "Moved sheet left".into();
                }
            }
            TabAction::MoveRight => {
                if idx + 1 < self.sheet_ids.len()
                    && let Ok(invoke) = Invoke::edit(&sheets.move_to(&id, idx + 1))
                    && self.submit(doc, invoke)
                {
                    self.active_sheet = idx + 1;
                    self.reload();
                    self.status = "Moved sheet right".into();
                }
            }
            TabAction::Delete => {
                if self.sheet_ids.len() <= 1 {
                    self.status = "A workbook keeps at least one sheet".into();
                } else {
                    let old_active = self.active_sheet;
                    if self.submit(
                        doc,
                        Workbook::DELETE_SHEET.with(verbs::DeleteSheet { sheet: id }),
                    ) {
                        let count = self.sheet_ids.len().max(1);
                        let new_active = match idx.cmp(&old_active) {
                            std::cmp::Ordering::Less => old_active - 1,
                            std::cmp::Ordering::Equal => old_active.min(count - 1),
                            std::cmp::Ordering::Greater => old_active,
                        };
                        self.active_sheet = new_active.min(count - 1);
                        self.grid.select_only((0, 0));
                        self.reload();
                        self.status = "Deleted sheet".into();
                    }
                }
            }
            TabAction::SetColor(color) => {
                if let Ok(invoke) = Invoke::edit(&sheets.item(&id).color.set(color)) {
                    self.submit(doc, invoke);
                }
                self.reload();
                self.status = if color.is_some() {
                    "Set tab colour".into()
                } else {
                    "Cleared tab colour".into()
                };
            }
        }
        self.pending.redraw = true;
    }

    /// The "fx" formula-insert affordance: focus the active cell for a formula
    /// entry, seeding it with `=` (prepended to any existing content) and
    /// placing the caret at the end so typing continues the formula.
    pub(super) fn start_formula_entry(&mut self) {
        if !self.grid.is_editing() {
            self.grid.begin_edit_in_place();
        }
        if !self.grid.active_display_raw().starts_with('=') {
            self.grid.set_edit_caret(0, false);
            self.grid.insert_text_public("=");
        }
        let len = self.grid.active_display_raw().chars().count();
        self.grid.set_edit_caret(len, false);
        self.status = "Editing formula".into();
        self.pending.redraw = true;
    }

    /// Make every selected cell bold, or regular again when they all are.
    pub(super) fn toggle_bold(&mut self, doc: &mut Edits<'_>) {
        let bold = !self
            .grid
            .selected_cells()
            .iter()
            .all(|addr| self.grid.cell_is_bold(*addr));
        let stored = self.stored_cells();
        let cells: Vec<(Addr, Cell)> = self
            .grid
            .selected_cells()
            .into_iter()
            .map(|addr| {
                let mut cell = stored(addr);
                cell.bold = bold;
                (addr, cell)
            })
            .collect();
        drop(stored);
        self.write_cells(doc, &cells);
        self.reload();
        self.active_cell_bold = self.grid.cell_is_bold(self.grid.active());
        self.pending.redraw = true;
    }

    /// Shared implementation behind the More menu, accessibility control, and
    /// the view effect. Formula visibility is presentation state,
    /// so it intentionally does not create a CRDT/undo entry.
    pub(super) fn toggle_formula_view(&mut self) {
        let on = self.grid.toggle_formula_view();
        self.status = if on {
            "Showing formulas".into()
        } else {
            "Showing results".into()
        };
        self.pending.redraw = true;
    }
}
