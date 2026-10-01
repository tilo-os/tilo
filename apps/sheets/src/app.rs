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
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use tilo_automerge::ApplyIncoming;
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};

use ui::ColorCompat;
use ui::chart_palette::ChartPalette;
use ui::collaboration::PresencePayload;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::command_palette::{CommandEntry, CommandPalette};
use ui::widget::{EditOutcome, EditableLabel, TextInput, Widget};
use uuid::Uuid;

use crate::actions::{self, SheetsAction, SheetsResourceCtx};
use crate::doc::{ChartKind, ChartSnapshot, CommentThread, SheetsDoc};
use crate::fill;
use crate::formula;
use crate::grid::{Grid, GridCmd, GridMetrics};
use crate::infer::infer_entry;
use crate::model::{
    Addr, CellFormat, CellValue, Currency, DateStyle, FormatCategory, TimeStyle, a1_to_addr,
    format_value,
};
use crate::settings::{DocSettings, EffectiveLocale, GlobalSettings};

/// 1× floors for the three chrome bands. Each band is derived from the
/// measured line box of the text it carries plus spacing tokens (see
/// [`ChromeMetrics`]); these floors reproduce the classic density exactly at
/// 1× and only give way when larger text needs the room.
const MENUBAR_H: f64 = 44.0;
const MENU_BUTTON_H: f64 = 32.0;
const FORMULA_FIELD_H: f64 = 32.0;
const TAB_H: f64 = 32.0;
const SETTING_CHIP_H: f64 = 26.0;
const MENU_ROW_H: f64 = 30.0;
const NAME_BOX_W: f64 = 92.0;
/// Deliberately off-scale: the sheet tab's colour underline sits 2 px above
/// the window edge so it reads as part of the tab, not the frame.
const TAB_STRIP_BOTTOM_INSET: f64 = 2.0;
/// Thickness of a sheet tab's colour / active underline.
const TAB_UNDERLINE_H: f64 = 3.0;
/// Menu rows leave an `s1` seam so the lifted fill of adjacent rows never
/// merges into one block — a token step, never a touch (HIG §7).
const MENU_ROW_SEAM: f64 = ui::tokens::Spacing::BASE.s1 as f64;
/// The name box's inline editor sits 2 px inside the box's top and bottom
/// edges: an authored inset for the shared `TextInput`, not a gap between
/// two elements.
const NAME_BOX_EDITOR_INSET: f64 = 2.0;
/// Colour swatches in the sheet-tab menu are square targets of this side.
const TAB_COLOUR_SWATCH: f64 = 20.0;
/// Classic 1× bounds of a sheet tab (touch-target floor, long-name ceiling);
/// both grow with the measured text.
const TAB_MIN_W: f64 = 64.0;
const TAB_MAX_W: f64 = 180.0;
/// The inline tab-rename editor gets at least this much room (grown with
/// the text) so a longer name fits while typing.
const TAB_RENAME_MIN_W: f64 = 150.0;
/// Minimum widths of the two menu panels; both grow to their measured labels.
const FILE_MENU_MIN_W: f64 = 196.0;
const FORMAT_MENU_MIN_W: f64 = 160.0;
const TAB_MENU_MIN_W: f64 = 188.0;
/// The document-settings panel's preferred width, and the floor it shrinks to
/// on a narrow viewport (chip rows wrap inside it).
const SETTINGS_PANEL_W: f64 = 440.0;
/// How far the document-settings dialog dims the sheet behind it
/// (`ui::chrome::fill_scrim` strength).
const SETTINGS_SCRIM_STRENGTH: f32 = 0.45;
const SETTINGS_PANEL_MIN_W: f64 = 300.0;
/// Reference label the name box is sized for, so it never resizes with the
/// selection and always fits a two-corner range reference.
const NAME_BOX_REFERENCE: &str = "ZZ999:ZZ999";
/// The menu bar's buttons in order: label, the dropdown it opens, the action
/// it fires. One table feeds layout, paint and the label pre-measurement so a
/// button is always exactly as wide as the text it paints.
const MENU_BAR_BUTTONS: [(&str, Option<OpenMenu>, Option<MenuItem>); 4] = [
    ("File", Some(OpenMenu::File), None),
    ("Format", Some(OpenMenu::Format), None),
    ("\u{03A3} Sum", None, Some(MenuItem::AutoSum)),
    ("Doc settings", None, Some(MenuItem::OpenDocSettings)),
];
/// Rows of the sheet-tab context menu, top to bottom.
const TAB_MENU_ACTIONS: [TabAction; 6] = [
    TabAction::Rename,
    TabAction::Duplicate,
    TabAction::InsertRight,
    TabAction::MoveLeft,
    TabAction::MoveRight,
    TabAction::Delete,
];
/// Option chips of the document-settings panel, in display order.
const DATE_STYLES: [(&str, DateStyle); 4] = [
    ("ISO", DateStyle::Iso),
    ("US", DateStyle::Us),
    ("EU", DateStyle::Eu),
    ("Long", DateStyle::Long),
];
const TIME_STYLES: [(&str, TimeStyle); 2] =
    [("24-hour", TimeStyle::H24), ("12-hour", TimeStyle::H12)];
const SETTINGS_SECTIONS: [&str; 3] = ["Currency", "Date", "Time"];
/// Every painted chart/comment command gets a normal touch target, not a
/// text-baseline-sized link. Kept as a shared constant so the renderer and
/// regression test cannot silently drift apart.
const ANNOTATION_ACTION_H: f64 = 44.0;
/// Classic 1× offsets inside the floating chart / comment cards. They are
/// the floors of [`ChromeMetrics::chart_card_rows`] /
/// [`ChromeMetrics::comment_card_rows`]: a 1× card keeps this rhythm exactly,
/// a larger text scale grows each row from its measured line box instead.
const CHART_TITLE_TOP: f64 = 21.0;
/// Least distance from the chart title's top to its action band (1×).
const CHART_CONTROLS_SEAM: f64 = 5.0;
/// The chart plot is content-sized, not text-sized.
const CHART_PLOT_H: f64 = 74.0;
const COMMENT_HEADING_TOP: f64 = 22.0;
const COMMENT_BODY_ADV: f64 = 26.0;
const COMMENT_DETAIL_ADV: f64 = 36.0;
const COMMENT_CONTROLS_ADV: f64 = 28.0;
const COMMENT_BOTTOM_PAD: f64 = 22.0;
/// Importing is deliberately bounded before a parsed workbook can become a
/// durable action stream.  This is large enough for useful real-world sheets,
/// while keeping one user-selected file from monopolising a collaborator's
/// document, history, or search projection.
const MAX_IMPORT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMPORT_CELLS: usize = actions::MAX_IMPORTED_CELLS;
/// CSV's interchange representation is rectangular.  A file with just one
/// populated cell at a hostile far-away coordinate must not turn into a
/// trillion-slot dense `Vec`; this caps the actually materialised rectangle,
/// independently of the populated-cell action limit above.
const MAX_CSV_MATERIALIZED_SLOTS: usize = crate::xlsx::MAX_MATERIALIZED_SLOTS;

type ImportedCellWrite = (Addr, String, Option<CellFormat>);
type ImportedWorkbook = Vec<(String, Vec<ImportedCellWrite>)>;
type LoadedWorkbook = Box<(AutoCommit, Vec<ChangeHash>)>;

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

/// A single accepted interchange job.  The UI owns CRDT mutation and status;
/// this worker owns only hostile file parsing/encoding and filesystem I/O.
/// Keeping its result channel capacity at one gives a hard backpressure bound
/// and lets the generation tag reject stale completions exactly.
enum InterchangeResult {
    Import {
        generation: u64,
        format: Interchange,
        path: PathBuf,
        result: Result<Vec<crate::xlsx::SheetTable>, String>,
    },
    Export {
        generation: u64,
        format: Interchange,
        destination: PathBuf,
        /// The worker has exclusively created and encoded this file in the
        /// destination directory. Only the UI receipt may publish it.
        result: Result<PathBuf, String>,
    },
}

/// A finished resource operation held at the UI boundary until lifecycle
/// polling confirms that the accepted invocation remains current. In
/// particular, a cancelled import never reaches the CRDT and a cancelled
/// export never reaches the selected destination.
enum PreparedResourceIo {
    Import {
        format: Interchange,
        path: PathBuf,
        tables: Vec<crate::xlsx::SheetTable>,
    },
    Export {
        format: Interchange,
        destination: PathBuf,
        temporary: PathBuf,
    },
}

/// The interchange worker's one reply. Sending, returning early or panicking
/// all wake the owning host after the channel settles: the worker is the only
/// source of progress for an accepted job, and an idle host has no frame that
/// would otherwise notice its result. A reply nobody can receive any more
/// (Sheets closed) removes its prepared export instead of leaking it.
struct InterchangeReply {
    tx: Option<SyncSender<InterchangeResult>>,
    wake: ui::wake::WakeHandle,
}

impl InterchangeReply {
    fn send(mut self, result: InterchangeResult) {
        let Some(tx) = self.tx.take() else { return };
        if let Err(mpsc::SendError(InterchangeResult::Export {
            result: Ok(temporary),
            ..
        })) = tx.send(result)
        {
            SheetsApp::discard_export_temp(&temporary);
        }
    }
}

impl Drop for InterchangeReply {
    fn drop(&mut self) {
        drop(self.tx.take());
        self.wake.wake();
    }
}

/// A bounded persistence job.  It deliberately carries an Automerge fork, not
/// bytes: serialising a large CRDT is the expensive part and belongs on the
/// worker, while merging a completed load remains a short UI-thread CRDT
/// receipt.
enum PersistenceResult {
    Load {
        generation: u64,
        /// The heads actually represented by the on-disk image. The UI must
        /// not acknowledge unrelated collaboration that arrived while loading.
        result: Result<Option<LoadedWorkbook>, String>,
    },
    Save {
        generation: u64,
        heads: Vec<ChangeHash>,
        result: Result<(), String>,
    },
}

#[derive(Clone)]
enum PersistenceJob {
    Load,
    Save { heads: Vec<ChangeHash> },
}

/// Failed saves are visible and remain dirty, but a permanently unavailable
/// data directory must not start a thread every frame.
const PERSIST_RETRY_DELAY: Duration = Duration::from_secs(1);

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
    /// A view-only operation: display raw formulas in the grid instead of
    /// their calculated values. Kept alongside the other painted commands so
    /// narrow layouts do not hide this useful inspection mode behind Ctrl+K.
    ToggleFormulaView,
    // Inline bar action.
    AutoSum,
}

/// Which dropdown (if any) is open.
#[derive(Clone, Copy, PartialEq)]
enum OpenMenu {
    None,
    File,
    Format,
    /// Holds secondary commands when the accessible text scale or viewport
    /// leaves insufficient room for the full toolbar.
    More,
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
    /// Mounted by a host on a record: the record is the workbook, so the
    /// app-wide file is never read or written. Only the record's creator
    /// adds the first sheet; a participant that joined waits for it.
    Record(ui::launch::RecordLaunch),
}

/// The app's only decorative animation is the transient status bubble. Keep
/// the preference as a semantic state rather than another independent bool so
/// it cannot accidentally conflict with the run mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MotionPreference {
    Full,
    Reduced,
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

    /// Whether an empty workbook gets its first blank sheet.
    fn seeds_blank_sheet(self) -> bool {
        self != Self::Record(ui::launch::RecordLaunch::Joined)
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingReveal {
    Sheet { sheet_id: String },
    Cell { sheet_id: String, addr: Addr },
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

/// Chrome text metrics resolved once per frame from the renderer that paints
/// the chrome.
///
/// Theme typography tokens are already accessibility-scaled and the renderer
/// scales its input again, so each token is converted through
/// [`TextRenderer::renderer_input_style`] exactly once here. Every band
/// height, control height and label slot in `relayout` derives from these
/// measured line boxes and label widths plus spacing tokens, so layout, paint
/// and hit-testing share one metric. `fallback()` stands in until the first
/// draw; `SheetsApp::refresh_chrome_metrics` swaps it for the measured set and
/// re-runs `relayout` in the same frame, so nothing is ever painted from stale
/// geometry.
#[derive(Clone, Debug, PartialEq)]
struct ChromeMetrics {
    /// Renderer text scale the line boxes and widths were measured under.
    scale: f64,
    /// Renderer-input sizes (theme token ÷ renderer scale).
    xs: f32,
    sm: f32,
    base: f32,
    lg: f32,
    /// Measured line boxes of the sizes above, in final pixels.
    lh_xs: f64,
    lh_sm: f64,
    lh_base: f64,
    /// Measured advance of the italic "fx" glyph at `base`.
    fx_w: f64,
    /// Measured width of [`NAME_BOX_REFERENCE`] at `sm` semibold.
    name_box_label_w: f64,
    /// Measured size of the add-sheet "+" glyph at `lg`.
    add_glyph: (f64, f64),
    /// Measured `sm` semibold widths of every chrome label laid out by
    /// `relayout` (menu buttons, sheet tabs, setting chips, menu rows).
    labels: HashMap<String, f64>,
    /// Theme spacing tokens.
    s1: f64,
    s2: f64,
    s3: f64,
    s4: f64,
    s6: f64,
    /// The grid's share of the same resolution.
    grid: GridMetrics,
}

impl ChromeMetrics {
    /// Resolve every chrome metric for `theme` against `tr`. `labels` is the
    /// set of strings `relayout` will size a slot for.
    fn resolve<'a>(
        tr: &mut TextRenderer,
        theme: &Theme,
        labels: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let scale = &theme.tokens.typography.scale;
        let input = |size: f32| tr.renderer_input_style(&TextStyle::new(size));
        let xs = input(scale.xs);
        let sm = input(scale.sm);
        let base = input(scale.base);
        let lg = input(scale.lg);
        let sm_semibold = sm.clone().with_weight(TextWeight::Semibold);
        let (_, lh_xs) = tr.measure_style("Ag", None, &xs);
        let (_, lh_sm) = tr.measure_style("Ag", None, &sm);
        let (_, lh_base) = tr.measure_style("Ag", None, &base);
        let (fx_w, _) = tr.measure_style("fx", None, &base.clone().with_italic(true));
        let (name_box_label_w, _) = tr.measure_style(NAME_BOX_REFERENCE, None, &sm_semibold);
        let (add_w, add_h) = tr.measure_style("+", None, &lg);
        let labels = labels
            .into_iter()
            .map(|label| {
                let (w, _) = tr.measure_style(label, None, &sm_semibold);
                (label.to_owned(), f64::from(w))
            })
            .collect();
        let sp = &theme.tokens.spacing;
        Self {
            scale: f64::from(tr.text_scale()),
            xs: xs.size,
            sm: sm.size,
            base: base.size,
            lg: lg.size,
            lh_xs: f64::from(lh_xs),
            lh_sm: f64::from(lh_sm),
            lh_base: f64::from(lh_base),
            fx_w: f64::from(fx_w),
            name_box_label_w: f64::from(name_box_label_w),
            add_glyph: (f64::from(add_w), f64::from(add_h)),
            labels,
            s1: f64::from(sp.s1),
            s2: f64::from(sp.s2),
            s3: f64::from(sp.s3),
            s4: f64::from(sp.s4),
            s6: f64::from(sp.s6),
            grid: GridMetrics::resolve(tr, theme),
        }
    }

    /// The 1× metrics used before the first draw has a renderer. Every band
    /// lands on its classic floor and label slots fall back to an estimate.
    fn fallback() -> Self {
        Self {
            scale: 1.0,
            xs: 10.0,
            sm: 12.0,
            base: 14.0,
            lg: 16.0,
            lh_xs: 12.5,
            lh_sm: 15.0,
            lh_base: 17.5,
            fx_w: 13.0,
            name_box_label_w: 70.0,
            add_glyph: (10.0, 20.0),
            labels: HashMap::new(),
            s1: 4.0,
            s2: 8.0,
            s3: 12.0,
            s4: 16.0,
            s6: 24.0,
            grid: GridMetrics::fallback(),
        }
    }

    /// Measured `sm` width of `label` in final pixels, or a per-character
    /// estimate (scaled like the renderer would) until the first draw has
    /// measured it.
    fn label_w(&self, label: &str) -> f64 {
        self.labels.get(label).copied().unwrap_or_else(|| {
            count_f64(label.chars().count()) * f64::from(self.sm) * 0.6 * self.scale
        })
    }

    /// How much the chrome text has grown past the 1× line box; scales the
    /// classic pixel floors and ceilings that bound text-driven slots.
    fn text_growth(&self) -> f64 {
        (self.lh_sm / Self::fallback().lh_sm).max(1.0)
    }

    /// A menu-bar button: its label plus `s3` each side.
    fn menu_button_w(&self, label: &str) -> f64 {
        self.label_w(label) + 2.0 * self.s3
    }

    /// Menu-bar buttons: the `sm` line box plus `s3`, never below the
    /// text-scaled fine-pointer target.
    fn menu_button_h(&self) -> f64 {
        (self.lh_sm + self.s3).max(MENU_BUTTON_H * self.scale)
    }

    /// Menu bar: a button plus `s3`, never below 44 px.
    fn menubar_h(&self) -> f64 {
        (self.menu_button_h() + self.s3).max(MENUBAR_H)
    }

    /// Name box + formula field: the `base` line box plus `s2`, never below
    /// the text-scaled fine-pointer target.
    fn formula_field_h(&self) -> f64 {
        (self.lh_base + self.s2).max(FORMULA_FIELD_H * self.scale)
    }

    /// Formula bar: the field plus `s1` above and below.
    fn formula_h(&self) -> f64 {
        self.formula_field_h() + 2.0 * self.s1
    }

    /// Name box: wide enough for [`NAME_BOX_REFERENCE`] plus `s2` each side,
    /// never below the classic 92 px.
    fn name_box_w(&self) -> f64 {
        (self.name_box_label_w + 2.0 * self.s2).max(NAME_BOX_W)
    }

    /// The "fx" affordance slot: the glyph plus `s2` each side.
    fn fx_slot_w(&self) -> f64 {
        self.fx_w + 2.0 * self.s2
    }

    /// Sheet tabs: the `sm` line box plus `s3`, never below the text-scaled
    /// fine-pointer target.
    fn tab_h(&self) -> f64 {
        (self.lh_sm + self.s3).max(TAB_H * self.scale)
    }

    /// Sheet-tab strip: a tab plus `s1` above and the underline inset below.
    fn tabs_h(&self) -> f64 {
        self.tab_h() + self.s1 + TAB_STRIP_BOTTOM_INSET
    }

    /// A sheet tab: its name plus `s3` each side, bounded between the classic
    /// touch-target floor and ceiling (both grown with the text) so one long
    /// name can never push the "+" button and later tabs off-screen; the
    /// painter elides a name that hits the ceiling.
    fn tab_w(&self, name: &str) -> f64 {
        let g = self.text_growth();
        (self.label_w(name) + 2.0 * self.s3).clamp(TAB_MIN_W * g, TAB_MAX_W * g)
    }

    /// Setting chips: the `sm` line box plus `s2`, never below 26 px.
    fn chip_h(&self) -> f64 {
        (self.lh_sm + self.s2).max(SETTING_CHIP_H)
    }

    /// A setting chip: its label plus `s3` each side.
    fn chip_w(&self, label: &str) -> f64 {
        self.label_w(label) + 2.0 * self.s3
    }

    /// Menu rows: the `sm` line box plus `s3`, never below 30 px.
    fn menu_row_h(&self) -> f64 {
        (self.lh_sm + self.s3).max(MENU_ROW_H)
    }

    /// Top of a line box centred in `rect`.
    fn centred_top(rect: Rect, line_h: f64) -> f64 {
        rect.y0 + (rect.height() - line_h) / 2.0
    }

    /// Top of the action band that follows an `sm` title line at
    /// `title_top`: the band's centred `xs` label clears the title line box by
    /// `s1`, and the band never starts closer than the classic seam.
    fn annotation_controls_top(&self, title_top: f64) -> f64 {
        let label_top = title_top + self.lh_sm + self.s1;
        (label_top - (ANNOTATION_ACTION_H - self.lh_xs) / 2.0).max(title_top + CHART_CONTROLS_SEAM)
    }

    /// Row rhythm of a floating chart card, relative to the card's top:
    /// title line, action band, plot, bottom `s4`. At 1× this reproduces the
    /// classic 176 px card; a larger text scale pushes the band and plot down
    /// so the title never paints under the actions.
    fn chart_card_rows(&self) -> ChartCardRows {
        let title_top = CHART_TITLE_TOP;
        let controls_y0 = self.annotation_controls_top(title_top);
        let controls_y1 = controls_y0 + ANNOTATION_ACTION_H;
        let plot_y0 = controls_y1 + self.s4;
        let plot_y1 = plot_y0 + CHART_PLOT_H;
        ChartCardRows {
            title_top,
            controls_y0,
            controls_y1,
            plot_y0,
            plot_y1,
            height: plot_y1 + self.s4,
        }
    }

    /// Row rhythm of the floating comment card, relative to the card's top:
    /// heading, body, reply/resolve detail line, action band. Each advance is
    /// the measured line box plus a spacing token, floored at the classic 1×
    /// value; the resolve/reopen target is a full action-height band centred
    /// on its detail line that ends where the action band begins.
    fn comment_card_rows(&self) -> CommentCardRows {
        let heading_top = COMMENT_HEADING_TOP;
        let body_top = heading_top + (self.lh_sm + self.s2).max(COMMENT_BODY_ADV);
        let detail_top = body_top + (self.lh_sm + self.s4).max(COMMENT_DETAIL_ADV);
        let controls_y0 = detail_top + (self.lh_xs + self.s3).max(COMMENT_CONTROLS_ADV);
        let toggle_y1 =
            (detail_top + self.lh_xs / 2.0 + ANNOTATION_ACTION_H / 2.0).min(controls_y0);
        CommentCardRows {
            heading_top,
            body_top,
            detail_top,
            controls_y0,
            toggle_y0: toggle_y1 - ANNOTATION_ACTION_H,
            toggle_y1,
            height: controls_y0 + ANNOTATION_ACTION_H + COMMENT_BOTTOM_PAD,
        }
    }
}

/// See [`ChromeMetrics::chart_card_rows`]. Offsets are relative to the card top.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ChartCardRows {
    title_top: f64,
    controls_y0: f64,
    controls_y1: f64,
    plot_y0: f64,
    plot_y1: f64,
    height: f64,
}

/// See [`ChromeMetrics::comment_card_rows`]. Offsets are relative to the card top.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CommentCardRows {
    heading_top: f64,
    body_top: f64,
    detail_top: f64,
    controls_y0: f64,
    toggle_y0: f64,
    toggle_y1: f64,
    height: f64,
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
    DeleteChart(String),
    Reply(String),
    EditComment(String),
    ToggleComment(String, bool),
    DeleteComment(String),
}

/// An in-progress inline rename of a sheet tab.
struct TabRename {
    idx: usize,
    rect: Rect,
    editor: EditableLabel,
}

/// How many theme-derived tab colours the picker offers. A sheet stores a
/// slot index, not an RGB, so the underline follows the live theme.
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
    reason = "the loading shot flag joins existing independent retained interaction, worker, and presentation flags; these are not mutually exclusive states"
)]
pub struct SheetsApp {
    grid: Grid,
    /// The custom chrome has no toolkit layout primitives to inherit type
    /// sizing from, so keep its touch targets and modal spacing aligned with
    /// the current accessibility text preference.
    text_scale: f64,
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
    /// Measured chrome text metrics driving `relayout` (see [`ChromeMetrics`]).
    metrics: ChromeMetrics,
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
    /// Durable chart/comment records for the active sheet; refreshed together
    /// with its formula snapshot, never created as an app-local side store.
    sheet_charts: Vec<ChartSnapshot>,
    sheet_comments: Vec<CommentThread>,
    /// Rebuilt on paint: annotation cards stay a visual projection of the
    /// shared records, while their click routes dispatch canonical actions.
    annotation_clicks: Vec<(Rect, AnnotationClick)>,
    /// Query-ready workbook projection, rebuilt only when document heads
    /// change so global search never reads the CRDT synchronously.
    searchable_sheets: Vec<actions::SearchableSheet>,
    /// The heads that produced `searchable_sheets`. This is intentionally
    /// separate from `cache`: changing the active tab must not walk every
    /// workbook body or rebuild global-search postings.
    search_projection_heads: Option<Vec<ChangeHash>>,
    /// Retained, Arc-backed catalogue handed to each Ctrl+K session by clone.
    /// It is refreshed only with a new search projection, never per query.
    search_provider: crate::search_provider::SheetSearchProvider,
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
    /// Snapshot-only counterpart to the real startup loading state. This lets
    /// headless visual regression cover the loading affordance after the shot
    /// harness has seeded its deterministic sample workbook.
    shots_loading: bool,
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
    motion: MotionPreference,
    /// Document heads last written to disk — persistence is skipped while the
    /// document is unchanged, so idle frames never re-serialize.
    saved_heads: Vec<ChangeHash>,
    /// A single capacity-one worker covers both startup loading and coalesced
    /// saves. A later edit while a save is running stays dirty and schedules
    /// one follow-up write after the exact saved heads are acknowledged.
    persistence_rx: Option<Receiver<PersistenceResult>>,
    persistence_generation: u64,
    active_persistence: Option<(u64, PersistenceJob)>,
    persistence_started: bool,
    load_start_heads: Option<Vec<ChangeHash>>,
    last_persist_failure: Option<Instant>,
    /// Memoised recalculation: `reload` recomputes only when the document
    /// heads or active sheet change, so selection/scroll frames are cheap.
    cache: Option<ReloadCache>,
    /// An in-flight interchange file dialog: the operation and the channel its
    /// chosen path arrives on, drained once per frame in [`Self::drain_io`].
    /// `None` when no import/export chooser is open.
    io: Option<(PendingIo, PortalChannel)>,
    /// One bounded file worker result slot.  A fresh channel is made only
    /// after the previous job settles, so accepted jobs cannot be displaced.
    interchange_rx: Option<Receiver<InterchangeResult>>,
    /// Result channels of cancelled jobs whose workers may still be running.
    /// They no longer hold the slot; each is reaped once its worker replies.
    orphaned_interchange: Vec<Receiver<InterchangeResult>>,
    interchange_generation: u64,
    active_interchange_generation: Option<u64>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    pending_resource_io: Option<(tilo_actions::InvocationId, actions::SheetsOperation)>,
    prepared_resource_io: Option<PreparedResourceIo>,
    /// Set by a non-cancelled lifecycle poll; the following mutable `sync`
    /// receipt may then publish/import the prepared result.
    resource_confirmation: Option<tilo_actions::InvocationId>,
    /// The host that accepted the current job; woken whenever that job can
    /// advance only through another `sync`.
    interchange_wake: ui::wake::WakeHandle,
    pending_reveal: Option<PendingReveal>,
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
    actions: crate::resource::ActionSink,
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
            text_scale: 1.0,
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
            metrics: ChromeMetrics::fallback(),
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
            sheet_charts: Vec::new(),
            sheet_comments: Vec::new(),
            annotation_clicks: Vec::new(),
            searchable_sheets: Vec::new(),
            search_projection_heads: None,
            search_provider: crate::search_provider::SheetSearchProvider::new(&[]),
            active_sheet: 0,
            status: "Ready".into(),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: String::new(),
            shots_status: false,
            shots_loading: false,
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
            run: ui::launch::record_launch().map_or(RunMode::Live, RunMode::Record),
            motion: MotionPreference::Full,
            saved_heads: Vec::new(),
            persistence_rx: None,
            persistence_generation: 0,
            active_persistence: None,
            persistence_started: false,
            load_start_heads: None,
            last_persist_failure: None,
            cache: None,
            io: None,
            interchange_rx: None,
            orphaned_interchange: Vec::new(),
            interchange_generation: 0,
            active_interchange_generation: None,
            resource_invocation: None,
            resource_result: None,
            pending_resource_io: None,
            prepared_resource_io: None,
            resource_confirmation: None,
            interchange_wake: ui::wake::WakeHandle::detached(),
            pending_reveal: None,
            clipboard: None,
            palette: CommandPalette::commands(),
            actions: crate::resource::ActionSink::default(),
        }
    }

    /// Take part in the framework: submit workbook edits through its dispatcher.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::SheetsWorkbook>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Show the sheet names the framework resource holds.
    /// An empty projection does not wipe tabs that have not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::SheetsState) {
        if state.sheets.is_empty() && !self.sheet_names.is_empty() {
            return;
        }
        if !state.sheets.is_empty() {
            self.sheet_names = state
                .sheets
                .iter()
                .map(|sheet| sheet.name.clone())
                .collect();
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
        // Interactive affordances retain the framework's non-panicking
        // contract.  Import uses the fallible helper below so it can surface a
        // refusal instead of falsely reporting completion.
        let _ = self.try_perform_action(doc, action);
    }

    /// The fallible counterpart used by external import.  An import must never
    /// claim success after a sheet or chunk was refused: every durable cell is
    /// committed through this same executor as keyboard edits and palette
    /// commands, or the whole operation reports its exact failure.
    fn try_perform_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &SheetsAction,
    ) -> Result<(), tilo_actions::ActionError> {
        let context = actions::history_context_for_doc(doc, action);
        self.actions.try_dispatch_with_context(doc, action, context)
    }

    /// The presentation context the Ctrl+K resource tree is built from: the open
    /// sheet names. Everything here mirrors state the document already holds.
    pub(crate) fn sheets_resource_ctx(&self) -> SheetsResourceCtx {
        let palette_actions = self.native_palette_actions();
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let label = if (c0, r0) == (c1, r1) {
            crate::model::addr_to_a1(c0, r0)
        } else {
            format!(
                "{}:{}",
                crate::model::addr_to_a1(c0, r0),
                crate::model::addr_to_a1(c1, r1)
            )
        };
        let selection =
            self.searchable_sheets
                .get(self.active_sheet)
                .map(|sheet| actions::RangeResourceCtx {
                    sheet_id: sheet.id.clone(),
                    label,
                    // Navigation and formula visibility are workbook views, not
                    // range mutations. Keeping those two on the workbook avoids
                    // duplicate prompted Ctrl+K entries while every selection
                    // operation (sum/fill/format/structure) remains addressable
                    // through this range resource.
                    actions: palette_actions
                        .iter()
                        .filter(|action| {
                            action.id != "go-to-cell" && action.id != "toggle-formula-view"
                        })
                        .cloned()
                        .collect(),
                });
        SheetsResourceCtx {
            sheet_names: self.sheet_names.clone(),
            searchable_sheets: self.searchable_sheets.clone(),
            palette_actions,
            selection,
        }
    }

    /// Build the context-sensitive command surface once, as resource actions.
    /// Dense spreadsheet commands can retain their custom UI while still
    /// sharing byte-identical action values with agents and the search bar.
    #[allow(
        clippy::too_many_lines,
        clippy::expect_used,
        reason = "the palette builds serializable action enum values in one authoritative surface"
    )]
    fn native_palette_actions(&self) -> Vec<tilo_actions::ActionSpec> {
        use tilo_actions::{ActionSpec, OperationKind};

        let immediate = |id: &str, label: &str, action: SheetsAction| {
            ActionSpec::immediate(id, label, actions::SHEETS_ACTION_TYPE, &action)
                .expect("SheetsAction is serializable")
                .reversible()
        };
        let sheet = self.active_sheet;
        let sheet_id = self.sheet_identity(sheet);
        let ((c0, r0), (c1, r1)) = self.grid.selection_bounds();
        let target_r = r1.saturating_add(1);
        let sum_writes: Vec<(Addr, String, Option<CellFormat>)> = (c0..=c1)
            .map(|c| {
                (
                    (c, target_r),
                    format!(
                        "=SUM({}:{})",
                        crate::model::addr_to_a1(c, r0),
                        crate::model::addr_to_a1(c, r1)
                    ),
                    None,
                )
            })
            .collect();
        let mut specs = vec![
            immediate(
                "insert-column-chart",
                "Create column chart from selection",
                actions::put_chart_action(
                    sheet,
                    sheet_id.clone(),
                    "Selection chart",
                    ChartKind::Column,
                    (c0, r0),
                    (c1, r1),
                ),
            ),
            immediate(
                "insert-line-chart",
                "Create line chart from selection",
                actions::put_chart_action(
                    sheet,
                    sheet_id.clone(),
                    "Selection chart",
                    ChartKind::Line,
                    (c0, r0),
                    (c1, r1),
                ),
            ),
            immediate(
                "comment-selection",
                "Comment on selection",
                actions::put_comment_action(
                    sheet,
                    sheet_id.clone(),
                    (c0, r0),
                    (c1, r1),
                    "You",
                    "New comment",
                ),
            ),
            immediate(
                "sum-selection",
                "Sum selection",
                actions::set_cells_action(sheet, sheet_id.clone(), sum_writes),
            ),
            immediate(
                "insert-row-active",
                "Insert row above",
                actions::insert_row_action(sheet, sheet_id.clone(), self.grid.active().1),
            ),
            immediate(
                "delete-row-active",
                "Delete row",
                actions::delete_row_action(sheet, sheet_id.clone(), self.grid.active().1),
            )
            .destructive(),
            immediate(
                "insert-column-active",
                "Insert column left",
                actions::insert_column_action(sheet, sheet_id.clone(), self.grid.active().0),
            ),
            immediate(
                "delete-column-active",
                "Delete column",
                actions::delete_column_action(sheet, sheet_id.clone(), self.grid.active().0),
            )
            .destructive(),
        ];

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
            let cells: Vec<(Addr, CellFormat)> = selected
                .iter()
                .copied()
                .map(|addr| (addr, format))
                .collect();
            specs.push(immediate(
                id,
                label,
                actions::set_formats_action(sheet, sheet_id.clone(), cells),
            ));
        }

        let mut down = Vec::new();
        if r1 > r0 {
            for c in c0..=c1 {
                let raw = self.grid.cell_raw_at((c, r0));
                let fmt = self.grid.cell_fmt_at((c, r0));
                for (offset, r) in (1_i64..).zip(r0.saturating_add(1)..=r1) {
                    down.push(((c, r), formula::offset_refs(&raw, 0, offset), Some(fmt)));
                }
            }
        }
        specs.push(immediate(
            "fill-down",
            "Fill down",
            actions::set_cells_action(sheet, sheet_id.clone(), down),
        ));

        let mut right = Vec::new();
        if c1 > c0 {
            for r in r0..=r1 {
                let raw = self.grid.cell_raw_at((c0, r));
                let fmt = self.grid.cell_fmt_at((c0, r));
                for (offset, c) in (1_i64..).zip(c0.saturating_add(1)..=c1) {
                    right.push(((c, r), formula::offset_refs(&raw, offset, 0), Some(fmt)));
                }
            }
        }
        specs.push(immediate(
            "fill-right",
            "Fill right",
            actions::set_cells_action(sheet, sheet_id.clone(), right),
        ));

        let (c, r) = self.grid.active();
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
                specs.push(immediate(
                    "autosum-active",
                    "AutoSum column",
                    actions::set_cell_action(sheet, sheet_id, (c, r), &formula, None),
                ));
            }
        }

        specs.push(tilo_actions::ActionSpec::prompted_operation(
            "go-to-cell",
            "Go to cell…",
            "Cell reference",
            "e.g. B12",
            OperationKind::View,
            actions::SHEETS_OPERATION_TYPE,
            |reference| {
                tilo_actions::EncodedAction::encode_operation(
                    actions::SHEETS_OPERATION_TYPE,
                    &actions::SheetsOperation::GoToCell {
                        reference: reference.to_string(),
                    },
                )
                .expect("SheetsOperation is serializable")
            },
        ));
        specs.push(
            ActionSpec::operation(
                "toggle-formula-view",
                if self.grid.formula_view() {
                    "Hide formulas"
                } else {
                    "Show formulas"
                },
                OperationKind::View,
                actions::SHEETS_OPERATION_TYPE,
                &actions::SheetsOperation::ToggleFormulaView,
            )
            .expect("SheetsOperation is serializable"),
        );
        specs
    }

    /// The last [`EncodedAction`](tilo_actions::EncodedAction) an on-screen
    /// affordance dispatched, for the affordance == [`ActionSpec`](tilo_actions::ActionSpec)
    /// covenant test: a control and its Ctrl+K entry must build the identical
    /// action.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
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
        self.status_bubble.poke(ui::time::now());
    }

    /// Shots hook: open the File menu so its actions appear in the frame.
    pub fn shots_open_file_menu(&mut self) {
        self.open_menu = OpenMenu::File;
    }

    /// Shots hook: exercise the same loading veil real persistent launches
    /// display while their bounded workbook load is still in flight.
    pub fn shots_show_loading(&mut self) {
        self.shots_loading = true;
    }

    /// Shots hook: stage a failed import acknowledgement in the normal status
    /// surface. The production path supplies the same text from its worker.
    pub fn shots_show_error(&mut self) {
        self.status = "Excel import failed: the workbook is password protected".into();
        self.arm_status_bubble_for_shots();
    }

    /// Shots hook: stage the collaboration acknowledgement used when an
    /// incoming revision supersedes the value currently being edited.
    pub fn shots_show_conflict(&mut self) {
        self.status = "Budget was updated by a collaborator; showing the latest value".into();
        self.arm_status_bubble_for_shots();
    }

    /// Shots hook: prove that acknowledgement remains visible with animations
    /// suppressed for the reduced-motion preference.
    pub fn shots_show_reduced_motion(&mut self) {
        self.set_motion_reduced(true);
        self.status = "Changes saved".into();
        self.arm_status_bubble_for_shots();
    }

    /// Shots hook: open the Format menu.
    pub fn shots_open_format_menu(&mut self) {
        self.open_menu = OpenMenu::Format;
    }

    /// Shots hook: expose the compact-toolbar overflow menu so responsive
    /// visual regression covers controls that are intentionally moved off the
    /// narrow, enlarged-text command row.
    pub fn shots_open_more_menu(&mut self) {
        self.open_menu = OpenMenu::More;
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
    pub fn shots_open_command_palette(&mut self, doc: &AutoCommit) {
        self.open_command_palette(doc);
    }

    /// Shots hook: offset both axes so the scrollbar thumbs visibly prove the
    /// two-dimensional grid viewport rather than merely drawing idle rails.
    pub fn shots_scroll_both_axes(&mut self) {
        self.grid.on_scroll(540.0, 260.0);
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

    /// Shots hook: add the two durable chart types and a range-anchored thread
    /// through the same replayable action path used by Ctrl+K and agents.
    pub fn shots_seed_charts_and_comments(&mut self, doc: &mut AutoCommit) {
        let sheet_id = self.sheet_identity(self.active_sheet);
        let charts = [
            actions::put_chart_action(
                self.active_sheet,
                sheet_id.clone(),
                "Quarterly spend",
                ChartKind::Column,
                (1, 1),
                (4, 6),
            ),
            actions::put_chart_action(
                self.active_sheet,
                sheet_id.clone(),
                "Monthly trend",
                ChartKind::Line,
                (1, 1),
                (3, 6),
            ),
        ];
        for chart in &charts {
            self.perform_action(doc, chart);
        }
        let comment = actions::put_comment_action(
            self.active_sheet,
            sheet_id.clone(),
            (1, 1),
            (4, 1),
            "Alex",
            "Check the rent increase before sending this budget.",
        );
        let SheetsAction::PutComment { comment_id, .. } = &comment else {
            return;
        };
        let comment_id = comment_id.clone();
        self.perform_action(doc, &comment);
        self.perform_action(
            doc,
            &actions::reply_comment_action(
                self.active_sheet,
                sheet_id.clone(),
                comment_id.clone(),
                "Sam",
                "I will verify the change.",
            ),
        );
        self.perform_action(
            doc,
            &actions::set_comment_resolved_action(self.active_sheet, sheet_id, comment_id, true),
        );
        self.reload(doc);
        self.status = "2 charts and 1 resolved comment thread added".into();
        self.arm_status_bubble_for_shots();
    }

    /// The document's effective locale (per-doc overrides over global).
    fn locale(&self) -> EffectiveLocale {
        EffectiveLocale::resolve(&self.global.locale, &self.doc_settings)
    }

    fn default_currency(&self) -> Currency {
        self.locale().currency
    }

    /// The durable identity cached alongside the rendered sheet list.  The
    /// cache is refreshed from the CRDT before UI actions are offered, so an
    /// encoded UI/palette action never falls back to a positional target.
    fn sheet_identity(&self, idx: usize) -> String {
        self.searchable_sheets
            .get(idx)
            .map(|sheet| sheet.id.clone())
            .unwrap_or_default()
    }

    fn sheet_slot_anchors(
        &self,
        at: usize,
        excluding: Option<usize>,
    ) -> (Option<String>, Option<String>) {
        let ids: Vec<String> = self
            .searchable_sheets
            .iter()
            .enumerate()
            .filter(|(idx, _)| Some(*idx) != excluding)
            .map(|(_, sheet)| sheet.id.clone())
            .collect();
        (
            ids.get(at).cloned(),
            at.checked_sub(1).and_then(|idx| ids.get(idx)).cloned(),
        )
    }

    // --- document plumbing --------------------------------------------------

    /// Finish startup after the persistence worker has either supplied the
    /// prior workbook or established that there is none.  A real launch never
    /// fabricates sample data; only the headless shots/test path seeds it.
    fn finish_initialize(&mut self, doc: &mut AutoCommit, durable_heads: Option<Vec<ChangeHash>>) {
        if SheetsDoc::is_empty(doc) {
            if self.run.seeds_sample_data() {
                seed_workbook(doc);
                self.active_sheet = 0;
                // Preselect a smart-typed currency cell so the formula bar and
                // the type indicator are populated in the captured frame.
                if let Some(addr) = a1_to_addr("E2") {
                    self.grid.select_only(addr);
                }
            } else if self.run.seeds_blank_sheet() {
                SheetsDoc::add_sheet(doc, "Sheet1");
                self.active_sheet = 0;
                self.grid.select_only((0, 0));
            }
        } else {
            // Restored workbook: land on the first sheet at A1. The active tab
            // is local presentation, not collaborative workbook content.
            self.active_sheet = 0;
            self.grid.select_only((0, 0));
        }
        let current_heads = doc.get_heads();
        let load_start_heads = self.load_start_heads.take().unwrap_or_default();
        // A disk image can only acknowledge the exact document it contained.
        // If local/collaborative changes existed before or arrived during the
        // asynchronous load, leave the merged document dirty so one coalesced
        // save captures every head rather than silently losing the newer one.
        self.saved_heads = match durable_heads {
            Some(durable) if load_start_heads.is_empty() && current_heads == durable => {
                current_heads
            }
            None if load_start_heads.is_empty() => current_heads,
            Some(_) | None => Vec::new(),
        };
        self.pending.seeded = true;
    }

    /// Start a capacity-one background load. The worker does both file I/O and
    /// Automerge decoding; the UI only merges the accepted completed document.
    fn start_persistence_load(&mut self, load_start_heads: Vec<ChangeHash>) -> Result<(), String> {
        let generation = self.persistence_generation.checked_add(1).ok_or_else(|| {
            "workbook persistence generation is exhausted; restart Sheets".to_string()
        })?;
        let (location, name) = workbook_store();
        let (tx, rx) = mpsc::sync_channel(1);
        run_worker("sheets-persistence-load", move || {
            let read = tilo_files::Dir::open(location)
                .and_then(|dir| tilo_files::blocking::read(&dir, &name));
            let result = match read {
                Ok(bytes) => AutoCommit::load(&bytes)
                    .map(|mut saved| {
                        let heads = saved.get_heads();
                        Some(Box::new((saved, heads)))
                    })
                    .map_err(|error| format!("could not decode saved workbook: {error}")),
                Err(tilo_files::FsError::NotFound) => Ok(None),
                Err(error) => Err(format!("could not read saved workbook: {error}")),
            };
            let _ = tx.send(PersistenceResult::Load { generation, result });
        })
        .map_err(|error| format!("could not start workbook load worker: {error}"))?;
        self.persistence_generation = generation;
        self.persistence_rx = Some(rx);
        self.active_persistence = Some((generation, PersistenceJob::Load));
        self.load_start_heads = Some(load_start_heads);
        self.status = "Loading workbook…".into();
        Ok(())
    }

    /// Serialize and write a stable document fork away from the event loop.
    /// `heads` identifies exactly what reached durable storage; concurrent edits
    /// remain dirty and are coalesced into one later write.
    fn start_persistence_save(
        &mut self,
        doc: &mut AutoCommit,
        heads: Vec<ChangeHash>,
    ) -> Result<(), String> {
        let generation = self.persistence_generation.checked_add(1).ok_or_else(|| {
            "workbook persistence generation is exhausted; restart Sheets".to_string()
        })?;
        let (location, name) = workbook_store();
        let mut saved_doc = doc.fork();
        let result_heads = heads.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        run_worker("sheets-persistence-save", move || {
            let result = tilo_files::Dir::open(location)
                .and_then(|dir| tilo_files::blocking::write_atomic(&dir, &name, &saved_doc.save()))
                .map_err(|error| error.to_string());
            let _ = tx.send(PersistenceResult::Save {
                generation,
                heads: result_heads,
                result,
            });
        })
        .map_err(|error| format!("could not start workbook save worker: {error}"))?;
        self.persistence_generation = generation;
        self.persistence_rx = Some(rx);
        self.active_persistence = Some((generation, PersistenceJob::Save { heads }));
        Ok(())
    }

    /// Receive exactly one current persistence result. Disconnected workers and
    /// failed writes leave the document dirty, visibly report the problem, and
    /// retry with backoff rather than silently advancing `saved_heads`.
    #[allow(
        clippy::too_many_lines,
        reason = "one worker receipt owns generation validation, startup handoff, and save settlement"
    )]
    fn drain_persistence_worker(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.persistence_rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.persistence_rx = None;
                let active = self.active_persistence.take();
                self.last_persist_failure = Some(ui::time::now());
                self.status = match active {
                    Some((_, PersistenceJob::Load)) => {
                        "Workbook load worker disconnected; opening a new workbook".into()
                    }
                    Some((_, PersistenceJob::Save { .. })) => {
                        "Workbook save worker disconnected; changes will be retried".into()
                    }
                    None => "Workbook persistence worker disconnected".into(),
                };
                if !self.pending.seeded {
                    self.finish_initialize(doc, None);
                }
                return;
            }
        };
        self.persistence_rx = None;
        let Some((generation, active)) = self.active_persistence.clone() else {
            return;
        };
        let result_generation = match &result {
            PersistenceResult::Load { generation, .. }
            | PersistenceResult::Save { generation, .. } => *generation,
        };
        if generation != result_generation {
            // A generation mismatch cannot be safely associated with the
            // retained job. Settle it as failed rather than letting a later
            // dirty check overwrite that ownership.
            self.active_persistence = None;
            self.last_persist_failure = Some(ui::time::now());
            self.status =
                "Discarded a stale workbook persistence result; changes will be retried".into();
            if !self.pending.seeded {
                self.finish_initialize(doc, None);
            }
            return;
        }
        self.active_persistence = None;
        match (active, result) {
            (PersistenceJob::Load, PersistenceResult::Load { result, .. }) => {
                let durable_heads = match result {
                    Ok(Some(saved)) => {
                        let (mut saved, durable_heads) = *saved;
                        // If any collaboration/local state appeared while the
                        // disk worker was running, an Automerge root-map merge
                        // can conflict at `wb`. Bring the saved sheets in via
                        // the same atomic canonical import action instead, so
                        // neither workbook silently wins or disappears.
                        if doc.get_heads().is_empty() {
                            if let Err(error) = doc.merge_incoming(&mut saved) {
                                self.status =
                                    format!("Saved workbook could not be merged: {error}");
                                self.last_persist_failure = Some(ui::time::now());
                                None
                            } else {
                                Some(durable_heads)
                            }
                        } else {
                            let tables = (0..SheetsDoc::sheet_count(&mut saved))
                                .map(|sheet| {
                                    let snapshot = SheetsDoc::snapshot_sheet(
                                        &mut saved,
                                        sheet,
                                        self.default_currency(),
                                    );
                                    Self::table_from_snapshot(&snapshot)
                                })
                                .collect();
                            if let Err(error) = self.import_tables_with(doc, tables, false, None) {
                                self.status = format!("Could not import saved workbook: {error}");
                                self.last_persist_failure = Some(ui::time::now());
                            }
                            self.load_start_heads = Some(doc.get_heads());
                            None
                        }
                    }
                    Ok(None) => None,
                    Err(error) => {
                        self.status = format!("Could not load saved workbook: {error}");
                        self.last_persist_failure = Some(ui::time::now());
                        None
                    }
                };
                self.finish_initialize(doc, durable_heads);
            }
            (
                PersistenceJob::Save { heads: expected },
                PersistenceResult::Save { heads, result, .. },
            ) if expected == heads => match result {
                Ok(()) => {
                    let had_failure = self.last_persist_failure.take().is_some();
                    self.saved_heads = heads;
                    if had_failure {
                        self.status = "Workbook saved".into();
                    }
                }
                Err(error) => {
                    self.status =
                        format!("Could not save workbook: {error}; changes will be retried");
                    self.last_persist_failure = Some(ui::time::now());
                }
            },
            _ => {
                self.status = "Discarded a stale workbook persistence result".into();
            }
        }
    }

    /// Queue a coalesced persistence write when the current CRDT heads are not
    /// yet durable. The fork/serialization/write itself is entirely off-thread.
    fn persist_if_dirty(&mut self, doc: &mut AutoCommit) {
        if !self.run.persists()
            || !self.pending.seeded
            || self.persistence_rx.is_some()
            || self.active_persistence.is_some()
        {
            return;
        }
        let heads = doc.get_heads();
        if heads == self.saved_heads {
            return;
        }
        if self
            .last_persist_failure
            .is_some_and(|failed_at| failed_at.elapsed() < PERSIST_RETRY_DELAY)
        {
            return;
        }
        if let Err(error) = self.start_persistence_save(doc, heads) {
            self.status = format!("Could not save workbook: {error}; changes will be retried");
            self.last_persist_failure = Some(ui::time::now());
        }
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
        let count = SheetsDoc::sheet_count(doc).max(1);
        let sheet = self.active_sheet.min(count - 1);
        if let Some(c) = &self.cache
            && c.sheet == sheet
            && c.heads == heads
        {
            self.active_sheet = sheet;
            return;
        }

        self.active_sheet = sheet;
        // A tab switch still needs an active-sheet recalculation below, but it
        // must not traverse every other sheet body. The complete search
        // projection and its postings change only with Automerge heads.
        if self.search_projection_heads.as_ref() != Some(&heads) {
            self.sheet_names = SheetsDoc::sheet_names(doc);
            self.sheet_colors = SheetsDoc::sheet_colors(doc);
            self.doc_settings = SheetsDoc::doc_settings(doc);
            let cur = self.default_currency();
            self.searchable_sheets = (0..self.sheet_names.len())
                .filter_map(|idx| {
                    let id = SheetsDoc::sheet_resource_id(doc, idx)?;
                    let snap = SheetsDoc::snapshot_sheet(doc, idx, cur);
                    let values = formula::recalculate(&snap.raw);
                    let mut cells = snap
                        .raw
                        .iter()
                        .map(|(&(col, row), raw)| {
                            let fmt = snap.fmt.get(&(col, row)).copied().unwrap_or_else(|| {
                                raw.strip_prefix('=')
                                    .and_then(|body| formula::infer_result_format(body, &snap.fmt))
                                    .unwrap_or(CellFormat::General)
                            });
                            let display = values
                                .get(&(col, row))
                                .map_or_else(String::new, |value| format_value(value, fmt));
                            actions::SearchableCell {
                                col,
                                row,
                                raw: raw.clone(),
                                display,
                            }
                        })
                        .collect::<Vec<_>>();
                    cells.sort_by_key(|cell| (cell.row, cell.col));
                    Some(actions::SearchableSheet {
                        id,
                        name: snap.name,
                        cells,
                    })
                })
                .collect();
            self.search_provider.refresh(&self.searchable_sheets);
            self.search_projection_heads = Some(heads.clone());
        }
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, self.active_sheet, cur);
        self.sheet_charts = SheetsDoc::charts(doc, self.active_sheet);
        self.sheet_comments = SheetsDoc::comments(doc, self.active_sheet);
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
        self.grid.set_bold(snap.bold);
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
            self.perform_action(
                doc,
                &actions::set_cells_action(
                    sheet,
                    SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                    writes,
                ),
            );
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
            self.perform_action(
                doc,
                &actions::set_formats_action(
                    sheet,
                    SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                    cells,
                ),
            );
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
            self.perform_action(
                doc,
                &actions::set_formats_action(
                    sheet,
                    SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                    cells,
                ),
            );
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
            self.perform_action(
                doc,
                &actions::set_cells_action(
                    sheet,
                    SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                    writes,
                ),
            );
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
        self.perform_action(
            doc,
            &actions::set_cells_action(
                sheet,
                SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                writes,
            ),
        );
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
            &actions::set_cell_action(
                self.active_sheet,
                SheetsDoc::sheet_resource_id(doc, self.active_sheet).unwrap_or_default(),
                (c, r),
                &formula,
                None,
            ),
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
        self.perform_action(
            doc,
            &actions::set_cells_action(
                sheet,
                SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                writes,
            ),
        );
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
        self.perform_action(
            doc,
            &actions::set_cells_action(
                sheet,
                SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                writes,
            ),
        );
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
        self.perform_action(
            doc,
            &actions::set_cells_action(
                sheet,
                SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                writes,
            ),
        );
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
    #[cfg(test)]
    fn import_csv_from(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        if let Err(error) = Self::check_import_file_size(path) {
            self.status = format!("CSV import failed: {error}");
            return;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("CSV import failed: {e}");
                return;
            }
        };
        let parsed = crate::csv::parse_csv(&text);
        if parsed.len() > MAX_IMPORT_CELLS {
            self.status = format!(
                "CSV import failed: the file contains more than {MAX_IMPORT_CELLS} populated cells"
            );
            return;
        }
        let cur = self.default_currency();
        let mut writes = Vec::with_capacity(parsed.len());
        for (addr, raw) in parsed {
            let inferred = infer_entry(&raw, cur);
            writes.push((
                addr,
                inferred.raw,
                (inferred.format != CellFormat::General).then_some(inferred.format),
            ));
        }
        match self.import_workbook_action(
            doc,
            vec![(sheet_name_from_path(path), writes)],
            true,
            Some(path),
        ) {
            Ok(count) => self.status = format!("Imported {count} cells from {}", file_label(path)),
            Err(error) => self.status = format!("CSV import failed: {error}"),
        }
        self.reload(doc);
    }

    /// Build an interchange table for one sheet from its computed values.
    ///
    /// This is intentionally explicit about the sheet index: CSV is a
    /// single-table format and exports only the active sheet, while workbook
    /// formats (`.xlsx`/`.ods`) must preserve every tab. Keeping the common
    /// materialisation here also ensures formulas and propagated formats take
    /// the same path for both kinds of export.
    #[cfg(test)]
    fn table_for_sheet(&self, doc: &mut AutoCommit, sheet: usize) -> crate::xlsx::SheetTable {
        let cur = self.default_currency();
        let snap = SheetsDoc::snapshot_sheet(doc, sheet, cur);
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
        crate::xlsx::SheetTable::from_cells(&snap.name, &snap.raw, &fmt, &values, &snap.bold)
    }

    /// Materialise the whole workbook in tab order for workbook interchange.
    /// Export runs only after the user chooses a destination, so this work is
    /// never paid on an interaction/render frame.
    #[cfg(test)]
    fn workbook_tables(&self, doc: &mut AutoCommit) -> Vec<crate::xlsx::SheetTable> {
        (0..SheetsDoc::sheet_count(doc))
            .map(|sheet| self.table_for_sheet(doc, sheet))
            .collect()
    }

    fn export_csv(&mut self, _doc: &mut AutoCommit) {
        self.begin_export(Interchange::Csv);
    }

    /// Core: write the active sheet's computed values to `path` as CSV.
    #[cfg(test)]
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

    /// Core: write every sheet to `path` as an `.xlsx` workbook.
    #[cfg(test)]
    fn export_xlsx_to(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let tables = self.workbook_tables(doc);
        match crate::xlsx::write_xlsx(path, &tables) {
            Ok(()) => self.status = format!("Exported {}", file_label(path)),
            Err(e) => self.status = format!("Excel export failed: {e}"),
        }
    }

    fn export_ods(&mut self, _doc: &mut AutoCommit) {
        self.begin_export(Interchange::Ods);
    }

    /// Core: write every sheet to `path` as an `.ods` spreadsheet.
    #[cfg(test)]
    fn export_ods_to(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        let tables = self.workbook_tables(doc);
        match crate::ods::write_ods(path, &tables) {
            Ok(()) => self.status = format!("Exported {}", file_label(path)),
            Err(e) => self.status = format!("Calc export failed: {e}"),
        }
    }

    /// Import a user's file (one table per sheet) into new sheets; into a
    /// workbook that is still blank, the file replaces its empty sheet.
    fn import_tables(
        &mut self,
        doc: &mut AutoCommit,
        tables: Vec<crate::xlsx::SheetTable>,
        source: Option<&std::path::Path>,
    ) -> Result<usize, String> {
        self.import_tables_with(doc, tables, true, source)
    }

    /// Import a parsed set of tables (one per sheet) into new sheets, running
    /// smart inference where a source cell carried no explicit format.
    fn import_tables_with(
        &mut self,
        doc: &mut AutoCommit,
        tables: Vec<crate::xlsx::SheetTable>,
        replace_blank: bool,
        source: Option<&std::path::Path>,
    ) -> Result<usize, String> {
        let cur = self.default_currency();
        let mut total: usize = 0;
        let mut imported = Vec::new();
        for table in tables {
            let mut writes = Vec::new();
            for (r, row) in (0u32..).zip(&table.rows) {
                for (c, cell) in (0u32..).zip(row) {
                    let Some(cell) = cell else { continue };
                    let addr = (c, r);
                    // A formula cell restores its live formula; a value cell
                    // stores its (re-inferred) value. The format still derives
                    // from the imported value, not the formula text.
                    let raw_to_store = cell.formula.as_deref().unwrap_or(cell.raw.as_str());
                    let fmt = if cell.format == CellFormat::General {
                        infer_entry(&cell.raw, cur).format
                    } else {
                        cell.format
                    };
                    writes.push((
                        addr,
                        raw_to_store.to_string(),
                        (fmt != CellFormat::General).then_some(fmt),
                    ));
                }
            }
            if total.saturating_add(writes.len()) > MAX_IMPORT_CELLS {
                return Err(format!(
                    "the workbook contains more than {MAX_IMPORT_CELLS} populated cells"
                ));
            }
            total += writes.len();
            imported.push((table.name, writes));
        }
        self.import_workbook_action(doc, imported, replace_blank, source)?;
        self.reload(doc);
        Ok(total)
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

    /// Materialise an entire parsed workbook through one canonical action.
    /// The action validates every tab and cell before touching the CRDT, so
    /// collaborators cannot observe a half-import if a later source cell is
    /// hostile or malformed; history and undo see one meaningful operation.
    fn import_workbook_action(
        &mut self,
        doc: &mut AutoCommit,
        sheets: ImportedWorkbook,
        replace_blank: bool,
        source: Option<&std::path::Path>,
    ) -> Result<usize, String> {
        let mut total = 0usize;
        let mut action =
            actions::import_sheets_action(sheets.into_iter().map(|(name, mut writes)| {
                let name = if name.trim().is_empty() {
                    "Imported".to_string()
                } else {
                    name
                };
                writes.sort_by_key(|(addr, _, _)| (addr.1, addr.0));
                total = total.saturating_add(writes.len());
                (
                    name,
                    writes
                        .into_iter()
                        .map(|(addr, raw, fmt)| actions::CellWrite {
                            col: addr.0,
                            row: addr.1,
                            raw,
                            fmt: fmt.map(CellFormat::as_str),
                        })
                        .collect(),
                )
            }));
        if total > MAX_IMPORT_CELLS {
            return Err(format!("more than {MAX_IMPORT_CELLS} populated cells"));
        }
        let last_sheet_id = match &mut action {
            SheetsAction::ImportSheets {
                sheets,
                replace_blank: replace,
                source: from,
            } => {
                *replace = replace_blank;
                *from = source.map(import_source);
                sheets.last().map(|sheet| sheet.sheet_id.clone())
            }
            _ => None,
        };
        self.try_perform_action(doc, &action)
            .map_err(|error| error.to_string())?;
        if let Some(sheet_id) = last_sheet_id {
            self.active_sheet = SheetsDoc::sheet_index_by_resource_id(doc, &sheet_id)
                .ok_or_else(|| "the imported sheet was not created".to_string())?;
        }
        Ok(total)
    }

    fn import_xlsx(&mut self, _doc: &mut AutoCommit) {
        self.begin_import(Interchange::Xlsx);
    }

    /// Core: import the `.xlsx` workbook at `path` into new sheets.
    #[cfg(test)]
    fn import_xlsx_from(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        if let Err(error) = Self::check_import_file_size(path) {
            self.status = format!("Excel import failed: {error}");
            return;
        }
        match crate::xlsx::read_xlsx(path) {
            Ok(tables) => match self.import_tables(doc, tables, Some(path)) {
                Ok(total) => {
                    self.status = format!("Imported {total} cells from {}", file_label(path));
                }
                Err(error) => self.status = format!("Excel import failed: {error}"),
            },
            Err(e) => self.status = format!("Excel import failed: {e}"),
        }
    }

    fn import_ods(&mut self, _doc: &mut AutoCommit) {
        self.begin_import(Interchange::Ods);
    }

    /// Core: import the `.ods` spreadsheet at `path` into new sheets.
    #[cfg(test)]
    fn import_ods_from(&mut self, doc: &mut AutoCommit, path: &std::path::Path) {
        if let Err(error) = Self::check_import_file_size(path) {
            self.status = format!("Calc import failed: {error}");
            return;
        }
        match crate::ods::read_ods(path) {
            Ok(tables) => match self.import_tables(doc, tables, Some(path)) {
                Ok(total) => {
                    self.status = format!("Imported {total} cells from {}", file_label(path));
                }
                Err(error) => self.status = format!("Calc import failed: {error}"),
            },
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

    /// Start exactly one fallible worker after a portal/direct-path operation
    /// has been accepted.  Generation allocation is checked rather than
    /// wrapping: reusing an exhausted generation could let an old completion
    /// mutate a newly accepted request.
    fn start_interchange_job(
        &mut self,
        doc: &mut AutoCommit,
        operation: PendingIo,
        path: PathBuf,
    ) -> Result<(), String> {
        if let PendingIo::Import(_) = operation
            && let Some(sheet) = SheetsDoc::sheet_index_by_source(doc, &import_source(&path))
        {
            self.switch_sheet(doc, sheet);
            let name = self.sheet_names.get(sheet).cloned().unwrap_or_default();
            self.status = format!("{} is already open as {name}", file_label(&path));
            // Imports publish a job result, which carries no terminal value.
            self.finish_resource_output(Ok(tilo_actions::ActionOutput::default()));
            return Ok(());
        }
        if self.interchange_rx.is_some() || self.active_interchange_generation.is_some() {
            return Err("a spreadsheet import or export is already in progress".into());
        }
        let generation = self.interchange_generation.checked_add(1).ok_or_else(|| {
            "spreadsheet operation generation is exhausted; restart Sheets".to_string()
        })?;
        self.interchange_generation = generation;
        let snapshots = match operation {
            PendingIo::Export(Interchange::Csv) => vec![SheetsDoc::snapshot_sheet(
                doc,
                self.active_sheet,
                self.default_currency(),
            )],
            PendingIo::Export(_) => (0..SheetsDoc::sheet_count(doc))
                .map(|sheet| SheetsDoc::snapshot_sheet(doc, sheet, self.default_currency()))
                .collect(),
            PendingIo::Import(_) => Vec::new(),
        };
        let (tx, rx): (SyncSender<InterchangeResult>, Receiver<InterchangeResult>) =
            mpsc::sync_channel(1);
        self.interchange_wake = ui::wake::WakeHandle::new();
        let reply = InterchangeReply {
            tx: Some(tx),
            wake: self.interchange_wake.clone(),
        };
        ui::rt::spawn_thread("sheets-interchange", move || {
            let reply = reply;
            let result = match operation {
                PendingIo::Import(format) => InterchangeResult::Import {
                    generation,
                    format,
                    path: path.clone(),
                    result: SheetsApp::read_import_tables(format, &path),
                },
                PendingIo::Export(format) => InterchangeResult::Export {
                    generation,
                    format,
                    destination: path.clone(),
                    result: SheetsApp::prepare_export_snapshots(
                        format, &path, generation, &snapshots,
                    ),
                },
            };
            reply.send(result);
        })
        .map_err(|error| format!("could not start spreadsheet worker: {error}"))?;
        self.interchange_rx = Some(rx);
        self.active_interchange_generation = Some(generation);
        self.status = match operation {
            PendingIo::Import(format) => format!("Importing .{}…", format.ext()),
            PendingIo::Export(format) => format!("Exporting .{}…", format.ext()),
        };
        Ok(())
    }

    fn read_import_tables(
        format: Interchange,
        path: &std::path::Path,
    ) -> Result<Vec<crate::xlsx::SheetTable>, String> {
        Self::check_import_file_size(path)?;
        match format {
            Interchange::Xlsx => crate::xlsx::read_xlsx(path).map_err(|error| error.to_string()),
            Interchange::Ods => crate::ods::read_ods(path).map_err(|error| error.to_string()),
            Interchange::Csv => {
                let bytes = crate::xlsx::read_file(path).map_err(|error| error.to_string())?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| "the CSV file is not UTF-8 text".to_string())?;
                let cells = crate::csv::parse_csv(&text);
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
                    name: sheet_name_from_path(path),
                    rows,
                }])
            }
        }
    }

    /// Encode to a private same-directory temporary file. This worker never
    /// writes the user-selected destination; publishing is a later UI receipt
    /// after a resource invocation has survived cancellation polling.
    fn prepare_export_snapshots(
        format: Interchange,
        destination: &std::path::Path,
        generation: u64,
        snapshots: &[crate::doc::SheetSnapshot],
    ) -> Result<PathBuf, String> {
        let temporary = Self::reserve_export_temp(destination, generation)?;
        let tables = snapshots
            .iter()
            .map(Self::table_from_snapshot)
            .collect::<Vec<_>>();
        let result = match format {
            Interchange::Xlsx => {
                crate::xlsx::write_xlsx(&temporary, &tables).map_err(|error| error.to_string())
            }
            Interchange::Ods => {
                crate::ods::write_ods(&temporary, &tables).map_err(|error| error.to_string())
            }
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
                crate::xlsx::write_file(&temporary, crate::csv::to_csv(&rows).as_bytes())
                    .map_err(|error| error.to_string())
            }
        };
        if let Err(error) = result {
            Self::discard_export_temp(&temporary);
            return Err(error);
        }
        Ok(temporary)
    }

    fn reserve_export_temp(
        destination: &std::path::Path,
        generation: u64,
    ) -> Result<PathBuf, String> {
        let parent = destination
            .parent()
            .ok_or_else(|| "export destination has no parent directory".to_string())?;
        let dir = tilo_files::Dir::open(tilo_files::Location::Native(parent.to_path_buf()))
            .map_err(|error| error.to_string())?;
        let stem = destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workbook");
        for _ in 0..4 {
            let name = format!(".{stem}.tilo-export-{generation}-{}.tmp", Uuid::new_v4());
            let reserved = tilo_files::VPath::new(&name)
                .map_err(|error| error.to_string())
                .and_then(|name| {
                    tilo_files::blocking::create_new(&dir, &name, b"")
                        .map_err(|error| error.to_string())
                })?;
            if reserved {
                return Ok(parent.join(name));
            }
        }
        Err("could not reserve an exclusive export temporary file".into())
    }

    fn publish_export_temp(
        temporary: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<(), String> {
        let bytes = crate::xlsx::read_file(temporary).map_err(|error| error.to_string())?;
        crate::xlsx::write_file(destination, &bytes).map_err(|error| error.to_string())?;
        Self::discard_export_temp(temporary);
        Ok(())
    }

    fn publish_or_discard_export_temp(
        temporary: &std::path::Path,
        destination: &std::path::Path,
    ) -> Result<(), String> {
        match Self::publish_export_temp(temporary, destination) {
            Ok(()) => Ok(()),
            Err(error) => {
                Self::discard_export_temp(temporary);
                Err(error)
            }
        }
    }

    fn discard_export_temp(temporary: &std::path::Path) {
        if let Ok((dir, name)) = tilo_files::file_at(temporary) {
            let _ = tilo_files::blocking::remove(&dir, &name);
        }
    }

    fn table_from_snapshot(snap: &crate::doc::SheetSnapshot) -> crate::xlsx::SheetTable {
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
                    PortalOutcome::Opened(path) => {
                        if let Err(error) = self.start_interchange_job(doc, op, path) {
                            self.status = format!("Spreadsheet operation failed: {error}");
                            self.finish_resource_io(Err(error));
                        }
                    }
                    PortalOutcome::Cancelled => {
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.resource_result =
                                Some(tilo_actions::InvocationUpdate::Cancelled {
                                    invocation,
                                    message: Some(
                                        "spreadsheet file selection was cancelled".into(),
                                    ),
                                });
                        }
                    }
                    PortalOutcome::Failed(e) => {
                        self.status = format!("File chooser unavailable: {e}");
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
                                    code: "sheets.portal_failed".into(),
                                    message: e,
                                    details: None,
                                    retryable: false,
                                },
                            });
                        }
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.io = None;
                self.status = "File chooser disconnected before returning a selection".into();
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "sheets.portal_disconnected".into(),
                            message: self.status.clone(),
                            details: None,
                            retryable: true,
                        },
                    });
                }
            }
        }
    }

    /// Apply a prepared resource result only after lifecycle polling has
    /// confirmed that its invocation remains current.
    fn settle_prepared_resource_io(&mut self, doc: &mut AutoCommit) {
        let Some(invocation) = self.resource_confirmation.take() else {
            return;
        };
        if self.resource_invocation != Some(invocation) {
            return;
        }
        let Some(prepared) = self.prepared_resource_io.take() else {
            return;
        };
        let result = match prepared {
            PreparedResourceIo::Import {
                format,
                path,
                tables,
                ..
            } => self.import_tables(doc, tables, Some(&path)).map(|total| {
                self.status = format!(
                    "Imported {total} cells from .{} {}",
                    format.ext(),
                    file_label(&path)
                );
            }),
            PreparedResourceIo::Export {
                format,
                destination,
                temporary,
                ..
            } => Self::publish_or_discard_export_temp(&temporary, &destination).map(|()| {
                self.status = format!("Exported .{} to {}", format.ext(), file_label(&destination));
            }),
        };
        if let Err(error) = &result {
            self.status = format!("Spreadsheet operation failed: {error}");
        }
        self.finish_resource_io(result);
    }

    /// Reap replies of cancelled jobs; their prepared exports never publish.
    fn drain_orphaned_interchange(&mut self) {
        self.orphaned_interchange.retain(|rx| match rx.try_recv() {
            Ok(InterchangeResult::Export {
                result: Ok(temporary),
                ..
            }) => {
                Self::discard_export_temp(&temporary);
                false
            }
            Ok(_) | Err(TryRecvError::Disconnected) => false,
            Err(TryRecvError::Empty) => true,
        });
    }

    fn discard_prepared_resource_io(&mut self) {
        if let Some(PreparedResourceIo::Export { temporary, .. }) = self.prepared_resource_io.take()
        {
            Self::discard_export_temp(&temporary);
        }
    }

    /// Apply only the completed result whose generation still belongs to the
    /// accepted job. File parsing/encoding happens off-thread; an agent-owned
    /// result stops at the prepared boundary until lifecycle polling permits
    /// its canonical import or destination publish.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded worker receipt owns generation, cancellation, and typed settlement"
    )]
    fn drain_interchange_worker(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.interchange_rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.interchange_rx = None;
                self.active_interchange_generation = None;
                self.status = "Spreadsheet worker disconnected".into();
                self.finish_resource_io(Err(self.status.clone()));
                return;
            }
        };
        self.interchange_rx = None;
        let generation = match &result {
            InterchangeResult::Import { generation, .. }
            | InterchangeResult::Export { generation, .. } => *generation,
        };
        if self.active_interchange_generation != Some(generation) {
            if let InterchangeResult::Export {
                result: Ok(temporary),
                ..
            } = result
            {
                Self::discard_export_temp(&temporary);
            }
            self.active_interchange_generation = None;
            return;
        }
        self.active_interchange_generation = None;
        if self.resource_invocation.is_some() {
            match result {
                InterchangeResult::Import {
                    format,
                    path,
                    result: Ok(tables),
                    ..
                } => {
                    self.prepared_resource_io = Some(PreparedResourceIo::Import {
                        format,
                        path,
                        tables,
                    });
                    self.status = "Spreadsheet import is ready to apply".into();
                }
                InterchangeResult::Export {
                    format,
                    destination,
                    result: Ok(temporary),
                    ..
                } => {
                    self.prepared_resource_io = Some(PreparedResourceIo::Export {
                        format,
                        destination,
                        temporary,
                    });
                    self.status = "Spreadsheet export is ready to save".into();
                }
                InterchangeResult::Import {
                    format,
                    result: Err(error),
                    ..
                }
                | InterchangeResult::Export {
                    format,
                    result: Err(error),
                    ..
                } => {
                    self.status =
                        format!("Spreadsheet .{} operation failed: {error}", format.ext());
                    self.finish_resource_io(Err(self.status.clone()));
                }
            }
            return;
        }
        let outcome = match result {
            InterchangeResult::Import {
                format,
                path,
                result,
                ..
            } => result
                .and_then(|tables| self.import_tables(doc, tables, Some(&path)))
                .map(|total| {
                    self.status = format!(
                        "Imported {total} cells from .{} {}",
                        format.ext(),
                        file_label(&path)
                    );
                }),
            InterchangeResult::Export {
                format,
                destination,
                result,
                ..
            } => result
                .and_then(|temporary| {
                    Self::publish_or_discard_export_temp(&temporary, &destination)
                })
                .map(|()| {
                    self.status =
                        format!("Exported .{} to {}", format.ext(), file_label(&destination));
                }),
        };
        if let Err(error) = outcome {
            self.status = format!("Spreadsheet operation failed: {error}");
        }
    }

    fn finish_resource_io(&mut self, result: Result<(), String>) {
        self.finish_resource_output(result.map(|()| tilo_actions::ActionOutput::default()));
    }

    fn finish_resource_output(&mut self, result: Result<tilo_actions::ActionOutput, String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.resource_confirmation = None;
        self.resource_result = Some(match result {
            Ok(output) => tilo_actions::InvocationUpdate::Succeeded { invocation, output },
            Err(message) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "sheets.io_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        });
    }

    /// Cancel the one accepted resource I/O operation. Workers are not
    /// forcibly killed (that is neither portable nor safe around codecs), but
    /// they release the in-progress slot at once, so a worker stuck on a slow
    /// mount cannot refuse every retry; its eventual reply is reaped without
    /// being applied, and any export already prepared is removed before it can
    /// publish.
    fn cancel_resource_io(&mut self, invocation: tilo_actions::InvocationId) -> bool {
        let pending_matches = self
            .pending_resource_io
            .as_ref()
            .is_some_and(|(pending, _)| *pending == invocation);
        if self.resource_invocation != Some(invocation) && !pending_matches {
            return false;
        }
        if pending_matches {
            self.pending_resource_io = None;
        }
        self.io = None;
        self.resource_confirmation = None;
        self.discard_prepared_resource_io();
        if let Some(rx) = self.interchange_rx.take() {
            self.orphaned_interchange.push(rx);
        }
        self.active_interchange_generation = None;
        self.resource_invocation = None;
        self.status = "Spreadsheet operation cancelled".into();
        self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
            invocation,
            message: Some("spreadsheet operation cancelled before it changed data or files".into()),
        });
        true
    }

    fn switch_sheet(&mut self, doc: &mut AutoCommit, idx: usize) {
        let mut out = Vec::new();
        self.grid.flush_edit(&mut out);
        let presentation_only = out.is_empty();
        self.apply_cmds(doc, out);
        self.active_sheet = idx.min(SheetsDoc::sheet_count(doc).saturating_sub(1));
        // The active tab is presentation-only. When this switch did not first
        // flush an edit, its
        // new head has exactly the same search corpus; carry the projection
        // forward so a tab click never re-walks every sheet body.
        if presentation_only {
            self.search_projection_heads = Some(doc.get_heads());
        }
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
        self.active_sheet = n;
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
    fn open_command_palette(&mut self, doc: &AutoCommit) {
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
        self.palette.clear();
        let resources = actions::provide_resources(&self.sheets_resource_ctx(), doc);
        let mut actions = tilo_actions::flatten(&resources);
        actions.sort_by_key(|action| usize::from(action.action.id != "go-to-cell"));
        for action in actions {
            if action.action.id == "commands" {
                continue;
            }
            let mut entry = CommandEntry::from_flat_action(&action);
            if action.action.id == "go-to-cell" {
                entry = entry.with_argument_prefill(active_ref.clone());
            }
            self.palette.push(entry);
        }
        self.palette.open();
        self.pending.redraw = true;
    }

    #[cfg(test)]
    fn dispatch_palette_view_for_test(&mut self, doc: &AutoCommit) {
        let intent = ui::widget::controls::take_control_invocation(self)
            .expect("palette emitted a resource action");
        let resources = Self::publish_resources(self, doc);
        let spec = tilo_actions::door::resolve(&resources, &intent.address)
            .expect("palette address resolves");
        assert_eq!(spec.kind, tilo_actions::OperationKind::View);
        let encoded = spec.build_json(&intent.arguments).expect("valid arguments");
        assert!(
            self.answer_resource_operation(
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
                doc,
                &automerge::ROOT,
            )
            .expect("view operation")
        );
    }

    /// Run the action the palette emitted, dispatching by id. `argument`
    /// carries the free-text a `with_argument` entry collected (e.g. the cell
    /// reference for "Go to cell").
    #[cfg(test)]
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
    #[cfg(test)]
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
        self.perform_action(
            doc,
            &actions::set_cells_action(
                sheet,
                SheetsDoc::sheet_resource_id(doc, sheet).unwrap_or_default(),
                writes,
            ),
        );
        self.reload(doc);
        self.grid
            .select_range_public((c0, target_r), (c1, target_r));
        self.status = format!("Summed selection into row {}", target_r.saturating_add(1));
    }

    #[cfg(test)]
    fn insert_row(&mut self, doc: &mut AutoCommit) {
        let r0 = self.grid.active().1;
        self.perform_action(
            doc,
            &actions::insert_row_action(
                self.active_sheet,
                SheetsDoc::sheet_resource_id(doc, self.active_sheet).unwrap_or_default(),
                r0,
            ),
        );
        self.reload(doc);
        self.status = format!("Inserted row {}", r0.saturating_add(1));
    }

    #[cfg(test)]
    fn delete_row(&mut self, doc: &mut AutoCommit) {
        let r0 = self.grid.active().1;
        self.perform_action(
            doc,
            &actions::delete_row_action(
                self.active_sheet,
                SheetsDoc::sheet_resource_id(doc, self.active_sheet).unwrap_or_default(),
                r0,
            ),
        );
        self.reload(doc);
        self.status = format!("Deleted row {}", r0.saturating_add(1));
    }

    #[cfg(test)]
    fn insert_column(&mut self, doc: &mut AutoCommit) {
        let c0 = self.grid.active().0;
        self.perform_action(
            doc,
            &actions::insert_column_action(
                self.active_sheet,
                SheetsDoc::sheet_resource_id(doc, self.active_sheet).unwrap_or_default(),
                c0,
            ),
        );
        self.reload(doc);
        self.status = format!(
            "Inserted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    #[cfg(test)]
    fn delete_column(&mut self, doc: &mut AutoCommit) {
        let c0 = self.grid.active().0;
        self.perform_action(
            doc,
            &actions::delete_column_action(
                self.active_sheet,
                SheetsDoc::sheet_resource_id(doc, self.active_sheet).unwrap_or_default(),
                c0,
            ),
        );
        self.reload(doc);
        self.status = format!(
            "Deleted column {}",
            crate::model::addr_to_a1(c0, 0).trim_end_matches(|ch: char| ch.is_ascii_digit())
        );
    }

    // --- layout -------------------------------------------------------------

    /// The label painted on a menu-bar button: openers carry a ▾ cue.
    fn menu_button_label(label: &str, opens: bool) -> String {
        if opens {
            format!("{label} \u{25BE}") // ▾
        } else {
            label.to_string()
        }
    }

    /// Every chrome string `relayout` reserves a slot for, so
    /// [`ChromeMetrics::resolve`] can measure each one with the style it is
    /// painted in.
    fn chrome_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = MENU_BAR_BUTTONS
            .iter()
            .map(|(label, opens, _)| Self::menu_button_label(label, opens.is_some()))
            .collect();
        labels.extend(self.sheet_names.iter().cloned());
        labels.extend(
            TAB_MENU_ACTIONS
                .iter()
                .map(|act| tab_action_label(*act).to_owned()),
        );
        labels.push("Colour".to_owned());
        labels.extend(Self::file_menu_items().iter().map(|(l, _)| (*l).to_owned()));
        labels.extend(
            self.format_menu_items()
                .iter()
                .map(|(l, _)| (*l).to_owned()),
        );
        labels.push("Default".to_owned());
        labels.extend(Currency::ALL.iter().map(|c| c.code().to_owned()));
        labels.extend(DATE_STYLES.iter().map(|(l, _)| (*l).to_owned()));
        labels.extend(TIME_STYLES.iter().map(|(l, _)| (*l).to_owned()));
        labels.extend(SETTINGS_SECTIONS.iter().map(|s| (*s).to_owned()));
        labels
    }

    /// Re-resolve the chrome metrics against the renderer about to paint. A
    /// change re-runs `relayout` immediately, so the frame that follows paints
    /// — and later hit-tests — the measured geometry rather than the fallback.
    fn refresh_chrome_metrics(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let labels = self.chrome_labels();
        let metrics = ChromeMetrics::resolve(tr, theme, labels.iter().map(String::as_str));
        if metrics == self.metrics {
            return;
        }
        self.grid.set_metrics(metrics.grid);
        self.metrics = metrics;
        self.relayout();
    }

    fn relayout(&mut self) {
        let r = self.rect;
        let m = &self.metrics;
        self.menubar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.menubar_h());
        self.formula_rect = Rect::new(
            r.x0,
            self.menubar_rect.y1,
            r.x1,
            self.menubar_rect.y1 + m.formula_h(),
        );
        self.tabs_rect = Rect::new(r.x0, r.y1 - m.tabs_h(), r.x1, r.y1);
        self.grid_rect = Rect::new(r.x0, self.formula_rect.y1, r.x1, self.tabs_rect.y0);
        self.grid.rect = self.grid_rect;

        // Menu bar: File ▾  Format ▾  Σ Sum  Doc settings. Each button is its
        // measured label plus `s3` padding, on one centre line, `s2` apart.
        self.menu_buttons.clear();
        let bh = m.menu_button_h();
        let by = self.menubar_rect.y0 + (self.menubar_rect.height() - bh) / 2.0;
        let mut x = self.menubar_rect.x0 + m.s2;
        let compact_toolbar = (m.scale > 1.0 && r.width() < 720.0)
            || MENU_BAR_BUTTONS
                .iter()
                .map(|(label, opens, _)| {
                    m.menu_button_w(&Self::menu_button_label(label, opens.is_some())) + m.s2
                })
                .sum::<f64>()
                > r.width() - 2.0 * m.s2;
        for (index, (label, opens, item)) in MENU_BAR_BUTTONS.into_iter().enumerate() {
            if compact_toolbar && index >= 2 {
                if index == 2 {
                    let w = m.menu_button_w("More ▾");
                    self.menu_buttons.push(MenuButton {
                        rect: Rect::new(x, by, x + w, by + bh),
                        label: "More".into(),
                        item: None,
                        opens: Some(OpenMenu::More),
                    });
                }
                continue;
            }
            let w = m.menu_button_w(&Self::menu_button_label(label, opens.is_some()));
            self.menu_buttons.push(MenuButton {
                rect: Rect::new(x, by, x + w, by + bh),
                label: label.to_string(),
                item,
                opens,
            });
            x += w + m.s2;
        }

        // Formula bar: name box · fx · field, all sharing one centre line and
        // one field height. The "fx" affordance is a slot around its glyph.
        let field_y0 = self.formula_rect.y0 + m.s1;
        let field_y1 = self.formula_rect.y1 - m.s1;
        let nb_x0 = self.formula_rect.x0 + m.s2;
        let nb_x1 = nb_x0 + m.name_box_w();
        self.name_box_rect = Rect::new(nb_x0, field_y0, nb_x1, field_y1);
        self.fx_rect = Rect::new(nb_x1, field_y0, nb_x1 + m.fx_slot_w(), field_y1);
        self.formula_field_rect = Rect::new(
            self.fx_rect.x1,
            field_y0,
            (self.formula_rect.x1 - m.s2).max(self.fx_rect.x1),
            field_y1,
        );

        // Sheet tabs: measured names plus `s3` padding, `s1` apart, with the
        // square "+" button after the last tab.
        self.tab_rects.clear();
        let mut tx = self.tabs_rect.x0 + m.s2;
        let ty = self.tabs_rect.y0 + m.s1;
        let th = m.tab_h();
        for (i, name) in self.sheet_names.iter().enumerate() {
            let w = m.tab_w(name);
            self.tab_rects.push((Rect::new(tx, ty, tx + w, ty + th), i));
            tx += w + m.s1;
        }
        self.add_tab_rect = Rect::new(tx, ty, tx + th, ty + th);
        // An in-flight rename follows its tab through the reflow, so the
        // editor never detaches from the tab it renames.
        if let Some(idx) = self.tab_rename.as_ref().map(|rn| rn.idx) {
            let rect = self.tab_rename_rect(idx);
            if let Some(rn) = &mut self.tab_rename {
                rn.rect = rect;
            }
        }

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
                    let items = Self::file_menu_items();
                    self.layout_dropdown(anchor, &items, FILE_MENU_MIN_W);
                }
            }
            OpenMenu::Format => {
                if let Some(anchor) = opener {
                    let items = self.format_menu_items();
                    self.layout_dropdown(anchor, &items, FORMAT_MENU_MIN_W);
                }
            }
            OpenMenu::More => {
                if let Some(anchor) = opener {
                    let formula_label = if self.grid.formula_view() {
                        "Hide formulas"
                    } else {
                        "Show formulas"
                    };
                    let items = [
                        ("AutoSum", MenuItem::AutoSum),
                        ("Document settings", MenuItem::OpenDocSettings),
                        (formula_label, MenuItem::ToggleFormulaView),
                    ];
                    self.layout_dropdown(anchor, &items, 190.0);
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

        let m = &self.metrics;
        let row_h = m.menu_row_h();
        let pad = m.s2;
        let swatch = TAB_COLOUR_SWATCH;
        let swatch_count = count_f64(TAB_PALETTE_LEN + 1);
        // The panel is as wide as its widest row label (plus the row inset and
        // panel padding on each side) or the swatch strip, never narrower than
        // its classic width.
        let widest = TAB_MENU_ACTIONS
            .iter()
            .map(|act| m.label_w(tab_action_label(*act)))
            .fold(0.0_f64, f64::max);
        let swatch_strip_w = swatch_count * swatch + (swatch_count - 1.0) * m.s1;
        let width = (widest + 2.0 * (pad + m.s2))
            .max(swatch_strip_w + 2.0 * pad)
            .max(TAB_MENU_MIN_W);
        // "Colour" caption (an `xs` line box) sits `s1` above the swatch row.
        let color_block = m.s2 + m.lh_xs + m.s1 + swatch;
        let h = pad * 2.0 + count_f64(TAB_MENU_ACTIONS.len()) * row_h + color_block;

        let x0 = anchor
            .x0
            .min(self.rect.x1 - width - m.s2)
            .max(self.rect.x0 + m.s1);
        let y1 = anchor.y0 - m.s1;
        let y0 = (y1 - h).max(self.rect.y0 + m.s1);
        self.tab_menu_rect = Rect::new(x0, y0, x0 + width, y1);

        let mut y = y0 + pad;
        for act in TAB_MENU_ACTIONS {
            self.tab_menu_rows
                .push((Rect::new(x0 + pad, y, x0 + width - pad, y + row_h), act));
            y += row_h;
        }
        // Colour swatch strip, below the "Colour" caption.
        let cy = y + m.s2 + m.lh_xs + m.s1;
        let mut cx = x0 + pad;
        self.tab_menu_swatches
            .push((Rect::new(cx, cy, cx + swatch, cy + swatch), None));
        cx += swatch + m.s1;
        for i in 0..TAB_PALETTE_LEN as u32 {
            self.tab_menu_swatches
                .push((Rect::new(cx, cy, cx + swatch, cy + swatch), Some(i)));
            cx += swatch + m.s1;
        }
    }

    /// The File menu's rows, in order.
    fn file_menu_items() -> [(&'static str, MenuItem); 7] {
        [
            ("New sheet", MenuItem::NewSheet),
            ("Import CSV", MenuItem::ImportCsv),
            ("Export CSV", MenuItem::ExportCsv),
            ("Import Excel (.xlsx)", MenuItem::ImportXlsx),
            ("Export Excel (.xlsx)", MenuItem::ExportXlsx),
            ("Import Calc (.ods)", MenuItem::ImportOds),
            ("Export Calc (.ods)", MenuItem::ExportOds),
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

    /// Lay out a menu-bar dropdown under `anchor`: measured rows `s2` inside a
    /// panel that grows to its widest label and never below `min_width`.
    fn layout_dropdown(&mut self, anchor: Rect, items: &[(&str, MenuItem)], min_width: f64) {
        let m = &self.metrics;
        let row_h = m.menu_row_h();
        let pad = m.s2;
        let widest = items
            .iter()
            .map(|(label, _)| m.label_w(label))
            .fold(0.0_f64, f64::max);
        let width = (widest + 2.0 * (pad + m.s2))
            .max(min_width)
            .min((self.rect.width() - 2.0 * m.s1).max(1.0));
        let x0 = anchor
            .x0
            .min(self.rect.x1 - width - m.s1)
            .max(self.rect.x0 + m.s1);
        let y0 = anchor.y1 + m.s1;
        let h = count_f64(items.len()) * row_h + pad * 2.0;
        self.dropdown_rect = Rect::new(x0, y0, x0 + width, y0 + h);
        let mut y = y0 + pad;
        for (_, item) in items {
            let rect = Rect::new(x0 + pad, y, x0 + width - pad, y + row_h);
            self.dropdown_rows.push(ClickRow {
                rect,
                item: item.clone(),
            });
            y += row_h;
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the settings modal lays out its sections top to bottom sharing the running y cursor and chip-row wrapping"
    )]
    fn layout_settings_panel(&mut self) {
        // Centered modal panel, sized to fit even the narrow viewport. Chip
        // rows start past the measured section-label column and wrap within
        // the panel width; the panel's height follows its content so every
        // section keeps the same rhythm at every text scale.
        let m = &self.metrics;
        let pad = m.s6;
        let pw = SETTINGS_PANEL_W
            .min(self.rect.width() - 2.0 * m.s4)
            .max(SETTINGS_PANEL_MIN_W);
        let px0 = self.rect.center().x - pw / 2.0;
        let chip_h = m.chip_h();
        let chip_gap = m.s2;
        let label_col = SETTINGS_SECTIONS
            .iter()
            .map(|s| m.label_w(s))
            .fold(0.0_f64, f64::max);
        let left = px0 + pad;
        let row_start = left + label_col + m.s4;
        let wrap_x = px0 + pw - pad;

        // Content height first, so the panel can be centred on it: title,
        // `s6`, then each chip section separated by `s4`, then the footer.
        self.setting_chips.clear();
        let mut chips: Vec<SettingChip> = Vec::new();
        let mut y = pad + m.lh_base + m.s6;
        let push = |chips: &mut Vec<SettingChip>, x: &mut f64, y: &mut f64, label: &str, kind| {
            let w = m.chip_w(label);
            if *x + w > wrap_x && *x > row_start {
                *x = row_start;
                *y += chip_h + chip_gap;
            }
            chips.push(SettingChip {
                rect: Rect::new(*x, *y, *x + w, *y + chip_h),
                kind,
            });
            *x += w + chip_gap;
        };

        // Section: currency. First an explicit "Default" (inherit global) chip.
        self.settings_section_y[0] = y;
        {
            let mut x = row_start;
            push(
                &mut chips,
                &mut x,
                &mut y,
                "Default",
                SettingKind::Currency(None),
            );
            for c in Currency::ALL {
                push(
                    &mut chips,
                    &mut x,
                    &mut y,
                    c.code(),
                    SettingKind::Currency(Some(c)),
                );
            }
        }
        y += chip_h + m.s4;

        // Section: date style.
        self.settings_section_y[1] = y;
        {
            let mut x = row_start;
            push(
                &mut chips,
                &mut x,
                &mut y,
                "Default",
                SettingKind::Date(None),
            );
            for (label, d) in DATE_STYLES {
                push(
                    &mut chips,
                    &mut x,
                    &mut y,
                    label,
                    SettingKind::Date(Some(d)),
                );
            }
        }
        y += chip_h + m.s4;

        // Section: time style.
        self.settings_section_y[2] = y;
        {
            let mut x = row_start;
            push(
                &mut chips,
                &mut x,
                &mut y,
                "Default",
                SettingKind::Time(None),
            );
            for (label, t) in TIME_STYLES {
                push(
                    &mut chips,
                    &mut x,
                    &mut y,
                    label,
                    SettingKind::Time(Some(t)),
                );
            }
        }
        // Footer note (an `xs` line box) sits `s6` under the last row.
        let ph = y + chip_h + m.s6 + m.lh_xs + pad;

        let py0 = self.rect.center().y - ph / 2.0;
        self.panel_rect = Rect::new(px0, py0, px0 + pw, py0 + ph);
        for section_y in &mut self.settings_section_y {
            *section_y += py0;
        }
        self.setting_chips = chips
            .into_iter()
            .map(|chip| SettingChip {
                rect: chip.rect + vello::kurbo::Vec2::new(0.0, py0),
                kind: chip.kind,
            })
            .collect();
    }

    // --- drawing ------------------------------------------------------------

    fn draw_chip(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        metrics: &ChromeMetrics,
        rect: Rect,
        label: &str,
        style: ChipStyle,
    ) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.sm);
        // Rest: the raised face with its derived edge (HIG §9.5 "Button
        // Neutral"); the AutoSum chip composites its accent wash over that
        // face first so the edge is measured against what is on screen.
        // Active: the accent fill alone — a chosen accent carries no hairline.
        if style.active {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5],
                None,
                &RoundedRect::from_rect(rect, radius),
            );
        } else {
            let face = if style.accent {
                colors
                    .accent_chip_on(
                        colors.surfaces.raised,
                        ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
                    )
                    .fill
            } else {
                colors.surfaces.raised
            };
            ui::surface::card(scene, transform, rect, radius, face);
        }
        let size = metrics.sm;
        let (tw, _) = tr.measure(label, size, None);
        let tx = rect.center().x - f64::from(tw) / 2.0;
        let tyy = ChromeMetrics::centred_top(rect, metrics.lh_sm);
        let color = if style.active {
            colors.on_accent(ui::tokens::colors::NON_TEXT_MIN_CONTRAST)
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
                (Some(OpenMenu::File), OpenMenu::File)
                    | (Some(OpenMenu::Format), OpenMenu::Format)
                    | (Some(OpenMenu::More), OpenMenu::More)
            );
            let accent = matches!(b.item, Some(MenuItem::AutoSum));
            let label = Self::menu_button_label(&b.label, b.opens.is_some());
            Self::draw_chip(
                scene,
                transform,
                tr,
                theme,
                &self.metrics,
                b.rect,
                &label,
                ChipStyle { active, accent },
            );
        }

        // Bar rule: the bar's own derived edge on its last pixel row (a
        // stroke centred on the boundary straddled two rows and blurred; the
        // declared `border_standard` was invisible on the dark raised bar).
        scene.fill(
            Fill::NonZero,
            transform,
            ui::surface::edge_on(colors.surfaces.raised),
            None,
            &Rect::new(
                self.menubar_rect.x0,
                self.menubar_rect.y1 - 1.0,
                self.menubar_rect.x1,
                self.menubar_rect.y1,
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
        let nb_radius = f64::from(theme.tokens.radii.sm);
        // Rest: the raised face with its derived edge; editing: the accent
        // ring (focus state, not a second edge).
        ui::surface::card(
            scene,
            transform,
            name_box,
            nb_radius,
            colors.surfaces.raised,
        );
        if nb_editing {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.accent[5],
                None,
                &RoundedRect::from_rect(name_box.inset(-1.0), (nb_radius - 1.0).max(0.0)),
            );
        }
        let sm = self.metrics.sm;
        let nb_text_x = name_box.x0 + self.metrics.s2;
        let nb_text_y = ChromeMetrics::centred_top(name_box, self.metrics.lh_sm);
        if let Some(input) = &mut self.name_box_edit {
            // Keep the compact spreadsheet chrome, while the shared editor
            // owns shaping, scrolling, selection, clipboard, IME and caret
            // hit-testing inside its content band. The editor takes the
            // renderer-input size, so its text lands at the same final size as
            // the static label it replaces.
            input.set_font_size(sm);
            input.apply_layout_external(Rect::new(
                nb_text_x,
                name_box.y0 + NAME_BOX_EDITOR_INSET,
                name_box.x1 - self.metrics.s1,
                name_box.y1 - NAME_BOX_EDITOR_INSET,
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
        let base = self.metrics.base;
        let text_x0 = self.formula_field_rect.x0 + self.metrics.s2;
        // Rebuild the per-character x boundaries for click/caret mapping. This
        // is derived from exactly the text being drawn, so a click maps to the
        // same glyph the user sees (whether it's the live edit buffer or the
        // active cell's stored input).
        self.formula_char_x = char_boundaries(tr, &content, base, text_x0);
        let origin = Point::new(
            text_x0,
            ChromeMetrics::centred_top(self.formula_field_rect, self.metrics.lh_base),
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
        // The glyph is centred in the same `fx_rect` that `handle_event` hit-tests,
        // so what the pointer lands on is exactly what is painted.
        let colors = &theme.tokens.colors;
        let m = &self.metrics;
        tr.draw_style(
            scene,
            "fx",
            colors.text_dim,
            transform
                * Affine::translate((
                    self.fx_rect.center().x - m.fx_w / 2.0,
                    ChromeMetrics::centred_top(self.fx_rect, m.lh_base),
                )),
            None,
            &TextStyle::new(m.base).with_italic(true),
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
        // A text field: its fill with the derived rest edge (HIG §9.5
        // "TextInput"); editing takes the accent ring instead.
        let field = ff_shape.rect();
        let radius = ff_shape.radii().top_left;
        ui::surface::card(scene, transform, field, radius, colors.surfaces.base);
        if editing {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                colors.accent[5],
                None,
                &RoundedRect::from_rect(field.inset(-1.0), (radius - 1.0).max(0.0)),
            );
        }
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
            // The highlight spans exactly the text's line box.
            let top = ChromeMetrics::centred_top(self.formula_field_rect, self.metrics.lh_base);
            let sel_rect = Rect::new(x0, top, x1, top + self.metrics.lh_base);
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
                scene, transform, tr, content, base, theme, origin, None,
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
            let top = ChromeMetrics::centred_top(self.formula_field_rect, self.metrics.lh_base);
            scene.stroke(
                &Stroke::new(1.5),
                transform,
                colors.accent[5],
                None,
                &Line::new((cx, top), (cx, top + self.metrics.lh_base)),
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
        let m = &self.metrics;
        let sm = m.sm;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &self.tabs_rect,
        );
        // Bar rule on the strip's first pixel row: the strip's derived edge.
        scene.fill(
            Fill::NonZero,
            transform,
            ui::surface::edge_on(colors.surfaces.raised),
            None,
            &Rect::new(
                self.tabs_rect.x0,
                self.tabs_rect.y0,
                self.tabs_rect.x1,
                self.tabs_rect.y0 + 1.0,
            ),
        );
        for (rect, idx) in &self.tab_rects {
            let active = *idx == self.active_sheet;
            // Sheet tabs are selectors (HIG §9.5): idle is the bare label on
            // the strip, the active one lifts to the base tier — a contrast
            // step against the raised strip — under its accent underline.
            if active {
                let shape = RoundedRect::from_rect(*rect, f64::from(theme.tokens.radii.sm));
                scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &shape);
            }
            // The tab's colour underline: its assigned palette colour when it
            // has one, otherwise the accent bar for the active tab.
            let tab_color = self
                .sheet_colors
                .get(*idx)
                .copied()
                .flatten()
                .map(|i| tab_palette_color(theme, i));
            let bar = tab_color.or(if active { Some(colors.accent[5]) } else { None });
            if let Some(bar_color) = bar {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    bar_color,
                    None,
                    &Rect::new(
                        rect.x0 + m.s1,
                        rect.y1 - TAB_UNDERLINE_H,
                        rect.x1 - m.s1,
                        rect.y1,
                    ),
                );
            }
            let full_name = self.sheet_names.get(*idx).cloned().unwrap_or_default();
            let weight = if active {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            };
            // A name longer than the tab's ceiling is elided with the painting
            // style, so it never runs under the neighbouring tab.
            let name = tr.elide_to_width_weighted(
                &full_name,
                sm,
                (rect.width() - 2.0 * m.s3).max(0.0) as f32,
                weight,
            );
            let (tw, _) = tr.measure_weighted(&name, sm, None, weight);
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
                transform * Affine::translate((tx, ChromeMetrics::centred_top(*rect, m.lh_sm))),
                None,
                weight,
            );
        }
        // The add-sheet button is a Neutral control: its face with the
        // derived rest edge.
        ui::surface::card(
            scene,
            transform,
            self.add_tab_rect,
            f64::from(theme.tokens.radii.sm),
            colors.surfaces.base,
        );
        // The "+" is centred on its measured glyph box, not nudged by hand.
        let (add_w, add_h) = m.add_glyph;
        tr.draw_text(
            scene,
            "+",
            m.lg,
            colors.text_dim,
            transform
                * Affine::translate((
                    self.add_tab_rect.center().x - add_w / 2.0,
                    ChromeMetrics::centred_top(self.add_tab_rect, add_h),
                )),
            None,
        );
    }

    /// Bottom edge of the floating status bubble: `s2` above the grid's
    /// horizontal scrollbar rail, so the pill floats over the body's
    /// bottom-right corner and never sits on the scrollbar or the sheet tabs.
    fn status_bubble_bottom(&self) -> f64 {
        self.grid_rect.y1 - Grid::scrollbar_thickness() - self.metrics.s2
    }

    /// Size of the floating status pill for `label`. The shared pill sizes
    /// itself from the renderer-input `sm`, which is right for its width but
    /// leaves its height at the 1× value under a larger text scale, so the
    /// measured `sm` line box plus `s2` above and below is the floor here.
    /// (Its label centring still assumes the nominal size — see the F2 note
    /// in the A7 review.)
    fn status_pill_size(&self, tr: &mut TextRenderer, label: &str) -> (f64, f64) {
        let (w, shared_h) = ui::status_bubble::pill_size(label, tr, self.metrics.sm, 0.0);
        (w, shared_h.max(self.metrics.lh_sm + 2.0 * self.metrics.s2))
    }

    /// Draw the transient floating status bubble at the bottom-right of the
    /// grid area, floated just *above* the scrollbar rail and sheet-tab strip
    /// so it never overlaps either. It shows the composed status text
    /// (selection summary / active format, plus any fresh action message) when
    /// that text changes or while the pointer hovers the bottom-right reveal
    /// band, then fades. All colours come from the theme (via `draw_pill`).
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let Some(label) = self.status_bubble_label() else {
            return;
        };
        // The pill takes the renderer-input `sm` size, so its text lands at
        // the same final size as the rest of the chrome at every text scale.
        let text_size = self.metrics.sm;
        let (w, h) = self.status_pill_size(tr, &label);
        let margin = self.metrics.s3;
        let y1 = self.status_bubble_bottom();
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
        let alpha = if self.motion == MotionPreference::Reduced {
            // Reduced motion keeps the useful acknowledgement, but never
            // animates its opacity. It disappears on the next ordinary draw
            // once its linger period has elapsed.
            if self.status_bubble.is_visible(now) {
                1.0
            } else {
                0.0
            }
        } else {
            self.status_bubble.alpha(now)
        };
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

    /// A non-blocking veil while a persistent workbook is decoded on the
    /// capacity-one worker. The normal chrome remains visible so the launch
    /// feels intentional, but the grid cannot be edited until its CRDT receipt
    /// has been merged. This is deliberately drawn from state rather than a
    /// timer, so reduced motion and a slow disk behave identically.
    fn draw_loading_veil(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let loading = (self.run.persists() && self.persistence_started && !self.pending.seeded)
            || self.shots_loading;
        if !loading {
            return;
        }
        let veil = self.grid_rect;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base.with_alpha_factor(0.88),
            None,
            &veil,
        );
        let label = "Opening workbook…";
        let size = self.metrics.base;
        let (width, height) = tr.measure(label, size, None);
        let origin = Point::new(
            veil.center().x - f64::from(width) / 2.0,
            veil.center().y - f64::from(height) / 2.0,
        );
        tr.draw_text(
            scene,
            label,
            size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((origin.x, origin.y)),
            None,
        );
    }

    /// Render the active sheet's small, data-derived chart collection. Charts
    /// intentionally sample only their selected rectangle and cap the plotted
    /// points, so a broad range cannot turn a paint into an unbounded formula
    /// or path build.
    #[allow(
        clippy::too_many_lines,
        reason = "the bounded chart card paint keeps layout, hit targets, and both chart renderers in one frame-local path"
    )]
    fn draw_charts(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(cache) = &self.cache else { return };
        let max_cards = 3usize;
        // Rows follow the measured `sm`/`xs` line boxes (1× floors), so a
        // scaled title never paints under the action band below it.
        let rows = self.metrics.chart_card_rows();
        // The cards float over the sheet body, `s3` inside the header bands
        // and the scrollbar lane (never on either), stacked `s3` apart.
        let (body_top, body_right) = self.annotation_body_bounds();
        let pad = self.metrics.s3;
        for (index, chart) in self.sheet_charts.iter().take(max_cards).enumerate() {
            let width = self.grid_rect.width().clamp(180.0, 300.0);
            let height = rows.height;
            let x1 = body_right - pad;
            let y0 = body_top + pad + index as f64 * (height + pad);
            if y0 + height > self.grid_rect.y1 {
                break;
            }
            let card = Rect::new(x1 - width, y0, x1, y0 + height);
            ui::surface::card(
                scene,
                transform,
                card,
                f64::from(theme.tokens.radii.md),
                theme.tokens.colors.surfaces.raised,
            );
            // The title is elided to the card so a long or scaled name ends
            // inside the frame rather than over the sheet beside it.
            let title = tr.elide_to_width_weighted(
                &chart.title,
                self.metrics.sm,
                (card.width() - 2.0 * pad) as f32,
                TextWeight::Regular,
            );
            tr.draw_text(
                scene,
                &title,
                self.metrics.sm,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((card.x0 + pad, card.y0 + rows.title_top)),
                None,
            );
            let controls = Rect::new(
                card.x0 + pad,
                card.y0 + rows.controls_y0,
                card.x1 - pad,
                card.y0 + rows.controls_y1,
            );
            let actions = [
                ("Open", AnnotationClick::Open(chart.start)),
                ("Edit", AnnotationClick::EditChart(chart.id.clone())),
                ("Delete", AnnotationClick::DeleteChart(chart.id.clone())),
            ];
            let action_width = controls.width() / actions.len() as f64;
            for (index, (label, action)) in actions.into_iter().enumerate() {
                let button = Rect::new(
                    controls.x0 + index as f64 * action_width,
                    controls.y0,
                    controls.x0 + (index + 1) as f64 * action_width,
                    controls.y1,
                );
                let (label_width, _) = tr.measure(label, self.metrics.xs, None);
                tr.draw_text(
                    scene,
                    label,
                    self.metrics.xs,
                    theme.tokens.colors.accent[5],
                    transform
                        * Affine::translate((
                            button.center().x - f64::from(label_width) / 2.0,
                            ChromeMetrics::centred_top(button, self.metrics.lh_xs),
                        )),
                    None,
                );
                self.annotation_clicks.push((button, action));
            }
            let plot = Rect::new(
                card.x0 + pad,
                card.y0 + rows.plot_y0,
                card.x1 - pad,
                card.y0 + rows.plot_y1,
            );
            let mut points = Vec::new();
            for row in chart.start.1..=chart.end.1 {
                for col in chart.start.0..=chart.end.0 {
                    if let Some(CellValue::Number(value)) = cache.values.get(&(col, row)) {
                        points.push(*value);
                    }
                    if points.len() == 48 {
                        break;
                    }
                }
                if points.len() == 48 {
                    break;
                }
            }
            if points.is_empty() {
                tr.draw_text(
                    scene,
                    "Select numeric cells to plot",
                    self.metrics.sm,
                    theme.tokens.colors.text_tertiary,
                    transform
                        * Affine::translate((
                            plot.x0,
                            ChromeMetrics::centred_top(plot, self.metrics.lh_sm),
                        )),
                    None,
                );
                continue;
            }
            let max = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let min = points.iter().copied().fold(f64::INFINITY, f64::min);
            let span = (max - min).abs().max(1.0);
            let accent = theme.tokens.colors.accent[5];
            match chart.kind {
                ChartKind::Column => {
                    for (i, value) in points.iter().enumerate() {
                        let step = plot.width() / points.len() as f64;
                        let h = ((value - min) / span * plot.height()).max(2.0);
                        let bar = Rect::new(
                            plot.x0 + i as f64 * step + 2.0,
                            plot.y1 - h,
                            plot.x0 + (i + 1) as f64 * step - 2.0,
                            plot.y1,
                        );
                        scene.fill(Fill::NonZero, transform, accent, None, &bar);
                    }
                }
                ChartKind::Line => {
                    for (i, pair) in points.windows(2).enumerate() {
                        let [first, second] = pair else { continue };
                        let step = if points.len() > 1 {
                            plot.width() / (points.len() - 1) as f64
                        } else {
                            0.0
                        };
                        let point = |offset: usize, value: f64| {
                            Point::new(
                                plot.x0 + (i + offset) as f64 * step,
                                plot.y1 - (value - min) / span * plot.height(),
                            )
                        };
                        scene.stroke(
                            &Stroke::new(2.0),
                            transform,
                            accent,
                            None,
                            &Line::new(point(0, *first), point(1, *second)),
                        );
                    }
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the compact threaded-comment card deliberately co-locates visible state and its bounded click targets"
    )]
    fn draw_comment_threads(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(thread) = self.sheet_comments.first() else {
            return;
        };
        let width = self.grid_rect.width().clamp(200.0, 310.0);
        // Rows follow the measured `sm`/`xs` line boxes (1× floors), so the
        // heading, body and detail lines stay clear of each other and of the
        // action band at every text scale.
        let rows = self.metrics.comment_card_rows();
        // The card floats over the sheet body, `s3` inside the row and column
        // header bands, so it never covers a header label.
        let (body_top, _) = self.annotation_body_bounds();
        let pad = self.metrics.s3;
        let x0 = self.grid_rect.x0 + self.metrics.grid.row_header_w() + pad;
        let y0 = body_top + pad;
        let card = Rect::new(x0, y0, x0 + width, y0 + rows.height);
        let text_w = (card.width() - 2.0 * pad) as f32;
        // A card like any other: raised face, derived edge. The accent stays
        // on the live parts (the reply count / resolve affordance), not on
        // the frame — accent means live, not selected (HIG §9.4).
        ui::surface::card(
            scene,
            transform,
            card,
            f64::from(theme.tokens.radii.md),
            theme.tokens.colors.surfaces.raised,
        );
        let range = if thread.start == thread.end {
            crate::model::addr_to_a1(thread.start.0, thread.start.1)
        } else {
            format!(
                "{}:{}",
                crate::model::addr_to_a1(thread.start.0, thread.start.1),
                crate::model::addr_to_a1(thread.end.0, thread.end.1)
            )
        };
        let heading = format!(
            "{} · {}{}",
            range,
            thread.author,
            if thread.resolved { " · Resolved" } else { "" }
        );
        // Heading and body are elided to the card: a scaled or long line ends
        // inside the frame instead of running over the grid beside it.
        let heading =
            tr.elide_to_width_weighted(&heading, self.metrics.sm, text_w, TextWeight::Regular);
        tr.draw_text(
            scene,
            &heading,
            self.metrics.sm,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + pad, card.y0 + rows.heading_top)),
            None,
        );
        let body: String = thread.body.chars().take(56).collect();
        let body = tr.elide_to_width_weighted(&body, self.metrics.sm, text_w, TextWeight::Regular);
        tr.draw_text(
            scene,
            &body,
            self.metrics.sm,
            theme.tokens.colors.text_tertiary,
            transform * Affine::translate((card.x0 + pad, card.y0 + rows.body_top)),
            None,
        );
        let detail = format!(
            "{} {} · {}",
            thread.replies.len(),
            if thread.replies.len() == 1 {
                "reply"
            } else {
                "replies"
            },
            if thread.resolved { "Reopen" } else { "Resolve" }
        );
        tr.draw_text(
            scene,
            &detail,
            self.metrics.xs,
            theme.tokens.colors.accent[5],
            transform * Affine::translate((card.x0 + pad, card.y0 + rows.detail_top)),
            None,
        );
        let controls = Rect::new(
            card.x0 + pad,
            card.y0 + rows.controls_y0,
            card.x1 - pad,
            card.y0 + rows.controls_y0 + ANNOTATION_ACTION_H,
        );
        let actions = [
            ("Reply", AnnotationClick::Reply(thread.id.clone())),
            ("Edit", AnnotationClick::EditComment(thread.id.clone())),
            ("Open", AnnotationClick::Open(thread.start)),
            ("Delete", AnnotationClick::DeleteComment(thread.id.clone())),
        ];
        let action_width = controls.width() / actions.len() as f64;
        for (index, (label, action)) in actions.into_iter().enumerate() {
            let button = Rect::new(
                controls.x0 + index as f64 * action_width,
                controls.y0,
                controls.x0 + (index + 1) as f64 * action_width,
                controls.y1,
            );
            let (label_width, _) = tr.measure(label, self.metrics.xs, None);
            tr.draw_text(
                scene,
                label,
                self.metrics.xs,
                theme.tokens.colors.accent[5],
                transform
                    * Affine::translate((
                        button.center().x - f64::from(label_width) / 2.0,
                        ChromeMetrics::centred_top(button, self.metrics.lh_xs),
                    )),
                None,
            );
            self.annotation_clicks.push((button, action));
        }
        // The state-changing resolve/reopen affordance has its own normal
        // target height, centred on the detail line it labels, and remains
        // distinct from the action band (it ends where that band begins).
        self.annotation_clicks.push((
            Rect::new(
                card.x0 + pad,
                card.y0 + rows.toggle_y0,
                card.x1 - pad,
                card.y0 + rows.toggle_y1,
            ),
            AnnotationClick::ToggleComment(thread.id.clone(), !thread.resolved),
        ));
    }

    /// Where a floating annotation card may sit: the top of the sheet body
    /// (below the column-header band) and its right edge (left of the
    /// vertical scrollbar lane). Cards keep `s3` inside both.
    fn annotation_body_bounds(&self) -> (f64, f64) {
        (
            self.grid_rect.y0 + self.metrics.grid.col_header_h(),
            self.grid_rect.x1 - Grid::scrollbar_thickness(),
        )
    }

    /// Paint a floating menu / dialog panel (HIG §9.1 Panel role): elevation,
    /// the panel ground and the panel border — never a raised fill under a
    /// declared `border_standard` with a literal black shadow.
    fn paint_panel(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect, radius: f64) {
        ui::chrome::panel_shadow(scene, transform, rect, radius, theme, 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::panel_ground(theme),
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(rect, radius),
        );
    }

    /// Paint a menu row's chosen / hovered state as a lift (HIG §9.3): the
    /// selection tier's fill and, when lifted, its hairline — never an
    /// accent wash, which is reserved for live state. `rect` is the row's
    /// full hit rect; the lift is inset half the row seam top and bottom so
    /// neighbouring lifts stay `s1` apart while the hit rects stay contiguous.
    fn paint_row_state(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        state: ui::chrome::SelectionState,
    ) {
        let tier = ui::chrome::selection_tier(theme, state);
        if tier.fill.components[3] <= 0.0 {
            return;
        }
        let rect = rect.inset((0.0, -MENU_ROW_SEAM / 2.0));
        let radius = f64::from(theme.tokens.radii.sm);
        scene.fill(
            Fill::NonZero,
            transform,
            tier.fill,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        if let Some(border) = tier.border {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                border,
                None,
                &ui::surface::edge_outline(rect, radius),
            );
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
        Self::paint_panel(
            scene,
            transform,
            theme,
            self.dropdown_rect,
            f64::from(theme.tokens.radii.md),
        );
        let sm = self.metrics.sm;
        let active_cat = self.current_active_format.category();
        for row in &self.dropdown_rows {
            let hovered = row.rect.contains(self.last_mouse);
            // The row matching the active cell's format is the chosen one.
            let matches_active = match &row.item {
                MenuItem::SetFormat(f) => f.category() == active_cat,
                _ => false,
            };
            Self::paint_row_state(
                scene,
                transform,
                theme,
                row.rect,
                ui::chrome::SelectionState::new(matches_active, hovered, false),
            );
            let label = menu_item_label(&row.item);
            tr.draw_text(
                scene,
                label,
                sm,
                colors.text_primary,
                transform
                    * Affine::translate((
                        row.rect.x0 + self.metrics.s2,
                        ChromeMetrics::centred_top(row.rect, self.metrics.lh_sm),
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
        // Dim the backdrop in the theme's own scrim, then the dialog panel.
        ui::chrome::fill_scrim(scene, transform, self.rect, SETTINGS_SCRIM_STRENGTH, theme);
        Self::paint_panel(
            scene,
            transform,
            theme,
            self.panel_rect,
            f64::from(theme.tokens.radii.lg),
        );

        let m = &self.metrics;
        let pad = m.s6;
        let title = "Document settings";
        tr.draw_text_weighted(
            scene,
            title,
            m.base,
            colors.text_primary,
            transform * Affine::translate((self.panel_rect.x0 + pad, self.panel_rect.y0 + pad)),
            None,
            TextWeight::Bold,
        );

        // Section labels, centred on the first chip row of their section (the
        // row Y captured at layout time, so they stay put when a row wraps).
        let label_x = self.panel_rect.x0 + pad;
        let chip_h = m.chip_h();
        for (label, &section_y) in SETTINGS_SECTIONS.iter().zip(&self.settings_section_y) {
            let row = Rect::new(label_x, section_y, label_x, section_y + chip_h);
            tr.draw_text(
                scene,
                label,
                m.sm,
                colors.text_dim,
                transform * Affine::translate((label_x, ChromeMetrics::centred_top(row, m.lh_sm))),
                None,
            );
        }

        self.draw_setting_chips(scene, transform, tr, theme, m.sm);

        // Footer: how the currently active cell renders under these settings.
        let footer = format!(
            "Applies to this workbook. Active cell: {}",
            self.current_active_format.label()
        );
        tr.draw_text(
            scene,
            &footer,
            m.xs,
            colors.text_dim,
            transform
                * Affine::translate((self.panel_rect.x0 + pad, self.panel_rect.y1 - pad - m.lh_xs)),
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
        let radius = f64::from(theme.tokens.radii.sm);
        for chip in &self.setting_chips {
            let (label, selected) = self.chip_label_and_state(chip.kind);
            // Quiet chip: raised face with its derived edge; selected: the
            // accent fill, no edge (HIG §9.5 "Chip quiet").
            if selected {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent[5],
                    None,
                    &RoundedRect::from_rect(chip.rect, radius),
                );
            } else {
                ui::surface::card(scene, transform, chip.rect, radius, colors.surfaces.raised);
            }
            let (tw, _) = tr.measure(&label, sm, None);
            let tx = chip.rect.center().x - f64::from(tw) / 2.0;
            let ty = ChromeMetrics::centred_top(chip.rect, self.metrics.lh_sm);
            let color = if selected {
                colors.on_accent(ui::tokens::colors::NON_TEXT_MIN_CONTRAST)
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
        Self::paint_panel(
            scene,
            transform,
            theme,
            self.tab_menu_rect,
            f64::from(theme.tokens.radii.md),
        );

        let sm = self.metrics.sm;
        for (rect, act) in &self.tab_menu_rows {
            Self::paint_row_state(
                scene,
                transform,
                theme,
                *rect,
                ui::chrome::SelectionState::new(false, rect.contains(self.last_mouse), false),
            );
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
                        rect.x0 + self.metrics.s2,
                        ChromeMetrics::centred_top(*rect, self.metrics.lh_sm),
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
            // The caption's line box ends `s1` above the swatch row, matching
            // the block `layout_tab_menu` reserved for it.
            tr.draw_text(
                scene,
                "Colour",
                self.metrics.xs,
                colors.text_dim,
                transform
                    * Affine::translate((
                        self.tab_menu_rect.x0 + 2.0 * self.metrics.s2,
                        first.y0 - self.metrics.s1 - self.metrics.lh_xs,
                    )),
                None,
            );
        }
        let current = self
            .tab_menu_target
            .and_then(|t| self.sheet_colors.get(t).copied().flatten());
        for (rect, color_opt) in &self.tab_menu_swatches {
            let radius = f64::from(theme.tokens.radii.sm);
            let shape = RoundedRect::from_rect(*rect, radius);
            match color_opt {
                None => {
                    ui::surface::card(scene, transform, *rect, radius, colors.surfaces.base);
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
                        tab_palette_color(theme, *i),
                        None,
                        &shape,
                    );
                }
            }
            let selected = current == *color_opt && color_opt.is_some();
            if selected || rect.contains(self.last_mouse) {
                // The chosen swatch's ring is the chrome ink, which contrasts
                // the panel on both palettes (a literal white ring vanished
                // on the light panel); hover keeps the accent.
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    if selected {
                        colors.text_primary
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
        // before the draw pass had a Theme reference; the shared editor takes
        // the renderer-input size so it lands at the tab label's final size.
        rn.editor.set_font_size(self.metrics.sm);
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
        MenuItem::ToggleFormulaView => "Show/hide formulas",
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

/// Flatten a cell's display text into a single TSV field: tabs and newlines
/// would break the row/column structure, so they collapse to spaces.
fn tsv_cell(display: &str) -> String {
    display.replace(['\t', '\n', '\r'], " ")
}

/// Where the workbook is persisted between sessions:
/// `$XDG_DATA_HOME/tilo/sheets/workbook.automerge` (falls back to
/// `~/.local/share`).
fn workbook_store() -> (tilo_files::Location, tilo_files::VPath) {
    let location = tilo_paths::env_dir("XDG_DATA_HOME").map_or_else(
        || tilo_paths::AppPaths::for_app("tilo").data(),
        |base| tilo_files::Location::Native(base.join("tilo")),
    );
    let name = tilo_files::VPath::new("sheets/workbook.automerge").unwrap_or_default();
    (location, name)
}

#[cfg(test)]
fn workbook_path() -> Option<PathBuf> {
    let (location, name) = workbook_store();
    Some(location.native_path().ok()?.join(name.as_str()))
}

/// Run `job` on a worker thread, or at once where there are no threads (a
/// browser page, where the app's files answer from memory).
fn run_worker(name: &str, job: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    if matches!(
        ui::rt::availability(ui::rt::Feature::Threads),
        ui::rt::Availability::Unsupported(_)
    ) {
        job();
        return Ok(());
    }
    ui::rt::spawn_thread(name, job).map(drop)
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
fn import_source(path: &std::path::Path) -> String {
    tilo_files::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
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
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::undo::UndoPoints
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[actions::SHEETS_OPERATION_TYPE]
    }

    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::cell_actions::READERS
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested {
            let _ = self.cancel_resource_io(invocation);
        }
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            if self.resource_invocation == Some(invocation) {
                self.resource_confirmation = Some(invocation);
                // Settling needs a mutable `sync`, which an idle host runs
                // only when woken; polling alone schedules no frame.
                if self.prepared_resource_io.is_some() {
                    self.interchange_wake.wake();
                }
            }
            None
        }
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        if self.run == RunMode::Record(ui::launch::RecordLaunch::Joined)
            && SheetsDoc::sheet_count_readonly(state) == 0
        {
            // The creator's workbook has not arrived. Reading through the
            // get-or-create accessors now would write a second `wb` that
            // competes with the creator's in the merge.
            return;
        }
        if !self.pending.seeded {
            if !self.persistence_started {
                // Pull the shared global defaults once at startup.
                self.global = GlobalSettings::load();
                self.persistence_started = true;
                if self.run.persists() {
                    if let Err(error) = self.start_persistence_load(state.get_heads()) {
                        self.status = format!("Could not start workbook load: {error}");
                        self.last_persist_failure = Some(ui::time::now());
                        self.finish_initialize(state, None);
                    }
                } else {
                    self.finish_initialize(state, None);
                }
            }
            self.drain_persistence_worker(state);
            // Keep startup responsive: layout can render its normal loading
            // state while the bounded worker reads and decodes a large CRDT.
            if !self.pending.seeded {
                return;
            }
        }
        if let Some((invocation, operation)) = self.pending_resource_io.take() {
            let path = match &operation {
                actions::SheetsOperation::ImportCsv { path }
                | actions::SheetsOperation::ImportXlsx { path }
                | actions::SheetsOperation::ImportOds { path }
                | actions::SheetsOperation::ExportCsv { path }
                | actions::SheetsOperation::ExportXlsx { path }
                | actions::SheetsOperation::ExportOds { path } => {
                    std::path::PathBuf::from(path.as_deref().unwrap_or_default())
                }
                _ => std::path::PathBuf::new(),
            };
            let pending = match operation {
                actions::SheetsOperation::ImportCsv { .. } => PendingIo::Import(Interchange::Csv),
                actions::SheetsOperation::ImportXlsx { .. } => PendingIo::Import(Interchange::Xlsx),
                actions::SheetsOperation::ImportOds { .. } => PendingIo::Import(Interchange::Ods),
                actions::SheetsOperation::ExportCsv { .. } => PendingIo::Export(Interchange::Csv),
                actions::SheetsOperation::ExportXlsx { .. } => PendingIo::Export(Interchange::Xlsx),
                actions::SheetsOperation::ExportOds { .. } => PendingIo::Export(Interchange::Ods),
                _ => return,
            };
            self.resource_invocation = Some(invocation);
            if let Err(error) = self.start_interchange_job(state, pending, path) {
                self.status = format!("Spreadsheet operation failed: {error}");
                self.finish_resource_io(Err(self.status.clone()));
            }
        }
        if let Some(reveal) = self.pending_reveal.take() {
            let (sheet_id, selection) = match reveal {
                PendingReveal::Sheet { sheet_id } => (sheet_id, (0, 0)),
                PendingReveal::Cell { sheet_id, addr } => (sheet_id, addr),
            };
            if let Some(idx) = SheetsDoc::sheet_index_by_resource_id(state, &sheet_id) {
                self.switch_sheet(state, idx);
                self.grid.select_only(selection);
                self.status = format!(
                    "Went to {}!{}",
                    self.sheet_names.get(idx).cloned().unwrap_or_default(),
                    crate::model::addr_to_a1(selection.0, selection.1)
                );
            } else {
                self.status = "That sheet no longer exists".into();
            }
        }
        // Land a finished import/export file dialog before reloading, so the
        // imported sheet (or the export's status) is reflected this frame.
        self.drain_io(state);
        self.drain_orphaned_interchange();
        self.drain_interchange_worker(state);
        self.settle_prepared_resource_io(state);
        self.drain_persistence_worker(state);
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
                self.status_bubble.poke(ui::time::now());
            }
            self.last_status_label = label;
        }
        self.persist_if_dirty(state);
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
        self.annotation_clicks.clear();
        // Measure first, so this frame's chrome (and the hit-rects it leaves
        // behind) is laid out from the renderer that is about to paint it.
        self.refresh_chrome_metrics(text_renderer, theme);
        self.draw_chrome(scene, transform, text_renderer, theme);
        self.grid.draw(scene, transform, text_renderer, theme);
        self.draw_charts(scene, transform, text_renderer, theme);
        self.draw_comment_threads(scene, transform, text_renderer, theme);
        self.draw_loading_veil(scene, transform, text_renderer, theme);
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
            // Keep the previous 110/255 veil; the hue is the theme scrim
            // so a light palette does not punch a black hole through the sheet.
            let scrim = theme
                .tokens
                .colors
                .surfaces
                .scrim
                .with_alpha_factor(110.0 / 255.0);
            scene.fill(Fill::NonZero, transform, scrim, None, &self.rect);
            let (pw, ph) = self.palette.desired_size();
            let x0 = self.rect.center().x - pw / 2.0;
            let y0 = self.rect.y0 + (self.rect.height() * 0.12).max(24.0);
            self.palette.set_rect(Rect::new(x0, y0, x0 + pw, y0 + ph));
            self.palette
                .draw(scene, transform, text_renderer, theme, presence);
        }
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

    fn handle_event(&mut self, event: &WindowEvent, event_ctx: &mut ui::widget::EventCtx<'_>) {
        if self.route_inline_editor(
            event,
            event_ctx.scale_factor,
            *event_ctx.modifiers,
            event_ctx.state,
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
                    event_ctx.state,
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
                self.run_pending_fill(event_ctx.state);
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
                    event_ctx.state,
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
        let toolbar_controls = self
            .menu_buttons
            .iter()
            .filter_map(|button| {
                let (name, target, description) = match (button.opens, &button.item) {
                    (Some(OpenMenu::File), _) => (
                        "File menu",
                        "sheets.toolbar.file",
                        "Open file and interchange actions",
                    ),
                    (Some(OpenMenu::Format), _) => (
                        "Format menu",
                        "sheets.toolbar.format",
                        "Apply a number or text format to the selection",
                    ),
                    (Some(OpenMenu::More), _) => (
                        "More sheet actions",
                        "sheets.toolbar.more",
                        "Open AutoSum, document settings, and formula-view actions",
                    ),
                    (_, Some(MenuItem::AutoSum)) => (
                        "AutoSum",
                        "sheets.toolbar.autosum",
                        "Insert a sum for the numeric run above the active cell",
                    ),
                    (_, Some(MenuItem::OpenDocSettings)) => (
                        "Document settings",
                        "sheets.toolbar.settings",
                        "Set workbook-wide currency, date, and time conventions",
                    ),
                    _ => return None,
                };
                Some(
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_description(description)
                        .with_action_target(target)
                        .with_bounds(button.rect)
                        .with_state(AccessState::new().focusable(true)),
                )
            })
            .chain([
                AccessNode::new(Role::Button)
                    .with_name(if self.grid.formula_view() {
                        "Show calculated results"
                    } else {
                        "Show formulas"
                    })
                    .with_description(
                        "Toggle calculated values and raw formulas; shortcut Control+Shift+F",
                    )
                    .with_action_target("sheets.toolbar.formula-view")
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
                    .with_action_target(format!("sheets.tab:{}", self.sheet_identity(index)))
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
            .with_state(
                AccessState::new().focusable(true).busy(
                    (self.run.persists() && self.persistence_started && !self.pending.seeded)
                        || self.shots_loading,
                ),
            )
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
                                "{}:{}; open, edit, or delete from the chart card",
                                crate::model::addr_to_a1(chart.start.0, chart.start.1),
                                crate::model::addr_to_a1(chart.end.0, chart.end.1)
                            ))
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name("Open chart range")
                                    .with_action_target(format!("{target}:open"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name("Edit chart")
                                    .with_description(format!(
                                        "Current value: {} | {} | {}:{}",
                                        chart.title,
                                        chart.kind.code(),
                                        crate::model::addr_to_a1(chart.start.0, chart.start.1),
                                        crate::model::addr_to_a1(chart.end.0, chart.end.1),
                                    ))
                                    .with_action_target(format!("{target}:edit"))
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
                            .with_name(format!("Comment by {}", thread.author))
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        let activates = matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus);
        if activates {
            let opened = match target {
                "sheets.toolbar.file" => Some(OpenMenu::File),
                "sheets.toolbar.format" => Some(OpenMenu::Format),
                "sheets.toolbar.more" => Some(OpenMenu::More),
                _ => None,
            };
            if let Some(menu) = opened {
                self.settings_panel = SettingsPanel::None;
                self.tab_menu_target = None;
                self.open_menu = if self.open_menu == menu {
                    OpenMenu::None
                } else {
                    menu
                };
                self.relayout_overlays();
                self.pending.redraw = true;
                return true;
            }
            match target {
                "sheets.toolbar.autosum" => {
                    self.run_menu_item(state, &MenuItem::AutoSum);
                    return true;
                }
                "sheets.toolbar.settings" => {
                    self.run_menu_item(state, &MenuItem::OpenDocSettings);
                    return true;
                }
                "sheets.toolbar.formula-view" => {
                    self.toggle_formula_view();
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
                && let Some(index) = self
                    .searchable_sheets
                    .iter()
                    .position(|sheet| sheet.id == sheet_id)
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

impl ui::undo::UndoPoints for SheetsApp {
    fn needs_commit(&self) -> bool {
        self.pending.commit
    }
    fn on_commit(&mut self) {
        self.pending.commit = false;
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

impl ui::search::capability::SearchSource for SheetsApp {
    /// Fold sheets and populated cells into unified search. Every hit names an
    /// exported resource View action; no app-owned activation callback exists.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
    }
}

impl ui::resources::PublishesResources for SheetsApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns the
    /// workbook door — keep only the live selection extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return crate::actions::live_presentation_resources(&self.sheets_resource_ctx());
        }
        self.publish_resources(doc)
    }
    fn action_directory_resource_page(
        &self,
        doc: &AutoCommit,
        cursor: &[u8],
        max_results: usize,
    ) -> Option<ui::widget::ActionDirectoryResourcePage> {
        // The framework root already publishes the workbook. The directory page
        // keeps the same live selection extras as `resources`, never a second root.
        if self.actions.is_hosted() {
            let resources = if cursor.is_empty() && max_results > 0 {
                crate::actions::live_presentation_resources(&self.sheets_resource_ctx())
            } else {
                Vec::new()
            };
            return Some(ui::widget::ActionDirectoryResourcePage {
                resources,
                next_cursor: Vec::new(),
                exhausted: true,
            });
        }
        // The workbook is the sole stable top-level root. Sheets/cells remain
        // in the revisioned retained search provider, not this RPC page.
        let resources = if cursor.is_empty() && max_results > 0 {
            vec![crate::actions::workbook_root(
                &self.sheets_resource_ctx(),
                doc,
            )]
        } else {
            Vec::new()
        };
        Some(ui::widget::ActionDirectoryResourcePage {
            resources,
            next_cursor: Vec::new(),
            exhausted: true,
        })
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == "sheets.workbook" {
            return None;
        }
        self.publish_resource(doc, id)
    }
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        address: &tilo_actions::ActionAddress,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        let sheet_id = address.resource.strip_prefix("sheets.sheet:")?;
        crate::cell_actions::edit_result(doc, sheet_id, &address.action, changes)
    }
}

impl ui::widget::controls::ActionControls for SheetsApp {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.palette.take_control_invocation()
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        if self.palette.is_open() {
            self.palette.control_action_bindings()
        } else {
            Vec::new()
        }
    }
}

impl ui::undo::CommittedActions for SheetsApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records one undo boundary bracketing it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl SheetsApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        if type_id != actions::SHEETS_OPERATION_TYPE {
            return Ok(false);
        }
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        let expected = actions::operation_kind(&operation);
        if kind != expected {
            return Err(format!(
                "operation kind {kind:?} does not match sheets operation kind {expected:?}"
            ));
        }
        match operation {
            actions::SheetsOperation::ImportCsv { path: None } => {
                self.begin_import(Interchange::Csv);
            }
            actions::SheetsOperation::ImportXlsx { path: None } => {
                self.begin_import(Interchange::Xlsx);
            }
            actions::SheetsOperation::ImportOds { path: None } => {
                self.begin_import(Interchange::Ods);
            }
            actions::SheetsOperation::ExportCsv { path: None } => {
                self.begin_export(Interchange::Csv);
            }
            actions::SheetsOperation::ExportXlsx { path: None } => {
                self.begin_export(Interchange::Xlsx);
            }
            actions::SheetsOperation::ExportOds { path: None } => {
                self.begin_export(Interchange::Ods);
            }
            actions::SheetsOperation::ImportCsv { path: Some(_) }
            | actions::SheetsOperation::ImportXlsx { path: Some(_) }
            | actions::SheetsOperation::ImportOds { path: Some(_) }
            | actions::SheetsOperation::ExportCsv { path: Some(_) }
            | actions::SheetsOperation::ExportXlsx { path: Some(_) }
            | actions::SheetsOperation::ExportOds { path: Some(_) } => {
                return Err("direct spreadsheet I/O requires lifecycle dispatch".into());
            }
            actions::SheetsOperation::OpenCommandPalette => self.open_command_palette(doc),
            actions::SheetsOperation::GoToCell { reference } => self.go_to_cell(&reference),
            actions::SheetsOperation::ToggleFormulaView => self.toggle_formula_view(),
            actions::SheetsOperation::RevealSheet { sheet_id } => {
                if !self
                    .searchable_sheets
                    .iter()
                    .any(|sheet| sheet.id == sheet_id)
                {
                    return Err("sheet no longer exists".into());
                }
                self.pending_reveal = Some(PendingReveal::Sheet { sheet_id });
            }
            actions::SheetsOperation::RevealCell { sheet_id, col, row } => {
                if !self.searchable_sheets.iter().any(|sheet| {
                    sheet.id == sheet_id
                        && sheet
                            .cells
                            .iter()
                            .any(|cell| cell.col == col && cell.row == row)
                }) {
                    return Err("cell no longer exists".into());
                }
                self.pending_reveal = Some(PendingReveal::Cell {
                    sheet_id,
                    addr: (col, row),
                });
            }
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        let expected = actions::operation_kind(&operation);
        if kind != expected {
            return Err(format!(
                "operation kind {kind:?} does not match sheets operation kind {expected:?}"
            ));
        }
        let direct = match &operation {
            actions::SheetsOperation::ImportCsv { path }
            | actions::SheetsOperation::ImportXlsx { path }
            | actions::SheetsOperation::ImportOds { path }
            | actions::SheetsOperation::ExportCsv { path }
            | actions::SheetsOperation::ExportXlsx { path }
            | actions::SheetsOperation::ExportOds { path } => path.is_some(),
            _ => false,
        };
        let is_interchange = matches!(
            &operation,
            actions::SheetsOperation::ImportCsv { .. }
                | actions::SheetsOperation::ImportXlsx { .. }
                | actions::SheetsOperation::ImportOds { .. }
                | actions::SheetsOperation::ExportCsv { .. }
                | actions::SheetsOperation::ExportXlsx { .. }
                | actions::SheetsOperation::ExportOds { .. }
        );
        // There is exactly one portal/result slot.  Refuse a second accepted
        // operation rather than overwriting the first invocation's identity
        // (which would strand an agent or Ctrl+K job forever).
        if is_interchange
            && (self.io.is_some()
                || self.pending_resource_io.is_some()
                || self.interchange_rx.is_some()
                || self.active_interchange_generation.is_some()
                || self.resource_invocation.is_some()
                || self.resource_result.is_some())
        {
            return Err("a spreadsheet import or export is already in progress".into());
        }
        if direct {
            self.pending_resource_io = Some((invocation, operation));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if self.io.is_some() {
            self.resource_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )))
        }
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
                    &mut ui::widget::EventCtx {
                        scale_factor,
                        modifiers: &modifiers,
                        state,
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
                        state,
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

    fn try_annotation_click(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
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

    /// Open the exact prompted resource operation behind a painted annotation
    /// control.  The entry is flattened from the current document, so the
    /// pointer route, keyboard palette, agents and history all share one
    /// stable `(resource_id, action_id)` address and one action builder.
    fn open_annotation_editor(
        &mut self,
        doc: &AutoCommit,
        resource_id: &str,
        prefill: String,
    ) -> bool {
        let resources = actions::provide_resources(&self.sheets_resource_ctx(), doc);
        let entry = tilo_actions::flatten(&resources)
            .into_iter()
            .find(|flat| flat.resource_id.as_str() == resource_id && flat.action.id == "edit")
            .map(|flat| CommandEntry::from_flat_action(&flat));
        let Some(entry) = entry else { return false };
        self.palette.open_action_argument(entry, Some(prefill));
        self.pending.redraw = true;
        true
    }

    fn apply_annotation_click(&mut self, action: AnnotationClick, state: &mut AutoCommit) {
        let sheet_id = self.sheet_identity(self.active_sheet);
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
                let form = format!(
                    "{} | {} | {}:{}",
                    chart.title,
                    chart.kind.code(),
                    crate::model::addr_to_a1(chart.start.0, chart.start.1),
                    crate::model::addr_to_a1(chart.end.0, chart.end.1),
                );
                if self.open_annotation_editor(
                    state,
                    &actions::chart_resource_id(&sheet_id, &id),
                    form,
                ) {
                    self.status = "Edit chart: title | type | source range".into();
                } else {
                    self.status = "Chart editor is no longer available".into();
                }
            }
            AnnotationClick::DeleteChart(id) => {
                self.perform_action(
                    state,
                    &SheetsAction::DeleteChart {
                        sheet: self.active_sheet,
                        sheet_id,
                        chart_id: id,
                    },
                );
                self.status = "Deleted chart".into();
            }
            AnnotationClick::Reply(id) => {
                self.perform_action(
                    state,
                    &actions::reply_comment_action(
                        self.active_sheet,
                        sheet_id,
                        id,
                        "You",
                        "Thanks — noted.",
                    ),
                );
                self.status = "Replied to comment".into();
            }
            AnnotationClick::EditComment(id) => {
                let Some(thread) = self.sheet_comments.iter().find(|thread| thread.id == id) else {
                    self.status = "Comment no longer exists".into();
                    return;
                };
                if self.open_annotation_editor(
                    state,
                    &actions::comment_resource_id(&sheet_id, &id),
                    thread.body.clone(),
                ) {
                    self.status = "Editing comment".into();
                } else {
                    self.status = "Comment editor is no longer available".into();
                }
            }
            AnnotationClick::ToggleComment(id, resolved) => {
                self.perform_action(
                    state,
                    &actions::set_comment_resolved_action(
                        self.active_sheet,
                        sheet_id,
                        id,
                        resolved,
                    ),
                );
                self.status = if resolved {
                    "Resolved comment"
                } else {
                    "Reopened comment"
                }
                .into();
            }
            AnnotationClick::DeleteComment(id) => {
                self.perform_action(
                    state,
                    &actions::delete_comment_action(self.active_sheet, sheet_id, id),
                );
                self.status = "Deleted comment".into();
            }
        }
        self.reload(state);
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
        state: &mut AutoCommit,
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
                &mut ui::widget::EventCtx {
                    scale_factor,
                    modifiers: &modifiers,
                    state,
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
                    self.open_command_palette(state);
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
            MenuItem::ToggleFormulaView => self.toggle_formula_view(),
        }
        self.pending.redraw = true;
    }

    /// Shared implementation behind the More menu, accessibility control, and
    /// the canonical view operation. Formula visibility is presentation state,
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
    /// Where the inline rename editor for tab `idx` sits: over the tab, with
    /// at least `TAB_RENAME_MIN_W` (grown with the text) so a longer name fits
    /// while typing, clamped to the window. Re-derived by `relayout` so the
    /// editor follows its tab through any reflow.
    fn tab_rename_rect(&self, idx: usize) -> Rect {
        let m = &self.metrics;
        let tab = self
            .tab_rects
            .iter()
            .find(|(_, i)| *i == idx)
            .map_or(self.add_tab_rect, |(r, _)| *r);
        let right = (tab.x0 + TAB_RENAME_MIN_W * m.text_growth())
            .min(self.rect.x1 - m.s2)
            .max(tab.x1);
        Rect::new(tab.x0, tab.y0, right, tab.y1)
    }

    fn begin_tab_rename(&mut self, idx: usize) {
        let name = self.sheet_names.get(idx).cloned().unwrap_or_default();
        let rect = self.tab_rename_rect(idx);
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
                    self.perform_action(
                        doc,
                        &actions::rename_sheet_action(
                            rename.idx,
                            SheetsDoc::sheet_resource_id(doc, rename.idx).unwrap_or_default(),
                            trimmed,
                        ),
                    );
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
    #[allow(
        clippy::too_many_lines,
        reason = "tab menu actions stay together to preserve their shared active-sheet update rules"
    )]
    fn run_tab_action(&mut self, doc: &mut AutoCommit, idx: usize, act: TabAction) {
        match act {
            TabAction::Rename => self.begin_tab_rename(idx),
            TabAction::Duplicate => {
                if idx < SheetsDoc::sheet_count(doc) {
                    self.perform_action(
                        doc,
                        &actions::duplicate_sheet_action(
                            idx,
                            SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_default(),
                        ),
                    );
                    // The duplicate lands immediately after the source.
                    self.active_sheet = idx + 1;
                    self.grid.select_only((0, 0));
                    self.reload(doc);
                    self.status = "Duplicated sheet".into();
                }
            }
            TabAction::InsertRight => {
                let n = SheetsDoc::sheet_count(doc);
                let at = (idx + 1).min(n);
                let (before_sheet_id, after_sheet_id) = self.sheet_slot_anchors(at, None);
                self.perform_action(
                    doc,
                    &actions::insert_sheet_action(
                        at,
                        before_sheet_id,
                        after_sheet_id,
                        &format!("Sheet{}", n + 1),
                    ),
                );
                self.active_sheet = at;
                self.grid.select_only((0, 0));
                self.reload(doc);
                self.status = "Inserted sheet".into();
            }
            TabAction::MoveLeft => {
                if idx > 0 {
                    let (before_sheet_id, after_sheet_id) =
                        self.sheet_slot_anchors(idx - 1, Some(idx));
                    self.perform_action(
                        doc,
                        &actions::move_sheet_action(
                            idx,
                            SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_default(),
                            idx - 1,
                            before_sheet_id,
                            after_sheet_id,
                        ),
                    );
                    self.active_sheet = idx - 1;
                    self.reload(doc);
                    self.status = "Moved sheet left".into();
                }
            }
            TabAction::MoveRight => {
                if idx + 1 < SheetsDoc::sheet_count(doc) {
                    let (before_sheet_id, after_sheet_id) =
                        self.sheet_slot_anchors(idx + 1, Some(idx));
                    self.perform_action(
                        doc,
                        &actions::move_sheet_action(
                            idx,
                            SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_default(),
                            idx + 1,
                            before_sheet_id,
                            after_sheet_id,
                        ),
                    );
                    self.active_sheet = idx + 1;
                    self.reload(doc);
                    self.status = "Moved sheet right".into();
                }
            }
            TabAction::Delete => {
                let count_before = SheetsDoc::sheet_count(doc);
                if count_before <= 1 {
                    self.status = "A workbook keeps at least one sheet".into();
                } else {
                    let old_active = self.active_sheet;
                    self.perform_action(
                        doc,
                        &actions::delete_sheet_action(
                            idx,
                            SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_default(),
                        ),
                    );
                    if SheetsDoc::sheet_count(doc) < count_before {
                        let count = SheetsDoc::sheet_count(doc);
                        let new_active = match idx.cmp(&old_active) {
                            std::cmp::Ordering::Less => old_active - 1,
                            std::cmp::Ordering::Equal => old_active.min(count - 1),
                            std::cmp::Ordering::Greater => old_active,
                        };
                        self.active_sheet = new_active.min(count - 1);
                        self.grid.select_only((0, 0));
                        self.reload(doc);
                        self.status = "Deleted sheet".into();
                    }
                }
            }
            TabAction::SetColor(color) => {
                self.perform_action(
                    doc,
                    &actions::set_sheet_color_action(
                        idx,
                        SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_default(),
                        color,
                    ),
                );
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
#[path = "app/r2_annotation_cards.rs"]
mod r2_annotation_cards;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::GridCmd;

    /// Serializes tests that round-trip through the process-global system
    /// clipboard, so a parallel test's copy can't land between another's
    /// copy and paste (the app itself is single-instance, so this only
    /// matters under the parallel test harness).
    static CLIP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Build an app in the seeded (sample) mode and run the first `sync`, which
    /// is exactly what the live/offscreen paths do before drawing.
    pub(super) fn seeded_app() -> (SheetsApp, AutoCommit) {
        let mut app = SheetsApp::new();
        app.shots_mode(); // seed sample data, no disk persistence
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    /// A seeded app laid out at `width × height` whose chrome metrics were
    /// resolved against a renderer at `text_scale`, exactly as the first draw
    /// of a frame does. Returns the renderer so a test can re-measure labels
    /// with the same instance.
    pub(super) fn measured_app(
        width: f64,
        height: f64,
        text_scale: f32,
    ) -> (SheetsApp, TextRenderer) {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, width, height);
        app.relayout();
        let theme = Theme::dark().with_text_scale(text_scale);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        app.text_scale = f64::from(theme.text_scale);
        app.grid.set_text_scale(app.text_scale);
        app.refresh_chrome_metrics(&mut tr, &theme);
        (app, tr)
    }

    /// Final-pixel width of `label` painted at the chrome's `sm` size.
    fn sm_label_w(tr: &mut TextRenderer, app: &SheetsApp, label: &str) -> f64 {
        f64::from(
            tr.measure_weighted(label, app.metrics.sm, None, TextWeight::Semibold)
                .0,
        )
    }

    /// Chrome bands, status bubble, tabs, settings labels, tab menu and pointer targets follow measured text.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn chrome_density_follows_measured_text() {
        // Case `chrome_metrics_keep_the_one_x_density`.
        // At 1× the measured chrome reproduces the classic band density
        // exactly: 44 px menu bar, 34 px formula bar, 34 px tab strip, 28 px
        // column header, 26 px default row, 56 px row header.
        {
            let (app, _tr) = measured_app(1280.0, 720.0, 1.0);
            assert!((app.menubar_rect.height() - 44.0).abs() < 0.01, "menu bar");
            assert!(
                (app.formula_rect.height() - 40.0).abs() < 0.01,
                "formula bar"
            );
            assert!((app.tabs_rect.height() - 38.0).abs() < 0.01, "tab strip");
            let g = app.grid.metrics();
            assert!((g.col_header_h() - 28.0).abs() < 0.01, "column header");
            assert!((g.default_row_h() - 26.0).abs() < 0.01, "default row");
            assert!((g.row_header_w() - 56.0).abs() < 0.01, "row header");
        }

        // Case `chrome_bands_and_labels_follow_measured_text_at_two_x`.
        // At 2× every chrome band, button and label slot follows the measured
        // text: nothing is painted from the 1× geometry, and the shared renderer
        // scale is applied exactly once (a token size fed to the renderer twice
        // would need roughly twice the room again).
        {
            let (app, mut tr) = measured_app(1280.0, 720.0, 2.0);
            let m = app.metrics.clone();
            // Sizes handed to the renderer are the theme tokens divided back by
            // the renderer scale, so the final glyphs land at the token size.
            assert!((m.sm - 12.0).abs() < 0.01, "sm renderer input {}", m.sm);
            assert!(
                (m.base - 14.0).abs() < 0.01,
                "base renderer input {}",
                m.base
            );
            assert!(m.lh_sm > 28.0 && m.lh_sm < 32.0, "sm line box {}", m.lh_sm);

            // Bands grew to their text.
            assert!(app.menubar_rect.height() > 44.0);
            assert!(app.formula_rect.height() >= m.lh_base + 2.0 * m.s1);
            assert!(app.tabs_rect.height() >= m.lh_sm + m.s1);
            assert!(app.grid.metrics().col_header_h() >= m.lh_sm);
            assert!(app.grid.metrics().default_row_h() >= m.lh_base);

            // Menu buttons: one centre line, `s2` apart, each wide enough for the
            // label it paints (measured with the painting style).
            let centre = app.menu_buttons[0].rect.center().y;
            for pair in app.menu_buttons.windows(2) {
                assert!(
                    (pair[0].rect.center().y - centre).abs() < 0.01,
                    "one centre line"
                );
                assert!(
                    (pair[1].rect.x0 - pair[0].rect.x1 - m.s2).abs() < 0.01,
                    "menu buttons are s2 apart"
                );
            }
            for b in &app.menu_buttons {
                let label = SheetsApp::menu_button_label(&b.label, b.opens.is_some());
                let w = sm_label_w(&mut tr, &app, &label);
                assert!(
                    b.rect.width() >= w + 2.0 * m.s3 - 0.01,
                    "'{label}' needs {w:.1} + padding, button is {:.1}",
                    b.rect.width()
                );
                assert!(
                    b.rect.height() >= m.lh_sm,
                    "button taller than its line box"
                );
            }

            // Formula bar: the fx glyph fits its own hit slot, which is exactly
            // where it is painted; field and name box share one centre line.
            assert!(app.fx_rect.width() >= m.fx_w + 2.0 * m.s2 - 0.01);
            assert!((app.fx_rect.center().y - app.name_box_rect.center().y).abs() < 0.01);
            assert!(
                (app.formula_field_rect.center().y - app.name_box_rect.center().y).abs() < 0.01
            );
            assert!(app.name_box_rect.width() >= sm_label_w(&mut tr, &app, NAME_BOX_REFERENCE));

            // Sheet tabs fit their names and share the strip's centre line.
            for (rect, idx) in &app.tab_rects {
                let name = &app.sheet_names[*idx];
                let w = sm_label_w(&mut tr, &app, name);
                assert!(rect.width() >= w + 2.0 * m.s3 - 0.01, "tab '{name}' fits");
                assert!(rect.height() >= m.lh_sm);
                assert!((rect.center().y - app.add_tab_rect.center().y).abs() < 0.01);
            }
            assert!(
                (app.add_tab_rect.width() - app.add_tab_rect.height()).abs() < 0.01,
                "+ is square"
            );
        }

        // Case `status_bubble_floats_above_the_scrollbar_rail`.
        // The floating status pill anchors above the grid's horizontal scrollbar
        // rail (with an `s2` gap), never inside it and never on the tab strip.
        {
            for scale in [1.0_f32, 2.0] {
                let (app, mut tr) = measured_app(1200.0, 800.0, scale);
                let rail_top = app.grid_rect.y1 - Grid::scrollbar_thickness();
                let bottom = app.status_bubble_bottom();
                assert!(
                    bottom <= rail_top - app.metrics.s2 + 0.01,
                    "at {scale}x the pill bottom {bottom} sits on the scrollbar rail (top {rail_top})"
                );
                assert!(bottom < app.tabs_rect.y0);
                // The pill is at least as tall as its measured text plus `s2`
                // above and below, so a 2× label never spills out of it.
                let (w, h) = app.status_pill_size(&mut tr, "Currency (USD)");
                let (tw, th) =
                    tr.measure_weighted("Currency (USD)", app.metrics.sm, None, TextWeight::Medium);
                assert!(
                    h >= f64::from(th) + 2.0 * app.metrics.s2 - 0.01,
                    "pill height {h} at {scale}x"
                );
                assert!(w >= f64::from(tw), "pill width {w} at {scale}x");
            }
        }

        // Case `sheet_tabs_keep_their_floor_and_ceiling_at_every_scale`.
        // Sheet tabs stay between the (text-grown) touch-target floor and the
        // long-name ceiling, so one long name never pushes the "+" button off the
        // strip, and the painter elides rather than overruns.
        {
            for scale in [1.0_f32, 2.0] {
                let (mut app, mut tr) = measured_app(1000.0, 700.0, scale);
                let long = "Quarterly consolidated regional operating expenses by cost centre";
                app.sheet_names = vec![long.to_owned(), "Q".to_owned()];
                app.relayout();
                let g = app.metrics.text_growth();
                let (wide, _) = app.tab_rects[0];
                let (narrow, _) = app.tab_rects[1];
                assert!(
                    wide.width() <= TAB_MAX_W * g + 0.01,
                    "long name is capped at {scale}x"
                );
                assert!(
                    narrow.width() >= TAB_MIN_W * g - 0.01,
                    "short name keeps the floor at {scale}x"
                );
                assert!(app.add_tab_rect.x1 <= app.rect.x1, "'+' stays on the strip");
                // The painter's elision fits the capped tab with `s3` each side.
                let elided = tr.elide_to_width_weighted(
                    long,
                    app.metrics.sm,
                    (wide.width() - 2.0 * app.metrics.s3) as f32,
                    TextWeight::Semibold,
                );
                let (ew, _) =
                    tr.measure_weighted(&elided, app.metrics.sm, None, TextWeight::Semibold);
                assert!(f64::from(ew) <= wide.width() - 2.0 * app.metrics.s3 + 0.5);
                assert!(elided.ends_with('…') && elided.chars().count() < long.chars().count());
            }
        }

        // Case `settings_panel_labels_align_with_their_chip_rows`.
        // Settings panel: each section label is centred on its first chip row,
        // chips in a row share one centre line with `s2` gaps, every chip fits
        // its label, and the panel wraps its own content.
        {
            for scale in [1.0_f32, 2.0] {
                let (mut app, mut tr) = measured_app(1200.0, 800.0, scale);
                app.shots_open_doc_settings();
                app.relayout();
                let m = app.metrics.clone();
                let firsts = [
                    SettingKind::Currency(None),
                    SettingKind::Date(None),
                    SettingKind::Time(None),
                ];
                for (section_y, first) in app.settings_section_y.iter().zip(firsts) {
                    let chip = app
                        .setting_chips
                        .iter()
                        .find(|c| c.kind == first)
                        .expect("section's Default chip");
                    assert!(
                        (chip.rect.y0 - section_y).abs() < 0.01,
                        "at {scale}x the section label row starts with its first chip"
                    );
                }
                for pair in app.setting_chips.windows(2) {
                    if (pair[0].rect.y0 - pair[1].rect.y0).abs() < 0.01 {
                        assert!(
                            (pair[1].rect.x0 - pair[0].rect.x1 - m.s2).abs() < 0.01,
                            "chips in a row are s2 apart"
                        );
                    } else {
                        assert!(
                            pair[1].rect.y0 >= pair[0].rect.y1 + m.s2 - 0.01,
                            "rows do not overlap"
                        );
                    }
                }
                for chip in &app.setting_chips {
                    let (label, _) = app.chip_label_and_state(chip.kind);
                    let w = sm_label_w(&mut tr, &app, &label);
                    assert!(
                        chip.rect.width() >= w + 2.0 * m.s3 - 0.01,
                        "chip '{label}' fits"
                    );
                    assert!(chip.rect.height() >= m.lh_sm);
                    assert!(app.panel_rect.contains(chip.rect.center()));
                    assert!(chip.rect.x1 <= app.panel_rect.x1 - m.s6 + 0.01);
                }
                let last = app
                    .setting_chips
                    .iter()
                    .map(|c| c.rect.y1)
                    .fold(0.0_f64, f64::max);
                assert!(
                    app.panel_rect.y1 >= last + m.s6 + m.lh_xs,
                    "panel leaves room for its footer under the last chip row"
                );
            }
        }

        // Case `tab_menu_reserves_room_for_its_caption_and_swatches_at_two_x`.
        // Sheet-tab menu: rows are measured, the "Colour" caption block and the
        // swatch strip both fit inside the panel at 2×.
        {
            let (mut app, mut tr) = measured_app(1200.0, 800.0, 2.0);
            app.shots_open_tab_menu();
            app.relayout();
            let m = app.metrics.clone();
            let panel = app.tab_menu_rect;
            for (rect, act) in &app.tab_menu_rows {
                assert!(
                    rect.height() >= m.lh_sm - MENU_ROW_SEAM,
                    "row fits its line box"
                );
                let w = sm_label_w(&mut tr, &app, tab_action_label(*act));
                assert!(
                    rect.width() >= w + 2.0 * m.s2 - 0.01,
                    "row fits '{}'",
                    tab_action_label(*act)
                );
            }
            let first = app.tab_menu_swatches[0].0;
            let caption_top = first.y0 - m.s1 - m.lh_xs;
            let last_row = app.tab_menu_rows.last().expect("rows").0;
            assert!(
                caption_top >= last_row.y1,
                "caption sits under the last row"
            );
            for (swatch, _) in &app.tab_menu_swatches {
                assert!(panel.contains(swatch.center()));
                assert!(swatch.x1 <= panel.x1 - m.s2 + 0.01 && swatch.y1 <= panel.y1 - m.s2 + 0.01);
            }
        }

        // Case `chrome_targets_meet_the_pointer_minimum_and_scale_with_type`.
        // Custom-painted controls have no toolkit button wrapper to enforce the
        // interaction minimum. Keep their hit geometry aligned with the shared
        // fine-pointer contract, and prove the same surfaces expand at 2× type.
        {
            let (app, _tr) = measured_app(560.0, 720.0, 1.0);
            let minimum =
                f64::from(ui::interaction::InteractionEnvironment::POINTER.minimum_target());
            assert!(
                app.menu_buttons
                    .iter()
                    .all(|button| button.rect.height() >= minimum)
            );
            assert!(app.name_box_rect.height() >= minimum);
            assert!(app.formula_field_rect.height() >= minimum);
            assert!(
                app.tab_rects
                    .iter()
                    .all(|(rect, _)| rect.height() >= minimum)
            );

            let (app, _tr) = measured_app(560.0, 720.0, 2.0);
            assert!(
                app.menu_buttons
                    .iter()
                    .all(|button| button.rect.height() >= minimum * 2.0)
            );
            assert!(
                app.grid.accessibility_window().rows[0].bounds.height()
                    >= crate::grid::DEFAULT_ROW_H * 2.0
            );
        }
    }

    /// Background persistence deliberately never blocks a frame. Tests that
    /// need its result drive the same frame pump a running app uses and,
    /// between frames, block on the outstanding worker's reply: a loaded
    /// runner can take many seconds to schedule a worker that writes and syncs
    /// a file, so no short budget is safe. A lost worker disconnects its
    /// channel and the app reports it; the hang bound only catches a hung one.
    fn sync_until(app: &mut SheetsApp, doc: &mut AutoCommit, done: impl Fn(&SheetsApp) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            app.sync(doc, &automerge::ROOT);
            if done(app) {
                return;
            }
            let within = deadline.saturating_duration_since(ui::time::now());
            let pending = app.status.clone();
            if let Some(rx) = app.persistence_rx.take() {
                app.persistence_rx = Some(await_reply(rx, within, "persistence", &pending));
            } else if let Some(rx) = app.interchange_rx.take() {
                app.interchange_rx = Some(await_reply(rx, within, "interchange", &pending));
            } else {
                // No reply outstanding: a retry delay or a frame-driven step.
                assert!(
                    !within.is_zero(),
                    "Sheets did not settle within {:?} with no worker outstanding (status: {pending})",
                    tilo_test_env::hang_bound()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// Wait for a worker's one reply and hand it back as a ready channel, so
    /// the app receives it through its own drain.
    fn await_reply<T>(
        rx: Receiver<T>,
        within: Duration,
        worker: &str,
        status: &str,
    ) -> Receiver<T> {
        match rx.recv_timeout(within) {
            Ok(reply) => {
                let (tx, ready) = mpsc::sync_channel(1);
                tx.send(reply)
                    .expect("the ready channel has room for one reply");
                ready
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => rx,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "background Sheets {worker} worker sent no reply before the hang bound (status: {status})"
            ),
        }
    }

    /// Unchanged syncs keep the search catalogue and reduced motion stops bubble frames.
    #[test]
    fn search_catalogue_and_idle_frames_do_no_redundant_work() {
        // Case `unchanged_sync_and_tab_switch_retain_the_search_catalogue`.
        {
            let (mut app, mut doc) = seeded_app();
            // Automerge commits the startup seed at the first following sync;
            // establish the steady-state revision before asserting idle work.
            app.sync(&mut doc, &automerge::ROOT);
            let first_heads = app.search_projection_heads.clone();
            let first_projection = app.searchable_sheets.as_ptr();
            let first_catalogue = app.search_provider.stats();
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(app.search_projection_heads, first_heads);
            assert_eq!(app.searchable_sheets.as_ptr(), first_projection);
            assert_eq!(app.search_provider.stats(), first_catalogue);

            app.switch_sheet(&mut doc, 1);
            app.sync(&mut doc, &automerge::ROOT);
            assert_eq!(app.searchable_sheets.as_ptr(), first_projection);
            assert_eq!(app.search_provider.stats(), first_catalogue);
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

    /// Resource operations reject wrong kinds; affordances dispatch the same encoded actions as their specs.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn affordances_and_resource_operations_match_action_specs() {
        // Case `resource_operations_reject_wrong_kinds_without_changing_state`.
        {
            let (mut app, mut doc) = seeded_app();
            assert!(!app.grid.formula_view());
            let toggle = serde_json::to_vec(&actions::SheetsOperation::ToggleFormulaView).unwrap();
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        actions::SHEETS_OPERATION_TYPE,
                        &toggle
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err()
            );
            assert!(!app.grid.formula_view());

            let import = serde_json::to_vec(&actions::SheetsOperation::ImportCsv {
                path: Some("/tmp/wrong-kind.csv".into()),
            })
            .unwrap();
            assert!(
                Widget::start_operation(
                    &mut app,
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        actions::SHEETS_OPERATION_TYPE,
                        &import
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .is_err()
            );
            assert!(app.pending_resource_io.is_none());
            assert!(app.resource_invocation.is_none());
        }

        // Case `tab_delete_affordance_matches_the_action_spec`.
        // Covenant — one source of truth: an on-screen affordance dispatches the
        // byte-identical [`EncodedAction`] its Ctrl+K [`ActionSpec`] builds. Driving
        // the real sheet-tab "Delete" context-menu handler sets `last_dispatched`
        // to exactly what the workbook resource tree's delete spec encodes.
        {
            let (mut app, mut doc) = seeded_app();
            let count_before = SheetsDoc::sheet_count(&mut doc);
            assert!(count_before >= 2, "seeded workbook has multiple sheets");

            // Capture the Ctrl+K delete spec for sheet 1 from the live resource tree,
            // *before* the affordance mutates the workbook (the tree shrinks after).
            let from_spec = {
                let resources = ui::resources::resources(&app, &doc);
                let sheet1 = &resources[0].children[1];
                let spec = sheet1.actions.iter().find(|a| a.id == "delete").unwrap();
                match &spec.input {
                    tilo_actions::ActionInput::Immediate(enc) => enc.clone(),
                    tilo_actions::ActionInput::Prompted { .. }
                    | tilo_actions::ActionInput::Deferred { .. } => {
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

        // Case `cell_edit_affordance_dispatches_a_matching_set_cell_action`.
        // Covenant on the real app path: a cell edit routed through the grid's
        // `SetCell` command dispatches a `SheetsAction::SetCell` carrying the exact
        // coordinates and value — the single CRDT-mutation path that undo, peer
        // replay, and Ctrl+K all share. The `last_dispatched` envelope decodes back
        // to the action the affordance built.
        {
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
            let action: crate::actions::SheetsAction = serde_json::from_slice(&dispatched.payload)
                .expect("payload decodes to SheetsAction");
            match action {
                crate::actions::SheetsAction::SetCells { sheet, cells, .. } => {
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
    }

    /// Cancelled, stale and prepared imports/exports never mutate state before confirmation.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn import_export_lifecycles_never_touch_the_destination_early() {
        // Case `cancelled_import_completion_is_inert_before_any_crdt_action`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = SheetsDoc::sheet_count(&mut doc);
            let invocation = tilo_actions::InvocationId::new();
            app.resource_invocation = Some(invocation);
            app.active_interchange_generation = Some(41);
            let cancelled =
                Widget::poll_resource_operation(&mut app, invocation, true, &doc, &automerge::ROOT);
            assert!(matches!(
                cancelled,
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(InterchangeResult::Import {
                generation: 41,
                format: Interchange::Csv,
                path: PathBuf::from("cancelled.csv"),
                result: Ok(vec![crate::xlsx::SheetTable {
                    name: "Must not import".into(),
                    rows: Vec::new(),
                }]),
            })
            .unwrap();
            app.interchange_rx = Some(rx);
            app.drain_interchange_worker(&mut doc);

            assert_eq!(SheetsDoc::sheet_count(&mut doc), before);
            assert!(app.active_interchange_generation.is_none());
            assert!(app.resource_result.is_none());
        }

        // Case `cancelling_prepared_export_never_touches_the_destination`.
        {
            let (mut app, doc) = seeded_app();
            let directory =
                std::env::temp_dir().join(format!("tilo-sheets-cancel-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let destination = directory.join("report.csv");
            let temporary = directory.join(".report.csv.tilo-export-prepared.tmp");
            std::fs::write(&destination, "original").unwrap();
            std::fs::write(&temporary, "new export").unwrap();
            let invocation = tilo_actions::InvocationId::new();
            app.resource_invocation = Some(invocation);
            app.prepared_resource_io = Some(PreparedResourceIo::Export {
                format: Interchange::Csv,
                destination: destination.clone(),
                temporary: temporary.clone(),
            });

            let cancelled =
                Widget::poll_resource_operation(&mut app, invocation, true, &doc, &automerge::ROOT);
            assert!(matches!(
                cancelled,
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));
            assert_eq!(std::fs::read_to_string(&destination).unwrap(), "original");
            assert!(!temporary.exists());
            let _ = std::fs::remove_dir_all(&directory);
        }

        // Case `stale_export_completion_is_discarded_without_destination_mutation`.
        {
            let (mut app, mut doc) = seeded_app();
            let directory =
                std::env::temp_dir().join(format!("tilo-sheets-stale-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let destination = directory.join("report.csv");
            let temporary = directory.join(".report.csv.tilo-export-stale.tmp");
            std::fs::write(&destination, "original").unwrap();
            std::fs::write(&temporary, "stale export").unwrap();
            app.active_interchange_generation = Some(44);
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(InterchangeResult::Export {
                generation: 43,
                format: Interchange::Csv,
                destination: destination.clone(),
                result: Ok(temporary.clone()),
            })
            .unwrap();
            app.interchange_rx = Some(rx);
            app.drain_interchange_worker(&mut doc);

            assert_eq!(std::fs::read_to_string(&destination).unwrap(), "original");
            assert!(!temporary.exists());
            let _ = std::fs::remove_dir_all(&directory);
        }

        // Case `export_preparation_does_not_mutate_destination_before_publish`.
        {
            let (app, mut doc) = seeded_app();
            let directory =
                std::env::temp_dir().join(format!("tilo-sheets-prepare-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let destination = directory.join("report.csv");
            std::fs::write(&destination, "original").unwrap();
            let snapshot = SheetsDoc::snapshot_sheet(&mut doc, 0, Currency::USD);
            let temporary = SheetsApp::prepare_export_snapshots(
                Interchange::Csv,
                &destination,
                45,
                &[snapshot],
            )
            .unwrap();

            assert_eq!(std::fs::read_to_string(&destination).unwrap(), "original");
            assert!(temporary.exists());
            SheetsApp::discard_export_temp(&temporary);
            let _ = std::fs::remove_dir_all(&directory);
            drop(app);
        }

        // Case `prepared_resource_import_waits_for_lifecycle_confirmation`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = SheetsDoc::sheet_count(&mut doc);
            let invocation = tilo_actions::InvocationId::new();
            app.resource_invocation = Some(invocation);
            app.prepared_resource_io = Some(PreparedResourceIo::Import {
                format: Interchange::Csv,
                path: PathBuf::from("prepared.csv"),
                tables: vec![crate::xlsx::SheetTable {
                    name: "Prepared".into(),
                    rows: Vec::new(),
                }],
            });

            app.settle_prepared_resource_io(&mut doc);
            assert_eq!(SheetsDoc::sheet_count(&mut doc), before);
            assert!(app.prepared_resource_io.is_some());
            assert!(
                Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT,
                )
                .is_none()
            );
            app.settle_prepared_resource_io(&mut doc);
            assert_eq!(SheetsDoc::sheet_count(&mut doc), before + 1);
            assert!(matches!(
                Widget::poll_resource_operation(
                    &mut app,
                    invocation,
                    false,
                    &doc,
                    &automerge::ROOT,
                ),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
        }
    }

    /// Admit an agent's direct-path `.xlsx` export and run the one model pass
    /// admission schedules.
    fn admit_direct_export(
        app: &mut SheetsApp,
        doc: &mut AutoCommit,
        destination: &std::path::Path,
    ) -> tilo_actions::InvocationId {
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&actions::SheetsOperation::ExportXlsx {
            path: Some(destination.display().to_string()),
        })
        .unwrap();
        let started = Widget::start_operation(
            app,
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                actions::SHEETS_OPERATION_TYPE,
                &payload,
            ),
            doc,
            &automerge::ROOT,
        );
        assert!(
            matches!(
                started,
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            ),
            "{started:?}"
        );
        app.sync(doc, &automerge::ROOT);
        invocation
    }

    /// Drive `invocation` the way an idle native or hosted runner does: poll
    /// on every pass, but `sync` only when a wake arrives. Nothing else makes
    /// frames, so a job whose progress wakes nobody stays Accepted forever.
    fn settle_on_idle_host(
        app: &mut SheetsApp,
        doc: &mut AutoCommit,
        wakes: &Receiver<ui::wake::WakeEvent>,
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::InvocationUpdate {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(update) =
                Widget::poll_resource_operation(app, invocation, false, doc, &automerge::ROOT)
            {
                return update;
            }
            let within = deadline.saturating_duration_since(Instant::now());
            wakes.recv_timeout(within).unwrap_or_else(|_| {
                panic!(
                    "the job never woke its idle host (status: {}); an agent would see it Accepted forever",
                    app.status
                )
            });
            app.sync(doc, &automerge::ROOT);
        }
    }

    /// An agent's direct-path export on an idle host reaches a terminal state
    /// through the app's own wakes, and cancelling one releases the single
    /// interchange slot immediately instead of refusing every retry.
    #[test]
    fn direct_path_export_settles_and_cancels_on_an_idle_host() {
        let mut scope = ui::wake::WakeScope::new();
        let _entry = scope.enter();
        let (sender, wakes) = mpsc::channel();
        scope
            .bind(move |event| {
                let _ = sender.send(event);
            })
            .unwrap();
        let directory =
            std::env::temp_dir().join(format!("tilo-sheets-idle-export-{}", Uuid::new_v4()));
        let destination = directory.join("out").join("in-stock.xlsx");
        let (mut app, mut doc) = seeded_app();

        // Case `export_reaches_a_terminal_state_without_foreign_frames`.
        {
            let invocation = admit_direct_export(&mut app, &mut doc, &destination);
            let update = settle_on_idle_host(&mut app, &mut doc, &wakes, invocation);
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{update:?}"
            );
            assert!(!crate::xlsx::read_xlsx(&destination).unwrap().is_empty());
        }

        // Case `cancelling_an_in_flight_export_releases_the_slot`.
        {
            std::fs::remove_file(&destination).unwrap();
            let cancelled_job = admit_direct_export(&mut app, &mut doc, &destination);
            assert!(app.interchange_rx.is_some(), "the export worker is running");
            let cancelled = Widget::poll_resource_operation(
                &mut app,
                cancelled_job,
                true,
                &doc,
                &automerge::ROOT,
            );
            assert!(
                matches!(
                    cancelled,
                    Some(tilo_actions::InvocationUpdate::Cancelled { .. })
                ),
                "{cancelled:?}"
            );
            assert!(app.interchange_rx.is_none());
            assert!(app.active_interchange_generation.is_none());

            let retry = admit_direct_export(&mut app, &mut doc, &destination);
            let update = settle_on_idle_host(&mut app, &mut doc, &wakes, retry);
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{update:?}"
            );
            assert!(destination.exists());

            // The cancelled worker's prepared file is reaped, never published.
            let deadline = tilo_test_env::hang_deadline();
            while !app.orphaned_interchange.is_empty() {
                let within = deadline.saturating_duration_since(Instant::now());
                wakes
                    .recv_timeout(within)
                    .expect("the cancelled worker wakes its host when it exits");
                app.sync(&mut doc, &automerge::ROOT);
            }
            let leftovers: Vec<_> = std::fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name != "in-stock.xlsx")
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Admit an agent's direct-path `.xlsx` import and run the one model pass
    /// admission schedules.
    fn admit_direct_import(
        app: &mut SheetsApp,
        doc: &mut AutoCommit,
        source: &std::path::Path,
    ) -> tilo_actions::InvocationId {
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&actions::SheetsOperation::ImportXlsx {
            path: Some(source.display().to_string()),
        })
        .unwrap();
        let started = Widget::start_operation(
            app,
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                actions::SHEETS_OPERATION_TYPE,
                &payload,
            ),
            doc,
            &automerge::ROOT,
        );
        assert!(
            matches!(
                started,
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            ),
            "{started:?}"
        );
        app.sync(doc, &automerge::ROOT);
        invocation
    }

    /// Opening a file the workbook already holds, by an agent's typed import
    /// or the Import dialog, reveals the sheet it became instead of importing
    /// a second copy.
    #[test]
    fn opening_an_open_file_again_reveals_it_instead_of_importing_a_copy() {
        let mut scope = ui::wake::WakeScope::new();
        let _entry = scope.enter();
        let (sender, wakes) = mpsc::channel();
        scope
            .bind(move |event| {
                let _ = sender.send(event);
            })
            .unwrap();
        let directory = std::env::temp_dir().join(format!("tilo-sheets-reopen-{}", Uuid::new_v4()));
        let file = directory.join("budget.xlsx");
        {
            let (mut app, mut doc) = seeded_app();
            let invocation = admit_direct_export(&mut app, &mut doc, &file);
            let update = settle_on_idle_host(&mut app, &mut doc, &wakes, invocation);
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{update:?}"
            );
        }
        let (mut app, mut doc) = seeded_app();
        let imported = SheetsDoc::sheet_count(&mut doc);
        let first = admit_direct_import(&mut app, &mut doc, &file);
        let update = settle_on_idle_host(&mut app, &mut doc, &wakes, first);
        assert!(
            matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
            "{update:?}"
        );
        let sheets = SheetsDoc::sheet_count(&mut doc);
        let imported_id = SheetsDoc::sheet_resource_id(&doc, imported);

        // Case `a_typed_reopen_returns_the_open_sheet`.
        {
            app.switch_sheet(&mut doc, 0);
            let respelled = directory.join(".").join("budget.xlsx");
            let again = admit_direct_import(&mut app, &mut doc, &respelled);
            let update = settle_on_idle_host(&mut app, &mut doc, &wakes, again);
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
                panic!("{update:?}");
            };
            tilo_actions::ResultSpec::Job
                .validate(&output)
                .expect("a reopen honours the import's published job result");
            assert_eq!(SheetsDoc::sheet_count(&mut doc), sheets, "no second copy");
            assert_eq!(app.active_sheet, imported, "the open sheet is revealed");
            assert_eq!(
                SheetsDoc::sheet_resource_id(&doc, app.active_sheet),
                imported_id
            );
        }

        // Case `the_import_dialog_reveals_the_open_sheet`.
        {
            app.switch_sheet(&mut doc, 0);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(file.clone()));
            ui::host::scoped(&services, || app.begin_import(Interchange::Xlsx));
            app.drain_io(&mut doc);
            assert!(app.interchange_rx.is_none(), "nothing is read again");
            assert_eq!(SheetsDoc::sheet_count(&mut doc), sheets, "no second copy");
            assert_eq!(app.active_sheet, imported, "the open sheet is revealed");
            assert!(app.status.contains("already open"), "{}", app.status);
        }

        // Case `a_file_whose_sheets_were_deleted_imports_again`.
        {
            while let Some(idx) = SheetsDoc::sheet_index_by_source(&doc, &import_source(&file)) {
                let deleted = actions::SheetsAction::DeleteSheet {
                    idx,
                    sheet_id: SheetsDoc::sheet_resource_id(&doc, idx).unwrap(),
                };
                app.try_perform_action(&mut doc, &deleted).unwrap();
            }
            app.reload(&mut doc);
            let after_delete = SheetsDoc::sheet_count(&mut doc);
            let reimport = admit_direct_import(&mut app, &mut doc, &file);
            let update = settle_on_idle_host(&mut app, &mut doc, &wakes, reimport);
            assert!(
                matches!(update, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{update:?}"
            );
            assert!(SheetsDoc::sheet_count(&mut doc) > after_delete);
        }
        let _ = std::fs::remove_dir_all(&directory);
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
            app.switch_sheet(&mut doc, 1); // click the "Sales" tab
            assert_eq!(app.active_sheet, 1);
            let wb = SheetsDoc::wb_map(&mut doc);
            assert!(
                automerge::ReadDoc::get(&doc, &wb, "active")
                    .unwrap()
                    .is_none(),
                "switching tabs is local presentation and must not write the shared workbook"
            );
            // The pending edit landed on sheet 0, not sheet 1.
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 30)), "draft");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (0, 30)), "");
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

        // Case `auto_sum_writes_a_working_formula`.
        {
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

        // Case `auto_sum_guard_nothing_above`.
        // `AutoSum` on row 1 has nothing above it: it reports so and writes no formula.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 0)); // A1, row 0
            app.auto_sum(&mut doc);
            assert_eq!(app.status, "Nothing above to sum");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "Category"); // unchanged
        }

        // Case `auto_sum_guard_no_numeric_run`.
        // `AutoSum` directly under a non-numeric cell finds no run and writes nothing.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((0, 1)); // A2, directly under A1 = "Category" (text)
            let before = SheetsDoc::cell_raw(&mut doc, 0, (0, 1));
            app.auto_sum(&mut doc);
            assert_eq!(app.status, "No numeric run above");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 1)), before);
            assert!(!SheetsDoc::cell_raw(&mut doc, 0, (0, 1)).starts_with("=SUM"));
        }
    }

    /// Format application over single and multi-range selections, clearing and percent commands.
    #[test]
    fn formats_apply_and_clear_across_selections() {
        // Case `apply_format_changes_active_type`.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((1, 8)); // B9 = 2600 (currency in the seed)
            app.apply_format(&mut doc, CellFormat::Percent);
            assert_eq!(app.format_of_active(), CellFormat::Percent);
        }

        // Case `format_applies_across_a_multi_range_selection`.
        // Format applied over a discontiguous multi-range selection reaches every
        // cell in *both* ranges — the real `apply_format` → `selected_cells` path.
        {
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

        // Case `clear_format_resets_applied_format`.
        // Clear-format resets an applied format back to General — the real Format
        // menu "Clear format" path.
        {
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

        // Case `command_format_percent_applies_to_selection`.
        {
            let (mut app, mut doc) = seeded_app();
            app.grid.select_only((1, 1)); // B2
            app.dispatch_command(&mut doc, "format_percent", None);
            // The stored format for B2 is now Percent.
            let cur = app.default_currency();
            let snap = SheetsDoc::snapshot_sheet(&mut doc, 0, cur);
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

    /// XLSX/CSV/ODS export-import round-trips, sparse refusal and portal drain/cancel.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn interchange_round_trips_through_every_format_and_portal() {
        // Case `xlsx_export_import_restores_live_formulas`.
        // The real export → import path for a workbook of formulas: the active
        // sheet is exported to a `.xlsx`, read back, and imported into a fresh
        // document. Formula cells restore their formula *text* (not a frozen
        // value) and recompute to the original results.
        {
            let (app, mut doc) = seeded_app();
            // Budget!E2 = =SUM(B2:D2) = 3600 in the seed.
            let table = app.table_for_sheet(&mut doc, app.active_sheet);
            let path = std::env::temp_dir().join("sheets-app-formula-rt.xlsx");
            crate::xlsx::write_xlsx(&path, std::slice::from_ref(&table)).unwrap();
            let tables = crate::xlsx::read_xlsx(&path).unwrap();
            let _ = std::fs::remove_file(&path);

            // Import into a brand-new, empty document via the real import path.
            let mut fresh = SheetsApp::new();
            let mut doc2 = AutoCommit::new();
            fresh.import_tables(&mut doc2, tables, None).unwrap();

            // The formula came back as a formula, not its computed value.
            assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (4, 1)), "=SUM(B2:D2)");
            // …and it recomputes to the original result.
            let snap = SheetsDoc::snapshot_sheet(&mut doc2, 0, Currency::USD);
            let values = formula::recalculate(&snap.raw);
            assert_eq!(values.get(&(4, 1)), Some(&CellValue::Number(3600.0)));
            // A non-formula neighbour still round-trips as a plain value.
            assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (1, 1)), "1200");
        }

        // Case `a_file_imported_into_a_new_workbook_is_the_workbook`.
        // A new workbook's empty Sheet1 gives way, so an export of the
        // imported file doesn't open on a blank first tab.
        {
            let mut app = SheetsApp::new();
            let mut doc = AutoCommit::new();
            SheetsDoc::add_sheet(&mut doc, "Sheet1");
            let table = crate::xlsx::SheetTable {
                name: "sales".into(),
                rows: vec![vec![Some(crate::xlsx::ImportedCell {
                    raw: "Region".into(),
                    format: CellFormat::General,
                    formula: None,
                    bold: false,
                })]],
            };
            app.import_tables(&mut doc, vec![table.clone()], None)
                .unwrap();
            assert_eq!(SheetsDoc::sheet_names(&mut doc), ["sales"]);
            assert_eq!(app.active_sheet, 0);
            app.import_tables(&mut doc, vec![table], None).unwrap();
            assert_eq!(
                SheetsDoc::sheet_names(&mut doc).len(),
                2,
                "a used workbook keeps its sheets"
            );
        }

        // Case `csv_export_then_import_roundtrips`.
        {
            export_then_import_roundtrip(
                "csv",
                SheetsApp::export_csv_to,
                SheetsApp::import_csv_from,
            );
        }

        // Case `sparse_csv_dimensions_refuse_before_dense_materialisation`.
        {
            let path = std::env::temp_dir()
                .join(format!("tilo-sheets-sparse-csv-{}.csv", std::process::id()));
            let payload = format!("{}x", "\n".repeat(MAX_CSV_MATERIALIZED_SLOTS));
            std::fs::write(&path, payload).expect("test CSV writes");
            let error = SheetsApp::read_import_tables(Interchange::Csv, &path)
                .expect_err("a sparse far-right cell must not allocate a huge rectangle");
            assert!(error.contains("dimensions require"), "{error}");
            let _ = std::fs::remove_file(path);
        }

        // Case `xlsx_export_then_import_roundtrips`.
        {
            export_then_import_roundtrip(
                "xlsx",
                SheetsApp::export_xlsx_to,
                SheetsApp::import_xlsx_from,
            );
        }

        // Case `ods_export_then_import_roundtrips`.
        {
            export_then_import_roundtrip(
                "ods",
                SheetsApp::export_ods_to,
                SheetsApp::import_ods_from,
            );
        }

        // Case `portal_drain_exports_then_imports_through_the_picked_path`.
        // The portal drain path: a Save outcome writes the active sheet, and an
        // Open outcome imports it back — driving the exact per-frame `drain_io`
        // the host file chooser feeds, answered by a recording host (no D-Bus
        // dialog).
        {
            let tmp =
                std::env::temp_dir().join(format!("tilo-sheets-portal-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).unwrap();
            let path = tmp.join("Exported.csv");

            let (mut app, mut doc) = seeded_app();

            // Answer the Save dialog as though the user picked `path`, then drain.
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            ui::host::scoped(&services, || app.begin_export(Interchange::Csv));
            assert!(recorder.pick_requests()[0].suggested_name().is_some());
            app.drain_io(&mut doc);
            // The file codec runs on the bounded worker. Drive the same frame pump
            // as a live app and wait by elapsed time, not an arbitrary count of
            // yields: under a saturated CI runner ten thousand yields can elapse
            // before the worker gets scheduled even though the operation is sound.
            sync_until(&mut app, &mut doc, |app| app.interchange_rx.is_none());
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

            // Answer an Open dialog with the same path and drain: it imports a sheet.
            let before = SheetsDoc::sheet_count(&mut doc);
            recorder.respond_to_pick(PortalOutcome::Opened(path));
            ui::host::scoped(&services, || app.begin_import(Interchange::Csv));
            assert_eq!(recorder.pick_requests().len(), 2);
            app.drain_io(&mut doc);
            sync_until(&mut app, &mut doc, |app| app.interchange_rx.is_none());
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

        // Case `portal_cancel_is_silent`.
        // A cancelled chooser clears the in-flight dialog and touches nothing.
        {
            let (mut app, mut doc) = seeded_app();
            let before = SheetsDoc::sheet_count(&mut doc);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || app.begin_import(Interchange::Xlsx));
            assert!(app.io.is_some(), "import asked the host chooser");
            app.drain_io(&mut doc);
            assert!(app.io.is_none());
            assert_eq!(
                SheetsDoc::sheet_count(&mut doc),
                before,
                "cancel imports nothing"
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
            app.fill_down(&mut doc);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 2)), "=SUM(B3:D3)");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (4, 3)), "=SUM(B4:D4)");
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

        // Case `fill_handle_drag_shifts_formula_refs`.
        // The fill handle on a formula cell fills down with the relative refs
        // shifted per row — the same adjustment copy/paste applies.
        {
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

        // Case `fill_handle_drag_fills_right`.
        // The fill handle also fills to the right, extending a series across
        // columns per row.
        {
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
            app.fill_right(&mut doc);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 1)), "=SUM(C2:E2)");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (6, 1)), "=SUM(D2:F2)");
            match value_at(&app, (5, 1)) {
                Some(CellValue::Number(n)) => assert!(n > 0.0, "filled formula recomputed to {n}"),
                other => panic!("expected numeric fill result, got {other:?}"),
            }
        }
    }

    /// Copy/cut/paste of values, formats and formulas, range tiling and external TSV.
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
            app.paste(&mut doc);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (1, 7)), "1200");
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
            app.paste(&mut doc);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 2)), "=SUM(C3:E3)");
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
            app.paste(&mut doc);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 0)), "");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (0, 20)), "Category");
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
            app.paste(&mut doc);
            // The formula is unchanged: a move keeps references pointing at B2:D2.
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (6, 10)), "=SUM(B2:D2)");
            assert_eq!(
                SheetsDoc::cell_raw(&mut doc, 0, (4, 1)),
                "",
                "source cleared"
            );
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
            app.paste(&mut doc);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 1)), "=SUM(C2:E2)");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 2)), "=SUM(C3:E3)");
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 0, (5, 3)), "=SUM(C4:E4)");
        }

        // Case `paste_tsv_parses_external_grid`.
        // Pasting a tab/newline grid from another app parses and smart-types each
        // field — the real system-clipboard branch of `paste` (`paste_tsv`).
        {
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

        // Case `annotation_controls_keep_a_normal_touch_target`.
        {
            let height = std::hint::black_box(ANNOTATION_ACTION_H);
            assert!(
                height >= 40.0,
                "chart/comment actions must not regress to text-line hit targets"
            );
        }

        // Case `narrow_large_text_collapses_secondary_toolbar_actions_without_clipping`.
        {
            let (mut app, _tr) = measured_app(560.0, 720.0, 2.0);
            let labels: Vec<&str> = app
                .menu_buttons
                .iter()
                .map(|button| button.label.as_str())
                .collect();
            assert_eq!(labels, vec!["File", "Format", "More"]);
            assert!(
                app.menu_buttons
                    .iter()
                    .all(|button| button.rect.x1 <= app.rect.x1)
            );
            app.open_menu = OpenMenu::More;
            app.relayout_overlays();
            assert_eq!(app.dropdown_rows.len(), 3);
            assert!(app.dropdown_rect.x0 >= app.rect.x0 && app.dropdown_rect.x1 <= app.rect.x1);
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
            assert_eq!(SheetsDoc::sheet_names(&mut doc)[0], "Ledger");
        }

        // Case `tab_rename_escape_cancels_without_a_document_write`.
        {
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

        // Case `tab_duplicate_creates_a_copy`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = SheetsDoc::sheet_count(&mut doc);
            app.run_tab_action(&mut doc, 0, TabAction::Duplicate);
            assert_eq!(SheetsDoc::sheet_count(&mut doc), before + 1);
            // Inserted right after the source and named "<name> copy".
            assert_eq!(SheetsDoc::sheet_names(&mut doc)[1], "Budget copy");
            // The copy carries the source's formulas.
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (4, 1)), "=SUM(B2:D2)");
        }

        // Case `tab_delete_removes_and_keeps_one_sheet`.
        {
            let (mut app, mut doc) = seeded_app();
            // Seed: Budget, Sales. Remove Sales.
            app.run_tab_action(&mut doc, 1, TabAction::Delete);
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Budget"]);
            // The last remaining sheet cannot be deleted away.
            app.run_tab_action(&mut doc, 0, TabAction::Delete);
            assert_eq!(SheetsDoc::sheet_count(&mut doc), 1);
        }

        // Case `tab_move_right_reorders_sheets`.
        {
            let (mut app, mut doc) = seeded_app();
            app.run_tab_action(&mut doc, 0, TabAction::MoveRight);
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Sales", "Budget"]);
            // The moved sheet keeps its data (Budget's E2 formula).
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (4, 1)), "=SUM(B2:D2)");
        }

        // Case `tab_set_and_clear_colour`.
        {
            let (mut app, mut doc) = seeded_app();
            app.run_tab_action(&mut doc, 0, TabAction::SetColor(Some(2)));
            assert_eq!(SheetsDoc::sheet_color(&mut doc, 0), Some(2));
            app.run_tab_action(&mut doc, 0, TabAction::SetColor(None));
            assert_eq!(SheetsDoc::sheet_color(&mut doc, 0), None);
        }
    }

    /// Blank launch, cache reuse/invalidation and persist/reload.
    #[test]
    fn workbook_launch_reload_and_persistence_round_trip() {
        // Case `blank_launch_opens_a_single_empty_sheet`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            // A real launch (persist on, but pointed at an empty temp data dir)
            // must NOT fabricate the sample workbook.
            let tmp =
                std::env::temp_dir().join(format!("tilo-sheets-blank-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            env.set("XDG_DATA_HOME", &tmp);

            let mut app = SheetsApp::new(); // RunMode::Live: blank, persisted
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            sync_until(&mut app, &mut doc, |app| app.pending.seeded);

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

        // Case `reload_reuses_unchanged_cache_and_invalidates_it_after_an_edit`.
        {
            let (mut app, mut doc) = seeded_app();
            // Commit the startup seed and refresh once at its steady-state heads.
            app.sync(&mut doc, &automerge::ROOT);
            let cached_heads = app
                .cache
                .as_ref()
                .expect("seed reload populates cache")
                .heads
                .clone();

            app.pending.redraw = false;
            app.reload(&mut doc);
            assert!(
                !app.pending.redraw,
                "unchanged reload should return before rebuilding"
            );
            assert_eq!(app.cache.as_ref().unwrap().heads, cached_heads);

            SheetsDoc::set_cell_raw(&mut doc, 0, (0, 0), "1234");
            app.pending.redraw = false;
            app.reload(&mut doc);

            let refreshed_heads = &app
                .cache
                .as_ref()
                .expect("edited reload refreshes cache")
                .heads;
            assert_ne!(refreshed_heads, &cached_heads);
            assert_eq!(refreshed_heads, &doc.get_heads());
            assert_eq!(value_at(&app, (0, 0)), Some(CellValue::Number(1234.0)));
            assert!(app.pending.redraw, "rebuilt grid must request a redraw");
        }

        // Case `workbook_persists_and_reloads`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let tmp =
                std::env::temp_dir().join(format!("tilo-sheets-persist-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            env.set("XDG_DATA_HOME", &tmp);

            // Session one: blank launch, edit a cell, persist.
            let mut app = SheetsApp::new();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            app.sync(&mut doc, &root);
            sync_until(&mut app, &mut doc, |app| app.pending.seeded);
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
            let expected_heads = doc.get_heads();
            sync_until(&mut app, &mut doc, |app| {
                app.persistence_rx.is_none() && app.saved_heads == expected_heads
            });
            assert!(
                workbook_path().unwrap().exists(),
                "workbook was not written"
            );

            // Session two: a fresh app + fresh document reopens the saved workbook.
            let mut app2 = SheetsApp::new();
            let mut doc2 = AutoCommit::new();
            app2.sync(&mut doc2, &root);
            sync_until(&mut app2, &mut doc2, |app| app.pending.seeded);
            assert_eq!(SheetsDoc::cell_raw(&mut doc2, 0, (0, 0)), "hello");

            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    /// Failed, coalesced, merged, disconnected and stale persistence keep heads honest.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn persistence_failures_never_lose_or_misacknowledge_edits() {
        // Case `failed_persistence_never_acknowledges_unsaved_heads`.
        {
            let (mut app, mut doc) = seeded_app();
            SheetsDoc::add_sheet(&mut doc, "Unsaved");
            let heads = doc.get_heads();
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(PersistenceResult::Save {
                generation: 7,
                heads: heads.clone(),
                result: Err("disk full".into()),
            })
            .unwrap();
            app.persistence_rx = Some(rx);
            app.active_persistence = Some((
                7,
                PersistenceJob::Save {
                    heads: heads.clone(),
                },
            ));
            app.drain_persistence_worker(&mut doc);

            assert_ne!(app.saved_heads, heads);
            assert!(app.last_persist_failure.is_some());
            assert!(app.status.contains("Could not save workbook"));
        }

        // Case `persistence_coalesces_edits_that_arrive_during_a_save`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let tmp = std::env::temp_dir().join(format!(
                "tilo-sheets-persist-coalesce-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&tmp);
            env.set("XDG_DATA_HOME", &tmp);
            let mut app = SheetsApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            sync_until(&mut app, &mut doc, |app| app.pending.seeded);

            app.apply_cmds(
                &mut doc,
                vec![GridCmd::SetCell {
                    addr: (0, 0),
                    raw: "first".into(),
                }],
            );
            let first_heads = doc.get_heads();
            app.start_persistence_save(&mut doc, first_heads.clone())
                .unwrap();
            // This mutation happens before the first worker result is received.
            app.apply_cmds(
                &mut doc,
                vec![GridCmd::SetCell {
                    addr: (1, 0),
                    raw: "second".into(),
                }],
            );
            let latest_heads = doc.get_heads();
            assert_ne!(first_heads, latest_heads);
            sync_until(&mut app, &mut doc, |app| {
                app.persistence_rx.is_none() && app.saved_heads == latest_heads
            });

            let bytes = std::fs::read(workbook_path().unwrap()).unwrap();
            let mut saved = AutoCommit::load(&bytes).unwrap();
            assert_eq!(SheetsDoc::cell_raw(&mut saved, 0, (1, 0)), "second");
            let _ = std::fs::remove_dir_all(&tmp);
        }

        // Case `incoming_change_during_load_is_saved_after_the_disk_workbook_merges`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            let tmp =
                std::env::temp_dir().join(format!("tilo-sheets-load-race-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            env.set("XDG_DATA_HOME", &tmp);
            let mut on_disk = AutoCommit::new();
            SheetsDoc::add_sheet(&mut on_disk, "Disk");
            SheetsDoc::set_cell_raw(&mut on_disk, 0, (0, 0), "saved");
            let path = workbook_path().unwrap();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, on_disk.save()).unwrap();

            let mut app = SheetsApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT); // starts load, never blocks for it
            SheetsDoc::add_sheet(&mut doc, "Arrived while loading");
            sync_until(&mut app, &mut doc, |app| {
                app.pending.seeded
                    && app.persistence_rx.is_none()
                    && app.active_persistence.is_none()
            });
            assert_eq!(app.saved_heads, doc.get_heads());

            let bytes = std::fs::read(path).unwrap();
            let mut saved = AutoCommit::load(&bytes).unwrap();
            let names = SheetsDoc::sheet_names(&mut saved);
            assert!(names.iter().any(|name| name == "Disk"));
            assert!(names.iter().any(|name| name == "Arrived while loading"));
            let _ = std::fs::remove_dir_all(&tmp);
        }

        // Case `disconnected_persistence_worker_leaves_document_dirty_and_retriable`.
        {
            let (mut app, mut doc) = seeded_app();
            SheetsDoc::add_sheet(&mut doc, "Retry me");
            let heads = doc.get_heads();
            let (tx, rx) = mpsc::sync_channel(1);
            drop(tx);
            app.persistence_rx = Some(rx);
            app.active_persistence = Some((
                9,
                PersistenceJob::Save {
                    heads: heads.clone(),
                },
            ));
            app.drain_persistence_worker(&mut doc);

            assert!(app.persistence_rx.is_none());
            assert!(app.active_persistence.is_none());
            assert_ne!(app.saved_heads, heads);
            assert!(app.last_persist_failure.is_some());
            assert!(app.status.contains("changes will be retried"));
        }

        // Case `stale_persistence_completion_cannot_advance_durable_heads`.
        {
            let (mut app, mut doc) = seeded_app();
            SheetsDoc::add_sheet(&mut doc, "Current");
            let current_heads = doc.get_heads();
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(PersistenceResult::Save {
                generation: 3,
                heads: current_heads.clone(),
                result: Ok(()),
            })
            .unwrap();
            app.persistence_rx = Some(rx);
            // Generation 4 is the only accepted job. A late A completion must not
            // overwrite its durable-head checkpoint; the inconsistent slot is
            // settled as retryable rather than leaving an orphaned B owner.
            app.active_persistence = Some((
                4,
                PersistenceJob::Save {
                    heads: current_heads.clone(),
                },
            ));
            app.drain_persistence_worker(&mut doc);

            assert_ne!(app.saved_heads, current_heads);
            assert!(app.active_persistence.is_none());
            assert!(app.last_persist_failure.is_some());
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
        let tmp = std::env::temp_dir().join(format!("tilo-sheets-io-{ext}-{}", std::process::id()));
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
        // CSV is deliberately one active table; real workbook formats carry
        // the whole two-tab workbook.
        let expected_added = if ext == "csv" { 1 } else { before };
        import_from(&mut app, &mut doc, &path);
        assert_eq!(
            SheetsDoc::sheet_count(&mut doc),
            before + expected_added,
            "import must restore every table carried by the format"
        );
        assert!(
            app.status.starts_with("Imported"),
            "import status: {}",
            app.status
        );
        // The re-imported sheet carries the Budget header text through the file.
        assert_eq!(SheetsDoc::cell_raw(&mut doc, before, (0, 0)), "Category");
        if ext != "csv" {
            assert_eq!(
                SheetsDoc::cell_raw(&mut doc, before + 1, (0, 0)),
                "Region",
                "the second workbook tab is not silently discarded"
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
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
            app.open_command_palette(&doc);
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
            app.open_command_palette(&doc);
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
            app.dispatch_palette_view_for_test(&doc);
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

    /// Sum-selection, row/column insertion, formula view and new-sheet commands.
    #[test]
    fn commands_edit_rows_columns_views_and_sheets() {
        // Case `command_sum_selection_writes_totals_below`.
        {
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

        // Case `command_insert_and_delete_row_shift_cells`.
        {
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

        // Case `command_insert_column_shifts_right`.
        {
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

        // Case `command_toggle_formula_view`.
        {
            let (mut app, mut doc) = seeded_app();
            assert!(!app.grid.formula_view());
            app.dispatch_command(&mut doc, "formula_view", None);
            assert!(app.grid.formula_view(), "formula view turned on");
            app.dispatch_command(&mut doc, "formula_view", None);
            assert!(!app.grid.formula_view(), "formula view toggled back off");
        }

        // Case `command_new_sheet_adds_a_sheet`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = SheetsDoc::sheet_count(&mut doc);
            app.dispatch_command(&mut doc, "new_sheet", None);
            assert_eq!(SheetsDoc::sheet_count(&mut doc), before + 1);
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

        // Case `loading_state_is_announced_as_busy_without_expanding_the_grid`.
        {
            use ui::widget::Widget as _;

            let (mut app, _doc) = seeded_app();
            app.shots_show_loading();
            let tree = app.accessibility().expect("spreadsheet semantics");
            let table = &tree.children[2];
            assert!(table.state.busy);
            assert!(table.children.len() < 30, "a11y remains viewport-bounded");
        }

        // Case `semantic_toolbar_and_tabs_use_the_same_live_controls`.
        {
            use ui::widget::Widget as _;

            let (mut app, mut doc) = seeded_app();
            let root = automerge::ROOT;
            assert!(app.perform_semantic_action(
                "sheets.toolbar.formula-view",
                &ui::a11y::Action::Activate,
                &mut doc,
                &root,
            ));
            assert!(app.grid.formula_view());
            assert!(app.perform_semantic_action(
                "sheets.name-box",
                &ui::a11y::Action::Focus,
                &mut doc,
                &root,
            ));
            assert!(app.name_box_edit.is_some());
            let target = format!("sheets.tab:{}", app.sheet_identity(1));
            assert!(app.perform_semantic_action(
                &target,
                &ui::a11y::Action::Activate,
                &mut doc,
                &root,
            ));
            assert_eq!(app.active_sheet, 1);
        }
    }

    #[test]
    fn hosted_directory_keeps_the_selection_without_the_workbook_root() {
        use ui::resources::PublishesResources;
        let mut app = SheetsApp::new();
        let doc = automerge::AutoCommit::new();
        let standalone = app
            .action_directory_resource_page(&doc, &[], 8)
            .expect("standalone directory");
        assert!(
            standalone
                .resources
                .iter()
                .any(|resource| resource.kind == "sheets.workbook"),
            "standalone directory publishes the workbook"
        );
        app.actions.mark_hosted_for_test();
        let hosted = app
            .action_directory_resource_page(&doc, &[], 8)
            .expect("hosted directory");
        assert!(
            hosted
                .resources
                .iter()
                .all(|resource| resource.kind != "sheets.workbook"),
            "hosted directory must not republish the workbook: {:?}",
            hosted
                .resources
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(hosted.resources.is_empty());
        assert!(app.resources(&doc).is_empty());
    }
}
