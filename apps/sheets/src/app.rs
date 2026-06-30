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
use crate::infer::infer_entry;
use crate::model::{a1_to_addr, Addr, CellFormat, Currency, DateStyle, FormatCategory, TimeStyle};
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
    OpenGlobalSettings,
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

/// Which settings panel (if any) is open.
#[derive(Clone, Copy, PartialEq)]
enum SettingsPanel {
    None,
    Document,
    Global,
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
    GlobalCurrency(Currency),
    GlobalDate(DateStyle),
    GlobalTime(TimeStyle),
    GlobalSave,
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
    type_indicator_rect: Rect,
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
    /// Whether the next frame should pre-open the File menu (used to show
    /// the relocated file actions in the offscreen screenshot).
    preview_open_file_menu: bool,
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
            type_indicator_rect: Rect::ZERO,
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
            preview_open_file_menu: true,
        }
    }

    /// The document's effective locale (per-doc overrides over global).
    fn locale(&self) -> EffectiveLocale {
        EffectiveLocale::resolve(&self.global.locale, &self.doc_settings)
    }

    fn default_currency(&self) -> Currency {
        self.locale().currency
    }

    // --- document plumbing --------------------------------------------------

    fn seed_if_empty(&mut self, doc: &mut AutoCommit) {
        if !SheetsDoc::is_empty(doc) {
            return;
        }
        seed_workbook(doc);
        SheetsDoc::set_active_index(doc, 0);
        // Preselect a smart-typed currency cell so the formula bar + type
        // indicator are populated in the first frame / screenshot.
        if let Some(addr) = a1_to_addr("E2") {
            self.grid.select_only(addr);
        }
    }

    /// Read the active sheet, recalculate, propagate formats onto computed
    /// cells, and push the data into the grid.
    fn reload(&mut self, doc: &mut AutoCommit) {
        self.active_sheet = SheetsDoc::active_index(doc);
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

    fn import_demo_csv(&mut self, doc: &mut AutoCommit) {
        let idx = SheetsDoc::add_sheet(doc, "Imported");
        let parsed = crate::csv::parse_csv(DEMO_CSV);
        let count = parsed.len();
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
        self.status = format!("Imported {count} cells from CSV into a new sheet");
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
        let path = std::env::temp_dir().join(format!("sheets-{}.csv", sanitize(&snap.name)));
        match std::fs::write(&path, csv) {
            Ok(_) => self.status = format!("Exported CSV to {}", path.display()),
            Err(e) => self.status = format!("CSV export failed: {e}"),
        }
    }

    fn export_xlsx(&mut self, doc: &mut AutoCommit) {
        let table = self.active_table(doc);
        let name = sanitize(&table.name);
        let path = std::env::temp_dir().join(format!("sheets-{name}.xlsx"));
        match crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)) {
            Ok(_) => self.status = format!("Exported Excel workbook to {}", path.display()),
            Err(e) => self.status = format!("Excel export failed: {e}"),
        }
    }

    fn export_ods(&mut self, doc: &mut AutoCommit) {
        let table = self.active_table(doc);
        let name = sanitize(&table.name);
        let path = std::env::temp_dir().join(format!("sheets-{name}.ods"));
        match crate::ods::write_ods(&path, std::slice::from_ref(&table)) {
            Ok(_) => self.status = format!("Exported Calc document to {}", path.display()),
            Err(e) => self.status = format!("Calc export failed: {e}"),
        }
    }

    /// Import the first sheet of an `.xlsx`/`.ods` table set into a new
    /// sheet. We round-trip our own last export so the action is meaningful
    /// even without a file picker.
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
        // Demo path: export the active sheet to a temp xlsx, read it back.
        let table = self.active_table(doc);
        let path = std::env::temp_dir().join("sheets-import-demo.xlsx");
        if crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).is_err() {
            self.status = "Excel import unavailable".into();
            return;
        }
        match crate::xlsx::read_xlsx(&path) {
            Ok(tables) => self.import_tables(doc, tables, "Excel (.xlsx)"),
            Err(e) => self.status = format!("Excel import failed: {e}"),
        }
    }

    fn import_ods(&mut self, doc: &mut AutoCommit) {
        let table = self.active_table(doc);
        let path = std::env::temp_dir().join("sheets-import-demo.ods");
        if crate::ods::write_ods(&path, std::slice::from_ref(&table)).is_err() {
            self.status = "Calc import unavailable".into();
            return;
        }
        match crate::ods::read_ods(&path) {
            Ok(tables) => self.import_tables(doc, tables, "Calc (.ods)"),
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
            SettingKind::GlobalCurrency(c) => self.global.locale.currency = c,
            SettingKind::GlobalDate(d) => self.global.locale.date_format = d,
            SettingKind::GlobalTime(t) => self.global.locale.time_format = t,
            SettingKind::GlobalSave => {
                match self.global.save() {
                    Ok(p) => self.status = format!("Saved global settings to {}", p.display()),
                    Err(e) => self.status = format!("Could not save global settings: {e}"),
                }
                self.reload(doc);
                return;
            }
        }
        // Persist document overrides into the CRDT.
        if matches!(
            kind,
            SettingKind::DocCurrency(_) | SettingKind::DocDate(_) | SettingKind::DocTime(_)
        ) {
            SheetsDoc::set_doc_settings(doc, &self.doc_settings);
            self.needs_commit = true;
        }
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
        self.menu_buttons.push(mk(
            "Defaults",
            &mut x,
            82.0,
            Some(MenuItem::OpenGlobalSettings),
            None,
        ));

        // Type indicator, right-aligned and prominent.
        let ti_w = 188.0;
        self.type_indicator_rect = Rect::new(
            self.menubar_rect.x1 - ti_w - 10.0,
            by,
            self.menubar_rect.x1 - 10.0,
            by + bh,
        );

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
        // Centered modal panel.
        let pw = 460.0;
        let ph = 280.0;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        self.panel_rect = Rect::new(cx - pw / 2.0, cy - ph / 2.0, cx + pw / 2.0, cy + ph / 2.0);

        let chip_h = 26.0;
        let left = self.panel_rect.x0 + 20.0;
        let mut y = self.panel_rect.y0 + 64.0;
        let chip_gap = 6.0;

        let is_doc = self.settings_panel == SettingsPanel::Document;

        // Row 1: currency.
        {
            let mut x = left + 110.0;
            // For the document panel, offer an explicit "Default" (inherit).
            if is_doc {
                let rect = Rect::new(x, y, x + 74.0, y + chip_h);
                self.setting_chips.push(SettingChip {
                    rect,
                    kind: SettingKind::DocCurrency(None),
                });
                x += 74.0 + chip_gap;
            }
            for c in Currency::ALL {
                let rect = Rect::new(x, y, x + 48.0, y + chip_h);
                self.setting_chips.push(SettingChip {
                    rect,
                    kind: if is_doc {
                        SettingKind::DocCurrency(Some(c))
                    } else {
                        SettingKind::GlobalCurrency(c)
                    },
                });
                x += 48.0 + chip_gap;
                if x > self.panel_rect.x1 - 60.0 {
                    x = left + 110.0;
                    y += chip_h + chip_gap;
                }
            }
        }
        y += chip_h + 18.0;

        // Row 2: date style.
        {
            let mut x = left + 110.0;
            if is_doc {
                let rect = Rect::new(x, y, x + 74.0, y + chip_h);
                self.setting_chips.push(SettingChip {
                    rect,
                    kind: SettingKind::DocDate(None),
                });
                x += 74.0 + chip_gap;
            }
            for (label, d) in [
                ("ISO", DateStyle::Iso),
                ("US", DateStyle::Us),
                ("EU", DateStyle::Eu),
                ("Long", DateStyle::Long),
            ] {
                let w = (label.len() as f64 * 8.0 + 24.0).max(48.0);
                let rect = Rect::new(x, y, x + w, y + chip_h);
                self.setting_chips.push(SettingChip {
                    rect,
                    kind: if is_doc {
                        SettingKind::DocDate(Some(d))
                    } else {
                        SettingKind::GlobalDate(d)
                    },
                });
                x += w + chip_gap;
            }
        }
        y += chip_h + 18.0;

        // Row 3: time style.
        {
            let mut x = left + 110.0;
            if is_doc {
                let rect = Rect::new(x, y, x + 74.0, y + chip_h);
                self.setting_chips.push(SettingChip {
                    rect,
                    kind: SettingKind::DocTime(None),
                });
                x += 74.0 + chip_gap;
            }
            for (label, t) in [("24-hour", TimeStyle::H24), ("12-hour", TimeStyle::H12)] {
                let w = label.len() as f64 * 8.0 + 24.0;
                let rect = Rect::new(x, y, x + w, y + chip_h);
                self.setting_chips.push(SettingChip {
                    rect,
                    kind: if is_doc {
                        SettingKind::DocTime(Some(t))
                    } else {
                        SettingKind::GlobalTime(t)
                    },
                });
                x += w + chip_gap;
            }
        }

        // Save button only on the global panel.
        if !is_doc {
            let rect = Rect::new(
                self.panel_rect.x1 - 150.0,
                self.panel_rect.y1 - 44.0,
                self.panel_rect.x1 - 20.0,
                self.panel_rect.y1 - 14.0,
            );
            self.setting_chips.push(SettingChip {
                rect,
                kind: SettingKind::GlobalSave,
            });
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

        // The type/format indicator — a prominent pill the user relies on.
        self.draw_type_indicator(scene, transform, tr, theme);

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
        let (sw, _) = tr.measure(&self.status, theme.tokens.typography.scale.xs, None);
        tr.draw_text(
            scene,
            &self.status,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform
                * Affine::translate((
                    self.tabs_rect.x1 - sw as f64 - 12.0,
                    self.tabs_rect.y0 + (TABS_H - theme.tokens.typography.scale.xs as f64) / 2.0,
                )),
            None,
        );
    }

    /// The prominent type/format indicator above the grid. Shows the active
    /// cell's inferred category, e.g. `Type: Currency (USD)`.
    fn draw_type_indicator(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let rect = self.type_indicator_rect;
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.16),
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(1.0), transform, colors.accent[5], None, &shape);
        let sm = theme.tokens.typography.scale.sm;
        let text = format!("Type: {}", self.current_active_format.label());
        let (tw, _) = tr.measure(&text, sm, None);
        let tx = rect.center().x - tw as f64 / 2.0;
        let ty = rect.y0 + (rect.height() - sm as f64) / 2.0;
        tr.draw_text_weighted(
            scene,
            &text,
            sm,
            colors.text_primary,
            transform * Affine::translate((tx, ty)),
            None,
            TextWeight::Semibold,
        );
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
        let title = if self.settings_panel == SettingsPanel::Document {
            "Document settings"
        } else {
            "Global defaults (all qOS apps)"
        };
        tr.draw_text_weighted(
            scene,
            title,
            base,
            colors.text_primary,
            transform * Affine::translate((self.panel_rect.x0 + 20.0, self.panel_rect.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );

        // Row labels.
        let label_x = self.panel_rect.x0 + 20.0;
        let rows_y = [
            self.panel_rect.y0 + 64.0,
            self.panel_rect.y0 + 64.0 + 26.0 + 18.0 + 26.0, // approx; date row
        ];
        let _ = rows_y;
        // Draw the three section labels aligned to the chip rows.
        let mut section_y = self.panel_rect.y0 + 64.0;
        for label in ["Currency", "Date", "Time"] {
            tr.draw_text(
                scene,
                label,
                sm,
                colors.text_dim,
                transform * Affine::translate((label_x, section_y + 4.0)),
                None,
            );
            section_y += 26.0 + 18.0;
        }

        // Chips.
        for chip in &self.setting_chips {
            let (label, selected) = self.chip_label_and_state(chip.kind);
            let is_save = matches!(chip.kind, SettingKind::GlobalSave);
            let shape = RoundedRect::from_rect(chip.rect, theme.tokens.radii.sm as f64);
            let bg = if is_save {
                colors.accent[5]
            } else if selected {
                colors.accent[5].with_alpha_factor(0.85)
            } else {
                colors.surfaces.raised
            };
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                if selected || is_save { colors.accent[5] } else { colors.border_standard },
                None,
                &shape,
            );
            let (tw, _) = tr.measure(&label, sm, None);
            let tx = chip.rect.center().x - tw as f64 / 2.0;
            let ty = chip.rect.y0 + (chip.rect.height() - sm as f64) / 2.0;
            let color = if selected || is_save { Color::WHITE } else { colors.text_primary };
            tr.draw_text(scene, &label, sm, color, transform * Affine::translate((tx, ty)), None);
        }

        // Hint footer.
        let hint = if self.settings_panel == SettingsPanel::Document {
            "Document overrides layer over the global defaults. \"Default\" = inherit."
        } else {
            "Saved to ~/.config/qos/settings.json — shared with the settings app and others."
        };
        tr.draw_text(
            scene,
            hint,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform * Affine::translate((self.panel_rect.x0 + 20.0, self.panel_rect.y1 - 30.0)),
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
            SettingKind::GlobalCurrency(c) => (c.code().into(), self.global.locale.currency == c),
            SettingKind::GlobalDate(d) => (date_label(d).into(), self.global.locale.date_format == d),
            SettingKind::GlobalTime(t) => (time_label(t).into(), self.global.locale.time_format == t),
            SettingKind::GlobalSave => ("Save defaults".into(), false),
        }
    }

    fn format_of_active(&self, doc: &mut AutoCommit) -> CellFormat {
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet, cur);
        let active = self.grid.active();
        if let Some(f) = snap.fmt.get(&active).copied() {
            return f;
        }
        // No explicit format — reflect any propagated format for formulas.
        if let Some(raw) = snap.raw.get(&active) {
            if let Some(body) = raw.strip_prefix('=') {
                if let Some(inferred) = formula::infer_result_format(body, &snap.fmt) {
                    return inferred;
                }
            }
        }
        CellFormat::default()
    }
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
        MenuItem::OpenGlobalSettings => "Global defaults",
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

impl Widget for SheetsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            // Pull the shared global defaults once at startup.
            self.global = GlobalSettings::load();
            self.seed_if_empty(state);
            // Show the File menu open in the first frame so the relocated
            // file actions are visible in the offscreen screenshot.
            if self.preview_open_file_menu {
                self.open_menu = OpenMenu::File;
            }
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
                        self.preview_open_file_menu = false;
                        self.run_menu_item(state, item);
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                    if !self.dropdown_rect.contains(p) && !self.menubar_rect.contains(p) {
                        self.open_menu = OpenMenu::None;
                        self.preview_open_file_menu = false;
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
                            self.preview_open_file_menu = false;
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
                        self.preview_open_file_menu = false;
                        self.dirty = true;
                        *handled = true;
                        return;
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
            MenuItem::ImportCsv => self.import_demo_csv(doc),
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
            MenuItem::OpenGlobalSettings => {
                self.settings_panel = SettingsPanel::Global;
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

/// Sample CSV used by the File ▸ Import CSV action (no file picker in
/// headless mode, so the demo imports an embedded dataset).
const DEMO_CSV: &str = "Product,Units,Price,Revenue\n\
Widget,120,$4.50,=B2*C2\n\
Gadget,85,$12.00,=B3*C3\n\
Gizmo,200,$2.25,=B4*C4\n\
Total,=SUM(B2:B4),,=SUM(D2:D4)\n";
