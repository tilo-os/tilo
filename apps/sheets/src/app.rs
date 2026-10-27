//! Root widget for the Sheets app. Owns the [`Grid`] and the surrounding
//! chrome: a command bar (File and Format menus, Bold, `AutoSum`, formula
//! view, document settings), a name box and formula bar, and a bottom
//! sheet-tab strip. The bars, the menus and the cards float over a
//! custom-painted cell canvas, each through the toolkit's faces.
//!
//! Data flow: the workbook the resource holds is presented to the app; it
//! reads the active sheet's raw inputs, recalculates every formula via the
//! [`crate::formula`] engine, propagates formats onto computed cells, and
//! hands the resulting values to the grid. `handle_event` routes
//! pointer/keyboard input to whichever region was hit and submits the
//! grid's [`GridCmd`]s — running smart type inference on each entry — as
//! field edits of the cells they write.

use std::collections::HashMap;
use std::path::PathBuf;
use ui::time::Instant;
use ui::widget::TaffyTreeExt;
use ui::widget::paint_damage::PaintParts;
use ui::widget::{AppModel, Edits, ModelPath};

use automerge::ObjId;
use std::rc::Rc;
use taffy::{NodeId, TaffyTree};
use ui::tilo_resource::{Atom, Id, Invoke};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use ui::chart_palette::ChartPalette;
use ui::collaboration::PresencePayload;
use ui::portal::{FileFilter, PickRequest};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::{
    Button, ColorSwatchPicker, ContextMenu, EditOutcome, EditableLabel, MenuItem as Row, Popover,
    PopoverSide, TextInput, Widget,
};

mod annotations;
mod cells;
mod chrome;
mod commands;
mod content_files;
mod editing;
mod exchange;
mod find;
mod input;
mod overlays;
#[cfg(test)]
mod polish_tests;
mod presence;
#[cfg(test)]
mod presence_tests;
mod resource;
mod selection;
mod staging;
mod window_bar;

use chrome::{ChromeMetrics, CommandSlot, MenuRow};
use overlays::SettingsSheet;

use crate::actions;
use crate::fill;
use crate::formula;
use crate::grid::{Grid, GridCmd, GridMetrics};
use crate::infer::infer_entry;
use crate::model::{Addr, CellFormat, CellValue, a1_to_addr, format_value};
use crate::resource::verbs;
use crate::settings::{DocSettings, EffectiveLocale};
use crate::workbook::{Cell, ChartKind, ChartSnapshot, CommentThread, Workbook};
use tilo_locale::{Currency, DateStyle, Locale};
use ui::clock::TimeStyle;

/// Rows of the sheet-tab context menu, top to bottom.
const TAB_MENU_ACTIONS: [TabAction; 6] = [
    TabAction::Rename,
    TabAction::Duplicate,
    TabAction::InsertRight,
    TabAction::MoveLeft,
    TabAction::MoveRight,
    TabAction::Delete,
];
/// Options of the document-settings sheet, in display order.
const DATE_STYLES: [(&str, DateStyle); 5] = [
    ("Language", DateStyle::Language),
    ("ISO", DateStyle::Iso),
    ("US", DateStyle::Us),
    ("EU", DateStyle::Eu),
    ("Long", DateStyle::Long),
];
const TIME_STYLES: [(&str, TimeStyle); 3] = [
    ("Language", TimeStyle::Language),
    ("24-hour", TimeStyle::H24),
    ("12-hour", TimeStyle::H12),
];
/// Importing is deliberately bounded before a parsed workbook can become a
/// durable action stream.  This is large enough for useful real-world sheets,
/// while keeping one user-selected file from monopolising a collaborator's
/// document, history, or search projection.
const MAX_IMPORT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMPORT_CELLS: usize = crate::resource::MAX_IMPORTED_CELLS;
/// CSV's interchange representation is rectangular.  A file with just one
/// populated cell at a hostile far-away coordinate must not turn into a
/// trillion-slot dense `Vec`; this caps the actually materialised rectangle,
/// independently of the populated-cell action limit above.
const MAX_CSV_MATERIALIZED_SLOTS: usize = crate::xlsx::MAX_MATERIALIZED_SLOTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interchange {
    Csv,
    Xlsx,
    Ods,
}

impl Interchange {
    fn ext(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
            Self::Ods => "ods",
        }
    }

    fn filter(self) -> FileFilter {
        match self {
            Self::Csv => FileFilter::new("CSV").glob("*.csv"),
            Self::Xlsx => FileFilter::new("Excel workbook").glob("*.xlsx"),
            Self::Ods => FileFilter::new("Calc spreadsheet").glob("*.ods"),
        }
    }
}

fn spreadsheet_import_request(format: Interchange) -> PickRequest {
    PickRequest::open("Import spreadsheet", "Import")
        .filter(format.filter())
        .all_files()
}

fn spreadsheet_export_request(format: Interchange, suggested: &str) -> PickRequest {
    PickRequest::save("Export spreadsheet", "Export", suggested).filter(format.filter())
}

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
    /// A view-only operation: display raw formulas in the grid instead of
    /// their calculated values. Kept alongside the other painted commands so
    /// narrow layouts do not hide this useful inspection mode behind Ctrl+K.
    ToggleFormulaView,
    /// Make the selection bold, or regular again when it all is.
    ToggleBold,
    // Inline bar action.
    AutoSum,
}

/// Whether the per-document settings panel is open.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SettingsPanel {
    None,
    Document,
}

/// One-shot duties the app still owes the frame loop.
#[derive(Clone, Copy, Default)]
struct Pending {
    /// The scene is stale and needs repainting.
    redraw: bool,
}

/// The part of a screenshot scenario staged against the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Staged {
    CommandPalette,
    FillSeries,
    PasteFormula,
}

/// A sheet to show at the next sync, with one of its cells selected.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingReveal {
    /// The sheet to show; `None` for the last one (an import's, still
    /// arriving).
    sheet_id: Option<Id>,
    addr: Addr,
    /// What to tell the person once it is shown; else where they went.
    status: Option<String>,
    /// The sheet is still being added (an import its dispatcher applies
    /// next): wait for it rather than report it gone.
    arriving: bool,
}

/// A clickable option of the settings sheet.
struct SettingChip {
    rect: Rect,
    kind: SettingKind,
}

/// The four extra parameters `Widget::handle_event` receives that only the
/// command palette's own [`Widget::handle_event`] delegation needs, bundled
/// so [`SheetsApp::handle_keyboard_input`] stays within the argument-count
/// limit.
struct PaletteRouteCtx<'a> {
    path: &'a ObjId,
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

/// Which per-document convention a settings chip sets. `None` means "inherit
/// the global default".
#[derive(Clone, Copy, PartialEq)]
enum SettingKind {
    Currency(Option<Currency>),
    Date(Option<DateStyle>),
    Time(Option<TimeStyle>),
}

/// An action offered by a sheet tab's right-click context menu.
#[derive(Clone, Copy, PartialEq)]
enum TabAction {
    Rename,
    Duplicate,
    InsertRight,
    MoveLeft,
    MoveRight,
    Delete,
    /// Set (or clear, with `None`) the tab's colour to a palette index.
    SetColor(Option<u32>),
}

/// A row of a cell's context menu. The menu names what it will act on (a
/// range, a count) and each row runs one workbook verb, so an insert or a
/// delete of several rows is one undo step.
#[derive(Clone, Copy, PartialEq)]
enum CellMenuRow {
    Cut,
    Copy,
    Paste,
    InsertRowsAbove,
    InsertRowsBelow,
    DeleteRows,
    InsertColumnsLeft,
    InsertColumnsRight,
    DeleteColumns,
    ClearContents,
    Bold,
    Format(CellFormat),
    ClearFormat,
}

/// One match the workbook's find found: which sheet, which cell, and the
/// byte range of the cell's raw input it covers. Search runs over the input
/// (formulas included), which is what replace can edit; the grid shows the
/// cell it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FoundCell {
    sheet: usize,
    addr: Addr,
    start: usize,
    end: usize,
}

#[derive(Clone)]
enum AnnotationClick {
    Open(Addr),
    ManageChart(String),
    EditChart(String),
    SetChartKind(String, ChartKind),
    ChartCells(String),
    DeleteChart(String),
    Reply(String),
    EditComment(String),
    ToggleComment(String, bool),
    DeleteComment(String),
}

/// An in-progress inline rename of a sheet tab.
struct TabRename {
    idx: usize,
    /// The tab's slot: the editor stands exactly on it.
    rect: Rect,
    /// Measured width of the text being typed, so the slot (and the tabs
    /// after it) make room for a longer name instead of the editor covering
    /// its neighbours.
    text_w: f64,
    editor: EditableLabel,
}

/// How many theme-derived tab colours the picker offers. A sheet stores a
/// slot index, not an RGB, so the tab's colour dot follows the live theme.
const TAB_PALETTE_LEN: usize = 6;

/// The concrete colour for a stored tab-colour palette index. Indices wrap
/// around the theme series, so any value a peer stored resolves to a real
/// colour after a theme change.
fn tab_palette_color(theme: &Theme, idx: u32) -> Color {
    ChartPalette::from_theme(theme).categorical(idx as usize)
}

/// Inclusive count of grid indices from `lo` through `hi`. Every caller passes
/// ordered selection bounds, so the saturation only guards the degenerate
/// whole-axis span rather than wrapping it.
/// Whether `id` is an opaque participant id (a long hex key) rather than a
/// name someone could read.
fn opaque_participant_id(id: &str) -> bool {
    id.len() >= 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn span(lo: u32, hi: u32) -> u32 {
    hi.saturating_sub(lo).saturating_add(1)
}

/// The signed offset that moves grid index `from` onto `to`. Both are `u32`,
/// so the difference always fits in `i64` and nothing is ever clamped.
fn delta(from: u32, to: u32) -> i64 {
    i64::from(to).saturating_sub(i64::from(from))
}

/// A grid address `dc` columns and `dr` rows on from `base`, clamped at the
/// last addressable cell rather than wrapping past it.
fn offset_addr(base: Addr, dc: u32, dr: u32) -> Addr {
    (base.0.saturating_add(dc), base.1.saturating_add(dr))
}

/// How many whole copies of a `block`-wide span fit into `sel`, when `sel` is
/// an exact multiple of it — Excel's "paste into a range" tiling. Anything
/// else (including an empty block) places the block exactly once.
fn tile_count(sel: u32, block: u32) -> u32 {
    if block == 0 || sel <= block {
        return 1;
    }
    match (sel.checked_rem(block), sel.checked_div(block)) {
        (Some(0), Some(n)) => n,
        _ => 1,
    }
}

fn tab_action_label(act: TabAction) -> &'static str {
    match act {
        TabAction::Rename => "Rename",
        TabAction::Duplicate => "Duplicate",
        TabAction::InsertRight => "Insert sheet",
        TabAction::MoveLeft => "Move left",
        TabAction::MoveRight => "Move right",
        TabAction::Delete => "Delete",
        TabAction::SetColor(_) => "Colour",
    }
}

pub struct SheetsApp {
    grid: Grid,
    /// The custom chrome has no toolkit layout primitives to inherit type
    /// sizing from, so keep its touch targets and modal spacing aligned with
    /// the current accessibility text preference.
    text_scale: f64,
    rect: Rect,
    formula_rect: Rect,
    grid_rect: Rect,
    tabs_rect: Rect,
    /// What the window bar's commands last ran ([`window_bar`]).
    command_slot: CommandSlot,
    /// The window bar's slot ids, and the command each runs.
    bar_commands: Vec<(String, MenuItem)>,
    /// The file the workbook was opened from, which names it; `None` for
    /// one never saved.
    file: Option<PathBuf>,
    /// The theme the chrome was last measured in, for laying the bar out
    /// between frames.
    chrome_theme: Theme,
    /// The document-settings sheet as last laid out.
    settings_sheet: Option<SettingsSheet>,
    /// Measured chrome text metrics driving `relayout` (see [`ChromeMetrics`]).
    metrics: ChromeMetrics,
    tab_rects: Vec<(Rect, usize)>,
    add_tab_rect: Rect,
    /// Per-sheet tab colours (palette indices), refreshed on each reload.
    sheet_colors: Vec<Option<u32>>,
    /// Right-click menu on a sheet tab: the target sheet, the menu and what
    /// each of its rows does (`None` for a separator), and the colour popover
    /// its "Tab colour" row opens.
    tab_menu_target: Option<usize>,
    tab_menu: ContextMenu,
    tab_menu_rows: Vec<Option<overlays::TabMenuRow>>,
    tab_colour: Popover<ColorSwatchPicker>,
    /// The tab and colour the popover's picker was last built for.
    tab_colour_shown: Option<(usize, Option<usize>)>,
    /// Right-click menu on a cell: the menu, what each row runs, and the
    /// cell that was under the pointer when it opened.
    cell_menu: ContextMenu,
    cell_menu_rows: Vec<Option<CellMenuRow>>,
    cell_menu_target: Option<Addr>,
    /// The window bar's search over the workbook: the matches it found and
    /// the one shown. This app is the `Findable` the bar drives; it draws no
    /// find field of its own.
    find_matches: Vec<FoundCell>,
    find_current: Option<usize>,
    /// A replacement the bar asked for, waiting for the next frame's edit
    /// path: `Findable::replace` has no document handle.
    pending_find_replace: Vec<(usize, Addr, String)>,
    /// The window's size changed: the next sync keeps the active cell in
    /// view once the grid's new metrics and data are in.
    pending_ensure_visible: bool,
    /// Whether the active cell is bold, for the Bold command's toggle state.
    active_cell_bold: bool,
    /// A screenshot scenario's menu or sheet, waiting for the first measured
    /// frame (see [`SheetsApp::stage_scenario`]).
    staged_overlay: Option<String>,
    /// An inline sheet-tab rename editor, when active.
    tab_rename: Option<TabRename>,
    /// The name box (cell-reference / Go-To field) hit-rect, and the "fx"
    /// formula-insert affordance's hit-rect, both to the left of the formula
    /// field. Derived in `relayout` so `handle_event` can route clicks to them.
    name_box_rect: Rect,
    fx_rect: Rect,
    /// An active Name box Go-To entry, when the user has clicked the name box.
    name_box_edit: Option<TextInput>,
    formula_field_rect: Rect,
    /// Cumulative x (layout space) of each character boundary in the formula
    /// bar's current text, rebuilt each draw. Drives click-to-position-caret,
    /// drag-select, and the bar's caret/selection rendering. `formula_char_x[i]`
    /// is the x of the boundary before char `i`; the last entry is the end.
    formula_char_x: Vec<f64>,
    /// Horizontal pan of the formula editor, keeping its caret inside the field.
    formula_scroll_x: f64,
    /// Whether a drag that began in the formula bar is selecting text.
    formula_bar_selecting: bool,
    sheet_names: Vec<String>,
    /// Durable chart/comment records for the active sheet; refreshed together
    /// with its formula snapshot, never created as an app-local side store.
    sheet_charts: Vec<ChartSnapshot>,
    sheet_comments: Vec<CommentThread>,
    /// Rebuilt on paint: annotation cards stay a visual projection of the
    /// shared records, while their click routes dispatch canonical actions.
    annotation_clicks: Vec<(Rect, AnnotationClick)>,
    /// Only this chart exposes its management controls; plots are uncluttered otherwise.
    managed_chart: Option<String>,
    /// The strip under the grid that holds the annotation cards, and where
    /// each card stands in it. Empty when the sheet has no annotations.
    dock_rect: Rect,
    dock_cards: Vec<(annotations::DockCard, Rect)>,
    /// Scrolls the dock sideways to the cards past its edge.
    dock_scroll: ui::widget::ScrollRegion,
    /// It is refreshed only with a new search projection, never per query.
    /// Each tab's durable sheet identity, which the edits it offers name.
    sheet_ids: Vec<Id>,
    /// The workbook as the resource holds it now.
    workbook: Rc<Workbook>,
    /// Increases whenever `workbook` is another state.
    revision: u64,
    /// The resource's live state, read again right after an own edit.
    live: Option<ui::framework::Live<Workbook>>,
    active_sheet: usize,
    /// The latest transient action message (e.g. "Pasted into B2"), said in
    /// the window's bar ([`status_notice`](Self::status_notice)) rather than
    /// a permanent status line. Initialised to `"Ready"`, which counts as
    /// "nothing to say".
    status: String,
    /// The selection summary or the active cell's format, or a fresh action
    /// message, said in the window's bar for a while when that text changes
    /// (a selection move/extend, an action), keeping the sheet-tab strip
    /// free of status text so it reads purely as the sheet switcher.
    status_notice: ui::widget::toast::Transient<String>,
    /// The status last said in the bar, so only a change is news.
    status_said: String,
    /// The last frame's drawing, part by part, so a frame reports where it
    /// differs from it.
    paint_damage: ui::widget::paint_damage::PaintDamage,
    /// The part of a screenshot scenario staged against the document, done
    /// by the `sync` that follows the staging.
    staged: Option<Staged>,
    /// One-shot frame duties: repaint.
    pending: Pending,
    last_press: Option<(Instant, Addr)>,
    last_mouse: Point,
    current_active_format: CellFormat,
    settings_panel: SettingsPanel,
    /// The person's locale defaults, as of the last `sync`.
    global: Locale,
    /// The shared `locale` setting, followed while the app runs.
    locale: Option<ui::framework::Setting<Locale>>,
    doc_settings: DocSettings,
    /// Memoised recalculation: `reload` recomputes only when the document
    /// heads or active sheet change, so selection/scroll frames are cheap.
    cache: Option<ReloadCache>,
    pending_reveal: Option<PendingReveal>,
    /// The last copied/cut rectangle, kept so an in-app paste can restore raw
    /// inputs and formats exactly (and shift relative formula references),
    /// beyond the plain-text payload other apps see on the system clipboard.
    clipboard: Option<ClipboardGrid>,
    /// The Ctrl+K command palette overlay. Seeded with the app's action set
    /// each time it opens; closed by default.
    palette: CommandPalette,
    /// The workbook's dispatcher: every edit is a field edit or a verb
    /// submitted here, checked, applied, undone and replayed like the
    /// palette's and an agent's. `None` until the framework attaches the
    /// surface.
    dispatch: Option<ui::framework::Dispatcher<Workbook>>,
    /// The store the host lends the window: workbooks imported from and
    /// exported as content are read and kept there.
    content: Option<ui::framework::content::ContentStore>,
    /// The participants' names, for the authors comments record.
    people: Option<ui::framework::People>,
    /// The search hit or `open` waiting for the next settle to show it.
    opening: Rc<std::cell::RefCell<Option<PendingReveal>>>,
    /// The edits last bound to the selection, with what they were built
    /// from: every publication asks for them, and they change only with the
    /// workbook, the selection or the locale.
    selection_decls:
        std::cell::RefCell<Option<(SelectionKey, Vec<ui::tilo_resource::ActionDecl<Workbook>>)>>,
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
/// sheet or re-run the formula engine.
struct ReloadCache {
    revision: u64,
    sheet: usize,
    /// Effective per-cell format (explicit format, else propagated formula
    /// unit). Drives the type indicator and the selection summary.
    eff_fmt: HashMap<Addr, CellFormat>,
    /// Computed cell values, reused for the live selection summary.
    values: HashMap<Addr, CellValue>,
}

impl Default for SheetsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SheetsApp {}

/// A sentence saying why a cell holding `raw` evaluates to `err`. For a
/// formula that could not be read, it names the most likely mistake.
fn explain_formula_error(raw: &str, err: crate::model::FormulaError) -> String {
    use crate::model::FormulaError;
    match err {
        FormulaError::Error => {
            let body = raw.trim_start_matches('=').trim_end();
            let opens = body.matches('(').count();
            let closes = body.matches(')').count();
            if body.is_empty() {
                "The formula is empty: type a value or reference after =".into()
            } else if body.ends_with(['+', '-', '*', '/', '^', '&', '=', '<', '>', ',', ';']) {
                "The formula ends with an operator: add a value or reference after it".into()
            } else if opens > closes {
                "A parenthesis is opened but not closed".into()
            } else if closes > opens {
                "A closing parenthesis has no matching opening one".into()
            } else if body.matches('"').count() % 2 == 1 {
                "A text value is missing its closing quote".into()
            } else {
                "The formula could not be read: check for a missing value, operator or parenthesis"
                    .into()
            }
        }
        FormulaError::Div0 => "Division by zero or by an empty cell".into(),
        FormulaError::Name => "Unknown function or name".into(),
        FormulaError::Ref => "The formula refers to a cell that does not exist".into(),
        FormulaError::Value => {
            "A value has the wrong type for this operation, such as text in arithmetic".into()
        }
        FormulaError::Na => "No value is available, such as a lookup that found nothing".into(),
        FormulaError::Num => "The result is not a valid number".into(),
        FormulaError::Cycle => "The formula depends on its own cell (circular reference)".into(),
    }
}

/// A numeric summary of the selection shown in the status bar.
struct SelectionSummary {
    sum: f64,
    avg: f64,
    count: usize,
    fmt: CellFormat,
}

fn date_label(d: DateStyle) -> &'static str {
    match d {
        DateStyle::Language => "Language",
        DateStyle::Iso => "ISO",
        DateStyle::Us => "US",
        DateStyle::Eu => "EU",
        DateStyle::Long => "Long",
    }
}
fn time_label(t: TimeStyle) -> &'static str {
    match t {
        TimeStyle::Language => "Language",
        TimeStyle::H24 => "24-hour",
        TimeStyle::H12 => "12-hour",
    }
}

/// Flatten a cell's display text into a single TSV field: tabs and newlines
/// would break the row/column structure, so they collapse to spaces.
fn tsv_cell(display: &str) -> String {
    display.replace(['\t', '\n', '\r'], " ")
}

/// A new sheet name derived from an imported file's stem.
fn sheet_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(std::string::ToString::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported".to_string())
}

/// How an imported sheet remembers its file: the resolved path, so every
/// spelling of one file finds the sheets it already became.
/// The durable identity of the file at `path`: the same file however it
/// is reached, by path or through the chooser.
fn import_source(path: &std::path::Path) -> String {
    ui::record::RecordId::identify_file(path).to_string()
}

/// Spreadsheet references never contain whitespace. Keep every other
/// printable character so submission can report a useful invalid-reference
/// error instead of silently rewriting the user's entry.
fn reference_box_char(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control()
}

/// Parse a Name box (Go-To) entry into an `(anchor, active)` selection. Accepts
/// a single reference (`B7`) or a range (`A1:C3`), case-insensitive, tolerating
/// absolute markers (`$A$1`). Returns `None` for anything that isn't a valid
/// reference or `ref:ref` range.
fn parse_ref_target(s: &str) -> Option<(Addr, Addr)> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once(':') {
        let a = a1_to_addr(a.trim())?;
        let b = a1_to_addr(b.trim())?;
        Some((a, b))
    } else {
        let a = a1_to_addr(s)?;
        Some((a, a))
    }
}

/// A short, user-facing label for a path (its file name) for status messages.
fn file_label(path: &std::path::Path) -> String {
    path.file_name().and_then(|s| s.to_str()).map_or_else(
        || path.display().to_string(),
        std::string::ToString::to_string,
    )
}

impl Widget for SheetsApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::Findable,
        dyn ui::presence::capability::PublishesPresence
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // Outside a host (a unit test) nobody else keeps the state current.
        let (doc, _) = ui::doc::document_ref(state, path);
        if let Some(live) = &self.live
            && live.sync(doc).is_ok()
        {
            let state = live.get();
            self.adopt_resource_state(&state);
        }
        if let Some(locale) = &self.locale {
            self.global = locale.get();
        }
        self.reload();
        if self.pending_ensure_visible {
            self.pending_ensure_visible = false;
            self.grid.ensure_active_visible();
        }
        self.current_active_format = self.format_of_active();
        self.active_cell_bold = self.grid.cell_is_bold(self.grid.active());
        // Say the composed label in the window's bar when it changes — a
        // selection move/extend, or a fresh action message; the idle
        // nothing state (empty label) never surfaces.
        let label = self.status_label().unwrap_or_default();
        if label != self.status_said {
            if label.is_empty() {
                self.status_notice.dismiss();
            } else {
                self.status_notice.show(label.clone());
            }
            self.status_said = label;
        }
        self.status_notice.expire();
        self.status_notice.publish(|text| {
            ui::window_toolbar::ToolbarNotice::new(text.clone(), ui::widget::toast::ToastKind::Info)
        });
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Unlike toolkit tables, this virtualised canvas owns its geometry.
        // Keep its row/header metrics in step with accessibility typography
        // before the subsequent layout + hit-test pass.
        self.text_scale = f64::from(theme.text_scale);
        self.grid.set_text_scale(self.text_scale);

        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let area = ui::widget::layout_rect(taffy, node, origin);
        if area != self.rect {
            self.pending_ensure_visible = true;
        }
        self.rect = area;
        self.relayout();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.annotation_clicks.clear();
        // Measure first, so this frame's chrome (and the hit-rects it leaves
        // behind) is laid out from the renderer that is about to paint it.
        self.refresh_chrome_metrics(text_renderer, theme);
        // A resize that arrived without a sync still keeps the active cell in
        // view: the draw is the first moment the new metrics are settled.
        if self.pending_ensure_visible {
            self.pending_ensure_visible = false;
            self.grid.ensure_active_visible();
        }
        // A staged menu or sheet opens once the chrome is measured, so it
        // anchors to what is painted (and the More menu knows whether the bar
        // overflowed).
        self.apply_staged_overlay();
        // Each region is a part of its own, so the frame reports as damage
        // the regions whose drawing changed: a typed character repaints the
        // editor and the formula bar, not the window.
        let window = self.rect;
        let tr = text_renderer;
        let mut parts = PaintParts::new(scene);
        self.draw_chrome(&mut parts, scene, transform, tr, theme, presence);
        self.grid.draw(&mut parts, scene, transform, tr, theme);
        parts.draw(scene, "sheet-find-matches", self.grid_rect, |scene| {
            let body = self.grid.body_rect();
            for (index, found) in self.find_matches.iter().enumerate() {
                if found.sheet != self.active_sheet {
                    continue;
                }
                let cell = self.grid.cell_screen_rect(found.addr.0, found.addr.1);
                let visible = cell.intersect(body);
                if visible.area() > 0.0 {
                    ui::widget::paint_find_match(
                        scene,
                        transform,
                        visible,
                        self.find_current == Some(index),
                        theme,
                    );
                }
            }
        });
        parts.draw(scene, "sheet-comment-markers", window, |scene| {
            self.draw_comment_markers(scene, transform, theme);
        });
        parts.draw(scene, "sheet-cell-presence", window, |scene| {
            self.draw_cell_presence(scene, transform, tr, theme, presence);
        });
        parts.draw(scene, "sheet-annotations", window, |scene| {
            self.draw_annotation_dock(scene, transform, tr, theme);
        });
        // The open comment floats over the cells beside its own.
        parts.draw(scene, "sheet-comment", window, |scene| {
            self.draw_comment_popover(scene, transform, tr, theme);
        });
        parts.draw(scene, "sheet-tab-rename", window, |scene| {
            self.draw_tab_rename(scene, transform, tr, theme);
        });
        // Presence-in-navigation: when a peer is on a DIFFERENT sheet,
        // glow that sheet's tab in their colour so you can see where they
        // went ("Alice is on Q2"). Painted after the chrome so it reads on
        // top of the tab surface.
        parts.draw(scene, "sheet-tab-presence", window, |scene| {
            self.draw_sheet_tab_presence(scene, transform, theme, presence);
        });
        self.paint_damage
            .record(window, parts.finish(scene, vec![window], transform, theme));
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint_overlays(scene, transform, text_renderer, theme, presence);
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.palette.set_motion_reduced(reduced);
    }

    fn needs_animation_frame(&self) -> bool {
        self.dock_scroll.needs_animation_frame(&self.chrome_theme)
            || self.children().iter().any(|c| c.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(self.status_notice.deadline())
            .min()
    }

    /// The annotation dock glides on after a sideways flick, as the wheel
    /// scrolls it; that changes no model.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        if self.dock_scroll.advance() {
            self.pending.redraw = true;
        }
        let mut changed = false;
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn settle(&mut self, edits: &mut Edits<'_>, _path: &ObjId) {
        // What the window's search asked to replace goes through the
        // workbook's own edit path, with the document in hand.
        self.drain_find_replace(edits);
        self.apply_window_toolbar(edits);
        if let Some(opening) = self.opening.borrow_mut().take() {
            self.pending_reveal = Some(opening);
        }
        self.follow(edits);
        if let Some(reveal) = self.pending_reveal.take() {
            let idx = match &reveal.sheet_id {
                Some(id) => self.sheet_ids.iter().position(|sheet| sheet == id),
                None => self.sheet_ids.len().checked_sub(1),
            };
            if let Some(idx) = idx {
                self.switch_sheet(edits, idx);
                self.grid.select_only(reveal.addr);
                self.status = reveal.status.unwrap_or_else(|| {
                    format!(
                        "Went to {}!{}",
                        self.sheet_names.get(idx).cloned().unwrap_or_default(),
                        crate::model::addr_to_a1(reveal.addr.0, reveal.addr.1)
                    )
                });
            } else if reveal.arriving {
                self.pending_reveal = Some(reveal);
            } else {
                self.status = "That sheet no longer exists".into();
            }
        }
        self.reload();
        self.finish_staging(edits);
    }

    fn handle_event(&mut self, event: &WindowEvent, event_ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = event_ctx.logical(position);
        }
        // Find and replace open the window bar's search, which owns the
        // field and the keys: the sheet's matches are painted from its
        // find, never typed into a field of the sheet's own.
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            let ctrl = event_ctx.modifiers.state().control_key()
                || event_ctx.modifiers.state().super_key();
            let shift = event_ctx.modifiers.state().shift_key();
            if key.state == ElementState::Pressed
                && ctrl
                && !self.palette.is_open()
                && self.settings_panel == SettingsPanel::None
                && self.name_box_edit.is_none()
                && self.tab_rename.is_none()
                && let winit::keyboard::Key::Character(character) = &key.logical_key
            {
                if character.eq_ignore_ascii_case("f") && !shift {
                    self.open_find(ui::window_toolbar::SearchOpening::Find);
                    *event_ctx.handled = true;
                    return;
                }
                if character.eq_ignore_ascii_case("h") {
                    self.open_find(ui::window_toolbar::SearchOpening::Replace);
                    *event_ctx.handled = true;
                    return;
                }
            }
        }
        // A menu or popover over the sheet is modal and takes the event first.
        if self.route_overlays(event, event_ctx) {
            return;
        }
        if self.route_dock_scroll(event, event_ctx) {
            return;
        }
        if self.route_inline_editor(
            event,
            event_ctx.scale_factor,
            *event_ctx.modifiers,
            &mut event_ctx.state,
            event_ctx.handled,
            &mut PaletteRouteCtx {
                path: event_ctx.path,
                focus_target: event_ctx.focus_target,
                node_id: event_ctx.node_id,
                taffy: event_ctx.taffy,
            },
        ) {
            return;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press(
                    *event_ctx.modifiers,
                    &mut event_ctx.state,
                    event_ctx.path,
                    event_ctx.handled,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.grid.pointer_up();
                // A completed fill-handle drag commits its expansion here.
                self.run_pending_fill(&mut event_ctx.state);
                self.formula_bar_selecting = false;
                self.pending.redraw = true;
                *event_ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.handle_right_press(event_ctx.handled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(
                    position.x,
                    position.y,
                    event_ctx.scale_factor,
                    event_ctx.handled,
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, event_ctx.scale_factor, event_ctx.handled);
            }
            WindowEvent::Ime(ime) if !self.palette.is_open() => {
                if self.grid.ime(ime) {
                    self.pending.redraw = true;
                    *event_ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { .. } => {
                let mut ctx = PaletteRouteCtx {
                    path: event_ctx.path,
                    focus_target: event_ctx.focus_target,
                    node_id: event_ctx.node_id,
                    taffy: event_ctx.taffy,
                };
                self.handle_keyboard_input(
                    event,
                    event_ctx.scale_factor,
                    *event_ctx.modifiers,
                    &mut event_ctx.state,
                    event_ctx.handled,
                    &mut ctx,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SheetsApp"
    }

    /// The grid owns Tab, as every spreadsheet's does: it commits the cell
    /// and moves right (Shift+Tab left) instead of handing focus to the next
    /// control. An open palette, settings sheet or name-box edit leaves Tab
    /// to the host's focus order.
    fn wants_tab_input(&self) -> bool {
        !self.palette.is_open()
            && self.settings_panel == SettingsPanel::None
            && self.name_box_edit.is_none()
    }

    /// A cell being edited takes input-method composition.
    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.grid
            .is_editing()
            .then_some(ui::ime::InputContent::TEXT)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.grid.ime_cell_rect()
    }

    /// The worksheet is a custom-painted cell canvas, so surface the active
    /// cell, current selection and sheet tabs rather than exporting an opaque
    /// drawing surface to assistive technology.
    #[allow(
        clippy::too_many_lines,
        reason = "the complete custom-canvas semantic tree stays co-located with the widget contract so its virtual bounds and interaction state remain auditable"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let active = self.grid.active();
        let active_name = crate::model::addr_to_a1(active.0, active.1);
        // The formula bar; the commands are the window bar's.
        let toolbar_controls: Vec<AccessNode> = [
            AccessNode::new(Role::Button)
                .with_name("Start a formula")
                .with_description("Begin a formula in the active cell")
                .with_action_target("sheets.fx")
                .with_bounds(self.fx_rect)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::TextInput)
                .with_name("Name box")
                .with_description("Go to a cell or range; shortcut Control+G")
                .with_value(self.grid.selection_label())
                .with_action_target("sheets.name-box")
                .with_bounds(self.name_box_rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.name_box_edit.is_some()),
                ),
            AccessNode::new(Role::TextInput)
                .with_name("Formula bar")
                .with_description("Edit the active cell's value or formula")
                .with_value(self.grid.active_display_raw())
                .with_action_target("sheets.formula-bar")
                .with_bounds(self.formula_field_rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.grid.is_editing()),
                ),
            AccessNode::new(Role::Button)
                .with_name("Add sheet")
                .with_action_target("sheets.new-sheet")
                .with_bounds(self.add_tab_rect)
                .with_state(AccessState::new().focusable(true)),
        ]
        .into();
        let tabs = self
            .sheet_names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let bounds = self
                    .tab_rects
                    .iter()
                    .find(|(_, candidate)| *candidate == index)
                    .map(|(rect, _)| *rect);
                AccessNode::new(Role::Tab)
                    .with_name(name.clone())
                    .with_position(index + 1, self.sheet_names.len())
                    .with_action_target(format!(
                        "sheets.tab:{}",
                        self.sheet_ids
                            .get(index)
                            .map(Id::as_str)
                            .unwrap_or_default()
                    ))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(index == self.active_sheet),
                    )
                    .with_bounds(bounds.unwrap_or_default())
            })
            .collect();
        // The painted grid is virtualized, so its semantic tree must be too:
        // expose every currently visible row/cell (plus a bounded pre-layout
        // window), never the workbook's full declared million-row extent.
        // Row/column positions retain the complete-table context for readers.
        let window = self.grid.accessibility_window();
        let header_cells = window
            .columns
            .iter()
            .map(|&column| {
                let letter = crate::model::col_to_letters(column);
                AccessNode::new(Role::Cell)
                    .with_name(format!("Column {letter}"))
                    .with_value(letter)
                    .with_position(column as usize + 1, window.total_columns as usize)
            })
            .collect();
        let mut rows = Vec::with_capacity(window.rows.len().saturating_add(1));
        rows.push(
            AccessNode::new(Role::Row)
                .with_name("Column headers")
                // The virtual table's data rows are keyed by their durable
                // row coordinate. Give the fixed header row its own reserved
                // key too: mixing keyed and positional synthetic siblings
                // makes AccessKit node identity shift as the viewport moves.
                .with_key(u64::MAX)
                .with_children(header_cells),
        );
        rows.extend(window.rows.into_iter().map(|row| {
            let selected = row.cells.iter().any(|cell| cell.selected);
            let cells = row
                .cells
                .into_iter()
                .map(|cell| {
                    let reference = crate::model::addr_to_a1(cell.addr.0, cell.addr.1);
                    let column = crate::model::col_to_letters(cell.addr.0);
                    let value = if cell.raw.is_empty() || cell.display == cell.raw {
                        cell.display
                    } else {
                        format!("{} (input: {})", cell.display, cell.raw)
                    };
                    AccessNode::new(Role::Cell)
                        .with_name(reference)
                        .with_description(format!("Column {column}, row {}", cell.addr.1 + 1))
                        .with_optional_value((!value.is_empty()).then_some(value))
                        .with_position(cell.addr.0 as usize + 1, window.total_columns as usize)
                        .with_bounds(cell.bounds)
                        .with_state(
                            AccessState::new()
                                .selected(cell.selected)
                                .focused(cell.addr == active),
                        )
                })
                .collect();
            AccessNode::new(Role::Row)
                .with_name(format!("Row {}", row.index + 1))
                .with_position(row.index as usize + 1, window.total_rows as usize)
                .with_state(AccessState::new().selected(selected))
                .with_key(u64::from(row.index))
                .with_bounds(row.bounds)
                .with_children(cells)
        }));
        let sheet_name = self
            .sheet_names
            .get(self.active_sheet)
            .cloned()
            .unwrap_or_else(|| "Worksheet".into());
        let sheet = AccessNode::new(Role::Table)
            .with_name(sheet_name)
            .with_description(format!(
                "Worksheet; selected range {}; {} columns by {} rows; only the visible grid window is announced",
                self.grid.selection_label(),
                window.total_columns,
                window.total_rows,
            ))
            .with_value(format!("Active cell {active_name}"))
            .with_bounds(window.bounds)
            .with_state(AccessState::new().focusable(true))
            .with_children(rows);
        let annotations = AccessNode::new(Role::List)
            .with_name("Comments and charts")
            .with_children(
                self.sheet_charts
                    .iter()
                    .map(|chart| {
                        let target = format!("sheets.annotation:chart:{}", chart.id);
                        AccessNode::new(Role::ListItem)
                            .with_name(format!("{} chart", chart.title))
                            .with_description(format!(
                                "{}:{}; open, rename, change its type or cells, or delete from the chart card",
                                crate::model::addr_to_a1(chart.start.0, chart.start.1),
                                crate::model::addr_to_a1(chart.end.0, chart.end.1)
                            ))
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name("Manage chart")
                                    .with_action_target(format!("{target}:manage"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Open chart range")
                                    .with_action_target(format!("{target}:open"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Rename chart")
                                    .with_value(chart.title.clone())
                                    .with_action_target(format!("{target}:edit"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Column chart")
                                    .with_action_target(format!("{target}:column"))
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .selected(chart.kind == ChartKind::Column),
                                    ),
                                AccessNode::new(Role::Button)
                                    .with_name("Line chart")
                                    .with_action_target(format!("{target}:line"))
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .selected(chart.kind == ChartKind::Line),
                                    ),
                                AccessNode::new(Role::Button)
                                    .with_name("Chart cells")
                                    .with_value(format!(
                                        "{}:{}",
                                        crate::model::addr_to_a1(chart.start.0, chart.start.1),
                                        crate::model::addr_to_a1(chart.end.0, chart.end.1),
                                    ))
                                    .with_action_target(format!("{target}:cells"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Delete chart")
                                    .with_action_target(format!("{target}:delete"))
                                    .with_state(AccessState::new().focusable(true)),
                            ])
                    })
                    .chain(self.sheet_comments.iter().map(|thread| {
                        let target = format!("sheets.annotation:comment:{}", thread.id);
                        AccessNode::new(Role::ListItem)
                            .with_name(format!("Comment by {}", self.author_name(&thread.author)))
                            .with_description(format!(
                                "{}; {} replies; {}",
                                thread.body,
                                thread.replies.len(),
                                if thread.resolved { "resolved" } else { "open" }
                            ))
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name("Reply to comment")
                                    .with_action_target(format!("{target}:reply"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Edit comment")
                                    .with_value(thread.body.clone())
                                    .with_action_target(format!("{target}:edit"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Open comment anchor")
                                    .with_action_target(format!("{target}:open"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Delete comment")
                                    .with_action_target(format!("{target}:delete"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(if thread.resolved {
                                        "Reopen comment"
                                    } else {
                                        "Resolve comment"
                                    })
                                    .with_action_target(format!("{target}:toggle"))
                                    .with_state(AccessState::new().focusable(true)),
                            ])
                    }))
                    .collect(),
            );
        let children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Spreadsheet actions")
                .with_children(toolbar_controls),
            AccessNode::new(Role::TabBar)
                .with_name("Sheets")
                .with_children(tabs),
            sheet,
            annotations,
        ];
        Some(
            AccessNode::new(Role::Document)
                .with_name("Spreadsheet")
                .with_description("Editable workbook with formula bar and sheet tabs")
                .with_state(AccessState::new().focusable(true))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let activates = matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus);
        if activates {
            match target {
                "sheets.fx" => {
                    self.start_formula_entry();
                    return true;
                }
                "sheets.name-box" => {
                    self.begin_name_box_edit();
                    return true;
                }
                "sheets.formula-bar" => {
                    self.grid.begin_edit_in_place();
                    self.pending.redraw = true;
                    return true;
                }
                "sheets.new-sheet" => {
                    self.add_sheet(state);
                    return true;
                }
                _ => {}
            }
            if let Some(sheet_id) = target.strip_prefix("sheets.tab:")
                && let Some(index) = self.sheet_ids.iter().position(|id| id.as_str() == sheet_id)
            {
                self.switch_sheet(state, index);
                return true;
            }
        }
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(annotation) = self.annotation_click_from_target(target) else {
            return false;
        };
        self.apply_annotation_click(annotation, state);
        true
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.pending.redraw || self.grid.dirty()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.rect, regions);
        }
    }

    fn damage_covers_own_box(&self) -> bool {
        self.paint_damage.covers_own_bounds()
    }

    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }

    fn mark_drawn(&mut self) {
        self.pending.redraw = false;
        self.grid.mark_drawn();
        self.paint_damage.mark_drawn();
    }

    /// The grid fills its node and places everything in it in
    /// `apply_layout`: there is never a size for the layout solver to
    /// measure, so a frame that only moves the window (a strip pan) solves
    /// none.
    fn layout_needs_measure(&self) -> bool {
        false
    }
}

impl ui::presence::capability::PublishesPresence for SheetsApp {
    /// Publish durable cell anchors so a peer's selection follows inserted
    /// rows and columns. Empty, not-yet-authored cells retain tab presence.
    fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
        let key = self.cell_presence_key().or_else(|| {
            self.sheet_names
                .get(self.active_sheet)
                .map(|name| PresenceKey::Tab { id: name.clone() })
        })?;
        Some((key, PresencePayload::Arrived))
    }
}

impl ui::widget::controls::ActionControls for SheetsApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.palette.take_control_invocation()
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        use ui::widget::Widget as _;
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        if self.palette.is_open() {
            bindings.extend(self.palette.control_action_bindings());
        }
        bindings
    }
}

impl SheetsApp {}

impl SheetsApp {}

#[cfg(test)]
#[path = "app/r2_annotation_cards.rs"]
mod r2_annotation_cards;

#[cfg(test)]
mod interaction_tests;

#[cfg(test)]
mod entry_tests;

#[cfg(test)]
mod window_bar_tests;

#[cfg(test)]
pub(crate) mod tests;

/// What the edits bound to the selection are built from.
#[derive(PartialEq)]
struct SelectionKey {
    revision: u64,
    sheet: usize,
    bounds: (Addr, Addr),
    active: Addr,
    selected: Vec<Addr>,
    currency: tilo_locale::Currency,
}
