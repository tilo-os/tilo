//! What is selected: the declarations presence publishes for it, and the sentence the status bar reads off the selection.

use super::*;

impl SheetsApp {
    /// The edits bound to where the user is — the selection, the active cell
    /// and its sheet — as the workbook's verbs with their arguments bound:
    /// the palette, agents and scripts invoke them through the same door as
    /// the rest of the workbook, and each acts on the selection as it is
    /// when they are published.
    pub(crate) fn selection_declarations(&self) -> Vec<ui::tilo_resource::ActionDecl<Workbook>> {
        let key = SelectionKey {
            revision: self.revision,
            sheet: self.active_sheet,
            bounds: self.grid.selection_bounds(),
            active: self.grid.active(),
            selected: self.grid.selected_cells(),
            currency: self.default_currency(),
        };
        if let Some((built, decls)) = &*self.selection_decls.borrow()
            && *built == key
        {
            return decls.clone();
        }
        let decls = self.build_selection_declarations();
        *self.selection_decls.borrow_mut() = Some((key, decls.clone()));
        decls
    }

    #[allow(
        clippy::too_many_lines,
        reason = "every selection-bound edit is declared in one place"
    )]
    pub(super) fn build_selection_declarations(
        &self,
    ) -> Vec<ui::tilo_resource::ActionDecl<Workbook>> {
        use crate::resource::CellWrite;
        let Some(sheet) = self.active_sheet_id() else {
            return Vec::new();
        };
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let (active_col, active_row) = self.grid.active();
        let selection = crate::resource::range_label((c0, r0), (c1, r1));
        let write = |id: &'static str, label: &str, cells: Vec<CellWrite>| {
            let p = verbs::WriteCells::paths();
            Workbook::WRITE_CELLS
                .bind(p.sheet, sheet.clone())
                .and(p.cells, cells)
                .declaration(id, label)
        };
        let mut decls = Vec::new();
        for (id, label, kind) in [
            (
                "insert-column-chart",
                "Create column chart from selection",
                ChartKind::Column,
            ),
            (
                "insert-line-chart",
                "Create line chart from selection",
                ChartKind::Line,
            ),
        ] {
            let p = verbs::AddChart::paths();
            decls.extend(
                Workbook::ADD_CHART
                    .bind(p.sheet, sheet.clone())
                    .and(p.range, selection.clone())
                    .and(p.kind, kind)
                    .and(p.title, "Selection chart".to_owned())
                    .declaration(id, label),
            );
        }
        let p = verbs::Comment::paths();
        decls.extend(
            Workbook::COMMENT
                .bind(p.sheet, sheet.clone())
                .and(p.range, selection)
                .declaration("comment-selection", "Comment on selection"),
        );
        let target_r = r1.saturating_add(1);
        let sums = (c0..=c1)
            .map(|c| {
                let formula = format!(
                    "=SUM({}:{})",
                    crate::model::addr_to_a1(c, r0),
                    crate::model::addr_to_a1(c, r1)
                );
                CellWrite::at((c, target_r), formula, None)
            })
            .collect();
        decls.extend(write("sum-selection", "Sum selection", sums));
        let p = verbs::InsertRows::paths();
        decls.extend(
            Workbook::INSERT_ROWS
                .bind(p.sheet, sheet.clone())
                .and(p.at, active_row.saturating_add(1))
                .declaration("insert-row-active", "Insert row above"),
        );
        let p = verbs::DeleteRows::paths();
        decls.extend(
            Workbook::DELETE_ROWS
                .bind(p.sheet, sheet.clone())
                .and(p.rows, vec![active_row.saturating_add(1)])
                .declaration("delete-row-active", "Delete row"),
        );
        let letters = crate::model::col_to_letters(active_col);
        let p = verbs::InsertColumns::paths();
        decls.extend(
            Workbook::INSERT_COLUMNS
                .bind(p.sheet, sheet.clone())
                .and(p.at, letters.clone())
                .declaration("insert-column-active", "Insert column left"),
        );
        let p = verbs::DeleteColumns::paths();
        decls.extend(
            Workbook::DELETE_COLUMNS
                .bind(p.sheet, sheet.clone())
                .and(p.columns, vec![letters])
                .declaration("delete-column-active", "Delete column"),
        );

        let selected = self.grid.selected_cells();
        for (id, label, format) in [
            (
                "format-currency",
                "Format as currency",
                CellFormat::Currency(self.default_currency()),
            ),
            ("format-percent", "Format as percent", CellFormat::Percent),
            ("clear-format", "Clear formatting", CellFormat::General),
        ] {
            let cells = selected
                .iter()
                .map(|addr| CellWrite {
                    cell: crate::model::addr_to_a1(addr.0, addr.1),
                    format: Some(format),
                    ..CellWrite::default()
                })
                .collect();
            decls.extend(write(id, label, cells));
        }

        let mut down = Vec::new();
        if r1 > r0 {
            for c in c0..=c1 {
                let raw = self.grid.cell_raw_at((c, r0));
                let fmt = self.grid.cell_fmt_at((c, r0));
                for (offset, r) in (1_i64..).zip(r0.saturating_add(1)..=r1) {
                    down.push(CellWrite::at(
                        (c, r),
                        formula::offset_refs(&raw, 0, offset),
                        Some(fmt),
                    ));
                }
            }
            decls.extend(write("fill-down", "Fill down", down));
        }

        let mut right = Vec::new();
        if c1 > c0 {
            for r in r0..=r1 {
                let raw = self.grid.cell_raw_at((c0, r));
                let fmt = self.grid.cell_fmt_at((c0, r));
                for (offset, c) in (1_i64..).zip(c0.saturating_add(1)..=c1) {
                    right.push(CellWrite::at(
                        (c, r),
                        formula::offset_refs(&raw, offset, 0),
                        Some(fmt),
                    ));
                }
            }
            decls.extend(write("fill-right", "Fill right", right));
        }

        let (c, r) = (active_col, active_row);
        if r > 0 {
            let mut top = r;
            for rr in (0..r).rev() {
                let raw = self.grid.cell_raw_at((c, rr));
                if raw.is_empty() || !(raw.trim().parse::<f64>().is_ok() || raw.starts_with('=')) {
                    break;
                }
                top = rr;
            }
            if top < r {
                let formula = format!(
                    "=SUM({}:{})",
                    crate::model::addr_to_a1(c, top),
                    crate::model::addr_to_a1(c, r - 1)
                );
                decls.extend(write(
                    "autosum-active",
                    "AutoSum column",
                    vec![CellWrite::at((c, r), formula, None)],
                ));
            }
        }
        decls
    }

    /// The workbook as the palette, agents and scripts see it from this
    /// surface: the resource's publication with the edits bound to the
    /// selection and its effects.
    pub(crate) fn published(&self) -> Vec<tilo_actions::Resource> {
        use ui::tilo_resource::{Effect as _, Resource as _};

        let mut workbook =
            ui::tilo_resource::publish::<Workbook>(Workbook::KIND, Workbook::LABEL, &self.workbook);
        workbook.actions.extend(
            self.selection_declarations()
                .iter()
                .map(ui::tilo_resource::declaration_spec),
        );
        workbook.actions.extend(actions::SheetsEffect::specs());
        vec![workbook]
    }

    /// Select `from`..`to` as a drag over the grid would.
    #[cfg(test)]
    pub(crate) fn select_range_for_test(&mut self, from: Addr, to: Addr) {
        self.grid.select_range_public(from, to);
    }

    /// A live summary of the current selection for the status bar: the sum,
    /// average, and count of numeric cells. Returns `None` unless at least two
    /// numbers are selected — a single cell shows its type instead, so the
    /// summary only appears when it is actually informative (matching how
    /// desktop spreadsheets behave).
    pub(super) fn selection_summary(&self) -> Option<SelectionSummary> {
        let cache = self.cache.as_ref()?;
        let mut sum = 0.0;
        let mut count = 0usize;
        for addr in self.grid.selected_cells() {
            if let Some(CellValue::Number(n)) = cache.values.get(&addr) {
                sum += *n;
                count += 1;
            }
        }
        if count < 2 {
            return None;
        }
        // Present the summary in the active cell's format when it is a
        // number/currency/percent, so `$` and `%` read naturally.
        let fmt = match self.format_of_active() {
            f @ (CellFormat::Currency(_) | CellFormat::Number | CellFormat::Percent) => f,
            _ => CellFormat::Number,
        };
        Some(SelectionSummary {
            sum,
            avg: sum / count as f64,
            count,
            fmt,
        })
    }

    /// What the window's bar says about the sheet, or `None` when there is
    /// nothing worth saying (an idle sheet: no pending action, no numeric
    /// selection, a plain `General` active cell).
    ///
    /// A fresh transient action message (e.g. "Pasted into B2") wins; absent
    /// one, the text is the live selection summary (count, sum and average of
    /// the numeric selection) or, absent numbers, the active cell's format.
    /// The idle default `"Ready"` (and the empty string) count as "no action
    /// message" so they never surface on their own.
    pub(super) fn status_label(&self) -> Option<String> {
        // A selected error cell explains itself before anything else.
        let (lo, hi) = self.grid.selection_bounds();
        if lo == hi
            && let Some(why) = self.error_explanation(self.grid.active())
        {
            return Some(why);
        }
        let summary = self.selection_summary().map(|s| {
            format!(
                "{} numbers: sum {}, average {}",
                s.count,
                format_value(&CellValue::Number(s.sum), s.fmt),
                format_value(&CellValue::Number(s.avg), s.fmt),
            )
        });
        let action = {
            let t = self.status.trim();
            if t.is_empty() || t == "Ready" {
                None
            } else {
                Some(t.to_string())
            }
        };
        match (action, summary) {
            (Some(a), _) => Some(a),
            (None, Some(s)) => Some(s),
            // No action and no numeric selection: fall back to the active
            // cell's format, but stay silent on a plain `General` cell so an
            // idle sheet shows no bubble at all.
            (None, None) => {
                if self.current_active_format == CellFormat::General {
                    None
                } else {
                    Some(self.current_active_format.label())
                }
            }
        }
    }
}
