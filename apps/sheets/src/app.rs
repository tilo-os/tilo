//! Root widget for the Sheets app. Owns the [`Grid`] and the surrounding
//! chrome — a compact menu bar (File / Format menus + AutoSum), a
//! prominent type/format indicator, a name-box + formula bar, and a bottom
//! sheet-tab strip + status line — all hand-drawn into the scene.
//!
//! Data flow: `sync` seeds the workbook into the CRDT on first run, reads
//! the active sheet's raw inputs, recalculates every formula via the
//! [`crate::formula`] engine, propagates formats onto computed cells, and
//! hands the resulting values to the grid. `handle_event` routes
//! pointer/keyboard input to whichever region was hit and applies the
//! grid's [`GridCmd`]s — running smart type inference on each entry —
//! back into the document.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use automerge::{AutoCommit, ChangeHash, ObjId};
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
use crate::infer::infer_entry;
use crate::model::{
    a1_to_addr, format_value, Addr, CellFormat, CellValue, Currency, DateStyle, FormatCategory,
    TimeStyle,
};
use crate::settings::{DocSettings, EffectiveLocale, GlobalSettings};

const MENUBAR_H: f64 = 44.0;
const FORMULA_H: f64 = 34.0;
const TABS_H: f64 = 34.0;
const NAME_BOX_W: f64 = 92.0;

/// A top-level menu-bar action.
#[derive(Clone, Debug, PartialEq)]
enum MenuItem {
    // File menu.
    NewSheet,
    ImportCsv,
    ExportCsv,
    ImportXlsx,
    ExportXlsx,
    ImportOds,
    ExportOds,
    // Format menu.
    SetFormat(CellFormat),
    ClearFormat,
    // Settings.
    OpenDocSettings,
    // Inline bar action.
    AutoSum,
}

/// Which dropdown (if any) is open.
#[derive(Clone, Copy, PartialEq)]
enum OpenMenu {
    None,
    File,
    Format,
}

/// Whether the per-document settings panel is open.
#[derive(Clone, Copy, PartialEq)]
enum SettingsPanel {
    None,
    Document,
}

struct MenuButton {
    rect: Rect,
    label: String,
    /// `None` for top-level menu openers (handled specially).
    item: Option<MenuItem>,
    opens: Option<OpenMenu>,
}

/// A clickable row inside an open dropdown or settings panel.
struct ClickRow {
    rect: Rect,
    item: MenuItem,
}

/// A clickable option chip inside a settings panel.
struct SettingChip {
    rect: Rect,
    kind: SettingKind,
}

#[derive(Clone, Copy)]
enum SettingKind {
    DocCurrency(Option<Currency>),
    DocDate(Option<DateStyle>),
    DocTime(Option<TimeStyle>),
}

pub struct SheetsApp {
    grid: Grid,
    rect: Rect,
    menubar_rect: Rect,
    formula_rect: Rect,
    grid_rect: Rect,
    tabs_rect: Rect,
    menu_buttons: Vec<MenuButton>,
    dropdown_rows: Vec<ClickRow>,
    dropdown_rect: Rect,
    setting_chips: Vec<SettingChip>,
    panel_rect: Rect,
    /// Y of each settings section's first chip row (Currency, Date, Time), so
    /// the section labels stay aligned even when a row wraps on narrow widths.
    settings_section_y: [f64; 3],
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
    open_menu: OpenMenu,
    settings_panel: SettingsPanel,
    global: GlobalSettings,
    doc_settings: DocSettings,
    /// Whether the workbook is persisted to disk between sessions. Off for the
    /// headless `--shots`/test path so screenshots never touch the user's data.
    persist: bool,
    /// Whether an empty workbook is seeded with the sample budget (only the
    /// headless `--shots`/test path does; a real launch opens a blank sheet).
    seed_sample_data: bool,
    /// Document heads last written to disk — persistence is skipped while the
    /// document is unchanged, so idle frames never re-serialize.
    saved_heads: Vec<ChangeHash>,
    /// Memoised recalculation: `reload` recomputes only when the document
    /// heads or active sheet change, so selection/scroll frames are cheap.
    cache: Option<ReloadCache>,
    /// The last copied/cut rectangle, kept so an in-app paste can restore raw
    /// inputs and formats exactly (and shift relative formula references),
    /// beyond the plain-text payload other apps see on the system clipboard.
    clipboard: Option<ClipboardGrid>,
}

/// A rectangle captured by copy/cut, retaining each cell's raw input and
/// format so an in-app paste is lossless. `tsv` is the exact plain-text
/// payload pushed to the system clipboard; a later paste compares the live
/// clipboard against it to decide whether to reuse this rich copy or parse
/// whatever another app has since placed there.
struct ClipboardGrid {
    /// Top-left source address of the copied rectangle.
    src: Addr,
    /// Row-major `(raw, format)` for every cell in the rectangle.
    cells: Vec<Vec<(String, CellFormat)>>,
    /// A cut moves cells verbatim (source cleared on paste, refs unshifted);
    /// a copy shifts relative references to the paste target.
    cut: bool,
    /// The tab/newline-separated display text written to the system clipboard.
    tsv: String,
}

/// The recomputed view of the active sheet, kept between frames so that
/// selection changes, scrolling, and format lookups don't re-snapshot the
/// CRDT or re-run the formula engine.
struct ReloadCache {
    heads: Vec<ChangeHash>,
    sheet: usize,
    /// Effective per-cell format (explicit format, else propagated formula
    /// unit). Drives the type indicator and the selection summary.
    eff_fmt: HashMap<Addr, CellFormat>,
    /// Computed cell values, reused for the live selection summary.
    values: HashMap<Addr, CellValue>,
}

impl SheetsApp {
    pub fn new() -> Self {
        Self {
            grid: Grid::new(),
            rect: Rect::ZERO,
            menubar_rect: Rect::ZERO,
            formula_rect: Rect::ZERO,
            grid_rect: Rect::ZERO,
            tabs_rect: Rect::ZERO,
            menu_buttons: Vec::new(),
            dropdown_rows: Vec::new(),
            dropdown_rect: Rect::ZERO,
            setting_chips: Vec::new(),
            panel_rect: Rect::ZERO,
            settings_section_y: [0.0; 3],
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
            open_menu: OpenMenu::None,
            settings_panel: SettingsPanel::None,
            global: GlobalSettings::default(),
            doc_settings: DocSettings::default(),
            persist: true,
            seed_sample_data: false,
            saved_heads: Vec::new(),
            cache: None,
            clipboard: None,
        }
    }

    /// Configure the app for the headless `--shots`/test path: seed the sample
    /// workbook so a captured frame shows real formulas, and disable on-disk
    /// persistence so screenshots never read or write the user's workbook.
    pub fn shots_mode(&mut self) {
        self.persist = false;
        self.seed_sample_data = true;
    }

    /// Shots hook: open the File menu so its actions appear in the frame.
    pub fn shots_open_file_menu(&mut self) {
        self.open_menu = OpenMenu::File;
    }

    /// Shots hook: open the Format menu.
    pub fn shots_open_format_menu(&mut self) {
        self.open_menu = OpenMenu::Format;
    }

    /// Shots hook: open the per-document settings panel.
    pub fn shots_open_doc_settings(&mut self) {
        self.settings_panel = SettingsPanel::Document;
    }

    /// Shots hook: begin editing the active cell so the in-cell editor and the
    /// active formula bar are captured.
    pub fn shots_begin_edit(&mut self) {
        self.grid.begin_edit_in_place();
    }

    /// Shots hook: select a multi-cell numeric range so the live status-bar
    /// summary (Sum / Avg / Count) is captured.
    pub fn shots_select_summary_range(&mut self) {
        // Budget!B2:B6 — the January column body.
        self.grid.select_range_public((1, 1), (1, 5));
    }

    /// The document's effective locale (per-doc overrides over global).
    fn locale(&self) -> EffectiveLocale {
        EffectiveLocale::resolve(&self.global.locale, &self.doc_settings)
    }

    fn default_currency(&self) -> Currency {
        self.locale().currency
    }

    // --- document plumbing --------------------------------------------------

    /// One-time startup: adopt any workbook persisted from a previous session,
    /// otherwise open a blank sheet (a real launch never fabricates sample
    /// data; only the headless shots/test path seeds the sample budget).
    fn initialize(&mut self, doc: &mut AutoCommit) {
        if self.persist {
            if let Some(path) = workbook_path() {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(mut saved) = AutoCommit::load(&bytes) {
                        let _ = doc.merge(&mut saved);
                    }
                }
            }
        }
        if SheetsDoc::is_empty(doc) {
            if self.seed_sample_data {
                seed_workbook(doc);
                SheetsDoc::set_active_index(doc, 0);
                // Preselect a smart-typed currency cell so the formula bar and
                // the type indicator are populated in the captured frame.
                if let Some(addr) = a1_to_addr("E2") {
                    self.grid.select_only(addr);
                }
            } else {
                SheetsDoc::add_sheet(doc, "Sheet1");
                SheetsDoc::set_active_index(doc, 0);
                self.grid.select_only((0, 0));
            }
        } else {
            // Restored workbook: clamp the persisted active index and land on A1.
            let active = SheetsDoc::active_index(doc);
            SheetsDoc::set_active_index(doc, active);
            self.grid.select_only((0, 0));
        }
        // Treat the freshly-seeded/restored state as "already on disk" so an
        // untouched workbook is never rewritten on idle frames.
        self.saved_heads = doc.get_heads();
    }

    /// Persist the workbook to the per-user data directory whenever it has
    /// changed since the last write. A no-op on idle/selection/scroll frames
    /// (the heads are unchanged) and in the shots/test path.
    fn persist_if_dirty(&mut self, doc: &mut AutoCommit) {
        if !self.persist {
            return;
        }
        let heads = doc.get_heads();
        if heads == self.saved_heads {
            return;
        }
        if let Some(path) = workbook_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let bytes = doc.save();
            if std::fs::write(&path, bytes).is_ok() {
                self.saved_heads = heads;
                return;
            }
        }
        // No writable data dir — record the heads anyway so we don't retry
        // (and thrash) on every subsequent frame.
        self.saved_heads = heads;
    }

    /// Read the active sheet, recalculate, propagate formats onto computed
    /// cells, and push the data into the grid.
    ///
    /// Memoised on `(document heads, active sheet)`: selection changes,
    /// scrolling, and hover — which never mutate the CRDT — reuse the cached
    /// result instead of re-snapshotting the document and re-running the whole
    /// formula engine every frame.
    fn reload(&mut self, doc: &mut AutoCommit) {
        let heads = doc.get_heads();
        let sheet = SheetsDoc::active_index(doc);
        if let Some(c) = &self.cache {
            if c.sheet == sheet && c.heads == heads {
                self.active_sheet = sheet;
                return;
            }
        }

        self.active_sheet = sheet;
        self.sheet_names = SheetsDoc::sheet_names(doc);
        self.doc_settings = SheetsDoc::doc_settings(doc);
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet, cur);
        let values = formula::recalculate(&snap.raw);

        // Effective per-cell formats: an explicit cell format wins; a
        // formula cell with no explicit format inherits from its operands.
        let mut fmt = snap.fmt.clone();
        for (addr, raw) in &snap.raw {
            if snap.fmt.contains_key(addr) {
                continue; // user set it explicitly
            }
            if let Some(body) = raw.strip_prefix('=') {
                if let Some(inferred) = formula::infer_result_format(body, &snap.fmt) {
                    fmt.insert(*addr, inferred);
                }
            }
        }

        self.cache = Some(ReloadCache {
            heads,
            sheet,
            eff_fmt: fmt.clone(),
            values: values.clone(),
        });
        self.grid.set_data(snap.raw, fmt, values);
        self.dirty = true;
    }

    fn apply_cmds(&mut self, doc: &mut AutoCommit, cmds: Vec<GridCmd>) {
        let mut mutated = false;
        let cur = self.default_currency();
        for cmd in cmds {
            match cmd {
                GridCmd::SetCell { addr, raw } => {
                    if raw.is_empty() {
                        SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, "");
                    } else {
                        // Smart type inference: normalize the raw and adopt
                        // the inferred format — unless the cell already has
                        // a user-chosen format that the new entry fits.
                        let inferred = infer_entry(&raw, cur);
                        SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, &inferred.raw);
                        if inferred.format != CellFormat::General {
                            SheetsDoc::set_cell_format(
                                doc,
                                self.active_sheet,
                                addr,
                                inferred.format,
                            );
                        }
                    }
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
        self.status = format!(
            "Applied {} to {}",
            fmt.label(),
            self.grid.selection_label()
        );
    }

    fn clear_format(&mut self, doc: &mut AutoCommit) {
        for addr in self.grid.selected_cells() {
            SheetsDoc::set_cell_format(doc, self.active_sheet, addr, CellFormat::General);
        }
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Cleared format on {}", self.grid.selection_label());
    }

    // --- clipboard ----------------------------------------------------------

    /// Copy (or cut) the current selection. The rich rectangle (raw + format)
    /// is retained for a lossless in-app paste, and a tab/newline-separated
    /// grid of the *displayed* values is pushed to the system clipboard so it
    /// pastes cleanly into other apps. A cut clears its source cells when the
    /// paste lands (spreadsheet "move" semantics), so nothing is lost if the
    /// user changes their mind.
    fn copy_selection(&mut self, cut: bool) {
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let mut cells = Vec::with_capacity((r1 - r0 + 1) as usize);
        let mut tsv_rows = Vec::with_capacity((r1 - r0 + 1) as usize);
        for r in r0..=r1 {
            let mut row_cells = Vec::with_capacity((c1 - c0 + 1) as usize);
            let mut tsv_cols = Vec::with_capacity((c1 - c0 + 1) as usize);
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
        self.clipboard = Some(ClipboardGrid { src: (c0, r0), cells, cut, tsv });
        let verb = if cut { "Cut" } else { "Copied" };
        self.status = format!("{verb} {}", self.grid.selection_label());
        self.dirty = true;
    }

    /// Paste at the active cell. When the system clipboard still holds exactly
    /// what we last copied, the rich rectangle is used — raw inputs and
    /// formats are restored and relative formula references shift to the new
    /// location. Otherwise the plain-text clipboard is parsed as a TSV grid
    /// and each field is smart-typed, so a paste from any other app works too.
    fn paste(&mut self, doc: &mut AutoCommit) {
        let target = self.grid.active();
        let system = ui::clipboard_paste();
        let use_internal = match (&self.clipboard, &system) {
            (Some(cb), Some(text)) => *text == cb.tsv,
            // No readable system clipboard (e.g. headless) → trust our own copy.
            (Some(_), None) => true,
            _ => false,
        };

        if use_internal {
            // `take` so a cut is one-shot; a plain copy is re-stored below.
            let cb = self.clipboard.take().unwrap();
            let rows = cb.cells.len() as u32;
            let cols = cb.cells.first().map(Vec::len).unwrap_or(0) as u32;
            if rows == 0 || cols == 0 {
                self.clipboard = Some(cb);
                return;
            }
            let dc = target.0 as i64 - cb.src.0 as i64;
            let dr = target.1 as i64 - cb.src.1 as i64;
            // A cut moves the block: clear the whole source first, then write.
            if cb.cut {
                for r in 0..rows {
                    for c in 0..cols {
                        let addr = (cb.src.0 + c, cb.src.1 + r);
                        SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, "");
                        SheetsDoc::set_cell_format(doc, self.active_sheet, addr, CellFormat::General);
                    }
                }
            }
            let mut last = target;
            for (ri, row) in cb.cells.iter().enumerate() {
                for (ci, (raw, fmt)) in row.iter().enumerate() {
                    let addr = (target.0 + ci as u32, target.1 + ri as u32);
                    let out_raw = if cb.cut {
                        raw.clone()
                    } else {
                        shift_formula_refs(raw, dc, dr)
                    };
                    SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, &out_raw);
                    SheetsDoc::set_cell_format(doc, self.active_sheet, addr, *fmt);
                    last = addr;
                }
            }
            if !cb.cut {
                self.clipboard = Some(cb);
            }
            self.grid.select_range_public(target, last);
        } else if let Some(text) = system {
            let cur = self.default_currency();
            let normalized = text.replace('\r', "");
            let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
            if body.is_empty() {
                self.status = "Clipboard is empty".into();
                return;
            }
            let mut last = target;
            for (ri, line) in body.split('\n').enumerate() {
                for (ci, field) in line.split('\t').enumerate() {
                    let addr = (target.0 + ci as u32, target.1 + ri as u32);
                    if field.is_empty() {
                        SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, "");
                        SheetsDoc::set_cell_format(doc, self.active_sheet, addr, CellFormat::General);
                    } else {
                        let inferred = infer_entry(field, cur);
                        SheetsDoc::set_cell_raw(doc, self.active_sheet, addr, &inferred.raw);
                        SheetsDoc::set_cell_format(doc, self.active_sheet, addr, inferred.format);
                    }
                    last = addr;
                }
            }
            self.grid.select_range_public(target, last);
        } else {
            self.status = "Clipboard is empty".into();
            return;
        }

        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Pasted into {}", self.grid.selection_label());
    }

    /// AutoSum: scan upward from the active cell for a run of numeric cells
    /// and write `=SUM(top:above)` into the active cell.
    fn auto_sum(&mut self, doc: &mut AutoCommit) {
        let (c, r) = self.grid.active();
        if r == 0 {
            self.status = "Nothing above to sum".into();
            return;
        }
        let mut top = r;
        let mut rr = r as i64 - 1;
        while rr >= 0 {
            let raw = SheetsDoc::cell_raw(doc, self.active_sheet, (c, rr as u32));
            let is_num = raw.trim().parse::<f64>().is_ok() || raw.starts_with('=');
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

    /// Import the most recently modified `.csv` file from the shared Sheets
    /// folder into a new sheet, running smart type inference on each field.
    /// No file picker exists in the toolkit yet, so "most recent in the
    /// folder" is the selection heuristic (drop a file there, then import).
    fn import_csv(&mut self, doc: &mut AutoCommit) {
        let Some(dir) = exports_dir() else {
            self.status = "No documents folder available for import".into();
            return;
        };
        let Some(path) = newest_with_ext(&dir, "csv") else {
            self.status = format!("No .csv files to import in {}", dir.display());
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("CSV import failed: {e}");
                return;
            }
        };
        let parsed = crate::csv::parse_csv(&text);
        let count = parsed.len();
        let idx = SheetsDoc::add_sheet(doc, &sheet_name_from_path(&path));
        let cur = self.default_currency();
        for (addr, raw) in parsed {
            let inferred = infer_entry(&raw, cur);
            SheetsDoc::set_cell_raw(doc, idx, addr, &inferred.raw);
            if inferred.format != CellFormat::General {
                SheetsDoc::set_cell_format(doc, idx, addr, inferred.format);
            }
        }
        SheetsDoc::set_active_index(doc, idx);
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Imported {count} cells from {}", file_label(&path));
    }

    /// Build an export table for the active sheet from its computed values.
    fn active_table(&self, doc: &mut AutoCommit) -> crate::xlsx::SheetTable {
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet, cur);
        let values = formula::recalculate(&snap.raw);
        // Re-apply format propagation so exported computed cells carry units.
        let mut fmt = snap.fmt.clone();
        for (addr, raw) in &snap.raw {
            if !snap.fmt.contains_key(addr) {
                if let Some(body) = raw.strip_prefix('=') {
                    if let Some(inferred) = formula::infer_result_format(body, &snap.fmt) {
                        fmt.insert(*addr, inferred);
                    }
                }
            }
        }
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &fmt, &values)
    }

    fn export_csv(&mut self, doc: &mut AutoCommit) {
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet, cur);
        let values = formula::recalculate(&snap.raw);
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
        let Some(path) = export_path(&snap.name, "csv") else {
            self.status = "No documents folder available for export".into();
            return;
        };
        match std::fs::write(&path, csv) {
            Ok(_) => self.status = format!("Exported {}", file_label(&path)),
            Err(e) => self.status = format!("CSV export failed: {e}"),
        }
    }

    fn export_xlsx(&mut self, doc: &mut AutoCommit) {
        let table = self.active_table(doc);
        let Some(path) = export_path(&table.name, "xlsx") else {
            self.status = "No documents folder available for export".into();
            return;
        };
        match crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)) {
            Ok(_) => self.status = format!("Exported {}", file_label(&path)),
            Err(e) => self.status = format!("Excel export failed: {e}"),
        }
    }

    fn export_ods(&mut self, doc: &mut AutoCommit) {
        let table = self.active_table(doc);
        let Some(path) = export_path(&table.name, "ods") else {
            self.status = "No documents folder available for export".into();
            return;
        };
        match crate::ods::write_ods(&path, std::slice::from_ref(&table)) {
            Ok(_) => self.status = format!("Exported {}", file_label(&path)),
            Err(e) => self.status = format!("Calc export failed: {e}"),
        }
    }

    /// Import a parsed set of tables (one per sheet) into new sheets, running
    /// smart inference where a source cell carried no explicit format.
    fn import_tables(&mut self, doc: &mut AutoCommit, tables: Vec<crate::xlsx::SheetTable>, kind: &str) {
        let cur = self.default_currency();
        let mut total = 0;
        for table in tables {
            let idx = SheetsDoc::add_sheet(doc, &table.name);
            for (r, row) in table.rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    let Some(cell) = cell else { continue };
                    let addr = (c as u32, r as u32);
                    // Re-infer to normalize, but respect the imported format.
                    SheetsDoc::set_cell_raw(doc, idx, addr, &cell.raw);
                    let fmt = if cell.format == CellFormat::General {
                        infer_entry(&cell.raw, cur).format
                    } else {
                        cell.format
                    };
                    if fmt != CellFormat::General {
                        SheetsDoc::set_cell_format(doc, idx, addr, fmt);
                    }
                    total += 1;
                }
            }
            SheetsDoc::set_active_index(doc, idx);
        }
        self.needs_commit = true;
        self.reload(doc);
        self.status = format!("Imported {total} cells from {kind}");
    }

    fn import_xlsx(&mut self, doc: &mut AutoCommit) {
        let Some(dir) = exports_dir() else {
            self.status = "No documents folder available for import".into();
            return;
        };
        let Some(path) = newest_with_ext(&dir, "xlsx") else {
            self.status = format!("No .xlsx files to import in {}", dir.display());
            return;
        };
        match crate::xlsx::read_xlsx(&path) {
            Ok(tables) => self.import_tables(doc, tables, &file_label(&path)),
            Err(e) => self.status = format!("Excel import failed: {e}"),
        }
    }

    fn import_ods(&mut self, doc: &mut AutoCommit) {
        let Some(dir) = exports_dir() else {
            self.status = "No documents folder available for import".into();
            return;
        };
        let Some(path) = newest_with_ext(&dir, "ods") else {
            self.status = format!("No .ods files to import in {}", dir.display());
            return;
        };
        match crate::ods::read_ods(&path) {
            Ok(tables) => self.import_tables(doc, tables, &file_label(&path)),
            Err(e) => self.status = format!("Calc import failed: {e}"),
        }
    }

    fn switch_sheet(&mut self, doc: &mut AutoCommit, idx: usize) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        SheetsDoc::set_active_index(doc, idx);
        self.grid.select_only((0, 0));
        self.reload(doc);
        self.status = format!(
            "Sheet: {}",
            self.sheet_names.get(idx).cloned().unwrap_or_default()
        );
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

    // --- settings actions ---------------------------------------------------

    fn apply_setting(&mut self, doc: &mut AutoCommit, kind: SettingKind) {
        match kind {
            SettingKind::DocCurrency(c) => self.doc_settings.currency = c,
            SettingKind::DocDate(d) => self.doc_settings.date_format = d,
            SettingKind::DocTime(t) => self.doc_settings.time_format = t,
        }
        // Persist the document overrides into the CRDT so collaborators (and
        // the next session) share the document's conventions.
        SheetsDoc::set_doc_settings(doc, &self.doc_settings);
        self.needs_commit = true;
        self.reload(doc);
    }

    // --- layout -------------------------------------------------------------

    fn relayout(&mut self) {
        let r = self.rect;
        self.menubar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + MENUBAR_H);
        self.formula_rect =
            Rect::new(r.x0, self.menubar_rect.y1, r.x1, self.menubar_rect.y1 + FORMULA_H);
        self.tabs_rect = Rect::new(r.x0, r.y1 - TABS_H, r.x1, r.y1);
        self.grid_rect = Rect::new(r.x0, self.formula_rect.y1, r.x1, self.tabs_rect.y0);
        self.grid.rect = self.grid_rect;

        // Menu bar: File ▾  Format ▾  Σ Sum  Settings ▾  ...  [type indicator]
        self.menu_buttons.clear();
        let bh = 30.0;
        let by = self.menubar_rect.y0 + (MENUBAR_H - bh) / 2.0;
        let mut x = self.menubar_rect.x0 + 10.0;
        let pad = 6.0;
        let mk = |label: &str,
                  x: &mut f64,
                  w: f64,
                  item: Option<MenuItem>,
                  opens: Option<OpenMenu>|
         -> MenuButton {
            let rect = Rect::new(*x, by, *x + w, by + bh);
            *x += w + pad;
            MenuButton {
                rect,
                label: label.to_string(),
                item,
                opens,
            }
        };
        self.menu_buttons
            .push(mk("File", &mut x, 56.0, None, Some(OpenMenu::File)));
        self.menu_buttons
            .push(mk("Format", &mut x, 70.0, None, Some(OpenMenu::Format)));
        self.menu_buttons.push(mk(
            "\u{03A3} Sum",
            &mut x,
            66.0,
            Some(MenuItem::AutoSum),
            None,
        ));
        self.menu_buttons.push(mk(
            "Doc settings",
            &mut x,
            104.0,
            Some(MenuItem::OpenDocSettings),
            None,
        ));

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

        self.relayout_overlays();
    }

    /// Lay out whichever dropdown / settings panel is open.
    fn relayout_overlays(&mut self) {
        self.dropdown_rows.clear();
        self.setting_chips.clear();
        let opener = self
            .menu_buttons
            .iter()
            .find(|b| b.opens == Some(self.open_menu))
            .map(|b| b.rect);

        match self.open_menu {
            OpenMenu::None => {}
            OpenMenu::File => {
                if let Some(anchor) = opener {
                    let items = [
                        ("New sheet", MenuItem::NewSheet),
                        ("Import CSV", MenuItem::ImportCsv),
                        ("Export CSV", MenuItem::ExportCsv),
                        ("Import Excel (.xlsx)", MenuItem::ImportXlsx),
                        ("Export Excel (.xlsx)", MenuItem::ExportXlsx),
                        ("Import Calc (.ods)", MenuItem::ImportOds),
                        ("Export Calc (.ods)", MenuItem::ExportOds),
                    ];
                    self.layout_dropdown(anchor, &items, 196.0);
                }
            }
            OpenMenu::Format => {
                if let Some(anchor) = opener {
                    let cur = self.default_currency();
                    let items = [
                        ("General", MenuItem::SetFormat(CellFormat::General)),
                        ("Number", MenuItem::SetFormat(CellFormat::Number)),
                        ("Currency", MenuItem::SetFormat(CellFormat::Currency(cur))),
                        ("Percent", MenuItem::SetFormat(CellFormat::Percent)),
                        (
                            "Date",
                            MenuItem::SetFormat(CellFormat::Date(self.locale().date_format)),
                        ),
                        (
                            "Time",
                            MenuItem::SetFormat(CellFormat::Time(self.locale().time_format)),
                        ),
                        ("Boolean", MenuItem::SetFormat(CellFormat::Boolean)),
                        ("Text", MenuItem::SetFormat(CellFormat::Text)),
                        ("Clear format", MenuItem::ClearFormat),
                    ];
                    self.layout_dropdown(anchor, &items, 160.0);
                }
            }
        }

        if self.settings_panel != SettingsPanel::None {
            self.layout_settings_panel();
        }
    }

    fn layout_dropdown(&mut self, anchor: Rect, items: &[(&str, MenuItem)], width: f64) {
        let row_h = 30.0;
        let pad = 6.0;
        let x0 = anchor.x0;
        let y0 = anchor.y1 + 4.0;
        let h = items.len() as f64 * row_h + pad * 2.0;
        self.dropdown_rect = Rect::new(x0, y0, x0 + width, y0 + h);
        let mut y = y0 + pad;
        for (label, item) in items {
            let rect = Rect::new(x0 + pad, y, x0 + width - pad, y + row_h - 2.0);
            self.dropdown_rows.push(ClickRow {
                rect,
                item: item.clone(),
            });
            let _ = label;
            y += row_h;
        }
    }

    fn layout_settings_panel(&mut self) {
        // Centered modal panel, sized to fit even the narrow viewport. Chip
        // rows start under the section label and wrap within the panel width.
        let pw = 440.0_f64.min(self.rect.width() - 32.0).max(300.0);
        let ph = 268.0;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        self.panel_rect = Rect::new(cx - pw / 2.0, cy - ph / 2.0, cx + pw / 2.0, cy + ph / 2.0);

        let chip_h = 26.0;
        let left = self.panel_rect.x0 + 20.0;
        let row_start = left + 96.0; // clears the section label column
        let chip_gap = 6.0;
        let wrap_x = self.panel_rect.x1 - 20.0;
        let mut y = self.panel_rect.y0 + 64.0;

        // Row: currency. First an explicit "Default" (inherit global) chip.
        self.settings_section_y[0] = y;
        {
            let mut x = row_start;
            let push = |chips: &mut Vec<SettingChip>, x: &mut f64, y: &mut f64, w: f64, kind| {
                if *x + w > wrap_x {
                    *x = row_start;
                    *y += chip_h + chip_gap;
                }
                chips.push(SettingChip {
                    rect: Rect::new(*x, *y, *x + w, *y + chip_h),
                    kind,
                });
                *x += w + chip_gap;
            };
            push(&mut self.setting_chips, &mut x, &mut y, 74.0, SettingKind::DocCurrency(None));
            for c in Currency::ALL {
                push(
                    &mut self.setting_chips,
                    &mut x,
                    &mut y,
                    48.0,
                    SettingKind::DocCurrency(Some(c)),
                );
            }
        }
        y += chip_h + 18.0;

        // Row: date style.
        self.settings_section_y[1] = y;
        {
            let mut x = row_start;
            let rect = Rect::new(x, y, x + 74.0, y + chip_h);
            self.setting_chips.push(SettingChip { rect, kind: SettingKind::DocDate(None) });
            x += 74.0 + chip_gap;
            for (label, d) in [
                ("ISO", DateStyle::Iso),
                ("US", DateStyle::Us),
                ("EU", DateStyle::Eu),
                ("Long", DateStyle::Long),
            ] {
                let w = (label.len() as f64 * 8.0 + 24.0).max(48.0);
                self.setting_chips.push(SettingChip {
                    rect: Rect::new(x, y, x + w, y + chip_h),
                    kind: SettingKind::DocDate(Some(d)),
                });
                x += w + chip_gap;
            }
        }
        y += chip_h + 18.0;

        // Row: time style.
        self.settings_section_y[2] = y;
        {
            let mut x = row_start;
            let rect = Rect::new(x, y, x + 74.0, y + chip_h);
            self.setting_chips.push(SettingChip { rect, kind: SettingKind::DocTime(None) });
            x += 74.0 + chip_gap;
            for (label, t) in [("24-hour", TimeStyle::H24), ("12-hour", TimeStyle::H12)] {
                let w = label.len() as f64 * 8.0 + 24.0;
                self.setting_chips.push(SettingChip {
                    rect: Rect::new(x, y, x + w, y + chip_h),
                    kind: SettingKind::DocTime(Some(t)),
                });
                x += w + chip_gap;
            }
        }
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
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &self.rect);

        // Menu bar.
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &self.menubar_rect);
        for b in &self.menu_buttons {
            let active = matches!(
                (&b.opens, self.open_menu),
                (Some(OpenMenu::File), OpenMenu::File) | (Some(OpenMenu::Format), OpenMenu::Format)
            );
            let accent = matches!(b.item, Some(MenuItem::AutoSum));
            let label = if b.opens.is_some() {
                format!("{} \u{25BE}", b.label) // ▾
            } else {
                b.label.clone()
            };
            self.draw_chip(scene, transform, tr, theme, b.rect, &label, active, accent);
        }

        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &Line::new(
                (self.menubar_rect.x0, self.menubar_rect.y1),
                (self.menubar_rect.x1, self.menubar_rect.y1),
            ),
        );

        // Formula bar.
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &self.formula_rect);
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
            transform
                * Affine::translate((
                    name_box.x0 + 8.0,
                    name_box.y0 + (name_box.height() - sm as f64) / 2.0,
                )),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text_styled(
            scene,
            "fx",
            theme.tokens.typography.scale.base,
            colors.text_dim,
            transform
                * Affine::translate((
                    self.formula_rect.x0 + NAME_BOX_W + 8.0,
                    self.formula_rect.y0 + 8.0,
                )),
            None,
            TextWeight::Regular,
            true,
            false,
        );
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
                transform
                    * Affine::translate((
                        self.formula_field_rect.x0 + 8.0,
                        self.formula_field_rect.y0
                            + (self.formula_field_rect.height() - base as f64) / 2.0,
                    )),
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
            &Line::new(
                (self.tabs_rect.x0, self.tabs_rect.y0),
                (self.tabs_rect.x1, self.tabs_rect.y0),
            ),
        );
        for (rect, idx) in &self.tab_rects {
            let active = *idx == self.active_sheet;
            let shape = RoundedRect::from_rect(*rect, theme.tokens.radii.sm as f64);
            let bg = if active { colors.surfaces.base } else { colors.surfaces.raised };
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
            if active {
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
        // Right side of the status bar: a live selection summary
        // (Sum / Avg / Count over the numeric cells in the selection), or —
        // when the selection holds no numbers — the active cell's type. This
        // is computed from real cell values every frame, and right-aligns so
        // it never collides with the tabs.
        let xs = theme.tokens.typography.scale.xs;
        let status_y = self.tabs_rect.y0 + (TABS_H - xs as f64) / 2.0;
        let info = match self.selection_summary() {
            Some(s) => format!(
                "Sum {}    Avg {}    Count {}",
                format_value(&CellValue::Number(s.sum), s.fmt),
                format_value(&CellValue::Number(s.avg), s.fmt),
                s.count,
            ),
            None => self.current_active_format.label(),
        };
        let (iw, _) = tr.measure(&info, xs, None);
        let info_x = self.tabs_rect.x1 - iw as f64 - 12.0;
        tr.draw_text_weighted(
            scene,
            &info,
            xs,
            colors.text_primary,
            transform * Affine::translate((info_x, status_y)),
            None,
            TextWeight::Semibold,
        );

        // The transient action message (e.g. "Exported…") sits between the
        // tabs and the summary, clipped so long paths never overrun it.
        let msg_left = self.add_tab_rect.x1 + 16.0;
        let msg_right = info_x - 16.0;
        if !self.status.is_empty() && msg_right - msg_left > 48.0 {
            let clip = Rect::new(msg_left, self.tabs_rect.y0, msg_right, self.tabs_rect.y1);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
            tr.draw_text(
                scene,
                &self.status,
                xs,
                colors.text_dim,
                transform * Affine::translate((msg_left, status_y)),
                None,
            );
            scene.pop_layer();
        }
    }

    fn draw_dropdown(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.open_menu == OpenMenu::None || self.dropdown_rows.is_empty() {
            return;
        }
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(self.dropdown_rect, theme.tokens.radii.md as f64);
        // Subtle shadow then panel.
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 60),
            None,
            &RoundedRect::from_rect(
                self.dropdown_rect + vello::kurbo::Vec2::new(0.0, 3.0),
                theme.tokens.radii.md as f64,
            ),
        );
        // Solid raised base first, then the overlay tint — keeps the panel
        // fully opaque so the grid never bleeds through.
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &shape);
        scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &shape);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &shape);
        let sm = theme.tokens.typography.scale.sm;
        let active_cat = self.current_active_format.category();
        for row in &self.dropdown_rows {
            let hovered = row.rect.contains(self.last_mouse);
            // Highlight the matching format row.
            let matches_active = match &row.item {
                MenuItem::SetFormat(f) => f.category() == active_cat,
                _ => false,
            };
            if hovered || matches_active {
                let rs = RoundedRect::from_rect(row.rect, theme.tokens.radii.sm as f64);
                let bg = if hovered {
                    colors.accent[5].with_alpha_factor(0.22)
                } else {
                    colors.accent[5].with_alpha_factor(0.10)
                };
                scene.fill(Fill::NonZero, transform, bg, None, &rs);
            }
            let label = menu_item_label(&row.item);
            tr.draw_text(
                scene,
                label,
                sm,
                colors.text_primary,
                transform
                    * Affine::translate((
                        row.rect.x0 + 10.0,
                        row.rect.y0 + (row.rect.height() - sm as f64) / 2.0,
                    )),
                None,
            );
        }
    }

    fn draw_settings_panel(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.settings_panel == SettingsPanel::None {
            return;
        }
        let colors = &theme.tokens.colors;
        // Dim the backdrop.
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 110),
            None,
            &self.rect,
        );
        let shape = RoundedRect::from_rect(self.panel_rect, theme.tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &shape);
        scene.fill(Fill::NonZero, transform, colors.surfaces.overlay, None, &shape);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &shape);

        let base = theme.tokens.typography.scale.base;
        let sm = theme.tokens.typography.scale.sm;
        let title = "Document settings";
        tr.draw_text_weighted(
            scene,
            title,
            base,
            colors.text_primary,
            transform * Affine::translate((self.panel_rect.x0 + 20.0, self.panel_rect.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );

        // Section labels, aligned to the exact chip-row Y captured at layout
        // time so they stay correct even when the currency row wraps.
        let label_x = self.panel_rect.x0 + 20.0;
        for (label, &section_y) in ["Currency", "Date", "Time"].iter().zip(&self.settings_section_y) {
            tr.draw_text(
                scene,
                label,
                sm,
                colors.text_dim,
                transform * Affine::translate((label_x, section_y + 4.0)),
                None,
            );
        }

        // Chips.
        for chip in &self.setting_chips {
            let (label, selected) = self.chip_label_and_state(chip.kind);
            let shape = RoundedRect::from_rect(chip.rect, theme.tokens.radii.sm as f64);
            let bg = if selected {
                colors.accent[5].with_alpha_factor(0.85)
            } else {
                colors.surfaces.raised
            };
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                if selected { colors.accent[5] } else { colors.border_standard },
                None,
                &shape,
            );
            let (tw, _) = tr.measure(&label, sm, None);
            let tx = chip.rect.center().x - tw as f64 / 2.0;
            let ty = chip.rect.y0 + (chip.rect.height() - sm as f64) / 2.0;
            let color = if selected { Color::WHITE } else { colors.text_primary };
            tr.draw_text(scene, &label, sm, color, transform * Affine::translate((tx, ty)), None);
        }

        // Footer: how the currently active cell renders under these settings.
        let footer = format!("Applies to this workbook. Active cell: {}", self.current_active_format.label());
        tr.draw_text(
            scene,
            &footer,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform * Affine::translate((self.panel_rect.x0 + 20.0, self.panel_rect.y1 - 28.0)),
            None,
        );
    }

    fn chip_label_and_state(&self, kind: SettingKind) -> (String, bool) {
        match kind {
            SettingKind::DocCurrency(None) => ("Default".into(), self.doc_settings.currency.is_none()),
            SettingKind::DocCurrency(Some(c)) => {
                (c.code().into(), self.doc_settings.currency == Some(c))
            }
            SettingKind::DocDate(None) => ("Default".into(), self.doc_settings.date_format.is_none()),
            SettingKind::DocDate(Some(d)) => (date_label(d).into(), self.doc_settings.date_format == Some(d)),
            SettingKind::DocTime(None) => ("Default".into(), self.doc_settings.time_format.is_none()),
            SettingKind::DocTime(Some(t)) => (time_label(t).into(), self.doc_settings.time_format == Some(t)),
        }
    }

    /// The active cell's effective format, read from the memoised reload cache
    /// (the propagation of formula units is already baked into `eff_fmt`).
    fn format_of_active(&self) -> CellFormat {
        let active = self.grid.active();
        self.cache
            .as_ref()
            .and_then(|c| c.eff_fmt.get(&active).copied())
            .unwrap_or_default()
    }

    /// A live summary of the current selection for the status bar: the sum,
    /// average, and count of numeric cells. Returns `None` unless at least two
    /// numbers are selected — a single cell shows its type instead, so the
    /// summary only appears when it is actually informative (matching how
    /// desktop spreadsheets behave).
    fn selection_summary(&self) -> Option<SelectionSummary> {
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
}

/// A numeric summary of the selection shown in the status bar.
struct SelectionSummary {
    sum: f64,
    avg: f64,
    count: usize,
    fmt: CellFormat,
}

fn menu_item_label(item: &MenuItem) -> &'static str {
    match item {
        MenuItem::NewSheet => "New sheet",
        MenuItem::ImportCsv => "Import CSV",
        MenuItem::ExportCsv => "Export CSV",
        MenuItem::ImportXlsx => "Import Excel (.xlsx)",
        MenuItem::ExportXlsx => "Export Excel (.xlsx)",
        MenuItem::ImportOds => "Import Calc (.ods)",
        MenuItem::ExportOds => "Export Calc (.ods)",
        MenuItem::SetFormat(f) => format_menu_label(*f),
        MenuItem::ClearFormat => "Clear format",
        MenuItem::OpenDocSettings => "Document settings",
        MenuItem::AutoSum => "AutoSum",
    }
}

fn format_menu_label(f: CellFormat) -> &'static str {
    match f.category() {
        FormatCategory::General => "General",
        FormatCategory::Number => "Number",
        FormatCategory::Currency => "Currency",
        FormatCategory::Percent => "Percent",
        FormatCategory::Date => "Date",
        FormatCategory::Time => "Time",
        FormatCategory::Boolean => "Boolean",
        FormatCategory::Text => "Text",
    }
}

fn date_label(d: DateStyle) -> &'static str {
    match d {
        DateStyle::Iso => "ISO",
        DateStyle::Us => "US",
        DateStyle::Eu => "EU",
        DateStyle::Long => "Long",
    }
}
fn time_label(t: TimeStyle) -> &'static str {
    match t {
        TimeStyle::H24 => "24-hour",
        TimeStyle::H12 => "12-hour",
    }
}

fn tab_width(name: &str) -> f64 {
    (name.chars().count() as f64 * 7.5 + 26.0).clamp(64.0, 180.0)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

/// Flatten a cell's display text into a single TSV field: tabs and newlines
/// would break the row/column structure, so they collapse to spaces.
fn tsv_cell(display: &str) -> String {
    display.replace(['\t', '\n', '\r'], " ")
}

/// Rewrite a formula so its **relative** cell references shift by `(dc, dr)`
/// when it is pasted to a new location — the behaviour every spreadsheet
/// applies on copy/paste. `$`-anchored column/row components stay put, string
/// literals and function names are left untouched, and numeric literals
/// (including scientific notation like `1e3`) are never mistaken for a
/// reference. Non-formula raw inputs are returned unchanged.
fn shift_formula_refs(raw: &str, dc: i64, dr: i64) -> String {
    let Some(body) = raw.strip_prefix('=') else {
        return raw.to_string();
    };
    if dc == 0 && dr == 0 {
        return raw.to_string();
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(raw.len() + 4);
    out.push('=');
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // Copy a string literal verbatim, including the closing quote.
            out.push(c);
            i += 1;
            while i < chars.len() {
                out.push(chars[i]);
                let done = chars[i] == '"';
                i += 1;
                if done {
                    break;
                }
            }
            continue;
        }
        if c == '$' || c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i] == '$' || chars[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            // A run right after a digit/dot is an exponent (`1e3`), not a ref.
            let prev_numish = start > 0
                && (chars[start - 1].is_ascii_digit() || chars[start - 1] == '.');
            // An identifier followed by `(` is a function name, not a ref.
            let mut k = i;
            while k < chars.len() && chars[k].is_whitespace() {
                k += 1;
            }
            let is_call = k < chars.len() && chars[k] == '(';
            if !prev_numish && !is_call {
                if let Some(shifted) = shift_ref_token(&token, dc, dr) {
                    out.push_str(&shifted);
                    continue;
                }
            }
            out.push_str(&token);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Shift one `A1`/`$A$1`-shaped token by `(dc, dr)`, preserving any `$`
/// anchors. Returns `None` when the token is not a well-formed reference
/// (leaving function names, booleans, etc. untouched).
fn shift_ref_token(tok: &str, dc: i64, dr: i64) -> Option<String> {
    let bytes = tok.as_bytes();
    let mut i = 0;
    let col_abs = i < bytes.len() && bytes[i] == b'$';
    if col_abs {
        i += 1;
    }
    let ls = i;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == ls {
        return None;
    }
    let letters = &tok[ls..i];
    let row_abs = i < bytes.len() && bytes[i] == b'$';
    if row_abs {
        i += 1;
    }
    let ds = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == ds || i != bytes.len() {
        return None;
    }
    let col = crate::model::letters_to_col(letters)? as i64;
    let row: i64 = tok[ds..i].parse().ok()?; // 1-based
    let new_col = if col_abs { col } else { (col + dc).max(0) };
    let new_row = if row_abs { row } else { (row + dr).max(1) };
    let mut s = String::new();
    if col_abs {
        s.push('$');
    }
    s.push_str(&crate::model::col_to_letters(new_col as u32));
    if row_abs {
        s.push('$');
    }
    s.push_str(&new_row.to_string());
    Some(s)
}

/// Where the workbook is persisted between sessions:
/// `$XDG_DATA_HOME/qos/sheets/workbook.automerge` (falls back to
/// `~/.local/share`).
fn workbook_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(dirs::data_dir)?;
    Some(base.join("qos").join("sheets").join("workbook.automerge"))
}

/// The shared folder for CSV/Excel/Calc import and export — the user's
/// documents directory when available, otherwise the app's data directory.
/// The same folder is both the export target and the import source, so a
/// file exported here (or dropped in) is the one a later import reads.
fn exports_dir() -> Option<PathBuf> {
    let base = dirs::document_dir().or_else(dirs::data_dir)?;
    Some(base.join("qOS Sheets"))
}

/// Build an export path `<exports_dir>/<sheet>.<ext>`, creating the folder.
fn export_path(sheet_name: &str, ext: &str) -> Option<PathBuf> {
    let dir = exports_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let name = sanitize(sheet_name);
    let stem = if name.is_empty() { "Sheet".to_string() } else { name };
    Some(dir.join(format!("{stem}.{ext}")))
}

/// The most recently modified file in `dir` whose extension matches `ext`
/// (case-insensitive), or `None` when the folder holds no such file.
fn newest_with_ext(dir: &std::path::Path, ext: &str) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let matches = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case(ext))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

/// A new sheet name derived from an imported file's stem.
fn sheet_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported".to_string())
}

/// A short, user-facing label for a path (its file name) for status messages.
fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string())
}

impl Widget for SheetsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            // Pull the shared global defaults once at startup.
            self.global = GlobalSettings::load();
            self.initialize(state);
            self.seeded = true;
        }
        self.reload(state);
        self.current_active_format = self.format_of_active();
        self.persist_if_dirty(state);
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
        // Overlays paint last so they sit above the grid.
        self.draw_dropdown(scene, transform, text_renderer, theme);
        self.draw_settings_panel(scene, transform, text_renderer, theme);
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

                // Settings panel is modal: clicks go to it (or dismiss it).
                if self.settings_panel != SettingsPanel::None {
                    if let Some(kind) = self.setting_chip_at(p) {
                        self.apply_setting(state, kind);
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                    if !self.panel_rect.contains(p) {
                        self.settings_panel = SettingsPanel::None;
                        self.dirty = true;
                    }
                    *handled = true;
                    return;
                }

                // Open dropdown takes priority.
                if self.open_menu != OpenMenu::None {
                    if let Some(item) = self.dropdown_item_at(p) {
                        self.open_menu = OpenMenu::None;
                        self.run_menu_item(state, item);
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                    if !self.dropdown_rect.contains(p) && !self.menubar_rect.contains(p) {
                        self.open_menu = OpenMenu::None;
                        self.dirty = true;
                    }
                }

                // Menu bar.
                if self.menubar_rect.contains(p) {
                    if let Some(b) = self.menu_buttons.iter().find(|b| b.rect.contains(p)) {
                        if let Some(opens) = b.opens {
                            self.open_menu = if self.open_menu == opens {
                                OpenMenu::None
                            } else {
                                opens
                            };
                        } else if let Some(item) = b.item.clone() {
                            self.run_menu_item(state, item);
                        }
                        self.dirty = true;
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
                if self.open_menu != OpenMenu::None || self.settings_panel != SettingsPanel::None {
                    self.dirty = true;
                }
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
                // Escape closes any open overlay.
                if let WindowEvent::KeyboardInput { event: ke, .. } = event {
                    if ke.state.is_pressed()
                        && matches!(
                            ke.logical_key,
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                        )
                        && (self.open_menu != OpenMenu::None
                            || self.settings_panel != SettingsPanel::None)
                    {
                        self.open_menu = OpenMenu::None;
                        self.settings_panel = SettingsPanel::None;
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                }

                // Clipboard chords + select-all operate on the grid selection,
                // so they are skipped while an in-cell edit owns the keyboard.
                if let WindowEvent::KeyboardInput { event: ke, .. } = event {
                    let ctrl =
                        modifiers.state().control_key() || modifiers.state().super_key();
                    if ke.state.is_pressed() && ctrl && !self.grid.is_editing() {
                        if let winit::keyboard::Key::Character(c) = &ke.logical_key {
                            match c.as_str() {
                                "c" | "C" => {
                                    self.copy_selection(false);
                                    *handled = true;
                                    return;
                                }
                                "x" | "X" => {
                                    self.copy_selection(true);
                                    *handled = true;
                                    return;
                                }
                                "v" | "V" => {
                                    self.paste(state);
                                    *handled = true;
                                    self.dirty = true;
                                    return;
                                }
                                "a" | "A" => {
                                    self.grid.select_used_range();
                                    *handled = true;
                                    self.dirty = true;
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }
                }

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
    fn cell_under(&self, p: Point) -> Option<Addr> {
        if self.grid_rect.contains(p) {
            self.grid.cell_at_public(p)
        } else {
            None
        }
    }

    fn dropdown_item_at(&self, p: Point) -> Option<MenuItem> {
        self.dropdown_rows
            .iter()
            .find(|r| r.rect.contains(p))
            .map(|r| r.item.clone())
    }

    fn setting_chip_at(&self, p: Point) -> Option<SettingKind> {
        self.setting_chips
            .iter()
            .find(|c| c.rect.contains(p))
            .map(|c| c.kind)
    }

    fn run_menu_item(&mut self, doc: &mut AutoCommit, item: MenuItem) {
        match item {
            MenuItem::NewSheet => self.add_sheet(doc),
            MenuItem::ImportCsv => self.import_csv(doc),
            MenuItem::ExportCsv => self.export_csv(doc),
            MenuItem::ImportXlsx => self.import_xlsx(doc),
            MenuItem::ExportXlsx => self.export_xlsx(doc),
            MenuItem::ImportOds => self.import_ods(doc),
            MenuItem::ExportOds => self.export_ods(doc),
            MenuItem::SetFormat(f) => self.apply_format(doc, f),
            MenuItem::ClearFormat => self.clear_format(doc),
            MenuItem::OpenDocSettings => {
                self.settings_panel = SettingsPanel::Document;
                self.open_menu = OpenMenu::None;
                self.relayout_overlays();
            }
            MenuItem::AutoSum => self.auto_sum(doc),
        }
        self.dirty = true;
    }
}

/// Seed a realistic two-sheet workbook: a monthly budget with SUM /
/// AVERAGE / IF formulas, and a quarterly sales sheet with growth %. The
/// money columns are typed via smart entry so they pick up currency
/// automatically; one row shows a date and a percent for the screenshot.
fn seed_workbook(doc: &mut AutoCommit) {
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
    set(doc, "G1", "Updated");

    let rows: &[(&str, f64, f64, f64)] = &[
        ("Rent", 1200.0, 1200.0, 1200.0),
        ("Groceries", 420.0, 460.0, 510.0),
        ("Utilities", 180.0, 165.0, 200.0),
        ("Transport", 90.0, 110.0, 95.0),
        ("Entertainment", 150.0, 200.0, 175.0),
    ];
    for (i, (name, jan, feb, mar)) in rows.iter().enumerate() {
        let r = i + 2;
        set(doc, &format!("A{r}"), name);
        set(doc, &format!("B{r}"), &jan.to_string());
        set(doc, &format!("C{r}"), &feb.to_string());
        set(doc, &format!("D{r}"), &mar.to_string());
        // E inherits currency via propagation from the SUM over a currency
        // range; we still mark it explicitly to be safe.
        set(doc, &format!("E{r}"), &format!("=SUM(B{r}:D{r})"));
        set(doc, &format!("F{r}"), &format!("=AVERAGE(B{r}:D{r})"));
    }
    set(doc, "A7", "Total");
    set(doc, "B7", "=SUM(B2:B6)");
    set(doc, "C7", "=SUM(C2:C6)");
    set(doc, "D7", "=SUM(D2:D6)");
    set(doc, "E7", "=SUM(E2:E6)");
    set(doc, "F7", "=AVERAGE(B7:D7)");
    set(doc, "A9", "Monthly budget");
    set(doc, "B9", "2600");
    set(doc, "A10", "Over budget?");
    set(doc, "B10", "=IF(B7>B9,\"OVER\",\"OK\")");
    set(doc, "A11", "Headroom");
    // =B9-B7 with both currency operands → currency propagates.
    set(doc, "B11", "=B9-B7");
    set(doc, "A12", "Tax set-aside");
    // A percent literal — smart inference would normally store 0.18; here we
    // store it pre-normalized and mark the format so the seed is explicit.
    set(doc, "B12", "0.18");
    setf(doc, "B12", CellFormat::Percent);

    // Currency on the money columns/rows (explicit so the seed is stable).
    for r in 2..=7 {
        for col in ["B", "C", "D", "E"] {
            setf(doc, &format!("{col}{r}"), CellFormat::Currency(Currency::USD));
        }
        setf(doc, &format!("F{r}"), CellFormat::Number);
    }
    setf(doc, "B9", CellFormat::Currency(Currency::USD));
    setf(doc, "B11", CellFormat::Currency(Currency::USD));
    // A date in the "Updated" column, smart-typed style.
    set(doc, "G2", "2024-01-15");
    setf(doc, "G2", CellFormat::Date(DateStyle::Iso));

    // ---- Sheet 2: Sales (EUR document feel via explicit currency) ----
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
            ssetf(doc, &format!("{col}{r}"), CellFormat::Currency(Currency::EUR));
        }
        ssetf(doc, &format!("G{r}"), CellFormat::Percent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::GridCmd;

    /// Serializes the tests that mutate the process-global `XDG_DATA_HOME`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serializes tests that round-trip through the process-global system
    /// clipboard, so a parallel test's copy can't land between another's
    /// copy and paste (the app itself is single-instance, so this only
    /// matters under the parallel test harness).
    static CLIP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Build an app in the seeded (sample) mode and run the first `sync`, which
    /// is exactly what the live/offscreen paths do before drawing.
    fn seeded_app() -> (SheetsApp, AutoCommit) {
        let mut app = SheetsApp::new();
        app.shots_mode(); // seed sample data, no disk persistence
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    fn value_at(app: &SheetsApp, addr: Addr) -> Option<CellValue> {
        app.cache.as_ref().and_then(|c| c.values.get(&addr).cloned())
    }

    #[test]
    fn seed_computes_real_formulas() {
        let (app, _doc) = seeded_app();
        // Budget!E2 = SUM(B2:D2) = 1200 + 1200 + 1200 = 3600.
        assert_eq!(value_at(&app, (4, 1)), Some(CellValue::Number(3600.0)));
        // The active cell (E2) is a propagated currency.
        assert!(matches!(app.format_of_active(), CellFormat::Currency(_)));
    }

    #[test]
    fn editing_a_cell_recalculates_dependents() {
        // This is the real path `handle_event` runs after `grid.key`: it hands
        // the emitted commands to `apply_cmds`, which infers the entry, writes
        // the CRDT, and recalculates.
        let (mut app, mut doc) = seeded_app();
        let b2 = (1, 1);
        let e2 = (4, 1);
        assert_eq!(value_at(&app, e2), Some(CellValue::Number(3600.0)));

        app.apply_cmds(
            &mut doc,
            vec![
                GridCmd::SetCell { addr: b2, raw: "1000".into() },
                GridCmd::Commit,
            ],
        );

        // The stored raw changed and the dependent SUM recomputed.
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, b2), "1000");
        assert_eq!(value_at(&app, e2), Some(CellValue::Number(3400.0)));
    }

    #[test]
    fn auto_sum_writes_a_working_formula() {
        let (mut app, mut doc) = seeded_app();
        // Put the active cell just below a numeric run and AutoSum it.
        app.grid.select_only((1, 7)); // B8, under B2:B7
        app.auto_sum(&mut doc);
        let raw = SheetsDoc::cell_raw(&mut doc, 0, (1, 7));
        assert!(raw.starts_with("=SUM("), "auto-sum wrote {raw:?}");
        // The computed value is finite and positive (a real sum, not an error).
        match value_at(&app, (1, 7)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0),
            other => panic!("expected a numeric AutoSum result, got {other:?}"),
        }
    }

    #[test]
    fn apply_format_changes_active_type() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 8)); // B9 = 2600 (currency in the seed)
        app.apply_format(&mut doc, CellFormat::Percent);
        assert_eq!(app.format_of_active(), CellFormat::Percent);
    }

    #[test]
    fn selection_summary_sums_real_values() {
        let (mut app, _doc) = seeded_app();
        // Select the Jan column body B2:B6 (five numeric cells).
        app.grid.set_selection((1, 1), (1, 5));
        let summary = app.selection_summary().expect("numeric selection");
        assert_eq!(summary.count, 5);
        // 1200 + 420 + 180 + 90 + 150 = 2040.
        assert!((summary.sum - 2040.0).abs() < 1e-6, "sum={}", summary.sum);
        assert!((summary.avg - 408.0).abs() < 1e-6, "avg={}", summary.avg);
    }

    #[test]
    fn copy_paste_preserves_value_and_format() {
        // Copy a currency cell into an empty one; the raw and the currency
        // format both come across, and it recomputes to the same number.
        let _clip = CLIP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 1)); // B2 = 1200, currency
        app.copy_selection(false);
        app.grid.select_only((1, 7)); // B8, empty
        app.paste(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (1, 7)), "1200");
        assert!(matches!(app.grid.cell_fmt_at((1, 7)), CellFormat::Currency(_)));
        assert_eq!(value_at(&app, (1, 7)), Some(CellValue::Number(1200.0)));
    }

    #[test]
    fn copy_paste_shifts_relative_formula_refs() {
        // Copy E2 (`=SUM(B2:D2)`) and paste it over F3: the relative refs
        // shift by (+1 col, +1 row) to `=SUM(C3:E3)`, overwriting F3's seeded
        // AVERAGE, and the pasted formula recomputes against the new operands.
        let _clip = CLIP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2
        app.copy_selection(false);
        app.grid.select_only((5, 2)); // F3
        app.paste(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 2)), "=SUM(C3:E3)");
        // C3=460, D3=510, E3=SUM(B3:D3)=1390 → 2360.
        assert_eq!(value_at(&app, (5, 2)), Some(CellValue::Number(2360.0)));
    }

    #[test]
    fn cut_moves_cell_and_clears_source() {
        let _clip = CLIP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0)); // A1 = "Category"
        app.copy_selection(true); // cut
        app.grid.select_only((0, 20)); // A21, empty
        app.paste(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 20)), "Category");
    }

    #[test]
    fn shift_refs_handles_anchors_functions_and_numbers() {
        assert_eq!(shift_formula_refs("=A1+$B$2", 1, 1), "=B2+$B$2");
        assert_eq!(shift_formula_refs("=SUM(A1:A3)*2", 2, 0), "=SUM(C1:C3)*2");
        // Mixed anchors: `$A1` shifts row only, `A$1` shifts column only.
        assert_eq!(shift_formula_refs("=$A1+A$1", 1, 1), "=$A2+B$1");
        // Scientific-notation literals are not references.
        assert_eq!(shift_formula_refs("=1e3+A1", 0, 1), "=1e3+A2");
        // Function names + string literals are left untouched.
        assert_eq!(
            shift_formula_refs("=IF(A1>0,\"A1\",B2)", 0, 1),
            "=IF(A2>0,\"A1\",B3)"
        );
        // A plain literal is returned unchanged.
        assert_eq!(shift_formula_refs("1200", 3, 3), "1200");
    }

    #[test]
    fn blank_launch_opens_a_single_empty_sheet() {
        let _guard = ENV_LOCK.lock().unwrap();
        // A real launch (persist on, but pointed at an empty temp data dir)
        // must NOT fabricate the sample workbook.
        let tmp = std::env::temp_dir().join(format!("qos-sheets-blank-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("XDG_DATA_HOME", &tmp) };

        let mut app = SheetsApp::new(); // persist = true, seed_sample_data = false
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        assert_eq!(SheetsDoc::sheet_count(&mut doc), 1);
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Sheet1"]);
        // No fabricated cells.
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
        assert!(snap.raw.is_empty(), "blank launch seeded {} cells", snap.raw.len());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Not a correctness test: a perf guard demonstrating that a no-op reload
    /// (no document change, as on every scroll/selection/hover frame) skips the
    /// snapshot + full recalculation. Run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_reload_memoisation() {
        let (mut app, mut doc) = seeded_app();
        let n = 20_000;

        // Memoised path (heads unchanged) — what interaction frames now hit.
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            app.reload(&mut doc);
        }
        let memo = t0.elapsed();

        // Forced path (snapshot + recalculate every call) — the old behaviour.
        let t1 = std::time::Instant::now();
        for _ in 0..n {
            app.cache = None;
            app.reload(&mut doc);
        }
        let forced = t1.elapsed();

        eprintln!(
            "reload x{n}: memoised={:?} ({:.3}us/call)  forced={:?} ({:.3}us/call)",
            memo,
            memo.as_secs_f64() * 1e6 / n as f64,
            forced,
            forced.as_secs_f64() * 1e6 / n as f64,
        );
        assert!(memo < forced, "memoised reload must be cheaper than a forced one");
    }

    #[test]
    fn workbook_persists_and_reloads() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("qos-sheets-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("XDG_DATA_HOME", &tmp) };

        // Session one: blank launch, edit a cell, persist.
        let mut app = SheetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        app.apply_cmds(
            &mut doc,
            vec![
                GridCmd::SetCell { addr: (0, 0), raw: "hello".into() },
                GridCmd::Commit,
            ],
        );
        app.persist_if_dirty(&mut doc);
        assert!(workbook_path().unwrap().exists(), "workbook was not written");

        // Session two: a fresh app + fresh document reopens the saved workbook.
        let mut app2 = SheetsApp::new();
        let mut doc2 = AutoCommit::new();
        app2.sync(&mut doc2, &root);
        assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (0, 0)), "hello");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
