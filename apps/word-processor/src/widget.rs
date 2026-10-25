//! `WordProcessorWidget` — the `ui::Widget` for the word processor. Renders the
//! `tilo-doc` document as a centered page (à la `LibreOffice` Writer) using the
//! `RichTextLayout` primitive: word-wrapped paragraphs, headings, list bullets,
//! inline **bold**/*italic*/underline, a selection highlight, and a caret. Maps
//! pointer + keyboard onto the editing model (selection, formatting, save).

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
use tilo_doc::snapshot::{BlockView, NoteView};
use ui::gesture::DragSelect;
use ui::richtext::RichTextLayout;
use ui::text_edit::EditCore;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
// Provides `.r()/.g()/.b()` accessors on `vello` `Color` (used to derive
// theme-tinted translucent chrome tones).
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

/// Convert the model's `(block, start, end)` ranges into serializable
/// [`Span`](crate::actions::Span)s for an action payload.
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

/// What a right-click context-menu row does. The shared [`ContextMenu`]'s
/// `Arc<dyn Fn>` callbacks can't mutate the widget, so the widget keeps this
/// parallel list and dispatches on the clicked row index.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CtxAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    /// Add the right-clicked (misspelled) word to the user dictionary.
    AddToDictionary(String),
    /// Replace the right-clicked word's block span `[start,end)` (char offsets)
    /// with a spelling suggestion.
    ApplySuggestion {
        block: String,
        start: usize,
        end: usize,
        replacement: String,
    },
    /// A comment or tracked-change operation contributed by the review overlay.
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

// Paper-context colors (the document sheet): document ink assumes a light sheet
// (`STYLE_PALETTE[0]` is near-black), so paper stays light in both themes — only
// chrome follows the theme. `ChromeColors::for_theme` derives `paper` from the
// theme's lightest neutral when light enough; these consts are the fallback.
/// Canonical paper tone — a barely-warm off-white. Ink/selection/caret below are
/// chosen to read on this in every theme.
const PAPER_DEFAULT: Color = Color::from_rgb8(0xfc, 0xfc, 0xfa);
/// Default ink for paper content that has no per-block style colour (table
/// cells, image-block fallbacks). Dark so it reads on the light paper.
const PAPER_INK: Color = Color::from_rgb8(0x1c, 0x1c, 0x20);
/// Muted ink for paper chrome (running heads/footers, placeholders on paper).
const PAPER_MUTED: Color = Color::from_rgb8(0x8a, 0x8a, 0x92);
/// Alpha of the selection highlight painted on paper: translucent so the
/// text reads through it.
const PAPER_SELECTION_ALPHA: u8 = 0x4d;
/// Alpha of the accent wash behind the header/footer band that is the active
/// edit context (Word-style header/footer edit mode).
const ACTIVE_BAND_FILL_ALPHA: u8 = 0x14;
/// Alpha of the veil drawn over the body while a header/footer is being
/// edited, so the body visibly recedes (like Word dimming the body in that mode).
const BODY_DIM_VEIL_ALPHA: u8 = 0x66;

/// The colours the page draws with, resolved once per frame from the
/// [`Theme`]: the sheet itself and the theme's intents made legible on it.
/// Window chrome reads the theme directly; only the document surface needs
/// colours measured against the paper.
#[derive(Clone, Copy)]
struct ChromeColors {
    /// The OS accent made legible on the paper: the caret, the selection
    /// tint and the active header/footer band's edge and tab.
    paper_accent: Color,
    /// Label ink on a [`Self::paper_accent`] fill (the band's tab).
    on_paper_accent: Color,
    /// The theme's `danger` made legible on the paper: the spelling squiggle.
    paper_danger: Color,
    /// Whisper tint on the sheet (missing-image / fidelity placeholders).
    paper_tint: Color,
    /// Hairline on the sheet (placeholder edges, image-handle contrast).
    paper_edge: Color,
    /// The paper sheet: the raised surface of a light theme, and the
    /// canonical light sheet ([`PAPER_DEFAULT`]) under a dark one, so the
    /// dark-authored document ink always reads.
    paper: Color,
    /// The sheet's lift off the desk behind it.
    paper_elevation: ui::tokens::Shadow,
}

impl ChromeColors {
    /// Map the [`Theme`] onto the page's palette.
    fn for_theme(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        let paper = if luminance(c.surfaces.raised) >= 0.85 {
            c.surfaces.raised
        } else {
            PAPER_DEFAULT
        };
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

/// Relative luminance (0..1) of a colour — used only to decide whether a theme's
/// raised surface is light enough to serve as paper.
fn luminance(c: Color) -> f64 {
    let r = f64::from(c.r()) / 255.0;
    let g = f64::from(c.g()) / 255.0;
    let b = f64::from(c.b()) / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

const PAGE_TOP: f64 = 28.0;
/// The least a page keeps beside either pane: the column it would lose the
/// sheet whole in. Below that width the pane floats instead of docking.
const PAGE_KEEPS: f64 = MAX_PAGE_W / 2.0;
/// The window width (at 1×) from which the outline docks beside a page it
/// leaves readable; below it the outline is a drawer over the page.
const OUTLINE_DOCKS_FROM: f64 = ui::tokens::Metrics::BASE.panel_min_width as f64 + PAGE_KEEPS;
/// The width from which the inspector docks beside a whole page; below it
/// the inspector is a sheet over the page.
const INSPECTOR_DOCKS_FROM: f64 =
    ui::tokens::Metrics::BASE.inspector_width as f64 + MAX_PAGE_W + 48.0;
const MAX_PAGE_W: f64 = 820.0;
/// Body blocks retained as rich views for a large imported document. The
/// canonical CRDT remains complete; this is strictly the renderer's bounded
/// working set (the selected block and a generous viewport band).
const VIEWPORT_BLOCK_WINDOW: usize = 192;
/// Approximate body-block extent used only to choose a large-document window
/// before it has been shaped. Measured blocks immediately replace this estimate
/// inside the loaded window; it prevents an offscreen import from being shaped
/// just to find the initial viewport.
const ESTIMATED_BLOCK_EXTENT: f64 = 34.0;
/// Small documents keep the straightforward all-block cache. Larger imports
/// switch to the bounded viewport cache below.
const VIRTUALIZE_BODY_AFTER: usize = VIEWPORT_BLOCK_WINDOW * 2;
/// Vertical gap between consecutive discrete pages (the canvas shows through it),
/// à la Google Docs. Together with `LETTER_RATIO` this drives true pagination.
const PAGE_GAP: f64 = 22.0;
const CARET_WIDTH: f64 = 2.0;
/// Find-match highlight is chrome on the paper — see `ChromeColors::find_highlight`.
/// Amplitude (px) of the wavy spell-check underline.
const SQUIGGLE_AMP: f64 = 1.6;
/// Horizontal period (px) of one full wave of the spell-check underline.
const SQUIGGLE_PERIOD: f64 = 4.0;
/// Thin separator above the footnote area at the bottom of a page.
/// Paper content (the footnote rule), not OS chrome.
const NOTE_SEP: Color = Color::from_rgb8(0xb8, 0xb8, 0xbe);
/// Footnote body text colour (slightly muted vs. body ink). Paper content.
const NOTE_INK: Color = Color::from_rgb8(0x3a, 0x3a, 0x40);
/// Footnote text size (smaller than body) and the gap above the separator.
const NOTE_CONTENT_PX: f32 = 11.0;
const NOTE_AREA_GAP: f64 = 10.0;

// Panel + guide chrome colours are theme-driven — see `ChromeColors`, not new consts.
// Layout guides + handles use the theme accent — see `chrome.accent`/`chrome.accent_faint`.
/// How close (px) the pointer must be to a guide line to grab it.
const GUIDE_GRAB: f64 = 7.0;

/// Which layout element the bottom bar is editing. Each mode shows draggable
/// guides on the page with live preview.
#[derive(Clone, Copy, PartialEq, Debug)]
enum LayoutMode {
    None,
    Margins,
    Spacing,
}

/// A per-side measurement (page margins / border insets), in px.
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

/// How a guide drag distributes to the other sides.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DragKind {
    /// Plain: move the grabbed side and its opposite symmetrically.
    Pair,
    /// Shift: move only the grabbed side.
    Single,
    /// Ctrl: scale all four sides proportionally.
    Proportional,
}

/// An active guide drag: which layout element, which side, and how it distributes.
#[derive(Clone, Copy)]
struct GuideDrag {
    mode: LayoutMode,
    side: Side,
    kind: DragKind,
}

/// A transient free-object move. The document is only written on release, so a
/// long pointer drag does not flood collaboration/history with intermediate
/// coordinates; the final placement is one canonical action and gets the same
/// stale-target refusal as palette-driven placement.
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

/// A numeric property of a style the panel can scrub or type into.
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
    /// The field the inspector edits this property with.
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
    /// Stepper (+/-) increment.
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

/// A clickable target inside the styles panel, resolved on press.
#[derive(Clone, Debug, PartialEq)]
enum PanelHit {
    /// Dismiss the contextual inspector. Especially important when the panel
    /// is a narrow-window overlay rather than a docked rail.
    Close,
    /// Apply this style id to the selection / caret block.
    Pick(String),
    /// Set the alignment of the **current paragraph(s)** (an instance `align`
    /// attr — direct formatting, not the style default). The "This paragraph"
    /// section, distinct from the named-style editor below it.
    ParaAlign(&'static str),
    SetColor([u8; 3]),
    /// Open the shared colour-picker popover to choose an arbitrary colour for
    /// the focused style (beyond the quick-preset swatches).
    OpenColorPicker,
    /// Duplicate the current style under a fresh id and apply it.
    NewStyle,
    /// Scale the selected image by a factor (a resize stepper: `0.9` shrinks,
    /// `1.1` grows). Keeps the aspect ratio.
    ImageScale(f64),
    /// Mirror the selected image horizontally (left↔right).
    ImageFlipH,
    /// Mirror the selected image vertically (top↔bottom).
    ImageFlipV,
    /// Inset every source edge by five percent. The canonical crop action
    /// rejects any crop that would leave too little source visible.
    ImageCropInset,
    /// Restore the full source image without changing its display frame.
    ImageCropReset,
    /// Put the selected object back in the text flow, between paragraphs.
    ImageInline,
    /// Turn the selected object into a freely positioned page object.
    ImageFreePosition,
    ImageWrapLeft,
    /// Float the selected object at paragraph scope, with text on its left.
    ImageWrapRight,
}

/// The zoom field of the status band: the page scale in percent, stepped
/// by ten.
fn zoom_field() -> ui::widget::NumberInput {
    ui::widget::NumberInput::new(100.0)
        .with_range(ZOOM_MIN * 100.0, ZOOM_MAX * 100.0)
        .with_step(ZOOM_STEP * 100.0)
        .with_suffix("%")
        .control_size(ControlSize::Small)
        .compact()
        .with_accessible_name(t("a11y-zoom"))
}

/// The inspector's line-spacing menu, reporting the index of the spacing
/// picked into `pick`.
fn line_spacing_menu(pick: &std::rc::Rc<std::cell::Cell<Option<usize>>>) -> ui::widget::Dropdown {
    let labels: Vec<String> = block_layout::LINE_SPACINGS
        .iter()
        .map(|spacing| line_spacing_label(*spacing))
        .collect();
    let pick = std::rc::Rc::clone(pick);
    let names = labels.clone();
    ui::widget::Dropdown::from_state(labels, &ui::LocalState::new(String::new()))
        .on_change(move |label| {
            if let Some(index) = names.iter().position(|name| name == label) {
                pick.set(Some(index));
            }
        })
        .control_size(ControlSize::Small)
        .with_accessible_name(t("a11y-line-spacing"))
}

/// The inspector's paragraph-style menu; a pick lands in `choice` as the
/// style's id.
fn style_pick_menu(choice: &std::rc::Rc<std::cell::Cell<Option<String>>>) -> ui::widget::Dropdown {
    let choice = std::rc::Rc::clone(choice);
    ui::widget::Dropdown::from_state(Vec::new(), &ui::LocalState::new(String::new()))
        .on_change(move |id| choice.set(Some(id.to_owned())))
        .control_size(ControlSize::Small)
        .with_accessible_name(t("panel-style"))
}

/// The toggleable inline mark a tilo-doc mark name denotes (Ctrl+B/I/U, toolbar,
/// super/sub), for routing through `editing::toggle_mark`.
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

/// Per-visual-line `(line_top, line_height)` for a shaped layout (same coordinate
/// space as `selection_rects`/`caret_geometry`). Used by intra-block pagination to
/// find the line boundary at which a too-tall block splits across a page.
///
/// `RichTextLayout` has no line iterator, so tops are recovered from selection
/// rects of the full text; blank wrapped lines (no glyphs, no rect) are back-filled
/// at the uniform line advance. Result is sorted, deduped, and spans `[0, height())`.
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
    // `selection_rects` may emit several spans on the same line (one per run); keep
    // one entry per distinct top, preferring the larger height for that line.
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
    // Ensure the first line starts at the top (a leading blank line has no rect).
    let line_h = tops.first().map_or(1.0, |&(_, h)| h).max(1.0);
    while tops.first().is_some_and(|&(top, _)| top > line_h * 0.5) {
        let first_top = tops.first().map_or(0.0, |&(top, _)| top);
        tops.insert(0, (first_top - line_h, line_h));
    }
    // Back-fill any trailing blank lines so the tops span the full height.
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

/// A digest-addressed stand-in key (`PresenceKey::from_scope_digest`).
fn is_scope_digest_key(key: &PresenceKey) -> bool {
    matches!(key, PresenceKey::Widget { path }
        if matches!(path.first(), Some(PathSegment::Field(field)) if field == ui::presence::DIGEST_KEY_FIELD))
}

/// A part of the document state last copied into the editor's views, kept
/// by reference so an unchanged part is recognised without comparing it.
struct Copied<T>(Option<std::rc::Rc<T>>);

impl<T> Default for Copied<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> Copied<T> {
    /// Whether `current` is not the part last seen; remembers it.
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
    /// A copy of `current` when it is not the part last copied.
    fn take_if_new(&mut self, current: &std::rc::Rc<T>) -> Option<T> {
        self.see(current).then(|| T::clone(current))
    }
}

/// Draw on `view` the comment marks of the threads about its text: one mark
/// per thread, valued with the thread's id, as the review margin and the
/// context menu read them.
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

/// What the editor last copied out of the document state.
#[derive(Default)]
struct CopiedParts {
    /// The shared paragraph behind each of `WordProcessorWidget::blocks`.
    blocks: Vec<std::rc::Rc<BlockView>>,
    header: Copied<Vec<BlockView>>,
    footer: Copied<Vec<BlockView>>,
    notes: Copied<Vec<NoteView>>,
    /// The comment threads last drawn, with where their text was.
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
    /// The document's set of content addresses changed: fetch what is new.
    content: bool,
    review: Option<(Vec<tilo_doc::CommentView>, Vec<tilo_doc::ChangeView>)>,
}

/// The collaboration presence key for a document block, keyed by its **stable
/// CRDT block id** (not its index): two peers share the same Automerge object
/// ids for a given block, whereas a block's positional index drifts as peers
/// concurrently insert/delete blocks. Keying by id therefore keeps a remote
/// peer's caret/selection pinned to the right paragraph even while the document
/// is being restructured. Used by BOTH the outbound `presence_payload` (so the
/// App broadcasts the local caret/selection under this key) and the inbound
/// render pass (so received caret/selection presence maps back onto the same
/// block) — keeping the two halves in agreement is the whole point of the
/// single helper.
/// The presence key a word-processor block is addressed by on the wire.
///
/// Spelled out here rather than shared: this is the cross-process contract
/// `apps/agent` reproduces from the outside (it has no window and no pointer,
/// only a caret to publish), so the two sides must independently agree on the
/// exact key shape.
///
/// The presence key of a paragraph. Hosted, it is the framework's resource
/// key for the paragraph, the same on every collaborator's screen whatever
/// their layout; standalone, the editor's own widget path.
fn block_presence_key(block_id: &str) -> PresenceKey {
    ui::framework::ResourceSelection::Item(block_id.to_owned())
        .presence::<tilo_document::RichText>(
            <crate::framework::WordProcessorApp as ui::framework::ResourceApp>::ID,
        )
        .0
}

/// One non-local participant's caret or selection, published by the host
/// under that participant's identity (see
/// [`WordProcessorWidget::participant_presence`]).
pub use ui::ParticipantPresence;

/// The presence payload for `view`, exactly as the local window derives its
/// own in `presence_payload`: the selection's part inside the caret block
/// when non-empty, else the bare caret; byte offsets into the block text.
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

/// A resolved remote collaborator caret to paint this frame: the on-screen
/// caret rectangle (in *content* space, ready to be mapped through the page
/// content transform), the peer's id (for colour + the name label), and the
/// label text. Produced by [`WordProcessorWidget::remote_presence_overlays`]
/// from the live `PresenceCtx` + the frame's laid-out blocks so the geometry is
/// pixel-identical to a local caret — never a re-derived approximation.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteCaretOverlay {
    pub user: UserId,
    pub rect: Rect,
    /// The pill text: the room's name for this peer, or their id if unnamed.
    /// Resolved here (not at paint time) so it is directly assertable.
    pub label: String,
}

/// A resolved remote collaborator selection to paint this frame: one-or-more
/// on-screen rectangles (a wrapped selection spans several lines) in content
/// space, plus the peer it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteSelectionOverlay {
    pub user: UserId,
    pub rects: Vec<Rect>,
}

/// A measured table row within [`WordProcessorWidget::paginate_blocks`]'s
/// table-layout branch: `(row_height, cells)`, each cell as `(col,
/// cell_height, block)`.
type TableRow<'a> = (f64, Vec<(usize, f64, &'a BlockView)>);

/// `prepare_pass2_inputs`'s `(multi_sel, note_nums, spell_by_block)` result:
/// whether the selection spans multiple cells, footnote citation numbers
/// keyed by block id, and (when spell-check is on) misspelled byte spans
/// keyed by block id.
type Pass2Inputs = (
    bool,
    HashMap<String, usize>,
    HashMap<String, Vec<(usize, usize)>>,
);

/// Draw a wavy red spell-check underline from `x0` to `x1` at baseline `y`, in the
/// given (already zoom-adjusted) transform. Approximated by short line segments
/// tracing a sine so it reads as a squiggle without per-glyph curve math.
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

/// A block laid out for the current frame; kept for click hit-testing. The
/// shaped `RichTextLayout` itself lives in `WordProcessorWidget::layout_cache`
/// (keyed by `id`) so it can be reused across frames; look it up by `id`.
#[derive(Clone)]
struct Laid {
    id: String,
    text: String,
    kind: String,
    size: f32,
    /// On-screen top of this slice's visible band (post-scroll). For an unsplit
    /// block this is where the shaped layout's first line sits. For a block split
    /// across a page boundary, each slice is a separate `Laid` and `y_top` is the
    /// top of the band that slice occupies on its page.
    y_top: f64,
    text_x: f64,
    width: f64,
    /// On-screen height of this slice's visible band. For an unsplit block this is
    /// the full shaped-layout height; for a split slice it is only the height of
    /// the lines that fall on this slice's page.
    height: f64,
    /// Y where the shaped layout's internal origin (`line_top` = 0) maps on screen.
    /// All layout-relative geometry (`selection_rects`, `caret_geometry`, `hit`)
    /// is measured from here, so a line at internal `line_top` draws at
    /// `layout_y + line_top`. Equals `y_top` for an unsplit block; for the lower
    /// slice of a split block it sits *above* `y_top` (and above the page) so the
    /// first carried-over line lands at the next page's top margin.
    layout_y: f64,
    /// When a block is split across pages this slice must clip its drawing to its
    /// own band so lines belonging to other slices (which share the same layout,
    /// mapped through `layout_y`) do not bleed onto this page. `None` for unsplit
    /// blocks (the band equals the whole layout, so no clip is needed).
    clip: Option<Rect>,
    bullet: Option<String>,
    /// Resolved text colour from the block's style.
    ink: Color,
    /// For `image` blocks: the blob hash to paint into this block's rect.
    image_hash: Option<String>,
    /// Full grid-cell rectangle (table cells only) — for borders + precise hit.
    cell_box: Option<Rect>,
    /// Grid row index (table cells only) — for header tint + zebra striping.
    cell_row: Option<usize>,
    /// The page index this item was laid onto (0-based). Used to assign footnotes
    /// to the page where their reference marker falls.
    page: usize,
    /// The input method's composition while it is laid out in this block:
    /// `text` stays the committed text, the shaped layout shows the
    /// composition at the caret.
    composition: Option<EditCore>,
}

impl Laid {
    /// The shaped layout's byte for committed char `offset`.
    fn layout_byte(&self, offset: usize) -> usize {
        shown_byte(
            self.composition.as_ref(),
            &self.text,
            char_to_byte(&self.text, offset),
        )
    }

    /// The shaped layout's byte for committed byte `byte`.
    fn shown(&self, byte: usize) -> usize {
        shown_byte(self.composition.as_ref(), &self.text, byte)
    }

    /// The committed char offset of shaped layout byte `byte`.
    fn offset_at(&self, byte: usize) -> usize {
        committed_offset(self.composition.as_ref(), &self.text, byte)
    }

    /// Where the caret at committed char `offset` draws in the shaped layout:
    /// inside the composition at the input method's cursor while composing.
    fn caret_byte(&self, offset: usize) -> usize {
        self.composition.as_ref().map_or_else(
            || char_to_byte(&self.text, offset),
            |core| core.display_caret(&self.text),
        )
    }
}

/// Committed byte `byte` of `text` mapped into the text as laid out with
/// `composition` shown at the caret.
fn shown_byte(composition: Option<&EditCore>, text: &str, byte: usize) -> usize {
    composition.map_or(byte, |core| core.to_display(text, byte))
}

/// Laid-out byte `byte` mapped back to a committed char offset of `text`;
/// a point inside the composition lands at the caret it is composed at.
fn committed_offset(composition: Option<&EditCore>, text: &str, byte: usize) -> usize {
    byte_to_char(
        text,
        composition.map_or(byte, |core| core.from_display(text, byte)),
    )
}

/// A floated image's reserved column, live while subsequent body blocks lay out
/// (coarse text-wrap). While `bottom` has not been passed, paragraphs on
/// the same page are shaped at a reduced width and offset to the non-float side so
/// they sit beside the image rather than under it. This is a *coarse* wrap: the
/// whole paragraph is narrowed for its full height (not per wrapped line), because
/// `ui::richtext` has no per-line exclusion zone. Once `y` clears `bottom`, blocks
/// return to full width.
#[derive(Clone, Copy)]
struct FloatReserve {
    /// `true` = image floated to the LEFT edge (text pushed right); `false` = RIGHT
    /// edge (text stays left, just narrowed).
    left: bool,
    /// Width the text column must give up: image width + gutter (px).
    reserve_w: f64,
    /// Content-space y (pre-scroll) where the image's band ends; blocks whose top
    /// is above this are narrowed, blocks at/below it are full-width again.
    bottom: f64,
    /// Page index the float lives on — a float only narrows blocks on its own page.
    page: usize,
}

/// One header/footer block laid out for the active edit context this frame, kept
/// for click hit-testing + caret rendering inside the band. Mirrors [`Laid`] but
/// carries the shaped layout inline (the band's layouts are small and not cached
/// across frames). Coordinates are content space (the band draws through `ctf`).
struct RegionLaid {
    id: String,
    /// The committed text; `layout` shows `composition` at the caret.
    text: String,
    /// Top-left origin of the run in content space.
    origin: Point,
    layout: RichTextLayout,
    composition: Option<EditCore>,
}

/// US-Letter page height in inches (width is 8.5in → `MAX_PAGE_W` px).
const LETTER_RATIO: f64 = 11.0 / 8.5;
/// Gap between a floated image's edge and the text column that wraps beside
/// it, in [`WordProcessorWidget::paginate_blocks`]'s float-reserve math.
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
    /// Whether the editor follows the host's personal dictionary; false
    /// until it is attached, which keeps added words in memory only.
    personal: bool,
    /// The personal words the dictionary was last built with.
    personal_words: proofing::PersonalDictionary,
    /// The person's spelling preferences every app follows, which name the
    /// language words are checked in; `None` until attached.
    shared: Option<ui::framework::Setting<ui::text_assistance::TextAssistancePrefs>>,
    /// The shared spelling language last applied, so a change made in
    /// Settings (or on another device) reaches an open editor once.
    applied_language: Option<String>,
}

impl ProofingPreferences {
    /// The session's language. A unit test starts from a fixed English
    /// checker instead, so its results do not depend on the host locale; a
    /// test that exercises another sets it explicitly. The person's words
    /// arrive with the editor's settings when it is attached.
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

/// The revision and preview state represented by the materialized body views.
#[derive(Default)]
struct MaterializedBody {
    heads: Vec<ChangeHash>,
    pending_ops: usize,
    preview: bool,
}

pub struct WordProcessorWidget {
    inner: WordProcessor,
    rect: Rect,
    lifecycle: EditorLifecycle,
    /// Where edits go: the framework's dispatcher, which checks the
    /// participant's permission, attributes each edit and groups typing into
    /// undo steps. `None` until attached; nothing is written then.
    dispatch: Option<ui::framework::Dispatcher<crate::framework::Document>>,
    /// The document as the framework keeps it, shared with History mode,
    /// search and the palette. `None` until attached.
    source: Option<ui::framework::Live<crate::framework::Document>>,
    /// The kept state changed in a sync made mid-edit, and the views have
    /// not yet been refreshed from it.
    source_unseen: bool,
    /// What was last copied out of the state, to copy only what changed.
    copied: CopiedParts,
    pointer: Point,
    drag: DragSelect,
    /// The word or paragraph a double- or triple-click selected, as its
    /// `(start, end)` carets: a drag that continues the click grows the
    /// selection by whole units and always keeps this one in it.
    drag_unit: Option<(Caret, Caret)>,
    path: Option<PathBuf>,
    /// Where the document was saved when that place has no path here (a
    /// browser page's Save dialog file), and as what: every later Save
    /// writes there without asking, and only Save As asks again.
    kept: Option<(ui::portal::SaveDestination, WpFormat)>,
    format: Option<WpFormat>,
    /// The window bar's search over the document: its query, matches and
    /// the replacements waiting for the document.
    find: DocumentFind,
    /// Image bytes (content address → bytes) fetched from the content store for the images the
    /// document references, plus a
    /// lazily-decoded image cache (`None` = decode failed / unsupported format).
    blobs: std::collections::BTreeMap<String, Vec<u8>>,
    image_cache: std::collections::HashMap<String, Option<ui::widget::Image>>,
    blocks: Vec<BlockView>,
    /// Canonical body order for a large document. This is intentionally ids
    /// only: offscreen rich text/marks/attrs stay in tilo-doc until its viewport
    /// window asks for them.
    body_order: std::rc::Rc<tilo_document::BodyOrder>,
    /// Start offset into `body_order` represented by `blocks`.
    body_window_start: usize,
    /// Committed/pending revision and ruler previews represented by the body.
    materialized: MaterializedBody,
    #[cfg(test)]
    reference_text_rendering: bool,
    /// Page-header block sequence (drawn in every page's top-margin band). A
    /// `page-number` field marker is substituted with the page number per page.
    header: Vec<BlockView>,
    /// Page-footer block sequence (drawn in every page's bottom-margin band).
    /// Mirrors `header`. When empty, a subtle default "Page N" indicator is drawn.
    footer: Vec<BlockView>,
    laid: Vec<Laid>,
    /// Laid geometry of the header/footer band currently being edited (empty when
    /// editing the body); content space, so hit-testing/caret reuse body coordinates.
    region_laid: Vec<RegionLaid>,
    /// The header / footer band rects of the reference (first) page in content
    /// space, cached each frame so the event handler can hit-test a click into a
    /// band and switch the edit context. `(header_band, footer_band)`.
    head_band_rect: Rect,
    foot_band_rect: Rect,
    /// Per-block shaped-text cache, keyed by shape key (text+marks+style+wrap
    /// width): only edited/resized blocks pay the shaping cost. Pruned each frame.
    layout_cache: HashMap<String, (u64, RichTextLayout)>,
    recent_layout_cache: HashMap<String, (u64, RichTextLayout)>,
    /// Diagnostics for the layout cache, recomputed each `draw`: number of
    /// blocks whose layout was reused (`hits`) vs re-shaped (`misses`).
    dbg_layout_hits: usize,
    dbg_layout_misses: usize,
    /// The command each of the window bar's slots runs, as last declared.
    bar_commands: Vec<(String, window_bar::BarCommand)>,
    paper_theme: Theme,
    /// The window's theme this frame, for what is drawn on the desk.
    desk_theme: Theme,
    /// System text scale used to size shared toolbar geometry and its hit map.
    /// Updated from the live theme before frame geometry is resolved.
    text_scale: f64,
    /// Last rich copy: `(plain_text, blocks)`. Paste uses it when the system
    /// clipboard still holds this exact plain text.
    rich_clip: Option<(String, Vec<ClipBlock>)>,
    /// Vertical scroll offset (px) and the max scroll (content overflow), updated
    /// each frame from the laid-out content height.
    scroll_y: f64,
    scroll_viewport: f64,
    max_scroll: f64,
    /// How much of the canvas the window's search covers from its top, in
    /// content px: the document scrolls that far before its start, so a
    /// match near the start can be shown clear of the search's options.
    find_inset: f64,
    /// What the window's search covers of the editor
    /// ([`ui::widget::Findable::set_find_cover`]).
    find_cover: Option<Rect>,
    /// The document's stylesheet (component-instance styles). Authoritative copy
    /// in the widget; reloaded from the doc when not mid-drag, persisted on edit.
    sheet: crate::styles::StyleSheet,
    /// Page margins — fully customizable by dragging guides on the page.
    margins: Edges,
    layout_mode: LayoutMode,
    /// Active guide drag (the side being dragged), if any.
    guide_drag: Option<GuideDrag>,
    /// A page-positioned image currently being dragged directly on canvas.
    object_drag: Option<ObjectDrag>,
    /// Whether the horizontal ruler band is shown (toggled via the toolbar
    /// "Ruler" button + the View bottom-bar chip). Default ON — it is the primary
    /// surface for margin / indent / tab-stop editing.
    view: ViewPreferences,
    /// Horizontal ruler component: painting, geometry and marker drag lifecycle.
    ruler: Ruler,
    /// The page rect in screen space (post-scroll), cached each frame so the
    /// event handler can hit-test layout guides against it.
    page_rect: Rect,
    /// How the page shows the document's margins ([`pagination::margin_scale`]),
    /// cached each frame with the page rect so the ruler draws and drags them
    /// where the text stands.
    page_margin_scale: f64,
    /// The bottom layout bar's clickable chips: `(mode, rect)`.
    layout_chips: Vec<(LayoutMode, Rect)>,
    layout_bar_height: f64,
    zoom_viewport: Rect,
    /// Whether the contextual styles panel is shown (dismissable via the toolbar
    /// "Styles" toggle).
    /// The styles panel rect in screen space, cached each frame for hit-testing.
    panel_rect: Rect,
    /// Clickable targets inside the styles panel, rebuilt each draw.
    panel_hits: Vec<(Rect, PanelHit)>,
    segmented_touch: SegmentedTouch,
    /// The numeric field currently being typed into (click-to-type), if any.
    editing_field: Option<NumField>,
    /// The inspector's style property fields.
    num_inputs: Vec<NumSlot>,
    /// The inspector's line-spacing menu, the choice it made, and where it
    /// was drawn.
    line_spacing: ui::widget::Dropdown,
    line_spacing_pick: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    line_spacing_rect: Rect,
    /// A failed save, shown as an error banner under the toolbar until the
    /// next save succeeds or it is dismissed; holds the banner's dismiss
    /// control as last drawn.
    save_error: Option<Rect>,
    /// The inspector's paragraph-style menu: the style it picked, the
    /// `(id, name)` choices it offers, and where it was drawn.
    style_pick: ui::widget::Dropdown,
    style_pick_choice: std::rc::Rc<std::cell::Cell<Option<String>>>,
    style_pick_options: Vec<(String, String)>,
    style_pick_rect: Rect,
    /// The inspector's scrolled column and how far it scrolls.
    panel_body: Rect,
    panel_scroll: f64,
    panel_max_scroll: f64,
    /// The zoom field in the status band, and where it was drawn.
    zoom_input: ui::widget::NumberInput,
    zoom_rect: Rect,
    /// Local page scale plus its toolbar drawing and hit-testing.
    zoom: ZoomControl,
    /// Follow the available canvas width until the user explicitly operates a
    /// zoom control. This makes a narrow/tiled editor readable on first open.
    /// Footnotes / endnotes (materialized from the doc), sorted by note id; the
    /// in-text numbering is derived from `note-ref` document order, not this order.
    notes: Vec<NoteView>,
    /// The English dictionary, loaded lazily on first spell-check pass (it is
    /// ~75k words, so building it eagerly would slow every cold start). Cloned
    /// once from the process-wide shared built-in list (parsed at most once per
    /// process), then **owned** by this widget so "Add to dictionary" can splice
    /// user words into it via [`spellcheck::Dictionary::add_word`]. The
    /// person's own words are layered in when it is built.
    dict: Option<spellcheck::Dictionary>,
    /// Whether spell-checking (the red squiggles) is enabled. Default ON.
    /// Document/session dictionary selected for multilingual checking.
    proofing: ProofingPreferences,
    last_autocorrection: Option<DocumentAutocorrection>,
    /// Per-block spell-check cache: block id → (text, misspelled byte spans). Keyed
    /// by the block's text so an edit (which changes the text) invalidates the
    /// entry and re-checks; an unchanged block reuses the spans for free.
    spell_cache: HashMap<String, (String, Vec<(usize, usize)>)>,
    /// Right-click context menu (Cut / Copy / Paste / Select All, plus spell-check
    /// "Add to dictionary" + suggestion rows when the clicked word is flagged).
    /// Reuses the shared [`ContextMenu`] primitive; painted in the overlay pass.
    context_menu: ContextMenu,
    /// Per-row action parallel to `context_menu`'s items (separators map to
    /// `None`). A left-click on a row dispatches by index through this list.
    context_actions: Vec<Option<CtxAction>>,
    /// The compact toolbar's labeled, keyboard/semantic shared swatch grid.
    /// It is local view state; selecting a colour routes through `ToolAction`.
    compact_swatch_palette: CompactSwatchPalette,
    action_palette_request: Option<tilo_actions::ActionAddress>,
    /// Comment/tracked-change visibility, materialized items, card drawing, and
    /// frame-local action hit targets. Document mutation remains editor policy.
    review: ReviewOverlay,
    /// The selected durable comment resource, if its thread composer is open.
    /// This stays outside `ReviewOverlay`: that overlay only lays out inline
    /// anchors/cards; the editor owns input and action dispatch.
    thread_comment: Option<String>,
    /// A materialized snapshot supplied only by the headless visual scenario.
    /// Interactive threads always resolve their current value from `review`.
    thread_snapshot: Option<tilo_doc::snapshot::CommentView>,
    /// The reply being written, in the SDK editor: Ctrl+Enter sends it,
    /// Escape closes the thread.
    thread_reply: Embedded<RichTextSurface<Draft>>,
    /// The thread's replies as written, one view each.
    thread_reply_views: Vec<RichTextView>,
    thread_panel: Rect,
    thread_submit: Rect,
    thread_cancel: Rect,
    /// Document-outline visibility, drawing, and frame-local hit geometry.
    outline: OutlinePane,
    /// OS-theme-driven chrome palette, resolved at the top of every `draw` from
    /// the passed-in [`Theme`]. Every chrome helper reads this instead of
    /// hard-coded colours, so the editor honours light/dark mode + the OS
    /// palette. Initialised to the dark theme's mapping for off-frame use.
    chrome: ChromeColors,
    /// The outline (the leading sidebar) and the inspector (the trailing
    /// panel): docked beside the page in a wide window, floating drawers
    /// over a scrim in a narrow one.
    outline_drawer: ui::widget::SidebarDrawer,
    inspector_drawer: ui::widget::SidebarDrawer,
    /// Crash-recovery persistence and its Restore/Discard surface.
    /// The "New" picker owns its shared menu, lifecycle, and typed outcomes.
    file_menu: FileMenu,
    /// An in-flight request for an image to insert: `Some` while the host's
    /// chooser is open; its answer is drained each frame in
    /// [`Self::drain_image_choice`].
    portal_rx: Option<portal::ContentsChannel>,
    /// A save whose destination the person chose, written on the next sync
    /// with the document, and the caller it answers.
    pending_save: Option<(PathBuf, ui::framework::EffectReply)>,
    /// Right-click style-menu lifecycle, drawing, hit-testing, and typed outcome.
    style_menu: StyleMenu,
    /// A transient operation result (e.g. save confirmation or failure),
    /// drawn as a toast in the overlay pass. Dismissed on the next interaction.
    toast: Notice,
    /// The last frame's drawing, part by part, so a frame reports where it
    /// differs from it.
    paint_damage: ui::widget::paint_damage::PaintDamage,
    ime_rect: Option<Rect>,
    /// The style inspector's shared color-picker popover. It owns its lifecycle,
    /// layout, drawing, and modal input routing; this editor applies its result.
    color_picker: StyleColorPicker,
    /// Views of the other participants (agents, remote invokers) this widget
    /// serves: bounded, idle-expiring, rebased on every change, and never
    /// written into the document. The local window's view is `inner`.
    participants: participants::ParticipantViews,
    /// A screenshot scenario to stage in the next sync, which has the
    /// document its actions wrote.
    staged_scenario: Option<String>,
    /// The person's layout setting, and the layout last applied from or
    /// written to it; `None` until attached in a window someone uses.
    layout: Option<(
        ui::framework::Setting<proofing::WriterLayout>,
        Option<proofing::WriterLayout>,
    )>,
    /// The file whose record this is, when the window was launched on one:
    /// writing it records what it now holds, so a later open can tell a
    /// change made elsewhere.
    record_file: Option<PathBuf>,
    /// The Find results of this window's view and of each participant's,
    /// as the document they were resolved against last placed them: what
    /// [`ResourceSurface::live_rows`] publishes.
    bound: BoundRows,
    /// The view an edit effect this window's person invoked leaves them
    /// with, adopted once the document moved past the version it was built
    /// against (the edit applied).
    pending_local: Option<(LocalVersion, crate::actions::participant::ParticipantView)>,
    /// The last action the editor's own affordances dispatched, so a test can
    /// compare it with what the matching published operation builds.
    #[cfg(test)]
    last_dispatched: Option<crate::actions::RichTextAction>,
}

/// The Find results the views a window serves offer, resolved at its last
/// sync.
#[derive(Default)]
struct BoundRows {
    local: Vec<ui::framework::LiveRow>,
    participants: HashMap<tilo_actions::ParticipantId, Vec<ui::framework::LiveRow>>,
}

/// Committed heads plus the open transaction's size: changes whenever an
/// edit applied.
type LocalVersion = (Vec<automerge::ChangeHash>, usize);

fn local_version(doc: &AutoCommit) -> LocalVersion {
    use automerge::transaction::Transactable;
    (doc.base_heads(), doc.pending_ops())
}

/// The host chooser's request for a document to open.
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

/// The host chooser's request for where to save, suggesting `suggested`.
/// The format follows the extension chosen.
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

/// One process-wide built-in dictionary, shared across every word-processor
/// widget so the ~75k-word list is parsed at most once per process.
fn shared_dictionary() -> std::sync::Arc<spellcheck::Dictionary> {
    use std::sync::OnceLock;
    static DICT: OnceLock<std::sync::Arc<spellcheck::Dictionary>> = OnceLock::new();
    DICT.get_or_init(|| std::sync::Arc::new(spellcheck::Dictionary::builtin()))
        .clone()
}

impl Default for WordProcessorWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl WordProcessorWidget {
    /// Construct from the canonical record; this performs no document writes.
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
        let line_spacing_pick = std::rc::Rc::new(std::cell::Cell::new(None));
        let style_pick_choice = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut widget = Self {
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
            bar_commands: Vec::new(),
            paper_theme: Theme::light(),
            desk_theme: Theme::dark(),
            text_scale: 1.0,
            rich_clip: None,
            scroll_y: 0.0,
            scroll_viewport: 0.0,
            max_scroll: 0.0,
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
            // Keep the writing surface primary on first launch. The complete
            // style inspector is one click away and becomes an overlay on a
            // narrow tile rather than permanently consuming half the canvas.
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
            line_spacing: ui::widget::Dropdown::new(Vec::new(), ""),
            line_spacing_pick: std::rc::Rc::clone(&line_spacing_pick),
            line_spacing_rect: Rect::ZERO,
            save_error: None,
            style_pick: ui::widget::Dropdown::new(Vec::new(), ""),
            style_pick_choice: std::rc::Rc::clone(&style_pick_choice),
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
            // Overwritten at the top of every `draw` from the live theme.
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
        };
        widget.line_spacing = line_spacing_menu(&line_spacing_pick);
        widget.style_pick = style_pick_menu(&style_pick_choice);
        widget
    }

    /// Take part in the framework: read the document from its live state and
    /// submit edits through its dispatcher.
    pub(crate) fn attach_framework(
        &mut self,
        live: ui::framework::Live<crate::framework::Document>,
        dispatcher: ui::framework::Dispatcher<crate::framework::Document>,
    ) {
        self.inner.attach(dispatcher.clone());
        self.dispatch = Some(dispatcher);
        self.source = Some(live);
    }

    /// An editor attached outside any host, as a unit test drives it: its
    /// edits go through a detached dispatcher to the document its `sync`
    /// receives, whose live state it reads from the first sync on.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        let mut widget = Self::new();
        let dispatch = ui::framework::testing::detached_dispatcher("test-author");
        widget.inner.attach(dispatch.clone());
        widget.dispatch = Some(dispatch);
        widget
    }

    /// The last action the editor's own affordances dispatched.
    #[cfg(test)]
    pub(crate) fn last_dispatched_for_test(&self) -> Option<&crate::actions::RichTextAction> {
        self.last_dispatched.as_ref()
    }

    /// The window shows the record of the file at `path`.
    pub(crate) fn show_record_of(&mut self, path: PathBuf) {
        let format = WpFormat::from_path(&path);
        self.set_file(path.clone(), format);
        self.record_file = Some(path);
    }

    /// `bytes` were written to `path`: when that is the record's own file,
    /// the record notes what the file now holds.
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

    /// The Find results of the view of whoever is invoking, as rows.
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

    /// Whether `word` is spelled right here: in the dictionary, or one of
    /// the person's own words.
    pub fn is_spelled_right(&mut self, word: &str) -> bool {
        self.dictionary().contains(word)
    }

    /// Check spelling in the language the person's shared spelling
    /// preferences name, and lay the editor out as they last left it.
    pub(crate) fn follow_preferences(
        &mut self,
        shared: ui::framework::Setting<ui::text_assistance::TextAssistancePrefs>,
        layout: ui::framework::Setting<proofing::WriterLayout>,
    ) {
        self.proofing.shared = Some(shared);
        self.layout = Some((layout, None));
        self.follow_settings();
    }

    /// Bring the spelling language and the panels up to date with the
    /// person's settings, and keep a panel they opened or closed here in
    /// their layout.
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

    /// Build the spelling dictionary with the person's own words, and keep
    /// the words they add there.
    pub(crate) fn follow_personal_dictionary(&mut self) {
        self.proofing.personal = true;
        self.dict = None;
        self.spell_cache.clear();
    }

    /// The framework presented a new document state: draw it.
    pub(crate) fn mark_document_changed(&mut self) {
        self.lifecycle.dirty = true;
    }

    /// Whether this participant may only view the document.
    pub(crate) fn read_only(&self) -> bool {
        self.dispatch
            .as_ref()
            .is_none_or(ui::framework::Dispatcher::read_only)
    }

    /// Lazily build (once) the widget's owned spell-check dictionary: a clone of
    /// the process-wide shared built-in list (parsed at most once per process)
    /// with any persisted user words layered in. Owned so "Add to dictionary"
    /// can mutate it.
    fn dictionary(&mut self) -> &mut spellcheck::Dictionary {
        // Explicit words added through the shared local host follow every
        // window: build the spelling overlay again when they change.
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

    /// Misspelled byte spans for a body block, using `spell_cache` (keyed by the
    /// block's text so an edit re-checks and an unchanged block is free).
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

    /// Toggle the spell-check squiggles on/off (default ON). View-only; forces a
    /// redraw.
    pub fn toggle_spellcheck(&mut self) {
        self.proofing.enabled = !self.proofing.enabled;
        self.lifecycle.dirty = true;
    }

    /// Whether the spell-check squiggles are currently enabled.
    pub fn spellcheck_enabled(&self) -> bool {
        self.proofing.enabled && ui::text_assistance::preferences().check_spelling
    }

    pub fn spell_language(&self) -> &str {
        &self.proofing.language
    }

    /// Whether the ruler, the styles panel and the outline are shown.
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

    /// Toggle comment + tracked-change markup on/off (default ON). View-only;
    /// forces a redraw and never mutates the document. When off, commented text
    /// shows without its highlight band/indicator and tracked-change runs render
    /// as plain text (no review colour / underline / strike).
    pub fn toggle_markup(&mut self) {
        self.review.toggle();
        // Shaping depends on review visibility because it changes per-run
        // colours, so force every block to reshape next frame.
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.lifecycle.dirty = true;
    }

    /// Whether comment + tracked-change markup is currently drawn.
    pub fn markup_enabled(&self) -> bool {
        self.review.is_visible()
    }

    pub fn model(&self) -> &WordProcessor {
        &self.inner
    }
    /// Put the responsive style inspector in a deterministic state for
    /// examples, visual regression shots and embedders.
    pub fn set_styles_panel_open(&mut self, open: bool) {
        self.view.styles_panel_visible = open;
        self.lifecycle.dirty = true;
    }

    /// Stage a representative non-destructive error toast for visual/a11y
    /// regression shots. Runtime failures use the same toast surface.
    pub fn stage_error_for_shot(&mut self) {
        self.save_error = Some(Rect::ZERO);
        self.lifecycle.dirty = true;
    }

    /// Show the matches of `query` for visual review. The query runs through
    /// the complete-document search path, so this catches virtualized or
    /// offscreen result regressions rather than painting mock highlights.
    pub fn stage_find_for_shot(&mut self, state: &AutoCommit, query: &str) {
        self.find_now(state, query);
    }

    /// Open one materialized durable comment thread for visual/a11y review.
    /// The screenshot path still uses the ordinary production panel rather
    /// than a fixture-only rendering branch.
    pub fn stage_comment_thread_for_shot(&mut self, comment: tilo_doc::snapshot::CommentView) {
        self.thread_comment = Some(comment.id.clone());
        self.thread_snapshot = Some(comment);
        self.focus_thread_reply(true);
        self.lifecycle.dirty = true;
    }

    /// Stage `scenario`'s view state, and the review and image edits its
    /// paragraphs carry, in the next action settlement.
    /// A plain capture stages nothing.
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

    /// The first paragraph with text, and its length in characters.
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

    /// A comment by one reviewer and a tracked insertion by another on the
    /// first paragraph, and with `thread` two replies and the thread panel
    /// open: the review a collaborator's document arrives with.
    fn stage_review(&mut self, state: &mut ui::widget::Edits<'_>, thread: bool) {
        use crate::model::{DocumentAction, document};
        let Some((block, len)) = Self::first_text_block(state) else {
            return;
        };
        self.perform_verb_as(
            state,
            DocumentAction::AddComment(document::AddComment {
                block: block.clone(),
                start: 0,
                end: len.min(12),
                text: "Could we make this opening more specific?".into(),
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
            self.perform_verb_as(
                state,
                DocumentAction::ReplyToComment(document::ReplyToComment {
                    comment: ui::tilo_resource::Id::from_raw(comment.clone()),
                    text: text.into(),
                    body: None,
                }),
                Some(author),
            );
        }
        if let Some(view) = threads(state).into_iter().find(|item| item.id == comment) {
            self.stage_comment_thread_for_shot(view);
        }
    }

    /// An image placed after the first paragraph, framed at an intentional
    /// size and selected, with the object inspector open.
    fn stage_selected_image(&mut self, state: &mut ui::widget::Edits<'_>) {
        // A tiny valid RGB PNG: real decodable bytes cover the renderer's
        // blob and image-cache path, while the pixels do not matter.
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

    /// Write `state` to `path` as `format`. A Save As destination becomes
    /// the document's own once the write succeeds; export-only formats leave
    /// it as it was.
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

    /// `state` as a file in `format`, keeping the comment threads in its
    /// text, as file formats do.
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

    /// Set the zoom factor (clamped) and request a repaint. Returns whether it
    /// changed.
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

    /// The content (page) transform for this frame: a scale about the cached
    /// top-center anchor. Everything inside the page clip draws through this so
    /// the toolbar / find-bar / status bar stay unscaled.
    fn content_transform(&self, base: Affine) -> Affine {
        self.zoom.transform(base)
    }

    /// Map a screen-space pointer into the unscaled content coordinate space the
    /// `Laid` rects live in (the inverse of `content_transform`'s scale-about).
    fn pointer_to_content(&self, p: Point) -> Point {
        self.zoom.screen_to_content(p)
    }

    /// The bounded slice to materialize around the viewport, including at the
    /// top. The insertion point can stay far away while the user scrolls; caret
    /// navigation explicitly moves the viewport through `scroll_to_block`.
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

    /// Materialize the body only when its CRDT heads change, or when scrolling
    /// moves a large document's bounded working set. tilo-doc's requested-block API
    /// preserves the canonical id order without doing a whole-region traversal.
    fn refresh_blocks(&mut self, state: &AutoCommit) {
        self.refresh_blocks_after_text_edit(state, None);
    }

    /// Follow the document through the incrementally maintained state: a
    /// change re-reads only the paragraphs it touched, and only the window
    /// around the viewport is ever read. What changed is copied into the
    /// views the layout draws from, paragraph by paragraph.
    fn refresh_blocks_after_text_edit(&mut self, state: &AutoCommit, _text_block: Option<&str>) {
        if self.inner.containers.is_none() {
            return;
        }
        let moved = self.refresh_live(state) | std::mem::take(&mut self.source_unseen);
        // Search ranges are character offsets, not stable anchors: an open
        // search reads the paragraphs again when the document moves.
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
                // Paragraphs: keep a view whose paragraph is unchanged (same
                // shared value), copy the rest.
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
                // Comment threads live beside the text: where each one's text
                // is now is read from its anchor, and drawn as the comment
                // mark on the paragraphs shown.
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
        // The spell cache deliberately follows the materialized render window:
        // no offscreen import can retain shaped/checker text indefinitely.
        let live: std::collections::HashSet<&str> =
            self.blocks.iter().map(|b| b.id.as_str()).collect();
        self.spell_cache.retain(|id, _| live.contains(id.as_str()));
    }

    /// The running region (if any) whose reference-page band contains content-
    /// space point `p`. Used to switch the edit context on a click.
    fn band_at_pointer(&self, p: Point) -> Option<Region> {
        if self.head_band_rect.height() > 0.0 && self.head_band_rect.contains(p) {
            Some(Region::Header)
        } else if self.foot_band_rect.height() > 0.0 && self.foot_band_rect.contains(p) {
            Some(Region::Footer)
        } else {
            None
        }
    }

    /// Resolve a pointer position to a caret in the **active running region's**
    /// band (`region_laid`) and apply it. Mirrors the body `caret_from_pointer`
    /// but over the band's laid geometry.
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
        // Fall back to the nearest band block (first/last) so a click anywhere in
        // the band still lands a caret.
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

    /// Resolve a pointer position to a caret and apply it (optionally extending
    /// the selection for click-drag).
    fn caret_from_pointer(&mut self, p: Point, extend: bool) {
        // When editing a running region, the caret lives in that band — hit-test
        // its laid geometry, not the body's.
        if self.inner.in_running_region() {
            self.band_caret_from_pointer(p, extend);
            return;
        }
        // Find the laid block under the pointer: cells hit by their full box (so
        // the right column is chosen), normal blocks by their vertical band.
        let mut hit: Option<(String, usize)> = None;
        for l in &self.laid {
            let inside = match l.cell_box {
                Some(bx) => bx.contains(p),
                None => p.y >= l.y_top && p.y < l.y_top + l.height,
            };
            if inside {
                // Hit-test in the layout's own coordinate space: `layout_y` maps the
                // shaped layout origin onto this slice, so `p.y - layout_y` is the
                // internal y of the line under the pointer (correct even when the
                // line lives in a page-split slice whose band starts mid-layout).
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
            // Paragraph spacing and page gaps belong to the nearest visible
            // text band, not the document's last block. Preserve x while clamping
            // y into that slice so dragging through whitespace stays local.
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
    /// The block the input method composes in, and its composition.
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
}

struct NotesSpellcheckInput<'a> {
    spell_by_block: &'a HashMap<String, Vec<(usize, usize)>>,
    note_numbers: &'a HashMap<String, usize>,
    page_notes: &'a mut HashMap<usize, Vec<usize>>,
}

/// The running column of the inspector: where the next row goes, what it
/// draws with, and the visible band rows are clipped to.
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

/// One numeric style property's field in the inspector.
struct NumSlot {
    field: NumField,
    input: ui::widget::NumberInput,
    /// Decimals the field shows: none unless the document's value has some.
    decimals: usize,
    /// Where it was drawn this frame (empty while scrolled out of view).
    rect: Rect,
}

/// One node of the word processor's accessibility tree: an [`ui::a11y::AccessNode`]
/// plus its accessible children.
///
/// The word processor renders its blocks directly (materialized [`BlockView`]s,
/// not child widgets), so [`Widget::accessibility`](ui::widget::Widget::accessibility)'s usual "single local node +
/// `Widget::children`" path doesn't apply. Instead it builds the whole tree itself
/// via [`WordProcessorWidget::accessibility_tree`], with this struct as the carrier;
/// `accessibility` returns this tree's root so the host still sees a focusable
/// `Document`.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yTreeNode {
    /// The local semantics of this element.
    pub node: ui::a11y::AccessNode,
    /// This element's accessible children, in reading order.
    pub children: Vec<A11yTreeNode>,
}

struct FrameGeometry {
    canvas: Rect,
    /// Conservative bounds of the actual canvas clip in final scene space.
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

/// [`WordProcessorWidget::paginate_blocks`]'s pagination + text-shaping
/// output, carried into the drawing passes that follow it.
struct PaginationResult {
    laid: Vec<Laid>,
    cache: HashMap<String, (u64, RichTextLayout)>,
    cur_page: usize,
    y: f64,
    blocks: Vec<BlockView>,
}

/// Loop-carried pagination state threaded through
/// [`WordProcessorWidget::paginate_blocks`]'s per-block-kind handlers.
struct PageCursor {
    /// Current page index.
    cur_page: usize,
    /// Highest page named by an out-of-flow object. It keeps the paper stack
    /// present even when normal text flow has not reached that page yet.
    max_object_page: usize,
    /// Whether anything has landed on the current page yet (the first item
    /// on a page never breaks, so an over-tall block simply overflows its
    /// own page — acceptable for v1).
    placed_on_page: bool,
    /// The running content y.
    y: f64,
    /// The currently-active floated image (anchor=paragraph/page,
    /// wrap=left/right), if any: subsequent paragraphs on its page are
    /// narrowed to flow beside it (coarse wrap — see [`FloatReserve`]).
    /// Cleared once `y` clears the float's bottom, when the page advances
    /// past it, or when the next float supersedes it.
    active_float: Option<FloatReserve>,
    /// Per-level ordered-list counters (index = nesting level); reset when a
    /// non-list block is reached or a shallower level re-enters.
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

/// [`WordProcessorWidget::resolve_paragraph_shape`]'s output: everything
/// [`WordProcessorWidget::paginate_paragraph`] needs to decide where the
/// block lands and how to lay it out.
struct ParagraphShape {
    st: crate::styles::StyleProps,
    indent: f64,
    bullet: Option<String>,
    /// The coarse float-wrap x-origin offset, valid only while the active
    /// float still applies to this block's (possibly page-advanced) page —
    /// the caller re-checks that before using it.
    float_offset: f64,
    wrap_w: f32,
    key: u64,
    layout: RichTextLayout,
}

/// The resolved per-block draw geometry [`WordProcessorWidget::paginate_paragraph`]
/// and [`WordProcessorWidget::emit_paragraph_slices`] both push into `Laid`
/// entries for one paragraph-like block.
struct ParagraphPlacement {
    text_x: f64,
    width: f64,
    size: f32,
    bullet: Option<String>,
    ink: Color,
    space_after: f32,
}

/// Per-frame accumulators [`WordProcessorWidget::draw_page_content`] collects
/// while painting `laid`, rendered by the caller once the pass completes.
struct Pass2Markers {
    page_notes: HashMap<usize, Vec<usize>>,
    comment_markers: Vec<(String, f64, f64)>,
    change_markers: Vec<(String, f64, f64)>,
}

/// One [`Laid`] item's shaped layout + its on-screen origin y, bundled so
/// [`WordProcessorWidget::draw_page_content`]'s per-item decoration helpers
/// below don't each carry the same three parameters.
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
        let mut widget = WordProcessorWidget::detached();
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
        let after = widget.primary_vertical_scroll().unwrap().metrics;
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
