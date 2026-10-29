//! The commands the bar, the menus and the palette run: sheets and tabs, document settings, the command palette, row and column verbs, navigation, and the effects an answer comes back as.

use super::{
    CellFormat, Edits, Id, Invoke, MenuItem, PendingReveal, SettingKind,
    SettingsPanel, SheetsApp, Workbook, a1_to_addr, actions, date_label, time_label, verbs,
};

impl SheetsApp {
    pub(super) fn switch_sheet(&mut self, doc: &mut Edits<'_>, idx: usize) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        self.active_sheet = idx.min(self.sheet_ids.len().saturating_sub(1));
        self.grid.select_only((0, 0));
        self.reload();
        self.status = format!(
            "Sheet: {}",
            self.sheet_names.get(idx).cloned().unwrap_or_default()
        );
    }

    pub(super) fn add_sheet(&mut self, doc: &mut Edits<'_>) {
        let n = self.sheet_ids.len();
        self.submit(
            doc,
            Workbook::NEW_SHEET.with(verbs::NewSheet {
                new: crate::resource::SheetSpec {
                    after: None,
                    ..crate::resource::SheetSpec::default()
                },
            }),
        );
        // The active sheet tab is presentation, not a document change. The
        // appended sheet is at `n`.
        self.active_sheet = n;
        self.grid.select_only((0, 0));
        self.reload();
        self.status = "Added sheet".into();
    }

    pub(super) fn apply_setting(&mut self, doc: &mut Edits<'_>, kind: SettingKind) {
        match kind {
            SettingKind::Currency(c) => self.doc_settings.currency = c,
            SettingKind::Date(d) => self.doc_settings.date_format = d,
            SettingKind::Time(t) => self.doc_settings.time_format = t,
        }
        // The document's overrides are the workbook's, so collaborators (and
        // the next session) share the document's conventions.
        let settings = Workbook::paths().settings;
        let edit = match kind {
            SettingKind::Currency(c) => settings.currency.set(c),
            SettingKind::Date(d) => settings.date_format.set(d),
            SettingKind::Time(t) => settings.time_format.set(t),
        };
        if let Ok(invoke) = Invoke::edit(&edit) {
            self.submit(doc, invoke);
        }
        self.reload();
    }

    /// Open the host's search on the workbook's actions, as Ctrl+K does,
    /// over any other overlay.
    pub(super) fn open_command_palette(&mut self) {
        self.settings_panel = SettingsPanel::None;
        self.tab_menu_target = None;
        self.name_box_edit = None;
        self.tab_rename = None;
        self.search.open(ui::search::SearchRequest::this_app());
        self.pending.redraw = true;
    }

    /// Run the action the palette emitted, dispatching by id. `argument`
    /// carries the free-text a `with_argument` entry collected (e.g. the cell
    /// reference for "Go to cell").
    #[cfg(test)]
    pub(super) fn dispatch_command(
        &mut self,
        doc: &mut Edits<'_>,
        id: &str,
        argument: Option<&str>,
    ) {
        match id {
            "goto" => self.go_to_cell(argument.unwrap_or("")),
            "sum_selection" => self.sum_selection(doc),
            "autosum" => self.auto_sum(doc),
            "fill_down" => self.fill_down(doc),
            "fill_right" => self.fill_right(doc),
            "insert_row" => self.insert_row(doc),
            "delete_row" => self.delete_row(doc),
            "insert_col" => self.insert_column(doc),
            "delete_col" => self.delete_column(doc),
            "formula_view" => {
                let on = self.grid.toggle_formula_view();
                self.status = if on {
                    "Showing formulas".into()
                } else {
                    "Showing results".into()
                };
            }
            "format_currency" => {
                let cur = self.default_currency();
                self.apply_format(doc, CellFormat::Currency(cur));
            }
            "format_percent" => self.apply_format(doc, CellFormat::Percent),
            "clear_format" => self.clear_format(doc),
            "new_sheet" => self.add_sheet(doc),
            _ => {}
        }
        self.pending.redraw = true;
    }

    /// "Go to cell": parse an A1 reference (or `A1:C3` range top-left) and jump
    /// the selection there, scrolling it into view.
    pub(super) fn go_to_cell(&mut self, arg: &str) {
        let token = arg.trim().split([':', ' ']).next().unwrap_or("").trim();
        if let Some(addr) = a1_to_addr(token) {
            self.grid.select_only(addr);
            self.status = format!("Jumped to {}", crate::model::addr_to_a1(addr.0, addr.1));
        } else {
            self.status = format!("'{}' is not a valid cell reference", arg.trim());
        }
    }

    /// "Sum selection": write a `=SUM(col_top:col_bottom)` total for each column
    /// of the selection into the row directly below it, then select that row.
    #[cfg(test)]
    pub(super) fn sum_selection(&mut self, doc: &mut Edits<'_>) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let target_r = r1.saturating_add(1);
        let mut writes: Vec<(crate::model::Addr, String, Option<CellFormat>)> = Vec::new();
        for c in c0..=c1 {
            let formula = format!(
                "=SUM({}:{})",
                crate::model::addr_to_a1(c, r0),
                crate::model::addr_to_a1(c, r1),
            );
            writes.push(((c, target_r), formula, None));
        }
        self.write_raw(doc, writes);
        self.reload();
        self.grid
            .select_range_public((c0, target_r), (c1, target_r));
        self.status = format!("Summed selection into row {}", target_r.saturating_add(1));
    }

    #[cfg(test)]
    pub(super) fn insert_row(&mut self, doc: &mut Edits<'_>) {
        let r0 = self.grid.active().1;
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        self.submit(
            doc,
            Workbook::INSERT_ROWS.with(verbs::InsertRows {
                sheet,
                at: r0.saturating_add(1),
                count: None,
            }),
        );
        self.reload();
        self.grid.rows_inserted(r0, 1);
        self.status = format!("Inserted row {}", r0.saturating_add(1));
    }

    #[cfg(test)]
    pub(super) fn delete_row(&mut self, doc: &mut Edits<'_>) {
        let r0 = self.grid.active().1;
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        self.submit(
            doc,
            Workbook::DELETE_ROWS.with(verbs::DeleteRows {
                sheet,
                rows: vec![r0.saturating_add(1)],
            }),
        );
        self.reload();
        self.grid.rows_removed(r0, 1);
        self.status = format!("Deleted row {}", r0.saturating_add(1));
    }

    #[cfg(test)]
    pub(super) fn insert_column(&mut self, doc: &mut Edits<'_>) {
        let c0 = self.grid.active().0;
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        self.submit(
            doc,
            Workbook::INSERT_COLUMNS.with(verbs::InsertColumns {
                sheet,
                at: crate::model::col_to_letters(c0),
                count: None,
            }),
        );
        self.reload();
        self.grid.columns_inserted(c0, 1);
        self.status = format!(
            "Inserted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    #[cfg(test)]
    pub(super) fn delete_column(&mut self, doc: &mut Edits<'_>) {
        let c0 = self.grid.active().0;
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        self.submit(
            doc,
            Workbook::DELETE_COLUMNS.with(verbs::DeleteColumns {
                sheet,
                columns: vec![crate::model::col_to_letters(c0)],
            }),
        );
        self.reload();
        self.grid.columns_removed(c0, 1);
        self.status = format!(
            "Deleted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    /// The File menu's rows, in order.
    pub(super) fn file_menu_items() -> [(&'static str, MenuItem); 8] {
        [
            ("New sheet", MenuItem::NewSheet),
            ("Import CSV", MenuItem::ImportCsv),
            ("Import Excel workbook", MenuItem::ImportXlsx),
            ("Import OpenDocument spreadsheet", MenuItem::ImportOds),
            ("Import Notion export", MenuItem::ImportNotion),
            ("Export CSV", MenuItem::ExportCsv),
            ("Export Excel workbook", MenuItem::ExportXlsx),
            ("Export OpenDocument spreadsheet", MenuItem::ExportOds),
        ]
    }

    /// The Format menu's rows, in order, bound to the workbook's locale.
    pub(super) fn format_menu_items(&self) -> [(&'static str, MenuItem); 9] {
        let cur = self.default_currency();
        let locale = self.locale();
        [
            ("General", MenuItem::SetFormat(CellFormat::General)),
            ("Number", MenuItem::SetFormat(CellFormat::Number)),
            ("Currency", MenuItem::SetFormat(CellFormat::Currency(cur))),
            ("Percent", MenuItem::SetFormat(CellFormat::Percent)),
            (
                "Date",
                MenuItem::SetFormat(CellFormat::Date(locale.date_format)),
            ),
            (
                "Time",
                MenuItem::SetFormat(CellFormat::Time(locale.time_format)),
            ),
            ("Boolean", MenuItem::SetFormat(CellFormat::Boolean)),
            ("Text", MenuItem::SetFormat(CellFormat::Text)),
            ("Clear format", MenuItem::ClearFormat),
        ]
    }

    pub(super) fn chip_label_and_state(&self, kind: SettingKind) -> (String, bool) {
        match kind {
            SettingKind::Currency(None) => ("Default".into(), self.doc_settings.currency.is_none()),
            SettingKind::Currency(Some(c)) => {
                (c.code().into(), self.doc_settings.currency == Some(c))
            }
            SettingKind::Date(None) => ("Default".into(), self.doc_settings.date_format.is_none()),
            SettingKind::Date(Some(d)) => (
                date_label(d).into(),
                self.doc_settings.date_format == Some(d),
            ),
            SettingKind::Time(None) => ("Default".into(), self.doc_settings.time_format.is_none()),
            SettingKind::Time(Some(t)) => (
                time_label(t).into(),
                self.doc_settings.time_format == Some(t),
            ),
        }
    }

    /// The active cell's effective format, read from the memoised reload cache
    /// (the propagation of formula units is already baked into `eff_fmt`).
    pub(super) fn format_of_active(&self) -> CellFormat {
        let active = self.grid.active();
        self.cache
            .as_ref()
            .and_then(|c| c.eff_fmt.get(&active).copied())
            .unwrap_or_default()
    }

    /// Invoke one of the workbook's effects through its door, as the palette
    /// or an agent does.
    pub(super) fn invoke_effect(&self, effect: &actions::SheetsEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
    }

    /// Do what an effect asked of the grid: show a cell, the formulas or a
    /// sheet, open the commands, say what happened.
    pub(super) fn take_ask(&mut self, ask: super::store::Ask) {
        use super::store::Ask;
        self.pending.redraw = true;
        match ask {
            Ask::Show(effect) => {
                if let Err(error) = self.show(effect) {
                    self.status = error;
                }
            }
            Ask::Commands => self.open_command_palette(),
            Ask::Status(status) => self.status = status,
            Ask::Reveal(reveal) => self.pending_reveal = Some(reveal),
            Ask::Stage(scenario) => self.stage_scenario(scenario.as_deref()),
        }
    }

    /// Change what is shown, as a view effect asks.
    pub(super) fn show(&mut self, effect: actions::SheetsEffect) -> Result<(), String> {
        use actions::SheetsEffect as E;
        match effect {
            E::GoToCell { reference } => self.go_to_cell(&reference),
            E::ToggleFormulaView => self.toggle_formula_view(),
            E::Open { sheet, col, row } => {
                let sheet = Id::from_raw(sheet);
                if !self.sheet_ids.contains(&sheet) {
                    return Err("the sheet no longer exists".into());
                }
                self.pending_reveal = Some(PendingReveal {
                    sheet_id: Some(sheet),
                    addr: (col, row),
                    status: None,
                    arriving: false,
                });
            }
            other => {
                return Err(format!(
                    "`{}` is not a view",
                    ui::tilo_resource::Effect::id(&other)
                ));
            }
        }
        Ok(())
    }
}
