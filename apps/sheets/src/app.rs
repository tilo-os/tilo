//! Root widget for the Sheets app. Owns the [`Grid`] and the surrounding
//! chrome — a compact menu bar (File / Format menus + `AutoSum`), a
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
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use ui::ColorCompat;
use ui::collaboration::PresencePayload;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::{EditOutcome, EditableLabel, TextInput, Widget};

use crate::actions::{self, SheetsAction, SheetsResourceCtx};
use crate::doc::SheetsDoc;
use crate::fill;
use crate::formula;
use crate::grid::{Grid, GridCmd};
use crate::infer::infer_entry;
use crate::model::{
    Addr, CellFormat, CellValue, Currency, DateStyle, FormatCategory, TimeStyle, a1_to_addr,
    format_value,
};
use crate::settings::{DocSettings, EffectiveLocale, GlobalSettings};

const MENUBAR_H: f64 = 44.0;
const FORMULA_H: f64 = 34.0;
const TABS_H: f64 = 34.0;
const NAME_BOX_W: f64 = 92.0;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingIo {
    Import(Interchange),
    Export(Interchange),
}

fn spreadsheet_import_request(format: Interchange) -> PickRequest {
    PickRequest::open("Import spreadsheet", "Import")
        .thread_name("sheets-portal-import")
        .filter(format.filter())
        .all_files()
}

fn spreadsheet_export_request(format: Interchange, suggested: &str) -> PickRequest {
    PickRequest::save("Export spreadsheet", "Export", suggested)
        .thread_name("sheets-portal-export")
        .filter(format.filter())
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

/// How this instance of the app is running.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// A real launch: opens blank and persists the workbook between sessions.
    Live,
    /// The headless `--shots`/test path: seeds the sample budget so a captured
    /// frame shows real formulas, and never reads or writes the user's data.
    Shots,
}

impl RunMode {
    /// Whether the workbook is read from and written to disk.
    fn persists(self) -> bool {
        self == Self::Live
    }

    /// Whether an empty workbook is filled with the sample budget.
    fn seeds_sample_data(self) -> bool {
        self == Self::Shots
    }
}

/// One-shot duties the app still owes the frame loop.
#[derive(Clone, Copy, Default)]
struct Pending {
    /// Startup (`initialize`) has already run.
    seeded: bool,
    /// The scene is stale and needs repainting.
    redraw: bool,
    /// The document carries a change the host should bracket as an undo point.
    commit: bool,
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

/// Highlight state for one [`SheetsApp::draw_chip`] call: `active` fills
/// solid accent; `accent` (when not active) tints the background lightly.
#[derive(Clone, Copy)]
struct ChipStyle {
    active: bool,
    accent: bool,
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
#[derive(Clone, Copy)]
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

/// An in-progress inline rename of a sheet tab.
struct TabRename {
    idx: usize,
    rect: Rect,
    editor: EditableLabel,
}

/// Palette of tab colours. Stored per sheet as an index into this list.
const TAB_PALETTE: [(u8, u8, u8); 6] = [
    (0xe2, 0x5b, 0x5b), // red
    (0xe0, 0x8a, 0x2a), // orange
    (0x3f, 0xbb, 0x53), // green
    (0x54, 0x9b, 0xf5), // blue
    (0xb1, 0x6c, 0xf0), // purple
    (0xe2, 0x63, 0xb0), // pink
];

/// The concrete colour for a stored tab-colour palette index. Indices wrap
/// around the palette, so any value a peer stored resolves to a real colour.
fn tab_palette_color(idx: u32) -> Color {
    // `TAB_PALETTE` is a non-empty constant, so the remainder and the lookup
    // both always land; the fallbacks keep the function total.
    let slot = (idx as usize).checked_rem(TAB_PALETTE.len()).unwrap_or(0);
    let (r, g, b) = TAB_PALETTE.get(slot).copied().unwrap_or((0x80, 0x80, 0x80));
    Color::from_rgb8(r, g, b)
}

/// Inclusive count of grid indices from `lo` through `hi`. Every caller passes
/// ordered selection bounds, so the saturation only guards the degenerate
/// whole-axis span rather than wrapping it.
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

/// An item count as an `f64` for layout maths. Menus, chip rows and labels run
/// to tens of items, far inside `f64`'s exact-integer range; an absurd count
/// clamps rather than silently losing precision.
fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
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
    /// Per-sheet tab colours (palette indices), refreshed on each reload.
    sheet_colors: Vec<Option<u32>>,
    /// Right-click context menu on a sheet tab: the target sheet plus the
    /// laid-out action rows and colour swatches.
    tab_menu_target: Option<usize>,
    tab_menu_rect: Rect,
    tab_menu_rows: Vec<(Rect, TabAction)>,
    tab_menu_swatches: Vec<(Rect, Option<u32>)>,
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
    /// Whether a drag that began in the formula bar is selecting text.
    formula_bar_selecting: bool,
    sheet_names: Vec<String>,
    active_sheet: usize,
    /// The latest transient action message (e.g. "Pasted into B2"), surfaced
    /// via the floating [`status_bubble`](Self::status_bubble) rather than a
    /// permanent status line. Initialised to `"Ready"`, which the bubble
    /// treats as "nothing to say".
    status: String,
    /// Transient floating status bubble carrying the selection summary
    /// (Sum / Avg / Count) or the active cell's format, plus any fresh action
    /// message. It surfaces when that composed text changes (a selection
    /// move/extend, an action) or while the pointer hovers the bottom-right
    /// reveal zone, then fades — keeping the sheet-tab strip free of status
    /// text so it reads purely as the sheet switcher.
    status_bubble: ui::StatusBubble,
    /// The composed bubble text last seen, so the bubble is poked only when it
    /// actually changes (never every frame, and never on the idle nothing
    /// state).
    last_status_label: String,
    /// `--shots` forces the bubble fully visible so a still frame captures it.
    shots_status: bool,
    /// One-shot frame duties: startup seeding, repaint, undo checkpoint.
    pending: Pending,
    last_press: Option<(Instant, Addr)>,
    last_mouse: Point,
    current_active_format: CellFormat,
    open_menu: OpenMenu,
    settings_panel: SettingsPanel,
    global: GlobalSettings,
    doc_settings: DocSettings,
    /// Whether this instance is a real session or the headless capture path.
    run: RunMode,
    /// Document heads last written to disk — persistence is skipped while the
    /// document is unchanged, so idle frames never re-serialize.
    saved_heads: Vec<ChangeHash>,
    /// Memoised recalculation: `reload` recomputes only when the document
    /// heads or active sheet change, so selection/scroll frames are cheap.
    cache: Option<ReloadCache>,
    /// An in-flight interchange file dialog: the operation and the channel its
    /// chosen path arrives on, drained once per frame in [`Self::drain_io`].
    /// `None` when no import/export chooser is open.
    io: Option<(PendingIo, PortalChannel)>,
    /// The last copied/cut rectangle, kept so an in-app paste can restore raw
    /// inputs and formats exactly (and shift relative formula references),
    /// beyond the plain-text payload other apps see on the system clipboard.
    clipboard: Option<ClipboardGrid>,
    /// The Ctrl+K command palette overlay. Seeded with the app's action set
    /// each time it opens; closed by default.
    palette: CommandPalette,
    /// Framework-owned action dispatch: every doc-mutating affordance routes
    /// through here so undo, byte-identical peer replay, and the OS Ctrl+K
    /// palette all work from one implementation. See [`crate::actions`].
    actions: ui::ActionHost,
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

impl Default for SheetsApp {
    fn default() -> Self {
        Self::new()
    }
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
            sheet_colors: Vec::new(),
            tab_menu_target: None,
            tab_menu_rect: Rect::ZERO,
            tab_menu_rows: Vec::new(),
            tab_menu_swatches: Vec::new(),
            tab_rename: None,
            name_box_rect: Rect::ZERO,
            fx_rect: Rect::ZERO,
            name_box_edit: None,
            formula_field_rect: Rect::ZERO,
            formula_char_x: Vec::new(),
            formula_bar_selecting: false,
            sheet_names: Vec::new(),
            active_sheet: 0,
            status: "Ready".into(),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: String::new(),
            shots_status: false,
            pending: Pending {
                seeded: false,
                redraw: true,
                commit: false,
            },
            last_press: None,
            last_mouse: Point::ZERO,
            current_active_format: CellFormat::General,
            open_menu: OpenMenu::None,
            settings_panel: SettingsPanel::None,
            global: GlobalSettings::default(),
            doc_settings: DocSettings::default(),
            run: RunMode::Live,
            saved_heads: Vec::new(),
            cache: None,
            io: None,
            clipboard: None,
            palette: CommandPalette::commands(),
            actions: ui::ActionHost::new(),
        }
    }

    // --- action dispatch ----------------------------------------------------

    /// Dispatch one [`SheetsAction`] — the single CRDT-mutation path shared with
    /// the Ctrl+K palette. Delegates to the framework's
    /// [`ActionHost`](ui::ActionHost), which brackets the undo boundary and
    /// applies the action through the executor (a pinned actor + committed
    /// change, so it replays byte-identically on peers), exactly as the palette
    /// does. This app supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &SheetsAction) {
        self.actions.dispatch_with_context(
            doc,
            actions::SHEETS_ACTION_TYPE,
            action,
            actions::history_context(action),
        );
    }

    /// The presentation context the Ctrl+K resource tree is built from: the open
    /// sheet names. Everything here mirrors state the document already holds.
    pub(crate) fn sheets_resource_ctx(&self) -> SheetsResourceCtx {
        SheetsResourceCtx {
            sheet_names: self.sheet_names.clone(),
        }
    }

    /// The last [`EncodedAction`](qos_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the affordance == [`ActionSpec`](qos_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical
    /// action.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Configure the app for the headless `--shots`/test path: seed the sample
    /// workbook so a captured frame shows real formulas, and disable on-disk
    /// persistence so screenshots never read or write the user's workbook.
    pub fn shots_mode(&mut self) {
        self.run = RunMode::Shots;
    }

    /// The index of the active sheet. Lets a test observe where a Ctrl+K
    /// activation navigated.
    pub fn active_sheet_index(&self) -> usize {
        self.active_sheet
    }

    /// `--shots`: force the floating status bubble fully visible so a single
    /// headless frame captures the floating Sum / Avg / Count pill (otherwise
    /// it would have faded, or never surfaced, by the time the frame renders).
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(std::time::Instant::now());
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

    /// Shots hook: build a formula purely by clicking cells (point mode),
    /// so the captured frame shows the coloured references it inserted and
    /// the matching outlines on the referenced cells (B2 and C2).
    pub fn shots_build_formula(&mut self) {
        self.grid.select_only((2, 9)); // C10, an empty cell
        self.grid.begin_edit(Some("=".into()));
        self.grid.point_click_addr((1, 1)); // click B2 → =B2
        self.grid.insert_text_public("+"); // type + → =B2+
        self.grid.point_click_addr((2, 1)); // click C2 → =B2+C2
    }

    /// Shots hook: select a multi-cell numeric range so the live status-bar
    /// summary (Sum / Avg / Count) is captured.
    pub fn shots_select_summary_range(&mut self) {
        // Budget!B2:B6 — the January column body.
        self.grid.select_range_public((1, 1), (1, 5));
        self.arm_status_bubble_for_shots();
    }

    /// Shots hook: build a discontiguous multi-range selection (the Jan column
    /// body plus the Q1 Total column) so the frame shows two separate
    /// highlighted rectangles with lit row/column headers.
    pub fn shots_multi_select(&mut self) {
        self.grid.select_range_public((1, 1), (1, 5)); // B2:B6
        self.grid.add_range_public((4, 1), (4, 6)); // E2:E7
        self.arm_status_bubble_for_shots();
    }

    /// Shots hook: open the right-click context menu on the first sheet tab so
    /// the captured frame shows the rename/duplicate/move/colour actions.
    pub fn shots_open_tab_menu(&mut self) {
        self.open_tab_menu(0);
    }

    /// Shots hook: edit the active formula cell and select the range reference
    /// inside it, so the captured frame shows the formula bar's text selection
    /// and caret (the editable formula bar).
    pub fn shots_formula_bar_select(&mut self) {
        self.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        self.grid.begin_edit_in_place();
        self.grid.set_edit_caret(5, false); // just before "B2:D2"
        self.grid.set_edit_caret(10, true); // select "B2:D2"
    }

    /// Shots hook: open the Ctrl+K command palette so the captured frame shows
    /// the action list with labels + descriptions.
    pub fn shots_open_command_palette(&mut self) {
        self.open_command_palette();
    }

    /// Shots hook: seed three fill patterns in an empty region and extend each
    /// through the *real* fill-handle path, so the captured frame shows genuine
    /// filled series — a linear number run (1, 2 → …6), a weekday sequence
    /// (Mon → …Sat), and a text+number run (Task 1 → …Task 6) — plus the fill
    /// handle on the resulting selection.
    pub fn shots_fill_series(&mut self, doc: &mut AutoCommit) {
        // Labels + seed patterns in columns B..D under an empty region.
        let seeds: &[(Addr, &str)] = &[
            ((0, 14), "Fill demo"),
            ((1, 14), "Count"),
            ((2, 14), "Weekday"),
            ((3, 14), "Task"),
            ((1, 15), "1"),
            ((1, 16), "2"),
            ((2, 15), "Mon"),
            ((3, 15), "Task 1"),
        ];
        let cmds: Vec<GridCmd> = seeds
            .iter()
            .map(|(a, v)| GridCmd::SetCell {
                addr: *a,
                raw: v.to_string(),
            })
            .collect();
        self.apply_cmds(doc, cmds);

        // Numeric pair B16:B17 → extend down to B21 (3,4,5,6).
        self.grid.select_range_public((1, 15), (1, 16));
        self.grid.start_fill_to((1, 20));
        self.run_pending_fill(doc);
        // Single weekday C16 → extend down to C21 (Tue…Sat).
        self.grid.select_only((2, 15));
        self.grid.start_fill_to((2, 20));
        self.run_pending_fill(doc);
        // Single text+number D16 → extend down to D21 (Task 2…Task 6).
        self.grid.select_only((3, 15));
        self.grid.start_fill_to((3, 20));
        self.run_pending_fill(doc);

        // Land the selection on the filled numeric column so its fill handle
        // shows at the bottom-right corner.
        self.grid.select_range_public((1, 15), (1, 20));
        self.reload(doc);
    }

    /// Shots hook: copy the `=SUM(B2:D2)` cell and paste it one column to the
    /// right, then select the pasted cell — the frame shows the *adjusted*
    /// formula `=SUM(C2:E2)` in the formula bar and its recomputed result in
    /// the cell, demonstrating relative-reference adjustment on paste.
    pub fn shots_paste_formula(&mut self, doc: &mut AutoCommit) {
        self.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        self.copy_selection(false);
        self.grid.select_only((5, 1)); // F2 (seeded AVERAGE; overwritten)
        self.paste(doc);
        self.grid.select_only((5, 1));
        self.reload(doc);
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
        if self.run.persists()
            && let Some(path) = workbook_path()
            && let Ok(bytes) = std::fs::read(&path)
            && let Ok(mut saved) = AutoCommit::load(&bytes)
        {
            let _ = doc.merge(&mut saved);
        }
        if SheetsDoc::is_empty(doc) {
            if self.run.seeds_sample_data() {
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
        if !self.run.persists() {
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
        if let Some(c) = &self.cache
            && c.sheet == sheet
            && c.heads == heads
        {
            self.active_sheet = sheet;
            return;
        }

        self.active_sheet = sheet;
        self.sheet_names = SheetsDoc::sheet_names(doc);
        self.sheet_colors = SheetsDoc::sheet_colors(doc);
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
            if let Some(body) = raw.strip_prefix('=')
                && let Some(inferred) = formula::infer_result_format(body, &snap.fmt)
            {
                fmt.insert(*addr, inferred);
            }
        }

        self.cache = Some(ReloadCache {
            heads,
            sheet,
            eff_fmt: fmt.clone(),
            values: values.clone(),
        });
        self.grid.set_data(snap.raw, fmt, values);
        self.pending.redraw = true;
    }

    fn apply_cmds(&mut self, doc: &mut AutoCommit, cmds: Vec<GridCmd>) {
        let cur = self.default_currency();
        let sheet = self.active_sheet;
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
            self.perform_action(doc, &actions::set_cells_action(sheet, writes));
            self.reload(doc);
        }
    }

    fn apply_format(&mut self, doc: &mut AutoCommit, fmt: CellFormat) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let sheet = self.active_sheet;
        let cells: Vec<(Addr, CellFormat)> = self
            .grid
            .selected_cells()
            .into_iter()
            .map(|a| (a, fmt))
            .collect();
        if !cells.is_empty() {
            self.perform_action(doc, &actions::set_formats_action(sheet, cells));
        }
        self.reload(doc);
        self.status = format!("Applied {} to {}", fmt.label(), self.grid.selection_label());
    }

    fn clear_format(&mut self, doc: &mut AutoCommit) {
        let sheet = self.active_sheet;
        let cells: Vec<(Addr, CellFormat)> = self
            .grid
            .selected_cells()
            .into_iter()
            .map(|a| (a, CellFormat::General))
            .collect();
        if !cells.is_empty() {
            self.perform_action(doc, &actions::set_formats_action(sheet, cells));
        }
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
    fn paste(&mut self, doc: &mut AutoCommit) {
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
            let sheet = self.active_sheet;
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
            self.perform_action(doc, &actions::set_cells_action(sheet, writes));
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

        self.reload(doc);
        self.status = format!("Pasted into {}", self.grid.selection_label());
    }

    /// Parse `text` as a tab/newline-separated grid pasted from another app and
    /// write it starting at `target`, smart-typing each non-empty field.
    /// Selects the pasted rectangle. Returns `false` (writing nothing) when the
    /// payload has no content. This is the system-clipboard branch of `paste`.
    fn paste_tsv(&mut self, doc: &mut AutoCommit, target: Addr, text: &str) -> bool {
        let cur = self.default_currency();
        let normalized = text.replace('\r', "");
        let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
        if body.is_empty() {
            return false;
        }
        let sheet = self.active_sheet;
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
        self.perform_action(doc, &actions::set_cells_action(sheet, writes));
        self.grid.select_range_public(target, last);
        true
    }

    /// `AutoSum`: scan upward from the active cell for a run of numeric cells
    /// and write `=SUM(top:above)` into the active cell.
    fn auto_sum(&mut self, doc: &mut AutoCommit) {
        let (c, r) = self.grid.active();
        if r == 0 {
            self.status = "Nothing above to sum".into();
            return;
        }
        let mut top = r;
        for rr in (0..r).rev() {
            let raw = SheetsDoc::cell_raw(doc, self.active_sheet, (c, rr));
            let is_num = raw.trim().parse::<f64>().is_ok() || raw.starts_with('=');
            if raw.is_empty() || !is_num {
                break;
            }
            top = rr;
        }
        if top == r {
            self.status = "No numeric run above".into();
            return;
        }
        // `r > 0` here (the zero row returned above), so the run ends on `r - 1`.
        let formula = format!(
            "=SUM({}:{})",
            crate::model::addr_to_a1(c, top),
            crate::model::addr_to_a1(c, r.saturating_sub(1))
        );
        self.perform_action(
            doc,
            &actions::set_cell_action(self.active_sheet, (c, r), &formula, None),
        );
        self.reload(doc);
        self.status = format!("Inserted {formula}");
    }

    /// Fill the top row of the selection downward through every row in the
    /// selection's bounding box — Excel's Ctrl+D. Each source cell's raw input
    /// and format copy down, with relative formula references shifted per row.
    fn fill_down(&mut self, doc: &mut AutoCommit) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        if r1 <= r0 {
            self.status = "Select two or more rows to fill down".into();
            return;
        }
        let sheet = self.active_sheet;
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
        self.perform_action(doc, &actions::set_cells_action(sheet, writes));
        self.reload(doc);
        self.status = format!("Filled down {}", self.grid.selection_label());
    }

    /// Fill the left column of the selection rightward — Excel's Ctrl+R.
    fn fill_right(&mut self, doc: &mut AutoCommit) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        if c1 <= c0 {
            self.status = "Select two or more columns to fill right".into();
            return;
        }
        let sheet = self.active_sheet;
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
        self.perform_action(doc, &actions::set_cells_action(sheet, writes));
        self.reload(doc);
        self.status = format!("Filled right {}", self.grid.selection_label());
    }

    /// Snapshot a cell for the fill engine: its raw input, computed value, and
    /// effective display format.
    fn fill_cell_at(&self, addr: Addr) -> fill::FillCell {
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
    fn run_pending_fill(&mut self, doc: &mut AutoCommit) {
        let Some((src_tl, src_br, dst_tl, dst_br)) = self.grid.take_pending_fill() else {
            return;
        };
        let sheet = self.active_sheet;
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
        self.perform_action(doc, &actions::set_cells_action(sheet, writes));
        self.reload(doc);
        self.grid.select_range_public(src_tl, dst_br);
        self.status = format!("Filled {}", self.grid.selection_label());
        self.pending.redraw = true;
    }

    /// Import the most recently modified `.csv` file from the shared Sheets
    /// folder into a new sheet, running smart type inference on each field.
    /// No file picker exists in the toolkit yet, so "most recent in the
    /// folder" is the selection heuristic (drop a file there, then import).
    fn import_csv(&mut self, _doc: &mut AutoCommit) {
        self.begin_import(Interchange::Csv);
    }

    /// Core: import the CSV file at `path` into a new sheet. Split out from the
    /// portal opener so the drain and tests drive it against a real file.
    fn import_csv_from(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("CSV import failed: {e}");
                return;
            }
        };
        let parsed = crate::csv::parse_csv(&text);
        let count = parsed.len();
        let idx = SheetsDoc::add_sheet(doc, &sheet_name_from_path(path));
        let cur = self.default_currency();
        for (addr, raw) in parsed {
            let inferred = infer_entry(&raw, cur);
            SheetsDoc::set_cell_raw(doc, idx, addr, &inferred.raw);
            if inferred.format != CellFormat::General {
                SheetsDoc::set_cell_format(doc, idx, addr, inferred.format);
            }
        }
        SheetsDoc::set_active_index(doc, idx);
        self.pending.commit = true;
        self.reload(doc);
        self.status = format!("Imported {count} cells from {}", file_label(path));
    }

    /// Build an export table for the active sheet from its computed values.
    fn active_table(&self, doc: &mut AutoCommit) -> crate::xlsx::SheetTable {
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet, cur);
        let values = formula::recalculate(&snap.raw);
        // Re-apply format propagation so exported computed cells carry units.
        let mut fmt = snap.fmt.clone();
        for (addr, raw) in &snap.raw {
            if !snap.fmt.contains_key(addr)
                && let Some(body) = raw.strip_prefix('=')
                && let Some(inferred) = formula::infer_result_format(body, &snap.fmt)
            {
                fmt.insert(*addr, inferred);
            }
        }
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &fmt, &values)
    }

    fn export_csv(&mut self, _doc: &mut AutoCommit) {
        self.begin_export(Interchange::Csv);
    }

    /// Core: write the active sheet's computed values to `path` as CSV.
    fn export_csv_to(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
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
        match std::fs::write(path, csv) {
            Ok(()) => self.status = format!("Exported {}", file_label(path)),
            Err(e) => self.status = format!("CSV export failed: {e}"),
        }
    }

    fn export_xlsx(&mut self, _doc: &mut AutoCommit) {
        self.begin_export(Interchange::Xlsx);
    }

    /// Core: write the active sheet to `path` as an `.xlsx` workbook.
    fn export_xlsx_to(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let table = self.active_table(doc);
        match crate::xlsx::write_xlsx(path, std::slice::from_ref(&table)) {
            Ok(()) => self.status = format!("Exported {}", file_label(path)),
            Err(e) => self.status = format!("Excel export failed: {e}"),
        }
    }

    fn export_ods(&mut self, _doc: &mut AutoCommit) {
        self.begin_export(Interchange::Ods);
    }

    /// Core: write the active sheet to `path` as an `.ods` spreadsheet.
    fn export_ods_to(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let table = self.active_table(doc);
        match crate::ods::write_ods(path, std::slice::from_ref(&table)) {
            Ok(()) => self.status = format!("Exported {}", file_label(path)),
            Err(e) => self.status = format!("Calc export failed: {e}"),
        }
    }

    /// Import a parsed set of tables (one per sheet) into new sheets, running
    /// smart inference where a source cell carried no explicit format.
    fn import_tables(
        &mut self,
        doc: &mut AutoCommit,
        tables: Vec<crate::xlsx::SheetTable>,
        kind: &str,
    ) {
        let cur = self.default_currency();
        let mut total: usize = 0;
        for table in tables {
            let idx = SheetsDoc::add_sheet(doc, &table.name);
            for (r, row) in (0u32..).zip(&table.rows) {
                for (c, cell) in (0u32..).zip(row) {
                    let Some(cell) = cell else { continue };
                    let addr = (c, r);
                    // A formula cell restores its live formula; a value cell
                    // stores its (re-inferred) value. The format still derives
                    // from the imported value, not the formula text.
                    let raw_to_store = cell.formula.as_deref().unwrap_or(cell.raw.as_str());
                    SheetsDoc::set_cell_raw(doc, idx, addr, raw_to_store);
                    let fmt = if cell.format == CellFormat::General {
                        infer_entry(&cell.raw, cur).format
                    } else {
                        cell.format
                    };
                    if fmt != CellFormat::General {
                        SheetsDoc::set_cell_format(doc, idx, addr, fmt);
                    }
                    total = total.saturating_add(1);
                }
            }
            SheetsDoc::set_active_index(doc, idx);
        }
        self.pending.commit = true;
        self.reload(doc);
        self.status = format!("Imported {total} cells from {kind}");
    }

    fn import_xlsx(&mut self, _doc: &mut AutoCommit) {
        self.begin_import(Interchange::Xlsx);
    }

    /// Core: import the `.xlsx` workbook at `path` into new sheets.
    fn import_xlsx_from(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        match crate::xlsx::read_xlsx(path) {
            Ok(tables) => self.import_tables(doc, tables, &file_label(path)),
            Err(e) => self.status = format!("Excel import failed: {e}"),
        }
    }

    fn import_ods(&mut self, _doc: &mut AutoCommit) {
        self.begin_import(Interchange::Ods);
    }

    /// Core: import the `.ods` spreadsheet at `path` into new sheets.
    fn import_ods_from(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        match crate::ods::read_ods(path) {
            Ok(tables) => self.import_tables(doc, tables, &file_label(path)),
            Err(e) => self.status = format!("Calc import failed: {e}"),
        }
    }

    /// Open the host file chooser to pick a file to import (unless one is
    /// already in flight). The chosen path is handled in [`Self::drain_io`].
    fn begin_import(&mut self, fmt: Interchange) {
        if self.io.is_some() {
            return;
        }
        self.io = Some((
            PendingIo::Import(fmt),
            spreadsheet_import_request(fmt).send(),
        ));
        self.status = format!("Choose a .{} file to import…", fmt.ext());
    }

    /// Open the host Save dialog to pick where to export the active sheet,
    /// seeding the filename with the sheet's name (unless one is already in
    /// flight). The chosen destination is handled in [`Self::drain_io`].
    fn begin_export(&mut self, fmt: Interchange) {
        if self.io.is_some() {
            return;
        }
        let name = self
            .sheet_names
            .get(self.active_sheet)
            .cloned()
            .unwrap_or_else(|| "Sheet".into());
        let suggested = format!("{name}.{}", fmt.ext());
        self.io = Some((
            PendingIo::Export(fmt),
            spreadsheet_export_request(fmt, &suggested).send(),
        ));
        self.status = "Choose where to save…".into();
    }

    /// Drain a finished interchange file dialog, if one landed since the last
    /// frame, and run the chosen import/export against the picked path. The UI
    /// thread never blocks: the portal round-trip lives on its own worker.
    fn drain_io(&mut self, doc: &mut AutoCommit) {
        let Some((op, rx)) = &self.io else { return };
        let op = *op;
        match rx.try_recv() {
            Ok(outcome) => {
                self.io = None;
                match outcome {
                    PortalOutcome::Opened(path) => match op {
                        PendingIo::Import(Interchange::Csv) => self.import_csv_from(doc, &path),
                        PendingIo::Import(Interchange::Xlsx) => self.import_xlsx_from(doc, &path),
                        PendingIo::Import(Interchange::Ods) => self.import_ods_from(doc, &path),
                        PendingIo::Export(Interchange::Csv) => self.export_csv_to(doc, &path),
                        PendingIo::Export(Interchange::Xlsx) => self.export_xlsx_to(doc, &path),
                        PendingIo::Export(Interchange::Ods) => self.export_ods_to(doc, &path),
                    },
                    PortalOutcome::Cancelled => {}
                    PortalOutcome::Failed(e) => {
                        self.status = format!("File chooser unavailable: {e}");
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.io = None,
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
        let name = format!("Sheet{}", n.saturating_add(1));
        self.perform_action(doc, &actions::add_sheet_action(&name));
        // The active sheet tab is presentation, not a document mutation: a plain
        // write, outside the action's undo bracket. The appended sheet is at `n`.
        SheetsDoc::set_active_index(doc, n);
        self.grid.select_only((0, 0));
        self.reload(doc);
        self.status = "Added sheet".into();
    }

    // --- settings actions ---------------------------------------------------

    fn apply_setting(&mut self, doc: &mut AutoCommit, kind: SettingKind) {
        match kind {
            SettingKind::Currency(c) => self.doc_settings.currency = c,
            SettingKind::Date(d) => self.doc_settings.date_format = d,
            SettingKind::Time(t) => self.doc_settings.time_format = t,
        }
        // Persist the document overrides into the CRDT so collaborators (and
        // the next session) share the document's conventions.
        let action = actions::set_doc_settings_action(&self.doc_settings);
        self.perform_action(doc, &action);
        self.reload(doc);
    }

    // --- command palette ----------------------------------------------------

    /// Seed the Ctrl+K command palette with the current action set and open it.
    /// Rebuilt each time so argument prefills (e.g. the active cell reference)
    /// stay current. Every entry is wired to a real mutation in
    /// [`dispatch_command`]; nothing here is a stub.
    fn open_command_palette(&mut self) {
        // Close any other overlay so the palette owns the surface.
        self.open_menu = OpenMenu::None;
        self.settings_panel = SettingsPanel::None;
        self.tab_menu_target = None;
        self.name_box_edit = None;
        self.tab_rename = None;

        let active_ref = {
            let (c, r) = self.grid.active();
            crate::model::addr_to_a1(c, r)
        };
        let formula_view_label = if self.grid.formula_view() {
            "Hide formulas"
        } else {
            "Show formulas"
        };

        self.palette.clear();
        let entries = [
            CommandEntry::new("goto", "Go to cell\u{2026}")
                .description("Jump to and select a cell by its reference")
                .with_argument("Go to cell", "e.g. B12")
                .with_argument_prefill(active_ref),
            CommandEntry::new("sum_selection", "Sum selection")
                .description("Write =SUM() totals in the row below the selection")
                .keybinding("Alt+="),
            CommandEntry::new("autosum", "AutoSum column")
                .description("Sum the run of numbers directly above the active cell"),
            CommandEntry::new("fill_down", "Fill down")
                .description("Copy the top row of the selection down over its rows")
                .keybinding("Ctrl+D"),
            CommandEntry::new("fill_right", "Fill right")
                .description("Copy the left column of the selection across its columns")
                .keybinding("Ctrl+R"),
            CommandEntry::new("insert_row", "Insert row above")
                .description("Insert a blank row above the selection, shifting cells down"),
            CommandEntry::new("delete_row", "Delete row")
                .description("Delete the active row, shifting the rows below it up"),
            CommandEntry::new("insert_col", "Insert column left")
                .description("Insert a blank column left of the selection, shifting cells right"),
            CommandEntry::new("delete_col", "Delete column")
                .description("Delete the active column, shifting the columns after it left"),
            CommandEntry::new("formula_view", formula_view_label)
                .description("Toggle between formula source and computed results"),
            CommandEntry::new("format_currency", "Format as currency")
                .description("Apply the document currency format to the selection"),
            CommandEntry::new("format_percent", "Format as percent")
                .description("Show the selected numbers as percentages"),
            CommandEntry::new("clear_format", "Clear formatting")
                .description("Reset the selection to the General format"),
            CommandEntry::new("export_csv", "Export CSV")
                .description("Write the active sheet to a .csv file"),
            CommandEntry::new("export_xlsx", "Export Excel (.xlsx)")
                .description("Write the workbook to an .xlsx spreadsheet file"),
            CommandEntry::new("new_sheet", "New sheet")
                .description("Add a fresh blank sheet to the workbook"),
        ];
        for e in entries {
            self.palette.push(e);
        }
        self.palette.open();
        self.pending.redraw = true;
    }

    /// Run the action the palette emitted, dispatching by id. `argument`
    /// carries the free-text a `with_argument` entry collected (e.g. the cell
    /// reference for "Go to cell").
    fn dispatch_command(&mut self, doc: &mut AutoCommit, id: &str, argument: Option<&str>) {
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
            "export_csv" => self.export_csv(doc),
            "export_xlsx" => self.export_xlsx(doc),
            "new_sheet" => self.add_sheet(doc),
            _ => {}
        }
        self.pending.redraw = true;
    }

    /// "Go to cell": parse an A1 reference (or `A1:C3` range top-left) and jump
    /// the selection there, scrolling it into view.
    fn go_to_cell(&mut self, arg: &str) {
        let token = arg.trim().split([':', ' ']).next().unwrap_or("").trim();
        if let Some(addr) = a1_to_addr(token) {
            self.grid.select_only(addr);
            self.status = format!("Jumped to {}", token.to_uppercase());
        } else {
            self.status = format!("'{}' is not a valid cell reference", arg.trim());
        }
    }

    /// "Sum selection": write a `=SUM(col_top:col_bottom)` total for each column
    /// of the selection into the row directly below it, then select that row.
    fn sum_selection(&mut self, doc: &mut AutoCommit) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let target_r = r1.saturating_add(1);
        let sheet = self.active_sheet;
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
        for c in c0..=c1 {
            let formula = format!(
                "=SUM({}:{})",
                crate::model::addr_to_a1(c, r0),
                crate::model::addr_to_a1(c, r1),
            );
            writes.push(((c, target_r), formula, None));
        }
        self.perform_action(doc, &actions::set_cells_action(sheet, writes));
        self.reload(doc);
        self.grid
            .select_range_public((c0, target_r), (c1, target_r));
        self.status = format!("Summed selection into row {}", target_r.saturating_add(1));
    }

    fn insert_row(&mut self, doc: &mut AutoCommit) {
        let r0 = self.grid.active().1;
        self.perform_action(doc, &actions::insert_row_action(self.active_sheet, r0));
        self.reload(doc);
        self.status = format!("Inserted row {}", r0.saturating_add(1));
    }

    fn delete_row(&mut self, doc: &mut AutoCommit) {
        let r0 = self.grid.active().1;
        self.perform_action(doc, &actions::delete_row_action(self.active_sheet, r0));
        self.reload(doc);
        self.status = format!("Deleted row {}", r0.saturating_add(1));
    }

    fn insert_column(&mut self, doc: &mut AutoCommit) {
        let c0 = self.grid.active().0;
        self.perform_action(doc, &actions::insert_column_action(self.active_sheet, c0));
        self.reload(doc);
        self.status = format!(
            "Inserted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    fn delete_column(&mut self, doc: &mut AutoCommit) {
        let c0 = self.grid.active().0;
        self.perform_action(doc, &actions::delete_column_action(self.active_sheet, c0));
        self.reload(doc);
        self.status = format!(
            "Deleted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    // --- layout -------------------------------------------------------------

    fn relayout(&mut self) {
        let r = self.rect;
        self.menubar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + MENUBAR_H);
        self.formula_rect = Rect::new(
            r.x0,
            self.menubar_rect.y1,
            r.x1,
            self.menubar_rect.y1 + FORMULA_H,
        );
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
        self.name_box_rect = Rect::new(
            self.formula_rect.x0 + 6.0,
            self.formula_rect.y0 + 4.0,
            self.formula_rect.x0 + NAME_BOX_W,
            self.formula_rect.y1 - 4.0,
        );
        // The clickable "fx" affordance spans the gap between the name box and
        // the formula field.
        self.fx_rect = Rect::new(
            self.formula_rect.x0 + NAME_BOX_W,
            self.formula_rect.y0 + 4.0,
            self.formula_rect.x0 + NAME_BOX_W + fx_w + 8.0,
            self.formula_rect.y1 - 4.0,
        );
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
        if self.tab_menu_target.is_some() {
            self.layout_tab_menu();
        }
    }

    /// Lay out the sheet-tab context menu above its target tab: a stack of
    /// action rows plus a colour-swatch strip. Re-derived from the current tab
    /// rects each pass, so it tracks the tab under a resize.
    fn layout_tab_menu(&mut self) {
        self.tab_menu_rows.clear();
        self.tab_menu_swatches.clear();
        let Some(idx) = self.tab_menu_target else {
            return;
        };
        let anchor = self
            .tab_rects
            .iter()
            .find(|(_, i)| *i == idx)
            .map_or(self.add_tab_rect, |(r, _)| *r);

        let actions = [
            TabAction::Rename,
            TabAction::Duplicate,
            TabAction::InsertRight,
            TabAction::MoveLeft,
            TabAction::MoveRight,
            TabAction::Delete,
        ];
        let width = 188.0_f64;
        let row_h = 30.0;
        let pad = 6.0;
        let swatch = 20.0;
        let color_block = 18.0 + swatch + 8.0; // "Colour" label + swatch row
        let h = pad * 2.0 + count_f64(actions.len()) * row_h + color_block;

        let x0 = anchor
            .x0
            .min(self.rect.x1 - width - 8.0)
            .max(self.rect.x0 + 4.0);
        let y1 = anchor.y0 - 6.0;
        let y0 = (y1 - h).max(self.rect.y0 + 4.0);
        self.tab_menu_rect = Rect::new(x0, y0, x0 + width, y1);

        let mut y = y0 + pad;
        for act in actions {
            self.tab_menu_rows.push((
                Rect::new(x0 + pad, y, x0 + width - pad, y + row_h - 2.0),
                act,
            ));
            y += row_h;
        }
        // Colour swatch strip, below a "Colour" label.
        let cy = y + 20.0;
        let mut cx = x0 + pad;
        self.tab_menu_swatches
            .push((Rect::new(cx, cy, cx + swatch, cy + swatch), None));
        cx += swatch + 6.0;
        for i in 0..TAB_PALETTE.len() as u32 {
            self.tab_menu_swatches
                .push((Rect::new(cx, cy, cx + swatch, cy + swatch), Some(i)));
            cx += swatch + 6.0;
        }
    }

    fn layout_dropdown(&mut self, anchor: Rect, items: &[(&str, MenuItem)], width: f64) {
        let row_h = 30.0;
        let pad = 6.0;
        let x0 = anchor.x0;
        let y0 = anchor.y1 + 4.0;
        let h = count_f64(items.len()) * row_h + pad * 2.0;
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
            push(
                &mut self.setting_chips,
                &mut x,
                &mut y,
                74.0,
                SettingKind::Currency(None),
            );
            for c in Currency::ALL {
                push(
                    &mut self.setting_chips,
                    &mut x,
                    &mut y,
                    48.0,
                    SettingKind::Currency(Some(c)),
                );
            }
        }
        y += chip_h + 18.0;

        // Row: date style.
        self.settings_section_y[1] = y;
        {
            let mut x = row_start;
            let rect = Rect::new(x, y, x + 74.0, y + chip_h);
            self.setting_chips.push(SettingChip {
                rect,
                kind: SettingKind::Date(None),
            });
            x += 74.0 + chip_gap;
            for (label, d) in [
                ("ISO", DateStyle::Iso),
                ("US", DateStyle::Us),
                ("EU", DateStyle::Eu),
                ("Long", DateStyle::Long),
            ] {
                let w = (count_f64(label.len()) * 8.0 + 24.0).max(48.0);
                self.setting_chips.push(SettingChip {
                    rect: Rect::new(x, y, x + w, y + chip_h),
                    kind: SettingKind::Date(Some(d)),
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
            self.setting_chips.push(SettingChip {
                rect,
                kind: SettingKind::Time(None),
            });
            x += 74.0 + chip_gap;
            for (label, t) in [("24-hour", TimeStyle::H24), ("12-hour", TimeStyle::H12)] {
                let w = count_f64(label.len()) * 8.0 + 24.0;
                self.setting_chips.push(SettingChip {
                    rect: Rect::new(x, y, x + w, y + chip_h),
                    kind: SettingKind::Time(Some(t)),
                });
                x += w + chip_gap;
            }
        }
    }

    // --- drawing ------------------------------------------------------------

    fn draw_chip(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        style: ChipStyle,
    ) {
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.sm));
        let bg = if style.active {
            colors.accent[5]
        } else if style.accent {
            colors.accent[5].with_alpha_factor(0.18)
        } else {
            colors.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            if style.active {
                colors.accent[5]
            } else {
                colors.border_standard
            },
            None,
            &shape,
        );
        let size = theme.tokens.typography.scale.sm;
        let (tw, _) = tr.measure(label, size, None);
        let tx = rect.center().x - f64::from(tw) / 2.0;
        let tyy = rect.y0 + (rect.height() - f64::from(size)) / 2.0;
        let color = if style.active {
            Color::WHITE
        } else {
            colors.text_primary
        };
        tr.draw_text(
            scene,
            label,
            size,
            color,
            transform * Affine::translate((tx, tyy)),
            None,
        );
    }

    fn draw_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &self.rect,
        );

        self.draw_menu_bar(scene, transform, tr, theme);
        self.draw_name_box(scene, transform, tr, theme);
        self.draw_formula_field(scene, transform, tr, theme);
        self.draw_sheet_tabs_strip(scene, transform, tr, theme);
    }

    /// Menu bar: File/Format buttons + bottom border.
    fn draw_menu_bar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &self.menubar_rect,
        );
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
            Self::draw_chip(
                scene,
                transform,
                tr,
                theme,
                b.rect,
                &label,
                ChipStyle { active, accent },
            );
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
    }

    /// Formula bar's name box: background, focus border, and either the live
    /// edit buffer (with caret + pre-fill selection) or the selection label.
    fn draw_name_box(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &self.formula_rect,
        );
        let name_box = self.name_box_rect;
        let nb_editing = self.name_box_edit.is_some();
        let nb_shape = RoundedRect::from_rect(name_box, f64::from(theme.tokens.radii.sm));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &nb_shape,
        );
        scene.stroke(
            &Stroke::new(if nb_editing { 2.0 } else { 1.0 }),
            transform,
            if nb_editing {
                colors.accent[5]
            } else {
                colors.border_standard
            },
            None,
            &nb_shape,
        );
        let sm = theme.tokens.typography.scale.sm;
        let nb_text_x = name_box.x0 + 8.0;
        let nb_text_y = name_box.y0 + (name_box.height() - f64::from(sm)) / 2.0;
        if let Some(input) = &mut self.name_box_edit {
            // Keep the compact spreadsheet chrome, while the shared editor
            // owns shaping, scrolling, selection, clipboard, IME and caret
            // hit-testing inside its content band.
            input.set_font_size(sm);
            input.apply_layout_external(Rect::new(
                nb_text_x,
                name_box.y0 + 2.0,
                name_box.x1 - 4.0,
                name_box.y1 - 2.0,
            ));
            input.draw(scene, transform, tr, theme, &PresenceCtx::default());
        } else {
            let label = self.grid.selection_label();
            tr.draw_text_weighted(
                scene,
                &label,
                sm,
                colors.text_primary,
                transform * Affine::translate((nb_text_x, nb_text_y)),
                None,
                TextWeight::Semibold,
            );
        }
    }

    /// Formula bar's "fx" field: the "fx" glyph, the input box, live text
    /// (colour-tinted formula references while editing/selected), selection
    /// highlight and caret. Rebuilds `self.formula_char_x` for click/caret
    /// mapping every frame, since it must track exactly what's drawn.
    fn draw_formula_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.draw_formula_fx_label(scene, transform, tr, theme);
        let ff_shape =
            RoundedRect::from_rect(self.formula_field_rect, f64::from(theme.tokens.radii.sm));
        let editing = self.grid.is_editing();
        Self::draw_formula_field_chrome(scene, transform, theme, ff_shape, editing);

        let content = self.grid.active_display_raw();
        let base = theme.tokens.typography.scale.base;
        let text_x0 = self.formula_field_rect.x0 + 8.0;
        // Rebuild the per-character x boundaries for click/caret mapping. This
        // is derived from exactly the text being drawn, so a click maps to the
        // same glyph the user sees (whether it's the live edit buffer or the
        // active cell's stored input).
        self.formula_char_x = char_boundaries(tr, &content, base, text_x0);
        let origin = Point::new(
            text_x0,
            self.formula_field_rect.y0 + (self.formula_field_rect.height() - f64::from(base)) / 2.0,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &ff_shape,
        );
        self.draw_formula_selection(scene, transform, theme, editing, text_x0);
        Self::draw_formula_content(scene, transform, tr, theme, &content, base, origin);
        self.draw_formula_caret(scene, transform, theme, editing, text_x0);
        scene.pop_layer();
    }

    fn draw_formula_fx_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
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
    }

    fn draw_formula_field_chrome(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        ff_shape: RoundedRect,
        editing: bool,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &ff_shape,
        );
        scene.stroke(
            &Stroke::new(if editing { 2.0 } else { 1.0 }),
            transform,
            if editing {
                colors.accent[5]
            } else {
                colors.border_standard
            },
            None,
            &ff_shape,
        );
    }

    /// Selection highlight behind the text (drawn whenever editing).
    fn draw_formula_selection(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        editing: bool,
        text_x0: f64,
    ) {
        let colors = &theme.tokens.colors;
        if editing && let Some((s, e)) = self.grid.edit_selection() {
            let x0 = self.formula_char_x.get(s).copied().unwrap_or(text_x0);
            let x1 = self.formula_char_x.get(e).copied().unwrap_or(x0);
            let sel_rect = Rect::new(
                x0,
                self.formula_field_rect.y0 + 5.0,
                x1,
                self.formula_field_rect.y1 - 5.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5].with_alpha_factor(0.30),
                None,
                &sel_rect,
            );
        }
    }

    /// Formulas (whether being edited OR merely selected) tint each cell
    /// reference to match the coloured outlines on the grid; plain values
    /// draw in the body colour.
    #[allow(clippy::too_many_arguments)]
    fn draw_formula_content(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: &str,
        base: f32,
        origin: Point,
    ) {
        let colors = &theme.tokens.colors;
        if content.starts_with('=') {
            crate::grid::draw_formula_line(
                scene,
                transform,
                tr,
                content,
                base,
                colors.text_primary,
                origin,
                None,
            );
        } else if !content.is_empty() {
            tr.draw_text(
                scene,
                content,
                base,
                colors.text_primary,
                transform * Affine::translate((origin.x, origin.y)),
                None,
            );
        }
    }

    /// Caret in the formula bar while editing.
    fn draw_formula_caret(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        editing: bool,
        text_x0: f64,
    ) {
        let colors = &theme.tokens.colors;
        if editing && let Some(caret) = self.grid.edit_caret() {
            let cx = self.formula_char_x.get(caret).copied().unwrap_or(text_x0);
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                colors.accent[5],
                None,
                &Line::new(
                    (cx, self.formula_field_rect.y0 + 5.0),
                    (cx, self.formula_field_rect.y1 - 5.0),
                ),
            );
        }
    }

    /// Bottom strip: sheet-tab switcher + the "+" add-sheet button. The
    /// selection summary and any transient action message are surfaced
    /// separately as a floating `ui::StatusBubble` — see `draw_status_bubble`
    /// — so this strip reads purely as navigation.
    fn draw_sheet_tabs_strip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let sm = theme.tokens.typography.scale.sm;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &self.tabs_rect,
        );
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
            let shape = RoundedRect::from_rect(*rect, f64::from(theme.tokens.radii.sm));
            let bg = if active {
                colors.surfaces.base
            } else {
                colors.surfaces.raised
            };
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
            // The tab's colour underline: its assigned palette colour when it
            // has one, otherwise the accent bar for the active tab.
            let tab_color = self
                .sheet_colors
                .get(*idx)
                .copied()
                .flatten()
                .map(tab_palette_color);
            let bar = tab_color.or(if active { Some(colors.accent[5]) } else { None });
            if let Some(bar_color) = bar {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    bar_color,
                    None,
                    &Rect::new(rect.x0 + 4.0, rect.y1 - 3.0, rect.x1 - 4.0, rect.y1),
                );
            }
            let name = self.sheet_names.get(*idx).cloned().unwrap_or_default();
            let (tw, _) = tr.measure(&name, sm, None);
            let tx = rect.center().x - f64::from(tw) / 2.0;
            let color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            tr.draw_text_weighted(
                scene,
                &name,
                sm,
                color,
                transform
                    * Affine::translate((tx, rect.y0 + (rect.height() - f64::from(sm)) / 2.0)),
                None,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                },
            );
        }
        let add_shape = RoundedRect::from_rect(self.add_tab_rect, f64::from(theme.tokens.radii.sm));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &add_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &add_shape,
        );
        tr.draw_text(
            scene,
            "+",
            theme.tokens.typography.scale.lg,
            colors.text_dim,
            transform
                * Affine::translate((
                    self.add_tab_rect.center().x - 4.0,
                    self.add_tab_rect.y0 + 4.0,
                )),
            None,
        );
    }

    /// Draw the transient floating status bubble at the bottom-right of the
    /// grid area, floated just *above* the sheet-tab strip so it never overlaps
    /// the tabs. It shows the composed status text (selection summary / active
    /// format, plus any fresh action message) when that text changes or while
    /// the pointer hovers the bottom-right reveal band, then fades. All colours
    /// come from the theme (via `draw_pill`).
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = std::time::Instant::now();
        let Some(label) = self.status_bubble_label() else {
            return;
        };
        let text_size = 12.0f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 12.0;
        // Lower bound: a few px above the tab strip, so the pill floats over
        // the grid's bottom-right corner and never covers the sheet tabs.
        let y1 = self.tabs_rect.y0 - 8.0;
        let rect = Rect::new(self.rect.x1 - margin - w, y1 - h, self.rect.x1 - margin, y1);
        // Reveal zone: the bottom-right corner band of the grid (plus the pill),
        // so nudging the pointer there re-surfaces the status on demand.
        let zone = Rect::new(
            rect.x0 - 40.0,
            rect.y0 - 24.0,
            self.rect.x1,
            self.tabs_rect.y0,
        );
        let over = self.shots_status || zone.contains(self.last_mouse);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
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
        let shape = RoundedRect::from_rect(self.dropdown_rect, f64::from(theme.tokens.radii.md));
        // Subtle shadow then panel.
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 60),
            None,
            &RoundedRect::from_rect(
                self.dropdown_rect + vello::kurbo::Vec2::new(0.0, 3.0),
                f64::from(theme.tokens.radii.md),
            ),
        );
        // Solid raised base first, then the overlay tint — keeps the panel
        // fully opaque so the grid never bleeds through.
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &shape,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &shape,
        );
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
                let rs = RoundedRect::from_rect(row.rect, f64::from(theme.tokens.radii.sm));
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
                        row.rect.y0 + (row.rect.height() - f64::from(sm)) / 2.0,
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
        let shape = RoundedRect::from_rect(self.panel_rect, f64::from(theme.tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &shape,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &shape,
        );

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
        for (label, &section_y) in ["Currency", "Date", "Time"]
            .iter()
            .zip(&self.settings_section_y)
        {
            tr.draw_text(
                scene,
                label,
                sm,
                colors.text_dim,
                transform * Affine::translate((label_x, section_y + 4.0)),
                None,
            );
        }

        self.draw_setting_chips(scene, transform, tr, theme, sm);

        // Footer: how the currently active cell renders under these settings.
        let footer = format!(
            "Applies to this workbook. Active cell: {}",
            self.current_active_format.label()
        );
        tr.draw_text(
            scene,
            &footer,
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform * Affine::translate((self.panel_rect.x0 + 20.0, self.panel_rect.y1 - 28.0)),
            None,
        );
    }

    /// The currency/date/time option chips inside the settings panel.
    fn draw_setting_chips(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        sm: f32,
    ) {
        let colors = &theme.tokens.colors;
        for chip in &self.setting_chips {
            let (label, selected) = self.chip_label_and_state(chip.kind);
            let shape = RoundedRect::from_rect(chip.rect, f64::from(theme.tokens.radii.sm));
            let bg = if selected {
                colors.accent[5].with_alpha_factor(0.85)
            } else {
                colors.surfaces.raised
            };
            scene.fill(Fill::NonZero, transform, bg, None, &shape);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                if selected {
                    colors.accent[5]
                } else {
                    colors.border_standard
                },
                None,
                &shape,
            );
            let (tw, _) = tr.measure(&label, sm, None);
            let tx = chip.rect.center().x - f64::from(tw) / 2.0;
            let ty = chip.rect.y0 + (chip.rect.height() - f64::from(sm)) / 2.0;
            let color = if selected {
                Color::WHITE
            } else {
                colors.text_primary
            };
            tr.draw_text(
                scene,
                &label,
                sm,
                color,
                transform * Affine::translate((tx, ty)),
                None,
            );
        }
    }

    /// Paint per-peer "presence-in-navigation" glow on the sheet tabs: for
    /// each sheet tab a remote peer is currently on, wash the tab in that
    /// peer's colour. Reads the same `PresenceKey::Tab { id: sheet_name }`
    /// scope the tabs are keyed by, so a peer who switches to another sheet
    /// lights up the tab that leads there.
    fn draw_sheet_tab_presence(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for (rect, idx) in &self.tab_rects {
            let Some(name) = self.sheet_names.get(*idx) else {
                continue;
            };
            let peers = presence.peers_on(&PresenceKey::Tab { id: name.clone() });
            if peers.is_empty() {
                continue;
            }
            let p0 = transform * vello::kurbo::Point::new(rect.x0, rect.y0);
            let p1 = transform * vello::kurbo::Point::new(rect.x1, rect.y1);
            let mist_rect = Rect::new(p0.x, p0.y, p1.x, p1.y);
            for user in peers {
                presence.draw_peer_mist(scene, theme, user, mist_rect);
            }
        }
    }

    fn draw_tab_menu(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.tab_menu_target.is_none() || self.tab_menu_rows.is_empty() {
            return;
        }
        let colors = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(self.tab_menu_rect, f64::from(theme.tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(0, 0, 0, 70),
            None,
            &RoundedRect::from_rect(
                self.tab_menu_rect + vello::kurbo::Vec2::new(0.0, 3.0),
                f64::from(theme.tokens.radii.md),
            ),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &shape,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &shape,
        );

        let sm = theme.tokens.typography.scale.sm;
        for (rect, act) in &self.tab_menu_rows {
            if rect.contains(self.last_mouse) {
                let rs = RoundedRect::from_rect(*rect, f64::from(theme.tokens.radii.sm));
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5].with_alpha_factor(0.22),
                    None,
                    &rs,
                );
            }
            let color = if matches!(act, TabAction::Delete) {
                colors.semantic.danger
            } else {
                colors.text_primary
            };
            tr.draw_text(
                scene,
                tab_action_label(*act),
                sm,
                color,
                transform
                    * Affine::translate((
                        rect.x0 + 10.0,
                        rect.y0 + (rect.height() - f64::from(sm)) / 2.0,
                    )),
                None,
            );
        }

        self.draw_tab_menu_swatches(scene, transform, tr, theme);
    }

    /// Colour label + swatch grid inside the tab context menu.
    fn draw_tab_menu_swatches(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        if let Some((first, _)) = self.tab_menu_swatches.first() {
            tr.draw_text(
                scene,
                "Colour",
                theme.tokens.typography.scale.xs,
                colors.text_dim,
                transform * Affine::translate((self.tab_menu_rect.x0 + 10.0, first.y0 - 16.0)),
                None,
            );
        }
        let current = self
            .tab_menu_target
            .and_then(|t| self.sheet_colors.get(t).copied().flatten());
        for (rect, color_opt) in &self.tab_menu_swatches {
            let shape = RoundedRect::from_rect(*rect, f64::from(theme.tokens.radii.sm));
            match color_opt {
                None => {
                    scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &shape);
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        colors.border_standard,
                        None,
                        &shape,
                    );
                    scene.stroke(
                        &Stroke::new(1.0),
                        transform,
                        colors.text_dim,
                        None,
                        &Line::new(
                            (rect.x0 + 3.0, rect.y1 - 3.0),
                            (rect.x1 - 3.0, rect.y0 + 3.0),
                        ),
                    );
                }
                Some(i) => {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        tab_palette_color(*i),
                        None,
                        &shape,
                    );
                }
            }
            let selected = current == *color_opt && color_opt.is_some();
            if selected || rect.contains(self.last_mouse) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    if selected {
                        Color::WHITE
                    } else {
                        colors.accent[5]
                    },
                    None,
                    &shape,
                );
            }
        }
    }

    fn draw_tab_rename(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(rn) = &mut self.tab_rename else {
            return;
        };
        // Theme typography remains authoritative even though the edit began
        // before the draw pass had a Theme reference.
        rn.editor.set_font_size(theme.tokens.typography.scale.sm);
        rn.editor.apply_layout_external(rn.rect);
        rn.editor
            .draw(scene, transform, tr, theme, &PresenceCtx::default());
    }

    fn chip_label_and_state(&self, kind: SettingKind) -> (String, bool) {
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

    /// The text for the floating status bubble, or `None` when there is nothing
    /// worth surfacing (an idle sheet: no pending action, no numeric selection,
    /// a plain `General` active cell — which keeps the bubble hidden).
    ///
    /// Primary text is the live selection summary (Sum / Avg / Count over the
    /// numeric selection) or, absent numbers, the active cell's format. A fresh
    /// transient action message (e.g. "Pasted into B2") is prepended when
    /// present; the idle default `"Ready"` (and the empty string) count as "no
    /// action message" so they never surface on their own.
    fn status_bubble_label(&self) -> Option<String> {
        let summary = self.selection_summary().map(|s| {
            format!(
                "Sum {}    Avg {}    Count {}",
                format_value(&CellValue::Number(s.sum), s.fmt),
                format_value(&CellValue::Number(s.avg), s.fmt),
                s.count,
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
            (Some(a), Some(s)) => Some(format!("{a}    ·    {s}")),
            (Some(a), None) => Some(a),
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

/// Flatten a cell's display text into a single TSV field: tabs and newlines
/// would break the row/column structure, so they collapse to spaces.
fn tsv_cell(display: &str) -> String {
    display.replace(['\t', '\n', '\r'], " ")
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

/// A new sheet name derived from an imported file's stem.
fn sheet_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(std::string::ToString::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported".to_string())
}

/// Cumulative x positions (layout space) of every character boundary in
/// `text` when laid out at `size` starting at `x0`. The returned vector has
/// `text.chars().count() + 1` entries: `[0]` is `x0` and the last is the text's
/// right edge. Used to map a click x to a caret index and to place the caret.
fn char_boundaries(tr: &mut TextRenderer, text: &str, size: f32, x0: f64) -> Vec<f64> {
    let chars: Vec<char> = text.chars().collect();
    let mut xs = Vec::with_capacity(chars.len() + 1);
    xs.push(x0);
    let mut prefix = String::new();
    for ch in &chars {
        prefix.push(*ch);
        let (w, _) = tr.measure(&prefix, size, None);
        xs.push(x0 + f64::from(w));
    }
    xs
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
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.pending.seeded {
            // Pull the shared global defaults once at startup.
            self.global = GlobalSettings::load();
            self.initialize(state);
            self.pending.seeded = true;
        }
        // Land a finished import/export file dialog before reloading, so the
        // imported sheet (or the export's status) is reflected this frame.
        self.drain_io(state);
        self.reload(state);
        self.current_active_format = self.format_of_active();
        // Surface the floating status bubble only when the composed label
        // actually changes — a selection move/extend, or a fresh action
        // message. `sync` runs every frame, so poking unconditionally would
        // pin the bubble open forever; and the idle nothing state (empty
        // label) never surfaces, so a fresh sheet shows no bubble.
        let label = self.status_bubble_label().unwrap_or_default();
        if label != self.last_status_label {
            if !label.is_empty() {
                self.status_bubble.poke(std::time::Instant::now());
            }
            self.last_status_label = label;
        }
        self.persist_if_dirty(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
        self.draw_chrome(scene, transform, text_renderer, theme);
        self.grid.draw(scene, transform, text_renderer, theme);
        // The transient floating status bubble sits above the grid + tab strip
        // (the reclaimed home of the old bottom status text), below the modal
        // overlays that follow.
        self.draw_status_bubble(scene, transform, text_renderer, theme);
        // Overlays paint last so they sit above the grid.
        self.draw_dropdown(scene, transform, text_renderer, theme);
        self.draw_settings_panel(scene, transform, text_renderer, theme);
        self.draw_tab_menu(scene, transform, text_renderer, theme);
        self.draw_tab_rename(scene, transform, text_renderer, theme);

        // The command palette floats above everything else, behind a scrim.
        if self.palette.is_open() {
            let scrim = Color::from_rgba8(0, 0, 0, 110);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.rect);
            let (pw, ph) = self.palette.desired_size();
            let x0 = self.rect.center().x - pw / 2.0;
            let y0 = self.rect.y0 + (self.rect.height() * 0.12).max(24.0);
            self.palette.set_rect(Rect::new(x0, y0, x0 + pw, y0 + ph));
            self.palette
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while the floating status bubble is lingering /
        // fading, so it hides itself without needing a stray pointer event.
        self.status_bubble.needs_frame(std::time::Instant::now())
            || self.children().iter().any(|c| c.needs_animation_frame())
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
        // Presence-in-navigation: when a peer is on a DIFFERENT sheet,
        // glow that sheet's tab in their colour so you can see where they
        // went ("Alice is on Q2"). Painted after the chrome so it reads on
        // top of the tab surface.
        self.draw_sheet_tab_presence(scene, transform, theme, presence);
    }

    /// Announce which sheet the local user is viewing, so peers can flag
    /// that tab. Keyed by sheet name (the same key
    /// [`Self::draw_sheet_tab_presence`] reads on the receiving side).
    fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
        let name = self.sheet_names.get(self.active_sheet)?;
        Some((
            PresenceKey::Tab { id: name.clone() },
            PresencePayload::Arrived,
        ))
    }

    /// Publish the open workbook + its sheets so the host's Ctrl+K action
    /// palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records one undo boundary bracketing it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the workbook's sheets into the unified Ctrl+K palette: a snapshot
    /// [`SheetSearchProvider`](crate::search_provider::SheetSearchProvider)
    /// makes every sheet findable there by name, alongside the actions the app
    /// exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::SheetSearchProvider::new(
            &self.sheet_names,
        ))]
    }

    /// Switch to the sheet a chosen palette row identifies.
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(idx) = crate::search_provider::SheetSearchProvider::parse_action_id(action_id)
            && idx < self.sheet_names.len()
        {
            self.switch_sheet(doc, idx);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if self.route_inline_editor(
            event,
            scale_factor,
            *modifiers,
            state,
            handled,
            &mut PaletteRouteCtx {
                path,
                focus_target,
                node_id,
                taffy,
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
                self.handle_left_press(*modifiers, state, path, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.grid.pointer_up();
                // A completed fill-handle drag commits its expansion here.
                self.run_pending_fill(state);
                self.formula_bar_selecting = false;
                self.pending.redraw = true;
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.handle_right_press(handled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y, scale_factor, handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, scale_factor, handled);
            }
            WindowEvent::KeyboardInput { .. } => {
                let mut ctx = PaletteRouteCtx {
                    path,
                    focus_target,
                    node_id,
                    taffy,
                };
                self.handle_keyboard_input(
                    event,
                    scale_factor,
                    *modifiers,
                    state,
                    handled,
                    &mut ctx,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SheetsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.pending.redraw || self.grid.dirty()
    }

    fn mark_drawn(&mut self) {
        self.pending.redraw = false;
        self.grid.mark_drawn();
    }

    fn needs_commit(&self) -> bool {
        self.pending.commit
    }

    fn on_commit(&mut self) {
        self.pending.commit = false;
    }
}

impl SheetsApp {
    /// Route every input kind through the active shared inline editor before
    /// the spreadsheet's grid or chrome can see it. Both editors are modal in
    /// Sheets: a tab rename blur-commits, while a Name-box blur cancels.
    fn route_inline_editor(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
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
                    scale_factor,
                    &modifiers,
                    state,
                    ctx.path,
                    handled,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.taffy,
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
                    scale_factor,
                    &modifiers,
                    state,
                    ctx.path,
                    handled,
                    ctx.focus_target,
                    ctx.node_id,
                    ctx.taffy,
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

    /// Left-press: routes to whichever modal surface is open (palette,
    /// rename, name box, tab menu, settings panel, dropdown menu), then the
    /// menu bar / name box / fx / formula field / sheet tabs, and finally the
    /// grid body, in priority order.
    fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let p = self.last_mouse;
        if self.try_palette_click(p, state, path, handled) {
            return;
        }
        if self.try_tab_menu_click(p, state, handled) {
            return;
        }
        if self.try_settings_panel_click(p, state, handled) {
            return;
        }
        if self.try_dropdown_click(p, state, handled) {
            return;
        }
        if self.try_menu_bar_click(p, state, handled) {
            return;
        }
        if self.try_chrome_click(p, modifiers, state, handled) {
            return;
        }
        self.try_grid_click(p, modifiers, state, handled);
    }

    /// The command palette is modal while open: a click on a row runs it, a
    /// click elsewhere dismisses it.
    fn try_palette_click(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if !self.palette.is_open() {
            return false;
        }
        if let Some(i) = self.palette.row_at(p) {
            self.palette.set_cursor(i);
            self.palette.activate_selected(state, path);
            if let Some(outcome) = self.palette.take_outcome() {
                self.dispatch_command(state, &outcome.id, outcome.argument.as_deref());
            }
        } else if !Widget::get_rect(&self.palette).contains(p) {
            self.palette.close();
        }
        self.pending.redraw = true;
        *handled = true;
        true
    }

    /// Open tab context menu: route clicks to its rows / swatches or dismiss
    /// it.
    fn try_tab_menu_click(&mut self, p: Point, state: &mut AutoCommit, handled: &mut bool) -> bool {
        let Some(target) = self.tab_menu_target else {
            return false;
        };
        if let Some(act) = self
            .tab_menu_rows
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, a)| *a)
        {
            self.tab_menu_target = None;
            self.run_tab_action(state, target, act);
            self.pending.redraw = true;
            *handled = true;
            return true;
        }
        if let Some(color) = self
            .tab_menu_swatches
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, c)| *c)
        {
            self.tab_menu_target = None;
            self.run_tab_action(state, target, TabAction::SetColor(color));
            self.pending.redraw = true;
            *handled = true;
            return true;
        }
        if !self.tab_menu_rect.contains(p) {
            self.tab_menu_target = None;
            self.pending.redraw = true;
        }
        *handled = true;
        true
    }

    /// Settings panel is modal: clicks go to it (or dismiss it).
    fn try_settings_panel_click(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if self.settings_panel == SettingsPanel::None {
            return false;
        }
        if let Some(kind) = self.setting_chip_at(p) {
            self.apply_setting(state, kind);
            self.pending.redraw = true;
            *handled = true;
            return true;
        }
        if !self.panel_rect.contains(p) {
            self.settings_panel = SettingsPanel::None;
            self.pending.redraw = true;
        }
        *handled = true;
        true
    }

    /// Open dropdown takes priority: a click on an item runs it (consumed).
    /// A click outside the dropdown dismisses it but still falls through so
    /// a menu-bar click underneath can open a different menu.
    fn try_dropdown_click(&mut self, p: Point, state: &mut AutoCommit, handled: &mut bool) -> bool {
        if self.open_menu == OpenMenu::None {
            return false;
        }
        if let Some(item) = self.dropdown_item_at(p) {
            self.open_menu = OpenMenu::None;
            self.run_menu_item(state, &item);
            self.pending.redraw = true;
            *handled = true;
            return true;
        }
        if !self.dropdown_rect.contains(p) && !self.menubar_rect.contains(p) {
            self.open_menu = OpenMenu::None;
            self.pending.redraw = true;
        }
        false
    }

    /// Menu bar: opens/closes a dropdown, or runs a plain menu item.
    fn try_menu_bar_click(&mut self, p: Point, state: &mut AutoCommit, handled: &mut bool) -> bool {
        if !self.menubar_rect.contains(p) {
            return false;
        }
        // Take what the hit says before touching `self`: `run_menu_item` needs
        // `&mut self`, which cannot coexist with the borrow `find` holds.
        let hit = self
            .menu_buttons
            .iter()
            .find(|b| b.rect.contains(p))
            .map(|b| (b.opens, b.item.clone()));
        if let Some((opens, item)) = hit {
            if let Some(opens) = opens {
                self.open_menu = if self.open_menu == opens {
                    OpenMenu::None
                } else {
                    opens
                };
            } else if let Some(item) = item {
                self.run_menu_item(state, &item);
            }
            self.pending.redraw = true;
            *handled = true;
        }
        true
    }

    /// Chrome hit-tests below the modal/menu checks: name box, "fx", formula
    /// field, and the sheet-tab strip.
    fn try_chrome_click(
        &mut self,
        p: Point,
        modifiers: Modifiers,
        state: &mut AutoCommit,
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
            self.grid.set_edit_caret(idx, shift);
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
    fn try_grid_click(
        &mut self,
        p: Point,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        let now = Instant::now();
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

    fn handle_right_press(&mut self, handled: &mut bool) {
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
        }
    }

    fn handle_cursor_moved(&mut self, x: f64, y: f64, scale_factor: f64, handled: &mut bool) {
        self.last_mouse = Point::new(x / scale_factor, y / scale_factor);
        // A drag that began in the formula bar extends the text
        // selection there, not the grid selection.
        if self.formula_bar_selecting {
            let idx = self.formula_caret_at_x(self.last_mouse.x);
            self.grid.set_edit_caret(idx, true);
            self.pending.redraw = true;
            *handled = true;
            return;
        }
        if self.open_menu != OpenMenu::None || self.settings_panel != SettingsPanel::None {
            self.pending.redraw = true;
        }
        if self.grid.pointer_move(self.last_mouse) {
            self.pending.redraw = true;
            *handled = true;
        }
    }

    fn handle_mouse_wheel(
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

    /// The command palette (when open) owns the keyboard: arrows, Enter, Esc
    /// and typing are consumed by it and never reach the grid behind. On
    /// Enter it emits an outcome we dispatch here. Otherwise: Ctrl+K opens
    /// the palette, Escape closes an overlay, clipboard chords route to either
    /// the grid selection or an in-progress edit's text selection, and
    /// anything left falls through to the grid's own key handling. Inline
    /// editors are routed before this method by [`Self::route_inline_editor`].
    fn handle_keyboard_input(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
        ctx: &mut PaletteRouteCtx<'_>,
    ) {
        if self.palette.is_open() {
            self.palette.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                ctx.path,
                handled,
                ctx.focus_target,
                ctx.node_id,
                ctx.taffy,
            );
            if let Some(outcome) = self.palette.take_outcome() {
                self.dispatch_command(state, &outcome.id, outcome.argument.as_deref());
            }
            self.pending.redraw = true;
            *handled = true;
            return;
        }
        // Ctrl+K opens the command palette.
        if let WindowEvent::KeyboardInput { event: ke, .. } = event {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
            if ke.state.is_pressed()
                && ctrl
                && !self.grid.is_editing()
                && let winit::keyboard::Key::Character(c) = &ke.logical_key
                && c.as_str().eq_ignore_ascii_case("k")
            {
                self.open_command_palette();
                *handled = true;
                return;
            }
        }

        // Escape closes any open overlay.
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state.is_pressed()
            && matches!(
                ke.logical_key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            )
            && (self.open_menu != OpenMenu::None
                || self.settings_panel != SettingsPanel::None
                || self.tab_menu_target.is_some())
        {
            self.open_menu = OpenMenu::None;
            self.settings_panel = SettingsPanel::None;
            self.tab_menu_target = None;
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

    /// Ctrl+C/X/V/A/D/R while the grid selection (not a cell edit) owns the
    /// keyboard. Returns whether the chord was handled.
    fn handle_grid_clipboard_chord(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        state: &mut AutoCommit,
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

    /// Clipboard + select-all chords while a cell/formula-bar edit owns the
    /// keyboard, operating on the edit buffer's text selection (so Ctrl+V
    /// pastes text instead of inserting the letter 'v'). Returns whether the
    /// chord was handled.
    fn handle_edit_clipboard_chord(
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
}

impl SheetsApp {
    fn cell_under(&self, p: Point) -> Option<Addr> {
        if self.grid_rect.contains(p) {
            self.grid.cell_at_public(p)
        } else {
            None
        }
    }

    /// Map a layout-space x within the formula bar to the nearest character
    /// boundary (caret index), using the per-character offsets cached on the
    /// last draw. Falls back to `0` before the first frame.
    fn formula_caret_at_x(&self, x: f64) -> usize {
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

    fn run_menu_item(&mut self, doc: &mut AutoCommit, item: &MenuItem) {
        match item {
            MenuItem::NewSheet => self.add_sheet(doc),
            MenuItem::ImportCsv => self.import_csv(doc),
            MenuItem::ExportCsv => self.export_csv(doc),
            MenuItem::ImportXlsx => self.import_xlsx(doc),
            MenuItem::ExportXlsx => self.export_xlsx(doc),
            MenuItem::ImportOds => self.import_ods(doc),
            MenuItem::ExportOds => self.export_ods(doc),
            MenuItem::SetFormat(f) => self.apply_format(doc, *f),
            MenuItem::ClearFormat => self.clear_format(doc),
            MenuItem::OpenDocSettings => {
                self.settings_panel = SettingsPanel::Document;
                self.open_menu = OpenMenu::None;
                self.relayout_overlays();
            }
            MenuItem::AutoSum => self.auto_sum(doc),
        }
        self.pending.redraw = true;
    }

    /// Open the right-click context menu for sheet tab `idx`, closing any other
    /// overlay first.
    fn open_tab_menu(&mut self, idx: usize) {
        self.open_menu = OpenMenu::None;
        self.settings_panel = SettingsPanel::None;
        self.tab_rename = None;
        self.tab_menu_target = Some(idx);
        self.layout_tab_menu();
        self.pending.redraw = true;
    }

    /// Begin an inline rename of sheet tab `idx`, seeding the field with its
    /// current name.
    fn begin_tab_rename(&mut self, idx: usize) {
        let name = self.sheet_names.get(idx).cloned().unwrap_or_default();
        let tab = self
            .tab_rects
            .iter()
            .find(|(_, i)| *i == idx)
            .map_or(self.add_tab_rect, |(r, _)| *r);
        // Give the editor a little more room than the tab so a longer name fits.
        let right = (tab.x0 + 150.0).min(self.rect.x1 - 8.0).max(tab.x1);
        let rect = Rect::new(tab.x0, tab.y0, right, tab.y1);
        let mut editor = EditableLabel::new(&name)
            .with_font_size(14.0)
            .with_id("sheet_tab_rename");
        editor.begin_edit();
        self.tab_menu_target = None;
        self.tab_rename = Some(TabRename { idx, rect, editor });
        self.pending.redraw = true;
    }

    fn resolve_tab_rename_outcome(&mut self, doc: &mut AutoCommit, outcome: EditOutcome) {
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
                    self.perform_action(doc, &actions::rename_sheet_action(rename.idx, trimmed));
                    self.reload(doc);
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

    // --- name box (Go-To) + fx formula entry -------------------------------

    /// Open the Name box for a Go-To entry, seeded with the active cell's
    /// reference and fully selected (Excel-style: the first keystroke replaces
    /// it). Closes any other overlay so the box owns input.
    fn begin_name_box_edit(&mut self) {
        self.open_menu = OpenMenu::None;
        self.settings_panel = SettingsPanel::None;
        self.tab_menu_target = None;
        let (c, r) = self.grid.active();
        let reference = crate::model::addr_to_a1(c, r);
        let mut input = TextInput::local()
            .with_initial_value(&reference)
            .with_char_filter(reference_box_char)
            .with_plain_chrome()
            .with_font_size(14.0)
            .with_enter_submits(true)
            .with_id("sheet_name_box");
        input.set_focus(true);
        input.select_all();
        self.name_box_edit = Some(input);
        self.pending.redraw = true;
    }

    /// Commit a Name box entry: parse it as a single reference or a range and
    /// move the selection there (scrolling it into view), or report an invalid
    /// reference without changing the selection.
    fn commit_name_box(&mut self) {
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

    /// The "fx" formula-insert affordance: focus the active cell for a formula
    /// entry, seeding it with `=` (prepended to any existing content) and
    /// placing the caret at the end so typing continues the formula.
    fn start_formula_entry(&mut self) {
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

    /// Execute a tab context-menu action against sheet `idx`.
    fn run_tab_action(&mut self, doc: &mut AutoCommit, idx: usize, act: TabAction) {
        match act {
            TabAction::Rename => self.begin_tab_rename(idx),
            TabAction::Duplicate => {
                if idx < SheetsDoc::sheet_count(doc) {
                    self.perform_action(doc, &actions::duplicate_sheet_action(idx));
                    // The duplicate lands immediately after the source.
                    SheetsDoc::set_active_index(doc, idx + 1);
                    self.grid.select_only((0, 0));
                    self.reload(doc);
                    self.status = "Duplicated sheet".into();
                }
            }
            TabAction::InsertRight => {
                let n = SheetsDoc::sheet_count(doc);
                let at = (idx + 1).min(n);
                self.perform_action(
                    doc,
                    &actions::insert_sheet_action(at, &format!("Sheet{}", n + 1)),
                );
                SheetsDoc::set_active_index(doc, at);
                self.grid.select_only((0, 0));
                self.reload(doc);
                self.status = "Inserted sheet".into();
            }
            TabAction::MoveLeft => {
                if idx > 0 {
                    self.perform_action(doc, &actions::move_sheet_action(idx, idx - 1));
                    SheetsDoc::set_active_index(doc, idx - 1);
                    self.reload(doc);
                    self.status = "Moved sheet left".into();
                }
            }
            TabAction::MoveRight => {
                if idx + 1 < SheetsDoc::sheet_count(doc) {
                    self.perform_action(doc, &actions::move_sheet_action(idx, idx + 1));
                    SheetsDoc::set_active_index(doc, idx + 1);
                    self.reload(doc);
                    self.status = "Moved sheet right".into();
                }
            }
            TabAction::Delete => {
                let count_before = SheetsDoc::sheet_count(doc);
                if count_before <= 1 {
                    self.status = "A workbook keeps at least one sheet".into();
                } else {
                    let old_active = SheetsDoc::active_index(doc);
                    self.perform_action(doc, &actions::delete_sheet_action(idx));
                    if SheetsDoc::sheet_count(doc) < count_before {
                        let count = SheetsDoc::sheet_count(doc);
                        let new_active = match idx.cmp(&old_active) {
                            std::cmp::Ordering::Less => old_active - 1,
                            std::cmp::Ordering::Equal => old_active.min(count - 1),
                            std::cmp::Ordering::Greater => old_active,
                        };
                        SheetsDoc::set_active_index(doc, new_active.min(count - 1));
                        self.grid.select_only((0, 0));
                        self.reload(doc);
                        self.status = "Deleted sheet".into();
                    }
                }
            }
            TabAction::SetColor(color) => {
                self.perform_action(doc, &actions::set_sheet_color_action(idx, color));
                self.reload(doc);
                self.status = if color.is_some() {
                    "Set tab colour".into()
                } else {
                    "Cleared tab colour".into()
                };
            }
        }
        self.pending.redraw = true;
    }
}

/// Seed a realistic two-sheet workbook: a monthly budget with SUM /
/// AVERAGE / IF formulas, and a quarterly sales sheet with growth %. The
/// money columns are typed via smart entry so they pick up currency
/// automatically; one row shows a date and a percent for the screenshot.
fn seed_workbook(doc: &mut AutoCommit) {
    seed_budget_sheet(doc);
    seed_sales_sheet(doc);
}

fn seed_budget_sheet(doc: &mut AutoCommit) {
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
            setf(
                doc,
                &format!("{col}{r}"),
                CellFormat::Currency(Currency::USD),
            );
        }
        setf(doc, &format!("F{r}"), CellFormat::Number);
    }
    setf(doc, "B9", CellFormat::Currency(Currency::USD));
    setf(doc, "B11", CellFormat::Currency(Currency::USD));
    // A date in the "Updated" column, smart-typed style.
    set(doc, "G2", "2024-01-15");
    setf(doc, "G2", CellFormat::Date(DateStyle::Iso));
}

/// Sheet 2: Sales (EUR document feel via explicit currency).
fn seed_sales_sheet(doc: &mut AutoCommit) {
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
            ssetf(
                doc,
                &format!("{col}{r}"),
                CellFormat::Currency(Currency::EUR),
            );
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
        app.cache
            .as_ref()
            .and_then(|c| c.values.get(&addr).cloned())
    }

    /// Presence-in-navigation, producer side: the app announces which
    /// sheet the local user is on as a `Tab { id: sheet_name }` scope, so
    /// peers can flag that tab. Switching sheets moves the announced scope.
    #[test]
    fn presence_payload_announces_active_sheet() {
        let (mut app, _doc) = seeded_app();
        assert!(!app.sheet_names.is_empty(), "seeded app has sheets");

        let (key, payload) = ui::widget::Widget::presence_payload(&app).expect("a sheet is active");
        assert_eq!(
            key,
            PresenceKey::Tab {
                id: app.sheet_names[app.active_sheet].clone()
            },
            "the announced scope must be the active sheet's tab"
        );
        assert_eq!(payload, PresencePayload::Arrived);

        // Switching sheets re-targets the announcement at the new tab.
        if app.sheet_names.len() > 1 {
            let other = (app.active_sheet + 1) % app.sheet_names.len();
            app.active_sheet = other;
            let (key2, _) = ui::widget::Widget::presence_payload(&app).expect("still active");
            assert_eq!(
                key2,
                PresenceKey::Tab {
                    id: app.sheet_names[other].clone()
                }
            );
        }
    }

    #[test]
    fn selected_formula_bar_shows_colored_body() {
        let (app, _doc) = seeded_app();
        // After seeding, E2 (a SUM formula) is the active cell and NOT editing.
        assert_eq!(app.grid.active(), (4, 1));
        assert!(!app.grid.is_editing());
        assert_eq!(app.grid.active_display_raw(), "=SUM(B2:D2)");
        // The selected-formula highlight must be available without editing.
        assert_eq!(
            app.grid.highlight_formula_body().as_deref(),
            Some("SUM(B2:D2)")
        );
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
                GridCmd::SetCell {
                    addr: b2,
                    raw: "1000".into(),
                },
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

    /// Format applied over a discontiguous multi-range selection reaches every
    /// cell in *both* ranges — the real `apply_format` → `selected_cells` path.
    #[test]
    fn format_applies_across_a_multi_range_selection() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 2)); // B2:B3
        app.grid.add_range_public((3, 1), (3, 2)); // D2:D3 (discontiguous)
        app.apply_format(&mut doc, CellFormat::Percent);
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
        for a in [(1, 1), (1, 2), (3, 1), (3, 2)] {
            assert_eq!(
                snap.fmt.get(&a).copied(),
                Some(CellFormat::Percent),
                "cell {a:?}"
            );
        }
        // The gap column between the two ranges is untouched (still currency).
        assert!(matches!(
            snap.fmt.get(&(2, 1)),
            Some(CellFormat::Currency(_))
        ));
    }

    /// The status-bar summary aggregates Sum/Avg/Count over a *discontiguous*
    /// multi-range selection — the real `selection_summary` → `selected_cells`
    /// path the status bar draws each frame. Column B (Jan) sums to 2040 and
    /// column D (Mar) to 2180 in the seed, so the two-range total is 4220 over
    /// 10 numeric cells.
    #[test]
    fn multi_range_selection_summary_aggregates_both_ranges() {
        let (mut app, _doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 5)); // B2:B6
        app.grid.add_range_public((3, 1), (3, 5)); // D2:D6 (discontiguous)
        assert!(app.grid.is_multi_range(), "two ranges are selected");

        let s = app
            .selection_summary()
            .expect("a numeric multi-range selection has a status-bar summary");
        assert_eq!(s.count, 10, "5 cells from B2:B6 + 5 from D2:D6");
        assert!(
            (s.sum - 4220.0).abs() < 1e-6,
            "sum over both ranges, got {}",
            s.sum
        );
        assert!(
            (s.avg - 422.0).abs() < 1e-6,
            "avg = 4220 / 10, got {}",
            s.avg
        );
    }

    /// The real export → import path for a workbook of formulas: the active
    /// sheet is exported to a `.xlsx`, read back, and imported into a fresh
    /// document. Formula cells restore their formula *text* (not a frozen
    /// value) and recompute to the original results.
    #[test]
    fn xlsx_export_import_restores_live_formulas() {
        let (app, mut doc) = seeded_app();
        // Budget!E2 = =SUM(B2:D2) = 3600 in the seed.
        let table = app.active_table(&mut doc);
        let path = std::env::temp_dir().join("sheets-app-formula-rt.xlsx");
        crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
        let tables = crate::xlsx::read_xlsx(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // Import into a brand-new, empty document via the real import path.
        let mut fresh = SheetsApp::new();
        let mut doc2 = AutoCommit::new();
        fresh.import_tables(&mut doc2, tables, "test.xlsx");

        // The formula came back as a formula, not its computed value.
        assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (4, 1)), "=SUM(B2:D2)");
        // …and it recomputes to the original result.
        let snap = SheetsDoc::snapshot_sheet(&mut doc2, 0, Currency::USD);
        let values = formula::recalculate(&snap.raw);
        assert_eq!(values.get(&(4, 1)), Some(&CellValue::Number(3600.0)));
        // A non-formula neighbour still round-trips as a plain value.
        assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (1, 1)), "1200");
    }

    /// Fill-down copies the top cell of the selection down, shifting relative
    /// formula references per row — the real Ctrl+D path.
    #[test]
    fn fill_down_copies_and_shifts_formulas() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((4, 1), (4, 3)); // E2:E4, E2 = =SUM(B2:D2)
        app.fill_down(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 2)), "=SUM(B3:D3)");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 3)), "=SUM(B4:D4)");
        // E3 recomputes against row 3's operands: 460 + 510 + ... etc > 0.
        match value_at(&app, (4, 2)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
            other => panic!("expected numeric fill result, got {other:?}"),
        }
    }

    /// The status-bar summary counts numeric cells across *all* ranges of a
    /// discontiguous selection.
    #[test]
    fn selection_summary_spans_multiple_ranges() {
        let (mut app, _doc) = seeded_app();
        app.grid.select_range_public((1, 1), (1, 2)); // B2:B3 = 1200, 420
        app.grid.add_range_public((2, 1), (2, 2)); // C2:C3 = 1200, 460
        let s = app
            .selection_summary()
            .expect("numeric multi-range selection");
        assert_eq!(s.count, 4);
        assert!(
            (s.sum - (1200.0 + 420.0 + 1200.0 + 460.0)).abs() < 1e-6,
            "sum={}",
            s.sum
        );
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
        let _clip = CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 1)); // B2 = 1200, currency
        app.copy_selection(false);
        app.grid.select_only((1, 7)); // B8, empty
        app.paste(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (1, 7)), "1200");
        assert!(matches!(
            app.grid.cell_fmt_at((1, 7)),
            CellFormat::Currency(_)
        ));
        assert_eq!(value_at(&app, (1, 7)), Some(CellValue::Number(1200.0)));
    }

    #[test]
    fn copy_paste_shifts_relative_formula_refs() {
        // Copy E2 (`=SUM(B2:D2)`) and paste it over F3: the relative refs
        // shift by (+1 col, +1 row) to `=SUM(C3:E3)`, overwriting F3's seeded
        // AVERAGE, and the pasted formula recomputes against the new operands.
        let _clip = CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let _clip = CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0)); // A1 = "Category"
        app.copy_selection(true); // cut
        app.grid.select_only((0, 20)); // A21, empty
        app.paste(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 20)), "Category");
    }

    /// A **cut** moves a formula verbatim — its references follow the cells and
    /// are *not* re-relativised (unlike a copy). The real cut→paste path.
    #[test]
    fn cut_paste_formula_does_not_relativize() {
        let _clip = CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        app.copy_selection(true); // cut
        app.grid.select_only((6, 10)); // G11, empty, far away
        app.paste(&mut doc);
        // The formula is unchanged: a move keeps references pointing at B2:D2.
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (6, 10)), "=SUM(B2:D2)");
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (4, 1)),
            "",
            "source cleared"
        );
    }

    /// Pasting a copied formula into a **range** tiles the block, offsetting
    /// each destination cell's relative references by its own position — the
    /// real range-paste path.
    #[test]
    fn paste_tiles_a_formula_across_a_range() {
        let _clip = CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        app.copy_selection(false);
        // Target a 1×3 range F2:F4; the 1×1 block tiles down it.
        app.grid.select_range_public((5, 1), (5, 3));
        app.paste(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 1)), "=SUM(C2:E2)");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 2)), "=SUM(C3:E3)");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 3)), "=SUM(C4:E4)");
    }

    /// The fill handle: dragging it from a two-number selection extends a
    /// linear series, driven through the production `pointer_up` →
    /// `run_pending_fill` path (no display).
    #[test]
    fn fill_handle_drag_extends_numeric_series() {
        let (mut app, mut doc) = seeded_app();
        app.apply_cmds(
            &mut doc,
            vec![
                GridCmd::SetCell {
                    addr: (7, 1),
                    raw: "1".into(),
                }, // H2
                GridCmd::SetCell {
                    addr: (7, 2),
                    raw: "2".into(),
                }, // H3
                GridCmd::Commit,
            ],
        );
        app.grid.select_range_public((7, 1), (7, 2)); // the pattern
        app.grid.start_fill_to((7, 5)); // drag the handle down to H6
        app.run_pending_fill(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (7, 3)), "3");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (7, 4)), "4");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (7, 5)), "5");
        assert_eq!(value_at(&app, (7, 5)), Some(CellValue::Number(5.0)));
    }

    /// The fill handle on a formula cell fills down with the relative refs
    /// shifted per row — the same adjustment copy/paste applies.
    #[test]
    fn fill_handle_drag_shifts_formula_refs() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
        app.grid.start_fill_to((4, 4)); // drag down to E5
        app.run_pending_fill(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 2)), "=SUM(B3:D3)");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 3)), "=SUM(B4:D4)");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 4)), "=SUM(B5:D5)");
        match value_at(&app, (4, 4)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
            other => panic!("expected numeric fill result, got {other:?}"),
        }
    }

    /// The fill handle also fills to the right, extending a series across
    /// columns per row.
    #[test]
    fn fill_handle_drag_fills_right() {
        let (mut app, mut doc) = seeded_app();
        app.apply_cmds(
            &mut doc,
            vec![
                GridCmd::SetCell {
                    addr: (7, 10),
                    raw: "10".into(),
                }, // H11
                GridCmd::SetCell {
                    addr: (8, 10),
                    raw: "20".into(),
                }, // I11
                GridCmd::Commit,
            ],
        );
        app.grid.select_range_public((7, 10), (8, 10));
        app.grid.start_fill_to((10, 10)); // drag right to K11
        app.run_pending_fill(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (9, 10)), "30");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (10, 10)), "40");
    }

    /// The copy/paste + fill relativisation the app dispatches goes through the
    /// formula layer's `offset_refs`; prove the wiring on the shapes the paste
    /// path relies on (the full matrix lives in `formula`'s own tests).
    #[test]
    fn shift_refs_handles_anchors_functions_and_numbers() {
        assert_eq!(formula::offset_refs("=A1+$B$2", 1, 1), "=B2+$B$2");
        assert_eq!(formula::offset_refs("=SUM(A1:A3)*2", 2, 0), "=SUM(C1:C3)*2");
        // Mixed anchors: `$A1` shifts row only, `A$1` shifts column only.
        assert_eq!(formula::offset_refs("=$A1+A$1", 1, 1), "=$A2+B$1");
        // Scientific-notation literals are not references.
        assert_eq!(formula::offset_refs("=1e3+A1", 0, 1), "=1e3+A2");
        // Function names + string literals are left untouched.
        assert_eq!(
            formula::offset_refs("=IF(A1>0,\"A1\",B2)", 0, 1),
            "=IF(A2>0,\"A1\",B3)"
        );
        // A plain literal is returned unchanged.
        assert_eq!(formula::offset_refs("1200", 3, 3), "1200");
    }

    /// The layout is re-derived from the current rect on every pass, so a
    /// resize between widths reflows rather than freezing at the first size.
    /// Drives the production `relayout` on one instance at two widths.
    #[test]
    fn layout_reflows_on_resize() {
        let (mut app, _doc) = seeded_app();

        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.relayout();
        let wide_grid_w = app.grid_rect.width();
        let wide_field_x1 = app.formula_field_rect.x1;
        assert!(
            (app.grid_rect.x1 - 1280.0).abs() < 0.5,
            "wide grid tracks rect"
        );

        // Simulate a resize to the narrow viewport on the SAME instance.
        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.relayout();
        assert!(
            (app.grid_rect.x1 - 560.0).abs() < 0.5,
            "narrow grid tracks the new rect"
        );
        assert!(
            app.grid_rect.width() < wide_grid_w,
            "grid reflowed narrower"
        );
        assert!(
            app.formula_field_rect.x1 < wide_field_x1,
            "formula field reflowed"
        );

        // And back to wide — proves it isn't a one-way latch.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.relayout();
        assert!(
            (app.grid_rect.width() - wide_grid_w).abs() < 0.5,
            "reflows back to wide"
        );
    }

    // --- sheet tab context-menu actions ------------------------------------

    #[test]
    fn tab_rename_updates_sheet_name() {
        let (mut app, mut doc) = seeded_app();
        app.begin_tab_rename(0);
        for ch in ["L", "e", "d", "g", "e", "r"] {
            assert!(feed(
                &mut app,
                &mut doc,
                &char_event(ch),
                Modifiers::default()
            ));
        }
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        assert_eq!(SheetsDoc::sheet_names(&mut doc)[0], "Ledger");
    }

    #[test]
    fn tab_rename_escape_cancels_without_a_document_write() {
        let (mut app, mut doc) = seeded_app();
        let before = SheetsDoc::sheet_names(&mut doc);
        app.begin_tab_rename(0);
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("Discarded"),
            Modifiers::default()
        ));
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Escape),
            Modifiers::default()
        ));
        assert!(app.tab_rename.is_none());
        assert_eq!(SheetsDoc::sheet_names(&mut doc), before);
        assert!(app.actions.take_committed().is_none());
    }

    #[test]
    fn tab_duplicate_creates_a_copy() {
        let (mut app, mut doc) = seeded_app();
        let before = SheetsDoc::sheet_count(&mut doc);
        app.run_tab_action(&mut doc, 0, TabAction::Duplicate);
        assert_eq!(SheetsDoc::sheet_count(&mut doc), before + 1);
        // Inserted right after the source and named "<name> copy".
        assert_eq!(SheetsDoc::sheet_names(&mut doc)[1], "Budget copy");
        // The copy carries the source's formulas.
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (4, 1)), "=SUM(B2:D2)");
    }

    #[test]
    fn tab_delete_removes_and_keeps_one_sheet() {
        let (mut app, mut doc) = seeded_app();
        // Seed: Budget, Sales. Remove Sales.
        app.run_tab_action(&mut doc, 1, TabAction::Delete);
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Budget"]);
        // The last remaining sheet cannot be deleted away.
        app.run_tab_action(&mut doc, 0, TabAction::Delete);
        assert_eq!(SheetsDoc::sheet_count(&mut doc), 1);
    }

    #[test]
    fn tab_move_right_reorders_sheets() {
        let (mut app, mut doc) = seeded_app();
        app.run_tab_action(&mut doc, 0, TabAction::MoveRight);
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Sales", "Budget"]);
        // The moved sheet keeps its data (Budget's E2 formula).
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (4, 1)), "=SUM(B2:D2)");
    }

    #[test]
    fn tab_set_and_clear_colour() {
        let (mut app, mut doc) = seeded_app();
        app.run_tab_action(&mut doc, 0, TabAction::SetColor(Some(2)));
        assert_eq!(SheetsDoc::sheet_color(&mut doc, 0), Some(2));
        app.run_tab_action(&mut doc, 0, TabAction::SetColor(None));
        assert_eq!(SheetsDoc::sheet_color(&mut doc, 0), None);
    }

    #[test]
    fn blank_launch_opens_a_single_empty_sheet() {
        let _guard = ENV_LOCK.lock().unwrap();
        // A real launch (persist on, but pointed at an empty temp data dir)
        // must NOT fabricate the sample workbook.
        let tmp = std::env::temp_dir().join(format!("qos-sheets-blank-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("XDG_DATA_HOME", &tmp) };

        let mut app = SheetsApp::new(); // RunMode::Live: blank, persisted
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);

        assert_eq!(SheetsDoc::sheet_count(&mut doc), 1);
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Sheet1"]);
        // No fabricated cells.
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
        assert!(
            snap.raw.is_empty(),
            "blank launch seeded {} cells",
            snap.raw.len()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Not a correctness test: a perf guard demonstrating that a no-op reload
    /// (no document change, as on every scroll/selection/hover frame) skips the
    /// snapshot + full recalculation. Run with `--ignored --nocapture`.
    #[test]
    #[ignore = "a benchmark that prints timings; run with --ignored --nocapture"]
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
            memo.as_secs_f64() * 1e6 / f64::from(n),
            forced,
            forced.as_secs_f64() * 1e6 / f64::from(n),
        );
        assert!(
            memo < forced,
            "memoised reload must be cheaper than a forced one"
        );
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
                GridCmd::SetCell {
                    addr: (0, 0),
                    raw: "hello".into(),
                },
                GridCmd::Commit,
            ],
        );
        app.persist_if_dirty(&mut doc);
        assert!(
            workbook_path().unwrap().exists(),
            "workbook was not written"
        );

        // Session two: a fresh app + fresh document reopens the saved workbook.
        let mut app2 = SheetsApp::new();
        let mut doc2 = AutoCommit::new();
        app2.sync(&mut doc2, &root);
        assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (0, 0)), "hello");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- name box (Go-To) + fx formula entry -------------------------------

    /// Parsing helper the Name box feeds: single refs and ranges, case- and
    /// absolute-marker-insensitive; junk is rejected.
    #[test]
    fn parse_ref_target_handles_cells_ranges_and_junk() {
        assert_eq!(parse_ref_target("b7"), Some(((1, 6), (1, 6))));
        assert_eq!(parse_ref_target("$A$1"), Some(((0, 0), (0, 0))));
        assert_eq!(parse_ref_target("A1:C3"), Some(((0, 0), (2, 2))));
        assert_eq!(parse_ref_target("not-a-ref"), None);
        assert_eq!(parse_ref_target("A1:"), None);
        assert_eq!(parse_ref_target(""), None);
    }

    /// Clicking the Name box, typing a reference, and pressing Enter moves the
    /// selection there through the same shared-TextInput event path used live.
    #[test]
    fn name_box_goto_navigates_to_a_cell() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0));
        app.begin_name_box_edit(); // seeds "A1", fully selected
        assert!(app.name_box_edit.is_some());
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event(" C 5 "),
            Modifiers::default()
        ));
        assert_eq!(
            app.name_box_edit.as_ref().map(TextInput::value),
            Some("C5"),
            "the reference field drops whitespace before domain parsing"
        );
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        assert!(app.name_box_edit.is_none());
        assert_eq!(app.grid.active(), (2, 4));
        assert!(app.status.contains("Went to"), "status={}", app.status);
    }

    /// A `ref:ref` Name box entry selects the whole range.
    #[test]
    fn name_box_goto_selects_a_range() {
        let (mut app, mut doc) = seeded_app();
        app.begin_name_box_edit();
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("B2:D4"),
            Modifiers::default()
        ));
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        let (tl, br) = app.grid.selection_bounds();
        assert_eq!(tl, (1, 1));
        assert_eq!(br, (3, 3));
    }

    /// An invalid Name box entry reports the error and leaves the selection put.
    #[test]
    fn name_box_invalid_reference_keeps_selection() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((4, 1)); // E2
        app.begin_name_box_edit();
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("nope!"),
            Modifiers::default()
        ));
        assert!(feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default()
        ));
        assert_eq!(app.grid.active(), (4, 1), "selection must not move");
        assert!(app.status.contains("Invalid"), "status={}", app.status);
    }

    /// Committing an emptied Name box is a no-op that just closes the editor.
    #[test]
    fn name_box_empty_commit_is_a_noop() {
        let (mut app, _doc) = seeded_app();
        app.grid.select_only((4, 1));
        app.begin_name_box_edit();
        if let Some(input) = &mut app.name_box_edit {
            input.set_text("");
        }
        app.commit_name_box();
        assert!(app.name_box_edit.is_none());
        assert_eq!(app.grid.active(), (4, 1));
    }

    /// The Name box and "fx" affordance get real, ordered, non-overlapping
    /// hit-rects to the left of the formula field, so `handle_event`'s
    /// `contains(p)` routing lands on the right target.
    #[test]
    fn name_box_and_fx_rects_are_hit_testable() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1000.0, 700.0);
        app.relayout();
        assert!(app.name_box_rect.width() > 0.0 && app.name_box_rect.height() > 0.0);
        assert!(app.fx_rect.width() > 0.0 && app.fx_rect.height() > 0.0);
        // Ordered left→right, non-overlapping: name box, then fx, then field.
        assert!(
            app.name_box_rect.x1 <= app.fx_rect.x0 + 0.01,
            "name box overlaps fx"
        );
        assert!(
            app.fx_rect.x1 <= app.formula_field_rect.x0 + 0.01,
            "fx overlaps field"
        );
        // Both sit inside the formula bar row.
        assert!(app.formula_rect.contains(app.name_box_rect.center()));
        assert!(app.formula_rect.contains(app.fx_rect.center()));
    }

    /// The "fx" affordance opens a formula edit on an empty cell, seeded with
    /// `=` and the caret at the end.
    #[test]
    fn fx_starts_a_formula_on_an_empty_cell() {
        let (mut app, _doc) = seeded_app();
        app.grid.select_only((0, 20)); // A21, empty
        assert!(!app.grid.is_editing());
        app.start_formula_entry();
        assert!(app.grid.is_editing());
        assert_eq!(app.grid.active_display_raw(), "=");
        assert_eq!(app.grid.edit_caret(), Some(1));
    }

    /// On a cell that already holds a value, "fx" prepends `=` so the value
    /// becomes the start of a formula, and it evaluates once committed.
    #[test]
    fn fx_prepends_equals_to_existing_content() {
        let (mut app, mut doc) = seeded_app();
        app.apply_cmds(
            &mut doc,
            vec![
                GridCmd::SetCell {
                    addr: (0, 21),
                    raw: "5".into(),
                },
                GridCmd::Commit,
            ],
        );
        app.grid.select_only((0, 21)); // A22 = 5
        app.start_formula_entry();
        assert_eq!(app.grid.active_display_raw(), "=5");
        // Commit the edit through the real flush → apply_cmds path.
        let mut out = Vec::new();
        app.grid.flush_edit(&mut out);
        app.apply_cmds(&mut doc, out);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 21)), "=5");
        assert_eq!(value_at(&app, (0, 21)), Some(CellValue::Number(5.0)));
    }

    // --- fill-right / clear-format / switch-sheet --------------------------

    /// Fill-right copies the left column of the selection rightward, shifting
    /// relative formula references per column — the real Ctrl+R path.
    #[test]
    fn fill_right_copies_and_shifts_formulas() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((4, 1), (6, 1)); // E2:G2, E2 = =SUM(B2:D2)
        app.fill_right(&mut doc);
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 1)), "=SUM(C2:E2)");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (6, 1)), "=SUM(D2:F2)");
        match value_at(&app, (5, 1)) {
            Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
            other => panic!("expected numeric fill result, got {other:?}"),
        }
    }

    /// Clear-format resets an applied format back to General — the real Format
    /// menu "Clear format" path.
    #[test]
    fn clear_format_resets_applied_format() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 25)); // A26, empty
        app.apply_format(&mut doc, CellFormat::Percent);
        assert_eq!(app.format_of_active(), CellFormat::Percent);
        app.clear_format(&mut doc);
        assert_eq!(app.format_of_active(), CellFormat::General);
        // General is not persisted in the snapshot, so the explicit format is gone.
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
        assert_eq!(snap.fmt.get(&(0, 25)).copied(), None);
    }

    /// Clicking a sheet tab flushes the pending in-cell edit onto the current
    /// sheet, then switches the active sheet — the real `switch_sheet` path.
    #[test]
    fn switch_sheet_flushes_edit_then_changes_active() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 30)); // A31 on sheet 0
        app.grid.begin_edit_in_place();
        app.grid.insert_text_public("draft");
        app.switch_sheet(&mut doc, 1); // click the "Sales" tab
        assert_eq!(SheetsDoc::active_index(&mut doc), 1);
        assert_eq!(app.active_sheet, 1);
        // The pending edit landed on sheet 0, not sheet 1.
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 30)), "draft");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (0, 30)), "");
    }

    // --- external-clipboard (TSV) paste + auto-sum guards ------------------

    /// Pasting a tab/newline grid from another app parses and smart-types each
    /// field — the real system-clipboard branch of `paste` (`paste_tsv`).
    #[test]
    fn paste_tsv_parses_external_grid() {
        let (mut app, mut doc) = seeded_app();
        let target = (0, 40); // A41, empty region
        assert!(app.paste_tsv(&mut doc, target, "10\t20\n30\t40\n"));
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 40)), "10");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (1, 40)), "20");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 41)), "30");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (1, 41)), "40");
        app.reload(&mut doc);
        assert_eq!(value_at(&app, (1, 41)), Some(CellValue::Number(40.0)));
        // Empty payload writes nothing.
        assert!(!app.paste_tsv(&mut doc, target, ""));
    }

    /// `AutoSum` on row 1 has nothing above it: it reports so and writes no formula.
    #[test]
    fn auto_sum_guard_nothing_above() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 0)); // A1, row 0
        app.auto_sum(&mut doc);
        assert_eq!(app.status, "Nothing above to sum");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "Category"); // unchanged
    }

    /// `AutoSum` directly under a non-numeric cell finds no run and writes nothing.
    #[test]
    fn auto_sum_guard_no_numeric_run() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 1)); // A2, directly under A1 = "Category" (text)
        let before = SheetsDoc::cell_raw(&mut doc, 0, (0, 1));
        app.auto_sum(&mut doc);
        assert_eq!(app.status, "No numeric run above");
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 1)), before);
        assert!(!SheetsDoc::cell_raw(&mut doc, 0, (0, 1)).starts_with("=SUM"));
    }

    // --- app-level import/export flows -------------------------------------

    /// The Export → write-file then Import → new-sheet round trip for each
    /// interchange format, exercised end to end through the real codecs + a real
    /// file on disk — the exact path-cores the portal drain drives, given an
    /// explicit temp path in place of a host file chooser.
    fn export_then_import_roundtrip(
        ext: &str,
        export_to: fn(&mut SheetsApp, &mut AutoCommit, &std::path::Path),
        import_from: fn(&mut SheetsApp, &mut AutoCommit, &std::path::Path),
    ) {
        let tmp = std::env::temp_dir().join(format!("qos-sheets-io-{ext}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join(format!("Budget.{ext}"));

        let (mut app, mut doc) = seeded_app();
        export_to(&mut app, &mut doc, &path);
        assert!(
            app.status.starts_with("Exported"),
            "export status: {}",
            app.status
        );
        // The file physically exists at the chosen path.
        assert!(path.exists(), "no .{ext} written to {path:?}");

        let before = SheetsDoc::sheet_count(&mut doc);
        import_from(&mut app, &mut doc, &path);
        assert_eq!(
            SheetsDoc::sheet_count(&mut doc),
            before + 1,
            "import must add a sheet"
        );
        assert!(
            app.status.starts_with("Imported"),
            "import status: {}",
            app.status
        );
        // The re-imported sheet carries the Budget header text through the file.
        let last = SheetsDoc::sheet_count(&mut doc) - 1;
        assert_eq!(SheetsDoc::cell_raw(&mut doc, last, (0, 0)), "Category");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn csv_export_then_import_roundtrips() {
        export_then_import_roundtrip("csv", SheetsApp::export_csv_to, SheetsApp::import_csv_from);
    }

    #[test]
    fn xlsx_export_then_import_roundtrips() {
        export_then_import_roundtrip(
            "xlsx",
            SheetsApp::export_xlsx_to,
            SheetsApp::import_xlsx_from,
        );
    }

    #[test]
    fn ods_export_then_import_roundtrips() {
        export_then_import_roundtrip("ods", SheetsApp::export_ods_to, SheetsApp::import_ods_from);
    }

    /// The portal drain path: a Save outcome writes the active sheet, and an
    /// Open outcome imports it back — driving the exact per-frame `drain_io`
    /// the host file chooser feeds, via an injected outcome (no D-Bus dialog).
    #[test]
    fn portal_drain_exports_then_imports_through_the_picked_path() {
        let tmp = std::env::temp_dir().join(format!("qos-sheets-portal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("Exported.csv");

        let (mut app, mut doc) = seeded_app();

        // Inject a Save outcome as though the user picked `path`, then drain.
        app.io = Some((
            PendingIo::Export(Interchange::Csv),
            ui::portal::inject_for_test(PortalOutcome::Opened(path.clone())),
        ));
        app.drain_io(&mut doc);
        assert!(
            app.io.is_none(),
            "a delivered outcome clears the in-flight dialog"
        );
        assert!(path.exists(), "the export wrote the picked file");
        assert!(
            app.status.starts_with("Exported"),
            "export status: {}",
            app.status
        );

        // Inject an Open outcome for the same path and drain: it imports a sheet.
        let before = SheetsDoc::sheet_count(&mut doc);
        app.io = Some((
            PendingIo::Import(Interchange::Csv),
            ui::portal::inject_for_test(PortalOutcome::Opened(path)),
        ));
        app.drain_io(&mut doc);
        assert_eq!(
            SheetsDoc::sheet_count(&mut doc),
            before + 1,
            "import added a sheet"
        );
        assert!(
            app.status.starts_with("Imported"),
            "import status: {}",
            app.status
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A cancelled chooser clears the in-flight dialog and touches nothing.
    #[test]
    fn portal_cancel_is_silent() {
        let (mut app, mut doc) = seeded_app();
        let before = SheetsDoc::sheet_count(&mut doc);
        app.io = Some((
            PendingIo::Import(Interchange::Xlsx),
            ui::portal::inject_for_test(PortalOutcome::Cancelled),
        ));
        app.drain_io(&mut doc);
        assert!(app.io.is_none());
        assert_eq!(
            SheetsDoc::sheet_count(&mut doc),
            before,
            "cancel imports nothing"
        );
    }

    // -----------------------------------------------------------------------
    // Command palette (Ctrl+K) integration
    // -----------------------------------------------------------------------

    /// A `Modifiers` value with Control held.
    fn ctrl_mods() -> Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        Modifiers::from(s)
    }

    /// Synthesise a pressed `WindowEvent::KeyboardInput` (winit keeps
    /// `KeyEventExtra` private, so we zero-init like the toolkit's own tests).
    fn key_event(logical: winit::keyboard::Key) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn char_event(s: &str) -> WindowEvent {
        let mut event = key_event(winit::keyboard::Key::Character(
            winit::keyboard::SmolStr::new(s),
        ));
        if let WindowEvent::KeyboardInput { event: key, .. } = &mut event {
            key.text = Some(winit::keyboard::SmolStr::new(s));
        }
        event
    }

    fn named_event(k: winit::keyboard::NamedKey) -> WindowEvent {
        key_event(winit::keyboard::Key::Named(k))
    }

    /// Drive one event through the real `handle_event` path; returns whether the
    /// app consumed it.
    fn feed(app: &mut SheetsApp, doc: &mut AutoCommit, ev: &WindowEvent, mods: Modifiers) -> bool {
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.handle_event(
            ev,
            1.0,
            &mods,
            doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn ctrl_k_opens_the_command_palette() {
        let (mut app, mut doc) = seeded_app();
        assert!(!app.palette.is_open());
        let handled = feed(&mut app, &mut doc, &char_event("k"), ctrl_mods());
        assert!(handled, "Ctrl+K must be consumed");
        assert!(app.palette.is_open(), "Ctrl+K must open the palette");
        // The first action is the argument-taking "Go to cell".
        assert_eq!(app.palette.selected_id(), Some("goto"));
    }

    #[test]
    fn open_palette_captures_arrows_instead_of_moving_the_grid() {
        let (mut app, mut doc) = seeded_app();
        let start = app.grid.active(); // E2 = (4, 1)
        app.open_command_palette();
        assert_eq!(app.palette.selected_id(), Some("goto"));

        // ArrowDown is consumed by the palette and advances its highlight — the
        // grid selection behind it must NOT move.
        let handled = feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::ArrowDown),
            Modifiers::default(),
        );
        assert!(handled, "the palette must consume ArrowDown");
        assert_eq!(
            app.palette.selected_id(),
            Some("sum_selection"),
            "highlight advanced"
        );
        assert_eq!(app.grid.active(), start, "the grid selection must not move");

        // ArrowUp back to the top, still captured.
        let handled = feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::ArrowUp),
            Modifiers::default(),
        );
        assert!(handled);
        assert_eq!(app.palette.selected_id(), Some("goto"));
        assert_eq!(app.grid.active(), start);
    }

    #[test]
    fn palette_go_to_cell_jumps_the_selection() {
        let (mut app, mut doc) = seeded_app();
        assert_ne!(app.grid.active(), (1, 11));
        app.open_command_palette();
        // Enter opens the argument step, pre-seeded (and fully selected) with
        // the active cell reference.
        feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default(),
        );
        assert!(app.palette.in_argument_mode());
        // Type a new reference — replaces the selected prefill.
        for ch in ["B", "1", "2"] {
            assert!(feed(
                &mut app,
                &mut doc,
                &char_event(ch),
                Modifiers::default()
            ));
        }
        assert_eq!(app.palette.argument_buffer(), Some("B12"));
        // Confirm → jumps the selection and closes the palette.
        feed(
            &mut app,
            &mut doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default(),
        );
        assert!(
            !app.palette.is_open(),
            "running a command closes the palette"
        );
        assert_eq!(app.grid.active(), (1, 11), "selection jumped to B12");
    }

    #[test]
    fn command_go_to_cell_reports_invalid_reference() {
        let (mut app, _doc) = seeded_app();
        app.go_to_cell("not-a-ref");
        assert!(app.status.contains("not a valid"), "status: {}", app.status);
    }

    #[test]
    fn command_sum_selection_writes_totals_below() {
        let (mut app, mut doc) = seeded_app();
        // Budget!B2:B6 — a column of numbers.
        app.grid.select_range_public((1, 1), (1, 5));
        app.dispatch_command(&mut doc, "sum_selection", None);
        // A SUM total lands in B7 (the row directly below the selection).
        let raw = SheetsDoc::cell_raw(&mut doc, 0, (1, 6));
        assert_eq!(raw, "=SUM(B2:B6)", "sum total written below the selection");
        // It computes to the real column sum.
        assert!(matches!(value_at(&app, (1, 6)), Some(CellValue::Number(_))));
    }

    #[test]
    fn command_insert_and_delete_row_shift_cells() {
        let (mut app, mut doc) = seeded_app();
        // The Budget header "Category" sits at A1; its January value is at B2.
        let b2 = SheetsDoc::cell_raw(&mut doc, 0, (1, 1));
        assert!(!b2.is_empty());
        // Insert a row above row 1 (active is E2 → row index 1).
        app.grid.select_only((0, 0)); // A1 → insert above row 0
        app.dispatch_command(&mut doc, "insert_row", None);
        // Everything shifted down one: the old A1 header is now at A2.
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (0, 0)),
            "",
            "row 0 is now blank"
        );
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (0, 1)),
            "Category",
            "header shifted to A2"
        );

        // Delete that blank top row again → header returns to A1.
        app.grid.select_only((0, 0));
        app.dispatch_command(&mut doc, "delete_row", None);
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (0, 0)),
            "Category",
            "header back at A1"
        );
    }

    #[test]
    fn command_insert_column_shifts_right() {
        let (mut app, mut doc) = seeded_app();
        let a1 = SheetsDoc::cell_raw(&mut doc, 0, (0, 0));
        assert_eq!(a1, "Category");
        app.grid.select_only((0, 0)); // insert left of column A
        app.dispatch_command(&mut doc, "insert_col", None);
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (0, 0)),
            "",
            "column A is now blank"
        );
        assert_eq!(
            SheetsDoc::cell_raw(&mut doc, 0, (1, 0)),
            "Category",
            "header shifted to B1"
        );
    }

    /// Covenant — one source of truth: an on-screen affordance dispatches the
    /// byte-identical [`EncodedAction`] its Ctrl+K [`ActionSpec`] builds. Driving
    /// the real sheet-tab "Delete" context-menu handler sets `last_dispatched`
    /// to exactly what the workbook resource tree's delete spec encodes.
    #[test]
    fn tab_delete_affordance_matches_the_action_spec() {
        let (mut app, mut doc) = seeded_app();
        let count_before = SheetsDoc::sheet_count(&mut doc);
        assert!(count_before >= 2, "seeded workbook has multiple sheets");

        // Capture the Ctrl+K delete spec for sheet 1 from the live resource tree,
        // *before* the affordance mutates the workbook (the tree shrinks after).
        let from_spec = {
            let resources = ui::widget::Widget::resources(&app, &doc);
            let sheet1 = &resources[0].children[1];
            let spec = sheet1.actions.iter().find(|a| a.id == "delete").unwrap();
            match &spec.input {
                qos_actions::ActionInput::Immediate(enc) => enc.clone(),
                qos_actions::ActionInput::Prompted { .. } => {
                    panic!("delete is an immediate action")
                }
            }
        };

        // The on-screen tab context-menu "Delete" on sheet 1 (the real handler).
        app.run_tab_action(&mut doc, 1, TabAction::Delete);

        let dispatched = app
            .last_dispatched()
            .expect("the affordance dispatched an action");
        assert_eq!(
            dispatched, &from_spec,
            "the tab-menu Delete builds the same EncodedAction as its Ctrl+K ActionSpec"
        );
        assert_eq!(
            SheetsDoc::sheet_count(&mut doc),
            count_before - 1,
            "the affordance really removed the sheet"
        );
    }

    /// Covenant on the real app path: a cell edit routed through the grid's
    /// `SetCell` command dispatches a `SheetsAction::SetCell` carrying the exact
    /// coordinates and value — the single CRDT-mutation path that undo, peer
    /// replay, and Ctrl+K all share. The `last_dispatched` envelope decodes back
    /// to the action the affordance built.
    #[test]
    fn cell_edit_affordance_dispatches_a_matching_set_cell_action() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 9)); // A10, an empty cell
        app.apply_cmds(
            &mut doc,
            vec![
                GridCmd::SetCell {
                    addr: (0, 9),
                    raw: "hello".into(),
                },
                GridCmd::Commit,
            ],
        );
        let dispatched = app
            .last_dispatched()
            .expect("the cell edit dispatched an action");
        assert_eq!(dispatched.type_id, crate::actions::SHEETS_ACTION_TYPE);
        let action: crate::actions::SheetsAction =
            serde_json::from_slice(&dispatched.payload).expect("payload decodes to SheetsAction");
        match action {
            crate::actions::SheetsAction::SetCells { sheet, cells } => {
                assert_eq!(sheet, 0);
                assert_eq!(cells.len(), 1, "one cell edited");
                assert_eq!(
                    (cells[0].col, cells[0].row),
                    (0, 9),
                    "wrote the selected cell A10"
                );
                assert_eq!(cells[0].raw, "hello");
            }
            other => panic!("cell edit built the wrong action: {other:?}"),
        }
        assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 9)), "hello");
    }

    #[test]
    fn command_toggle_formula_view() {
        let (mut app, mut doc) = seeded_app();
        assert!(!app.grid.formula_view());
        app.dispatch_command(&mut doc, "formula_view", None);
        assert!(app.grid.formula_view(), "formula view turned on");
        app.dispatch_command(&mut doc, "formula_view", None);
        assert!(!app.grid.formula_view(), "formula view toggled back off");
    }

    #[test]
    fn command_format_percent_applies_to_selection() {
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((1, 1)); // B2
        app.dispatch_command(&mut doc, "format_percent", None);
        // The stored format for B2 is now Percent.
        let cur = app.default_currency();
        let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, cur);
        assert_eq!(snap.fmt.get(&(1, 1)), Some(&CellFormat::Percent));
    }

    #[test]
    fn command_new_sheet_adds_a_sheet() {
        let (mut app, mut doc) = seeded_app();
        let before = SheetsDoc::sheet_count(&mut doc);
        app.dispatch_command(&mut doc, "new_sheet", None);
        assert_eq!(SheetsDoc::sheet_count(&mut doc), before + 1);
    }
}
