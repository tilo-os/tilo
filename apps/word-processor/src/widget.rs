//! The word processor's editor and its window: the document as a centred page, drawn with
//! `RichTextLayout`, with pointer and keyboard mapped onto the editing model.

use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(test)]
use ui::widget::Edits;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::Modifiers;

use tilo_doc::model::Region;
use tilo_doc::paging::PageFlow;
use tilo_doc::snapshot::{BlockView, NoteView};
use ui::PresenceCtx;
use ui::gesture::DragSelect;
use ui::richtext::RichTextLayout;
use ui::text_edit::EditCore;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
// Gives `vello::Color` its `.r()/.g()/.b()`.
use tilo_rich_text::{Draft, RichTextSurface, RichTextView, SubmitKey};
use ui::ColorCompat;
#[cfg(test)]
use ui::clipboard_copy;
use ui::permissions::UserId;
#[cfg(test)]
use ui::widget::Widget;
use ui::widget::context_menu::ContextMenu;
use ui::widget::toast::Notice;
use ui::widget::{BlockEditor, Embedded};
use ui::{PathSegment, PresenceKey};

use crate::WpFormat;
use crate::compact_swatch_palette::CompactSwatchPalette;
use crate::document_find::DocumentFind;
use crate::file_menu::FileMenu;
use crate::i18n::t;
use crate::num;
use crate::outline_pane::OutlinePane;
#[cfg(test)]
use crate::review_overlay::{DELETION_RGB, INSERTION_RGB};
use crate::review_overlay::{ReviewAction, ReviewOverlay};
use crate::ruler::Ruler;
#[cfg(test)]
use crate::ruler::RulerMarker;
use crate::style_color_picker::StyleColorPicker;
use crate::style_menu::StyleMenu;
use crate::text_offsets::{byte_to_char, char_to_byte};
use crate::word_processor::{Caret, ClipBlock, WordProcessor};
use crate::zoom_control::{MAX as ZOOM_MAX, MIN as ZOOM_MIN, STEP as ZOOM_STEP, ZoomControl};
use ui::portal;
use ui::widget::{ControlSize, SegmentedTouch};

mod accessibility;
mod block_layout;
mod controls;
#[cfg(test)]
mod damage_tests;
mod dispatch;
mod find;
#[cfg(test)]
mod find_cover_tests;
#[cfg(test)]
mod format_click_tests;
mod image_layout;
mod images;
mod input;
mod layout_cache;
mod pagination;
mod paint;
mod panels;
#[cfg(test)]
mod participant_tests;
mod participants;
mod proofing;
mod review;
mod semantic_geometry;
mod text_runs;
mod toolbar;
mod traits;
mod window;
mod window_bar;

use pagination::page_stack_to_flow;

#[cfg(test)]
use block_layout::LIST_MARKER_TEXT_GAP;
use block_layout::line_spacing_label;
#[cfg(test)]
use image_layout::PaintOptions as ImagePaintOptions;
pub use proofing::WriterLayout;
use proofing::misspelled_spans;
#[cfg(test)]
use toolbar::ToolAction;
pub use window::WordProcessorWidget;

fn to_spans(ranges: Vec<(String, usize, usize)>) -> Vec<crate::actions::Span> {
    ranges
        .into_iter()
        .map(|(block, start, end)| crate::actions::Span { block, start, end })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyPressEffect {
    Ignored,
    Motion,
    Edit,
}

/// What a right-click menu row does: the shared [`ContextMenu`]'s callbacks cannot mutate
/// the widget, so the rows carry these and dispatch by clicked index.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CtxAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    AddToDictionary(String),
    ApplySuggestion {
        block: String,
        start: usize,
        end: usize,
        replacement: String,
    },
    Review(ReviewAction),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocumentAutocorrection {
    block: String,
    start: usize,
    end: usize,
    boundary_end: usize,
    original: String,
    replacement: String,
}

// Paper stays light in both themes: the document ink assumes a light sheet, and only
// chrome follows the theme.
/// The canonical sheet; the ink, selection and caret below read on it in every theme.
const PAPER_DEFAULT: Color = Color::from_rgb8(0xfc, 0xfc, 0xfa);
const PAPER_INK: Color = Color::from_rgb8(0x1c, 0x1c, 0x20);
const PAPER_MUTED: Color = Color::from_rgb8(0x8a, 0x8a, 0x92);
const PAPER_SELECTION_ALPHA: u8 = 0x4d;
const ACTIVE_BAND_FILL_ALPHA: u8 = 0x14;
const BODY_DIM_VEIL_ALPHA: u8 = 0x66;

/// The colours the page draws with, resolved each frame from the [`Theme`].
#[derive(Clone, Copy)]
struct ChromeColors {
    paper_accent: Color,
    on_paper_accent: Color,
    paper_danger: Color,
    paper_tint: Color,
    paper_edge: Color,
    /// The paper sheet: never darker than the window behind it in a light theme, and the
    /// canonical sheet under a dark one, so the document ink always reads.
    paper: Color,
    paper_elevation: ui::tokens::Shadow,
}

impl ChromeColors {
    fn for_theme(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        // Paper on a light theme's raised tier reads grey, as if disabled: take the lightest of
        // that tier, the window ground and the canonical sheet.
        let paper = [c.surfaces.raised, c.surfaces.content, PAPER_DEFAULT]
            .into_iter()
            .filter(|colour| luminance(*colour) >= 0.85)
            .fold(PAPER_DEFAULT, |lightest, colour| {
                if luminance(colour) > luminance(lightest) {
                    colour
                } else {
                    lightest
                }
            });
        let non_text = ui::tokens::colors::NON_TEXT_MIN_CONTRAST;
        let paper_accent = c.ink_on(paper, c.intent.you, non_text);
        let on_paper = theme.on_surface(paper);
        Self {
            paper,
            paper_accent,
            on_paper_accent: theme.on_surface(paper_accent).ink,
            paper_danger: c.ink_on(paper, c.semantic.danger, non_text),
            paper_tint: on_paper.tint,
            paper_edge: ui::surface::edge_on(paper),
            paper_elevation: theme.tokens.elevation.floating,
        }
    }
}

fn luminance(c: Color) -> f64 {
    let r = f64::from(c.r()) / 255.0;
    let g = f64::from(c.g()) / 255.0;
    let b = f64::from(c.b()) / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

const PAGE_TOP: f64 = 28.0;
const PAGE_KEEPS: f64 = MAX_PAGE_W / 2.0;
const OUTLINE_DOCKS_FROM: f64 = ui::tokens::Metrics::BASE.panel_min_width as f64 + PAGE_KEEPS;
const INSPECTOR_DOCKS_FROM: f64 =
    ui::tokens::Metrics::BASE.inspector_width as f64 + MAX_PAGE_W + 48.0;
const MAX_PAGE_W: f64 = 820.0;
const VIEWPORT_BLOCK_WINDOW: usize = 192;
const ESTIMATED_BLOCK_EXTENT: f64 = 34.0;
const VIRTUALIZE_BODY_AFTER: usize = VIEWPORT_BLOCK_WINDOW * 2;
const PAGE_GAP: f64 = 22.0;
const CARET_WIDTH: f64 = 2.0;
const SQUIGGLE_AMP: f64 = 1.6;
const SQUIGGLE_PERIOD: f64 = 4.0;
const NOTE_SEP: Color = Color::from_rgb8(0xb8, 0xb8, 0xbe);
const NOTE_INK: Color = Color::from_rgb8(0x3a, 0x3a, 0x40);
const NOTE_CONTENT_PX: f32 = 11.0;
const NOTE_AREA_GAP: f64 = 10.0;

const GUIDE_GRAB: f64 = 7.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum LayoutMode {
    None,
    Margins,
    Spacing,
}

#[derive(Clone, Copy, Debug)]
struct Edges {
    top: f64,
    right: f64,
    bottom: f64,
    left: f64,
}

impl Edges {
    fn uniform(v: f64) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
    fn get(&self, s: Side) -> f64 {
        match s {
            Side::Top => self.top,
            Side::Right => self.right,
            Side::Bottom => self.bottom,
            Side::Left => self.left,
        }
    }
    fn set(&mut self, s: Side, v: f64) {
        match s {
            Side::Top => self.top = v,
            Side::Right => self.right = v,
            Side::Bottom => self.bottom = v,
            Side::Left => self.left = v,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DragKind {
    Pair,
    Single,
    Proportional,
}

#[derive(Clone, Copy)]
struct GuideDrag {
    mode: LayoutMode,
    side: Side,
    kind: DragKind,
}

/// A free-object move: the document is written on release only, as one action that gets
/// the same stale-target refusal as a placed one.
#[derive(Clone)]
struct ObjectDrag {
    id: String,
    start_pointer: Point,
    start_x: f64,
    start_y: f64,
    page: usize,
    x: f64,
    y: f64,
    moved: bool,
}

impl Side {
    fn opposite(self) -> Side {
        match self {
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NumField {
    Size,
    SpaceBefore,
    SpaceAfter,
    Indent,
}

impl NumField {
    fn get(self, s: &crate::styles::StyleProps) -> f32 {
        match self {
            NumField::Size => s.size,
            NumField::SpaceBefore => s.space_before,
            NumField::SpaceAfter => s.space_after,
            NumField::Indent => s.indent,
        }
    }
    fn clamp(self, v: f32) -> f32 {
        match self {
            NumField::Size => v.clamp(6.0, 200.0),
            _ => v.clamp(0.0, 200.0),
        }
    }
    fn set(self, s: &mut crate::styles::StyleProps, v: f32) {
        let v = self.clamp(v);
        match self {
            NumField::Size => s.size = v,
            NumField::SpaceBefore => s.space_before = v,
            NumField::SpaceAfter => s.space_after = v,
            NumField::Indent => s.indent = v,
        }
    }
    fn input(self) -> ui::widget::NumberInput {
        let (min, max) = match self {
            NumField::Size => (6.0, 200.0),
            _ => (0.0, 200.0),
        };
        ui::widget::NumberInput::new(min)
            .with_range(min, max)
            .with_step(f64::from(self.step()))
            .control_size(ControlSize::Small)
            .compact()
            .with_accessible_name(self.label())
    }
    fn step(self) -> f32 {
        match self {
            NumField::Size => 1.0,
            _ => 2.0,
        }
    }
    fn semantic_target(self) -> String {
        format!(
            "word-processor.styles:number:{}",
            match self {
                Self::Size => "size",
                Self::SpaceBefore => "space-before",
                Self::SpaceAfter => "space-after",
                Self::Indent => "indent",
            }
        )
    }
    fn label(self) -> String {
        t(match self {
            NumField::Size => "panel-num-size",
            NumField::SpaceBefore => "panel-num-space-before",
            NumField::SpaceAfter => "panel-num-space-after",
            NumField::Indent => "panel-num-indent",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PanelHit {
    Close,
    Pick(String),
    ParaAlign(&'static str),
    SetColor([u8; 3]),
    OpenColorPicker,
    NewStyle,
    ImageScale(f64),
    ImageFlipH,
    ImageFlipV,
    /// Inset every edge by five percent; a crop that leaves too little source is refused.
    ImageCropInset,
    ImageCropReset,
    ImageInline,
    ImageFreePosition,
    ImageWrapLeft,
    ImageWrapRight,
}

fn zoom_field() -> ui::widget::NumberInput {
    ui::widget::NumberInput::new(100.0)
        .with_range(ZOOM_MIN * 100.0, ZOOM_MAX * 100.0)
        .with_step(ZOOM_STEP * 100.0)
        .with_suffix("%")
        .control_size(ControlSize::Small)
        .compact()
        .with_accessible_name(t("a11y-zoom"))
}

fn line_spacing_menu() -> ui::widget::Dropdown {
    let labels: Vec<String> = block_layout::LINE_SPACINGS
        .iter()
        .map(|spacing| line_spacing_label(*spacing))
        .collect();
    ui::widget::Dropdown::from_state(labels, &ui::LocalState::new(String::new()))
        .control_size(ControlSize::Small)
        .with_accessible_name(t("a11y-line-spacing"))
}

fn style_pick_menu() -> ui::widget::Dropdown {
    ui::widget::Dropdown::from_state(Vec::new(), &ui::LocalState::new(String::new()))
        .control_size(ControlSize::Small)
        .with_accessible_name(t("panel-style"))
}

fn inline_mark(name: &str) -> Option<crate::actions::editing::InlineMark> {
    use crate::actions::editing::InlineMark;
    Some(match name {
        "strong" => InlineMark::Strong,
        "em" => InlineMark::Em,
        "underline" => InlineMark::Underline,
        "strike" => InlineMark::Strike,
        "super" => InlineMark::Superscript,
        "sub" => InlineMark::Subscript,
        _ => return None,
    })
}

/// Per-line `(top, height)` of a shaped layout, to split a too-tall block across pages.
#[allow(
    clippy::while_float,
    reason = "line_h/step are clamped to >= 1.0 above each loop, so every \
              iteration strictly advances past a fixed floor — this can't \
              spin on float imprecision, only run a bounded number of times"
)]
fn layout_line_tops(layout: &RichTextLayout, text: &str) -> Vec<(f64, f64)> {
    let mut tops: Vec<(f64, f64)> = layout
        .selection_rects(0, text.len().max(1))
        .into_iter()
        .map(|(_, y0, _, y1)| (f64::from(y0), f64::from(y1 - y0)))
        .collect();
    // `selection_rects` may span a line several times: one entry per top, the larger height.
    tops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    tops.dedup_by(|b, a| {
        if (a.0 - b.0).abs() < 0.5 {
            a.1 = a.1.max(b.1);
            true
        } else {
            false
        }
    });
    let total_h = f64::from(layout.height());
    if tops.is_empty() {
        return vec![(0.0, total_h.max(1.0))];
    }
    let line_h = tops.first().map_or(1.0, |&(_, h)| h).max(1.0);
    while tops.first().is_some_and(|&(top, _)| top > line_h * 0.5) {
        let first_top = tops.first().map_or(0.0, |&(top, _)| top);
        tops.insert(0, (first_top - line_h, line_h));
    }
    let Some(&(last_top, last_h)) = tops.last() else {
        return tops;
    };
    let mut next = last_top + last_h;
    let step = last_h.max(1.0);
    while next + step * 0.5 < total_h {
        tops.push((next, step));
        next += step;
    }
    tops
}

fn is_scope_digest_key(key: &PresenceKey) -> bool {
    matches!(key, PresenceKey::Widget { path }
        if matches!(path.first(), Some(PathSegment::Field(field)) if field == ui::presence::DIGEST_KEY_FIELD))
}

struct Copied<T>(Option<std::rc::Rc<T>>);

impl<T> Default for Copied<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> Copied<T> {
    fn see(&mut self, current: &std::rc::Rc<T>) -> bool {
        if self
            .0
            .as_ref()
            .is_some_and(|seen| std::rc::Rc::ptr_eq(seen, current))
        {
            return false;
        }
        self.0 = Some(std::rc::Rc::clone(current));
        true
    }
}

impl<T: Clone> Copied<T> {
    fn take_if_new(&mut self, current: &std::rc::Rc<T>) -> Option<T> {
        self.see(current).then(|| T::clone(current))
    }
}

fn mark_comments(view: &mut BlockView, comments: &[tilo_doc::CommentView]) {
    view.marks
        .retain(|mark| mark.name != tilo_doc::schema::MARK_COMMENT);
    for comment in comments {
        if let Some((block, start, end)) = &comment.range
            && *block == view.id
        {
            view.marks.push(tilo_doc::document::MarkSpan {
                name: tilo_doc::schema::MARK_COMMENT.to_owned(),
                start: *start,
                end: *end,
                value: tilo_doc::document::MarkValue::Str(comment.id.clone()),
            });
        }
    }
}

#[derive(Default)]
struct CopiedParts {
    blocks: Vec<std::rc::Rc<BlockView>>,
    header: Copied<Vec<BlockView>>,
    footer: Copied<Vec<BlockView>>,
    notes: Copied<Vec<NoteView>>,
    comments: Vec<tilo_doc::CommentView>,
    changes: Copied<Vec<tilo_doc::ChangeView>>,
    content: Copied<std::collections::BTreeSet<String>>,
}

/// The parts of the document state one refresh found changed.
#[derive(Default)]
struct PartsUpdate {
    header: Option<Vec<BlockView>>,
    footer: Option<Vec<BlockView>>,
    notes: Option<Vec<NoteView>>,
    content: bool,
    review: Option<(Vec<tilo_doc::CommentView>, Vec<tilo_doc::ChangeView>)>,
}

/// The presence key of a paragraph: its stable CRDT block id, not its index, so a peer's
/// caret stays on the same paragraph while the document is restructured. `apps/agent`
/// reproduces this key shape from outside.
fn block_presence_key(block_id: &str) -> PresenceKey {
    ui::framework::ResourceSelection::Item(block_id.to_owned())
        .presence::<tilo_document::RichText>(
            <crate::framework::WordProcessorApp as ui::framework::ResourceApp>::ID,
        )
        .0
}

pub use ui::ParticipantPresence;

fn participant_presence_payload(
    view: &crate::actions::participant::ParticipantView,
    doc: &AutoCommit,
) -> Option<ui::PresencePayload> {
    let ctx = view.ctx(doc)?;
    let containers = crate::model::open(doc).ok()?;
    let obj = tilo_doc::model::region_block_obj(
        doc,
        &containers,
        view.region.to_region(),
        &view.caret.block,
    )?;
    let text = tilo_doc::model::block_text_on(doc, &obj).unwrap_or_default();
    if let Some(span) = ctx
        .format_spans
        .iter()
        .find(|span| span.block == view.caret.block && span.start < span.end)
    {
        return Some(ui::PresencePayload::Selection {
            start: char_to_byte(&text, span.start),
            end: char_to_byte(&text, span.end),
        });
    }
    Some(ui::PresencePayload::Caret {
        pos: char_to_byte(&text, view.caret.offset),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteCaretOverlay {
    pub user: UserId,
    pub rect: Rect,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteSelectionOverlay {
    pub user: UserId,
    pub rects: Vec<Rect>,
}

type TableRow<'a> = (f64, Vec<(usize, f64, &'a BlockView)>);

type Pass2Inputs = (
    bool,
    HashMap<String, usize>,
    HashMap<String, Vec<(usize, usize)>>,
);

#[allow(
    clippy::while_float,
    reason = "x advances by the fixed positive SQUIGGLE_PERIOD/2.0 each turn \
              (clamped to x1), so this is bounded by (x1-x0)/step, not float \
              imprecision"
)]
fn draw_squiggle(scene: &mut Scene, transform: Affine, color: Color, x0: f64, x1: f64, y: f64) {
    if x1 <= x0 {
        return;
    }
    let mut path = vello::kurbo::BezPath::new();
    let mut x = x0;
    let mut up = false;
    path.move_to((x0, y));
    while x < x1 {
        x = (x + SQUIGGLE_PERIOD / 2.0).min(x1);
        let dy = if up { -SQUIGGLE_AMP } else { SQUIGGLE_AMP };
        path.line_to((x, y + dy));
        up = !up;
    }
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        color,
        None,
        &path,
    );
}

/// A block laid out for the current frame, for click hit-testing; the shaped layout itself
#[derive(Clone)]
struct Laid {
    id: String,
    text: String,
    kind: String,
    size: f32,
    /// The top of this slice's band on screen after scroll; a split block has one slice per page.
    y_top: f64,
    text_x: f64,
    width: f64,
    height: f64,
    /// Where the shaped layout's origin maps on screen: layout-relative geometry is measured
    /// from here, and a lower slice of a split block sits above its own top.
    layout_y: f64,
    clip: Option<Rect>,
    bullet: Option<String>,
    ink: Color,
    image_hash: Option<String>,
    cell_box: Option<Rect>,
    cell_row: Option<usize>,
    page: usize,
    composition: Option<EditCore>,
}

impl Laid {
    fn layout_byte(&self, offset: usize) -> usize {
        shown_byte(
            self.composition.as_ref(),
            &self.text,
            char_to_byte(&self.text, offset),
        )
    }

    fn shown(&self, byte: usize) -> usize {
        shown_byte(self.composition.as_ref(), &self.text, byte)
    }

    fn offset_at(&self, byte: usize) -> usize {
        committed_offset(self.composition.as_ref(), &self.text, byte)
    }

    fn caret_byte(&self, offset: usize) -> usize {
        self.composition.as_ref().map_or_else(
            || char_to_byte(&self.text, offset),
            |core| core.display_caret(&self.text),
        )
    }
}

fn shown_byte(composition: Option<&EditCore>, text: &str, byte: usize) -> usize {
    composition.map_or(byte, |core| core.to_display(text, byte))
}

fn committed_offset(composition: Option<&EditCore>, text: &str, byte: usize) -> usize {
    byte_to_char(
        text,
        composition.map_or(byte, |core| core.from_display(text, byte)),
    )
}

/// A floated image's column: paragraphs on its page are shaped narrower beside it, whole
/// rather than per line, until the flow passes its bottom.
#[derive(Clone, Copy)]
struct FloatReserve {
    left: bool,
    reserve_w: f64,
    bottom: f64,
    page: usize,
}

struct RegionLaid {
    id: String,
    text: String,
    origin: Point,
    layout: RichTextLayout,
    composition: Option<EditCore>,
}

const LETTER_RATIO: f64 = 11.0 / 8.5;
const FLOAT_GUTTER: f64 = 12.0;

struct EditorLifecycle {
    focused: bool,
    dirty: bool,
}

struct ViewPreferences {
    ruler_visible: bool,
    styles_panel_visible: bool,
    auto_fit_page: bool,
}

struct ProofingPreferences {
    enabled: bool,
    language: String,
    personal: bool,
    personal_words: proofing::PersonalDictionary,
    shared: Option<ui::framework::Setting<ui::text_assistance::TextAssistancePrefs>>,
    applied_language: Option<String>,
}

impl ProofingPreferences {
    fn for_session() -> Self {
        Self {
            enabled: true,
            language: if cfg!(test) {
                "en_US".to_owned()
            } else {
                spellcheck::default_language()
            },
            personal: false,
            personal_words: proofing::PersonalDictionary::default(),
            shared: None,
            applied_language: None,
        }
    }
}

#[derive(Default)]
struct MaterializedBody {
    heads: Vec<ChangeHash>,
    pending_ops: usize,
    preview: bool,
}

pub struct Editor {
    inner: WordProcessor,
    rect: Rect,
    lifecycle: EditorLifecycle,
    /// Where edits go: checks permission, attributes each edit, groups typing into undo steps.
    dispatch: Option<ui::framework::Dispatcher<crate::framework::Document>>,
    source: Option<ui::framework::Live<crate::framework::Document>>,
    source_unseen: bool,
    copied: CopiedParts,
    pointer: Point,
    drag: DragSelect,
    drag_unit: Option<(Caret, Caret)>,
    path: Option<PathBuf>,
    /// Where the document was saved when that place has no path here, and as what: every
    /// later Save writes there, and only Save As asks again.
    kept: Option<(ui::portal::SaveDestination, WpFormat)>,
    format: Option<WpFormat>,
    find: DocumentFind,
    blobs: std::collections::BTreeMap<String, Vec<u8>>,
    image_cache: std::collections::HashMap<String, Option<ui::widget::Image>>,
    blocks: Vec<BlockView>,
    body_order: std::rc::Rc<tilo_document::BodyOrder>,
    body_window_start: usize,
    materialized: MaterializedBody,
    #[cfg(test)]
    reference_text_rendering: bool,
    /// The page header's blocks, drawn in every page's top margin; `page-number` fills in.
    header: Vec<BlockView>,
    footer: Vec<BlockView>,
    laid: Vec<Laid>,
    region_laid: Vec<RegionLaid>,
    head_band_rect: Rect,
    foot_band_rect: Rect,
    layout_cache: HashMap<String, (u64, RichTextLayout)>,
    recent_layout_cache: HashMap<String, (u64, RichTextLayout)>,
    dbg_layout_hits: usize,
    dbg_layout_misses: usize,
    bar: ui::framework::BarCommands<window_bar::BarCommand>,
    paper_theme: Theme,
    desk_theme: Theme,
    text_scale: f64,
    rich_clip: Option<(String, Vec<ClipBlock>)>,
    scroll_y: f64,
    scroll_viewport: f64,
    max_scroll: f64,
    pages: usize,
    page_pitch: f64,
    /// How much of the canvas the window's search covers from its top, in content px, so a
    /// match near the start is shown clear of the search's options.
    find_inset: f64,
    find_cover: Option<Rect>,
    sheet: crate::styles::StyleSheet,
    margins: Edges,
    layout_mode: LayoutMode,
    guide_drag: Option<GuideDrag>,
    object_drag: Option<ObjectDrag>,
    view: ViewPreferences,
    ruler: Ruler,
    page_rect: Rect,
    page_margin_scale: f64,
    layout_chips: Vec<(LayoutMode, Rect)>,
    layout_bar_height: f64,
    zoom_viewport: Rect,
    panel_rect: Rect,
    panel_hits: Vec<(Rect, PanelHit)>,
    segmented_touch: SegmentedTouch,
    editing_field: Option<NumField>,
    num_inputs: Vec<NumSlot>,
    line_spacing: ui::widget::Dropdown,
    line_spacing_shown: String,
    line_spacing_rect: Rect,
    save_error: Option<Rect>,
    style_pick: ui::widget::Dropdown,
    style_pick_shown: String,
    style_pick_options: Vec<(String, String)>,
    style_pick_rect: Rect,
    panel_body: Rect,
    panel_scroll: f64,
    panel_max_scroll: f64,
    zoom_input: ui::widget::NumberInput,
    zoom_rect: Rect,
    zoom: ZoomControl,
    notes: Vec<NoteView>,
    /// The English dictionary, built on the first spell-check pass (~75k words), then owned
    /// here so words can be added to it.
    dict: Option<spellcheck::Dictionary>,
    proofing: ProofingPreferences,
    last_autocorrection: Option<DocumentAutocorrection>,
    /// Per-block spell-check cache, keyed by the block's text so an edit invalidates it.
    spell_cache: HashMap<String, (String, Vec<(usize, usize)>)>,
    context_menu: ContextMenu,
    context_actions: Vec<Option<CtxAction>>,
    compact_swatch_palette: CompactSwatchPalette,
    action_palette_request: Option<tilo_actions::ActionAddress>,
    review: ReviewOverlay,
    thread_comment: Option<String>,
    thread_snapshot: Option<tilo_doc::snapshot::CommentView>,
    thread_reply: Embedded<RichTextSurface<Draft>>,
    thread_reply_views: Vec<RichTextView>,
    thread_panel: Rect,
    thread_submit: Rect,
    thread_cancel: Rect,
    outline: OutlinePane,
    chrome: ChromeColors,
    outline_drawer: ui::widget::SidebarDrawer,
    inspector_drawer: ui::widget::SidebarDrawer,
    file_menu: FileMenu,
    portal_rx: Option<portal::ContentsChannel>,
    pending_save: Option<(PathBuf, ui::framework::EffectReply)>,
    style_menu: StyleMenu,
    toast: Notice,
    paint_damage: ui::widget::paint_damage::PaintDamage,
    ime_rect: Option<Rect>,
    color_picker: StyleColorPicker,
    participants: participants::ParticipantViews,
    staged_scenario: Option<String>,
    layout: Option<(
        ui::framework::Setting<proofing::WriterLayout>,
        Option<proofing::WriterLayout>,
    )>,
    record_file: Option<PathBuf>,
    bound: BoundRows,
    pending_local: Option<(LocalVersion, crate::actions::participant::ParticipantView)>,
    #[cfg(test)]
    last_dispatched: Option<crate::actions::RichTextAction>,
}

#[derive(Default)]
struct BoundRows {
    local: Vec<ui::framework::LiveRow>,
    participants: HashMap<tilo_actions::ParticipantId, Vec<ui::framework::LiveRow>>,
}

/// Committed heads plus the open transaction's size: changes whenever an edit applied.
type LocalVersion = (Vec<automerge::ChangeHash>, usize);

fn local_version(doc: &AutoCommit) -> LocalVersion {
    use automerge::transaction::Transactable;
    (doc.base_heads(), doc.pending_ops())
}

fn open_request() -> portal::PickRequest {
    portal::PickRequest::open("Open document", "Open")
        .filter(
            portal::FileFilter::new("Documents")
                .glob("*.docx")
                .glob("*.odt")
                .glob("*.md")
                .glob("*.markdown")
                .glob("*.txt")
                .glob("*.html")
                .glob("*.htm")
                .glob("*.rtf"),
        )
        .thread_name("wp-portal-open")
}

fn save_request(suggested: &str) -> portal::PickRequest {
    portal::PickRequest::save("Save as", "Save", suggested)
        .filter(portal::FileFilter::new("Word document").glob("*.docx"))
        .filter(portal::FileFilter::new("OpenDocument text").glob("*.odt"))
        .filter(
            portal::FileFilter::new("Markdown")
                .glob("*.md")
                .glob("*.markdown"),
        )
        .filter(portal::FileFilter::new("Rich Text").glob("*.rtf"))
        .filter(portal::FileFilter::new("HTML").glob("*.html").glob("*.htm"))
        .filter(portal::FileFilter::new("PDF").glob("*.pdf"))
        .filter(portal::FileFilter::new("EPUB").glob("*.epub"))
        .thread_name("wp-portal-save")
}

fn shared_dictionary() -> std::sync::Arc<spellcheck::Dictionary> {
    use std::sync::OnceLock;
    static DICT: OnceLock<std::sync::Arc<spellcheck::Dictionary>> = OnceLock::new();
    DICT.get_or_init(|| std::sync::Arc::new(spellcheck::Dictionary::builtin()))
        .clone()
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn from_document(state: &AutoCommit) -> Result<Self, String> {
        let mut widget = Self::new();
        widget.inner.bind_view_only(state, &automerge::ROOT);
        if widget.inner.containers.is_none() {
            return Err("document does not carry the Writer schema".into());
        }
        widget.refresh_blocks(state);
        widget.sheet = widget.inner.stylesheet(state);
        Ok(widget)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the constructor lists every child widget and its initial state in one place"
    )]
    pub fn new() -> Self {
        Self {
            inner: WordProcessor::new(),
            rect: Rect::ZERO,
            lifecycle: EditorLifecycle {
                focused: true,
                dirty: true,
            },
            dispatch: None,
            source: None,
            source_unseen: false,
            copied: CopiedParts::default(),
            pointer: Point::ZERO,
            drag: DragSelect::new(),
            drag_unit: None,
            path: None,
            format: None,
            find: DocumentFind::default(),
            blobs: std::collections::BTreeMap::new(),
            image_cache: std::collections::HashMap::new(),
            blocks: Vec::new(),
            body_order: std::rc::Rc::default(),
            body_window_start: 0,
            materialized: MaterializedBody::default(),
            #[cfg(test)]
            reference_text_rendering: false,
            header: Vec::new(),
            footer: Vec::new(),
            laid: Vec::new(),
            region_laid: Vec::new(),
            head_band_rect: Rect::ZERO,
            foot_band_rect: Rect::ZERO,
            layout_cache: HashMap::new(),
            recent_layout_cache: HashMap::new(),
            dbg_layout_hits: 0,
            dbg_layout_misses: 0,
            bar: ui::framework::BarCommands::new(),
            paper_theme: Theme::light(),
            desk_theme: Theme::dark(),
            text_scale: 1.0,
            rich_clip: None,
            scroll_y: 0.0,
            scroll_viewport: 0.0,
            max_scroll: 0.0,
            pages: 1,
            page_pitch: 0.0,
            find_inset: 0.0,
            find_cover: None,
            sheet: crate::styles::StyleSheet::default(),
            margins: Edges::uniform(72.0),
            layout_mode: LayoutMode::None,
            guide_drag: None,
            object_drag: None,
            view: ViewPreferences {
                ruler_visible: true,
                styles_panel_visible: false,
                auto_fit_page: true,
            },
            ruler: Ruler::default(),
            page_rect: Rect::ZERO,
            page_margin_scale: 1.0,
            layout_chips: Vec::new(),
            layout_bar_height: 0.0,
            zoom_viewport: Rect::ZERO,
            // Keep the writing surface primary: the inspector is one click away.
            panel_rect: Rect::ZERO,
            panel_hits: Vec::new(),
            segmented_touch: SegmentedTouch::default(),
            editing_field: None,
            num_inputs: [
                NumField::Size,
                NumField::SpaceBefore,
                NumField::SpaceAfter,
                NumField::Indent,
            ]
            .into_iter()
            .map(|field| NumSlot {
                field,
                input: field.input(),
                decimals: 0,
                rect: Rect::ZERO,
            })
            .collect(),
            line_spacing: line_spacing_menu(),
            line_spacing_shown: String::new(),
            line_spacing_rect: Rect::ZERO,
            save_error: None,
            style_pick: style_pick_menu(),
            style_pick_shown: String::new(),
            style_pick_options: Vec::new(),
            style_pick_rect: Rect::ZERO,
            panel_body: Rect::ZERO,
            panel_scroll: 0.0,
            panel_max_scroll: 0.0,
            zoom_input: zoom_field(),
            zoom_rect: Rect::ZERO,
            zoom: ZoomControl::new(),
            notes: Vec::new(),
            dict: None,
            proofing: ProofingPreferences::for_session(),
            last_autocorrection: None,
            spell_cache: HashMap::new(),
            context_menu: ContextMenu::new(),
            context_actions: Vec::new(),
            compact_swatch_palette: CompactSwatchPalette::default(),
            action_palette_request: None,
            review: ReviewOverlay::default(),
            thread_comment: None,
            thread_snapshot: None,
            thread_reply: Embedded::new(
                RichTextSurface::over(Draft::new())
                    .with_editor(
                        BlockEditor::new()
                            .with_wrap_lines(true)
                            .with_compact_paragraph_spacing(true)
                            .with_block_handles(false),
                    )
                    .with_placeholder(&t("review-reply-placeholder"))
                    .with_submit(SubmitKey::ModEnter),
            ),
            thread_reply_views: Vec::new(),
            thread_panel: Rect::ZERO,
            thread_submit: Rect::ZERO,
            thread_cancel: Rect::ZERO,
            outline: OutlinePane::default(),
            chrome: ChromeColors::for_theme(&Theme::dark()),
            outline_drawer: ui::widget::SidebarDrawer::new()
                .left()
                .closed()
                .breakpoint(OUTLINE_DOCKS_FROM),
            inspector_drawer: ui::widget::SidebarDrawer::new()
                .right()
                .closed()
                .docked_width(f64::from(ui::tokens::Metrics::BASE.inspector_width))
                .overlay_width(f64::from(ui::tokens::Metrics::BASE.inspector_width))
                .breakpoint(INSPECTOR_DOCKS_FROM),
            file_menu: FileMenu::default(),
            portal_rx: None,
            pending_save: None,
            kept: None,
            style_menu: StyleMenu::default(),
            toast: Notice::default(),
            paint_damage: ui::widget::paint_damage::PaintDamage::default(),
            ime_rect: None,
            color_picker: StyleColorPicker::default(),
            participants: participants::ParticipantViews::default(),
            staged_scenario: None,
            layout: None,
            record_file: None,
            bound: BoundRows::default(),
            pending_local: None,
            #[cfg(test)]
            last_dispatched: None,
        }
    }

    pub(crate) fn attach_framework(
        &mut self,
        live: ui::framework::Live<crate::framework::Document>,
        dispatcher: ui::framework::Dispatcher<crate::framework::Document>,
    ) {
        self.inner.attach(dispatcher.clone());
        self.dispatch = Some(dispatcher);
        self.source = Some(live);
    }

    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        let mut widget = Self::new();
        let dispatch = ui::framework::testing::detached_dispatcher("test-author");
        widget.inner.attach(dispatch.clone());
        widget.dispatch = Some(dispatch);
        widget
    }

    #[cfg(test)]
    pub(crate) fn last_dispatched_for_test(&self) -> Option<&crate::actions::RichTextAction> {
        self.last_dispatched.as_ref()
    }

    pub(crate) fn show_record_of(&mut self, path: PathBuf) {
        let format = WpFormat::from_path(&path);
        self.set_file(path.clone(), format);
        self.record_file = Some(path);
    }

    fn note_file_written(&self, path: &std::path::Path, bytes: &[u8]) {
        if self.record_file.as_deref() != Some(path) {
            return;
        }
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(
                crate::model::Document::NOTE_FILE_WRITTEN.with(tilo_content::hash_bytes(bytes)),
            );
        }
    }

    pub(crate) fn search_rows(&self) -> Vec<ui::framework::LiveRow> {
        match ui::invocation_scope::current_viewer() {
            Some(participant) => self
                .bound
                .participants
                .get(&participant)
                .cloned()
                .unwrap_or_default(),
            None => self.bound.local.clone(),
        }
    }

    pub fn is_spelled_right(&mut self, word: &str) -> bool {
        self.dictionary().contains(word)
    }

    pub(crate) fn follow_preferences(
        &mut self,
        shared: ui::framework::Setting<ui::text_assistance::TextAssistancePrefs>,
        layout: ui::framework::Setting<proofing::WriterLayout>,
    ) {
        self.proofing.shared = Some(shared);
        self.layout = Some((layout, None));
        self.follow_settings();
    }

    fn follow_settings(&mut self) {
        if let Some(shared) = &self.proofing.shared {
            let language = shared.get().spelling_language;
            if self.proofing.applied_language.as_ref() != Some(&language) {
                self.proofing.language = if language.is_empty() {
                    spellcheck::default_language()
                } else {
                    language.clone()
                };
                self.proofing.applied_language = Some(language);
                self.spell_cache.clear();
            }
        }
        let shown = proofing::WriterLayout {
            show_ruler: self.view.ruler_visible,
            show_styles_panel: self.view.styles_panel_visible,
            show_outline: self.outline.is_open(),
        };
        let Some((setting, applied)) = &mut self.layout else {
            return;
        };
        let stored = setting.get();
        match applied {
            // Changed here since: keep it.
            Some(applied) if *applied != shown && *applied == stored => {
                if let Err(error) = setting.set(shown.clone()) {
                    eprintln!("word-processor: could not keep the layout: {error}");
                }
                *applied = shown;
            }
            // Changed in another window or on another device: show it.
            Some(applied) if *applied == stored => {}
            _ => {
                self.view.ruler_visible = stored.show_ruler;
                self.view.styles_panel_visible = stored.show_styles_panel;
                if self.outline.is_open() != stored.show_outline {
                    self.outline.toggle();
                }
                *applied = Some(stored);
                self.lifecycle.dirty = true;
            }
        }
    }

    pub(crate) fn follow_personal_dictionary(&mut self) {
        self.proofing.personal = true;
        self.dict = None;
        self.spell_cache.clear();
    }

    pub(crate) fn present(&mut self, state: &crate::framework::Document) {
        if self.inner.caret().block.is_empty()
            && let Some(first) = state.text().and_then(|text| text.body().id_at(0))
        {
            self.inner.set_caret(Caret {
                block: first.to_owned(),
                offset: 0,
            });
        }
        self.lifecycle.dirty = true;
    }

    pub(crate) fn read_only(&self) -> bool {
        self.dispatch
            .as_ref()
            .is_none_or(ui::framework::Dispatcher::read_only)
    }

    fn dictionary(&mut self) -> &mut spellcheck::Dictionary {
        if self.proofing.personal {
            let snapshot = ui::personal_dictionary::snapshot();
            let language = self
                .proofing
                .language
                .replace('_', "-")
                .to_ascii_lowercase();
            let words = proofing::PersonalDictionary {
                dictionary_words: snapshot
                    .ok()
                    .map(|snapshot| snapshot.hints(&language).map(str::to_owned).collect())
                    .unwrap_or_default(),
            };
            if words != self.proofing.personal_words {
                self.proofing.personal_words = words;
                self.dict = None;
                self.spell_cache.clear();
            }
        }
        let personal = &self.proofing.personal_words;
        self.dict
            .get_or_insert_with(|| personal.over(&shared_dictionary()))
    }

    fn block_spell_spans(&mut self, id: &str, text: &str) -> Vec<(usize, usize)> {
        self.dictionary();
        if let Some((cached_text, spans)) = self.spell_cache.get(id)
            && cached_text == text
        {
            return spans.clone();
        }
        let language = self.proofing.language.clone();
        let dict = self.dictionary();
        let spans = misspelled_spans(dict, &language, text);
        self.spell_cache
            .insert(id.to_string(), (text.to_string(), spans.clone()));
        spans
    }

    pub fn toggle_spellcheck(&mut self) {
        self.proofing.enabled = !self.proofing.enabled;
        self.lifecycle.dirty = true;
    }

    pub fn spellcheck_enabled(&self) -> bool {
        self.proofing.enabled && ui::text_assistance::preferences().check_spelling
    }

    pub fn spell_language(&self) -> &str {
        &self.proofing.language
    }

    pub fn panels_shown(&self) -> (bool, bool, bool) {
        (
            self.view.ruler_visible,
            self.view.styles_panel_visible,
            self.outline.is_open(),
        )
    }

    pub fn set_spell_language(&mut self, language: &str) {
        self.proofing.language = language.to_string();
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
    }

    pub fn toggle_markup(&mut self) {
        self.review.toggle();
        // Review visibility changes per-run colours, so every block reshapes next frame.
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.lifecycle.dirty = true;
    }

    pub fn markup_enabled(&self) -> bool {
        self.review.is_visible()
    }

    pub fn model(&self) -> &WordProcessor {
        &self.inner
    }
    pub fn set_styles_panel_open(&mut self, open: bool) {
        self.view.styles_panel_visible = open;
        self.lifecycle.dirty = true;
    }

    pub fn stage_error_for_shot(&mut self) {
        self.save_error = Some(Rect::ZERO);
        self.lifecycle.dirty = true;
    }

    pub fn stage_find_for_shot(&mut self, state: &AutoCommit, query: &str) {
        self.find_now(state, query);
    }

    pub fn stage_comment_thread_for_shot(&mut self, comment: tilo_doc::snapshot::CommentView) {
        self.thread_comment = Some(comment.id.clone());
        self.thread_snapshot = Some(comment);
        self.focus_thread_reply(true);
        self.lifecycle.dirty = true;
    }

    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.staged_scenario = scenario.map(str::to_owned);
        self.lifecycle.dirty = true;
    }

    fn run_staged_scenario(&mut self, state: &mut ui::widget::Edits<'_>, scenario: &str) {
        match scenario {
            "styles" => self.set_styles_panel_open(true),
            "error" => self.stage_error_for_shot(),
            "find" => self.stage_find_for_shot(state, "report"),
            "comments" => self.stage_review(state, false),
            "threaded-comments" => self.stage_review(state, true),
            "image-object" => self.stage_selected_image(state),
            _ => {}
        }
    }

    fn first_text_block(state: &AutoCommit) -> Option<(String, usize)> {
        let containers = crate::model::open(state).ok()?;
        tilo_doc::model::blocks_in_order(state, &containers)
            .into_iter()
            .find_map(|id| {
                let len = tilo_doc::model::block_text(state, &containers, &id)?
                    .chars()
                    .count();
                (len > 0).then_some((id, len))
            })
    }

    fn stage_review(&mut self, state: &mut ui::widget::Edits<'_>, thread: bool) {
        use crate::model::{DocumentAction, document};
        let Some((block, len)) = Self::first_text_block(state) else {
            return;
        };
        let Some(text) =
            ui::tilo_resource::NonBlank::parse("Could we make this opening more specific?")
        else {
            return;
        };
        self.perform_verb_as(
            state,
            DocumentAction::AddComment(document::AddComment {
                block: block.clone(),
                start: 0,
                end: len.min(12),
                text,
            }),
            Some("Maya Chen"),
        );
        self.perform_edits(
            state,
            &crate::actions::RichTextAction::TrackInsert {
                block,
                at: len,
                text: " — reviewed".into(),
                author: "Jonas Weber".into(),
                date: "2026-09-05".into(),
            },
        );
        if !thread {
            return;
        }
        let threads = |state: &AutoCommit| {
            <crate::model::Document as ui::tilo_resource::Resource>::read(state)
                .map(|document| document.comment_views(state))
                .unwrap_or_default()
        };
        let Some(comment) = threads(state).into_iter().next().map(|comment| comment.id) else {
            return;
        };
        for (author, text) in [
            ("Jonas Weber", "I can revise the opening."),
            ("Maya Chen", "Thanks — please keep the decision clear."),
        ] {
            let Some(text) = ui::tilo_resource::NonBlank::parse(text) else {
                continue;
            };
            self.perform_verb_as(
                state,
                DocumentAction::ReplyToComment(document::ReplyToComment {
                    comment: ui::tilo_resource::Id::from_raw(comment.clone()),
                    text,
                    body: None,
                }),
                Some(author),
            );
        }
        if let Some(view) = threads(state).into_iter().find(|item| item.id == comment) {
            self.stage_comment_thread_for_shot(view);
        }
    }

    fn stage_selected_image(&mut self, state: &mut ui::widget::Edits<'_>) {
        // A real decodable PNG: the bytes go through the blob and image-cache path.
        const SHOT_PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3,
            8, 2, 0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73,
            69, 78, 68, 174, 66, 96, 130,
        ];
        let Some((block, len)) = Self::first_text_block(state) else {
            return;
        };
        let Ok(hash) = self.inner.content_store().put(SHOT_PNG) else {
            return;
        };
        self.perform_edits(
            state,
            &crate::actions::insert_image_action(
                &block,
                len,
                crate::actions::ImageInput::of(hash, SHOT_PNG),
            ),
        );
        let image = crate::model::open(state).ok().and_then(|containers| {
            tilo_doc::model::materialize(state, &containers)
                .blocks
                .into_iter()
                .find(|block| block.kind == "image")
        });
        let Some(image) = image else {
            return;
        };
        self.perform_edits(
            state,
            &crate::actions::set_image_size_action(&image.id, 300, 180),
        );
        self.inner.set_caret(crate::word_processor::Caret {
            block: image.id,
            offset: 0,
        });
        self.set_styles_panel_open(true);
    }

    pub fn model_mut(&mut self) -> &mut WordProcessor {
        &mut self.inner
    }

    pub fn set_file(&mut self, path: PathBuf, format: WpFormat) {
        self.path = Some(path);
        self.kept = None;
        self.format = Some(format);
    }

    fn try_write_to_path(
        &mut self,
        state: &AutoCommit,
        path: &std::path::Path,
        format: WpFormat,
    ) -> Result<(), String> {
        self.lifecycle.dirty = true;
        let bytes = self.export_bytes(state, format)?;
        tilo_files::native::atomic::write_preserving_target(path, &bytes).map_err(|e| {
            eprintln!("word-processor: could not write {}: {e}", path.display());
            self.save_error = Some(Rect::ZERO);
            format!("could not write {}: {e}", path.display())
        })?;
        self.save_error = None;
        self.note_file_written(path, &bytes);
        if !matches!(format, WpFormat::Pdf | WpFormat::Epub) {
            self.set_file(path.to_path_buf(), format);
        }
        Ok(())
    }

    fn export_bytes(&mut self, state: &AutoCommit, format: WpFormat) -> Result<Vec<u8>, String> {
        if self.inner.containers.is_none() {
            return Err("document has no writable path or initialized body".into());
        }
        let file = crate::model::export_copy(state)?;
        format
            .export(file.as_ref(), self.inner.content_store())
            .map_err(|e| {
                eprintln!("word-processor: export failed: {e}");
                self.save_error = Some(Rect::ZERO);
                format!("export failed: {e}")
            })
    }

    pub(crate) fn zoom_page(&mut self, step: ui::content_zoom::ZoomStep) -> f32 {
        match step {
            ui::content_zoom::ZoomStep::In => self.zoom_by(ZOOM_STEP),
            ui::content_zoom::ZoomStep::Out => self.zoom_by(-ZOOM_STEP),
            ui::content_zoom::ZoomStep::Reset => self.set_zoom(1.0),
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a zoom factor is a small positive float"
        )]
        let factor = self.zoom.factor() as f32;
        factor
    }

    fn set_zoom(&mut self, z: f64) -> bool {
        self.view.auto_fit_page = false;
        if self.zoom.set(z) {
            self.lifecycle.dirty = true;
            true
        } else {
            false
        }
    }

    fn zoom_by(&mut self, delta: f64) -> bool {
        self.view.auto_fit_page = false;
        let changed = self.zoom.step(delta);
        self.lifecycle.dirty |= changed;
        changed
    }

    fn content_transform(&self, base: Affine) -> Affine {
        self.zoom.transform(base)
    }

    fn pointer_to_content(&self, p: Point) -> Point {
        self.zoom.screen_to_content(p)
    }

    /// The bounded slice materialized around the viewport; the caret can be far off screen.
    fn body_window_bounds(&self) -> (usize, usize) {
        let len = self.body_order.len();
        if len <= VIRTUALIZE_BODY_AFTER {
            return (0, len);
        }
        let flow = page_stack_to_flow(
            (self.scroll_y - PAGE_TOP).max(0.0),
            MAX_PAGE_W * LETTER_RATIO,
            self.margins,
        );
        let scrolled = num::usize_of_f64((flow / ESTIMATED_BLOCK_EXTENT).floor());
        let start = scrolled.saturating_sub(VIEWPORT_BLOCK_WINDOW / 4);
        let end = start.saturating_add(VIEWPORT_BLOCK_WINDOW).min(len);
        (end.saturating_sub(VIEWPORT_BLOCK_WINDOW), end)
    }

    fn refresh_blocks(&mut self, state: &AutoCommit) {
        self.refresh_blocks_after_text_edit(state, None);
    }

    fn refresh_blocks_after_text_edit(&mut self, state: &AutoCommit, _text_block: Option<&str>) {
        if self.inner.containers.is_none() {
            return;
        }
        let moved = self.refresh_live(state) | std::mem::take(&mut self.source_unseen);
        // Search ranges are character offsets, not anchors: re-read on every move.
        self.follow_find(state, moved);
        if let Some(body) = self.with_live(|s| std::rc::Rc::clone(s.body())) {
            self.body_order = body;
        }
        let (start, end) = self.body_window_bounds();
        let window_changed =
            self.body_window_start != start || self.blocks.len() != end.saturating_sub(start);
        if !moved && !window_changed && !self.materialized.preview {
            return;
        }
        self.demand_live(state, ui::tilo_resource::Demand::range(start..end));
        let preview = std::mem::take(&mut self.materialized.preview);
        let copied = &mut self.copied;
        let blocks = &mut self.blocks;
        let mut parts = PartsUpdate::default();
        if let Some(live) = self.source.clone() {
            live.with(|document| {
                let Some(doc) = document.text() else {
                    return;
                };
                let mut kept: HashMap<String, (std::rc::Rc<BlockView>, BlockView)> = if preview {
                    HashMap::new()
                } else {
                    std::mem::take(&mut copied.blocks)
                        .into_iter()
                        .zip(std::mem::take(blocks))
                        .map(|(source, view)| (view.id.clone(), (source, view)))
                        .collect()
                };
                let mut sources = Vec::with_capacity(end.saturating_sub(start));
                let mut views = Vec::with_capacity(end.saturating_sub(start));
                let comments = document.comment_views(state);
                for source in doc.blocks(start..end) {
                    let mut view = match kept.remove(&source.id) {
                        Some((old, view)) if std::rc::Rc::ptr_eq(&old, source) => view,
                        _ => BlockView::clone(source),
                    };
                    mark_comments(&mut view, &comments);
                    sources.push(std::rc::Rc::clone(source));
                    views.push(view);
                }
                copied.blocks = sources;
                *blocks = views;
                parts.header = copied.header.take_if_new(doc.header());
                parts.footer = copied.footer.take_if_new(doc.footer());
                parts.notes = copied.notes.take_if_new(doc.notes());
                parts.content = copied.content.see(doc.content());
                let comments_moved = copied.comments != comments;
                let changes = copied.changes.take_if_new(doc.changes());
                if comments_moved || changes.is_some() {
                    copied.comments.clone_from(&comments);
                    parts.review = Some((
                        comments,
                        changes.unwrap_or_else(|| doc.changes().as_ref().clone()),
                    ));
                }
            });
        }
        if let Some(header) = parts.header {
            self.header = header;
        }
        if let Some(footer) = parts.footer {
            self.footer = footer;
        }
        if let Some(notes) = parts.notes {
            self.notes = notes;
        }
        if parts.content
            && let Some(c) = self.inner.containers.as_ref()
        {
            Self::refresh_images(
                &mut self.blobs,
                &mut self.image_cache,
                self.inner.content_store(),
                state,
                c,
            );
        }
        if let Some((comments, changes)) = parts.review {
            self.review.replace(comments, changes);
        }
        self.materialized.heads = state.base_heads();
        self.materialized.pending_ops = state.pending_ops();
        self.body_window_start = start;
        // The spell cache follows the materialized window, so no offscreen text is kept.
        let live: std::collections::HashSet<&str> =
            self.blocks.iter().map(|b| b.id.as_str()).collect();
        self.spell_cache.retain(|id, _| live.contains(id.as_str()));
    }

    fn band_at_pointer(&self, p: Point) -> Option<Region> {
        if self.head_band_rect.height() > 0.0 && self.head_band_rect.contains(p) {
            Some(Region::Header)
        } else if self.foot_band_rect.height() > 0.0 && self.foot_band_rect.contains(p) {
            Some(Region::Footer)
        } else {
            None
        }
    }

    fn band_caret_from_pointer(&mut self, p: Point, extend: bool) {
        let mut hit: Option<(String, usize)> = None;
        for rl in &self.region_laid {
            let h = f64::from(rl.layout.height());
            if p.y >= rl.origin.y && p.y < rl.origin.y + h {
                let byte = rl.layout.hit(
                    num::f32_of_f64(p.x - rl.origin.x),
                    num::f32_of_f64(p.y - rl.origin.y),
                );
                hit = Some((
                    rl.id.clone(),
                    committed_offset(rl.composition.as_ref(), &rl.text, byte),
                ));
                break;
            }
        }
        // A click anywhere in the band lands a caret: fall back to its nearest block.
        if hit.is_none() {
            if let Some(rl) = self.region_laid.first()
                && p.y < rl.origin.y
            {
                hit = Some((rl.id.clone(), 0));
            }
            if hit.is_none()
                && let Some(rl) = self.region_laid.last()
            {
                let byte = rl
                    .layout
                    .hit(num::f32_of_f64(p.x - rl.origin.x), rl.layout.height() - 1.0);
                hit = Some((
                    rl.id.clone(),
                    committed_offset(rl.composition.as_ref(), &rl.text, byte),
                ));
            }
        }
        if let Some((block, offset)) = hit {
            if extend {
                self.inner.set_anchor();
            } else {
                self.inner.clear_anchor();
            }
            self.inner.set_caret(Caret { block, offset });
            self.lifecycle.dirty = true;
        }
    }

    fn caret_from_pointer(&mut self, p: Point, extend: bool) {
        if self.inner.in_running_region() {
            self.band_caret_from_pointer(p, extend);
            return;
        }
        let mut hit: Option<(String, usize)> = None;
        for l in &self.laid {
            let inside = match l.cell_box {
                Some(bx) => bx.contains(p),
                None => p.y >= l.y_top && p.y < l.y_top + l.height,
            };
            if inside {
                // `layout_y` maps the shaped origin onto this slice: `p.y - layout_y` is internal.
                let byte = self.layout_cache.get(&l.id).map_or(0, |(_, lay)| {
                    lay.hit(
                        num::f32_of_f64(p.x - l.text_x),
                        num::f32_of_f64(p.y - l.layout_y),
                    )
                });
                hit = Some((l.id.clone(), l.offset_at(byte)));
                break;
            }
        }
        if hit.is_none() {
            let distance = |l: &Laid| {
                let top = l.cell_box.map_or(l.y_top, |r| r.y0);
                let bottom = l.cell_box.map_or(l.y_top + l.height, |r| r.y1);
                let dy = (top - p.y).max(0.0).max(p.y - bottom);
                let dx = l
                    .cell_box
                    .map_or(0.0, |r| (r.x0 - p.x).max(0.0).max(p.x - r.x1));
                dx * dx + dy * dy
            };
            if let Some(l) = self
                .laid
                .iter()
                .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            {
                let y =
                    p.y.clamp(l.y_top, (l.y_top + l.height - 0.001).max(l.y_top));
                let byte = self.layout_cache.get(&l.id).map_or(0, |(_, lay)| {
                    lay.hit(
                        num::f32_of_f64(p.x - l.text_x),
                        num::f32_of_f64(y - l.layout_y),
                    )
                });
                hit = Some((l.id.clone(), l.offset_at(byte)));
            }
        }
        if let Some((block, offset)) = hit {
            if extend {
                self.inner.set_anchor();
            } else {
                self.inner.clear_anchor();
            }
            self.inner.set_caret(Caret { block, offset });
            self.lifecycle.dirty = true;
        }
    }
}

struct RunningBand<'a> {
    blocks: &'a [BlockView],
    page_no: usize,
    rect: Rect,
    is_footer: bool,
    editing: bool,
    composition: Option<(&'a str, &'a EditCore)>,
}

struct EditorEventCtx<'a> {
    scale_factor: f64,
    modifiers: Modifiers,
    state: ui::widget::Edits<'a>,
    path: &'a ObjId,
    handled: &'a mut bool,
}

#[derive(Clone, Copy)]
struct UtilityBarsGeometry {
    left: f64,
    right: f64,
    top: f64,
}

#[derive(Clone, Copy)]
struct PageSheetInput<'a> {
    header: &'a [BlockView],
    footer: &'a [BlockView],
    composition: Option<(&'a str, &'a EditCore)>,
    edit_region: Region,
    index: usize,
    scroll_y: f64,
}

#[derive(Clone, Copy)]
struct PageContentInput<'a> {
    laid: &'a [Laid],
    cache: &'a HashMap<String, (u64, RichTextLayout)>,
    multi_selection: bool,
    spell_by_block: &'a HashMap<String, Vec<(usize, usize)>>,
    note_numbers: &'a HashMap<String, usize>,
    presence: &'a PresenceCtx,
}

struct NotesSpellcheckInput<'a> {
    spell_by_block: &'a HashMap<String, Vec<(usize, usize)>>,
    note_numbers: &'a HashMap<String, usize>,
    page_notes: &'a mut HashMap<usize, Vec<usize>>,
}

struct PanelCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    tr: &'a mut TextRenderer,
    theme: &'a Theme,
    x0: f64,
    right: f64,
    y: f64,
    clip: Rect,
}

struct NumSlot {
    field: NumField,
    input: ui::widget::NumberInput,
    decimals: usize,
    rect: Rect,
}

/// The word processor's accessibility tree, built whole by [`Editor::accessibility_tree`]:
/// its blocks are drawn directly, so the usual single-node path does not apply.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yTreeNode {
    pub node: ui::a11y::AccessNode,
    pub children: Vec<A11yTreeNode>,
}

struct FrameGeometry {
    canvas: Rect,
    canvas_clip: Rect,
    canvas_left: f64,
    canvas_right: f64,
    layout_bar_top: f64,
    content_bottom: f64,
    ctf: Affine,
    page_x: f64,
    page_top: f64,
    page_w: f64,
    content_x: f64,
    content_w: f32,
    m: Edges,
    letter_h: f64,
    page_pitch: f64,
}

struct PaginationResult {
    laid: Vec<Laid>,
    cache: HashMap<String, (u64, RichTextLayout)>,
    cur_page: usize,
    y: f64,
    blocks: Vec<BlockView>,
}

struct PageCursor {
    flow: PageFlow,
    flow_laid: Vec<std::ops::Range<usize>>,
    max_object_page: usize,
    active_float: Option<FloatReserve>,
    ol_counts: Vec<u32>,
}

struct TableLayoutContext<'a> {
    cell_padding: f64,
    text_renderer: &'a mut TextRenderer,
    old_cache: &'a mut HashMap<String, (u64, RichTextLayout)>,
    new_cache: &'a mut HashMap<String, (u64, RichTextLayout)>,
}

struct ParagraphPaginationContext<'a> {
    text_renderer: &'a mut TextRenderer,
    old_cache: &'a mut HashMap<String, (u64, RichTextLayout)>,
    new_cache: &'a mut HashMap<String, (u64, RichTextLayout)>,
    laid: &'a mut Vec<Laid>,
    cursor: &'a mut PageCursor,
}

struct ResolvedImage {
    hash: String,
    intrinsic: Option<(u32, u32)>,
    width: f64,
    height: f64,
}

struct ImagePlacement {
    x: f64,
    y: f64,
    page: usize,
}

struct ParagraphShape {
    st: crate::styles::StyleProps,
    indent: f64,
    bullet: Option<String>,
    float_offset: f64,
    wrap_w: f32,
    key: u64,
    layout: RichTextLayout,
}

struct ParagraphPlacement {
    text_x: f64,
    width: f64,
    size: f32,
    bullet: Option<String>,
    ink: Color,
}

struct Pass2Markers {
    page_notes: HashMap<usize, Vec<usize>>,
    comment_markers: Vec<(String, f64, f64)>,
    change_markers: Vec<(String, f64, f64)>,
}

struct LaidItemCtx<'a> {
    l: &'a Laid,
    layout: &'a RichTextLayout,
    ly: f64,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CommentThreadLayout {
    panel: Rect,
    reply_field: Rect,
    cancel_button: Rect,
    submit_button: Rect,
}

#[cfg(test)]
impl CommentThreadLayout {
    fn for_host(host: Rect, scale: f64) -> Self {
        let width = (372.0 * scale).min((host.width() - 24.0 * scale).max(260.0 * scale));
        let height = (360.0 * scale).min((host.height() - 76.0 * scale).max(238.0 * scale));
        let panel = Rect::new(
            host.x1 - width - 12.0 * scale,
            host.y1 - height - 38.0 * scale,
            host.x1 - 12.0 * scale,
            host.y1 - 38.0 * scale,
        );
        Self {
            panel,
            reply_field: Rect::new(
                panel.x0 + 12.0 * scale,
                panel.y1 - 82.0 * scale,
                panel.x1 - 12.0 * scale,
                panel.y1 - 52.0 * scale,
            ),
            cancel_button: Rect::new(
                panel.x1 - 210.0 * scale,
                panel.y1 - 40.0 * scale,
                panel.x1 - 118.0 * scale,
                panel.y1 - 12.0 * scale,
            ),
            submit_button: Rect::new(
                panel.x1 - 110.0 * scale,
                panel.y1 - 40.0 * scale,
                panel.x1 - 12.0 * scale,
                panel.y1 - 12.0 * scale,
            ),
        }
    }
}

#[cfg(test)]
#[path = "widget/r2_chrome_metrics.rs"]
mod r2_chrome_metrics;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod canonical_scroll_tests {
    use super::*;
    use ui::headless::Framed;

    #[test]
    fn primary_scroll_uses_drawn_geometry_without_document_mutation() {
        use ui::widget::PrimaryScrollAction;

        let mut state = crate::initialized_state();
        let mut widget = Editor::detached();
        widget.sync(&state.doc, &state.root);
        for _ in 0..40 {
            widget
                .model_mut()
                .insert_str(&mut Edits::lend(&mut state.doc), "A retained paragraph.");
            widget.model_mut().enter(&mut Edits::lend(&mut state.doc));
        }
        let mut framed_widget = tests::mount(state, widget, 1000, 400);
        let (widget, state) = Framed::split(&mut framed_widget);
        let heads = state.doc.get_heads();
        let before = widget.primary_vertical_scroll().unwrap().metrics;
        assert!(before.max_scroll() > 0.0);

        widget.apply_primary_vertical_scroll(PrimaryScrollAction::End);
        let (widget, state) = tests::pipeline_draw(&mut framed_widget, 1000, 400);
        let after = widget.primary_vertical_scroll().metrics;
        assert!(after.offset > before.offset);
        assert_eq!(after.offset, after.max_scroll());
        assert_eq!(heads, state.doc.get_heads());

        widget.apply_primary_vertical_scroll(PrimaryScrollAction::Start);
        assert_eq!(widget.scroll_y, 0.0);
        assert!(matches!(
            widget.apply_primary_vertical_scroll(PrimaryScrollAction::Absolute(f64::NAN)),
            ui::widget::PrimaryScrollResult::Unsupported
        ));
    }
}

#[cfg(test)]
#[path = "widget/mark_boundary_tests.rs"]
mod mark_boundary_tests;
