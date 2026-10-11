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
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::{
    Button, ColorSwatchPicker, ContextMenu, EditOutcome, EditableLabel, Popover, PopoverSide,
    TextInput, Widget,
};

mod annotations;
mod chrome;
mod overlays;

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
use tilo_locale::{Currency, DateStyle, Locale, TimeStyle};

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
const DATE_STYLES: [(&str, DateStyle); 4] = [
    ("ISO", DateStyle::Iso),
    ("US", DateStyle::Us),
    ("EU", DateStyle::Eu),
    ("Long", DateStyle::Long),
];
const TIME_STYLES: [(&str, TimeStyle); 2] =
    [("24-hour", TimeStyle::H24), ("12-hour", TimeStyle::H12)];
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

/// The app's only decorative animation is the transient status bubble. Keep
/// the preference as a semantic state rather than another independent bool so
/// it cannot accidentally conflict with the run mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MotionPreference {
    Full,
    Reduced,
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

#[derive(Clone)]
enum AnnotationClick {
    Open(Addr),
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent retained interaction, worker and presentation flags; they are not mutually exclusive states"
)]
pub struct SheetsApp {
    grid: Grid,
    /// The custom chrome has no toolkit layout primitives to inherit type
    /// sizing from, so keep its touch targets and modal spacing aligned with
    /// the current accessibility text preference.
    text_scale: f64,
    rect: Rect,
    toolbar_rect: Rect,
    formula_rect: Rect,
    grid_rect: Rect,
    tabs_rect: Rect,
    /// The command bar, and what its commands and menus last ran.
    toolbar: ui::widget::Toolbar,
    command_slot: CommandSlot,
    /// The Format menu's rows as the bar holds them.
    format_rows_shown: Option<Vec<MenuRow>>,
    /// The theme the chrome was last measured in, for laying the bar out
    /// between frames.
    chrome_theme: Theme,
    /// The window's History control in the header row's corner.
    history: ui::widget::HistoryOpener,
    history_rect: Rect,
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
    /// The latest transient action message (e.g. "Pasted into B2"), surfaced
    /// via the floating [`status_bubble`](Self::status_bubble) rather than a
    /// permanent status line. Initialised to `"Ready"`, which the bubble
    /// treats as "nothing to say".
    status: String,
    /// Transient floating status bubble carrying the selection summary or the
    /// active cell's format, plus any fresh action message. It surfaces when
    /// that composed text changes (a selection move/extend, an action) or
    /// while the pointer rests in its corner, then fades — keeping the
    /// sheet-tab strip free of status text so it reads purely as the sheet
    /// switcher.
    status_bubble: ui::StatusBubble,
    /// A staged capture pins the bubble open so a still frame shows it.
    staged_status: bool,
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
    motion: MotionPreference,
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

impl SheetsApp {
    pub fn new() -> Self {
        let command_slot = CommandSlot::default();
        Self {
            toolbar: Self::build_command_bar(&command_slot),
            command_slot,
            format_rows_shown: None,
            chrome_theme: Theme::dark(),
            history: ui::widget::HistoryOpener::new(),
            history_rect: Rect::ZERO,
            grid: Grid::new(),
            text_scale: 1.0,
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            formula_rect: Rect::ZERO,
            grid_rect: Rect::ZERO,
            tabs_rect: Rect::ZERO,
            settings_sheet: None,
            metrics: ChromeMetrics::fallback(),
            tab_rects: Vec::new(),
            add_tab_rect: Rect::ZERO,
            sheet_colors: Vec::new(),
            tab_menu_target: None,
            tab_menu: ContextMenu::new(),
            tab_menu_rows: Vec::new(),
            tab_colour: Popover::with_content(ColorSwatchPicker::new(Vec::new(), 1))
                .side(PopoverSide::Above),
            tab_colour_shown: None,
            active_cell_bold: false,
            staged_overlay: None,
            tab_rename: None,
            name_box_rect: Rect::ZERO,
            fx_rect: Rect::ZERO,
            name_box_edit: None,
            formula_field_rect: Rect::ZERO,
            formula_char_x: Vec::new(),
            formula_bar_selecting: false,
            sheet_names: Vec::new(),
            sheet_charts: Vec::new(),
            sheet_comments: Vec::new(),
            annotation_clicks: Vec::new(),
            dock_rect: Rect::ZERO,
            dock_cards: Vec::new(),
            dock_scroll: ui::widget::ScrollRegion::horizontal(),
            sheet_ids: Vec::new(),
            workbook: Rc::new(Workbook::default()),
            revision: 0,
            live: None,
            active_sheet: 0,
            status: "Ready".into(),
            status_bubble: ui::StatusBubble::new(),
            staged_status: false,
            staged: None,
            pending: Pending { redraw: true },
            last_press: None,
            last_mouse: Point::ZERO,
            current_active_format: CellFormat::General,
            settings_panel: SettingsPanel::None,
            global: Locale::default(),
            locale: None,
            doc_settings: DocSettings::default(),
            motion: MotionPreference::Full,
            cache: None,
            pending_reveal: None,
            clipboard: None,
            palette: {
                let mut palette = CommandPalette::commands();
                palette.set_query_placeholder("Search commands\u{2026}");
                palette
            },
            dispatch: None,
            opening: Rc::default(),
            selection_decls: std::cell::RefCell::default(),
        }
    }

    /// Take part in the framework: submit workbook edits through its dispatcher.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<Workbook>,
        live: ui::framework::Live<Workbook>,
    ) {
        self.dispatch = Some(dispatcher);
        self.adopt_resource_state(&live.get());
        self.live = Some(live);
    }

    /// What shows a search hit or an agent's `open`: a sheet, or a cell of
    /// one, revealed at the next settle.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&str) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let opening = Rc::clone(&self.opening);
        move |anchor: &str| {
            let (sheet, addr) = match crate::resource::parse_cell_anchor(anchor) {
                Some(cell) => cell,
                None => ui::tilo_resource::items::item_of_resource::<Workbook>(anchor)
                    .map(|(_, sheet)| (sheet, (0, 0)))
                    .ok_or_else(|| {
                        ui::tilo_resource::Refusal::new(format!(
                            "`{anchor}` is not in the workbook"
                        ))
                    })?,
            };
            *opening.borrow_mut() = Some(PendingReveal {
                sheet_id: Some(sheet),
                addr,
                status: None,
                arriving: false,
            });
            Ok(())
        }
    }

    /// How a comment's `author` is named to the person reading it: "You"
    /// for their own.
    fn author_name(&self, author: &str) -> String {
        let me = self.dispatch.as_ref().and_then(|dispatch| dispatch.me());
        if author.is_empty() {
            "Someone".to_owned()
        } else if me == Some(author) {
            "You".to_owned()
        } else {
            author.to_owned()
        }
    }

    /// Follow the person's shared locale defaults.
    pub(crate) fn follow_locale(&mut self, locale: ui::framework::Setting<Locale>) {
        self.locale = Some(locale);
    }

    /// Show the workbook as the resource holds it.
    pub(crate) fn adopt_resource_state(&mut self, state: &Rc<Workbook>) {
        if Rc::ptr_eq(state, &self.workbook) {
            return;
        }
        self.workbook = Rc::clone(state);
        self.revision = self.revision.wrapping_add(1);
        self.sheet_names = state
            .sheets
            .values()
            .map(|sheet| sheet.name.0.clone())
            .collect();
        self.sheet_colors = state.sheets.values().map(|sheet| sheet.color).collect();
        self.sheet_ids = state.sheets.ids().cloned().collect();
        self.doc_settings = state.settings;
        self.pending.redraw = true;
    }

    /// Read the workbook again after an own edit applied to `doc`.
    fn follow(&mut self, doc: &Edits<'_>) {
        let Some(live) = &self.live else {
            return;
        };
        if live.sync(doc.doc()).is_ok() {
            let state = live.get();
            self.adopt_resource_state(&state);
        }
    }

    /// The active sheet's id.
    fn active_sheet_id(&self) -> Option<Id> {
        self.sheet_ids.get(self.active_sheet).cloned()
    }

    /// The cells of the active sheet as stored: blank where nothing is.
    fn stored_cells(&self) -> impl Fn(Addr) -> Cell + '_ {
        let sheet = self
            .active_sheet_id()
            .and_then(|id| self.workbook.sheet(&id));
        let layout = sheet.map(crate::workbook::Sheet::layout);
        move |addr| {
            sheet
                .zip(layout.as_ref())
                .and_then(|(sheet, layout)| sheet.cell_at(layout, addr).cloned())
                .unwrap_or_default()
        }
    }

    // --- action dispatch ----------------------------------------------------

    /// Submit `invoke` now through the workbook's dispatcher — the one door
    /// every edit takes, the palette's and an agent's too — and read the
    /// workbook again. A refusal is the status.
    fn submit(&mut self, doc: &mut Edits<'_>, invoke: Invoke<Workbook>) -> bool {
        let Some(dispatch) = &self.dispatch else {
            self.status = "The workbook is not open for editing".into();
            return false;
        };
        let applied = match dispatch.submit_now(doc, invoke) {
            Ok(()) => true,
            Err(error) => {
                self.status = error.to_string();
                false
            }
        };
        self.follow(doc);
        applied
    }

    /// Write `writes` (input, and format when given) into the active sheet
    /// as field edits of the cells, one undo step; a cell keeps what a
    /// write leaves out.
    fn write_raw(&mut self, doc: &mut Edits<'_>, writes: Vec<(Addr, String, Option<CellFormat>)>) {
        let stored = self.stored_cells();
        let cells: Vec<(Addr, Cell)> = writes
            .into_iter()
            .map(|(addr, raw, format)| {
                let mut cell = stored(addr);
                cell.raw = Atom(raw);
                if let Some(format) = format {
                    cell.format = format;
                }
                (addr, cell)
            })
            .collect();
        drop(stored);
        self.write_cells(doc, &cells);
    }

    /// Set the format of `cells` of the active sheet, one undo step.
    fn write_formats(&mut self, doc: &mut Edits<'_>, cells: Vec<(Addr, CellFormat)>) {
        let stored = self.stored_cells();
        let cells: Vec<(Addr, Cell)> = cells
            .into_iter()
            .map(|(addr, format)| {
                let mut cell = stored(addr);
                cell.format = format;
                (addr, cell)
            })
            .collect();
        drop(stored);
        self.write_cells(doc, &cells);
    }

    /// Store `cells` in the active sheet as they are, one undo step.
    fn write_cells(&mut self, doc: &mut Edits<'_>, cells: &[(Addr, Cell)]) {
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        let edits = crate::workbook::cell_edits(&self.workbook, &sheet, cells);
        if edits.is_empty() {
            return;
        }
        let members: Vec<Invoke<Workbook>> = edits
            .iter()
            .filter_map(|edit| Invoke::edit(edit).ok())
            .collect();
        // One write is one action: one change, one history entry.
        self.submit(doc, Invoke::group(members));
    }

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
    fn build_selection_declarations(&self) -> Vec<ui::tilo_resource::ActionDecl<Workbook>> {
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

    /// Open the per-document settings sheet.
    fn open_doc_settings(&mut self) {
        self.close_overlays();
        self.settings_panel = SettingsPanel::Document;
    }

    /// Pin the floating status bubble open, so a still frame shows the Sum /
    /// Avg / Count pill or the message it carries rather than a bubble that
    /// has faded.
    fn pin_status_bubble(&mut self) {
        self.staged_status = true;
    }

    /// Stage a screenshot scenario's view state over the sample workbook its
    /// actions wrote: the active cell, an edit in progress, a message. A menu
    /// or sheet opens in the first measured frame (it anchors to what that
    /// frame lays out); what needs the document (a fill, a paste, the
    /// palette's actions) is done by the `sync` that follows.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        // A smart-typed currency cell, so the formula bar and the type
        // indicator are populated.
        if let Some(addr) = a1_to_addr("E2") {
            self.grid.select_only(addr);
        }
        match scenario {
            Some(
                name @ ("file" | "format" | "more" | "docsettings" | "tabmenu" | "tabcolour"
                | "tabrename"),
            ) => self.staged_overlay = Some(name.to_owned()),
            Some("editing") => self.grid.begin_edit_in_place(),
            // A formula built purely by clicking cells (point mode): the
            // coloured references it inserted and the matching outlines on
            // the referenced cells (B2 and C2).
            Some("pointmode") => {
                self.grid.select_only((2, 9)); // C10, an empty cell
                self.grid.begin_edit(Some("=".into()));
                self.grid.point_click_addr((1, 1)); // click B2 → =B2
                self.grid.insert_text_public("+"); // type + → =B2+
                self.grid.point_click_addr((2, 1)); // click C2 → =B2+C2
            }
            // Budget!B2:B6 — the January column body — and its summary.
            Some("summary") => {
                self.grid.select_range_public((1, 1), (1, 5));
                self.pin_status_bubble();
            }
            // Two separate highlighted rectangles with lit headers: the
            // January column body and the Q1 Total column.
            Some("multiselect") => {
                self.grid.select_range_public((1, 1), (1, 5)); // B2:B6
                self.grid.add_range_public((4, 1), (4, 6)); // E2:E7
                self.pin_status_bubble();
            }
            // The range reference inside E2's `=SUM(B2:D2)` selected in the
            // editable formula bar.
            Some("fbarselect") => {
                self.grid.select_only((4, 1));
                self.grid.begin_edit_in_place();
                self.grid.set_edit_caret(5, false); // just before "B2:D2"
                self.grid.set_edit_caret(10, true); // select "B2:D2"
            }
            Some("commandpalette") => self.staged = Some(Staged::CommandPalette),
            // Both scrollbar thumbs offset, proving the two-dimensional
            // viewport rather than idle rails.
            Some("scrollbars") => self.grid.on_scroll(540.0, 260.0),
            Some("fillseries") => self.staged = Some(Staged::FillSeries),
            Some("pasteformula") => self.staged = Some(Staged::PasteFormula),
            Some("error") => {
                self.status = "Excel import failed: the workbook is password protected".into();
                self.pin_status_bubble();
            }
            // An incoming revision superseding the value being edited.
            Some("conflict") => {
                self.status =
                    "Budget was updated by a collaborator; showing the latest value".into();
                self.pin_status_bubble();
            }
            // The acknowledgement stays visible with animations suppressed.
            Some("reducedmotion") => {
                self.set_motion_reduced(true);
                self.status = "Changes saved".into();
                self.pin_status_bubble();
            }
            Some("charts" | "comments") => {
                self.status = "2 charts and 1 resolved comment thread added".into();
                self.pin_status_bubble();
            }
            // The bold header row, so the Bold command shows it is on.
            Some("bold") => self.grid.select_range_public((0, 0), (6, 0)),
            _ => {}
        }
    }

    /// Open the staged menu or sheet, if any, now that the chrome is measured
    /// and laid out where it will be painted.
    fn apply_staged_overlay(&mut self) {
        let Some(name) = self.staged_overlay.take() else {
            return;
        };
        match name.as_str() {
            "file" => self.toolbar.open_command_menu(chrome::bar::FILE),
            "format" => self.toolbar.open_command_menu(chrome::bar::FORMAT),
            "more" => {
                if self.toolbar.has_overflow() {
                    self.toolbar.open_overflow();
                } else {
                    // Nothing to open, and a capture of the plain window under
                    // this name would pass for the menu.
                    #[allow(clippy::print_stderr, reason = "a capture-time diagnostic")]
                    {
                        eprintln!(
                            "sheets: scenario `more` needs a window too narrow for the command \
                             bar, for example --viewport 390x844 or --text-scale 2"
                        );
                    }
                }
            }
            "docsettings" => self.open_doc_settings(),
            "tabmenu" => self.open_tab_menu(0),
            "tabrename" => self.begin_tab_rename(0),
            // The popover opens over tabs that already carry a colour.
            "tabcolour" => {
                self.open_tab_menu(0);
                self.tab_menu.close();
                self.tab_colour.set_open(true);
            }
            _ => {}
        }
    }

    /// Finish the staged part of a scenario that works on the document.
    fn finish_staging(&mut self, doc: &mut Edits<'_>) {
        let Some(staged) = self.staged.take() else {
            return;
        };
        match staged {
            Staged::CommandPalette => self.open_command_palette(),
            // The seeded patterns extended through the real fill-handle path:
            // a number run (1, 2 → …6), weekdays (Mon → …Sat) and a
            // text+number run (Task 1 → …Task 6), with the fill handle on the
            // filled numeric column.
            Staged::FillSeries => {
                self.grid.select_range_public((1, 15), (1, 16));
                self.grid.start_fill_to((1, 20));
                self.run_pending_fill(doc);
                self.grid.select_only((2, 15));
                self.grid.start_fill_to((2, 20));
                self.run_pending_fill(doc);
                self.grid.select_only((3, 15));
                self.grid.start_fill_to((3, 20));
                self.run_pending_fill(doc);
                self.grid.select_range_public((1, 15), (1, 20));
            }
            // E2's `=SUM(B2:D2)` copied one row down: the formula bar shows
            // the adjusted `=SUM(B3:D3)` and the cell its result.
            Staged::PasteFormula => {
                self.grid.select_only((4, 1));
                self.copy_selection(false);
                self.grid.select_only((4, 2)); // E3, the Groceries total
                self.paste(doc);
                self.grid.select_only((4, 2));
            }
        }
        self.reload();
    }

    /// The document's effective locale (per-doc overrides over global).
    fn locale(&self) -> EffectiveLocale {
        EffectiveLocale::resolve(&self.global, &self.doc_settings)
    }

    fn default_currency(&self) -> Currency {
        self.locale().currency
    }

    // --- document plumbing --------------------------------------------------

    /// Read the active sheet, recalculate, propagate formats onto computed
    /// cells, and push the data into the grid.
    ///
    /// Memoised on `(workbook revision, active sheet)`: selection changes,
    /// scrolling, and hover — which never change the workbook — reuse the cached
    /// result instead of re-snapshotting the document and re-running the whole
    /// formula engine every frame.
    fn reload(&mut self) {
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
                sheet.snapshot_in(layout),
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
            revision: self.revision,
            sheet,
            eff_fmt: fmt.clone(),
            values: values.clone(),
        });
        self.grid.set_bold(snap.bold);
        self.grid.set_data(snap.raw, fmt, values);
        self.pending.redraw = true;
    }

    fn apply_cmds(&mut self, doc: &mut Edits<'_>, cmds: Vec<GridCmd>) {
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
            self.write_raw(doc, writes);
            self.reload();
        }
    }

    fn apply_format(&mut self, doc: &mut Edits<'_>, fmt: CellFormat) {
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

    fn clear_format(&mut self, doc: &mut Edits<'_>) {
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
    fn paste(&mut self, doc: &mut Edits<'_>) {
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
    fn paste_tsv(&mut self, doc: &mut Edits<'_>, target: Addr, text: &str) -> bool {
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
    /// and write `=SUM(top:above)` into the active cell.
    fn auto_sum(&mut self, doc: &mut Edits<'_>) {
        let (c, r) = self.grid.active();
        if r == 0 {
            self.status = "Nothing above to sum".into();
            return;
        }
        let mut top = r;
        for rr in (0..r).rev() {
            let raw = self.grid.cell_raw_at((c, rr));
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
        self.write_raw(doc, vec![((c, r), formula.clone(), None)]);
        self.reload();
        self.status = format!("Inserted {formula}");
    }

    /// Fill the top row of the selection downward through every row in the
    /// selection's bounding box — Excel's Ctrl+D. Each source cell's raw input
    /// and format copy down, with relative formula references shifted per row.
    fn fill_down(&mut self, doc: &mut Edits<'_>) {
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
    fn fill_right(&mut self, doc: &mut Edits<'_>) {
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
    fn run_pending_fill(&mut self, doc: &mut Edits<'_>) {
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

    // --- file interchange ---------------------------------------------------

    /// Invoke one of the workbook's effects through its door, as the palette
    /// or an agent does.
    fn invoke_effect(&self, effect: &actions::SheetsEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
    }

    /// Answer one of the workbook's effects: reading or writing a file is a
    /// job, a view changes what is shown at once.
    pub(crate) fn answer_effect(
        &mut self,
        effect: actions::SheetsEffect,
        cx: &mut ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::EffectAnswer {
        use actions::SheetsEffect as E;
        self.pending.redraw = true;
        match effect {
            E::ImportCsv { path } => Ok(self.import(Interchange::Csv, path, cx)),
            E::ImportXlsx { path } => Ok(self.import(Interchange::Xlsx, path, cx)),
            E::ImportOds { path } => Ok(self.import(Interchange::Ods, path, cx)),
            E::ExportCsv { path } => self.export(Interchange::Csv, path, cx),
            E::ExportXlsx { path } => self.export(Interchange::Xlsx, path, cx),
            E::ExportOds { path } => self.export(Interchange::Ods, path, cx),
            E::ReadCells { sheet, range } => {
                let snapshot = self
                    .workbook
                    .sheet(&Id::from_raw(sheet))
                    .ok_or_else(|| "no such sheet".to_owned())?
                    .snapshot();
                crate::cell_actions::read(&snapshot, range.as_deref())
                    .map(ui::framework::Effected::value)
            }
            E::Commands => {
                let resources = self.published();
                self.open_palette_with(&resources);
                Ok(ui::framework::Effected::done())
            }
            view => {
                self.show(view)?;
                Ok(ui::framework::Effected::done())
            }
        }
    }

    /// Change what is shown, as a view effect asks.
    fn show(&mut self, effect: actions::SheetsEffect) -> Result<(), String> {
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

    /// Import a spreadsheet file into new sheets: the one at `path`, or the
    /// one the person chooses. A file the workbook already holds is shown
    /// instead of imported again.
    fn import(
        &mut self,
        format: Interchange,
        path: Option<String>,
        cx: &mut ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::Effected {
        let Some(path) = path else {
            self.status = format!("Choose a .{} file to import…", format.ext());
            return cx.choose_file(
                spreadsheet_import_request(format),
                move |app: &mut Self, chosen, cx| {
                    let Some(file) = chosen else {
                        app.status = "Import cancelled".into();
                        return Ok(ui::framework::Effected::done());
                    };
                    if u64::try_from(file.bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_FILE_BYTES {
                        return app.failed(format!(
                            "{} is larger than the {} MiB import limit",
                            file.name,
                            MAX_IMPORT_FILE_BYTES / (1024 * 1024)
                        ));
                    }
                    // The same file chosen again is shown, not imported twice.
                    let source = file.identity.as_ref().map(ToString::to_string);
                    if let Some(source) = &source
                        && app.show_imported(source, &file.name, cx)
                    {
                        return Ok(ui::framework::Effected::done());
                    }
                    app.status = format!("Importing .{}…", format.ext());
                    let name = file.name;
                    let sheet = sheet_name_from_path(std::path::Path::new(&name));
                    Ok(cx.thread(
                        move |_| Self::read_tables(format, &file.bytes, &sheet),
                        move |app: &mut Self, tables, cx| {
                            app.finish_import(tables, &name, source, cx)
                        },
                    ))
                },
            );
        };
        let path = PathBuf::from(path);
        let source = import_source(&path);
        if self.show_imported(&source, &file_label(&path), cx) {
            return ui::framework::Effected::done();
        }
        self.status = format!("Importing .{}…", format.ext());
        cx.thread(
            move |_| Self::read_tables_at(format, &path).map(|tables| (tables, path)),
            move |app: &mut Self, read, cx| match read {
                Ok((tables, path)) => {
                    app.finish_import(Ok(tables), &file_label(&path), Some(source), cx)
                }
                Err(error) => app.failed(error),
            },
        )
    }

    /// Show the sheet the file `source` was already imported as, if any:
    /// whether there was one.
    fn show_imported(
        &mut self,
        source: &str,
        label: &str,
        _cx: &ui::framework::EffectCx<Workbook, Self>,
    ) -> bool {
        let open = self
            .workbook
            .sheets
            .iter()
            .find(|(_, sheet)| {
                sheet.source.as_ref().map(|source| source.0.as_str()) == Some(source)
            })
            .map(|(id, sheet)| (id.clone(), sheet.name.0.clone()));
        let Some((sheet_id, name)) = open else {
            return false;
        };
        self.pending_reveal = Some(PendingReveal {
            sheet_id: Some(sheet_id),
            addr: (0, 0),
            status: Some(format!("{label} is already open as {name}")),
            arriving: false,
        });
        true
    }

    /// Add the sheets a file was read into as one action, and show the last
    /// of them once it is applied.
    fn finish_import(
        &mut self,
        tables: Result<Vec<crate::xlsx::SheetTable>, String>,
        label: &str,
        source: Option<String>,
        cx: &ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::EffectAnswer {
        let built = tables.and_then(|tables| self.import_action(tables, source));
        let (invoke, total) = match built {
            Ok(built) => built,
            Err(error) => return self.failed(error),
        };
        cx.submit(invoke);
        self.status = format!("Imported {total} cells from {label}");
        self.pending_reveal = Some(PendingReveal {
            sheet_id: None,
            addr: (0, 0),
            status: Some(self.status.clone()),
            arriving: true,
        });
        Ok(ui::framework::Effected::done())
    }

    /// Write the workbook as a spreadsheet file (a CSV holds the active sheet
    /// only): to `path`, or where the person chooses. Encoding runs off the UI
    /// thread; the file is written only when the job was not cancelled.
    fn export(
        &mut self,
        format: Interchange,
        path: Option<String>,
        cx: &mut ui::framework::EffectCx<Workbook, Self>,
    ) -> ui::framework::EffectAnswer {
        let active = self.active_sheet_id();
        let sheets: Vec<crate::workbook::SheetSnapshot> = match format {
            Interchange::Csv => active
                .as_ref()
                .and_then(|id| self.workbook.sheet(id))
                .map(crate::workbook::Sheet::snapshot)
                .into_iter()
                .collect(),
            Interchange::Xlsx | Interchange::Ods => self
                .workbook
                .sheets
                .values()
                .map(crate::workbook::Sheet::snapshot)
                .collect(),
        };
        let Some(name) = active
            .as_ref()
            .and_then(|id| self.workbook.sheet(id))
            .map(|sheet| sheet.name.0.clone())
            .or_else(|| sheets.first().map(|sheet| sheet.name.clone()))
        else {
            return self.failed("there is no sheet to export".into());
        };
        let _ = cx;
        self.status = format!("Exporting .{}…", format.ext());
        let encode = move |_| Self::encode_tables(format, &sheets);
        let Some(path) = path else {
            let suggested = format!("{name}.{}", format.ext());
            return Ok(cx.thread(encode, move |app: &mut Self, bytes, cx| {
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(error) => return app.failed(error),
                };
                app.status = "Choose where to save…".into();
                Ok(cx.save_file(
                    spreadsheet_export_request(format, &suggested),
                    bytes,
                    move |app: &mut Self, saved, _| {
                        app.status = if saved {
                            format!("Exported .{}", format.ext())
                        } else {
                            "Export cancelled".into()
                        };
                        Ok(ui::framework::Effected::done())
                    },
                ))
            }));
        };
        let path = PathBuf::from(path);
        Ok(cx.thread(encode, move |app: &mut Self, bytes, _| {
            let written = bytes.and_then(|bytes| {
                crate::xlsx::write_file(&path, &bytes).map_err(|error| error.to_string())
            });
            match written {
                Ok(()) => {
                    app.status = format!("Exported .{} to {}", format.ext(), file_label(&path));
                    Ok(ui::framework::Effected::done())
                }
                Err(error) => app.failed(error),
            }
        }))
    }

    /// Report a failed import or export, and fail its effect.
    fn failed(&mut self, error: String) -> ui::framework::EffectAnswer {
        self.status = format!("Spreadsheet operation failed: {error}");
        self.pending.redraw = true;
        Err(error)
    }

    /// The one verb that adds a parsed set of tables as new sheets, running
    /// smart inference where a source cell carried no explicit format, with
    /// how many cells it writes. Into a workbook that is still blank, the
    /// file replaces its empty sheet. The verb checks every tab and cell
    /// before anything is written, so collaborators never observe a
    /// half-import; history and undo see one operation.
    fn import_action(
        &self,
        tables: Vec<crate::xlsx::SheetTable>,
        source: Option<String>,
    ) -> Result<(Invoke<Workbook>, usize), String> {
        use crate::resource::{CellWrite, ImportedSheet};
        let cur = self.default_currency();
        let mut total: usize = 0;
        let mut sheets = Vec::new();
        for table in tables {
            let mut writes = Vec::new();
            for (r, row) in (0u32..).zip(&table.rows) {
                for (c, cell) in (0u32..).zip(row) {
                    let Some(cell) = cell else { continue };
                    // A formula cell restores its live formula; a value cell
                    // stores its value. The format still derives from the
                    // imported value, not the formula text.
                    let raw = cell.formula.as_deref().unwrap_or(cell.raw.as_str());
                    let fmt = if cell.format == CellFormat::General {
                        infer_entry(&cell.raw, cur).format
                    } else {
                        cell.format
                    };
                    writes.push((
                        (r, c),
                        CellWrite {
                            cell: crate::model::addr_to_a1(c, r),
                            input: Some(raw.to_string()),
                            format: (fmt != CellFormat::General).then_some(fmt),
                            bold: cell.bold.then_some(true),
                        },
                    ));
                }
            }
            total = total.saturating_add(writes.len());
            if total > MAX_IMPORT_CELLS {
                return Err(format!(
                    "the workbook contains more than {MAX_IMPORT_CELLS} populated cells"
                ));
            }
            writes.sort_by_key(|(at, _)| *at);
            let name = if table.name.trim().is_empty() {
                "Imported".to_string()
            } else {
                table.name
            };
            sheets.push(ImportedSheet {
                name,
                cells: writes.into_iter().map(|(_, write)| write).collect(),
            });
        }
        let invoke = Workbook::IMPORT_SHEETS.with(verbs::ImportSheets { sheets, source });
        Ok((invoke, total))
    }

    /// Refuse a path before handing it to an interchange reader.  Archive
    /// formats can still expand internally, so the parsed-cell bound below is
    /// the authoritative limit; this inexpensive first gate avoids allocating
    /// an arbitrary local file just to discover it was never eligible.
    fn check_import_file_size(path: &std::path::Path) -> Result<(), String> {
        let metadata = tilo_files::file_at(path)
            .and_then(|(dir, name)| tilo_files::blocking::metadata(&dir, &name))
            .map_err(|error| format!("cannot inspect {}: {error}", file_label(path)))?;
        if metadata.kind != tilo_files::Kind::File {
            return Err("the selected path is not a regular file".into());
        }
        if metadata.len > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "{} is larger than the {} MiB import limit",
                file_label(path),
                MAX_IMPORT_FILE_BYTES / (1024 * 1024)
            ));
        }
        Ok(())
    }

    /// The tables of the spreadsheet file at `path`.
    fn read_tables_at(
        format: Interchange,
        path: &std::path::Path,
    ) -> Result<Vec<crate::xlsx::SheetTable>, String> {
        Self::check_import_file_size(path)?;
        let bytes = crate::xlsx::read_file(path).map_err(|error| error.to_string())?;
        Self::read_tables(format, &bytes, &sheet_name_from_path(path))
    }

    /// The tables a spreadsheet file holds, one per sheet; a CSV is one,
    /// named `csv_name`.
    fn read_tables(
        format: Interchange,
        bytes: &[u8],
        csv_name: &str,
    ) -> Result<Vec<crate::xlsx::SheetTable>, String> {
        match format {
            Interchange::Xlsx => crate::xlsx::parse_xlsx(bytes).map_err(|error| error.to_string()),
            Interchange::Ods => crate::ods::parse_ods(bytes).map_err(|error| error.to_string()),
            Interchange::Csv => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| "the CSV file is not UTF-8 text".to_string())?;
                let cells = crate::csv::parse_csv(text);
                if cells.len() > MAX_IMPORT_CELLS {
                    return Err(format!("more than {MAX_IMPORT_CELLS} populated cells"));
                }
                let max_col = cells.keys().map(|addr| addr.0).max().unwrap_or(0);
                let max_row = cells.keys().map(|addr| addr.1).max().unwrap_or(0);
                if max_col > 16_383 || max_row > 1_048_575 {
                    return Err("a cell is outside the supported workbook grid".into());
                }
                let width = usize::try_from(max_col)
                    .map_err(|_| "column index is too large")?
                    .checked_add(1)
                    .ok_or_else(|| "column count overflows this platform".to_string())?;
                let height = usize::try_from(max_row)
                    .map_err(|_| "row index is too large")?
                    .checked_add(1)
                    .ok_or_else(|| "row count overflows this platform".to_string())?;
                let slots = width
                    .checked_mul(height)
                    .ok_or_else(|| "CSV dimensions overflow this platform".to_string())?;
                if slots > MAX_CSV_MATERIALIZED_SLOTS {
                    return Err(format!(
                        "CSV dimensions require {slots} cells, above the {MAX_CSV_MATERIALIZED_SLOTS} import limit"
                    ));
                }
                let mut rows = vec![vec![None; width]; height];
                for ((col, row), raw) in cells {
                    let c = usize::try_from(col).map_err(|_| "column index is too large")?;
                    let r = usize::try_from(row).map_err(|_| "row index is too large")?;
                    let Some(line) = rows.get_mut(r) else {
                        return Err("row is outside import bounds".into());
                    };
                    let Some(cell) = line.get_mut(c) else {
                        return Err("column is outside import bounds".into());
                    };
                    *cell = Some(crate::xlsx::ImportedCell {
                        raw,
                        format: CellFormat::General,
                        formula: None,
                        bold: false,
                    });
                }
                Ok(vec![crate::xlsx::SheetTable {
                    name: csv_name.to_string(),
                    rows,
                }])
            }
        }
    }

    /// `sheets` as the bytes of a spreadsheet file; a CSV holds the first.
    fn encode_tables(
        format: Interchange,
        sheets: &[crate::workbook::SheetSnapshot],
    ) -> Result<Vec<u8>, String> {
        let tables = sheets
            .iter()
            .map(Self::table_from_snapshot)
            .collect::<Vec<_>>();
        match format {
            Interchange::Xlsx => {
                crate::xlsx::xlsx_bytes(&tables).map_err(|error| error.to_string())
            }
            Interchange::Ods => crate::ods::ods_bytes(&tables).map_err(|error| error.to_string()),
            Interchange::Csv => {
                let table = tables
                    .first()
                    .ok_or_else(|| "there is no active sheet to export".to_string())?;
                let rows = table
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|cell| {
                                cell.as_ref()
                                    .map_or_else(String::new, |cell| cell.raw.clone())
                            })
                            .collect()
                    })
                    .collect::<Vec<Vec<String>>>();
                Ok(crate::csv::to_csv(&rows).into_bytes())
            }
        }
    }

    fn table_from_snapshot(snap: &crate::workbook::SheetSnapshot) -> crate::xlsx::SheetTable {
        let values = formula::recalculate(&snap.raw);
        let mut fmt = snap.fmt.clone();
        for (addr, raw) in &snap.raw {
            if !snap.fmt.contains_key(addr)
                && let Some(body) = raw.strip_prefix('=')
                && let Some(inferred) = formula::infer_result_format(body, &snap.fmt)
            {
                fmt.insert(*addr, inferred);
            }
        }
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &fmt, &values, &snap.bold)
    }

    /// Add `tables` as new sheets now, as an import does, and show the last.
    #[cfg(test)]
    fn import_tables(
        &mut self,
        doc: &mut Edits<'_>,
        tables: Vec<crate::xlsx::SheetTable>,
    ) -> Result<usize, String> {
        let (invoke, total) = self.import_action(tables, None)?;
        if !self.submit(doc, invoke) {
            return Err(self.status.clone());
        }
        self.active_sheet = self.sheet_ids.len().saturating_sub(1);
        self.reload();
        Ok(total)
    }

    fn switch_sheet(&mut self, doc: &mut Edits<'_>, idx: usize) {
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

    fn add_sheet(&mut self, doc: &mut Edits<'_>) {
        let n = self.sheet_ids.len();
        self.submit(
            doc,
            Workbook::NEW_SHEET.with(verbs::NewSheet {
                name: None,
                after: None,
            }),
        );
        // The active sheet tab is presentation, not a document change. The
        // appended sheet is at `n`.
        self.active_sheet = n;
        self.grid.select_only((0, 0));
        self.reload();
        self.status = "Added sheet".into();
    }

    // --- settings actions ---------------------------------------------------

    fn apply_setting(&mut self, doc: &mut Edits<'_>, kind: SettingKind) {
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

    // --- command palette ----------------------------------------------------

    /// Seed the Ctrl+K command palette with the current action set and open it.
    /// Rebuilt each time so argument prefills (e.g. the active cell reference)
    /// stay current. Every entry is wired to a real mutation in
    /// [`dispatch_command`]; nothing here is a stub.
    fn open_command_palette(&mut self) {
        let resources = self.published();
        self.open_palette_with(&resources);
    }

    /// Open the command palette on `resources`, the workbook as published.
    fn open_palette_with(&mut self, resources: &[tilo_actions::Resource]) {
        // Close any other overlay so the palette owns the surface.
        self.toolbar.close_menus();
        self.settings_panel = SettingsPanel::None;
        self.tab_menu_target = None;
        self.name_box_edit = None;
        self.tab_rename = None;

        let active_ref = {
            let (c, r) = self.grid.active();
            crate::model::addr_to_a1(c, r)
        };
        self.palette.clear();
        let mut actions = tilo_actions::flatten(resources);
        actions.sort_by_key(|action| usize::from(action.action.id != "go-to-cell"));
        for action in actions {
            if action.action.id == "commands" {
                continue;
            }
            let mut entry = CommandEntry::from_flat_action(&action);
            // The palette already says whose commands these are; the row is
            // just the command.
            entry.label.clone_from(&action.action.label);
            if action.action.id == "go-to-cell" {
                entry = entry.with_argument_prefill(active_ref.clone());
            }
            self.palette.push(entry);
        }
        self.palette.open();
        self.pending.redraw = true;
    }

    #[cfg(test)]
    fn dispatch_palette_view_for_test(&mut self) {
        let intent = ui::widget::controls::take_control_invocation(self)
            .expect("palette emitted a resource action");
        let resources = self.published();
        let spec = tilo_actions::door::resolve(&resources, &intent.address)
            .expect("palette address resolves");
        assert_eq!(spec.kind, tilo_actions::OperationKind::View);
        let encoded = spec.build_json(&intent.arguments).expect("valid arguments");
        assert_eq!(
            encoded.type_id,
            <actions::SheetsEffect as ui::tilo_resource::Effect>::OPERATION
        );
        let effect =
            <actions::SheetsEffect as ui::tilo_resource::Effect>::from_payload(&encoded.payload)
                .expect("a workbook effect");
        self.show(effect).expect("the view is shown");
    }

    /// Run the action the palette emitted, dispatching by id. `argument`
    /// carries the free-text a `with_argument` entry collected (e.g. the cell
    /// reference for "Go to cell").
    #[cfg(test)]
    fn dispatch_command(&mut self, doc: &mut Edits<'_>, id: &str, argument: Option<&str>) {
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
    #[cfg(test)]
    fn sum_selection(&mut self, doc: &mut Edits<'_>) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        self.apply_cmds(doc, out);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let target_r = r1.saturating_add(1);
        let mut writes: Vec<(Addr, String, Option<CellFormat>)> = Vec::new();
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
    fn insert_row(&mut self, doc: &mut Edits<'_>) {
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
        self.status = format!("Inserted row {}", r0.saturating_add(1));
    }

    #[cfg(test)]
    fn delete_row(&mut self, doc: &mut Edits<'_>) {
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
        self.status = format!("Deleted row {}", r0.saturating_add(1));
    }

    #[cfg(test)]
    fn insert_column(&mut self, doc: &mut Edits<'_>) {
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
        self.status = format!(
            "Inserted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    #[cfg(test)]
    fn delete_column(&mut self, doc: &mut Edits<'_>) {
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
        self.status = format!(
            "Deleted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    // --- layout -------------------------------------------------------------

    /// The File menu's rows, in order.
    fn file_menu_items() -> [(&'static str, MenuItem); 7] {
        [
            ("New sheet", MenuItem::NewSheet),
            ("Import CSV", MenuItem::ImportCsv),
            ("Import Excel workbook", MenuItem::ImportXlsx),
            ("Import OpenDocument spreadsheet", MenuItem::ImportOds),
            ("Export CSV", MenuItem::ExportCsv),
            ("Export Excel workbook", MenuItem::ExportXlsx),
            ("Export OpenDocument spreadsheet", MenuItem::ExportOds),
        ]
    }

    /// The Format menu's rows, in order, bound to the workbook's locale.
    fn format_menu_items(&self) -> [(&'static str, MenuItem); 9] {
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
    /// A fresh transient action message (e.g. "Pasted into B2") wins; absent
    /// one, the text is the live selection summary (count, sum and average of
    /// the numeric selection) or, absent numbers, the active cell's format.
    /// The idle default `"Ready"` (and the empty string) count as "no action
    /// message" so they never surface on their own.
    fn status_bubble_label(&self) -> Option<String> {
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

/// A numeric summary of the selection shown in the status bar.
struct SelectionSummary {
    sum: f64,
    avg: f64,
    count: usize,
    fmt: CellFormat,
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
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::widget::WindowHeader
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
        self.current_active_format = self.format_of_active();
        self.active_cell_bold = self.grid.cell_is_bold(self.grid.active());
        // Hand the composed label to the floating status bubble. It shows
        // only when the text actually changes — a selection move/extend, or a
        // fresh action message — so calling this every frame is fine, and the
        // idle nothing state (empty label) never surfaces.
        let label = self.status_bubble_label().unwrap_or_default();
        self.status_bubble.show(&label, ui::time::now());
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
        self.rect = ui::widget::layout_rect(taffy, node, origin);
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
        // A staged menu or sheet opens once the chrome is measured, so it
        // anchors to what is painted (and the More menu knows whether the bar
        // overflowed).
        self.apply_staged_overlay();
        self.draw_chrome(scene, transform, text_renderer, theme, presence);
        self.grid.draw(scene, transform, text_renderer, theme);
        self.draw_annotation_dock(scene, transform, text_renderer, theme);
        // The transient floating status bubble sits above the grid and the
        // tab strip, below the top layer.
        self.draw_status_bubble(scene, transform, text_renderer, theme);
        self.draw_tab_rename(scene, transform, text_renderer, theme);
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
        self.motion = if reduced {
            MotionPreference::Reduced
        } else {
            MotionPreference::Full
        };
        self.palette.set_motion_reduced(reduced);
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while the floating status bubble is lingering /
        // fading, so it hides itself without needing a stray pointer event.
        (self.motion == MotionPreference::Full && self.status_bubble.needs_frame(ui::time::now()))
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

    fn settle(&mut self, edits: &mut Edits<'_>, _path: &ObjId) {
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
            self.last_mouse = Point::new(
                position.x / event_ctx.scale_factor,
                position.y / event_ctx.scale_factor,
            );
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

    /// A cell being edited takes input-method composition.
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.grid
            .is_editing()
            .then_some(winit::window::ImePurpose::Normal)
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
        // The command bar's own nodes (its commands, the More button and
        // any open menu), the window's History control, then the formula bar.
        let toolbar_controls: Vec<AccessNode> = Widget::accessibility(&self.toolbar)
            .map(|toolbar| toolbar.children)
            .unwrap_or_default()
            .into_iter()
            .chain(self.history.accessibility(self.history_rect))
            .chain([
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
            ])
            .collect();
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
        Some(
            AccessNode::new(Role::Document)
                .with_name("Spreadsheet")
                .with_description("Editable workbook with formula bar and sheet tabs")
                .with_state(AccessState::new().focusable(true))
                .with_children(vec![
                    AccessNode::new(Role::Toolbar)
                        .with_name("Spreadsheet actions")
                        .with_children(toolbar_controls),
                    AccessNode::new(Role::TabBar)
                        .with_name("Sheets")
                        .with_children(tabs),
                    sheet,
                    annotations,
                ]),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self.history.perform_semantic_action(target, action) {
            self.pending.redraw = true;
            return true;
        }
        if self
            .toolbar
            .perform_semantic_action(target, action, state, path)
        {
            self.drain_command_bar(state);
            self.pending.redraw = true;
            return true;
        }
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

    fn mark_drawn(&mut self) {
        self.pending.redraw = false;
        self.grid.mark_drawn();
    }
}

impl ui::presence::capability::PublishesPresence for SheetsApp {
    /// Announce which sheet the local user is viewing, so peers can flag
    /// that tab. Keyed by sheet name (the same key
    /// `Self::draw_sheet_tab_presence` reads on the receiving side).
    fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
        let name = self.sheet_names.get(self.active_sheet)?;
        Some((
            PresenceKey::Tab { id: name.clone() },
            PresencePayload::Arrived,
        ))
    }
}

/// The command bar's row is the window's header row: it holds the History
/// control the host offers, so the window draws no History bar above the
/// sheet.
impl ui::widget::WindowHeader for SheetsApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        if self.history.control() != control {
            self.history.set_control(control);
            self.relayout();
            self.pending.redraw = true;
        }
    }
}

impl ui::widget::controls::ActionControls for SheetsApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.history
            .take()
            .or_else(|| self.palette.take_control_invocation())
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

impl SheetsApp {
    /// Route every input kind through the active shared inline editor before
    /// the spreadsheet's grid or chrome can see it. Both editors are modal in
    /// Sheets: a tab rename blur-commits, while a Name-box blur cancels.
    fn route_inline_editor(
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

    /// Left-press: routes to whichever modal surface is open (palette,
    /// rename, name box, tab menu, settings panel, dropdown menu), then the
    /// menu bar / name box / fx / formula field / sheet tabs, and finally the
    /// grid body, in priority order.
    fn handle_left_press(
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

    /// The command palette is modal while open: a click on a row runs it, a
    /// click elsewhere dismisses it.
    fn try_palette_click(
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

    /// Chrome hit-tests below the modal/menu checks: name box, "fx", formula
    /// field, and the sheet-tab strip.
    fn try_chrome_click(
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

    fn try_annotation_click(
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

    /// Ask in the palette for what the published action `action` of the
    /// annotation `place` (`chart:<id>`, `comment:<id>`) takes, starting
    /// from `prefill`; the palette invokes it as Ctrl+K does.
    fn ask_annotation(&mut self, place: &str, action: &str, prefill: String) -> bool {
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

    fn apply_annotation_click(&mut self, action: AnnotationClick, state: &mut Edits<'_>) {
        let Some(sheet) = self.active_sheet_id() else {
            return;
        };
        match action {
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

    fn annotation_click_from_target(&self, target: &str) -> Option<AnnotationClick> {
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

    /// Fallback: click lands on the grid body. Detects double-click (skipped
    /// while a formula is being edited, since grid clicks there are
    /// reference insertions in point mode) and drives the resulting
    /// pointer-down / selection.
    fn try_grid_click(
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
        let before = self.last_mouse;
        self.last_mouse = Point::new(x / scale_factor, y / scale_factor);
        // The painted buttons and tabs light up under the pointer, so a move
        // into, within or out of the bars repaints them.
        let on_bar = |p: Point| {
            self.toolbar_rect.contains(p) || self.tabs_rect.contains(p) || self.annotation_hit(p)
        };
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
        // Familiar spreadsheet navigation remains available even when the
        // compact toolbar has moved secondary commands into More.
        if let WindowEvent::KeyboardInput { event: ke, .. } = event {
            let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
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

    /// Ctrl+C/X/V/A/D/R while the grid selection (not a cell edit) owns the
    /// keyboard. Returns whether the chord was handled.
    fn handle_grid_clipboard_chord(
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

    fn run_menu_item(&mut self, doc: &mut Edits<'_>, item: &MenuItem) {
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

    /// Make every selected cell bold, or regular again when they all are.
    fn toggle_bold(&mut self, doc: &mut Edits<'_>) {
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
    fn toggle_formula_view(&mut self) {
        let on = self.grid.toggle_formula_view();
        self.status = if on {
            "Showing formulas".into()
        } else {
            "Showing results".into()
        };
        self.pending.redraw = true;
    }

    fn begin_tab_rename(&mut self, idx: usize) {
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

    fn resolve_tab_rename_outcome(&mut self, doc: &mut Edits<'_>, outcome: EditOutcome) {
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

    // --- name box (Go-To) + fx formula entry -------------------------------

    /// Open the Name box for a Go-To entry, seeded with the active cell's
    /// reference and fully selected (Excel-style: the first keystroke replaces
    /// it). Closes any other overlay so the box owns input.
    fn begin_name_box_edit(&mut self) {
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
    #[allow(
        clippy::too_many_lines,
        reason = "tab menu actions stay together to preserve their shared active-sheet update rules"
    )]
    fn run_tab_action(&mut self, doc: &mut Edits<'_>, idx: usize, act: TabAction) {
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
                        name: None,
                        after: Some(id),
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
}

#[cfg(test)]
#[path = "app/r2_annotation_cards.rs"]
mod r2_annotation_cards;

#[cfg(test)]
mod interaction_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::actions::SheetsEffect;
    use crate::grid::GridCmd;
    use automerge::AutoCommit;
    use automerge::transaction::Transactable as _;
    use ui::framework::testing::TestApp;
    use ui::portal::PortalOutcome;

    /// Serializes tests that round-trip through the process-global system
    /// clipboard, so a parallel test's copy can't land between another's
    /// copy and paste (the app itself is single-instance, so this only
    /// matters under the parallel test harness).
    pub(crate) static CLIP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An app whose edits go through a workbook dispatcher onto the document
    /// a test hands it.
    pub(crate) fn attached_app(doc: &AutoCommit) -> SheetsApp {
        let mut app = SheetsApp::new();
        app.attach_framework(
            ui::framework::testing::dispatcher("so.tilo.sheets"),
            ui::framework::testing::detached_live::<Workbook>(doc).unwrap(),
        );
        app
    }

    /// A new workbook's document: one empty sheet.
    pub(crate) fn new_doc() -> AutoCommit {
        ui::tilo_resource::Replica::<Workbook>::new().doc().clone()
    }

    /// The sample workbook's document.
    pub(crate) fn sample_doc() -> AutoCommit {
        crate::sample::replica().doc().clone()
    }

    /// The workbook `doc` holds.
    pub(crate) fn doc_workbook(doc: &AutoCommit) -> Workbook {
        use ui::tilo_resource::Resource as _;
        Workbook::read(doc).unwrap()
    }

    /// Sheet `idx` of `doc` by position.
    pub(crate) fn doc_snapshot(
        doc: &AutoCommit,
        idx: usize,
        _currency: Currency,
    ) -> crate::workbook::SheetSnapshot {
        doc_workbook(doc)
            .sheets
            .values()
            .nth(idx)
            .map(crate::workbook::Sheet::snapshot)
            .unwrap_or_default()
    }

    /// What the cell at `addr` of sheet `idx` of `doc` holds.
    pub(crate) fn doc_cell_raw(doc: &AutoCommit, idx: usize, addr: Addr) -> String {
        doc_snapshot(doc, idx, Currency::USD)
            .raw
            .get(&addr)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn doc_sheet_count(doc: &AutoCommit) -> usize {
        doc_workbook(doc).sheets.len()
    }

    pub(crate) fn doc_sheet_names(doc: &AutoCommit) -> Vec<String> {
        doc_workbook(doc)
            .sheets
            .values()
            .map(|sheet| sheet.name.0.clone())
            .collect()
    }

    pub(crate) fn doc_sheet_color(doc: &AutoCommit, idx: usize) -> Option<u32> {
        doc_workbook(doc)
            .sheets
            .values()
            .nth(idx)
            .and_then(|sheet| sheet.color)
    }

    /// Apply what the app asked the person to confirm (a destructive verb
    /// waits for them), as the door does once they agree, and follow it.
    pub(crate) fn confirm(app: &mut SheetsApp, doc: &mut AutoCommit) {
        let Some(dispatch) = app.dispatch.clone() else {
            return;
        };
        let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
        for action in dispatch.take_asked().expect("what was asked builds") {
            book.apply_action(&action)
                .expect("a confirmed change applies");
        }
        *doc = book.doc().clone();
        app.sync(doc, &automerge::ROOT);
    }

    /// An app over the sample workbook a screenshot shows, staged as a plain
    /// capture is and synced once, as every frame is before it draws.
    pub(super) fn seeded_app() -> (SheetsApp, AutoCommit) {
        let mut doc = sample_doc();
        let mut app = attached_app(&doc);
        app.stage_scenario(None);
        app.settle(&mut Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        (app, doc)
    }

    /// A seeded app laid out at `width × height` whose chrome metrics were
    /// resolved against a renderer at `text_scale`, exactly as the first draw
    /// of a frame does. Returns the renderer and the theme so a test can
    /// re-measure labels with the same instances.
    pub(super) fn measured_app(
        width: f64,
        height: f64,
        text_scale: f32,
    ) -> (SheetsApp, TextRenderer, Theme) {
        let (app, _doc, tr, theme) = measured_app_with_doc(width, height, text_scale);
        (app, tr, theme)
    }

    /// [`measured_app`] with the document the app was seeded into, for a test
    /// that drives events against it.
    pub(super) fn measured_app_with_doc(
        width: f64,
        height: f64,
        text_scale: f32,
    ) -> (SheetsApp, AutoCommit, TextRenderer, Theme) {
        let (mut app, doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, width, height);
        app.relayout();
        let theme = Theme::dark().with_text_scale(text_scale);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        app.text_scale = f64::from(theme.text_scale);
        app.grid.set_text_scale(app.text_scale);
        app.refresh_chrome_metrics(&mut tr, &theme);
        (app, doc, tr, theme)
    }

    /// Paint one whole frame, then the top layer, as the host does, so the
    /// hit rects the app keeps between frames are the ones a pointer meets.
    pub(super) fn paint_frame(app: &mut SheetsApp, tr: &mut TextRenderer, theme: &Theme) {
        let mut scene = Scene::new();
        let presence = PresenceCtx::default();
        app.draw(&mut scene, Affine::IDENTITY, tr, theme, &presence);
        app.draw_overlay(&mut scene, Affine::IDENTITY, tr, theme, &presence);
    }

    /// Final-pixel width of a tab name in the chosen tab's strong `Label`.
    fn tab_label_w(tr: &mut TextRenderer, theme: &Theme, label: &str) -> f64 {
        f64::from(
            tr.measure_style(label, None, &theme.text(TextRole::Label).strong())
                .0,
        )
    }

    /// Chrome bands, status bubble, tabs, settings sheet, tab menu and pointer targets follow measured text.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn chrome_density_follows_measured_text() {
        // Case `chrome_metrics_keep_the_one_x_density`.
        // At 1× the measured chrome is built from the toolkit's control
        // heights and the spacing scale: the command bar is the window's
        // header row, a 40 px formula bar, a 44 px tab strip, the
        // small control height for the column header and the default row, and
        // a 56 px row header.
        {
            let (app, _tr, theme) = measured_app(1280.0, 720.0, 1.0);
            assert!(
                (app.toolbar_rect.height() - ui::chrome::HeaderRowFace::height(&theme)).abs()
                    < 0.01,
                "command bar"
            );
            assert!(
                (app.formula_rect.height() - 40.0).abs() < 0.01,
                "formula bar"
            );
            assert!((app.tabs_rect.height() - 44.0).abs() < 0.01, "tab strip");
            let g = app.grid.metrics();
            assert!((g.col_header_h() - 28.0).abs() < 1.0, "column header");
            assert!(
                g.default_row_h() >= 28.0 && g.default_row_h() < 30.0,
                "default row {}",
                g.default_row_h()
            );
            assert!((g.row_header_w() - 56.0).abs() < 0.01, "row header");
        }

        // Case `chrome_bands_and_labels_follow_measured_text_at_two_x`.
        // At 2× every chrome band, button and label slot follows the measured
        // text: nothing is painted from the 1× geometry, and a role is scaled
        // exactly once (a size fed to the renderer twice would need roughly
        // twice the room again).
        {
            let (app, mut tr, theme) = measured_app(1280.0, 720.0, 2.0);
            let m = app.metrics.clone();
            assert!(
                m.lh_label > 32.0 && m.lh_label < 36.0,
                "label line box {}",
                m.lh_label
            );

            // Bands grew to their text.
            assert!(app.toolbar_rect.height() > 44.0);
            assert!(app.formula_rect.height() >= m.lh_body + 2.0 * m.s1);
            assert!(app.tabs_rect.height() >= m.lh_label + m.s1);
            assert!(app.grid.metrics().col_header_h() >= m.lh_caption);
            assert!(app.grid.metrics().default_row_h() >= m.lh_body);

            // Commands: one centre line in the header row, each at least a
            // line box tall.
            let shown: Vec<Rect> = chrome::bar::COMMANDS
                .iter()
                .filter_map(|index| app.toolbar.item_rect(*index))
                .collect();
            assert!(shown.len() >= 6, "the whole bar fits 1280 px at 2x");
            for rect in &shown {
                assert!(
                    (rect.center().y - app.toolbar_rect.center().y).abs() < 0.5,
                    "one centre line"
                );
                assert!(
                    rect.height() >= m.lh_label,
                    "button taller than its line box"
                );
            }

            // Formula bar: the fx label fits its own hit slot, which is
            // exactly where it is painted; field and name box share one centre
            // line.
            assert!(app.fx_rect.width() >= m.fx_w + 2.0 * m.s2 - 0.01);
            assert!((app.fx_rect.center().y - app.name_box_rect.center().y).abs() < 0.01);
            assert!(
                (app.formula_field_rect.center().y - app.name_box_rect.center().y).abs() < 0.01
            );
            let reference = f64::from(
                tr.measure_style("ZZ999:ZZ999", None, &theme.text(TextRole::Body))
                    .0,
            );
            assert!(app.name_box_rect.width() >= reference);

            // Sheet tabs fit their names and share the strip's centre line.
            for (rect, idx) in &app.tab_rects {
                let name = &app.sheet_names[*idx];
                let w = tab_label_w(&mut tr, &theme, name);
                assert!(rect.width() >= w + 2.0 * m.s3 - 0.01, "tab '{name}' fits");
                assert!(rect.height() >= m.lh_label);
                assert!((rect.center().y - app.add_tab_rect.center().y).abs() < 0.01);
            }
            assert!(
                (app.add_tab_rect.width() - app.add_tab_rect.height()).abs() < 0.01,
                "+ is square"
            );
        }

        // Case `status_bubble_floats_above_the_scrollbar_gutter`.
        // The floating status pill anchors inside the grid body, clear of the
        // scroll bar gutters and never on the tab strip.
        {
            for scale in [1.0_f32, 2.0] {
                let (mut app, mut tr, theme) = measured_app(1200.0, 800.0, scale);
                let area = app.status_area();
                let gutter = app.grid.scrollbar_thickness();
                assert!(
                    (area.y1 - (app.grid_rect.y1 - gutter)).abs() < 0.01,
                    "the area stops at the horizontal bar's gutter"
                );
                assert!(
                    (area.x1 - (app.grid_rect.x1 - gutter)).abs() < 0.01,
                    "the area stops at the vertical bar's gutter"
                );
                app.status_bubble.show("Currency (USD)", ui::time::now());
                let pill = app.status_bubble.rect_in(area, &mut tr, &theme);
                assert!(
                    pill.y1 <= area.y1 && pill.x1 <= area.x1,
                    "{pill:?} in {area:?}"
                );
                assert!(pill.y1 < app.tabs_rect.y0);
                // The pill is at least as tall as its measured text, so a 2×
                // label never spills out of it.
                let (_, th) =
                    tr.measure_style("Currency (USD)", None, &theme.text(TextRole::Label));
                assert!(
                    pill.height() >= f64::from(th) - 0.01,
                    "pill height {} at {scale}x",
                    pill.height()
                );
            }
        }

        // Case `sheet_tabs_keep_their_floor_and_ceiling_at_every_scale`.
        // Sheet tabs stay between the (text-grown) floor and the long-name
        // ceiling, so one long name never pushes the "+" button off the
        // strip, and the face elides rather than overruns.
        {
            for scale in [1.0_f32, 2.0] {
                let (mut app, mut tr, theme) = measured_app(1000.0, 700.0, scale);
                let long = "Quarterly consolidated regional operating expenses by cost centre";
                app.sheet_names = vec![long.to_owned(), "Q".to_owned()];
                app.refresh_chrome_metrics(&mut tr, &theme);
                app.relayout();
                let g = app.metrics.text_growth();
                let s16 = app.metrics.s16;
                let (wide, _) = app.tab_rects[0];
                let (narrow, _) = app.tab_rects[1];
                assert!(
                    wide.width() <= 3.0 * s16 * g + 0.01,
                    "long name is capped at {scale}x"
                );
                assert!(
                    narrow.width() >= s16 * g - 0.01,
                    "short name keeps the floor at {scale}x"
                );
                assert!(app.add_tab_rect.x1 <= app.rect.x1, "'+' stays on the strip");
                // The face's elision fits the capped tab with `s3` each side.
                let style = theme.text(TextRole::Label).strong();
                let room = (wide.width() - 2.0 * app.metrics.s3) as f32;
                let elided = tr.elide_to_width_style(long, room, &style);
                let (ew, _) = tr.measure_style(&elided, None, &style);
                assert!(f64::from(ew) <= f64::from(room) + 0.5);
                assert!(elided.ends_with('…') && elided.chars().count() < long.chars().count());
            }
        }

        // Case `settings_sheet_options_fit_the_sheet_at_every_scale`.
        // Settings sheet: every option fits its label inside the sheet's
        // body, rows of chips do not overlap, and the sheet stays in the
        // window.
        {
            for scale in [1.0_f32, 2.0] {
                let (mut app, mut tr, theme) = measured_app(1200.0, 800.0, scale);
                app.open_doc_settings();
                paint_frame(&mut app, &mut tr, &theme);
                let sheet = app.settings_sheet.as_ref().expect("the sheet laid out");
                assert!(
                    app.rect.contains(sheet.modal.card.center())
                        && sheet.modal.card.x0 >= app.rect.x0
                        && sheet.modal.card.x1 <= app.rect.x1
                        && sheet.modal.card.y1 <= app.rect.y1,
                    "the sheet stays in the window at {scale}x"
                );
                for chip in &sheet.options {
                    assert!(
                        sheet.modal.body.x0 - 0.01 <= chip.rect.x0
                            && chip.rect.x1 <= sheet.modal.body.x1 + 0.01,
                        "{:?} stays inside the body at {scale}x",
                        chip.rect
                    );
                    let (label, _) = app.chip_label_and_state(chip.kind);
                    let style = theme.text(TextRole::Label);
                    let w = f64::from(tr.measure_style(&label, None, &style).0);
                    assert!(chip.rect.width() >= w, "option '{label}' fits");
                }
                for (i, a) in sheet.options.iter().enumerate() {
                    for b in sheet.options.iter().skip(i + 1) {
                        assert!(
                            a.rect.intersect(b.rect).area() < 0.01,
                            "options {:?} and {:?} overlap at {scale}x",
                            a.rect,
                            b.rect
                        );
                    }
                }
                assert!(
                    sheet
                        .modal
                        .actions
                        .iter()
                        .all(|done| sheet.modal.card.contains(done.center())),
                    "Done is on the sheet"
                );
            }
        }

        // Case `tab_menu_lists_its_rows_and_stays_in_the_window_at_two_x`.
        // Sheet-tab menu: every row of the menu is a row the app answers,
        // and the menu stays inside the window at 2×.
        {
            let (mut app, mut tr, theme) = measured_app(1200.0, 800.0, 2.0);
            app.open_tab_menu(0);
            paint_frame(&mut app, &mut tr, &theme);
            assert!(app.tab_menu.is_open());
            assert_eq!(
                app.tab_menu.items().len(),
                app.tab_menu_rows.len(),
                "every menu row has an answer"
            );
            let panel = Widget::get_rect(&app.tab_menu);
            assert!(
                panel.x0 >= app.rect.x0 - 0.01
                    && panel.x1 <= app.rect.x1 + 0.01
                    && panel.y0 >= app.rect.y0 - 0.01
                    && panel.y1 <= app.rect.y1 + 0.01,
                "the menu {panel:?} stays inside the window"
            );
            let tab = app.tab_rects[0].0;
            assert!(
                panel.y1 <= tab.y0 + 0.01 || panel.y0 >= tab.y1 - 0.01,
                "the menu does not cover the tab it acts on"
            );
        }

        // Case `chrome_targets_meet_the_pointer_minimum_and_scale_with_type`.
        // Custom-painted controls have no toolkit button wrapper to enforce the
        // interaction minimum. Keep their hit geometry aligned with the shared
        // fine-pointer contract, and prove the same surfaces expand at 2× type.
        {
            let (app, _tr, _theme) = measured_app(560.0, 720.0, 1.0);
            let minimum =
                f64::from(ui::interaction::InteractionEnvironment::POINTER.minimum_target());
            let commands = |app: &SheetsApp| -> Vec<Rect> {
                chrome::bar::COMMANDS
                    .iter()
                    .filter_map(|index| app.toolbar.item_rect(*index))
                    .collect()
            };
            assert!(
                commands(&app)
                    .iter()
                    .all(|rect| rect.height() >= minimum - 0.01),
                "every command reaches the pointer minimum"
            );
            assert!(app.name_box_rect.height() >= minimum);
            assert!(app.formula_field_rect.height() >= minimum);
            assert!(
                app.tab_rects
                    .iter()
                    .all(|(rect, _)| rect.height() + 2.0 * app.metrics.s1 >= minimum)
            );

            let (app2, _tr, _theme) = measured_app(560.0, 720.0, 2.0);
            assert!(
                commands(&app2)
                    .iter()
                    .all(|rect| rect.height() >= minimum * 1.25)
            );
            assert!(
                app2.grid.accessibility_window().rows[0].bounds.height()
                    >= app.grid.accessibility_window().rows[0].bounds.height() * 1.5
            );
        }
    }

    /// Unchanged syncs and tab switches keep the tabs read, and reduced motion
    /// stops bubble frames.
    #[test]
    fn idle_frames_do_no_redundant_work() {
        // Case `unchanged_sync_and_tab_switch_keep_the_tabs`.
        {
            let (mut app, mut doc) = seeded_app();
            app.sync(&doc, &automerge::ROOT);
            let first_revision = app.revision;
            let first_ids = app.sheet_ids.as_ptr();
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(app.revision, first_revision);
            assert_eq!(app.sheet_ids.as_ptr(), first_ids);

            app.switch_sheet(&mut Edits::lend(&mut doc), 1);
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(app.sheet_ids.as_ptr(), first_ids);
        }

        // Case `reduced_motion_stops_status_bubble_animation_frames`.
        {
            let (mut app, _doc) = seeded_app();
            app.status_bubble.poke(ui::time::now());
            assert!(app.needs_animation_frame());
            app.set_motion_reduced(true);
            assert!(!app.needs_animation_frame());
        }
    }

    /// The on-screen affordances submit the workbook's own actions: the tab
    /// menu's Delete is the sheet's published `delete-sheet`, a cell edit
    /// is the field edit of that one cell.
    #[test]
    fn affordances_and_resource_operations_match_action_specs() {
        {
            let (mut app, mut doc) = seeded_app();
            let count_before = doc_sheet_count(&doc);
            assert!(count_before >= 2, "seeded workbook has multiple sheets");
            let sales = app.sheet_ids[1].clone();
            let item = ui::tilo_resource::items::item_resource_id::<Workbook>("sheets", &sales);
            let resources = app.published();
            let offered = tilo_actions::flatten(&resources)
                .into_iter()
                .any(|flat| flat.resource_id.as_str() == item && flat.action.id == "delete-sheet");
            assert!(offered, "the sheet offers its delete");

            app.run_tab_action(&mut Edits::lend(&mut doc), 1, TabAction::Delete);
            confirm(&mut app, &mut doc);
            assert_eq!(
                doc_sheet_count(&doc),
                count_before - 1,
                "the affordance really removed the sheet"
            );
        }

        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 9)); // A10, an empty cell
            let before = doc.get_heads();
            app.apply_cmds(
                &mut Edits::lend(&mut doc),
                vec![
                    GridCmd::SetCell {
                        addr: (0, 9),
                        raw: "hello".into(),
                    },
                    GridCmd::Commit,
                ],
            );
            assert_eq!(doc_cell_raw(&doc, 0, (0, 9)), "hello");
            let after = doc.get_heads();
            let patches = doc.diff(&before, &after);
            assert!(
                patches.iter().all(|patch| {
                    patch.path.iter().all(
                        |(_, prop)| !matches!(prop, automerge::Prop::Map(key) if key == "name"),
                    )
                }),
                "a cell edit writes its cell, not the sheet"
            );
        }
    }

    /// A workbook holding the sample's Budget and Sales sheets, run as the app.
    fn sample_app() -> TestApp<crate::resource::SheetsShell> {
        let mut app = TestApp::<crate::resource::SheetsShell>::new().unwrap();
        for invoke in crate::sample::workbook() {
            app.invoke_confirmed(invoke).unwrap();
        }
        app
    }

    /// The active sheet's index and the status the surface shows.
    fn shown(app: &mut TestApp<crate::resource::SheetsShell>) -> (usize, String) {
        app.with_surface::<SheetsApp, _>(|sheets| (sheets.active_sheet, sheets.status.clone()))
            .expect("the grid is the surface")
    }

    fn raw_at(app: &TestApp<crate::resource::SheetsShell>, sheet: usize, addr: Addr) -> String {
        app.state()
            .sheets
            .values()
            .nth(sheet)
            .map(crate::workbook::Sheet::snapshot)
            .and_then(|snapshot| snapshot.raw.get(&addr).cloned())
            .unwrap_or_default()
    }

    /// The workbook offers reading and writing every interchange format and
    /// its views as effects, and each sheet, chart and comment opens itself.
    #[test]
    fn the_workbook_publishes_its_effects_and_items_open_themselves() {
        let mut app = sample_app();
        let resources = app.harness().resources();
        let workbook = &resources[0];
        for id in [
            "import-csv",
            "import-xlsx",
            "import-ods",
            "export-csv",
            "export-xlsx",
            "export-ods",
            "commands",
            "go-to-cell",
            "toggle-formula-view",
        ] {
            assert!(
                workbook.actions.iter().any(|action| action.id == id),
                "{id} is published"
            );
        }
        let sales = workbook
            .children
            .iter()
            .find(|child| child.label == "Sales")
            .expect("the Sales sheet");
        assert!(
            workbook.actions.iter().any(|action| action.id == "open"),
            "the workbook opens what a search hit or an agent names"
        );

        app.call("open", serde_json::json!({ "anchor": sales.id.as_str() }))
            .expect("the sheet opens");
        app.settle();
        let (active, status) = shown(&mut app);
        assert_eq!(active, 1, "the Sales sheet is shown");
        assert_eq!(status, "Went to Sales!A1");

        let budget = app.state().sheets.ids().next().unwrap().clone();
        let cell = crate::resource::cell_anchor(&budget, (1, 4));
        app.call("open", serde_json::json!({ "anchor": cell }))
            .expect("a cell opens");
        app.settle();
        let selected = app
            .with_surface::<SheetsApp, _>(|sheets| (sheets.active_sheet, sheets.grid.active()))
            .unwrap();
        assert_eq!(selected, (0, (1, 4)), "Budget!B5 is selected");

        app.effect(&SheetsEffect::GoToCell {
            reference: "C3".into(),
        })
        .unwrap();
        app.effect(&SheetsEffect::ToggleFormulaView).unwrap();
        let view = app
            .with_surface::<SheetsApp, _>(|sheets| {
                (sheets.grid.active(), sheets.grid.formula_view())
            })
            .unwrap();
        assert_eq!(view, ((2, 2), true));
    }

    /// Every format exports to a path and imports back as new sheets (a CSV
    /// carries the active sheet, a workbook every sheet), in one undo step,
    /// showing the last sheet it added; opening the file again shows that
    /// sheet instead of importing a copy.
    #[test]
    fn every_format_exports_to_a_path_and_imports_back_once() {
        let dir = tempfile::tempdir().unwrap();
        for ext in ["csv", "xlsx", "ods"] {
            let mut app = sample_app();
            let path = dir.path().join(format!("Budget.{ext}"));
            let at = Some(path.display().to_string());
            let (export, import) = match ext {
                "csv" => (
                    SheetsEffect::ExportCsv { path: at.clone() },
                    SheetsEffect::ImportCsv { path: at },
                ),
                "xlsx" => (
                    SheetsEffect::ExportXlsx { path: at.clone() },
                    SheetsEffect::ImportXlsx { path: at },
                ),
                _ => (
                    SheetsEffect::ExportOds { path: at.clone() },
                    SheetsEffect::ImportOds { path: at },
                ),
            };
            app.effect_confirmed(&export).unwrap();
            assert!(path.exists(), "the .{ext} was written");
            assert!(shown(&mut app).1.starts_with("Exported"));

            let before = app.state().sheets.len();
            app.effect(&import).unwrap();
            app.settle();
            let added = if ext == "csv" { 1 } else { before };
            assert_eq!(app.state().sheets.len(), before + added, ".{ext}");
            assert_eq!(raw_at(&app, before, (0, 0)), "Category");
            if ext != "csv" {
                assert_eq!(
                    raw_at(&app, before + 1, (0, 0)),
                    "Region",
                    "the second sheet travels too"
                );
                assert_eq!(
                    raw_at(&app, before, (4, 1)),
                    "=SUM(B2:D2)",
                    "a formula comes back live"
                );
            }
            let (active, status) = shown(&mut app);
            assert_eq!(active, before + added - 1, "the last added sheet is shown");
            assert!(status.starts_with("Imported"), "{status}");

            app.with_surface::<SheetsApp, _>(|sheets| sheets.active_sheet = 0);
            app.effect(&import).unwrap();
            app.settle();
            assert_eq!(app.state().sheets.len(), before + added, "no second copy");
            let (active, status) = shown(&mut app);
            assert_eq!(active, before, "the open sheet is shown");
            assert!(status.contains("already open"), "{status}");

            assert!(app.undo(), "the import is one undo step");
            assert_eq!(app.state().sheets.len(), before);
        }
    }

    /// Without a path, the host's chooser picks where an export is saved and
    /// which file is imported; cancelling it changes nothing.
    #[test]
    fn the_file_chooser_saves_exports_and_picks_imports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Exported.csv");
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = sample_app();

        recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
        app.effect_confirmed(&SheetsEffect::ExportCsv { path: None })
            .unwrap();
        let written = std::fs::read_to_string(&path).expect("the chosen file was written");
        assert!(written.starts_with("Category"), "{written}");
        assert_eq!(
            recorder.pick_requests()[0].suggested_name(),
            Some("Budget.csv")
        );

        let before = app.state().sheets.len();
        recorder.respond_to_pick(PortalOutcome::Cancelled);
        app.effect(&SheetsEffect::ImportXlsx { path: None })
            .unwrap();
        app.settle();
        assert_eq!(
            app.state().sheets.len(),
            before,
            "a cancelled import adds nothing"
        );

        recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
        app.effect(&SheetsEffect::ImportCsv { path: None }).unwrap();
        app.settle();
        let state = app.state();
        assert_eq!(state.sheets.len(), before + 1);
        assert_eq!(
            state
                .sheets
                .values()
                .nth(before)
                .map(|sheet| sheet.name.0.as_str()),
            Some("Exported")
        );
        assert_eq!(raw_at(&app, before, (0, 0)), "Category");
        assert_eq!(recorder.pick_requests().len(), 3);

        // The same file chosen again, or opened by its path, is shown
        // rather than imported a second time.
        app.with_surface::<SheetsApp, _>(|sheets| sheets.active_sheet = 0);
        recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
        app.effect(&SheetsEffect::ImportCsv { path: None }).unwrap();
        app.settle();
        assert_eq!(app.state().sheets.len(), before + 1, "no second copy");
        let (active, status) = shown(&mut app);
        assert_eq!(active, before, "the imported sheet is shown");
        assert!(status.contains("already open"), "{status}");
        app.effect(&SheetsEffect::ImportCsv {
            path: Some(path.display().to_string()),
        })
        .unwrap();
        app.settle();
        assert_eq!(app.state().sheets.len(), before + 1, "by path either");
    }

    /// A file too large, or not a spreadsheet, fails its import with why,
    /// and the workbook is unchanged.
    #[test]
    fn a_file_that_cannot_be_read_fails_its_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.xlsx");
        std::fs::write(&path, b"not a workbook").unwrap();
        let mut app = sample_app();
        let before = app.state().sheets.len();
        let error = app
            .effect(&SheetsEffect::ImportXlsx {
                path: Some(path.display().to_string()),
            })
            .expect_err("a broken package fails");
        assert!(
            matches!(error, tilo_actions::DoorError::Failed(_)),
            "{error:?}"
        );
        assert_eq!(app.state().sheets.len(), before);
        assert!(
            shown(&mut app)
                .1
                .starts_with("Spreadsheet operation failed")
        );

        let sparse = format!("{}x", "\n".repeat(MAX_CSV_MATERIALIZED_SLOTS));
        let error = SheetsApp::read_tables(Interchange::Csv, sparse.as_bytes(), "sparse")
            .expect_err("a sparse far-away cell must not allocate a huge rectangle");
        assert!(error.contains("dimensions require"), "{error}");
    }

    fn value_at(app: &SheetsApp, addr: Addr) -> Option<CellValue> {
        app.cache
            .as_ref()
            .and_then(|c| c.values.get(&addr).cloned())
    }

    /// Presence announces the active sheet; switching tabs flushes the pending edit first.
    #[test]
    fn presence_and_sheet_switching_follow_the_active_tab() {
        // Case `presence_payload_announces_active_sheet`.
        // Presence-in-navigation, producer side: the app announces which
        // sheet the local user is on as a `Tab { id: sheet_name }` scope, so
        // peers can flag that tab. Switching sheets moves the announced scope.
        {
            let (mut app, _doc) = seeded_app();
            assert!(!app.sheet_names.is_empty(), "seeded app has sheets");

            let (key, payload) =
                ui::presence::capability::presence_payload(&app).expect("a sheet is active");
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
                let (key2, _) =
                    ui::presence::capability::presence_payload(&app).expect("still active");
                assert_eq!(
                    key2,
                    PresenceKey::Tab {
                        id: app.sheet_names[other].clone()
                    }
                );
            }
        }

        // Case `switch_sheet_flushes_edit_then_changes_active`.
        // Clicking a sheet tab flushes the pending in-cell edit onto the current
        // sheet, then switches the active sheet — the real `switch_sheet` path.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 30)); // A31 on sheet 0
            app.grid.begin_edit_in_place();
            app.grid.insert_text_public("draft");
            app.switch_sheet(&mut Edits::lend(&mut doc), 1); // click the "Sales" tab
            assert_eq!(app.active_sheet, 1);
            assert_eq!(
                doc.pending_ops(),
                0,
                "switching tabs is local presentation and must not write the shared workbook"
            );
            // The pending edit landed on sheet 0, not sheet 1.
            assert_eq!(doc_cell_raw(&doc, 0, (0, 30)), "draft");
            assert_eq!(doc_cell_raw(&doc, 1, (0, 30)), "");
        }
    }

    /// Coloured formula bodies, name box/fx hit-rects, and fx on empty or filled cells.
    #[test]
    fn formula_bar_and_fx_start_formulas() {
        // Case `selected_formula_bar_shows_colored_body`.
        {
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

        // Case `name_box_and_fx_rects_are_hit_testable`.
        // The Name box and "fx" affordance get real, ordered, non-overlapping
        // hit-rects to the left of the formula field, so `handle_event`'s
        // `contains(p)` routing lands on the right target.
        {
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

        // Case `fx_starts_a_formula_on_an_empty_cell`.
        // The "fx" affordance opens a formula edit on an empty cell, seeded with
        // `=` and the caret at the end.
        {
            let (mut app, _doc) = seeded_app();
            app.grid.select_only((0, 20)); // A21, empty
            assert!(!app.grid.is_editing());
            app.start_formula_entry();
            assert!(app.grid.is_editing());
            assert_eq!(app.grid.active_display_raw(), "=");
            assert_eq!(app.grid.edit_caret(), Some(1));
        }

        // Case `fx_prepends_equals_to_existing_content`.
        // On a cell that already holds a value, "fx" prepends `=` so the value
        // becomes the start of a formula, and it evaluates once committed.
        {
            let (mut app, mut doc) = seeded_app();
            app.apply_cmds(
                &mut Edits::lend(&mut doc),
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
            app.apply_cmds(&mut Edits::lend(&mut doc), out);
            assert_eq!(doc_cell_raw(&doc, 0, (0, 21)), "=5");
            assert_eq!(value_at(&app, (0, 21)), Some(CellValue::Number(5.0)));
        }
    }

    /// Seeded formulas compute, edits recalc dependents, `AutoSum` writes or refuses.
    #[test]
    fn recalculation_and_auto_sum_write_working_formulas() {
        // Case `seed_computes_real_formulas`.
        {
            let (app, _doc) = seeded_app();
            // Budget!E2 = SUM(B2:D2) = 1200 + 1200 + 1200 = 3600.
            assert_eq!(value_at(&app, (4, 1)), Some(CellValue::Number(3600.0)));
            // The active cell (E2) is a propagated currency.
            assert!(matches!(app.format_of_active(), CellFormat::Currency(_)));
        }

        // Case `editing_a_cell_recalculates_dependents`.
        {
            // This is the real path `handle_event` runs after `grid.key`: it hands
            // the emitted commands to `apply_cmds`, which infers the entry, writes
            // the CRDT, and recalculates.
            let (mut app, mut doc) = seeded_app();
            let b2 = (1, 1);
            let e2 = (4, 1);
            assert_eq!(value_at(&app, e2), Some(CellValue::Number(3600.0)));

            app.apply_cmds(
                &mut Edits::lend(&mut doc),
                vec![
                    GridCmd::SetCell {
                        addr: b2,
                        raw: "1000".into(),
                    },
                    GridCmd::Commit,
                ],
            );

            // The stored raw changed and the dependent SUM recomputed.
            assert_eq!(doc_cell_raw(&doc, 0, b2), "1000");
            assert_eq!(value_at(&app, e2), Some(CellValue::Number(3400.0)));
        }

        // Case `auto_sum_writes_a_working_formula`.
        {
            let (mut app, mut doc) = seeded_app();
            // Put the active cell just below a numeric run and AutoSum it.
            app.grid.select_only((1, 7)); // B8, under B2:B7
            app.auto_sum(&mut Edits::lend(&mut doc));
            let raw = doc_cell_raw(&doc, 0, (1, 7));
            assert!(raw.starts_with("=SUM("), "auto-sum wrote {raw:?}");
            // The computed value is finite and positive (a real sum, not an error).
            match value_at(&app, (1, 7)) {
                Some(CellValue::Number(n)) => assert!(n > 0.0),
                other => panic!("expected a numeric AutoSum result, got {other:?}"),
            }
        }

        // Case `auto_sum_guard_nothing_above`.
        // `AutoSum` on row 1 has nothing above it: it reports so and writes no formula.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 0)); // A1, row 0
            app.auto_sum(&mut Edits::lend(&mut doc));
            assert_eq!(app.status, "Nothing above to sum");
            assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "Category"); // unchanged
        }

        // Case `auto_sum_guard_no_numeric_run`.
        // `AutoSum` directly under a non-numeric cell finds no run and writes nothing.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 1)); // A2, directly under A1 = "Category" (text)
            let before = doc_cell_raw(&doc, 0, (0, 1));
            app.auto_sum(&mut Edits::lend(&mut doc));
            assert_eq!(app.status, "No numeric run above");
            assert_eq!(doc_cell_raw(&doc, 0, (0, 1)), before);
            assert!(!doc_cell_raw(&doc, 0, (0, 1)).starts_with("=SUM"));
        }
    }

    /// Format application over single and multi-range selections, clearing and percent commands.
    #[test]
    fn formats_apply_and_clear_across_selections() {
        // Case `apply_format_changes_active_type`.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((1, 8)); // B9 = 2600 (currency in the seed)
            app.apply_format(&mut Edits::lend(&mut doc), CellFormat::Percent);
            assert_eq!(app.format_of_active(), CellFormat::Percent);
        }

        // Case `format_applies_across_a_multi_range_selection`.
        // Format applied over a discontiguous multi-range selection reaches every
        // cell in *both* ranges — the real `apply_format` → `selected_cells` path.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_range_public((1, 1), (1, 2)); // B2:B3
            app.grid.add_range_public((3, 1), (3, 2)); // D2:D3 (discontiguous)
            app.apply_format(&mut Edits::lend(&mut doc), CellFormat::Percent);
            let snap = doc_snapshot(&doc, 0, Currency::USD);
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

        // Case `clear_format_resets_applied_format`.
        // Clear-format resets an applied format back to General — the real Format
        // menu "Clear format" path.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 25)); // A26, empty
            app.apply_format(&mut Edits::lend(&mut doc), CellFormat::Percent);
            assert_eq!(app.format_of_active(), CellFormat::Percent);
            app.clear_format(&mut Edits::lend(&mut doc));
            assert_eq!(app.format_of_active(), CellFormat::General);
            // General is not persisted in the snapshot, so the explicit format is gone.
            let snap = doc_snapshot(&doc, 0, Currency::USD);
            assert_eq!(snap.fmt.get(&(0, 25)).copied(), None);
        }

        // Case `command_format_percent_applies_to_selection`.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((1, 1)); // B2
            app.dispatch_command(&mut Edits::lend(&mut doc), "format_percent", None);
            // The stored format for B2 is now Percent.
            let cur = app.default_currency();
            let snap = doc_snapshot(&doc, 0, cur);
            assert_eq!(snap.fmt.get(&(1, 1)), Some(&CellFormat::Percent));
        }
    }

    /// The status summary aggregates real values across discontiguous ranges.
    #[test]
    fn selection_summary_aggregates_every_range() {
        // Case `multi_range_selection_summary_aggregates_both_ranges`.
        // The status-bar summary aggregates Sum/Avg/Count over a *discontiguous*
        // multi-range selection — the real `selection_summary` → `selected_cells`
        // path the status bar draws each frame. Column B (Jan) sums to 2040 and
        // column D (Mar) to 2180 in the seed, so the two-range total is 4220 over
        // 10 numeric cells.
        {
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

        // Case `selection_summary_spans_multiple_ranges`.
        // The status-bar summary counts numeric cells across *all* ranges of a
        // discontiguous selection.
        {
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

        // Case `selection_summary_sums_real_values`.
        {
            let (mut app, _doc) = seeded_app();
            // Select the Jan column body B2:B6 (five numeric cells).
            app.grid.set_selection((1, 1), (1, 5));
            let summary = app.selection_summary().expect("numeric selection");
            assert_eq!(summary.count, 5);
            // 1200 + 420 + 180 + 90 + 150 = 2040.
            assert!((summary.sum - 2040.0).abs() < 1e-6, "sum={}", summary.sum);
            assert!((summary.avg - 408.0).abs() < 1e-6, "avg={}", summary.avg);
        }
    }

    /// Imported tables restore live formulas, and a file imported into a new
    /// workbook replaces its blank sheet.
    #[test]
    fn imported_tables_restore_formulas_and_replace_a_blank_workbook() {
        // Case `xlsx_export_import_restores_live_formulas`.
        // The real export → import path for a workbook of formulas: the active
        // sheet is exported to a `.xlsx`, read back, and imported into a fresh
        // document. Formula cells restore their formula *text* (not a frozen
        // value) and recompute to the original results.
        {
            let (app, doc) = seeded_app();
            // Budget!E2 = =SUM(B2:D2) = 3600 in the seed.
            let table = SheetsApp::table_from_snapshot(&doc_snapshot(
                &doc,
                app.active_sheet,
                Currency::USD,
            ));
            let path = std::env::temp_dir().join("sheets-app-formula-rt.xlsx");
            crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
            let tables = crate::xlsx::read_xlsx(&path).unwrap();
            let _ = std::fs::remove_file(&path);

            // Import into a brand-new, empty document via the real import path.
            let mut doc2 = new_doc();
            let mut fresh = attached_app(&doc2);
            fresh
                .import_tables(&mut Edits::lend(&mut doc2), tables)
                .unwrap();

            // The formula came back as a formula, not its computed value.
            assert_eq!(doc_cell_raw(&doc2, 0, (4, 1)), "=SUM(B2:D2)");
            // …and it recomputes to the original result.
            let snap = doc_snapshot(&doc2, 0, Currency::USD);
            let values = formula::recalculate(&snap.raw);
            assert_eq!(values.get(&(4, 1)), Some(&CellValue::Number(3600.0)));
            // A non-formula neighbour still round-trips as a plain value.
            assert_eq!(doc_cell_raw(&doc2, 0, (1, 1)), "1200");
        }

        // Case `a_file_imported_into_a_new_workbook_is_the_workbook`.
        // A new workbook's empty Sheet1 gives way, so an export of the
        // imported file doesn't open on a blank first tab.
        {
            let mut doc = new_doc();
            let mut app = attached_app(&doc);
            let table = crate::xlsx::SheetTable {
                name: "sales".into(),
                rows: vec![vec![Some(crate::xlsx::ImportedCell {
                    raw: "Region".into(),
                    format: CellFormat::General,
                    formula: None,
                    bold: false,
                })]],
            };
            app.import_tables(&mut Edits::lend(&mut doc), vec![table.clone()])
                .unwrap();
            assert_eq!(doc_sheet_names(&doc), ["sales"]);
            assert_eq!(app.active_sheet, 0);
            app.import_tables(&mut Edits::lend(&mut doc), vec![table])
                .unwrap();
            assert_eq!(
                doc_sheet_names(&doc).len(),
                2,
                "a used workbook keeps its sheets"
            );
        }
    }

    /// Fill down/right and fill-handle drags extend series and shift references.
    #[test]
    fn fill_commands_and_handle_extend_series_and_formulas() {
        // Case `fill_down_copies_and_shifts_formulas`.
        // Fill-down copies the top cell of the selection down, shifting relative
        // formula references per row — the real Ctrl+D path.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_range_public((4, 1), (4, 3)); // E2:E4, E2 = =SUM(B2:D2)
            app.fill_down(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (4, 2)), "=SUM(B3:D3)");
            assert_eq!(doc_cell_raw(&doc, 0, (4, 3)), "=SUM(B4:D4)");
            // E3 recomputes against row 3's operands: 460 + 510 + ... etc > 0.
            match value_at(&app, (4, 2)) {
                Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
                other => panic!("expected numeric fill result, got {other:?}"),
            }
        }

        // Case `fill_handle_drag_extends_numeric_series`.
        // The fill handle: dragging it from a two-number selection extends a
        // linear series, driven through the production `pointer_up` →
        // `run_pending_fill` path (no display).
        {
            let (mut app, mut doc) = seeded_app();
            app.apply_cmds(
                &mut Edits::lend(&mut doc),
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
            app.run_pending_fill(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (7, 3)), "3");
            assert_eq!(doc_cell_raw(&doc, 0, (7, 4)), "4");
            assert_eq!(doc_cell_raw(&doc, 0, (7, 5)), "5");
            assert_eq!(value_at(&app, (7, 5)), Some(CellValue::Number(5.0)));
        }

        // Case `fill_handle_drag_shifts_formula_refs`.
        // The fill handle on a formula cell fills down with the relative refs
        // shifted per row — the same adjustment copy/paste applies.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
            app.grid.start_fill_to((4, 4)); // drag down to E5
            app.run_pending_fill(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (4, 2)), "=SUM(B3:D3)");
            assert_eq!(doc_cell_raw(&doc, 0, (4, 3)), "=SUM(B4:D4)");
            assert_eq!(doc_cell_raw(&doc, 0, (4, 4)), "=SUM(B5:D5)");
            match value_at(&app, (4, 4)) {
                Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
                other => panic!("expected numeric fill result, got {other:?}"),
            }
        }

        // Case `fill_handle_drag_fills_right`.
        // The fill handle also fills to the right, extending a series across
        // columns per row.
        {
            let (mut app, mut doc) = seeded_app();
            app.apply_cmds(
                &mut Edits::lend(&mut doc),
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
            app.run_pending_fill(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (9, 10)), "30");
            assert_eq!(doc_cell_raw(&doc, 0, (10, 10)), "40");
        }

        // Case `shift_refs_handles_anchors_functions_and_numbers`.
        // The copy/paste + fill relativisation the app dispatches goes through the
        // formula layer's `offset_refs`; prove the wiring on the shapes the paste
        // path relies on (the full matrix lives in `formula`'s own tests).
        {
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

        // Case `fill_right_copies_and_shifts_formulas`.
        // Fill-right copies the left column of the selection rightward, shifting
        // relative formula references per column — the real Ctrl+R path.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_range_public((4, 1), (6, 1)); // E2:G2, E2 = =SUM(B2:D2)
            app.fill_right(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (5, 1)), "=SUM(C2:E2)");
            assert_eq!(doc_cell_raw(&doc, 0, (6, 1)), "=SUM(D2:F2)");
            match value_at(&app, (5, 1)) {
                Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
                other => panic!("expected numeric fill result, got {other:?}"),
            }
        }
    }

    /// Copy/cut/paste of values, formats and formulas, range tiling and external TSV.
    /// A paste of many cells is one change: one history entry, one undo
    /// step.
    #[test]
    fn a_paste_is_one_change() {
        let _clipboard = CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut app, mut doc) = seeded_app();
        app.grid.select_range_public((0, 1), (3, 3)); // A2:D4
        app.copy_selection(false);
        app.grid.select_only((0, 20));
        let before = doc.get_heads();
        app.paste(&mut Edits::lend(&mut doc));
        assert_eq!(doc_cell_raw(&doc, 0, (0, 20)), "Rent");
        assert_eq!(
            doc_cell_raw(&doc, 0, (3, 22)),
            doc_cell_raw(&doc, 0, (3, 3))
        );
        assert_eq!(
            doc.get_changes(&before).len(),
            1,
            "one change for twelve cells"
        );
    }

    #[test]
    fn copy_cut_and_paste_relativise_formulas_correctly() {
        // Case `copy_paste_preserves_value_and_format`.
        {
            // Copy a currency cell into an empty one; the raw and the currency
            // format both come across, and it recomputes to the same number.
            let _clip = CLIP_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((1, 1)); // B2 = 1200, currency
            app.copy_selection(false);
            app.grid.select_only((1, 7)); // B8, empty
            app.paste(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (1, 7)), "1200");
            assert!(matches!(
                app.grid.cell_fmt_at((1, 7)),
                CellFormat::Currency(_)
            ));
            assert_eq!(value_at(&app, (1, 7)), Some(CellValue::Number(1200.0)));
        }

        // Case `copy_paste_shifts_relative_formula_refs`.
        {
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
            app.paste(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (5, 2)), "=SUM(C3:E3)");
            // C3=460, D3=510, E3=SUM(B3:D3)=1390 → 2360.
            assert_eq!(value_at(&app, (5, 2)), Some(CellValue::Number(2360.0)));
        }

        // Case `cut_moves_cell_and_clears_source`.
        {
            let _clip = CLIP_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 0)); // A1 = "Category"
            app.copy_selection(true); // cut
            app.grid.select_only((0, 20)); // A21, empty
            app.paste(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "");
            assert_eq!(doc_cell_raw(&doc, 0, (0, 20)), "Category");
        }

        // Case `cut_paste_formula_does_not_relativize`.
        // A **cut** moves a formula verbatim — its references follow the cells and
        // are *not* re-relativised (unlike a copy). The real cut→paste path.
        {
            let _clip = CLIP_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
            app.copy_selection(true); // cut
            app.grid.select_only((6, 10)); // G11, empty, far away
            app.paste(&mut Edits::lend(&mut doc));
            // The formula is unchanged: a move keeps references pointing at B2:D2.
            assert_eq!(doc_cell_raw(&doc, 0, (6, 10)), "=SUM(B2:D2)");
            assert_eq!(doc_cell_raw(&doc, 0, (4, 1)), "", "source cleared");
        }

        // Case `paste_tiles_a_formula_across_a_range`.
        // Pasting a copied formula into a **range** tiles the block, offsetting
        // each destination cell's relative references by its own position — the
        // real range-paste path.
        {
            let _clip = CLIP_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((4, 1)); // E2 = =SUM(B2:D2)
            app.copy_selection(false);
            // Target a 1×3 range F2:F4; the 1×1 block tiles down it.
            app.grid.select_range_public((5, 1), (5, 3));
            app.paste(&mut Edits::lend(&mut doc));
            assert_eq!(doc_cell_raw(&doc, 0, (5, 1)), "=SUM(C2:E2)");
            assert_eq!(doc_cell_raw(&doc, 0, (5, 2)), "=SUM(C3:E3)");
            assert_eq!(doc_cell_raw(&doc, 0, (5, 3)), "=SUM(C4:E4)");
        }

        // Case `paste_tsv_parses_external_grid`.
        // Pasting a tab/newline grid from another app parses and smart-types each
        // field — the real system-clipboard branch of `paste` (`paste_tsv`).
        {
            let (mut app, mut doc) = seeded_app();
            let target = (0, 40); // A41, empty region
            assert!(app.paste_tsv(&mut Edits::lend(&mut doc), target, "10\t20\n30\t40\n"));
            assert_eq!(doc_cell_raw(&doc, 0, (0, 40)), "10");
            assert_eq!(doc_cell_raw(&doc, 0, (1, 40)), "20");
            assert_eq!(doc_cell_raw(&doc, 0, (0, 41)), "30");
            assert_eq!(doc_cell_raw(&doc, 0, (1, 41)), "40");
            app.sync(&doc, &automerge::ROOT);
            assert_eq!(value_at(&app, (1, 41)), Some(CellValue::Number(40.0)));
            // Empty payload writes nothing.
            assert!(!app.paste_tsv(&mut Edits::lend(&mut doc), target, ""));
        }
    }

    /// Resize reflow, annotation touch targets and narrow large-text toolbars.
    #[test]
    fn layout_reflows_and_keeps_controls_usable_at_large_text() {
        // Case `layout_reflows_on_resize`.
        // The layout is re-derived from the current rect on every pass, so a
        // resize between widths reflows rather than freezing at the first size.
        // Drives the production `relayout` on one instance at two widths.
        {
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

        // Case `narrow_large_text_collapses_secondary_toolbar_actions_without_clipping`.
        {
            let (mut app, mut tr, theme) = measured_app(560.0, 720.0, 2.0);
            assert!(app.toolbar.item_rect(chrome::bar::FILE).is_some());
            assert!(app.toolbar.item_rect(chrome::bar::FORMAT).is_some());
            assert!(
                app.toolbar.has_overflow(),
                "what does not fit moves into More"
            );
            let shown = chrome::bar::COMMANDS
                .iter()
                .filter_map(|index| app.toolbar.item_rect(*index))
                .collect::<Vec<_>>();
            assert!(shown.iter().all(|rect| rect.x1 <= app.rect.x1));
            app.toolbar.open_overflow();
            paint_frame(&mut app, &mut tr, &theme);
            let (_, menu) = app.toolbar.shown_menu().expect("the More menu opens");
            assert_eq!(
                menu.items()
                    .iter()
                    .filter(|item| item.label().is_some())
                    .count(),
                // Six commands, File and Format among those shown.
                6 - shown.len(),
                "More holds exactly the commands the bar dropped"
            );
            let panel = Widget::get_rect(menu);
            assert!(panel.x0 >= app.rect.x0 && panel.x1 <= app.rect.x1);
        }
    }

    // --- sheet tab context-menu actions ------------------------------------

    /// Sheet-tab menu operations.
    #[test]
    fn tab_menu_renames_duplicates_deletes_moves_and_colours() {
        // Case `tab_rename_updates_sheet_name`.
        {
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
            assert_eq!(doc_sheet_names(&doc)[0], "Ledger");
        }

        // Case `tab_rename_escape_cancels_without_a_document_write`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = doc_sheet_names(&doc);
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
            assert_eq!(doc_sheet_names(&doc), before);
            assert_eq!(doc.pending_ops(), 0, "nothing was written");
        }

        // Case `tab_duplicate_creates_a_copy`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = doc_sheet_count(&doc);
            app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::Duplicate);
            assert_eq!(doc_sheet_count(&doc), before + 1);
            // Inserted right after the source and named "<name> copy".
            assert_eq!(doc_sheet_names(&doc)[1], "Budget copy");
            // The copy carries the source's formulas.
            assert_eq!(doc_cell_raw(&doc, 1, (4, 1)), "=SUM(B2:D2)");
        }

        // Case `tab_delete_removes_and_keeps_one_sheet`.
        {
            let (mut app, mut doc) = seeded_app();
            // Seed: Budget, Sales. Remove Sales.
            app.run_tab_action(&mut Edits::lend(&mut doc), 1, TabAction::Delete);
            confirm(&mut app, &mut doc);
            assert_eq!(doc_sheet_names(&doc), vec!["Budget"]);
            // The last remaining sheet cannot be deleted away.
            app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::Delete);
            assert_eq!(doc_sheet_count(&doc), 1);
        }

        // Case `tab_move_right_reorders_sheets`.
        {
            let (mut app, mut doc) = seeded_app();
            app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::MoveRight);
            assert_eq!(doc_sheet_names(&doc), vec!["Sales", "Budget"]);
            // The moved sheet keeps its data (Budget's E2 formula).
            assert_eq!(doc_cell_raw(&doc, 1, (4, 1)), "=SUM(B2:D2)");
        }

        // Case `tab_set_and_clear_colour`.
        {
            let (mut app, mut doc) = seeded_app();
            app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::SetColor(Some(2)));
            assert_eq!(doc_sheet_color(&doc, 0), Some(2));
            app.run_tab_action(&mut Edits::lend(&mut doc), 0, TabAction::SetColor(None));
            assert_eq!(doc_sheet_color(&doc, 0), None);
        }
    }

    /// Cache reuse and invalidation.
    #[test]
    fn reload_reuses_unchanged_cache_and_invalidates_it_after_an_edit() {
        // Case `reload_reuses_unchanged_cache_and_invalidates_it_after_an_edit`.
        {
            let (mut app, mut doc) = seeded_app();
            // Commit the startup seed and refresh once at its steady-state heads.
            doc.commit();
            app.sync(&doc, &automerge::ROOT);
            let cached = app
                .cache
                .as_ref()
                .expect("seed reload populates cache")
                .revision;

            app.pending.redraw = false;
            app.sync(&doc, &automerge::ROOT);
            assert!(
                !app.pending.redraw,
                "unchanged reload should return before rebuilding"
            );
            assert_eq!(app.cache.as_ref().unwrap().revision, cached);

            app.write_raw(
                &mut Edits::lend(&mut doc),
                vec![((0, 0), "1234".to_owned(), None)],
            );
            app.pending.redraw = false;
            app.sync(&doc, &automerge::ROOT);

            let refreshed = app
                .cache
                .as_ref()
                .expect("edited reload refreshes cache")
                .revision;
            assert_ne!(refreshed, cached, "an edit invalidates the cached state");
            assert_eq!(value_at(&app, (0, 0)), Some(CellValue::Number(1234.0)));
            assert!(app.pending.redraw, "rebuilt grid must request a redraw");
        }
    }

    // --- name box (Go-To) + fx formula entry -------------------------------

    /// Name box parsing, go-to cell/range, invalid and empty commits.
    #[test]
    fn name_box_parses_and_navigates_references() {
        // Case `parse_ref_target_handles_cells_ranges_and_junk`.
        // Parsing helper the Name box feeds: single refs and ranges, case- and
        // absolute-marker-insensitive; junk is rejected.
        {
            assert_eq!(parse_ref_target("b7"), Some(((1, 6), (1, 6))));
            assert_eq!(parse_ref_target("$A$1"), Some(((0, 0), (0, 0))));
            assert_eq!(parse_ref_target("A1:C3"), Some(((0, 0), (2, 2))));
            assert_eq!(parse_ref_target("not-a-ref"), None);
            assert_eq!(parse_ref_target("A1:"), None);
            assert_eq!(parse_ref_target(""), None);
        }

        // Case `name_box_goto_navigates_to_a_cell`.
        // Clicking the Name box, typing a reference, and pressing Enter moves the
        // selection there through the same shared-TextInput event path used live.
        {
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

        // Case `name_box_goto_selects_a_range`.
        // A `ref:ref` Name box entry selects the whole range.
        {
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

        // Case `name_box_invalid_reference_keeps_selection`.
        // An invalid Name box entry reports the error and leaves the selection put.
        {
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

        // Case `name_box_empty_commit_is_a_noop`.
        // Committing an emptied Name box is a no-op that just closes the editor.
        {
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
    }

    // --- fill-right / clear-format / switch-sheet --------------------------

    // --- external-clipboard (TSV) paste + auto-sum guards ------------------

    // -----------------------------------------------------------------------
    // Command palette (Ctrl+K) integration
    // -----------------------------------------------------------------------

    /// A `Modifiers` value with Control held.
    pub(super) fn ctrl_mods() -> Modifiers {
        let mut s = winit::keyboard::ModifiersState::empty();
        s.set(winit::keyboard::ModifiersState::CONTROL, true);
        Modifiers::from(s)
    }

    /// Synthesise a pressed `WindowEvent::KeyboardInput`.
    fn key_event(logical: winit::keyboard::Key) -> WindowEvent {
        ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            logical,
            None,
            ElementState::Pressed,
        ))
    }

    pub(super) fn char_event(s: &str) -> WindowEvent {
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
    pub(super) fn feed(
        app: &mut SheetsApp,
        doc: &mut AutoCommit,
        ev: &WindowEvent,
        mods: Modifiers,
    ) -> bool {
        ui::testing::Dispatch::new()
            .modifiers(mods)
            .doc(doc)
            .send(app, ev)
            .handled
    }

    /// A cell edit takes an input-method composition: shown at the caret,
    /// typed into the cell only on commit; Backspace removes whole clusters.
    #[test]
    fn cell_edit_composes_with_the_input_method_and_deletes_whole_clusters() {
        use winit::event::Ime;
        use winit::keyboard::NamedKey;
        let (mut app, mut doc) = seeded_app();
        app.grid.select_only((0, 20)); // A21, empty
        assert!(feed(
            &mut app,
            &mut doc,
            &char_event("a"),
            Modifiers::default()
        ));
        assert!(app.grid.is_editing());
        assert_eq!(
            app.ime_purpose(),
            Some(winit::window::ImePurpose::Normal),
            "the editing cell asks for the input method"
        );
        assert!(app.ime_cursor_rect().is_some());

        let ime = |app: &mut SheetsApp, doc: &mut AutoCommit, ime: Ime| {
            feed(app, doc, &WindowEvent::Ime(ime), Modifiers::default())
        };
        assert!(ime(
            &mut app,
            &mut doc,
            Ime::Preedit("にほ".into(), Some((3, 3)))
        ));
        assert_eq!(
            app.grid.edit_shown(),
            Some(("aにほ".to_owned(), 2)),
            "the composition shows at the caret, the caret inside it"
        );
        assert_eq!(app.grid.active_display_raw(), "a", "but is not typed yet");
        assert!(ime(&mut app, &mut doc, Ime::Commit("日本".into())));
        assert_eq!(app.grid.active_display_raw(), "a日本");

        // A combined letter, an emoji with a skin tone and a flag each go in
        // as one character and come out with one Backspace.
        for typed in ["e\u{301}", "👍🏽", "🇩🇪"] {
            assert!(ime(&mut app, &mut doc, Ime::Commit(typed.into())));
        }
        assert_eq!(app.grid.active_display_raw(), "a日本e\u{301}👍🏽🇩🇪");
        let backspace = named_event(NamedKey::Backspace);
        assert!(feed(&mut app, &mut doc, &backspace, Modifiers::default()));
        assert_eq!(app.grid.active_display_raw(), "a日本e\u{301}👍🏽");
        assert!(feed(&mut app, &mut doc, &backspace, Modifiers::default()));
        assert!(feed(&mut app, &mut doc, &backspace, Modifiers::default()));
        assert_eq!(app.grid.active_display_raw(), "a日本");

        // A committed composition replaces the selection.
        app.grid.set_edit_caret(1, false);
        app.grid.set_edit_caret(3, true);
        assert!(ime(&mut app, &mut doc, Ime::Commit("x".into())));
        assert_eq!(app.grid.active_display_raw(), "ax");
    }

    /// While a cell composes, the formula bar draws the cell editor's display
    /// text: the composition at the caret, underlined, with the caret inside
    /// it; the committed text only changes on commit.
    #[test]
    fn formula_bar_shows_the_cells_composition() {
        use winit::event::Ime;
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
        app.relayout();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        app.refresh_chrome_metrics(&mut tr, &theme);
        app.grid.select_only((0, 20)); // A21, empty
        for typed in ["a", "b"] {
            assert!(feed(
                &mut app,
                &mut doc,
                &char_event(typed),
                Modifiers::default()
            ));
        }
        app.grid.set_edit_caret(1, false);
        let ime = WindowEvent::Ime(Ime::Preedit("にほ".into(), Some((3, 3))));
        assert!(feed(&mut app, &mut doc, &ime, Modifiers::default()));

        let bar = app.grid.formula_bar();
        assert_eq!(bar.text, "aにほb", "the bar shows the composition in place");
        assert_eq!(bar.preedit, Some((1, 3)), "underlined over the composition");
        assert_eq!(bar.caret, Some(2), "the caret sits inside it");
        assert_eq!(app.grid.active_display_raw(), "ab", "nothing is typed yet");

        let mut scene = Scene::new();
        app.draw_formula_field(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        assert_eq!(
            app.formula_char_x.len(),
            "aにほb".chars().count() + 1,
            "the bar laid out the composed text"
        );
        assert!(
            app.formula_char_x[3] > app.formula_char_x[1] + 1.0,
            "the committed 'b' draws after the composition"
        );

        // A click past the composition lands on the committed text it shows.
        app.grid.set_edit_caret_shown(4, false);
        assert_eq!(app.grid.edit_caret(), Some(2), "after the 'b' of \"ab\"");
        app.grid.set_edit_caret(1, false);

        let commit = WindowEvent::Ime(Ime::Commit("日本".into()));
        assert!(feed(&mut app, &mut doc, &commit, Modifiers::default()));
        assert_eq!(app.grid.active_display_raw(), "a日本b");
        let bar = app.grid.formula_bar();
        assert_eq!(bar.text, "a日本b");
        assert_eq!((bar.preedit, bar.caret), (None, Some(3)));
    }

    /// Ctrl+K opens the palette, which captures arrows and handles go-to-cell.
    #[test]
    fn command_palette_captures_keys_and_goes_to_cells() {
        // Case `ctrl_k_opens_the_command_palette`.
        {
            let (mut app, mut doc) = seeded_app();
            assert!(!app.palette.is_open());
            let handled = feed(&mut app, &mut doc, &char_event("k"), ctrl_mods());
            assert!(handled, "Ctrl+K must be consumed");
            assert!(app.palette.is_open(), "Ctrl+K must open the palette");
            // The first action is the argument-taking "Go to cell".
            assert!(
                app.palette
                    .selected_id()
                    .is_some_and(|id| id.ends_with("go-to-cell"))
            );
        }

        // Case `open_palette_captures_arrows_instead_of_moving_the_grid`.
        {
            let (mut app, mut doc) = seeded_app();
            let start = app.grid.active(); // E2 = (4, 1)
            app.open_command_palette();
            let first = app
                .palette
                .selected_id()
                .expect("the palette has an initial highlighted entry")
                .to_string();
            assert!(first.ends_with("go-to-cell"));

            // ArrowDown is consumed by the palette and advances its highlight — the
            // grid selection behind it must NOT move.
            let handled = feed(
                &mut app,
                &mut doc,
                &named_event(winit::keyboard::NamedKey::ArrowDown),
                Modifiers::default(),
            );
            assert!(handled, "the palette must consume ArrowDown");
            assert_ne!(
                app.palette.selected_id(),
                Some(first.as_str()),
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
            assert_eq!(app.palette.selected_id(), Some(first.as_str()));
            assert_eq!(app.grid.active(), start);
        }

        // Case `palette_go_to_cell_jumps_the_selection`.
        {
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
            app.dispatch_palette_view_for_test();
            assert!(
                !app.palette.is_open(),
                "running a command closes the palette"
            );
            assert_eq!(app.grid.active(), (1, 11), "selection jumped to B12");
        }

        // Case `command_go_to_cell_reports_invalid_reference`.
        {
            let (mut app, _doc) = seeded_app();
            app.go_to_cell("not-a-ref");
            assert!(app.status.contains("not a valid"), "status: {}", app.status);
        }
    }

    /// Run the published action the palette emitted against `doc`, as the
    /// door does, and let the app follow.
    fn run_palette_action(app: &mut SheetsApp, doc: &mut AutoCommit) {
        let intent = ui::widget::controls::take_control_invocation(app)
            .expect("the palette invoked a published action");
        let resources = app.published();
        let spec = tilo_actions::door::resolve(&resources, &intent.address)
            .expect("the palette's address resolves");
        let encoded = spec
            .build_json(&intent.arguments)
            .expect("the palette's arguments build");
        let action: <Workbook as ui::tilo_resource::Resource>::Action =
            serde_json::from_slice(&encoded.payload).expect("a workbook action");
        let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
        book.apply_action(&action).expect("the action applies");
        *doc = book.doc().clone();
        app.sync(doc, &automerge::ROOT);
    }

    /// Type `text` over the palette's argument, then press Enter.
    fn answer_palette(app: &mut SheetsApp, doc: &mut AutoCommit, text: &str) {
        assert!(app.palette.in_argument_mode(), "the palette asks");
        for ch in text.chars() {
            feed(app, doc, &char_event(&ch.to_string()), Modifiers::default());
        }
        feed(
            app,
            doc,
            &named_event(winit::keyboard::NamedKey::Enter),
            Modifiers::default(),
        );
    }

    /// Editing a chart, replying to a comment and editing a comment ask in
    /// the palette for the argument of the action the chart or comment
    /// publishes, and invoke that action.
    #[test]
    fn annotation_edits_ask_for_the_published_actions() {
        let (mut app, mut doc) = seeded_app();
        let sheet = app.sheet_ids[0].clone();
        let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
        book.apply(Workbook::ADD_CHART.with(verbs::AddChart {
            sheet: sheet.clone(),
            range: "B2:C4".into(),
            kind: ChartKind::Column,
            title: "Spend".into(),
        }))
        .unwrap();
        book.apply(Workbook::COMMENT.with(verbs::Comment {
            sheet: sheet.clone(),
            range: "B2".into(),
            text: "Check this".into(),
        }))
        .unwrap();
        doc = book.doc().clone();
        app.sync(&doc, &automerge::ROOT);
        let state = doc_workbook(&doc);
        let chart = state
            .sheets
            .get(&sheet)
            .unwrap()
            .charts
            .ids()
            .next()
            .unwrap()
            .clone();
        let comment = state.comments.ids().next().unwrap().clone();

        app.apply_annotation_click(
            AnnotationClick::EditChart(chart.as_str().to_owned()),
            &mut Edits::lend(&mut doc),
        );
        assert_eq!(app.palette.argument_buffer(), Some("Spend"));
        answer_palette(&mut app, &mut doc, "Costs");
        run_palette_action(&mut app, &mut doc);
        let charted = doc_workbook(&doc);
        let edited = charted
            .sheets
            .get(&sheet)
            .unwrap()
            .charts
            .get(&chart)
            .unwrap();
        assert_eq!(edited.title.0, "Costs");
        assert_eq!(edited.kind, ChartKind::Column, "the type is kept");

        app.apply_annotation_click(
            AnnotationClick::Reply(comment.as_str().to_owned()),
            &mut Edits::lend(&mut doc),
        );
        answer_palette(&mut app, &mut doc, "Done");
        run_palette_action(&mut app, &mut doc);

        app.apply_annotation_click(
            AnnotationClick::EditComment(comment.as_str().to_owned()),
            &mut Edits::lend(&mut doc),
        );
        assert_eq!(app.palette.argument_buffer(), Some("Check this"));
        answer_palette(&mut app, &mut doc, "Checked");
        run_palette_action(&mut app, &mut doc);

        let state = doc_workbook(&doc);
        let thread = state.comments.get(&comment).unwrap();
        assert_eq!(thread.body, "Checked");
        let replies: Vec<&str> = thread
            .replies
            .values()
            .map(|reply| reply.body.as_str())
            .collect();
        assert_eq!(replies, ["Done"]);
    }

    /// A chart's card picks its type and its cells: the type picker's
    /// buttons edit the chart's `kind` field, Cells asks in the palette for
    /// the chart's published `chart-cells` edit of its `area`.
    #[test]
    fn a_chart_card_changes_its_type_and_cells() {
        let (mut app, mut doc, mut tr, theme) = measured_app_with_doc(1800.0, 1200.0, 1.0);
        let sheet = app.sheet_ids[0].clone();
        let mut book = ui::tilo_resource::Replica::<Workbook>::from_doc(doc.clone());
        book.apply(Workbook::ADD_CHART.with(verbs::AddChart {
            sheet: sheet.clone(),
            range: "B2:C4".into(),
            kind: ChartKind::Column,
            title: "Spend".into(),
        }))
        .unwrap();
        doc = book.doc().clone();
        app.sync(&doc, &automerge::ROOT);
        paint_frame(&mut app, &mut tr, &theme);
        let chart = doc_workbook(&doc)
            .sheets
            .get(&sheet)
            .unwrap()
            .charts
            .ids()
            .next()
            .unwrap()
            .clone();

        let line = app
            .annotation_clicks
            .iter()
            .find(|(_, action)| matches!(action, AnnotationClick::SetChartKind(_, ChartKind::Line)))
            .map(|(rect, _)| rect.center())
            .expect("the card offers a line chart");
        let mut handled = false;
        assert!(app.try_annotation_click(line, &mut Edits::lend(&mut doc), &mut handled));
        let kind_of = |doc: &AutoCommit| {
            doc_workbook(doc)
                .sheets
                .get(&sheet)
                .unwrap()
                .charts
                .get(&chart)
                .unwrap()
                .kind
        };
        assert_eq!(kind_of(&doc), ChartKind::Line);

        // The same picker is offered to assistive technology.
        let column = format!("sheets.annotation:chart:{}:column", chart.as_str());
        assert!(matches!(
            app.annotation_click_from_target(&column),
            Some(AnnotationClick::SetChartKind(_, ChartKind::Column))
        ));

        paint_frame(&mut app, &mut tr, &theme);
        let cells = app
            .annotation_clicks
            .iter()
            .find(|(_, action)| matches!(action, AnnotationClick::ChartCells(_)))
            .map(|(rect, _)| rect.center())
            .expect("the card offers its cells");
        assert!(app.try_annotation_click(cells, &mut Edits::lend(&mut doc), &mut handled));
        assert_eq!(app.palette.argument_buffer(), Some("B2:C4"));
        answer_palette(&mut app, &mut doc, "A1:D3");
        run_palette_action(&mut app, &mut doc);
        let state = doc_workbook(&doc);
        let sheet_state = state.sheets.get(&sheet).unwrap();
        let shown = sheet_state
            .chart_snapshots(&sheet_state.layout())
            .into_iter()
            .find(|snapshot| snapshot.id == chart.as_str())
            .unwrap();
        assert_eq!((shown.start, shown.end), ((0, 0), (3, 2)));
        assert_eq!(shown.kind, ChartKind::Line, "the type is kept");
    }

    /// An agent edits a chart's title, type and cells in one call, and a
    /// comment's text and replies on the comment itself.
    #[test]
    fn an_agent_edits_charts_and_comments_in_one_call_each() {
        let mut app = TestApp::<crate::resource::SheetsShell>::new().unwrap();
        let sheet = app.state().sheets.ids().next().unwrap().clone();
        app.invoke(Workbook::ADD_CHART.with(verbs::AddChart {
            sheet: sheet.clone(),
            range: "A1:B3".into(),
            kind: ChartKind::Column,
            title: "Spend".into(),
        }))
        .unwrap();
        app.invoke(Workbook::COMMENT.with(verbs::Comment {
            sheet: sheet.clone(),
            range: "A1".into(),
            text: "Check this".into(),
        }))
        .unwrap();
        let state = app.state();
        let chart = state
            .sheets
            .get(&sheet)
            .unwrap()
            .charts
            .ids()
            .next()
            .unwrap()
            .clone();
        let comment = state.comments.ids().next().unwrap().clone();
        let on_sheet = ui::tilo_resource::items::item_resource_id::<Workbook>("sheets", &sheet);
        let on_comment =
            ui::tilo_resource::items::item_resource_id::<Workbook>("comments", &comment);

        app.call_on(
            &on_sheet,
            "edit-chart",
            serde_json::json!({"chart": chart, "title": "Costs", "kind": "line", "range": "B2:C5"}),
        )
        .unwrap();
        assert!(
            app.call_on(
                &on_sheet,
                "edit-chart",
                serde_json::json!({"chart": chart, "title": "Costs", "range": "B2:"}),
            )
            .is_err(),
            "a range is cells"
        );
        app.call_on(
            &on_comment,
            "edit-comment",
            serde_json::json!({"body": "Checked"}),
        )
        .unwrap();
        app.call_on(&on_comment, "reply", serde_json::json!({"text": "Done"}))
            .unwrap();

        let state = app.state();
        let sheet_state = state.sheets.get(&sheet).unwrap();
        let edited = sheet_state.charts.get(&chart).unwrap();
        assert_eq!(
            (edited.title.0.as_str(), edited.kind),
            ("Costs", ChartKind::Line)
        );
        let layout = sheet_state.layout();
        let snapshot = sheet_state
            .chart_snapshots(&layout)
            .into_iter()
            .find(|snapshot| snapshot.id == chart.as_str())
            .unwrap();
        assert_eq!((snapshot.start, snapshot.end), ((1, 1), (2, 4)));
        let thread = state.comments.get(&comment).unwrap();
        assert_eq!(thread.body, "Checked");
        assert_eq!(thread.replies.len(), 1);
    }

    /// Sum-selection, row/column insertion, formula view and new-sheet commands.
    #[test]
    fn commands_edit_rows_columns_views_and_sheets() {
        // Case `command_sum_selection_writes_totals_below`.
        {
            let (mut app, mut doc) = seeded_app();
            // Budget!B2:B6 — a column of numbers.
            app.grid.select_range_public((1, 1), (1, 5));
            app.dispatch_command(&mut Edits::lend(&mut doc), "sum_selection", None);
            // A SUM total lands in B7 (the row directly below the selection).
            let raw = doc_cell_raw(&doc, 0, (1, 6));
            assert_eq!(raw, "=SUM(B2:B6)", "sum total written below the selection");
            // It computes to the real column sum.
            assert!(matches!(value_at(&app, (1, 6)), Some(CellValue::Number(_))));
        }

        // Case `command_insert_and_delete_row_shift_cells`.
        {
            let (mut app, mut doc) = seeded_app();
            // The Budget header "Category" sits at A1; its January value is at B2.
            let b2 = doc_cell_raw(&doc, 0, (1, 1));
            assert!(!b2.is_empty());
            // Insert a row above row 1 (active is E2 → row index 1).
            app.grid.select_only((0, 0)); // A1 → insert above row 0
            app.dispatch_command(&mut Edits::lend(&mut doc), "insert_row", None);
            // Everything shifted down one: the old A1 header is now at A2.
            assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "", "row 0 is now blank");
            assert_eq!(
                doc_cell_raw(&doc, 0, (0, 1)),
                "Category",
                "header shifted to A2"
            );

            // Delete that blank top row again → header returns to A1.
            app.grid.select_only((0, 0));
            app.dispatch_command(&mut Edits::lend(&mut doc), "delete_row", None);
            confirm(&mut app, &mut doc);
            assert_eq!(
                doc_cell_raw(&doc, 0, (0, 0)),
                "Category",
                "header back at A1"
            );
        }

        // Case `command_insert_column_shifts_right`.
        {
            let (mut app, mut doc) = seeded_app();
            let a1 = doc_cell_raw(&doc, 0, (0, 0));
            assert_eq!(a1, "Category");
            app.grid.select_only((0, 0)); // insert left of column A
            app.dispatch_command(&mut Edits::lend(&mut doc), "insert_col", None);
            assert_eq!(doc_cell_raw(&doc, 0, (0, 0)), "", "column A is now blank");
            assert_eq!(
                doc_cell_raw(&doc, 0, (1, 0)),
                "Category",
                "header shifted to B1"
            );
        }

        // Case `command_toggle_formula_view`.
        {
            let (mut app, mut doc) = seeded_app();
            assert!(!app.grid.formula_view());
            app.dispatch_command(&mut Edits::lend(&mut doc), "formula_view", None);
            assert!(app.grid.formula_view(), "formula view turned on");
            app.dispatch_command(&mut Edits::lend(&mut doc), "formula_view", None);
            assert!(!app.grid.formula_view(), "formula view toggled back off");
        }

        // Case `command_new_sheet_adds_a_sheet`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = doc_sheet_count(&doc);
            app.dispatch_command(&mut Edits::lend(&mut doc), "new_sheet", None);
            assert_eq!(doc_sheet_count(&doc), before + 1);
        }
    }

    /// Active cell/sheet semantics, busy loading and shared toolbar/tab controls.
    #[test]
    fn accessibility_exposes_cells_busy_state_and_live_controls() {
        // Case `accessibility_exposes_active_cell_and_selected_sheet`.
        {
            use ui::widget::Widget as _;

            let (mut app, _doc) = seeded_app();
            app.grid.select_only((1, 1));
            let tree = app.accessibility().expect("spreadsheet semantics");
            assert_eq!(tree.role, ui::a11y::Role::Document);
            let toolbar = &tree.children[0];
            assert_eq!(toolbar.role, ui::a11y::Role::Toolbar);
            assert!(toolbar.children.iter().any(|control| {
                control.name.as_deref() == Some("Name box") && control.action_target.is_some()
            }));
            let tabs = &tree.children[1];
            assert_eq!(tabs.role, ui::a11y::Role::TabBar);
            assert!(tabs.children.iter().any(|tab| tab.state.selected));
            assert!(tabs.children.iter().all(|tab| tab.action_target.is_some()));
            let cell = &tree.children[2].children[0].children[0];
            assert_eq!(cell.name.as_deref(), Some("Column A"));
            let visible_row = tree.children[2]
                .children
                .iter()
                .find(|row| row.name.as_deref() == Some("Row 2"))
                .expect("visible second worksheet row");
            let cell = visible_row
                .children
                .iter()
                .find(|cell| cell.name.as_deref() == Some("B2"))
                .expect("active cell in complete visible table semantics");
            assert!(cell.state.selected && cell.state.focused);
            assert!(cell.bounds.is_some());
        }

        // Case `semantic_toolbar_and_tabs_use_the_same_live_controls`.
        {
            use ui::widget::Widget as _;

            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.relayout();
            let root = automerge::ROOT;
            assert!(app.perform_semantic_action(
                &format!("toolbar.item.{}", chrome::bar::FORMULAS),
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut doc),
                &root,
            ));
            assert!(app.grid.formula_view());
            assert!(app.perform_semantic_action(
                "sheets.name-box",
                &ui::a11y::Action::Focus,
                &mut Edits::lend(&mut doc),
                &root,
            ));
            assert!(app.name_box_edit.is_some());
            let target = format!("sheets.tab:{}", app.sheet_ids[1].as_str());
            assert!(app.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut doc),
                &root,
            ));
            assert_eq!(app.active_sheet, 1);
        }

        // Case `semantic_toolbar_controls_are_inventoried_action_bindings`.
        {
            use ui::widget::controls::ActionControls as _;

            let (app, _tr, _theme) = measured_app(1280.0, 720.0, 1.0);
            let bound: std::collections::HashSet<_> = app
                .control_action_bindings()
                .into_iter()
                .map(|binding| binding.control_id)
                .collect();
            let bold = format!("toolbar.item.{}", chrome::bar::BOLD);
            let formulas = format!("toolbar.item.{}", chrome::bar::FORMULAS);
            for target in [
                bold.as_str(),
                formulas.as_str(),
                "sheets.fx",
                "sheets.name-box",
                "sheets.formula-bar",
                "sheets.new-sheet",
            ] {
                assert!(
                    bound.contains(target),
                    "{target} has no action binding: {bound:?}"
                );
            }
        }
    }
}

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
