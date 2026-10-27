//! What an edit does to the sheet: reloading the presented view, applying the grid's commands and formats, the clipboard's copy and paste, and the fills that extend a selection.

use super::*;

impl SheetsApp {
    /// Read the active sheet, recalculate, propagate formats onto computed
    /// cells, and push the data into the grid.
    ///
    /// Memoised on `(workbook revision, active sheet)`: selection changes,
    /// scrolling, and hover — which never change the workbook — reuse the cached
    /// result instead of re-snapshotting the document and re-running the whole
    /// formula engine every frame.
    pub(super) fn reload(&mut self) {
        let count = self.sheet_ids.len().max(1);
        let sheet = self.active_sheet.min(count - 1);
        if let Some(c) = &self.cache
            && c.sheet == sheet
            && c.revision == self.revision
        {
            self.active_sheet = sheet;
            return;
        }

        self.active_sheet = sheet;
        let state = Rc::clone(&self.workbook);
        let current = self
            .active_sheet_id()
            .and_then(|id| state.sheet(&id).map(|sheet| (sheet, sheet.layout())));
        let no_sheet = Id::default();
        let (snap, charts, comments) = match &current {
            Some((sheet, layout)) => (
                {
                    // Formulas reading other sheets evaluate with the workbook.
                    let snap = sheet.snapshot_in(layout);
                    match self.active_sheet_id() {
                        Some(id) if crate::formula::Book::needed(&snap.raw) => {
                            state.snapshot_of(&id).unwrap_or(snap)
                        }
                        _ => snap,
                    }
                },
                sheet.chart_snapshots(layout),
                crate::workbook::Sheet::comment_snapshots(
                    self.sheet_ids.get(self.active_sheet).unwrap_or(&no_sheet),
                    &state.comments,
                    layout,
                ),
            ),
            None => Default::default(),
        };
        self.sheet_charts = charts;
        self.sheet_comments = comments;
        // The dock takes its height from the annotations, so the sheet
        // re-lays out whenever they change.
        self.relayout();
        let values = formula::recalculate_snapshot(&snap);

        // Effective per-cell formats: an explicit cell format wins; a
        // formula cell with no explicit format inherits from its operands.
        let mut fmt = snap.fmt.clone();
        for (addr, raw) in &snap.raw {
            if snap.fmt.contains_key(addr) {
                continue; // user set it explicitly
            }
            if let Some(body) = raw.strip_prefix('=')
                && let Some(inferred) = formula::infer_result_format(body, &snap.fmt)
            {
                fmt.insert(*addr, inferred);
            }
        }

        self.cache = Some(ReloadCache {
            revision: self.revision,
            sheet,
            eff_fmt: fmt.clone(),
            values: values.clone(),
        });
        self.grid.set_bold(snap.bold);
        self.grid.set_data(snap.raw, fmt, values);
        self.pending.redraw = true;
    }

    pub(super) fn apply_cmds(&mut self, doc: &mut Edits<'_>, cmds: Vec<GridCmd>) {
        let cur = self.default_currency();
        // Collect the event's cell writes into one batched action, so undo, peer
        // replay, and the Ctrl+K palette all share one path and a burst of edits
        // is a single change. Smart type inference runs here (authoring-side) and
        // the normalized raw + inferred format are baked in, keeping
        // `Action::apply` a pure write.
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
        for cmd in cmds {
            match cmd {
                GridCmd::SetCell { addr, raw } => {
                    if raw.is_empty() {
                        writes.push((addr, String::new(), None));
                    } else {
                        let inferred = infer_entry(&raw, cur);
                        // Adopt the inferred format only when it is non-General;
                        // otherwise leave any user-chosen format the entry fits.
                        let fmt =
                            (inferred.format != CellFormat::General).then_some(inferred.format);
                        writes.push((addr, inferred.raw, fmt));
                    }
                }
                GridCmd::Commit => {
                    // The undo boundary is bracketed by the action host, so the
                    // whole batch is one checkpoint.
                }
            }
        }
        if !writes.is_empty() {
            // A single typed entry that evaluates to an error says why right
            // away: the selection usually moves on with Enter, so the error
            // cell would otherwise sit there unexplained.
            let single = match writes.as_slice() {
                [(addr, ..)] => Some(*addr),
                _ => None,
            };
            self.write_raw(doc, writes);
            self.reload();
            if let Some(addr) = single
                && let Some(why) = self.error_explanation(addr)
            {
                self.status = format!("{}: {why}", crate::model::addr_to_a1(addr.0, addr.1));
            }
        }
    }

    /// Why the cell at `addr` shows an error, in words, or `None` when its
    /// value is not an error.
    pub(super) fn error_explanation(&self, addr: Addr) -> Option<String> {
        let Some(CellValue::Error(err)) = self.grid.cell_value_at(addr) else {
            return None;
        };
        Some(explain_formula_error(&self.grid.cell_raw_at(addr), err))
    }

    pub(super) fn apply_format(&mut self, doc: &mut Edits<'_>, fmt: CellFormat) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let cells: Vec<(Addr, CellFormat)> = self
            .grid
            .selected_cells()
            .into_iter()
            .map(|a| (a, fmt))
            .collect();
        if !cells.is_empty() {
            self.write_formats(doc, cells);
        }
        self.reload();
        self.status = format!("Applied {} to {}", fmt.label(), self.grid.selection_label());
    }

    pub(super) fn clear_format(&mut self, doc: &mut Edits<'_>) {
        let cells: Vec<(Addr, CellFormat)> = self
            .grid
            .selected_cells()
            .into_iter()
            .map(|a| (a, CellFormat::General))
            .collect();
        if !cells.is_empty() {
            self.write_formats(doc, cells);
        }
        self.reload();
        self.status = format!("Cleared format on {}", self.grid.selection_label());
    }

    /// Copy (or cut) the current selection. The rich rectangle (raw + format)
    /// is retained for a lossless in-app paste, and a tab/newline-separated
    /// grid of the *displayed* values is pushed to the system clipboard so it
    /// pastes cleanly into other apps. A cut clears its source cells when the
    /// paste lands (spreadsheet "move" semantics), so nothing is lost if the
    /// user changes their mind.
    pub(super) fn copy_selection(&mut self, cut: bool) {
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let mut cells = Vec::with_capacity(span(r0, r1) as usize);
        let mut tsv_rows = Vec::with_capacity(span(r0, r1) as usize);
        for r in r0..=r1 {
            let mut row_cells = Vec::with_capacity(span(c0, c1) as usize);
            let mut tsv_cols = Vec::with_capacity(span(c0, c1) as usize);
            for c in c0..=c1 {
                let addr = (c, r);
                let raw = self.grid.cell_raw_at(addr);
                let fmt = self.grid.cell_fmt_at(addr);
                let display = self
                    .grid
                    .cell_value_at(addr)
                    .map(|v| format_value(&v, fmt))
                    .unwrap_or_default();
                row_cells.push((raw, fmt));
                tsv_cols.push(tsv_cell(&display));
            }
            cells.push(row_cells);
            tsv_rows.push(tsv_cols.join("\t"));
        }
        let tsv = tsv_rows.join("\n");
        ui::clipboard_copy(&tsv);
        self.clipboard = Some(ClipboardGrid {
            src: (c0, r0),
            cells,
            cut,
            tsv,
        });
        let verb = if cut { "Cut" } else { "Copied" };
        self.status = if self.grid.is_multi_range() {
            format!("{verb} bounding range {}", crate::model::addr_to_a1(c0, r0))
        } else {
            format!("{verb} {}", self.grid.selection_label())
        };
        self.pending.redraw = true;
    }

    /// Paste at the selection's top-left. When the system clipboard still holds
    /// exactly what we last copied, the rich rectangle is used — raw inputs and
    /// formats are restored and, for a *copy*, relative formula references shift
    /// to the new location (a *cut* moves the block verbatim, references
    /// following it). When the paste target is a selection that is a whole
    /// multiple of the copied block, the block **tiles** to fill it, each tile
    /// offset by its own position — matching Excel/Sheets. Otherwise the
    /// plain-text clipboard is parsed as a TSV grid and each field is
    /// smart-typed, so a paste from any other app works too.
    pub(super) fn paste(&mut self, doc: &mut Edits<'_>) {
        let (target, sel_br) = self.grid.selection_bounds();
        let system = ui::clipboard_paste();
        let use_internal = match (&self.clipboard, &system) {
            (Some(cb), Some(text)) => *text == cb.tsv,
            // No readable system clipboard (e.g. headless) → trust our own copy.
            (Some(_), None) => true,
            _ => false,
        };

        // `take` so a cut is one-shot; a plain copy is re-stored below.
        let internal = if use_internal {
            self.clipboard.take()
        } else {
            None
        };

        if let Some(cb) = internal {
            // The block is addressed in `u32` grid coordinates: one that cannot
            // be addressed is not pastable, and neither is an empty one.
            let (Ok(rows), Ok(cols)) = (
                u32::try_from(cb.cells.len()),
                u32::try_from(cb.cells.first().map_or(0, Vec::len)),
            ) else {
                self.clipboard = Some(cb);
                return;
            };
            if rows == 0 || cols == 0 {
                self.clipboard = Some(cb);
                return;
            }
            let dc = delta(cb.src.0, target.0);
            let dr = delta(cb.src.1, target.1);
            // Tile the block to fill the target selection when it is a whole
            // multiple of the block in both axes (Excel's "paste into a range").
            // A cut never tiles — a move places its block exactly once.
            let (tile_cols, tile_rows) = if cb.cut {
                (1u32, 1u32)
            } else {
                (
                    tile_count(span(target.0, sel_br.0), cols),
                    tile_count(span(target.1, sel_br.1), rows),
                )
            };
            // Collect the whole paste into one batched change. A cut moves the
            // block: clear the source first (pushed ahead of the writes so an
            // overlapping target still lands last), then write.
            let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
            if cb.cut {
                for r in 0..rows {
                    for c in 0..cols {
                        let addr = offset_addr(cb.src, c, r);
                        writes.push((addr, String::new(), Some(CellFormat::General)));
                    }
                }
            }
            let mut last = target;
            for tr in 0..tile_rows {
                for tc in 0..tile_cols {
                    // Where this tile's top-left sits relative to the block's
                    // own origin, in columns/rows and as a reference shift.
                    let tile_col_off = tc.saturating_mul(cols);
                    let tile_row_off = tr.saturating_mul(rows);
                    for (ri, row) in (0u32..).zip(&cb.cells) {
                        for (ci, (raw, fmt)) in (0u32..).zip(row) {
                            let addr = offset_addr(
                                target,
                                tile_col_off.saturating_add(ci),
                                tile_row_off.saturating_add(ri),
                            );
                            let out_raw = if cb.cut {
                                raw.clone()
                            } else {
                                formula::offset_refs(
                                    raw,
                                    dc.saturating_add(i64::from(tile_col_off)),
                                    dr.saturating_add(i64::from(tile_row_off)),
                                )
                            };
                            writes.push((addr, out_raw, Some(*fmt)));
                            last = addr;
                        }
                    }
                }
            }
            self.write_raw(doc, writes);
            if !cb.cut {
                self.clipboard = Some(cb);
            }
            self.grid.select_range_public(target, last);
        } else if let Some(text) = system {
            if !self.paste_tsv(doc, target, &text) {
                self.status = "Clipboard is empty".into();
                return;
            }
        } else {
            self.status = "Clipboard is empty".into();
            return;
        }

        self.reload();
        self.status = format!("Pasted into {}", self.grid.selection_label());
    }

    /// Parse `text` as a tab/newline-separated grid pasted from another app and
    /// write it starting at `target`, smart-typing each non-empty field.
    /// Selects the pasted rectangle. Returns `false` (writing nothing) when the
    /// payload has no content. This is the system-clipboard branch of `paste`.
    pub(super) fn paste_tsv(&mut self, doc: &mut Edits<'_>, target: Addr, text: &str) -> bool {
        let cur = self.default_currency();
        let normalized = text.replace('\r', "");
        let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
        if body.is_empty() {
            return false;
        }
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
        let mut last = target;
        for (ri, line) in (0u32..).zip(body.split('\n')) {
            for (ci, field) in (0u32..).zip(line.split('\t')) {
                let addr = offset_addr(target, ci, ri);
                if field.is_empty() {
                    writes.push((addr, String::new(), Some(CellFormat::General)));
                } else {
                    let inferred = infer_entry(field, cur);
                    writes.push((addr, inferred.raw, Some(inferred.format)));
                }
                last = addr;
            }
        }
        self.write_raw(doc, writes);
        self.grid.select_range_public(target, last);
        true
    }

    /// `AutoSum`: scan upward from the active cell for a run of numeric cells
    /// and write `=SUM(top:bottom)` into the active cell.
    ///
    /// Error cells directly above are stepped over (summing them would only
    /// propagate the error), and the run stops at a blank, text, an error or
    /// an existing total (a `SUM` formula), so a column's subtotal is never
    /// counted twice.
    pub(super) fn auto_sum(&mut self, doc: &mut Edits<'_>) {
        let (c, r) = self.grid.active();
        if r == 0 {
            self.status = "Nothing above to sum".into();
            return;
        }
        let is_error =
            |grid: &Grid, rr: u32| matches!(grid.cell_value_at((c, rr)), Some(CellValue::Error(_)));
        let mut bottom = r;
        while bottom > 0 && is_error(&self.grid, bottom - 1) {
            bottom -= 1;
        }
        let mut top = bottom;
        for rr in (0..bottom).rev() {
            let raw = self.grid.cell_raw_at((c, rr));
            let is_formula = raw.starts_with('=');
            let is_total = is_formula && raw.to_ascii_uppercase().contains("SUM(");
            let is_num = raw.trim().parse::<f64>().is_ok() || is_formula;
            if raw.is_empty() || !is_num || is_total || is_error(&self.grid, rr) {
                break;
            }
            top = rr;
        }
        if top == bottom {
            self.status = "No numeric run above".into();
            return;
        }
        // `top < bottom` here, so the run ends on `bottom - 1`.
        let formula = format!(
            "=SUM({}:{})",
            crate::model::addr_to_a1(c, top),
            crate::model::addr_to_a1(c, bottom.saturating_sub(1))
        );
        self.write_raw(doc, vec![((c, r), formula.clone(), None)]);
        self.reload();
        self.status = format!("Inserted {formula}");
    }

    /// Fill the top row of the selection downward through every row in the
    /// selection's bounding box — Excel's Ctrl+D. Each source cell's raw input
    /// and format copy down, with relative formula references shifted per row.
    pub(super) fn fill_down(&mut self, doc: &mut Edits<'_>) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        if r1 <= r0 {
            self.status = "Select two or more rows to fill down".into();
            return;
        }
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
        for c in c0..=c1 {
            let src_raw = self.grid.cell_raw_at((c, r0));
            let src_fmt = self.grid.cell_fmt_at((c, r0));
            // `offset` counts rows below the source, so it is exactly `r - r0`.
            for (offset, r) in (1i64..).zip(r0.saturating_add(1)..=r1) {
                let out_raw = formula::offset_refs(&src_raw, 0, offset);
                writes.push(((c, r), out_raw, Some(src_fmt)));
            }
        }
        self.write_raw(doc, writes);
        self.reload();
        self.status = format!("Filled down {}", self.grid.selection_label());
    }

    /// Fill the left column of the selection rightward — Excel's Ctrl+R.
    pub(super) fn fill_right(&mut self, doc: &mut Edits<'_>) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        if c1 <= c0 {
            self.status = "Select two or more columns to fill right".into();
            return;
        }
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
        for r in r0..=r1 {
            let src_raw = self.grid.cell_raw_at((c0, r));
            let src_fmt = self.grid.cell_fmt_at((c0, r));
            // `offset` counts columns right of the source, i.e. `c - c0`.
            for (offset, c) in (1i64..).zip(c0.saturating_add(1)..=c1) {
                let out_raw = formula::offset_refs(&src_raw, offset, 0);
                writes.push(((c, r), out_raw, Some(src_fmt)));
            }
        }
        self.write_raw(doc, writes);
        self.reload();
        self.status = format!("Filled right {}", self.grid.selection_label());
    }

    /// Snapshot a cell for the fill engine: its raw input, computed value, and
    /// effective display format.
    pub(super) fn fill_cell_at(&self, addr: Addr) -> fill::FillCell {
        fill::FillCell {
            raw: self.grid.cell_raw_at(addr),
            value: self.grid.cell_value_at(addr).unwrap_or(CellValue::Empty),
            fmt: self.grid.cell_fmt_at(addr),
        }
    }

    /// Commit a fill-handle drag the grid recorded: the selected block is the
    /// *pattern*, and the dragged-out region is filled by extending it (per
    /// column when filling down, per row when filling right) through the smart
    /// [`crate::fill`] engine — formulas offset their relative refs, numeric
    /// pairs extrapolate a series, dates/names continue, text+number
    /// increments. One batched action, one undo boundary.
    pub(super) fn run_pending_fill(&mut self, doc: &mut Edits<'_>) {
        let Some((src_tl, src_br, dst_tl, dst_br)) = self.grid.take_pending_fill() else {
            return;
        };
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
        // Vertical when the destination starts below the source; else horizontal.
        let vertical = dst_tl.1 > src_br.1;
        if vertical {
            let count = span(dst_tl.1, dst_br.1) as usize;
            for c in src_tl.0..=src_br.0 {
                let pattern: Vec<fill::FillCell> = (src_tl.1..=src_br.1)
                    .map(|r| self.fill_cell_at((c, r)))
                    .collect();
                for (i, out) in (0u32..).zip(fill::extend_series(&pattern, count, 0, 1)) {
                    writes.push(((c, dst_tl.1.saturating_add(i)), out.raw, Some(out.fmt)));
                }
            }
        } else {
            let count = span(dst_tl.0, dst_br.0) as usize;
            for r in src_tl.1..=src_br.1 {
                let pattern: Vec<fill::FillCell> = (src_tl.0..=src_br.0)
                    .map(|c| self.fill_cell_at((c, r)))
                    .collect();
                for (i, out) in (0u32..).zip(fill::extend_series(&pattern, count, 1, 0)) {
                    writes.push(((dst_tl.0.saturating_add(i), r), out.raw, Some(out.fmt)));
                }
            }
        }
        if writes.is_empty() {
            return;
        }
        self.write_raw(doc, writes);
        self.reload();
        self.grid.select_range_public(src_tl, dst_br);
        self.status = format!("Filled {}", self.grid.selection_label());
        self.pending.redraw = true;
    }
}
