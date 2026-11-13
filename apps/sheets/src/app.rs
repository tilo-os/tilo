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
use ui::i18n::ArgValue;
use ui::portal::{FileFilter, PickRequest};
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Button, ColorSwatchPicker, ContextMenu, EditOutcome, EditableLabel, MenuItem as Row, Popover,
    PopoverSide, TextInput, Widget,
};

mod annotations;
mod cells;
mod chrome;
mod commands;
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
mod store;
mod window_bar;

use chrome::{ChromeMetrics, CommandSlot, MenuRow};
use overlays::SettingsSheet;

use crate::actions;
use crate::fill;
use crate::grid::{Grid, GridCmd, GridMetrics};
use crate::infer::infer_entry;
use crate::model::{Addr, CellFormat, CellValue, a1_to_addr, format_value};
use crate::resource::CATALOG;
use crate::resource::verbs;
use crate::settings::{DocSettings, EffectiveLocale};
use crate::workbook::{Cell, ChartKind, ChartSnapshot, CommentThread, Workbook};
use tilo_locale::{Currency, DateStyle, Locale};
use ui::clock::TimeStyle;

const TAB_MENU_ACTIONS: [TabAction; 6] = [
    TabAction::Rename,
    TabAction::Duplicate,
    TabAction::InsertRight,
    TabAction::MoveLeft,
    TabAction::MoveRight,
    TabAction::Delete,
];
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
const MAX_IMPORT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMPORT_CELLS: usize = crate::resource::MAX_IMPORTED_CELLS;
/// CSV is rectangular: a one-cell file at a far coordinate must not become a trillion-slot
/// dense vector.
const MAX_CSV_MATERIALIZED_SLOTS: usize = crate::xlsx::MAX_MATERIALIZED_SLOTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interchange {
    Csv,
    Xlsx,
    Ods,
    Notion,
}

impl Interchange {
    fn ext(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
            Self::Ods => "ods",
            Self::Notion => "zip",
        }
    }

    fn filter(self) -> FileFilter {
        match self {
            Self::Csv => FileFilter::new("CSV").glob("*.csv"),
            Self::Xlsx => FileFilter::new("Excel workbook").glob("*.xlsx"),
            Self::Ods => FileFilter::new("Calc spreadsheet").glob("*.ods"),
            Self::Notion => FileFilter::new(&CATALOG.t("sheets-notion-files"))
                .glob("*.zip")
                .glob("*.json"),
        }
    }
}

fn spreadsheet_import_request(format: Interchange) -> PickRequest {
    let request = if format == Interchange::Notion {
        let title = CATALOG.t("sheets-notion-import-title");
        PickRequest::open(&title, &title)
    } else {
        PickRequest::open("Import spreadsheet", "Import")
    };
    request.filter(format.filter()).all_files()
}

fn spreadsheet_export_request(format: Interchange, suggested: &str) -> PickRequest {
    PickRequest::save("Export spreadsheet", "Export", suggested).filter(format.filter())
}

#[derive(Clone, Debug, PartialEq)]
enum MenuItem {
    NewSheet,
    ImportCsv,
    ExportCsv,
    ImportXlsx,
    ExportXlsx,
    ImportOds,
    ImportNotion,
    ExportOds,
    SetFormat(CellFormat),
    ClearFormat,
    OpenDocSettings,
    ToggleFormulaView,
    ToggleBold,
    AutoSum,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SettingsPanel {
    None,
    Document,
}

#[derive(Clone, Copy, Default)]
struct Pending {
    redraw: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Staged {
    CommandPalette,
    FillSeries,
    PasteFormula,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingReveal {
    sheet_id: Option<Id>,
    addr: Addr,
    status: Option<String>,
    arriving: bool,
}

struct SettingChip {
    rect: Rect,
    kind: SettingKind,
}

struct PaletteRouteCtx<'a> {
    path: &'a ObjId,
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

#[derive(Clone, Copy, PartialEq)]
enum SettingKind {
    Currency(Option<Currency>),
    Date(Option<DateStyle>),
    Time(Option<TimeStyle>),
}

#[derive(Clone, Copy, PartialEq)]
enum TabAction {
    Rename,
    Duplicate,
    InsertRight,
    MoveLeft,
    MoveRight,
    Delete,
    SetColor(Option<u32>),
}

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

struct TabRename {
    idx: usize,
    rect: Rect,
    text_w: f64,
    editor: EditableLabel,
}

const TAB_PALETTE_LEN: usize = 6;

fn tab_palette_color(theme: &Theme, idx: u32) -> Color {
    ChartPalette::from_theme(theme).categorical(idx as usize)
}

fn opaque_participant_id(id: &str) -> bool {
    id.len() >= 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn span(lo: u32, hi: u32) -> u32 {
    hi.saturating_sub(lo).saturating_add(1)
}

fn delta(from: u32, to: u32) -> i64 {
    i64::from(to).saturating_sub(i64::from(from))
}

fn offset_addr(base: Addr, dc: u32, dr: u32) -> Addr {
    (base.0.saturating_add(dc), base.1.saturating_add(dr))
}

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
    text_scale: f64,
    rect: Rect,
    formula_rect: Rect,
    grid_rect: Rect,
    tabs_rect: Rect,
    command_slot: CommandSlot,
    bar_commands: ui::framework::BarCommands<MenuItem>,
    /// The file the workbook was opened from, which names it; `None` for
    /// one never saved.
    file: Option<PathBuf>,
    chrome_theme: Theme,
    settings_sheet: Option<SettingsSheet>,
    metrics: ChromeMetrics,
    tab_rects: Vec<(Rect, usize)>,
    add_tab_rect: Rect,
    sheet_colors: Vec<Option<u32>>,
    tab_menu_target: Option<usize>,
    tab_menu: ContextMenu,
    tab_menu_rows: Vec<Option<overlays::TabMenuRow>>,
    tab_colour: Popover<ColorSwatchPicker>,
    tab_colour_shown: Option<(usize, Option<usize>)>,
    cell_menu: ContextMenu,
    cell_menu_rows: Vec<Option<CellMenuRow>>,
    cell_menu_target: Option<Addr>,
    find_matches: Vec<FoundCell>,
    find_current: Option<usize>,
    pending_find_replace: Vec<(usize, Addr, String)>,
    pending_ensure_visible: bool,
    active_cell_bold: bool,
    staged_overlay: Option<String>,
    tab_rename: Option<TabRename>,
    name_box_rect: Rect,
    fx_rect: Rect,
    name_box_edit: Option<TextInput>,
    formula_field_rect: Rect,
    formula_char_x: Vec<f64>,
    formula_scroll_x: f64,
    formula_bar_selecting: bool,
    sheet_names: Vec<String>,
    /// Durable chart/comment records for the active sheet; refreshed together
    /// with its formula snapshot, never created as an app-local side store.
    sheet_charts: Vec<ChartSnapshot>,
    sheet_comments: Vec<CommentThread>,
    annotation_clicks: Vec<(Rect, AnnotationClick)>,
    managed_chart: Option<String>,
    dock_rect: Rect,
    dock_cards: Vec<(annotations::DockCard, Rect)>,
    dock_scroll: ui::widget::ScrollRegion,
    /// Each tab's durable sheet identity, which the edits it offers name.
    sheet_ids: Vec<Id>,
    workbook: Rc<Workbook>,
    revision: u64,
    live: Option<ui::framework::Live<Workbook>>,
    active_sheet: usize,
    status: String,
    status_notice: ui::widget::toast::Transient<String>,
    status_said: String,
    paint_damage: ui::widget::paint_damage::PaintDamage,
    staged: Option<Staged>,
    pending: Pending,
    last_press: Option<(Instant, Addr)>,
    last_mouse: Point,
    current_active_format: CellFormat,
    settings_panel: SettingsPanel,
    global: Locale,
    locale: Option<ui::framework::Setting<Locale>>,
    doc_settings: DocSettings,
    cache: Option<ReloadCache>,
    calc: crate::calc::WorkbookCalc,
    pending_reveal: Option<PendingReveal>,
    clipboard: Option<ClipboardGrid>,
    search: ui::search::SearchOpener,
    dispatch: Option<ui::framework::Dispatcher<Workbook>>,
    people: Option<ui::framework::People>,
    opening: Rc<std::cell::RefCell<Option<PendingReveal>>>,
    store: store::Shared,
}

struct ClipboardGrid {
    src: Addr,
    cells: Vec<Vec<(String, CellFormat)>>,
    cut: bool,
    tsv: String,
}

/// The recomputed view of the active sheet, kept so format lookups do not re-run the engine.
struct ReloadCache {
    revision: u64,
    sheet: usize,
    eff_fmt: HashMap<Addr, CellFormat>,
    values: HashMap<Addr, CellValue>,
}

impl Default for SheetsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl SheetsApp {}

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

fn tsv_cell(display: &str) -> String {
    display.replace(['\t', '\n', '\r'], " ")
}

fn sheet_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(std::string::ToString::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported".to_string())
}

fn import_source(path: &std::path::Path) -> String {
    ui::record::RecordId::identify_file(path).to_string()
}

/// A reference takes every printable character but whitespace, so invalid input is reported.
fn reference_box_char(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control()
}

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
        let asks = std::mem::take(&mut self.store.borrow_mut().asks);
        for ask in asks {
            self.take_ask(ask);
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
        self.share_view();
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        self.refresh_chrome_metrics(text_renderer, theme);
        if self.pending_ensure_visible {
            self.pending_ensure_visible = false;
            self.grid.ensure_active_visible();
        }
        self.apply_staged_overlay();
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
            self.draw_comment_markers(scene, transform, theme, presence);
        });
        parts.draw(scene, "sheet-cell-presence", window, |scene| {
            self.draw_cell_presence(scene, transform, tr, theme, presence);
        });
        parts.draw(scene, "sheet-annotations", window, |scene| {
            self.draw_annotation_dock(scene, transform, tr, theme);
        });
        parts.draw(scene, "sheet-comment", window, |scene| {
            self.draw_comment_popover(scene, transform, tr, theme, presence);
        });
        parts.draw(scene, "sheet-tab-rename", window, |scene| {
            self.draw_tab_rename(scene, transform, tr, theme);
        });
        parts.draw(scene, "sheet-tab-presence", window, |scene| {
            self.draw_sheet_tab_presence(scene, transform, theme, presence);
        });
        self.paint_damage
            .record(window, parts.finish(scene, vec![window], transform, theme));
        self.share_view();
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

    fn needs_animation_frame(&self) -> bool {
        self.grid.is_animating()
            || self.dock_scroll.needs_animation_frame(&self.chrome_theme)
            || self.children().iter().any(|c| c.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(self.status_notice.deadline())
            .min()
    }

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
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.route_find_shortcut(key, event_ctx)
        {
            return;
        }
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
                    event_ctx.handled,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.grid.pointer_up();
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
            WindowEvent::Ime(ime) => {
                if self.grid.ime(ime) {
                    self.pending.redraw = true;
                    *event_ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_keyboard_input(
                    event,
                    *event_ctx.modifiers,
                    &mut event_ctx.state,
                    event_ctx.handled,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SheetsApp"
    }

    fn wants_tab_input(&self) -> bool {
        self.settings_panel == SettingsPanel::None && self.name_box_edit.is_none()
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.grid
            .is_editing()
            .then_some(ui::ime::InputContent::TEXT)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.grid.ime_cell_rect()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete custom-canvas semantic tree stays co-located with the widget contract so its virtual bounds and interaction state remain auditable"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let active = self.grid.active();
        let active_name = crate::model::addr_to_a1(active.0, active.1);
        let toolbar_controls: Vec<AccessNode> = [
            AccessNode::new(Role::Button)
                .with_name(crate::resource::CATALOG.t("sheets-start-formula"))
                .with_description(crate::resource::CATALOG.t("sheets-start-formula-detail"))
                .with_action_target("sheets.fx")
                .with_bounds(self.fx_rect)
                .with_state(AccessState::new().focusable(true)),
            AccessNode::new(Role::TextInput)
                .with_name(crate::resource::CATALOG.t("sheets-name-box"))
                .with_description(crate::resource::CATALOG.t("sheets-name-box-detail"))
                .with_value(self.grid.selection_label())
                .with_action_target("sheets.name-box")
                .with_bounds(self.name_box_rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.name_box_edit.is_some()),
                ),
            AccessNode::new(Role::TextInput)
                .with_name(crate::resource::CATALOG.t("sheets-formula-bar"))
                .with_description(crate::resource::CATALOG.t("sheets-formula-bar-detail"))
                .with_value(self.grid.active_display_raw())
                .with_action_target("sheets.formula-bar")
                .with_bounds(self.formula_field_rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.grid.is_editing()),
                ),
            AccessNode::new(Role::Button)
                .with_name(crate::resource::CATALOG.t("sheets-add-sheet"))
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
        // The painted grid is virtualised, so its semantic tree exposes the visible rows only, with
        // the full table's context for readers.
        let window = self.grid.accessibility_window();
        let header_cells = window
            .columns
            .iter()
            .map(|&column| {
                let letter = crate::model::col_to_letters(column);
                AccessNode::new(Role::Cell)
                    .with_name(CATALOG.t_args(
                        "sheets-a11y-column",
                        &[("letter", ArgValue::Text(letter.clone()))],
                    ))
                    .with_value(letter)
                    .with_position(column as usize + 1, window.total_columns as usize)
            })
            .collect();
        let mut rows = Vec::with_capacity(window.rows.len().saturating_add(1));
        rows.push(
            AccessNode::new(Role::Row)
                .with_name(crate::resource::CATALOG.t("sheets-column-headers"))
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
                        .with_description(CATALOG.t_args(
                            "sheets-a11y-cell",
                            &[
                                ("column", ArgValue::Text(column)),
                                ("row", ArgValue::Text((cell.addr.1 + 1).to_string())),
                            ],
                        ))
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
                .with_name(CATALOG.t_args(
                    "sheets-a11y-row",
                    &[("row", ArgValue::Text((row.index + 1).to_string()))],
                ))
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
            .with_description(CATALOG.t_args(
                "sheets-a11y-worksheet",
                &[
                    ("range", ArgValue::Text(self.grid.selection_label())),
                    ("columns", ArgValue::Text(window.total_columns.to_string())),
                    ("rows", ArgValue::Text(window.total_rows.to_string())),
                ],
            ))
            .with_value(format!("Active cell {active_name}"))
            .with_bounds(window.bounds)
            .with_state(AccessState::new().focusable(true))
            .with_children(rows);
        let annotations = AccessNode::new(Role::List)
            .with_name(crate::resource::CATALOG.t("sheets-comments-charts"))
            .with_children(
                self.sheet_charts
                    .iter()
                    .map(|chart| {
                        let target = format!("sheets.annotation:chart:{}", chart.id);
                        AccessNode::new(Role::ListItem)
                            .with_name(format!("{} chart", chart.title))
                            .with_description(CATALOG.t_args(
                                "sheets-a11y-chart-extent",
                                &[
                                    (
                                        "start",
                                        ArgValue::Text(crate::model::addr_to_a1(
                                            chart.start.0,
                                            chart.start.1,
                                        )),
                                    ),
                                    (
                                        "end",
                                        ArgValue::Text(crate::model::addr_to_a1(
                                            chart.end.0,
                                            chart.end.1,
                                        )),
                                    ),
                                ],
                            ))
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-manage-chart"))
                                    .with_action_target(format!("{target}:manage"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(
                                        crate::resource::CATALOG.t("sheets-open-chart-range"),
                                    )
                                    .with_action_target(format!("{target}:open"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-rename-chart"))
                                    .with_value(chart.title.clone())
                                    .with_action_target(format!("{target}:edit"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-column-chart"))
                                    .with_action_target(format!("{target}:column"))
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .selected(chart.kind == ChartKind::Column),
                                    ),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-line-chart"))
                                    .with_action_target(format!("{target}:line"))
                                    .with_state(
                                        AccessState::new()
                                            .focusable(true)
                                            .selected(chart.kind == ChartKind::Line),
                                    ),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-chart-cells"))
                                    .with_value(format!(
                                        "{}:{}",
                                        crate::model::addr_to_a1(chart.start.0, chart.start.1),
                                        crate::model::addr_to_a1(chart.end.0, chart.end.1),
                                    ))
                                    .with_action_target(format!("{target}:cells"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-delete-chart"))
                                    .with_action_target(format!("{target}:delete"))
                                    .with_state(AccessState::new().focusable(true)),
                            ])
                    })
                    .chain(self.sheet_comments.iter().map(|thread| {
                        let target = format!("sheets.annotation:comment:{}", thread.id);
                        AccessNode::new(Role::ListItem)
                            .with_name(CATALOG.t_args(
                                "sheets-a11y-comment-by",
                                &[("author", ArgValue::Text(self.author_name(&thread.author)))],
                            ))
                            .with_description(format!(
                                "{}; {} replies; {}",
                                thread.body,
                                thread.replies.len(),
                                if thread.resolved { "resolved" } else { "open" }
                            ))
                            .with_children(vec![
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-reply-comment"))
                                    .with_action_target(format!("{target}:reply"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-edit-comment"))
                                    .with_value(thread.body.clone())
                                    .with_action_target(format!("{target}:edit"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(
                                        crate::resource::CATALOG.t("sheets-open-comment-anchor"),
                                    )
                                    .with_action_target(format!("{target}:open"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(crate::resource::CATALOG.t("sheets-delete-comment"))
                                    .with_action_target(format!("{target}:delete"))
                                    .with_state(AccessState::new().focusable(true)),
                                AccessNode::new(Role::Button)
                                    .with_name(if thread.resolved {
                                        crate::resource::CATALOG.t("sheets-reopen-comment")
                                    } else {
                                        crate::resource::CATALOG.t("sheets-resolve-comment")
                                    })
                                    .with_action_target(format!("{target}:toggle"))
                                    .with_state(AccessState::new().focusable(true)),
                            ])
                    }))
                    .collect(),
            );
        let children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name(crate::resource::CATALOG.t("sheets-actions"))
                .with_children(toolbar_controls),
            AccessNode::new(Role::TabBar)
                .with_name(crate::resource::CATALOG.t("sheets-window-name"))
                .with_children(tabs),
            sheet,
            annotations,
        ];
        Some(
            AccessNode::new(Role::Document)
                .with_name(crate::resource::CATALOG.t("sheets-window-name"))
                .with_description(crate::resource::CATALOG.t("sheets-window-detail"))
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

    /// The grid fills its node in `apply_layout`, so there is never a size to measure.
    fn layout_needs_measure(&self) -> bool {
        false
    }
}

impl ui::presence::capability::PublishesPresence for SheetsApp {
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
        self.search.take()
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        use ui::widget::Widget as _;
        ui::widget::semantic_action_bindings(self.accessibility())
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

pub(crate) use store::window;

#[derive(PartialEq)]
struct SelectionKey {
    revision: u64,
    sheet: usize,
    bounds: (Addr, Addr),
    active: Addr,
    selected: Vec<Addr>,
    currency: tilo_locale::Currency,
}
