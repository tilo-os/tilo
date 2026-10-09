//! `WordProcessorWidget` — the `ui::Widget` for the word processor. Renders the
//! `qdoc` document as a centered page (à la `LibreOffice` Writer) using the
//! `RichTextLayout` primitive: word-wrapped paragraphs, headings, list bullets,
//! inline **bold**/*italic*/underline, a selection highlight, and a caret. Maps
//! pointer + keyboard onto the editing model (selection, formatting, save).

use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_doc::model::Region;
use tilo_doc::snapshot::{AttrVal, BlockView, NoteView};
use ui::gesture::{DragSelect, Granularity};
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_edit::EditCore;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
// Provides `.r()/.g()/.b()` accessors on `vello` `Color` (used to derive
// theme-tinted translucent chrome tones).
use tilo_rich_text::{Draft, FieldOutcome, RichTextSurface, RichTextView, SubmitKey};
use ui::ColorCompat;
#[cfg(test)]
use ui::clipboard_copy;
use ui::icons::Icon;
use ui::permissions::UserId;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{BlockEditor, Embedded, Widget};
use ui::{PathSegment, PresenceCtx, PresenceKey, clipboard_paste};

use crate::WpFormat;
use crate::compact_swatch_palette::{
    CompactSwatchPalette, PaletteChoice, PaletteKind, PaletteRequest,
};
use crate::effects::WordProcessorEffect;
use crate::find_bar::{Blocks, FindBar, FindEffect, FindHost, FindMatch};
use crate::i18n::{arg, t, t_args};
use crate::num;
use crate::outline_pane::OutlinePane;
use crate::paint_context::PaintContext;
use crate::review_overlay::{
    DELETION_INK, INSERTION_INK, ReviewAction, ReviewColors, ReviewOverlay, ReviewPlacement,
};
#[cfg(test)]
use crate::review_overlay::{DELETION_RGB, INSERTION_RGB};
#[cfg(test)]
use crate::ruler::RulerMarker;
use crate::ruler::{Ruler, RulerEffect, RulerModel};
use crate::style_color_picker::StyleColorPicker;
use crate::style_menu::{StyleMenu, StyleMenuAction};
use crate::template_picker::TemplatePicker;
use crate::text_offsets::{byte_to_char, char_to_byte};
use crate::word_processor::{Caret, ClipBlock, WordProcessor};
use crate::zoom_control::{MAX as ZOOM_MAX, MIN as ZOOM_MIN, STEP as ZOOM_STEP, ZoomControl};
use ui::paint::with_alpha;
use ui::portal;
use ui::tilo_resource::Effect as _;
use ui::tokens::TextRole;
use ui::widget::{
    Button, ButtonFace, ButtonRole, CardFace, ColorSwatchFace, ControlSize, SegmentedFace,
    SegmentedTouch, SegmentedTouchGroup, SegmentedTouchResult, Tone,
};

mod block_layout;
mod image_layout;
mod layout_cache;
mod participants;
pub(crate) use participants::note_participant_outcome;
#[cfg(test)]
mod participant_tests;
mod proofing;
mod semantic_geometry;
mod text_runs;
mod toolbar;

#[cfg(test)]
use block_layout::LIST_MARKER_TEXT_GAP;
use block_layout::{
    LIST_INDENT_STEP, align as block_align, bullet_glyph, cell_rc, cell_table, font as parse_font,
    indent as block_indent_attr, line_spacing as block_line_spacing, line_spacing_label,
    list_level, list_marker_x, page_break_before as block_page_break_before, style_align,
    style_key as block_style_key,
};
#[cfg(test)]
use image_layout::PaintOptions as ImagePaintOptions;
use image_layout::{
    anchor as image_anchor, crop_is_active as image_crop_is_active,
    cropped_source as cropped_image_source, explicit_size as image_explicit_size,
    floats as image_floats, free_position as image_free_position,
    paint_options as image_paint_options, paint_transform as image_paint_transform,
    wrap as image_wrap,
};
use layout_cache::{ShapeKeyInput, shape_key};
use proofing::misspelled_spans;
pub use proofing::{PersonalDictionary, WriterLayout};
#[cfg(test)]
use text_runs::block_runs;
use text_runs::{
    block_change_decos, block_change_ranges, block_comment_ranges, block_note_refs,
    block_runs_markup, char_font_family, char_font_size, char_highlight, composed_block,
    note_numbering, note_text, parse_hex_rgb, running_head_runs, running_head_runs_editing,
};
use toolbar::{HIGHLIGHT_PALETTE, STYLE_PALETTE, ToolAction, ToolEntry};

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
/// The window width (at 1×) from which the outline docks beside a page it
/// leaves readable; below it the outline is a drawer over the page.
const OUTLINE_DOCKS_FROM: f64 = ui::tokens::Metrics::BASE.sidebar_width as f64 + 600.0;
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
    /// Set the focused style's font family.
    SetFont(&'static str),
    ToggleBold,
    ToggleItalic,
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
    /// Float the selected object at paragraph scope, with text on its right.
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

/// The toggleable inline mark a qdoc mark name denotes (Ctrl+B/I/U, toolbar,
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
    // Step in half-periods, alternating the peak above/below the baseline.
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
    /// The person's own words, which "Add to dictionary" adds to and the
    /// dictionary is built with; `None` until the editor is attached, which
    /// keeps added words in memory only.
    personal: Option<ui::framework::Setting<proofing::PersonalDictionary>>,
    /// The personal words the dictionary was last built with.
    personal_words: proofing::PersonalDictionary,
    migration_requested: bool,
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
            personal: None,
            personal_words: proofing::PersonalDictionary::default(),
            migration_requested: false,
            shared: None,
            applied_language: None,
        }
    }
}

#[derive(Default)]
struct StagedOverlays {
    template_picker: bool,
    toolbar_overflow: bool,
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
    path: Option<PathBuf>,
    format: Option<WpFormat>,
    /// Find & Replace owns its query modes, match cursor, drawing, and hit map.
    find: FindBar,
    /// Image bytes (content address → bytes) fetched from the content store for the images the
    /// document references, plus a
    /// lazily-decoded image cache (`None` = decode failed / unsupported format).
    blobs: std::collections::BTreeMap<String, Vec<u8>>,
    image_cache: std::collections::HashMap<String, Option<ui::widget::Image>>,
    blocks: Vec<BlockView>,
    /// Canonical body order for a large document. This is intentionally ids
    /// only: offscreen rich text/marks/attrs stay in qdoc until its viewport
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
    /// Each visible toolbar command's rect this frame, for hit-testing and
    /// the accessibility tree.
    toolbar: Vec<(ToolAction, Rect)>,
    /// The toolbar itself, hosted in the painted chrome.
    tool_bar: ui::widget::Toolbar,
    /// The commands the toolbar was built from.
    tool_entries: Vec<ToolEntry>,
    /// The command the toolbar fired since the last event was routed.
    tool_fired: std::rc::Rc<std::cell::Cell<Option<ToolAction>>>,
    /// The toolbar's band this frame.
    tool_band: Rect,
    /// The Find bar's band this frame (empty while it is closed).
    find_band: Rect,
    /// The theme for what is drawn on the paper, this frame.
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
    /// One-shot headless staging deferred until toolbar geometry is laid out.
    staged: StagedOverlays,
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
    /// Their show/hide buttons at the ends of the toolbar band, the
    /// window's header row.
    outline_toggle: Rect,
    inspector_toggle: Rect,
    /// The window's History control in the header row's trailing corner,
    /// when the host offers it.
    window_history: ui::widget::HistoryOpener,
    window_history_rect: Rect,
    /// The retained toolbar or History button changed since the tree was built.
    retained_tree_stale: bool,
    /// Crash-recovery persistence and its Restore/Discard surface.
    /// The "New" picker owns its shared menu, lifecycle, and typed outcomes.
    template_picker: TemplatePicker,
    /// An in-flight request for an image to insert: `Some` while the host's
    /// chooser is open; its answer is drained each frame in
    /// [`Self::drain_image_choice`].
    portal_rx: Option<portal::PortalChannel>,
    /// A save whose destination the person chose, written on the next sync
    /// with the document, and the caller it answers.
    pending_save: Option<(PathBuf, ui::framework::EffectReply)>,
    // ---- paragraph-style context menu -------------------------------------
    /// Right-click style-menu lifecycle, drawing, hit-testing, and typed outcome.
    style_menu: StyleMenu,
    // ---- transient status toast -------------------------------------------
    /// A transient operation result (e.g. save confirmation or failure),
    /// drawn as a toast in the overlay pass. Dismissed on the next interaction.
    toast: Option<(String, ToastKind)>,
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
    ///
    /// # Errors
    ///
    /// Returns an error when the document does not carry the Writer schema.
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
            path: None,
            format: None,
            find: FindBar::default(),
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
            toolbar: Vec::new(),
            tool_bar: ui::widget::Toolbar::new(ui::widget::toolbar::Orientation::Horizontal),
            tool_entries: Vec::new(),
            tool_fired: std::rc::Rc::new(std::cell::Cell::new(None)),
            tool_band: Rect::ZERO,
            find_band: Rect::ZERO,
            paper_theme: Theme::light(),
            desk_theme: Theme::dark(),
            text_scale: 1.0,
            rich_clip: None,
            scroll_y: 0.0,
            scroll_viewport: 0.0,
            max_scroll: 0.0,
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
            staged: StagedOverlays::default(),
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
            outline_toggle: Rect::ZERO,
            inspector_toggle: Rect::ZERO,
            window_history: ui::widget::HistoryOpener::new(),
            window_history_rect: Rect::ZERO,
            retained_tree_stale: false,
            template_picker: TemplatePicker::default(),
            portal_rx: None,
            pending_save: None,
            style_menu: StyleMenu::default(),
            toast: None,
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

    // ---- Framework hosting ---------------------------------------------------

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
            dispatch.invoke(
                crate::model::Document::NOTE_FILE_WRITTEN.with(tilo_content::hash_bytes(bytes)),
            );
        }
    }

    /// Resolve the Find results of this window's view and of each
    /// participant's against `doc` as it is now.
    fn resolve_bound_edits(&mut self, doc: &AutoCommit) {
        if self.read_only() {
            self.bound = BoundRows::default();
            return;
        }
        let local = self
            .wp_resource_ctx(doc)
            .map(|ctx| crate::actions::search_rows(&ctx, doc))
            .unwrap_or_default();
        let mut participants = HashMap::new();
        let known: Vec<_> = self.participants.participants().cloned().collect();
        for participant in known {
            let Some(ctx) = self
                .participant_view_or_default(&participant, doc)
                .and_then(|view| view.ctx(doc))
            else {
                continue;
            };
            participants.insert(participant, crate::actions::search_rows(&ctx, doc));
        }
        self.bound = BoundRows {
            local,
            participants,
        };
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

    /// Adopt the view an edit effect left this window's person with, once
    /// the edit applied.
    fn adopt_local_outcome(&mut self, state: &AutoCommit) {
        let Some((version, _)) = &self.pending_local else {
            return;
        };
        if *version == local_version(state) {
            return;
        }
        if let Some((_, next)) = self.pending_local.take() {
            let body = self.kept_body();
            let next = next.settle_in(state, body.as_deref());
            self.adopt_local_view(&next, state);
        }
    }

    /// Make the edit an edit effect names at the invoker's caret: a
    /// participant's own view when one invoked it, else this window's. The
    /// edit goes through the door as the invoker's, one undo step.
    fn answer_edit_effect(
        &mut self,
        effect: &WordProcessorEffect,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
    ) -> Option<ui::framework::EffectAnswer> {
        let doc = match cx.document() {
            Some(doc) => doc.clone(),
            None => return Some(Err("the document is not at hand".into())),
        };
        let participant = ui::invocation_scope::current_participant();
        let view = match &participant {
            Some(participant) => match self.participant_view_or_default(participant, &doc) {
                Some(view) => view,
                None => return Some(Err("the document has no body to place a caret in".into())),
            },
            None => self.local_edit_view(),
        };
        let dispatch = cx.dispatcher();
        if let WordProcessorEffect::CommentSelection { text } = effect {
            return Some(
                crate::actions::editing::comment_range(&view, &doc)
                    .map(|range| {
                        dispatch.invoke(crate::model::Document::ADD_COMMENT.with(
                            crate::model::document::AddComment {
                                block: range.block,
                                start: range.start,
                                end: range.end,
                                text: text.clone(),
                            },
                        ));
                        ui::framework::Effected::done()
                    })
                    .map_err(|error| error.to_string()),
            );
        }
        let built = effect.edit(&view, &doc, self.inner.content_store())?;
        let (envelopes, next) = match built {
            Ok(outcome) => outcome,
            Err(error) => return Some(Err(error.to_string())),
        };
        if envelopes.is_empty() {
            return Some(Err("there is nothing to change here".into()));
        }
        dispatch.group(|| {
            for op in &envelopes {
                dispatch.submit(crate::model::edit(op));
            }
        });
        match participant {
            Some(participant) => {
                self.participants.note_resolution(&participant, &doc);
                note_participant_outcome(&next);
            }
            None => self.pending_local = Some((local_version(&doc), next)),
        }
        self.lifecycle.dirty = true;
        // One that answers its caller does so with the blocks its edit
        // changed, once the door applied it.
        if answers_later(effect) {
            return Some(cx.when_landed(|_, landed, _| {
                Ok(ui::framework::Effected::value(
                    landed.answer.unwrap_or_default(),
                ))
            }));
        }
        Some(Ok(ui::framework::Effected::done()))
    }

    /// Bring the kept state up to date with `state`. Returns whether it
    /// changed.
    fn refresh_live(&self, state: &AutoCommit) -> bool {
        self.source.as_ref().is_some_and(|live| {
            live.sync(state).unwrap_or_else(|error| {
                eprintln!("word-processor: document unreadable: {error}");
                false
            })
        })
    }

    /// Read the kept state, once there is one.
    fn with_live<T>(&self, read: impl FnOnce(&tilo_document::RichTextState) -> T) -> Option<T> {
        self.source
            .as_ref()
            .and_then(|live| live.with(|document| document.text().map(read)))
    }

    /// Show the paragraphs `demand` names.
    fn demand_live(&self, state: &AutoCommit, demand: ui::tilo_resource::Demand) {
        if let Some(live) = &self.source
            && let Err(error) = live.demand(state, demand)
        {
            eprintln!("word-processor: paragraphs unreadable: {error}");
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
    pub(crate) fn follow_personal_dictionary(
        &mut self,
        personal: ui::framework::Setting<proofing::PersonalDictionary>,
    ) {
        self.proofing.personal = Some(personal);
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

    // ---- Tilo action dispatch ------------------------------------------------

    /// Dispatch one [`RichTextAction`](crate::actions::RichTextAction) —
    /// the single CRDT-mutation path shared with the Ctrl+K palette — through the
    /// Tilo action executor. Document heads before the event's first action are
    /// stashed so the host records **one** undo boundary bracketing the whole event.
    fn perform_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) {
        let _ = self.try_perform_action(state, action);
    }

    fn perform_edits(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) {
        let _ = self.try_perform_edits(state, action);
    }

    /// Submit one document action. `false` when the host refuses it, so a
    /// caller can leave the document and caret as they were.
    fn try_perform_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) -> bool {
        self.try_perform_edits(state, action)
    }

    fn try_perform_edits(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &crate::actions::RichTextAction,
    ) -> bool {
        let done = self.try_perform_document(
            state,
            &crate::model::action(action),
            tilo_actions::ActionContext::for_app("org.tilo.Writer", "Writer"),
        );
        #[cfg(test)]
        if done {
            self.last_dispatched = Some(action.clone());
        }
        done
    }

    /// Take the document verb `operation` now, as this window's person.
    fn perform_verb(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        operation: crate::model::DocumentAction,
    ) -> bool {
        self.perform_verb_as(state, operation, None)
    }

    /// Take the document verb `operation` now, as `author` when one is
    /// named (a scenario staging a collaborator's comment), else as this
    /// window's person.
    fn perform_verb_as(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        operation: crate::model::DocumentAction,
        author: Option<&str>,
    ) -> bool {
        let mut context = tilo_actions::ActionContext::for_app("org.tilo.Writer", "Writer");
        if let Some(author) = author {
            context = context.with_invoker(tilo_actions::ActionInvoker::actor(author));
        }
        let action = ui::tilo_resource::Stamped {
            operation,
            now_ms: ui::tilo_resource::origin_now_ms(),
        };
        self.try_perform_document(state, &action, context)
    }

    /// Submit one action of the document now. `false` when the host
    /// refuses it, so a caller can leave the document and caret as they
    /// were.
    fn try_perform_document(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        action: &<crate::model::Document as ui::tilo_resource::Resource>::Action,
        context: tilo_actions::ActionContext,
    ) -> bool {
        let Some(dispatch) = self.dispatch.clone() else {
            return false;
        };
        match dispatch.apply_now_with(state, action, context) {
            Ok(()) => true,
            Err(error) => {
                if self.read_only() {
                    self.toast = Some((t("toast-view-only"), ToastKind::Error));
                    self.lifecycle.dirty = true;
                }
                eprintln!("word-processor: edit refused: {error}");
                false
            }
        }
    }

    /// Persist only the changed fields of named styles through stable typed
    /// actions. This avoids replaying a stale whole-sheet snapshot over a
    /// collaborator's unrelated style edit and gives custom-style deletion an
    /// explicit tombstone plus absolute instance fallback set.
    fn persist_stylesheet_via_actions(&mut self, state: &mut ui::widget::Edits<'_>) {
        let before = self.inner.stylesheet(state);
        let after = self.sheet.clone();
        for (style, properties) in &after.styles {
            let old = before.styles.get(style);
            for (key, value) in properties.to_attrs() {
                if old.is_some_and(|entry| entry.to_attrs().get(&key) == Some(&value)) {
                    continue;
                }
                let value = match value {
                    tilo_doc::AttrVal::Bool(value) => crate::actions::AttrVal::Bool(value),
                    tilo_doc::AttrVal::Int(value) => crate::actions::AttrVal::Int(value),
                    tilo_doc::AttrVal::Float(value) => crate::actions::AttrVal::Float(value),
                    tilo_doc::AttrVal::Str(value) => crate::actions::AttrVal::Str(value),
                };
                self.perform_action(
                    state,
                    &crate::actions::set_style_property_action(style, &key, value),
                );
            }
        }
        let removed: Vec<String> = before
            .styles
            .keys()
            .filter(|style| !after.styles.contains_key(*style))
            .cloned()
            .collect();
        if !removed.is_empty()
            && let Ok(containers) = crate::model::open(state)
        {
            let snapshot = tilo_doc::model::materialize(state, &containers);
            for style in removed {
                let instances = snapshot
                    .blocks
                    .iter()
                    .filter(|block| {
                        matches!(block.attrs.get("style"), Some(AttrVal::Str(id)) if id == &style)
                    })
                    .map(|block| block.id.clone())
                    .collect();
                self.perform_action(
                    state,
                    &crate::actions::delete_style_action(&style, instances),
                );
            }
        }
    }

    /// Replace one exact, already-resolved text range through the canonical
    /// action executor.  Find/replace, autocorrection, and ordinary typing all
    /// use this rather than opening a second CRDT write path in the widget.
    fn splice_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        region: crate::actions::RegionKind,
        block: &str,
        start: usize,
        end: usize,
        text: &str,
    ) {
        self.perform_action(
            state,
            &crate::actions::splice_text_action(region, block, start, end, text),
        );
    }

    /// This window's view for an edit: [`Self::local_participant_view`]
    /// without the Find hits, which no editing primitive reads (keeps a
    /// keystroke from cloning them).
    fn local_edit_view(&self) -> crate::actions::participant::ParticipantView {
        use crate::actions::participant::{ParticipantView, ViewPoint};
        let caret = self.inner.caret();
        ParticipantView {
            caret: ViewPoint::new(caret.block.clone(), caret.offset),
            anchor: self
                .inner
                .selection_anchor
                .as_ref()
                .map(|anchor| ViewPoint::new(anchor.block.clone(), anchor.offset)),
            region: self.inner.region_kind(),
            find: crate::actions::participant::FindState::default(),
            pending: None,
        }
    }

    /// Run one editor input through its pure `editing::*` primitive — the
    /// function Ctrl+K and agents invoke as a published operation — against
    /// this window's view, dispatch its envelopes and adopt its next view.
    ///
    /// Returns `false` only while the primitive has not landed
    /// (`NotYetImplemented`), so the caller runs its equivalent direct path;
    /// the keyboard↔action parity tests pin the two byte-identical. Any other
    /// refusal (no caret, needs a selection, stale arguments) is a no-op, as
    /// it is for the direct path.
    fn edit_via_primitive(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        primitive: impl FnOnce(
            &crate::actions::participant::ParticipantView,
            &AutoCommit,
        ) -> crate::actions::editing::EditResult,
    ) -> bool {
        self.edit_with(state, primitive)
    }

    fn edit_with(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        primitive: impl FnOnce(
            &crate::actions::participant::ParticipantView,
            &AutoCommit,
        ) -> crate::actions::editing::EditResult,
    ) -> bool {
        let view = self.local_edit_view();
        match primitive(&view, state) {
            Err(crate::actions::editing::EditingError::NotYetImplemented(_)) => false,
            Err(_) => true,
            Ok((envelopes, next)) => {
                for action in &envelopes {
                    self.perform_edits(state, action);
                }
                // A caret waiting on a block the edit created settles on the
                // kept order, brought up to date, not on a listed body.
                let next = if next.pending.is_some() {
                    self.sync_source(state);
                    let body = self.kept_body();
                    next.settle_in(state, body.as_deref())
                } else {
                    next
                };
                self.adopt_local_view(&next, state);
                true
            }
        }
    }

    /// Bring the kept state up to date with `state` in the middle of an edit;
    /// the next refresh still copies what changed into the views.
    fn sync_source(&mut self, state: &AutoCommit) {
        self.source_unseen |= self.refresh_live(state);
    }

    /// The kept body order, once there is kept state.
    fn kept_body(&self) -> Option<std::rc::Rc<tilo_document::BodyOrder>> {
        self.with_live(|s| std::rc::Rc::clone(s.body()))
    }

    /// Make `next` (an editing primitive's result, settled against the
    /// document its envelopes produced) this window's caret and selection.
    /// The Find bar keeps its own state.
    fn adopt_local_view(
        &mut self,
        next: &crate::actions::participant::ParticipantView,
        state: &AutoCommit,
    ) {
        let next = next.settle(state);
        if next.caret.block.is_empty() {
            return;
        }
        self.inner.set_caret(Caret {
            block: next.caret.block,
            offset: next.caret.offset,
        });
        self.inner.selection_anchor = next.anchor.map(|anchor| Caret {
            block: anchor.block,
            offset: anchor.offset,
        });
    }

    /// Apply a published view operation (`move-caret`, `select-*`,
    /// `find-text`) to this window: the same pure function a participant's
    /// invocation runs, adopted as the local caret/selection. `find-text`
    /// opens the Find bar on the query, as typing it there would.
    pub(crate) fn apply_local_view_operation(
        &mut self,
        state: &AutoCommit,
        movement: &crate::effects::ViewMove,
    ) -> Result<(), String> {
        let operation = match movement {
            crate::effects::ViewMove::Operation(operation) => operation,
            crate::effects::ViewMove::NavigateTo { block, start, end } => {
                self.navigate_to_range(state, block, *start, *end)?;
                self.resolve_bound_edits(state);
                return Ok(());
            }
        };
        if let crate::actions::view_ops::ViewOperation::FindText { query } = operation {
            if query.len() > crate::actions::editing::MAX_FIND_QUERY_BYTES {
                return Err("the Find query is too long".into());
            }
            let blocks = self.search_blocks(state);
            let effect = self.find.open(Some(query), &mut Blocks(&blocks));
            self.apply_find_effect(state, effect);
            return Ok(());
        }
        let view = self.local_edit_view();
        let (_, next) = crate::actions::view_ops::apply(&view, state, operation)
            .map_err(|error| error.to_string())?;
        self.adopt_local_view(&next, state);
        self.resolve_bound_edits(state);
        Ok(())
    }

    /// Move `participant`'s own view (its caret, selection or Find) as
    /// `movement` says. Nothing is written into the document.
    ///
    /// # Errors
    /// Returns the view primitive's refusal (stale range, no caret, …).
    pub(crate) fn move_participant_view(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        movement: &crate::effects::ViewMove,
        doc: &AutoCommit,
    ) -> Result<(), String> {
        let participant = participant.clone();
        let view = self
            .participant_view_or_default(&participant, doc)
            .ok_or("the document has no body to place a caret in")?;
        let outcome = match movement {
            crate::effects::ViewMove::Operation(operation) => {
                crate::actions::view_ops::apply(&view, doc, operation)
            }
            crate::effects::ViewMove::NavigateTo { block, start, end } => {
                crate::actions::view_ops::navigate_to(&view, doc, block, *start, *end)
            }
        };
        let (_, next) = outcome.map_err(|error| error.to_string())?;
        self.set_participant_view(&participant, &next, doc);
        Ok(())
    }

    /// Select the whole active region (Ctrl+A, context menu).
    fn select_all_via_primitive(&mut self, state: &mut ui::widget::Edits<'_>) {
        if !self.edit_via_primitive(state, crate::actions::editing::select_all) {
            self.inner.select_all(state);
        }
    }

    /// Insert ordinary typed text through the same serializable actions exposed
    /// to agents and Ctrl+K.  A multi-block selection is a structural merge,
    /// represented by its own single envelope rather than a hidden UI-only
    /// sequence of splice/delete writes.
    fn insert_text_via_action(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        self.insert_text_edits(state, text);
    }

    fn insert_text_edits(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        if self.edit_with(state, |view, doc| {
            crate::actions::editing::type_text(view, doc, text)
        }) {
            return;
        }
        if let Some((start, end)) = self.inner.selection_span(state) {
            let region = self.inner.region_kind();
            self.perform_edits(
                state,
                &crate::actions::replace_selection_action(
                    region,
                    &start.block,
                    start.offset,
                    &end.block,
                    end.offset,
                    text,
                ),
            );
            self.inner.set_caret(start);
            self.inner.clear_anchor();
            return;
        }
        let caret = self.inner.caret().clone();
        let region = self.inner.region_kind();
        self.perform_edits(
            state,
            &crate::actions::splice_text_action(
                region,
                &caret.block,
                caret.offset,
                caret.offset,
                text,
            ),
        );
        self.inner.set_caret(Caret {
            block: caret.block,
            offset: caret.offset.saturating_add(text.chars().count()),
        });
    }

    /// Enter is a structural document edit, so split the addressed block through
    /// the same typed action stream as typing and formatting. The newly-created
    /// stable id is discovered from the synchronous before/after set solely to
    /// update the local caret; it is minted by the action's pinned actor.
    fn enter_via_action(&mut self, state: &mut ui::widget::Edits<'_>) {
        // The paragraph that follows the caret is read off the kept order.
        self.sync_source(state);
        let body = self.kept_body();
        if self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::new_paragraph_in(view, doc, body.as_deref())
        }) {
            return;
        }
        if self.inner.has_selection() {
            self.insert_text_via_action(state, "");
        }
        let caret = self.inner.caret().clone();
        let region = self.inner.region_kind();
        let block = self.blocks.iter().find(|entry| entry.id == caret.block);
        let kind = block.map_or("paragraph", |entry| entry.kind.as_str());
        if kind == "list_item" && block.is_some_and(|entry| entry.text.is_empty()) {
            self.perform_action(
                state,
                &crate::actions::set_paragraph_style_action(
                    region,
                    vec![caret.block],
                    "paragraph",
                    None,
                    None,
                ),
            );
            return;
        }
        let ordered = block.and_then(|entry| match entry.attrs.get("ordered") {
            Some(AttrVal::Bool(value)) if kind == "list_item" => Some(*value),
            _ => None,
        });
        let new_kind = if kind == "list_item" {
            "list_item"
        } else {
            "paragraph"
        };
        // The new paragraph follows the split one: in the body, read it off
        // the kept paragraph order rather than listing the region twice.
        let body = matches!(region, crate::actions::RegionKind::Body);
        let before: std::collections::HashSet<String> = if body {
            std::collections::HashSet::new()
        } else {
            self.inner.ordered_blocks(state).into_iter().collect()
        };
        // The kept order names the paragraph that follows, so the split is
        // placed without the document listing its paragraphs.
        let next = body
            .then(|| {
                self.with_live(|doc| {
                    let order = doc.body();
                    order
                        .position(&caret.block)
                        .and_then(|at| order.id_at(at.saturating_add(1)))
                        .map(str::to_owned)
                })
            })
            .flatten()
            .flatten();
        self.perform_action(
            state,
            &crate::actions::split_block_before_action(
                region,
                &caret.block,
                caret.offset,
                new_kind,
                ordered,
                next,
            ),
        );
        let new_block = if body {
            self.sync_source(state);
            self.with_live(|doc| {
                let order = doc.body();
                order
                    .position(&caret.block)
                    .and_then(|at| order.id_at(at.saturating_add(1)))
                    .map(str::to_owned)
            })
            .flatten()
        } else {
            self.inner
                .ordered_blocks(state)
                .into_iter()
                .find(|id| !before.contains(id))
        };
        if let Some(new_block) = new_block {
            self.inner.set_caret(Caret {
                block: new_block,
                offset: 0,
            });
        }
    }

    fn page_break_via_action(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self.inner.has_selection() {
            self.insert_text_via_action(state, "");
        }
        let caret = self.inner.caret().clone();
        let region = self.inner.region_kind();
        let before: std::collections::HashSet<String> =
            self.inner.ordered_blocks(state).into_iter().collect();
        self.perform_action(
            state,
            &crate::actions::insert_page_break_in_action(region, &caret.block, caret.offset),
        );
        if let Some(new_block) = self
            .inner
            .ordered_blocks(state)
            .into_iter()
            .find(|id| !before.contains(id))
        {
            self.inner.set_caret(Caret {
                block: new_block,
                offset: 0,
            });
        }
        self.inner.clear_anchor();
    }

    fn replace_body_via_action(&mut self, state: &mut ui::widget::Edits<'_>, markdown: &str) {
        self.perform_action(state, &crate::actions::replace_body_action(markdown));
        if let Some(first) = self.inner.ordered_blocks(state).into_iter().next() {
            self.inner.set_caret(Caret {
                block: first,
                offset: 0,
            });
        }
        self.inner.clear_anchor();
    }

    fn insert_page_number_via_action(&mut self, state: &mut ui::widget::Edits<'_>) {
        if self.inner.has_selection() {
            self.insert_text_via_action(state, "");
        }
        let caret = self.inner.caret().clone();
        self.perform_action(
            state,
            &crate::actions::insert_page_number_action(
                self.inner.region_kind(),
                &caret.block,
                caret.offset,
            ),
        );
        self.inner.set_caret(Caret {
            block: caret.block,
            offset: caret.offset.saturating_add(1),
        });
        self.inner.clear_anchor();
    }

    fn paste_fragment_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        blocks: Vec<crate::word_processor::ClipBlock>,
    ) {
        let caret = self.inner.caret().clone();
        let (start, end) = self
            .inner
            .selection_span(state)
            .unwrap_or_else(|| (caret.clone(), caret));
        let start_prefix_empty = start.offset == 0;
        let end_len = self
            .blocks
            .iter()
            .find(|block| block.id == end.block)
            .map_or(end.offset, |block| block.text.chars().count());
        let adopt_first_style = start_prefix_empty && end.offset == end_len;
        let before: std::collections::HashSet<String> =
            self.inner.ordered_blocks(state).into_iter().collect();
        let final_offset = blocks.last().map_or(0, |block| block.text.chars().count());
        let one_block = blocks.len() == 1;
        self.perform_action(
            state,
            &crate::actions::paste_fragment_action(
                self.inner.region_kind(),
                crate::actions::SelectionTarget {
                    start_block: start.block.clone(),
                    start: start.offset,
                    end_block: end.block.clone(),
                    end: end.offset,
                },
                blocks,
                adopt_first_style,
            ),
        );
        let new_last = self
            .inner
            .ordered_blocks(state)
            .into_iter()
            .rfind(|id| !before.contains(id));
        self.inner.set_caret(Caret {
            block: if one_block {
                start.block
            } else {
                new_last.unwrap_or(start.block)
            },
            offset: if one_block {
                start.offset.saturating_add(final_offset)
            } else {
                final_offset
            },
        });
        self.inner.clear_anchor();
    }

    /// Backspace/Delete are expressed as absolute splices or one cross-block
    /// replacement action. Selection/caret motion remains presentation-local.
    /// Characters in the projected block `id`, or 0 when it is not projected.
    fn block_char_len(&self, id: &str) -> usize {
        self.blocks
            .iter()
            .find(|entry| entry.id == id)
            .map_or(0, |entry| entry.text.chars().count())
    }

    fn delete_via_action(&mut self, state: &mut ui::widget::Edits<'_>, backward: bool) {
        let direction = if backward {
            crate::actions::editing::DeleteDirection::Backward
        } else {
            crate::actions::editing::DeleteDirection::Forward
        };
        if self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::delete(view, doc, direction, 1)
        }) {
            return;
        }
        if self.inner.has_selection() {
            self.insert_text_via_action(state, "");
            return;
        }
        let caret = self.inner.caret().clone();
        let region = self.inner.region_kind();
        let blocks = self.inner.ordered_blocks(state);
        let Some(index) = blocks.iter().position(|id| id == &caret.block) else {
            return;
        };
        let len = self.block_char_len(&caret.block);
        if backward
            && caret.offset == 0
            && self
                .blocks
                .iter()
                .find(|entry| entry.id == caret.block)
                .is_some_and(|entry| entry.kind == "list_item")
        {
            self.perform_action(
                state,
                &crate::actions::set_paragraph_style_action(
                    region,
                    vec![caret.block],
                    "paragraph",
                    None,
                    None,
                ),
            );
            return;
        }
        if backward && caret.offset > 0 {
            self.splice_via_action(
                state,
                region,
                &caret.block,
                caret.offset - 1,
                caret.offset,
                "",
            );
            self.inner.set_caret(Caret {
                block: caret.block,
                offset: caret.offset - 1,
            });
        } else if !backward && caret.offset < len {
            self.splice_via_action(
                state,
                region,
                &caret.block,
                caret.offset,
                caret.offset + 1,
                "",
            );
        } else if backward && index > 0 {
            let Some(previous) = blocks.get(index.saturating_sub(1)).cloned() else {
                return;
            };
            let previous_len = self.block_char_len(&previous);
            self.perform_action(
                state,
                &crate::actions::replace_selection_action(
                    region,
                    &previous,
                    previous_len,
                    &caret.block,
                    0,
                    "",
                ),
            );
            self.inner.set_caret(Caret {
                block: previous,
                offset: previous_len,
            });
        } else if !backward && index + 1 < blocks.len() {
            let Some(next) = blocks.get(index.saturating_add(1)) else {
                return;
            };
            self.perform_action(
                state,
                &crate::actions::replace_selection_action(region, &caret.block, len, next, 0, ""),
            );
        }
        self.inner.clear_anchor();
    }

    /// The context Ctrl+K needs to build the document's resource tree: the caret
    /// anchor for structural inserts and the blocks a block-level format targets.
    /// `None` until the document is bound (a fresh, unsynced widget).
    pub(crate) fn wp_resource_ctx(
        &self,
        doc: &AutoCommit,
    ) -> Option<crate::actions::WpResourceCtx> {
        let caret = self.inner.caret();
        if caret.block.is_empty() {
            return None;
        }
        // The caret block is the alignment target for a palette-initiated block
        // format (the palette runs between frames with no live selection).
        let target = if self.blocks.iter().any(|b| b.id == caret.block) {
            vec![caret.block.clone()]
        } else {
            Vec::new()
        };
        Some(crate::actions::WpResourceCtx {
            caret_block: caret.block.clone(),
            caret_offset: caret.offset,
            target_blocks: if target.is_empty() {
                vec![caret.block.clone()]
            } else {
                target
            },
            format_spans: to_spans(
                self.selection_blocks_cached()
                    .into_iter()
                    .filter(|(_, start, end)| start < end)
                    .collect(),
            ),
            selection: self.inner.selection_span(doc).map(|(start, end)| {
                crate::actions::SelectionTarget {
                    start_block: start.block,
                    start: start.offset,
                    end_block: end.block,
                    end: end.offset,
                }
            }),
            region: self.inner.region_kind(),
            search_hits: self
                .find
                .matches()
                .iter()
                .take(crate::actions::MAX_RESOURCE_SEARCH_HITS)
                .map(|(block, start, end)| crate::actions::SearchTarget {
                    block: block.clone(),
                    start: *start,
                    end: *end,
                })
                .collect(),
            content: self.inner.content_store().clone(),
        })
    }

    // ---- participant views ---------------------------------------------------

    /// This window's own view as a participant view: the caret, the
    /// selection anchor, the active region and the Find bar's query and hits.
    /// [`Self::wp_resource_ctx`] is its projection (computed from the render
    /// cache so it stays cheap per frame); `view.ctx(doc)` yields the same
    /// context.
    pub fn local_participant_view(&self) -> crate::actions::participant::ParticipantView {
        use crate::actions::participant::{FindState, ParticipantView, ViewPoint};
        let caret = self.inner.caret();
        ParticipantView {
            caret: ViewPoint::new(caret.block.clone(), caret.offset),
            anchor: self
                .inner
                .selection_anchor
                .as_ref()
                .map(|anchor| ViewPoint::new(anchor.block.clone(), anchor.offset)),
            region: self.inner.region_kind(),
            find: FindState::new(
                self.find.query(),
                self.find.matches().iter().map(|(block, start, end)| {
                    crate::actions::SearchTarget {
                        block: block.clone(),
                        start: *start,
                        end: *end,
                    }
                }),
            ),
            pending: None,
        }
    }

    /// `participant`'s current view, rebased onto `doc`. `None` for a
    /// participant this widget holds no view for.
    pub fn participant_view(
        &self,
        participant: &tilo_actions::ParticipantId,
        doc: &AutoCommit,
    ) -> Option<crate::actions::participant::ParticipantView> {
        self.participants.view(participant, doc)
    }

    /// The view an invocation by `participant` resolves against: its stored
    /// view, or — for a participant without one yet — a collapsed caret at the
    /// end of the body (an agent's spawn position). `None` only for a document
    /// without body blocks.
    pub fn participant_view_or_default(
        &self,
        participant: &tilo_actions::ParticipantId,
        doc: &AutoCommit,
    ) -> Option<crate::actions::participant::ParticipantView> {
        self.participants
            .view(participant, doc)
            .or_else(|| crate::actions::participant::ParticipantView::at_document_end(doc))
    }

    /// Store `participant`'s view, valid against `doc` as it is now (after
    /// its envelopes were applied). Presentation state only: nothing is
    /// written into the document. The map stays bounded (LRU, idle TTL).
    pub fn set_participant_view(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        view: &crate::actions::participant::ParticipantView,
        doc: &AutoCommit,
    ) {
        self.participants
            .set(participant, view, doc, ui::time::now());
        self.resolve_bound_edits(doc);
        self.lifecycle.dirty = true;
    }

    /// The name painted next to `participant`'s caret (e.g. "Agent for Ada").
    pub fn set_participant_label(
        &mut self,
        participant: &tilo_actions::ParticipantId,
        label: &str,
    ) {
        self.participants.set_label(participant, label);
    }

    /// Forget `participant`'s view (its task ended). Returns whether it had one.
    pub fn remove_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        self.participants.remove(participant)
    }

    /// The Find results `participant` sees, as resources: this window's
    /// for `None`, else built from that participant's own view only, so no
    /// other participant's hits leak into it.
    pub fn participant_resources(
        &self,
        participant: Option<&tilo_actions::ParticipantId>,
        doc: &AutoCommit,
    ) -> Vec<tilo_actions::Resource> {
        let ctx = match participant {
            None => self.wp_resource_ctx(doc),
            Some(participant) => self
                .participant_view_or_default(participant, doc)
                .and_then(|view| view.ctx(doc)),
        };
        ctx.map(|ctx| {
            crate::actions::search_rows(&ctx, doc)
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Apply one participant's edit — the envelopes and next view an
    /// `editing::*` primitive returned for that participant's view — and
    /// store the view. Each envelope goes through the canonical executor with
    /// the invoker in its context (durable attribution), on a dispatcher
    /// separate from this window's, so the edit never joins the local user's
    /// undo group.
    ///
    /// # Errors
    /// Refuses an invoker without a participant, and returns the first
    /// executor refusal; envelopes before it stay applied, the view is then
    /// left as it was (rebased).
    pub fn apply_participant_edit(
        &mut self,
        invoker: &tilo_actions::ActionInvoker,
        state: &mut ui::widget::Edits<'_>,
        outcome: crate::actions::editing::EditOutcome,
    ) -> Result<(), String> {
        let Some(participant) = invoker.participant.clone() else {
            return Err("a participant edit needs the invoking participant".into());
        };
        let (envelopes, view) = outcome;
        let Some(dispatch) = self.dispatch.clone() else {
            return Err("this view edits nothing".into());
        };
        for action in &envelopes {
            dispatch
                .apply_now_with(
                    state,
                    &crate::model::action(action),
                    tilo_actions::ActionContext::for_app("org.tilo.Writer", "Writer")
                        .with_invoker(invoker.clone()),
                )
                .map_err(|error| error.to_string())?;
        }
        self.set_participant_view(&participant, &view, state);
        Ok(())
    }

    /// Every other participant's caret or selection as collaboration
    /// presence, under the same `wp_block` key and byte offsets the local
    /// window publishes, so peers render an agent exactly like a person.
    /// The host publishes each entry under that participant's identity.
    pub fn participant_presence(&self, doc: &AutoCommit) -> Vec<ParticipantPresence> {
        let mut out = Vec::new();
        for participant in self.participants.participants() {
            let Some(view) = self.participants.view(participant, doc) else {
                continue;
            };
            let Some(payload) = participant_presence_payload(&view, doc) else {
                continue;
            };
            out.push(ParticipantPresence {
                participant: participant.clone(),
                label: self.participants.label(participant).map(str::to_string),
                key: block_presence_key(&view.caret.block),
                payload,
            });
        }
        out.sort_by(|a, b| a.participant.as_str().cmp(b.participant.as_str()));
        out
    }

    // ---- formatting affordances → actions -----------------------------------

    /// Toggle an inline mark (bold/italic/underline/strike) over the selection,
    /// as one replayable action. A toggle with no selection is a no-op.
    fn dispatch_toggle_mark(&mut self, state: &mut ui::widget::Edits<'_>, name: &str) {
        if let Some(mark) = inline_mark(name)
            && self.edit_via_primitive(state, |view, doc| {
                crate::actions::editing::toggle_mark(view, doc, mark)
            })
        {
            return;
        }
        let Some((spans, active)) = self.inner.plan_toggle_mark(state, name) else {
            return;
        };
        let region = self.inner.region_kind();
        let value = if active {
            None
        } else {
            Some(crate::actions::MarkVal::Bool(true))
        };
        let action = crate::actions::set_mark_action(region, to_spans(spans), name, value, vec![]);
        self.perform_action(state, &action);
    }

    /// Toggle a mutually-exclusive mark (super/sub) over the selection / caret
    /// block, clearing its sibling, as one replayable action.
    fn dispatch_exclusive_mark(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        sibling: &str,
    ) {
        if let Some(mark) = inline_mark(name)
            && self.edit_via_primitive(state, |view, doc| {
                crate::actions::editing::toggle_mark(view, doc, mark)
            })
        {
            return;
        }
        let (spans, active) = self.inner.plan_exclusive_mark(state, name);
        if spans.iter().all(|(_, s, e)| s >= e) {
            return;
        }
        let region = self.inner.region_kind();
        let value = if active {
            None
        } else {
            Some(crate::actions::MarkVal::Bool(true))
        };
        // When turning on, also clear the sibling over the same spans.
        let also_clear = if active {
            vec![]
        } else {
            vec![sibling.to_string()]
        };
        let action =
            crate::actions::set_mark_action(region, to_spans(spans), name, value, also_clear);
        self.perform_action(state, &action);
    }

    /// Set a valued inline mark (font/size/colour/highlight) over the selection
    /// or caret block, as one replayable action (replacing any prior value).
    fn dispatch_set_mark_value(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        name: &str,
        value: crate::actions::MarkVal,
    ) {
        let primitive_value = value.clone();
        if self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::set_mark(view, doc, name, primitive_value)
        }) {
            return;
        }
        let spans = self.inner.format_spans(state);
        if spans.iter().all(|(_, s, e)| s >= e) {
            return;
        }
        let region = self.inner.region_kind();
        let action =
            crate::actions::set_mark_action(region, to_spans(spans), name, Some(value), vec![]);
        self.perform_action(state, &action);
    }

    /// Clear a valued inline mark over the selection or caret block.
    fn dispatch_clear_mark(&mut self, state: &mut ui::widget::Edits<'_>, name: &str) {
        if self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::clear_mark(view, doc, name)
        }) {
            return;
        }
        let spans = self.inner.format_spans(state);
        if spans.iter().all(|(_, s, e)| s >= e) {
            return;
        }
        let region = self.inner.region_kind();
        let action = crate::actions::set_mark_action(region, to_spans(spans), name, None, vec![]);
        self.perform_action(state, &action);
    }

    /// Set the paragraph kind/level over the selection, as one replayable action.
    fn dispatch_paragraph_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        kind: &str,
        level: Option<i64>,
        ordered: Option<bool>,
    ) {
        let style = crate::actions::editing::ParagraphStyle {
            kind: kind.to_string(),
            level,
            ordered,
        };
        if self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::set_paragraph_style(view, doc, &style)
        }) {
            return;
        }
        let blocks = self.inner.paragraph_style_targets(state);
        if blocks.is_empty() {
            return;
        }
        let action = crate::actions::set_paragraph_style_action(
            self.inner.region_kind(),
            blocks,
            kind,
            level,
            ordered,
        );
        self.perform_action(state, &action);
    }

    /// Apply a named paragraph style (a style-picker pick) over the current
    /// paragraph targets as one replayable action: the style attr plus, for a
    /// built-in, the structural kind / level / ordered.
    fn dispatch_apply_named_style(&mut self, state: &mut ui::widget::Edits<'_>, id: &str) {
        if self.edit_via_primitive(state, |view, doc| {
            crate::actions::editing::apply_named_style(view, doc, id)
        }) {
            return;
        }
        let blocks = self.inner.paragraph_style_targets(state);
        if blocks.is_empty() {
            return;
        }
        let (kind, level, ordered) = match crate::styles::structural_kind_for(id) {
            Some((k, l, o)) => (Some(k), l, o),
            None => (None, None, None),
        };
        let action = crate::actions::apply_named_style_action(
            self.inner.region_kind(),
            blocks,
            id,
            kind,
            level,
            ordered,
        );
        self.perform_action(state, &action);
    }

    /// Set a block attribute (alignment / line-spacing / style) over the current
    /// target blocks, as one replayable action.
    fn dispatch_block_attr(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        key: &str,
        value: crate::actions::AttrVal,
    ) {
        let blocks = self.inner.target_block_ids(state);
        if blocks.is_empty() {
            return;
        }
        let region = self.inner.region_kind();
        let action = crate::actions::set_block_attr_action(region, blocks, key, value);
        self.perform_action(state, &action);
    }

    /// Insert a `rows`×`cols` table after the caret as one replayable action,
    /// then re-seat the caret into the new table's first cell (presentation) —
    /// the cell ids are minted inside `apply`, so they're discovered from the
    /// document afterwards rather than known up front.
    fn insert_table_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        rows: usize,
        cols: usize,
    ) {
        let after = self.inner.caret().block.clone();
        let before: std::collections::HashSet<String> = self.body_cell_ids(state);
        self.perform_action(
            state,
            &crate::actions::insert_table_action(&after, rows, cols),
        );
        // The first cell of the freshly-created table = the new `cell` block at
        // (row 0, col 0). Seat the caret there so typing continues in the table.
        if let Some(c) = self.inner.containers.clone() {
            let snap = tilo_doc::model::materialize(state, &c);
            let first = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "cell" && !before.contains(&b.id))
                .find(|b| {
                    matches!(b.attrs.get("row"), Some(AttrVal::Int(0)))
                        && matches!(b.attrs.get("col"), Some(AttrVal::Int(0)))
                })
                .map(|b| b.id.clone());
            if let Some(fc) = first {
                self.inner.set_caret(Caret {
                    block: fc,
                    offset: 0,
                });
                self.inner.clear_anchor();
            }
        }
    }

    fn caret_table_target(&self) -> Option<(String, usize, usize, usize, usize)> {
        let current = self
            .blocks
            .iter()
            .find(|block| block.id == self.inner.caret.block)?;
        let table = match current.attrs.get("table") {
            Some(AttrVal::Str(table)) => table.clone(),
            _ => return None,
        };
        let coordinate = |block: &BlockView, key: &str| match block.attrs.get(key) {
            Some(AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
            _ => 0,
        };
        let row = coordinate(current, "row");
        let column = coordinate(current, "col");
        let table_blocks: Vec<_> = self
            .blocks
            .iter()
            .filter(|block| {
                block.kind == "cell"
                    && matches!(block.attrs.get("table"), Some(AttrVal::Str(id)) if id == &table)
            })
            .collect();
        let rows = table_blocks
            .iter()
            .map(|block| coordinate(block, "row"))
            .max()?
            .saturating_add(1);
        let columns = table_blocks
            .iter()
            .map(|block| coordinate(block, "col"))
            .max()?
            .saturating_add(1);
        Some((table, row, column, rows, columns))
    }

    fn edit_table_via_action(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        edit: crate::actions::TableEdit,
    ) {
        let Some((table, row, column, rows, columns)) = self.caret_table_target() else {
            return;
        };
        let order = self.inner.ordered_blocks(state);
        let before_table = self
            .blocks
            .iter()
            .filter(|block| {
                block.kind == "cell"
                    && matches!(block.attrs.get("table"), Some(AttrVal::Str(id)) if id == &table)
            })
            .filter_map(|block| order.iter().position(|id| id == &block.id))
            .min()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| order.get(index).cloned());
        self.perform_action(state, &crate::actions::edit_table_action(&table, edit));
        self.refresh_blocks(state);
        let desired = match edit {
            crate::actions::TableEdit::InsertRow { at } => Some((at, 0)),
            crate::actions::TableEdit::InsertColumn { at } => Some((row, at)),
            crate::actions::TableEdit::DeleteRow { row: removed } => {
                (rows > 1).then_some((removed.min(rows.saturating_sub(2)), column))
            }
            crate::actions::TableEdit::DeleteColumn { column: removed } => {
                (columns > 1).then_some((row, removed.min(columns.saturating_sub(2))))
            }
            crate::actions::TableEdit::Delete => None,
        };
        let next = desired.and_then(|(target_row, target_column)| {
            self.blocks
                .iter()
                .find(|block| {
                    matches!(block.attrs.get("table"), Some(AttrVal::Str(id)) if id == &table)
                        && matches!(block.attrs.get("row"), Some(AttrVal::Int(value)) if *value == num::i64_of_usize(target_row))
                        && matches!(block.attrs.get("col"), Some(AttrVal::Int(value)) if *value == num::i64_of_usize(target_column))
                })
                .map(|block| block.id.clone())
        });
        if let Some(block) = next
            .or(before_table)
            .or_else(|| self.body_order.id_at(0).map(str::to_owned))
        {
            self.inner.set_caret(Caret { block, offset: 0 });
        }
        self.inner.clear_anchor();
    }

    /// The ids of every `cell` block currently in the body.
    fn body_cell_ids(&self, state: &AutoCommit) -> std::collections::HashSet<String> {
        match self.inner.containers.clone() {
            Some(c) => tilo_doc::model::materialize(state, &c)
                .blocks
                .into_iter()
                .filter(|b| b.kind == "cell")
                .map(|b| b.id)
                .collect(),
            None => std::collections::HashSet::new(),
        }
    }

    /// Lazily build (once) the widget's owned spell-check dictionary: a clone of
    /// the process-wide shared built-in list (parsed at most once per process)
    /// with any persisted user words layered in. Owned so "Add to dictionary"
    /// can mutate it.
    fn dictionary(&mut self) -> &mut spellcheck::Dictionary {
        // Explicit words added through the shared local host follow every
        // window: build the spelling overlay again when they change.
        if let Some(personal) = &self.proofing.personal {
            let snapshot = ui::personal_dictionary::snapshot();
            if !self.proofing.migration_requested
                && let Ok(snapshot) = &snapshot
            {
                let words = personal.get().dictionary_words;
                if words.is_empty()
                    || ui::personal_dictionary::edit(
                        snapshot.revision,
                        ui::personal_dictionary::Edit::ImportWriter { words },
                    )
                    .is_ok()
                {
                    self.proofing.migration_requested = true;
                }
            }
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

    /// The language words are checked in.
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

    /// Edit the page margins on the page, as choosing Margins in the status
    /// band does.
    pub fn set_margins_mode(&mut self) {
        self.toggle_layout_mode(LayoutMode::Margins);
    }

    /// Show or hide the document outline.
    pub fn set_outline_open(&mut self, open: bool) {
        if self.outline.is_open() != open {
            self.outline.toggle();
        }
        self.lifecycle.dirty = true;
    }

    /// Stage a representative non-destructive error toast for visual/a11y
    /// regression shots. Runtime failures use the same toast surface.
    pub fn stage_error_for_shot(&mut self) {
        self.save_error = Some(Rect::ZERO);
        self.lifecycle.dirty = true;
    }

    /// Stage the template picker for the next headless overlay pass.
    pub fn stage_template_picker_for_shot(&mut self) {
        self.staged.template_picker = true;
    }

    /// Stage the compact-toolbar overflow menu for the next headless overlay
    /// pass.
    pub fn stage_toolbar_overflow_for_shot(&mut self) {
        self.staged.toolbar_overflow = true;
    }

    /// Put the production Find bar into a deterministic populated state for
    /// visual review. The query still runs through the complete-document search
    /// path, so this catches virtualized/offscreen result regressions rather
    /// than painting a mock search surface.
    pub fn stage_find_for_shot(&mut self, state: &AutoCommit, query: &str) {
        let blocks = self.search_blocks(state);
        let effect = self.find.open(Some(query), &mut Blocks(&blocks));
        self.apply_find_effect(state, effect);
        self.lifecycle.dirty = true;
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
            "template-picker" => self.stage_template_picker_for_shot(),
            "toolbar-overflow" => self.stage_toolbar_overflow_for_shot(),
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
        self.format = Some(format);
    }

    /// Fetch the bytes of every image the document references that is not fetched yet from the
    /// content store.
    /// Content that is not here stays absent, which is what draws a placeholder.
    fn refresh_images(
        blobs: &mut std::collections::BTreeMap<String, Vec<u8>>,
        image_cache: &mut std::collections::HashMap<String, Option<ui::widget::Image>>,
        content: &tilo_content::ContentStore,
        state: &AutoCommit,
        c: &tilo_doc::Containers,
    ) {
        for hash in tilo_doc::model::blob_hashes(state, c) {
            if blobs.contains_key(&hash) {
                continue;
            }
            if let Some(bytes) = content.read(&hash) {
                // A cached "not decodable yet" result must not outlive the bytes' arrival.
                image_cache.remove(&hash);
                blobs.insert(hash, bytes);
            }
        }
    }

    /// Read an image file and insert it at the caret. Returns whether it inserted
    /// (false if the file couldn't be read or kept). The bytes go in the app's
    /// content store and the document references them; the renderer/exporter
    /// decode them on demand.
    fn insert_image_from_path(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
    ) -> bool {
        self.insert_image_edits(state, path)
    }

    fn insert_image_edits(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
    ) -> bool {
        let file = tilo_files::file_at(path);
        match file
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|(dir, name)| tilo_files::blocking::read(dir, name))
        {
            Ok(bytes) => {
                let hash = match self.inner.content_store().put(&bytes) {
                    Ok(hash) => hash,
                    Err(error) => {
                        eprintln!(
                            "word-processor: could not keep image {}: {error}",
                            path.display()
                        );
                        return false;
                    }
                };
                if self.inner.has_selection() {
                    self.insert_text_edits(state, "");
                }
                let caret = self.inner.caret().clone();
                let before: std::collections::HashSet<String> =
                    self.inner.ordered_blocks(state).into_iter().collect();
                let mut image = crate::actions::ImageInput::of(hash, &bytes);
                // Prefer the decoder's size: it reads formats the header sniffer does not.
                if let Ok(decoded) = ui::widget::Image::from_encoded(&bytes) {
                    let (width, height) = decoded.intrinsic_size();
                    image.width = Some(i64::from(width));
                    image.height = Some(i64::from(height));
                }
                self.perform_edits(
                    state,
                    &crate::actions::insert_image_action(&caret.block, caret.offset, image),
                );
                if let Some(after) = self
                    .inner
                    .ordered_blocks(state)
                    .into_iter()
                    .rfind(|id| !before.contains(id))
                {
                    self.inner.set_caret(Caret {
                        block: after,
                        offset: 0,
                    });
                    self.inner.clear_anchor();
                }
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
                eprintln!(
                    "word-processor: inserted image from {} ({} bytes)",
                    path.display(),
                    bytes.len()
                );
                // A drop arrives as a file the toolkit spooled into our
                // runtime dir; once the bytes live in the document the
                // spool copy has no owner.
                if is_dnd_spool_path(path)
                    && let Ok((dir, name)) = &file
                {
                    let _ = tilo_files::blocking::remove(dir, name);
                }
                true
            }
            Err(e) => {
                eprintln!(
                    "word-processor: could not read image {}: {e}",
                    path.display()
                );
                false
            }
        }
    }

    /// Insert the image the host's chooser answered with, if it answered.
    /// A cancel or a chooser failure leaves the document as it was; a
    /// chooser gone without answering frees the request.
    fn drain_image_choice(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    portal::PortalOutcome::Opened(path) => {
                        self.insert_image_edits(state, &path);
                    }
                    portal::PortalOutcome::Failed(reason) => {
                        eprintln!("word-processor: portal error: {reason}");
                        self.toast = Some((t("toast-file-dialog-failed"), ToastKind::Error));
                    }
                    portal::PortalOutcome::Cancelled => {}
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.portal_rx = None,
        }
    }

    /// Toolbar "Insert image" affordance. Opens the host **XDG file-chooser
    /// portal** (per the Tilo HIG the app never draws its own picker): the D-Bus
    /// round-trip + host dialog run on a worker thread ([`portal::PickRequest`]);
    /// the chosen path comes back over a channel drained each frame in
    /// [`Self::drain_portal`], which inserts the image at the caret. A second
    /// request while one is already in flight is ignored.
    fn request_image_insert(&mut self, _state: &mut ui::widget::Edits<'_>) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(
            portal::PickRequest::open("Insert image", "Insert")
                .filter(
                    portal::FileFilter::new("Images")
                        .glob("*.png")
                        .glob("*.jpg")
                        .glob("*.jpeg")
                        .glob("*.gif")
                        .glob("*.webp")
                        .glob("*.bmp")
                        .glob("*.svg"),
                )
                .thread_name("wp-portal-image")
                .send(),
        );
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
        if self.inner.containers.is_some() {
            // The file keeps the comment threads in its text, as file
            // formats do.
            let file = crate::model::export_copy(state)?;
            let c = crate::model::open(&file)?;
            let dr = tilo_doc::DocRef { doc: &file, c: &c };
            match format.export(dr, self.inner.content_store()) {
                Ok(bytes) => {
                    tilo_files::native::atomic::write_preserving_target(path, &bytes).map_err(
                        |e| {
                            eprintln!("word-processor: could not write {}: {e}", path.display());
                            self.save_error = Some(Rect::ZERO);
                            format!("could not write {}: {e}", path.display())
                        },
                    )?;
                    self.save_error = None;
                    self.toast = Some((t("toast-saved"), ToastKind::Success));
                    self.note_file_written(path, &bytes);
                }
                Err(e) => {
                    eprintln!("word-processor: export failed: {e}");
                    self.save_error = Some(Rect::ZERO);
                    return Err(format!("export failed: {e}"));
                }
            }
        } else {
            return Err("document has no writable path or initialized body".into());
        }
        if !matches!(format, WpFormat::Pdf | WpFormat::Epub) {
            self.set_file(path.to_path_buf(), format);
        }
        Ok(())
    }

    // ---- Find & Replace -----------------------------------------------------

    fn open_find(&mut self, state: &AutoCommit) {
        // Seed the query from a single-line selection, if any.
        let seed = if let Some((bid, s, e)) = self.selection_blocks_cached().first().cloned()
            && let Some(b) = self.blocks.iter().find(|b| b.id == bid)
        {
            let sel: String = b.text.chars().skip(s).take(e.saturating_sub(s)).collect();
            (!sel.is_empty() && !sel.contains('\n')).then_some(sel)
        } else {
            None
        };
        let blocks = self.search_blocks(state);
        let effect = self.find.open(seed.as_deref(), &mut Blocks(&blocks));
        self.apply_find_effect(state, effect);
    }

    fn close_find(&mut self) {
        self.find.close();
    }

    // ---- Zoom ---------------------------------------------------------------

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

    /// Select (so it highlights) + scroll to the active match.
    fn select_current_match(&mut self, _state: &AutoCommit) {
        if let Some((bid, s, e)) = self.find.active_match().cloned() {
            self.inner.clear_anchor();
            self.inner.set_caret(Caret {
                block: bid.clone(),
                offset: s,
            });
            self.inner.set_anchor();
            self.inner.set_caret(Caret {
                block: bid.clone(),
                offset: e,
            });
            self.scroll_to_block(&bid);
        }
    }

    /// Navigate a resource-addressed body range after checking it still exists
    /// in the canonical document. This is intentionally separate from Find's
    /// index because global search/agents may invoke a result after rendering
    /// has virtualized or reordered its local cache.
    fn navigate_to_range(
        &mut self,
        state: &AutoCommit,
        block: &str,
        start: usize,
        end: usize,
    ) -> Result<(), String> {
        let Some(containers) = self.inner.containers.as_ref() else {
            return Err("Writer document is not initialized".into());
        };
        let Some(text) = tilo_doc::model::block_text(state, containers, block) else {
            return Err("the requested document range no longer exists".into());
        };
        let len = text.chars().count();
        if start > end || end > len {
            return Err("the requested document range is stale".into());
        }
        if self.inner.in_running_region() {
            return Err("leave header or footer editing before navigating body results".into());
        }
        self.inner.clear_anchor();
        self.inner.set_caret(Caret {
            block: block.to_string(),
            offset: start,
        });
        if start < end {
            self.inner.set_anchor();
            self.inner.set_caret(Caret {
                block: block.to_string(),
                offset: end,
            });
        }
        self.scroll_to_block(block);
        self.lifecycle.dirty = true;
        Ok(())
    }

    fn scroll_to_block(&mut self, bid: &str) {
        if let Some(l) = self.laid.iter().find(|l| l.id == bid) {
            // l.y_top is screen-space (post-scroll); recover content space.
            let content_y = l.y_top + self.scroll_y;
            self.scroll_y = (content_y - 180.0).clamp(0.0, self.max_scroll);
        } else if let Some(index) = self.body_order.position(bid) {
            // Find can target an offscreen block that is intentionally absent
            // from the bounded render cache. Its stable canonical position gives
            // us a cheap initial scroll estimate; the next sync loads/shapes the
            // local window and refines the visual position.
            let content_y = self.zoom.anchor().y
                + PAGE_TOP
                + flow_to_page_stack(
                    num::f64_of_usize(index) * ESTIMATED_BLOCK_EXTENT,
                    MAX_PAGE_W * LETTER_RATIO,
                    self.margins,
                );
            self.scroll_y = (content_y - 180.0).clamp(0.0, self.max_scroll);
        }
    }

    fn apply_find_effect(&mut self, state: &AutoCommit, effect: FindEffect) {
        if effect == FindEffect::SelectActive {
            self.select_current_match(state);
        }
    }

    /// Carry what the person did in the Find bar to the document: search
    /// again over the whole document, step to a match and select it, or
    /// replace through the editor's own actions.
    fn drive_find(&mut self, state: &mut ui::widget::Edits<'_>) {
        let mut find = std::mem::take(&mut self.find);
        let effect = find.drive(&mut EditorFind { wp: self, state });
        self.find = find;
        self.apply_find_effect(state, effect);
    }

    #[cfg(test)]
    fn replace_all_matches(&mut self, state: &mut ui::widget::Edits<'_>, with: &str) -> usize {
        let matches = self.find.matches().to_vec();
        let replaced = EditorFind { wp: self, state }.replace_matches(&matches, with);
        let mut find = std::mem::take(&mut self.find);
        let effect = find.refresh(&mut EditorFind { wp: self, state });
        self.find = find;
        self.apply_find_effect(state, effect);
        replaced
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
    /// moves a large document's bounded working set. qdoc's requested-block API
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

    /// Complete document search is intentionally separate from rendering's
    /// bounded cache. It is built transiently for a Find operation, then dropped,
    /// so search/resource identity sees every block without pinning offscreen
    /// rich views in the frame cache.
    fn search_blocks(&self, state: &AutoCommit) -> Vec<BlockView> {
        self.inner
            .containers
            .as_ref()
            .map_or_else(Vec::new, |c| tilo_doc::model::materialize(state, c).blocks)
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

/// Screen axis of a margin/border guide for `side`: `(is_vertical, coordinate)`.
fn guide_axis(page: Rect, e: Edges, side: Side) -> (bool, f64) {
    match side {
        Side::Left => (true, page.x0 + e.left),
        Side::Right => (true, page.x1 - e.right),
        Side::Top => (false, page.y0 + e.top),
        Side::Bottom => (false, page.y1 - e.bottom),
    }
}

/// New inset for `side` from a pointer position, clamped to a sane range so a
/// margin/border can never invert or swallow the whole page.
fn inset_from_pointer(page: Rect, side: Side, p: Point) -> f64 {
    let raw = match side {
        Side::Left => p.x - page.x0,
        Side::Right => page.x1 - p.x,
        Side::Top => p.y - page.y0,
        Side::Bottom => page.y1 - p.y,
    };
    let max = match side {
        Side::Left | Side::Right => (page.width() / 2.0 - 48.0).max(8.0),
        Side::Top | Side::Bottom => (page.height() / 2.0 - 48.0).max(8.0),
    };
    raw.clamp(4.0, max)
}

/// Apply a guide drag to `e`: Pair = grabbed side + its opposite, Single = just
/// the grabbed side, Proportional = scale all four by the same factor.
fn apply_edges(e: &mut Edges, page: Rect, side: Side, p: Point, kind: DragKind) {
    let v = inset_from_pointer(page, side, p);
    match kind {
        DragKind::Single => e.set(side, v),
        DragKind::Pair => {
            e.set(side, v);
            e.set(side.opposite(), v);
        }
        DragKind::Proportional => {
            let f = v / e.get(side).max(1.0);
            let max_h = (page.width() / 2.0 - 48.0).max(8.0);
            let max_v = (page.height() / 2.0 - 48.0).max(8.0);
            e.left = (e.left * f).clamp(4.0, max_h);
            e.right = (e.right * f).clamp(4.0, max_h);
            e.top = (e.top * f).clamp(4.0, max_v);
            e.bottom = (e.bottom * f).clamp(4.0, max_v);
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

impl WordProcessorWidget {
    /// The selection highlight painted on paper: the paper accent, translucent
    /// so the text reads through it.
    fn paper_selection(&self) -> Color {
        with_alpha(self.chrome.paper_accent, PAPER_SELECTION_ALPHA)
    }

    /// Toggle a layout-edit mode from a chip click.
    fn toggle_layout_mode(&mut self, mode: LayoutMode) {
        self.layout_mode = if self.layout_mode == mode {
            LayoutMode::None
        } else {
            mode
        };
    }

    /// Persist the current page margins into the document (page setup lives in the
    /// document so it survives save/reopen). Called on margin-guide drag release.
    fn persist_margins(&mut self, state: &mut ui::widget::Edits<'_>) {
        self.perform_action(
            state,
            &crate::actions::set_page_margins_action([
                self.margins.top,
                self.margins.right,
                self.margins.bottom,
                self.margins.left,
            ]),
        );
    }

    /// Draw the page header (or footer) blocks for one page sheet inside the given
    /// margin band, anchored to the band's bottom (footers) or top (headers), drawn
    /// through `ctf` so it scales with the page content.
    ///
    /// When `editing` is set, the band is shaped with *raw* (1 char ⇄ 1 caret offset)
    /// runs so click-to-caret lines up with the model, and per-block laid geometry is
    /// returned for the caller to stash. Returns `(drew_anything, laid)`; `laid` is
    /// empty unless `editing`.
    fn draw_running_band(
        scene: &mut Scene,
        ctf: Affine,
        tr: &mut TextRenderer,
        band: &RunningBand<'_>,
    ) -> (bool, Vec<RegionLaid>) {
        let RunningBand {
            blocks,
            page_no,
            rect: band,
            is_footer,
            editing,
            composition,
        } = *band;
        if blocks.is_empty() || band.height() <= 2.0 {
            return (false, Vec::new());
        }
        let wrap_w = num::f32_of_f64(band.width().max(40.0));
        // Header/footer run text is a touch smaller and muted, so it reads as
        // chrome rather than body copy (LibreOffice-style running heads).
        let size = 12.0f32;
        // Shape every block first so footers can be bottom-anchored.
        let mut shaped: Vec<RichTextLayout> = Vec::with_capacity(blocks.len());
        let mut total_h = 0.0f64;
        for b in blocks {
            // The block being composed in shapes the composition inside its
            // text, underlined.
            let composing = composition
                .filter(|(id, _)| *id == b.id)
                .map(|(_, core)| core);
            let runs = match composing {
                Some(core) if editing => running_head_runs_editing(&composed_block(b, core)),
                _ if editing => running_head_runs_editing(b),
                _ => running_head_runs(b, page_no),
            };
            let underlines = composing
                .and_then(|core| core.preedit_display_range(&b.text))
                .map(|range| vec![(range.start, range.end)])
                .unwrap_or_default();
            let align = block_align(b, TextAlign::Left);
            let layout = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &runs,
                &RichTextLayout::block_style(false, size, FontFamily::Sans, 1.0),
                Some(wrap_w),
                align,
                underlines,
                vec![],
            );
            total_h += f64::from(layout.height());
            shaped.push(layout);
        }
        // Stack from the top of the band (headers) or just enough above the band
        // bottom to fit the footer's content (footers).
        let mut y = if is_footer {
            (band.y1 - total_h).max(band.y0)
        } else {
            band.y0
        };
        let mut drew = false;
        // Active-context bands draw their ink at full strength (so the editable
        // text reads as live copy); non-active bands stay muted chrome. The
        // editable ink is paper-context (dark), not chrome-context — the band sits
        // on the light paper sheet, so chrome ink (light in dark mode) would be
        // invisible there.
        let ink = if editing { PAPER_INK } else { PAPER_MUTED };
        let mut laid: Vec<RegionLaid> = Vec::new();
        // Consume `shaped` so the (non-Clone) layouts can be *moved* into the laid
        // geometry when editing, instead of cloned.
        for (idx, layout) in shaped.into_iter().enumerate() {
            let origin = Point::new(band.x0, y);
            tr.draw_rich(
                scene,
                &layout,
                ctf * Affine::translate((origin.x, origin.y)),
                ink,
            );
            let h = f64::from(layout.height());
            if editing && let Some(b) = blocks.get(idx) {
                laid.push(RegionLaid {
                    id: b.id.clone(),
                    text: b.text.clone(),
                    origin,
                    layout,
                    composition: composition
                        .filter(|(id, _)| *id == b.id)
                        .map(|(_, core)| core.clone()),
                });
            }
            y += h;
            drew = true;
        }
        (drew, laid)
    }

    /// Draw the "you are editing this band" affordance around the active
    /// header/footer band (Word-style header/footer edit mode): a subtle dashed
    /// accent outline plus a small "Header"/"Footer" tab label at the band's top
    /// edge. Drawn through `ctf` so it tracks the page zoom.
    fn draw_active_band_chrome(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        tr: &mut TextRenderer,
        band: Rect,
    ) {
        // A faint fill so the active band reads as the focused surface.
        let pad = Rect::new(band.x0 - 2.0, band.y0 - 1.0, band.x1 + 2.0, band.y1 + 1.0);
        let edge = self.chrome.paper_accent;
        scene.fill(
            Fill::NonZero,
            ctf,
            with_alpha(edge, ACTIVE_BAND_FILL_ALPHA),
            None,
            &pad,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 3.0]),
            ctf,
            edge,
            None,
            &pad,
        );
        let label = if self.inner.region() == Region::Header {
            t("band-header")
        } else {
            t("band-footer")
        };
        let style = self.paper_theme.text(TextRole::Caption);
        let (w, h) = tr.measure_style(&label, None, &style);
        let pad_x = f64::from(self.paper_theme.tokens.spacing.s1);
        let tab = Rect::new(
            pad.x0,
            pad.y0 - f64::from(h),
            pad.x0 + f64::from(w) + 2.0 * pad_x,
            pad.y0,
        );
        let radius = f64::from(self.paper_theme.tokens.radii.detail) / 2.0;
        scene.fill(
            Fill::NonZero,
            ctf,
            edge,
            None,
            &vello::kurbo::RoundedRect::from_rect(tab, radius),
        );
        tr.draw_style(
            scene,
            &label,
            self.chrome.on_paper_accent,
            ctf * Affine::translate((tab.x0 + pad_x, tab.y0)),
            None,
            &style,
        );
    }

    /// Render the caret (and any selection highlight) inside the active
    /// header/footer band, using the laid geometry captured in `region_laid`. The
    /// band caret mirrors the body caret (same `caret_geometry` machinery) so it
    /// blinks-free but lands exactly between glyphs.
    fn draw_band_caret(&mut self, scene: &mut Scene, ctf: Affine) {
        if !self.lifecycle.focused {
            return;
        }
        let caret = self.inner.caret();
        // Selection highlight (single-band, char range) when present.
        if let Some((s, e)) = self.inner.band_selection_range()
            && let Some(rl) = self.region_laid.iter().find(|r| r.id == s.0)
        {
            let composition = rl.composition.as_ref();
            let sb = shown_byte(composition, &rl.text, char_to_byte(&rl.text, s.1));
            let eb = shown_byte(composition, &rl.text, char_to_byte(&rl.text, e.1));
            for (rx0, ry0, rx1, ry1) in rl.layout.selection_rects(sb, eb) {
                let r = Rect::new(
                    rl.origin.x + f64::from(rx0),
                    rl.origin.y + f64::from(ry0),
                    rl.origin.x + f64::from(rx1),
                    rl.origin.y + f64::from(ry1),
                );
                scene.fill(Fill::NonZero, ctf, self.paper_selection(), None, &r);
            }
        }
        if let Some(rl) = self.region_laid.iter().find(|r| r.id == caret.block) {
            let byte = rl.composition.as_ref().map_or_else(
                || char_to_byte(&rl.text, caret.offset),
                |core| core.display_caret(&rl.text),
            );
            let (cx, cy, ch) = rl.layout.caret_geometry(byte);
            let c = Rect::new(
                rl.origin.x + f64::from(cx),
                rl.origin.y + f64::from(cy),
                rl.origin.x + f64::from(cx) + CARET_WIDTH,
                rl.origin.y + f64::from(cy) + f64::from(ch),
            );
            self.ime_rect = Some(ctf.transform_rect_bbox(c));
            scene.fill(Fill::NonZero, ctf, self.chrome.paper_accent, None, &c);
        }
    }

    /// The per-side measurement the active mode edits (the page margins).
    fn active_edges(&self) -> Option<Edges> {
        match self.layout_mode {
            LayoutMode::Margins => Some(self.margins),
            _ => None,
        }
    }

    /// The style id of the block under the caret.
    fn focused_style_key(&self) -> Option<String> {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .map(block_style_key)
    }

    /// The caret block's id when it is an `image` block (an image is "selected"),
    /// read from the cached block set so it works mid-`draw` without the doc.
    /// Drives the panel image inspector + on-page selection handles.
    fn selected_image_id(&self) -> Option<String> {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block && b.kind == "image")
            .map(|b| b.id.clone())
    }

    /// The content hash of the picture the editor would share: the one at
    /// the caret, else the document's only one; `None` with several and the
    /// caret on none of them, or with none.
    pub(crate) fn shared_image(&self) -> Option<String> {
        let images: Vec<&BlockView> = self.blocks.iter().filter(|b| b.kind == "image").collect();
        let chosen =
            images
                .iter()
                .find(|b| b.id == self.inner.caret.block)
                .or(match images.as_slice() {
                    [only] => Some(only),
                    _ => None,
                })?;
        match chosen.attrs.get("blob") {
            Some(AttrVal::Str(hash)) if !hash.is_empty() => Some(hash.clone()),
            _ => None,
        }
    }

    /// The current on-page display size `(w, h)` of the selected image, read from
    /// this frame's laid geometry (post fit-to-column), or `None` when unknown.
    fn selected_image_dims(&self) -> Option<(f64, f64)> {
        let id = self.selected_image_id()?;
        self.laid
            .iter()
            .find(|l| l.id == id && l.kind == "image")
            .map(|l| (l.width, l.height))
    }

    /// The `space_after` of the focused block's style.
    fn focused_space_after(&self) -> f64 {
        self.focused_style_key()
            .map_or(8.0, |k| f64::from(self.sheet.get(&k).space_after))
    }

    /// `(x-center, y)` of the spacing grip — centered in the gap *after the
    /// focused block*, so dragging it edits that block's style (every block of
    /// the same style follows).
    fn spacing_handle(&self) -> Option<(f64, f64)> {
        let l = self
            .laid
            .iter()
            .find(|l| l.id == self.inner.caret.block)
            .or_else(|| self.laid.first())?;
        let cx = f64::midpoint(self.page_rect.x0, self.page_rect.x1);
        Some((cx, l.y_top + l.height + self.focused_space_after() / 2.0))
    }

    /// Which guide (if any) is under the pointer, for the active layout mode.
    fn hit_guide(&self, p: Point) -> Option<Side> {
        let page = self.page_rect;
        if let Some(e) = self.active_edges() {
            let mut best: Option<(Side, f64)> = None;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let along = if vert {
                    p.y >= page.y0 && p.y <= page.y1
                } else {
                    p.x >= page.x0 && p.x <= page.x1
                };
                let d = if vert {
                    (p.x - c).abs()
                } else {
                    (p.y - c).abs()
                };
                if along && d <= GUIDE_GRAB && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((side, d));
                }
            }
            return best.map(|(s, _)| s);
        }
        if self.layout_mode == LayoutMode::Spacing
            && let Some((_, hy)) = self.spacing_handle()
            && (p.y - hy).abs() <= GUIDE_GRAB + 4.0
            && p.x >= page.x0
            && p.x <= page.x1
        {
            return Some(Side::Bottom); // side unused for spacing
        }
        None
    }

    /// Apply a live guide drag to the relevant layout value.
    fn apply_guide_drag(&mut self, gd: GuideDrag, p: Point) {
        let page = self.page_rect;
        match gd.mode {
            LayoutMode::Margins => apply_edges(&mut self.margins, page, gd.side, p, gd.kind),
            LayoutMode::Spacing => {
                // Edit the focused block's *style* spacing — all blocks of that
                // style follow (component-instance model).
                let bottom = self
                    .laid
                    .iter()
                    .find(|l| l.id == self.inner.caret.block)
                    .or_else(|| self.laid.first())
                    .map(|l| l.y_top + l.height);
                let key = self.focused_style_key();
                if let (Some(bottom), Some(key)) = (bottom, key) {
                    let after = num::f32_of_f64((2.0 * (p.y - bottom)).clamp(0.0, 80.0));
                    if let Some(st) = self.sheet.styles.get_mut(&key) {
                        st.space_after = after;
                    }
                }
            }
            LayoutMode::None => {}
        }
    }

    // ---- horizontal ruler ----------------------------------------------------

    /// The caret paragraph's left/hanging indent (px), read from the cached block
    /// views (the `indent` block attr). `0.0` when unset. Mirrors the model's
    /// `block_indent` so the draw pass (which has no `state`) can read it.
    fn caret_block_indent(&self) -> f64 {
        self.block_num_attr_view(&self.inner.caret.block, "indent")
    }

    /// The caret paragraph's first-line indent (px), read from cached block views.
    fn caret_block_first_line_indent(&self) -> f64 {
        self.block_num_attr_view(&self.inner.caret.block, "first_line_indent")
    }

    /// The caret paragraph's tab stops (px from the left text margin), read from
    /// cached block views (the comma-encoded `tab_stops` attr).
    fn caret_block_tab_stops(&self) -> Vec<f64> {
        let raw = self
            .blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .and_then(|b| match b.attrs.get("tab_stops") {
                Some(AttrVal::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        crate::word_processor::parse_tab_stops(&raw)
    }

    /// Read a numeric (`Float`/`Int`) attr off a cached block view, `0.0` absent.
    fn block_num_attr_view(&self, block: &str, key: &str) -> f64 {
        self.blocks
            .iter()
            .find(|b| b.id == block)
            .map_or(0.0, |b| match b.attrs.get(key) {
                Some(AttrVal::Float(f)) => *f,
                Some(AttrVal::Int(n)) => num::f64_of_i64(*n),
                _ => 0.0,
            })
    }

    /// Map an unscaled content x (the page coordinate space) to screen x using the
    /// cached zoom + anchor — the same scale-about the page content draws through.
    #[cfg(test)]
    fn content_x_to_screen(&self, cx: f64) -> f64 {
        self.zoom.content_x_to_screen(cx)
    }

    fn ruler_model(&self, band: Rect, page_x: f64, page_w: f64) -> RulerModel {
        RulerModel {
            band,
            page_x,
            page_w,
            margin_left: self.margins.left,
            margin_right: self.margins.right,
            indent_left: self.caret_block_indent(),
            indent_first: self.caret_block_first_line_indent(),
            tab_stops: self.caret_block_tab_stops(),
            zoom: self.zoom.factor(),
            anchor_x: self.zoom.anchor().x,
        }
    }

    fn current_ruler_model(&self) -> RulerModel {
        self.ruler_model(self.ruler.rect(), self.page_rect.x0, self.page_rect.width())
    }

    #[cfg(test)]
    fn ruler_markers(&self, page_x: f64, page_w: f64) -> Vec<(RulerMarker, f64)> {
        Ruler::markers(&self.ruler_model(self.ruler.rect(), page_x, page_w))
    }

    fn draw_ruler(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        band: Rect,
        page_x: f64,
        page_w: f64,
    ) {
        let model = self.ruler_model(band, page_x, page_w);
        self.ruler.draw(scene, transform, text, theme, &model);
    }

    fn apply_ruler_preview(&mut self, effect: RulerEffect) {
        match effect {
            RulerEffect::PreviewMarginLeft(value) => self.margins.left = value,
            RulerEffect::PreviewMarginRight(value) => self.margins.right = value,
            RulerEffect::PreviewIndentLeft(value) => self.set_caret_block_indent_view(value),
            RulerEffect::PreviewIndentFirst(value) => {
                self.set_caret_block_first_line_view(value);
            }
            RulerEffect::PreviewTab { index, value } => {
                self.set_caret_tab_stop_view(index, value);
            }
            _ => {}
        }
    }

    fn ruler_press(&mut self, state: &mut ui::widget::Edits<'_>) {
        let model = self.current_ruler_model();
        if let Some(RulerEffect::AddTab(value)) = self.ruler.press(self.pointer, &model) {
            let block = self.inner.caret.block.clone();
            let mut stops = self.inner.block_tab_stops(state, &block);
            if stops.iter().all(|stop| (*stop - value).abs() >= 2.0) {
                stops.push(value.max(0.0));
                stops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let encoded = stops
                    .iter()
                    .map(|stop| format!("{stop:.1}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "tab_stops",
                        crate::actions::AttrVal::Str(encoded),
                    ),
                );
            }
            self.refresh_blocks(state);
        }
    }

    fn ruler_drag_move(&mut self) {
        let model = self.current_ruler_model();
        if let Some(effect) = self.ruler.drag_to(self.pointer, &model) {
            self.apply_ruler_preview(effect);
        }
    }

    fn ruler_drag_release(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(effect) = self.ruler.release() else {
            return;
        };
        match effect {
            RulerEffect::CommitMargins => self.persist_margins(state),
            RulerEffect::CommitIndentLeft => {
                let value = self.caret_block_indent();
                let block = self.inner.caret.block.clone();
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "indent",
                        crate::actions::AttrVal::Float(value),
                    ),
                );
            }
            RulerEffect::CommitIndentFirst => {
                let value = self.caret_block_first_line_indent();
                let block = self.inner.caret.block.clone();
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "first_line_indent",
                        crate::actions::AttrVal::Float(value),
                    ),
                );
            }
            RulerEffect::CommitTabs { remove } => {
                let mut stops = self.caret_block_tab_stops();
                if let Some(index) = remove.filter(|index| *index < stops.len()) {
                    stops.remove(index);
                }
                let block = self.inner.caret.block.clone();
                let encoded = stops
                    .iter()
                    .map(|stop| format!("{stop:.1}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.perform_action(
                    state,
                    &crate::actions::set_block_attr_action(
                        self.inner.region_kind(),
                        vec![block],
                        "tab_stops",
                        crate::actions::AttrVal::Str(encoded),
                    ),
                );
            }
            _ => return,
        }
        self.refresh_blocks(state);
    }

    /// Update the caret block's `indent` attr in the cached block views only (a
    /// live drag preview before the release persists it).
    fn set_caret_block_indent_view(&mut self, v: f64) {
        let id = self.inner.caret.block.clone();
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            self.materialized.preview = true;
            b.attrs
                .insert("indent".to_string(), AttrVal::Float(v.max(0.0)));
        }
    }

    /// Update the caret block's `first_line_indent` attr in the cached block views.
    fn set_caret_block_first_line_view(&mut self, v: f64) {
        let id = self.inner.caret.block.clone();
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            self.materialized.preview = true;
            b.attrs
                .insert("first_line_indent".to_string(), AttrVal::Float(v));
        }
    }

    /// Update tab stop `i` in the cached block views (live drag preview).
    fn set_caret_tab_stop_view(&mut self, i: usize, v: f64) {
        let id = self.inner.caret.block.clone();
        let mut stops = self.caret_block_tab_stops();
        if let Some(stop) = stops.get_mut(i) {
            *stop = v.max(0.0);
        }
        let encoded = stops
            .iter()
            .map(|p| format!("{p:.1}"))
            .collect::<Vec<_>>()
            .join(",");
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            self.materialized.preview = true;
            b.attrs
                .insert("tab_stops".to_string(), AttrVal::Str(encoded));
        }
    }

    /// Draggable guides + live value labels for the active layout mode, drawn
    /// over the page content.
    fn draw_layout_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        page: Rect,
    ) {
        // The guides are what you are dragging: your colour, made legible
        // on the paper; the guide at rest is a quieter hairline of it.
        let theme = &self.paper_theme;
        let you = self.chrome.paper_accent;
        let rest = self.chrome.paper_edge;
        let rule = f64::from(theme.tokens.metrics.chrome_rule_px).max(1.0);
        let grip = f64::from(theme.tokens.spacing.s1);
        let gap = f64::from(theme.tokens.spacing.s2);
        let label_style = theme.text(TextRole::Caption).numeric();
        if let Some(e) = self.active_edges() {
            let drag = self.guide_drag;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let active = drag.is_some_and(|g| match g.kind {
                    DragKind::Proportional => true,
                    DragKind::Pair => g.side == side || g.side.opposite() == side,
                    DragKind::Single => g.side == side,
                });
                let (col, t) = if active {
                    (you, rule * 2.0)
                } else {
                    (rest, rule)
                };
                // At rest a guide runs through the margins only, from the
                // page's edge to the text block's corner on either side, so
                // it never runs along the text; the guide being dragged
                // spans the page.
                let (lo, hi) = if vert {
                    (page.y0 + e.top, page.y1 - e.bottom)
                } else {
                    (page.x0 + e.left, page.x1 - e.right)
                };
                let (start, end) = if vert {
                    (page.y0, page.y1)
                } else {
                    (page.x0, page.x1)
                };
                let spans = if active {
                    vec![(start, end)]
                } else {
                    vec![(start, lo), (hi, end)]
                };
                for (a, b) in spans {
                    let line = if vert {
                        Rect::new(c - t / 2.0, a, c + t / 2.0, b)
                    } else {
                        Rect::new(a, c - t / 2.0, b, c + t / 2.0)
                    };
                    scene.fill(Fill::NonZero, transform, col, None, &line);
                }
                // A grip in the margin shows where to take the guide: half
                // way across the top margin on a side guide, half way across
                // the leading margin on a top or bottom one.
                let (gx, gy) = if vert {
                    (c, f64::midpoint(start, lo))
                } else {
                    (f64::midpoint(start, lo), c)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    you,
                    None,
                    &vello::kurbo::Circle::new((gx, gy), grip),
                );
                // The value, set just inside the page beside its guide.
                let label = format!("{}", num::i64_of_f64(e.get(side).round()));
                let (lx, ly) = if vert {
                    (c + gap, page.y0 + gap)
                } else {
                    (page.x0 + gap, c + gap)
                };
                tr.draw_style(
                    scene,
                    &label,
                    you,
                    transform * Affine::translate((lx, ly)),
                    None,
                    &label_style,
                );
            }
        } else if self.layout_mode == LayoutMode::Spacing
            && let Some((hx, hy)) = self.spacing_handle()
        {
            let half = f64::from(theme.tokens.spacing.s4);
            let pill = Rect::new(hx - half, hy - grip / 2.0, hx + half, hy + grip / 2.0);
            scene.fill(
                Fill::NonZero,
                transform,
                you,
                None,
                &vello::kurbo::RoundedRect::from_rect(pill, grip / 2.0),
            );
            let label = format!("{} px", num::i64_of_f64(self.focused_space_after().round()));
            let (_, h) = tr.measure_style(&label, None, &label_style);
            tr.draw_style(
                scene,
                &label,
                you,
                transform * Affine::translate((hx + half + gap, hy - f64::from(h) / 2.0)),
                None,
                &label_style,
            );
        }
    }

    /// The status band's height: one small control with the tightest step
    /// above and below.
    fn status_band_height(theme: &Theme) -> f64 {
        ButtonFace::height(ControlSize::Small, theme) + 2.0 * f64::from(theme.tokens.spacing.s1)
    }

    /// The status band along the bottom: the Margins / Spacing layout modes
    /// as one segmented control, the live readout of the mode in use, the
    /// word and character counts, and the zoom field. It stands on the
    /// content ground with one hairline above it.
    fn draw_layout_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        ui::chrome::status_bar::draw(scene, transform, tr, theme, bar, "", "");
        let sp = &theme.tokens.spacing;
        let (s3, s4) = (f64::from(sp.s3), f64::from(sp.s4));
        let cy = bar.center().y;
        let control_h = ButtonFace::height(ControlSize::Small, theme);

        // The two layout modes are toggles, at most one of them in use and
        // neither at rest: plain buttons that stand on the selection lift
        // while their mode is on.
        let gap = f64::from(sp.s1);
        let mut x = bar.x0 + s3;
        let mut chips = Vec::with_capacity(2);
        for (mode, label) in [
            (LayoutMode::Margins, t("layout-margins")),
            (LayoutMode::Spacing, t("layout-spacing")),
        ] {
            let w = ButtonFace::new(Rect::ZERO, &label)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme);
            let rect = Rect::new(x, cy - control_h / 2.0, x + w, cy + control_h / 2.0);
            Button::paint(
                scene,
                transform,
                tr,
                theme,
                &ButtonFace::new(rect, &label)
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small)
                    .toggled(self.layout_mode == mode)
                    .hovered(rect.contains(self.pointer)),
            );
            chips.push((mode, rect));
            x = rect.x1 + gap;
        }
        let strip_x1 = x - gap;
        self.layout_chips = chips;

        // Trailing: the zoom field, then the counts before it.
        let zoom_w = self.zoom_input.preferred_width(theme, tr);
        let zoom = Rect::new(
            bar.x1 - s3 - zoom_w,
            cy - control_h / 2.0,
            bar.x1 - s3,
            cy + control_h / 2.0,
        );
        if !self.zoom_input.is_editing() {
            self.zoom_input
                .set_value((self.zoom.factor() * 100.0).round());
        }
        self.zoom_input.apply_layout_external(zoom);
        self.zoom_rect = zoom;
        Widget::draw(
            &mut self.zoom_input,
            scene,
            transform,
            tr,
            theme,
            &PresenceCtx::empty(),
        );

        let style = theme.text(TextRole::Caption).numeric();
        let ink = theme.tokens.colors.text_dim;
        let mut right = zoom.x0 - s4;
        let mut left = strip_x1 + s4;
        for readout in self.status_readouts().into_iter().rev() {
            let (w, h) = tr.measure_style(&readout, None, &style);
            let w = f64::from(w);
            if right - w < left {
                break;
            }
            tr.draw_style(
                scene,
                &readout,
                ink,
                transform * Affine::translate((right - w, cy - f64::from(h) / 2.0)),
                None,
                &style,
            );
            right -= w + s4;
        }

        if let Some(readout) = self.layout_readout() {
            let (w, h) = tr.measure_style(&readout, None, &style);
            if left + f64::from(w) <= right {
                tr.draw_style(
                    scene,
                    &readout,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((left, cy - f64::from(h) / 2.0)),
                    None,
                    &style,
                );
                left += f64::from(w) + s4;
            }
        }
        let _ = left;
    }

    /// The live value of the layout mode in use, or nothing at rest.
    fn layout_readout(&self) -> Option<String> {
        match self.layout_mode {
            LayoutMode::Margins => Some(t_args(
                "layout-margins-readout",
                &[
                    arg("l", num::i64_of_f64(self.margins.left.round())),
                    arg("t", num::i64_of_f64(self.margins.top.round())),
                    arg("r", num::i64_of_f64(self.margins.right.round())),
                    arg("b", num::i64_of_f64(self.margins.bottom.round())),
                ],
            )),
            LayoutMode::Spacing => {
                let name = self.focused_style_key().map_or_else(
                    || t("style-body"),
                    |k| crate::styles::style_display_name(&k, &self.sheet.get(&k)),
                );
                Some(t_args(
                    "layout-spacing-readout",
                    &[
                        arg("name", name),
                        arg("px", num::i64_of_f64(self.focused_space_after().round())),
                    ],
                ))
            }
            LayoutMode::None => None,
        }
    }

    /// Whether the styles panel is drawn this frame.
    fn styles_panel_visible(&self) -> bool {
        self.view.styles_panel_visible
    }

    /// Wider compact tiles use a drawer; phones present the inspector as
    /// the foreground page. Desktop windows reserve space beside the document.
    fn styles_panel_overlay(&self) -> bool {
        self.view.styles_panel_visible && self.inspector_drawer.is_overlay()
    }

    fn styles_panel_full_window(&self) -> bool {
        self.view.styles_panel_visible
            && ui::widget::modal_dialog::uses_full_window(self.rect, &self.desk_theme)
    }

    /// The contextual inspector. Docked, it is the window's inspector region;
    /// on a phone it fills the window using the shared opaque page surface.
    /// A header names
    /// what it inspects beside its close button, and below it the image
    /// inspector or the paragraph's two sections scroll as one column.
    fn draw_styles_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
    ) {
        self.panel_hits.clear();
        for slot in &mut self.num_inputs {
            slot.rect = Rect::ZERO;
        }
        self.line_spacing_rect = Rect::ZERO;
        self.style_pick_rect = Rect::ZERO;
        let local = theme.clone();
        let sp = &local.tokens.spacing;
        self.panel_rect = panel;
        let pad = f64::from(local.tokens.spacing.s4);
        let x0 = panel.x0 + pad;
        let right = panel.x1 - pad;

        // Header: what the panel inspects, and its close button.
        let image = self.selected_image_id().is_some();
        let title = if image {
            t("panel-image")
        } else {
            t("toolbar-styles")
        };
        let title_style = local.text(TextRole::Heading);
        let (_, title_h) = tr.measure_style(&title, None, &title_style);
        let target = ButtonFace::height(ControlSize::Small, &local)
            .max(f64::from(local.tokens.metrics.min_pointer_target))
            .max(f64::from(title_h));
        let header = Rect::new(
            x0,
            panel.y0 + f64::from(sp.s2),
            right,
            panel.y0 + f64::from(sp.s2) + target,
        );
        let close = Rect::new(right - target, header.y0, right, header.y1);
        tr.draw_style(
            scene,
            &title,
            local.tokens.colors.text_primary,
            transform * Affine::translate((x0, header.center().y - f64::from(title_h) / 2.0)),
            Some(num::f32_of_f64((close.x0 - x0).max(1.0))),
            &title_style,
        );
        let close_label = t("panel-close");
        Button::paint(
            scene,
            transform,
            tr,
            &local,
            &ButtonFace::icon_only(close, Icon::X, &close_label)
                .hovered(close.contains(self.pointer)),
        );
        self.panel_hits.push((close, PanelHit::Close));

        // The column below the header scrolls when it is taller than the
        // panel.
        let body = Rect::new(panel.x0, header.y1 + f64::from(sp.s2), panel.x1, panel.y1);
        self.panel_body = body;
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        let top = body.y0 - self.panel_scroll;
        let mut ctx = PanelCtx {
            scene,
            transform,
            tr,
            theme: &local,
            x0,
            right,
            y: top,
            clip: body,
        };
        if image {
            self.draw_image_inspector(&mut ctx);
        } else if let Some(key) = self.focused_style_key() {
            let st = self.sheet.get(&key);
            self.draw_panel_this_paragraph(&mut ctx);
            self.draw_panel_style(&mut ctx, &key, &st);
        } else {
            let style = local.text(TextRole::Body);
            ctx.tr.draw_style(
                ctx.scene,
                &t("panel-caret-hint"),
                local.tokens.colors.text_dim,
                transform * Affine::translate((x0, ctx.y)),
                Some(num::f32_of_f64((right - x0).max(1.0))),
                &style,
            );
            ctx.y += f64::from(ctx.tr.measure_style("Ag", None, &style).1);
        }
        let content_h = ctx.y + pad - top;
        ctx.scene.pop_layer();
        self.panel_max_scroll = (content_h - body.height()).max(0.0);
        self.panel_scroll = self.panel_scroll.clamp(0.0, self.panel_max_scroll);
    }

    /// "This paragraph": direct formatting of the paragraph(s) under the
    /// caret — alignment as a segmented control and line spacing as a menu.
    fn draw_panel_this_paragraph(&mut self, ctx: &mut PanelCtx<'_>) {
        ctx.section(&t("panel-this-paragraph"), true);
        ctx.field_label(&t("panel-align"));
        let values = ["left", "center", "right", "justify"];
        let segments = [
            ui::widget::Segment::icon(ui::icons::Icon::AlignLeft, t("panel-align-left")),
            ui::widget::Segment::icon(ui::icons::Icon::AlignCenter, t("panel-align-center")),
            ui::widget::Segment::icon(ui::icons::Icon::AlignRight, t("panel-align-right")),
            ui::widget::Segment::icon(ui::icons::Icon::AlignJustify, t("panel-align-justify")),
        ];
        let current = self.current_align();
        let selected = values
            .iter()
            .position(|v| *v == current)
            .unwrap_or(usize::MAX);
        let rects = ctx.segmented_items(&segments, selected, self.pointer);
        for (rect, value) in rects.into_iter().zip(values) {
            self.panel_hits.push((rect, PanelHit::ParaAlign(value)));
        }
        ctx.field_label(&t("panel-line-spacing"));
        let rect = ctx.control_line();
        let current = line_spacing_label(self.current_line_spacing());
        if !self.line_spacing.is_open() {
            self.line_spacing.set_selected(current);
        }
        let viewport = self.floating_viewport();
        self.line_spacing_rect = Self::host_menu(&mut self.line_spacing, ctx, rect, viewport);
    }

    /// Lay out and draw a hosted menu in `rect`; returns where it was drawn
    /// (nothing while scrolled out of view).
    fn host_menu(
        menu: &mut ui::widget::Dropdown,
        ctx: &mut PanelCtx<'_>,
        rect: Rect,
        viewport: Rect,
    ) -> Rect {
        menu.apply_layout_external(rect);
        menu.set_viewport(viewport);
        if !ctx.visible(rect) {
            return Rect::ZERO;
        }
        Widget::draw(
            menu,
            ctx.scene,
            ctx.transform,
            ctx.tr,
            ctx.theme,
            &PresenceCtx::empty(),
        );
        rect
    }

    /// "Paragraph style": the named style under the caret, picked from a
    /// menu, then the style's properties, which change every paragraph of
    /// that style.
    fn draw_panel_style(
        &mut self,
        ctx: &mut PanelCtx<'_>,
        key: &str,
        st: &crate::styles::StyleProps,
    ) {
        ctx.section(&t("panel-style"), false);
        let options: Vec<(String, String)> = self
            .sheet
            .order()
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    crate::styles::style_display_name(id, &self.sheet.get(id)),
                )
            })
            .collect();
        if options != self.style_pick_options {
            self.style_pick.set_value_options(options.clone());
            self.style_pick_options = options;
        }
        if !self.style_pick.is_open() {
            self.style_pick.set_selected(key);
        }
        let rect = ctx.control_line();
        let viewport = self.floating_viewport();
        self.style_pick_rect = Self::host_menu(&mut self.style_pick, ctx, rect, viewport);
        ctx.caption(&t("panel-style-hint"));

        // Font family, as a segmented control.
        ctx.field_label(&t("panel-font"));
        let fonts = ["sans", "serif", "mono"];
        let labels = [
            t("panel-font-sans"),
            t("panel-font-serif"),
            t("panel-font-mono"),
        ];
        let selected = fonts
            .iter()
            .position(|f| *f == st.font.as_str())
            .unwrap_or(0);
        let rects = ctx.segmented(&labels, selected, self.pointer);
        for (rect, font) in rects.into_iter().zip(fonts) {
            self.panel_hits.push((rect, PanelHit::SetFont(font)));
        }

        // Bold and italic, as two toggles side by side.
        ctx.field_label(&t("panel-weight"));
        let (rb, ri) = ctx.pair();
        ctx.button(rb, &t("panel-bold"), Some(st.bold), self.pointer);
        ctx.button(ri, &t("panel-italic"), Some(st.italic), self.pointer);
        if ctx.visible(rb) {
            self.panel_hits.push((rb, PanelHit::ToggleBold));
            self.panel_hits.push((ri, PanelHit::ToggleItalic));
        }
        self.draw_panel_style_properties(ctx, st);
    }

    /// The style's colour, sizes and spacing, then a new style from this
    /// paragraph's look.
    fn draw_panel_style_properties(
        &mut self,
        ctx: &mut PanelCtx<'_>,
        st: &crate::styles::StyleProps,
    ) {
        // Colour: the document palette as swatches and a custom colour.
        ctx.field_label(&t("panel-colour"));
        let swatch_gap = f64::from(ctx.theme.tokens.spacing.s1);
        let count = STYLE_PALETTE.len() + 1;
        let side = ((ctx.right - ctx.x0 - swatch_gap * num::f64_of_usize(count - 1))
            / num::f64_of_usize(count))
        .min(ButtonFace::height(ControlSize::Small, ctx.theme));
        let names = swatch_names(PaletteKind::Text);
        let mut x = ctx.x0;
        for (c, name) in STYLE_PALETTE.iter().zip(names) {
            let r = Rect::new(x, ctx.y, x + side, ctx.y + side);
            if ctx.visible(r) {
                ColorSwatchFace::new(r, Color::from_rgb8(c[0], c[1], c[2]), name)
                    .selected(*c == st.color)
                    .hovered(r.contains(self.pointer))
                    .paint(ctx.scene, ctx.transform, ctx.tr, ctx.theme);
                self.panel_hits.push((r, PanelHit::SetColor(*c)));
            }
            x += side + swatch_gap;
        }
        let custom = Rect::new(x, ctx.y, x + side, ctx.y + side);
        if ctx.visible(custom) {
            let label = t("panel-colour-custom");
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                ctx.theme,
                &ButtonFace::icon_only(custom, Icon::Plus, &label)
                    .control_size(ControlSize::Small)
                    .hovered(custom.contains(self.pointer)),
            );
            self.panel_hits.push((custom, PanelHit::OpenColorPicker));
        }
        ctx.y += side + f64::from(ctx.theme.tokens.spacing.rhythm.related);

        // Size and spacing, as number fields two to a line, each under its
        // label.
        for pair in [
            [NumField::Size, NumField::Indent],
            [NumField::SpaceBefore, NumField::SpaceAfter],
        ] {
            let (left, right) = ctx.label_pair(&pair[0].label(), &pair[1].label());
            for (field, column) in pair.into_iter().zip([left, right]) {
                let value = f64::from(field.get(st));
                let decimals = usize::from(value.fract().abs() > 0.05);
                let Some(slot) = self.num_inputs.iter_mut().find(|slot| slot.field == field) else {
                    continue;
                };
                if slot.decimals != decimals {
                    slot.input = field.input().with_precision(decimals);
                    slot.decimals = decimals;
                }
                if !slot.input.is_editing() {
                    slot.input.set_value(value);
                }
                let w = slot
                    .input
                    .preferred_width(ctx.theme, ctx.tr)
                    .min(column.width());
                let rect = Rect::new(column.x0, column.y0, column.x0 + w, column.y1);
                slot.input.apply_layout_external(rect);
                if ctx.visible(rect) {
                    Widget::draw(
                        &mut slot.input,
                        ctx.scene,
                        ctx.transform,
                        ctx.tr,
                        ctx.theme,
                        &PresenceCtx::empty(),
                    );
                    slot.rect = rect;
                } else {
                    slot.rect = Rect::ZERO;
                }
            }
        }

        // A new style from this paragraph's look.
        ctx.y += f64::from(ctx.theme.tokens.spacing.rhythm.related);
        let r = ctx.control_line();
        ctx.button(r, &t("panel-new-style"), None, self.pointer);
        if ctx.visible(r) {
            self.panel_hits.push((r, PanelHit::NewStyle));
        }
    }

    /// The image inspector shown when an image is selected: its size, a
    /// smaller / larger stepper (aspect-locked), flips, crop and placement.
    /// Its controls register as `panel_hits` so pointer and assistive
    /// technology activation share one dispatch path.
    fn draw_image_inspector(&mut self, ctx: &mut PanelCtx<'_>) {
        let (dw, dh) = self.selected_image_dims().unwrap_or((0.0, 0.0));
        let dims = format!(
            "{} × {} px",
            num::i64_of_f64(dw.round()),
            num::i64_of_f64(dh.round())
        );
        ctx.text_line(&dims, TextRole::Body, false, false);
        ctx.y += f64::from(ctx.theme.tokens.spacing.rhythm.related);

        let img_id = self.selected_image_id();
        let block = self
            .blocks
            .iter()
            .find(|b| Some(&b.id) == img_id.as_ref())
            .cloned();
        let (fh, fv) = block.as_ref().map_or((false, false), |b| {
            (
                matches!(b.attrs.get("flip_h"), Some(AttrVal::Bool(true))),
                matches!(b.attrs.get("flip_v"), Some(AttrVal::Bool(true))),
            )
        });

        for (label, first, second, toggles) in [
            (
                t("panel-image-size"),
                (t("panel-image-smaller"), PanelHit::ImageScale(0.9), false),
                (t("panel-image-larger"), PanelHit::ImageScale(1.1), false),
                false,
            ),
            (
                t("panel-image-flip"),
                (t("panel-image-flip-h"), PanelHit::ImageFlipH, fh),
                (t("panel-image-flip-v"), PanelHit::ImageFlipV, fv),
                true,
            ),
            (
                t("panel-image-crop"),
                (t("panel-image-crop-inset"), PanelHit::ImageCropInset, false),
                (t("panel-image-crop-reset"), PanelHit::ImageCropReset, false),
                false,
            ),
        ] {
            ctx.field_label(&label);
            let (r1, r2) = ctx.pair();
            for (rect, (label, hit, on)) in [(r1, first), (r2, second)] {
                ctx.button(rect, &label, toggles.then_some(on), self.pointer);
                if ctx.visible(rect) {
                    self.panel_hits.push((rect, hit));
                }
            }
        }

        // Placement, as one segmented control the width of the column.
        ctx.field_label(&t("panel-image-position"));
        let labels = [
            t("panel-image-inline"),
            t("panel-image-free"),
            t("panel-image-wrap-left"),
            t("panel-image-wrap-right"),
        ];
        let selected =
            block
                .as_ref()
                .map_or(usize::MAX, |b| match (image_anchor(b), image_wrap(b)) {
                    ("page", _) => 1,
                    (_, "left") => 2,
                    (_, "right") => 3,
                    _ => 0,
                });
        let rects = ctx.segmented(&labels, selected, self.pointer);
        for (rect, hit) in rects.into_iter().zip([
            PanelHit::ImageInline,
            PanelHit::ImageFreePosition,
            PanelHit::ImageWrapLeft,
            PanelHit::ImageWrapRight,
        ]) {
            self.panel_hits.push((rect, hit));
        }
    }

    // ---- document outline / navigation pane -------------------------------

    /// The document outline: one entry per `heading` block (in document order),
    /// derived from the live block set. Mirrors `WordProcessor::headings`, but the
    /// widget reads its already-materialized `self.blocks` so it works mid-frame
    /// without re-materializing the doc. Excludes generated TOC lines.
    fn outline_entries(&self) -> Vec<crate::word_processor::Heading> {
        self.blocks
            .iter()
            .filter(|b| b.kind == "heading" && !crate::word_processor::block_is_toc(b))
            .map(|b| {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) if *n >= 1 => *n,
                    _ => 1,
                };
                crate::word_processor::Heading {
                    id: b.id.clone(),
                    level,
                    text: b.text.clone(),
                }
            })
            .collect()
    }

    /// The heading nearest (at or just above) the current scroll position — the
    /// one to highlight in the outline as "where you are". Uses the last frame's
    /// laid geometry (`y_top` is screen-space post-scroll; recover content space).
    /// Returns the heading block id, or `None` when there are no headings laid.
    fn active_outline_block(&self) -> Option<String> {
        // Content-space top of the visible canvas region.
        let view_top = self.scroll_y;
        let mut best: Option<(f64, String)> = None; // (content_y, id)
        let mut first: Option<(f64, String)> = None;
        for l in &self.laid {
            if l.kind != "heading" {
                continue;
            }
            let cy = l.y_top + self.scroll_y; // content-space top
            if first.as_ref().is_none_or(|(y, _)| cy < *y) {
                first = Some((cy, l.id.clone()));
            }
            // The deepest heading whose top is at/above the viewport top.
            if cy <= view_top + 1.0 && best.as_ref().is_none_or(|(y, _)| cy >= *y) {
                best = Some((cy, l.id.clone()));
            }
        }
        best.or(first).map(|(_, id)| id)
    }

    /// Generate (or regenerate) the table of contents from the document headings.
    /// Page numbers are read from the last frame's laid geometry (`Laid::page`);
    /// a heading not yet laid out falls back to page 1. Delegates the block
    /// mutation to the model so the insert/replace logic stays testable.
    fn generate_toc(&mut self, state: &mut ui::widget::Edits<'_>) {
        // Map heading block id → 1-based page from the last laid-out frame.
        let page_of: HashMap<&str, usize> = self
            .laid
            .iter()
            .map(|l| (l.id.as_str(), l.page + 1))
            .collect();
        let entries: Vec<crate::actions::TocTarget> = self
            .outline_entries()
            .into_iter()
            .map(|h| {
                let page = page_of.get(h.id.as_str()).copied();
                crate::actions::TocTarget {
                    heading: h.id,
                    level: h.level,
                    text: h.text,
                    page,
                }
            })
            .collect();
        if entries.is_empty() {
            // Nothing to build a TOC from — tell the user *why* nothing happened.
            self.toast = Some((t("toast-toc-empty"), ToastKind::Info));
            self.lifecycle.dirty = true;
            return;
        }
        let n = entries.len();
        self.perform_action(state, &crate::actions::generate_toc_action(entries));
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        // Scroll the freshly-inserted TOC (always at the document top) into view
        // and surface a confirmation toast, so the action has a visible result.
        self.scroll_y = 0.0;
        self.toast = Some((
            t_args("toast-toc-updated", &[arg("n", num::i64_of_usize(n))]),
            ToastKind::Success,
        ));
    }

    /// Draw the transient confirmation through the shared toast-card renderer.
    /// It remains anchored above the status bar and is dismissed by the next
    /// interaction.
    fn draw_toast(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some((msg, kind)) = self.toast.as_ref() else {
            return;
        };
        let area = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1 - self.layout_bar_height,
        );
        paint_transient(
            scene,
            transform,
            tr,
            theme,
            area,
            ToastAnchor::BottomCenter,
            &ToastCard::new(msg, *kind),
        );
    }
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

impl PanelCtx<'_> {
    /// Whether `rect` shows in the scrolled column at all.
    fn visible(&self, rect: Rect) -> bool {
        rect.y1 > self.clip.y0 && rect.y0 < self.clip.y1
    }

    /// One block of text, wrapped to the column.
    fn text_line(&mut self, text: &str, role: TextRole, strong: bool, dim: bool) {
        let style = if strong {
            self.theme.text(role).strong()
        } else {
            self.theme.text(role)
        };
        let width = num::f32_of_f64((self.right - self.x0).max(1.0));
        let colors = &self.theme.tokens.colors;
        let ink = if dim {
            colors.text_dim
        } else {
            colors.text_primary
        };
        let (_, h) = self.tr.measure_style(text, Some(width), &style);
        let rect = Rect::new(self.x0, self.y, self.right, self.y + f64::from(h));
        if self.visible(rect) {
            self.tr.draw_style(
                self.scene,
                text,
                ink,
                self.transform * Affine::translate((self.x0, self.y)),
                Some(width),
                &style,
            );
        }
        self.y += f64::from(h);
    }

    /// A section header in `Subheading`, a group's gap above it unless it is
    /// the panel's first.
    fn section(&mut self, title: &str, first: bool) {
        if !first {
            self.y += f64::from(self.theme.tokens.spacing.rhythm.group);
        }
        self.text_line(title, TextRole::Subheading, false, false);
        self.y += f64::from(self.theme.tokens.spacing.rhythm.related);
    }

    /// A field's label, in secondary ink, over the control on the line
    /// below it. Every field of the inspector is set this way, so labels
    /// and controls share one leading edge.
    fn field_label(&mut self, label: &str) {
        self.text_line(label, TextRole::Label, false, true);
        self.y += f64::from(self.theme.tokens.spacing.rhythm.tight);
    }

    /// One line of `Caption` in secondary ink under the control above it.
    fn caption(&mut self, text: &str) {
        self.y -= f64::from(self.theme.tokens.spacing.rhythm.related)
            - f64::from(self.theme.tokens.spacing.rhythm.tight);
        self.text_line(text, TextRole::Caption, false, true);
        self.y += f64::from(self.theme.tokens.spacing.rhythm.related);
    }

    /// A line one small control tall, the width of the column; returns it
    /// for the control.
    fn control_line(&mut self) -> Rect {
        let h = ButtonFace::height(ControlSize::Small, self.theme);
        let line = Rect::new(self.x0, self.y, self.right, self.y + h);
        self.y += h + f64::from(self.theme.tokens.spacing.rhythm.related);
        line
    }

    /// A control line split into two equal halves.
    fn pair(&mut self) -> (Rect, Rect) {
        let gap = f64::from(self.theme.tokens.spacing.s2);
        let line = self.control_line();
        let mid = f64::midpoint(line.x0, line.x1);
        (
            Rect::new(line.x0, line.y0, mid - gap / 2.0, line.y1),
            Rect::new(mid + gap / 2.0, line.y0, line.x1, line.y1),
        )
    }

    /// Two fields side by side, each label over its half of the control
    /// line below; returns the two halves.
    fn label_pair(&mut self, first: &str, second: &str) -> (Rect, Rect) {
        let gap = f64::from(self.theme.tokens.spacing.s2);
        let mid = f64::midpoint(self.x0, self.right);
        let style = self.theme.text(TextRole::Label);
        let ink = self.theme.tokens.colors.text_dim;
        let width = num::f32_of_f64((mid - gap / 2.0 - self.x0).max(1.0));
        let (_, h) = self.tr.measure_style(first, Some(width), &style);
        let label = Rect::new(self.x0, self.y, self.right, self.y + f64::from(h));
        if self.visible(label) {
            for (text, x) in [(first, self.x0), (second, mid + gap / 2.0)] {
                self.tr.draw_style(
                    self.scene,
                    text,
                    ink,
                    self.transform * Affine::translate((x, self.y)),
                    Some(width),
                    &style,
                );
            }
        }
        self.y += f64::from(h) + f64::from(self.theme.tokens.spacing.rhythm.tight);
        self.pair()
    }

    /// A segmented control the width of the column on its own line;
    /// returns each segment's rect (none while scrolled out of view).
    fn segmented(&mut self, labels: &[String], selected: usize, pointer: Point) -> Vec<Rect> {
        let segments: Vec<ui::widget::Segment> =
            labels.iter().map(ui::widget::Segment::new).collect();
        self.segmented_items(&segments, selected, pointer)
    }

    /// [`Self::segmented`] of segments that may show icons (each named by
    /// its label).
    fn segmented_items(
        &mut self,
        segments: &[ui::widget::Segment],
        selected: usize,
        pointer: Point,
    ) -> Vec<Rect> {
        let h = SegmentedFace::height(ControlSize::Small, self.theme);
        let row = Rect::new(self.x0, self.y, self.right, self.y + h);
        let rects = self.segmented_in(row, segments, selected, pointer);
        self.y += h + f64::from(self.theme.tokens.spacing.rhythm.related);
        rects
    }

    /// A segmented control filling `row`; returns each segment's rect.
    fn segmented_in(
        &mut self,
        row: Rect,
        segments: &[ui::widget::Segment],
        selected: usize,
        pointer: Point,
    ) -> Vec<Rect> {
        if !self.visible(row) || segments.is_empty() {
            return Vec::new();
        }
        let n = num::f64_of_usize(segments.len());
        let seg = row.width() / n;
        let rects: Vec<Rect> = (0..segments.len())
            .map(|i| {
                let i = num::f64_of_usize(i);
                Rect::new(row.x0 + i * seg, row.y0, row.x0 + (i + 1.0) * seg, row.y1)
            })
            .collect();
        let hovered = rects.iter().position(|r| r.contains(pointer));
        ui::widget::ToggleGroup::paint(
            self.scene,
            self.transform,
            self.tr,
            self.theme,
            &SegmentedFace::of_segments(&rects, segments, selected)
                .control_size(ControlSize::Small)
                .hovered(hovered),
        );
        rects
    }

    /// A small secondary button in `rect`; with `toggle`, a toggle that
    /// stands on the selection lift while on.
    fn button(&mut self, rect: Rect, label: &str, toggle: Option<bool>, pointer: Point) {
        if !self.visible(rect) {
            return;
        }
        Button::paint(
            self.scene,
            self.transform,
            self.tr,
            self.theme,
            &ButtonFace::new(rect, label)
                .control_size(ControlSize::Small)
                .toggled(toggle == Some(true))
                .hovered(rect.contains(pointer)),
        );
    }
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

/// The spoken names of a palette's swatches, in palette order.
fn swatch_names(kind: PaletteKind) -> Vec<String> {
    match kind {
        PaletteKind::Text => [
            "color-black",
            "color-gray",
            "color-red",
            "color-blue",
            "color-green",
            "color-amber",
            "color-purple",
        ]
        .into_iter()
        .map(|key| t_args("a11y-text-colour-name", &[arg("colour", t(key))]))
        .collect(),
        PaletteKind::Highlight => [
            "color-yellow",
            "color-green",
            "color-cyan",
            "color-pink",
            "color-orange",
        ]
        .into_iter()
        .map(|key| t_args("a11y-highlight-name", &[arg("colour", t(key))]))
        .collect(),
    }
}

/// One node of the word processor's accessibility tree: an [`ui::a11y::AccessNode`]
/// plus its accessible children.
///
/// The word processor renders its blocks directly (materialized [`BlockView`]s,
/// not child widgets), so [`Widget::accessibility`]'s usual "single local node +
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

impl A11yTreeNode {
    fn leaf(node: ui::a11y::AccessNode) -> Self {
        Self {
            node,
            children: Vec::new(),
        }
    }

    /// Convert this test-friendly carrier into the synthetic subtree the
    /// shared AccessKit/AT-SPI bridge actually exports. Without this explicit
    /// conversion, tests can inspect a rich private tree while the host sees
    /// only the document root.
    fn into_access_node(self) -> ui::a11y::AccessNode {
        self.node.with_children(
            self.children
                .into_iter()
                .map(Self::into_access_node)
                .collect(),
        )
    }
}

/// Stable, locale-independent identity for an immediate-mode toolbar control.
fn tool_action_id(action: ToolAction) -> String {
    match action {
        ToolAction::Bold => "bold".into(),
        ToolAction::Italic => "italic".into(),
        ToolAction::Underline => "underline".into(),
        ToolAction::Strike => "strike".into(),
        ToolAction::Super => "superscript".into(),
        ToolAction::Sub => "subscript".into(),
        ToolAction::Hyperlink => "hyperlink".into(),
        ToolAction::FontCycle => "font-family".into(),
        ToolAction::FontSizeDec => "font-size-decrease".into(),
        ToolAction::FontSizeInc => "font-size-increase".into(),
        ToolAction::InsertImage => "insert-image".into(),
        ToolAction::InsertTable => "insert-table".into(),
        ToolAction::TableRowAdd => "table-row-add".into(),
        ToolAction::TableRowDel => "table-row-delete".into(),
        ToolAction::TableColAdd => "table-column-add".into(),
        ToolAction::TableColDel => "table-column-delete".into(),
        ToolAction::TableDelete => "table-delete".into(),
        ToolAction::ToggleStyles => "toggle-styles".into(),
        ToolAction::ToggleRuler => "toggle-ruler".into(),
        ToolAction::ToggleOutline => "toggle-outline".into(),
        ToolAction::InsertToc => "insert-table-of-contents".into(),
        ToolAction::NewMenu => "new-from-template".into(),
        ToolAction::TextColorPalette => "text-color".into(),
        ToolAction::HighlightPalette => "highlight".into(),
        ToolAction::TextColor(None) => "text-color-default".into(),
        ToolAction::TextColor(Some(rgb)) => {
            format!("text-color-{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
        }
        ToolAction::Highlight(None) => "highlight-clear".into(),
        ToolAction::Highlight(Some(rgb)) => {
            format!("highlight-{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
        }
    }
}

/// A human-readable accessible name for a toolbar [`ToolAction`]. The visible
/// toolbar faces are terse ("B", "x²", an icon) for sighted
/// users; a screen reader needs the spelled-out verb instead.
fn tool_action_a11y_label(a: ToolAction) -> String {
    let key = match a {
        ToolAction::Bold => "a11y-bold",
        ToolAction::Italic => "a11y-italic",
        ToolAction::Underline => "a11y-underline",
        ToolAction::Strike => "a11y-strikethrough",
        ToolAction::Super => "a11y-superscript",
        ToolAction::Sub => "a11y-subscript",
        ToolAction::Hyperlink => "a11y-hyperlink",
        ToolAction::FontCycle => "a11y-font",
        ToolAction::FontSizeDec => "a11y-size-down",
        ToolAction::FontSizeInc => "a11y-size-up",
        ToolAction::InsertImage => "a11y-insert-image",
        ToolAction::InsertTable => "a11y-insert-table",
        ToolAction::TableRowAdd => "a11y-table-row-add",
        ToolAction::TableRowDel => "a11y-table-row-del",
        ToolAction::TableColAdd => "a11y-table-col-add",
        ToolAction::TableColDel => "a11y-table-col-del",
        ToolAction::TableDelete => "a11y-table-delete",
        ToolAction::ToggleStyles => "a11y-styles-panel",
        ToolAction::ToggleRuler => "a11y-ruler",
        ToolAction::ToggleOutline => "a11y-document-outline",
        ToolAction::InsertToc => "a11y-insert-toc",
        ToolAction::NewMenu => "a11y-new-from-template",
        ToolAction::TextColorPalette => "picker-text-color",
        ToolAction::HighlightPalette => "picker-highlight",
        ToolAction::TextColor(c) => {
            return match c {
                Some(rgb) => {
                    let hex = format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                    t_args("a11y-text-color", &[arg("hex", hex)])
                }
                None => t("a11y-text-color-default"),
            };
        }
        ToolAction::Highlight(c) => {
            return match c {
                Some(rgb) => {
                    let hex = format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
                    t_args("a11y-highlight", &[arg("hex", hex)])
                }
                None => t("a11y-highlight-clear"),
            };
        }
    };
    t(key)
}

fn review_action_target(action: &ReviewAction) -> String {
    match action {
        ReviewAction::AcceptChange(id) => format!("word-processor.review:accept:{id}"),
        ReviewAction::RejectChange(id) => format!("word-processor.review:reject:{id}"),
        ReviewAction::AcceptAllChanges => "word-processor.review:accept-all".into(),
        ReviewAction::RejectAllChanges => "word-processor.review:reject-all".into(),
        ReviewAction::ResolveComment(id) => format!("word-processor.review:resolve:{id}"),
        ReviewAction::OpenCommentThread(id) => format!("word-processor.review:reply:{id}"),
    }
}

fn review_action_a11y_label(
    action: &ReviewAction,
    changes: &[tilo_doc::snapshot::ChangeView],
) -> String {
    use tilo_doc::snapshot::ChangeKind;
    match action {
        ReviewAction::AcceptChange(id) => match changes.iter().find(|change| change.id == *id) {
            Some(change) if change.kind == ChangeKind::Del => t("review-accept-deletion"),
            _ => t("review-accept-insertion"),
        },
        ReviewAction::RejectChange(id) => match changes.iter().find(|change| change.id == *id) {
            Some(change) if change.kind == ChangeKind::Del => t("review-reject-deletion"),
            _ => t("review-reject-insertion"),
        },
        ReviewAction::AcceptAllChanges => t("review-accept-all"),
        ReviewAction::RejectAllChanges => t("review-reject-all"),
        ReviewAction::ResolveComment(_) => t("review-resolve-comment"),
        ReviewAction::OpenCommentThread(_) => t("review-reply"),
    }
}

impl WordProcessorWidget {
    /// The 1-based heading level for a heading block (defaults to 1 when the
    /// `level` attr is absent or non-positive).
    fn block_heading_level(b: &BlockView) -> u8 {
        match b.attrs.get("level") {
            Some(AttrVal::Int(n)) if *n >= 1 => num::u8_of_i64((*n).min(255)),
            _ => 1,
        }
    }

    /// The accessible name for an image block: its explicit `alt` attr (set by
    /// the ODT/DOCX importers, round-tripped through `svg:desc`), falling back to
    /// the block's own text (a caption), or `None` when neither is present (a
    /// decorative / unlabelled image).
    fn image_alt(b: &BlockView) -> Option<String> {
        if let Some(AttrVal::Str(alt)) = b.attrs.get("alt") {
            let alt = alt.trim();
            if !alt.is_empty() {
                return Some(alt.to_string());
            }
        }
        let text = b.text.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    }

    /// The accessibility node for a single document block, or `None` for a block
    /// kind that carries no standalone semantics here (table cells are emitted as
    /// children of their table node, not at the top level).
    fn block_access_node(b: &BlockView) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let node = match b.kind.as_str() {
            "heading" => AccessNode::new(Role::Heading {
                level: Self::block_heading_level(b),
            })
            .with_name(b.text.clone()),
            "paragraph" => AccessNode::new(Role::Paragraph).with_value(b.text.clone()),
            "list_item" => AccessNode::new(Role::ListItem).with_value(b.text.clone()),
            "image" => AccessNode::new(Role::Image).with_optional_name(Self::image_alt(b)),
            // Cells are surfaced under their table; standalone they have no role.
            "cell" => return None,
            // Unknown / future block kinds degrade to non-editable static text so
            // their content is still announced rather than dropped.
            _ => AccessNode::new(Role::StaticText).with_value(b.text.clone()),
        };
        Some(node)
    }

    /// Build the toolbar group node + a button node per visible toolbar control.
    /// Returns `None` when the toolbar has not been laid out yet (no frame drawn),
    /// so the document tree never carries an empty toolbar group.
    fn toolbar_access_node(&self) -> Option<A11yTreeNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.toolbar.is_empty() {
            return None;
        }
        let mut buttons: Vec<A11yTreeNode> = self
            .toolbar
            .iter()
            .map(|(action, rect)| {
                let role = if matches!(
                    action,
                    ToolAction::Bold
                        | ToolAction::Italic
                        | ToolAction::Underline
                        | ToolAction::Strike
                        | ToolAction::Super
                        | ToolAction::Sub
                        | ToolAction::ToggleStyles
                        | ToolAction::ToggleRuler
                        | ToolAction::ToggleOutline
                ) {
                    Role::ToggleButton
                } else {
                    Role::Button
                };
                let node = AccessNode::new(role)
                    .with_name(tool_action_a11y_label(*action))
                    .with_bounds(*rect)
                    .with_action_target(format!(
                        "word-processor.toolbar:{}",
                        tool_action_id(*action)
                    ))
                    .with_state(AccessState {
                        focusable: true,
                        checked: (role == Role::ToggleButton).then(|| match action {
                            ToolAction::Bold => self.mark_active_in_selection("strong"),
                            ToolAction::Italic => self.mark_active_in_selection("em"),
                            ToolAction::Underline => self.mark_active_in_selection("underline"),
                            ToolAction::Strike => self.mark_active_in_selection("strike"),
                            ToolAction::Super => self.mark_active_in_selection("super"),
                            ToolAction::Sub => self.mark_active_in_selection("sub"),
                            ToolAction::ToggleStyles => self.view.styles_panel_visible,
                            ToolAction::ToggleRuler => self.view.ruler_visible,
                            ToolAction::ToggleOutline => self.outline.is_open(),
                            _ => false,
                        }),
                        ..Default::default()
                    });
                A11yTreeNode::leaf(node)
            })
            .collect();
        // The zoom field sits in the status band but belongs to the view
        // controls: expose it with them.
        if let Some(node) = Widget::accessibility(&self.zoom_input) {
            buttons.push(A11yTreeNode::leaf(node.with_bounds(self.zoom_rect)));
        }
        Some(A11yTreeNode {
            node: AccessNode::new(Role::Toolbar).with_name(t("a11y-group-formatting")),
            children: buttons,
        })
    }

    /// Expose the currently painted margin-card controls as real semantic
    /// buttons. Their bounds and targets are sourced from the same hit list as
    /// pointer input, so assistive technology never gets a parallel, stale UI.
    fn review_access_node(&self) -> Option<A11yTreeNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if !self.review.is_visible() || self.review.hits().is_empty() {
            return None;
        }
        let buttons = self
            .review
            .hits()
            .iter()
            .map(|(rect, action)| {
                A11yTreeNode::leaf(
                    AccessNode::new(Role::Button)
                        .with_name(review_action_a11y_label(action, self.review.changes()))
                        .with_bounds(*rect)
                        .with_action_target(review_action_target(action))
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        }),
                )
            })
            .collect();
        Some(A11yTreeNode {
            node: AccessNode::new(Role::Group).with_name(t("a11y-group-review")),
            children: buttons,
        })
    }

    /// Expose the selected image's placement choices from the exact hit list
    /// painted by the inspector. The full names/descriptions avoid making the
    /// compact visual labels the only cue for screen-reader and tooltip users.
    fn image_placement_access_node(&self) -> Option<A11yTreeNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let buttons = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| {
                let (id, name, description) = match hit {
                    PanelHit::ImageInline => (
                        "inline",
                        t("a11y-image-inline"),
                        t("a11y-image-inline-description"),
                    ),
                    PanelHit::ImageFreePosition => (
                        "free",
                        t("a11y-image-free-position"),
                        t("a11y-image-free-position-description"),
                    ),
                    PanelHit::ImageWrapLeft => (
                        "left",
                        t("a11y-image-wrap-left"),
                        t("a11y-image-wrap-left-description"),
                    ),
                    PanelHit::ImageWrapRight => (
                        "right",
                        t("a11y-image-wrap-right"),
                        t("a11y-image-wrap-right-description"),
                    ),
                    _ => return None,
                };
                Some(A11yTreeNode::leaf(
                    AccessNode::new(Role::Button)
                        .with_name(name)
                        .with_description(description)
                        .with_bounds(*rect)
                        .with_action_target(format!("word-processor.image-placement:{id}"))
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        }),
                ))
            })
            .collect::<Vec<_>>();
        (!buttons.is_empty()).then(|| A11yTreeNode {
            node: AccessNode::new(Role::Group).with_name(t("a11y-group-image-placement")),
            children: buttons,
        })
    }

    fn panel_hit_identity(&self, hit: &PanelHit) -> (String, String) {
        let (id, label) = match hit {
            PanelHit::Close => ("close".into(), t("panel-close")),
            PanelHit::Pick(id) => (format!("pick:{id}"), self.sheet.get(id).name),
            PanelHit::ParaAlign(value) => {
                (format!("align:{value}"), t(&format!("panel-align-{value}")))
            }
            PanelHit::SetFont(value) => {
                (format!("font:{value}"), t(&format!("panel-font-{value}")))
            }
            PanelHit::ToggleBold => ("bold".into(), t("panel-bold")),
            PanelHit::ToggleItalic => ("italic".into(), t("panel-italic")),
            PanelHit::SetColor(color) => {
                let label = STYLE_PALETTE
                    .iter()
                    .zip(swatch_names(PaletteKind::Text))
                    .find_map(|(candidate, label)| (candidate == color).then_some(label))
                    .unwrap_or_else(|| tool_action_a11y_label(ToolAction::TextColor(Some(*color))));
                let [red, green, blue] = color;
                (format!("color:{red:02x}{green:02x}{blue:02x}"), label)
            }
            PanelHit::OpenColorPicker => ("custom-color".into(), t("panel-colour-custom")),
            PanelHit::NewStyle => ("new-style".into(), t("panel-new-style")),
            PanelHit::ImageScale(factor) => {
                if *factor < 1.0 {
                    ("image-smaller".into(), t("panel-image-smaller"))
                } else {
                    ("image-larger".into(), t("panel-image-larger"))
                }
            }
            PanelHit::ImageFlipH => ("image-flip-h".into(), t("panel-image-flip-h")),
            PanelHit::ImageFlipV => ("image-flip-v".into(), t("panel-image-flip-v")),
            PanelHit::ImageCropInset => ("image-crop-inset".into(), t("panel-image-crop-inset")),
            PanelHit::ImageCropReset => ("image-crop-reset".into(), t("panel-image-crop-reset")),
            PanelHit::ImageInline => ("image-inline".into(), t("a11y-image-inline")),
            PanelHit::ImageFreePosition => ("image-free".into(), t("a11y-image-free-position")),
            PanelHit::ImageWrapLeft => ("image-left".into(), t("a11y-image-wrap-left")),
            PanelHit::ImageWrapRight => ("image-right".into(), t("a11y-image-wrap-right")),
        };
        (format!("word-processor.styles:{id}"), label)
    }

    fn panel_hit_checked(&self, hit: &PanelHit) -> Option<bool> {
        let style = self.focused_style_key().map(|key| self.sheet.get(&key));
        match hit {
            PanelHit::ParaAlign(value) => Some(self.current_align() == *value),
            PanelHit::SetFont(value) => style.map(|style| style.font == *value),
            PanelHit::ToggleBold => style.map(|style| style.bold),
            PanelHit::ToggleItalic => style.map(|style| style.italic),
            PanelHit::SetColor(value) => style.map(|style| style.color == *value),
            _ => None,
        }
    }

    fn visible_styles_rect(&self, rect: Rect) -> Option<Rect> {
        let visible = rect.intersect(self.panel_body);
        (visible.width() > 0.0 && visible.height() > 0.0).then_some(visible)
    }

    /// Project the same painted hits and shared fields the foreground inspector
    /// actually owns. Scrolled-out controls have no actionable UI node.
    fn styles_page_accessibility_tree(&self) -> A11yTreeNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| {
                let close = matches!(hit, PanelHit::Close);
                let bounds = if close {
                    Some(*rect)
                } else {
                    self.visible_styles_rect(*rect)
                }?;
                let (target, label) = self.panel_hit_identity(hit);
                let checked = self.panel_hit_checked(hit);
                let role = if matches!(hit, PanelHit::ToggleBold | PanelHit::ToggleItalic) {
                    Role::ToggleButton
                } else if checked.is_some() {
                    Role::RadioButton
                } else {
                    Role::Button
                };
                Some(A11yTreeNode::leaf(
                    AccessNode::new(role)
                        .with_name(label)
                        .with_bounds(bounds)
                        .with_action_target(target)
                        .with_state(AccessState {
                            focusable: true,
                            disabled: !close && self.read_only(),
                            checked,
                            ..Default::default()
                        }),
                ))
            })
            .collect::<Vec<_>>();
        for slot in &self.num_inputs {
            if let Some(bounds) = self.visible_styles_rect(slot.rect)
                && let Some(mut node) = Widget::accessibility(&slot.input)
            {
                node.state.disabled |= self.read_only();
                children.push(A11yTreeNode::leaf(
                    node.with_bounds(bounds)
                        .with_action_target(slot.field.semantic_target()),
                ));
            }
        }
        for (field, rect, label, target) in [
            (
                &self.line_spacing,
                self.line_spacing_rect,
                "panel-line-spacing",
                "word-processor.styles:line-spacing",
            ),
            (
                &self.style_pick,
                self.style_pick_rect,
                "panel-style",
                "word-processor.styles:style",
            ),
        ] {
            if let Some(bounds) = self.visible_styles_rect(rect)
                && let Some(mut node) = Widget::accessibility(field)
            {
                node.state.disabled |= self.read_only();
                children.push(A11yTreeNode::leaf(
                    node.with_name(t(label))
                        .with_bounds(bounds)
                        .with_action_target(target),
                ));
            }
        }
        let title = if self.selected_image_id().is_some() {
            t("panel-image")
        } else {
            t("toolbar-styles")
        };
        A11yTreeNode {
            node: AccessNode::new(Role::Group)
                .with_name(title)
                .with_bounds(self.panel_rect),
            children,
        }
    }

    fn perform_styles_page_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
    ) -> Option<bool> {
        if !self.styles_panel_full_window() {
            return None;
        }
        let target = key_path.last().copied();
        let hit = self.panel_hits.iter().find_map(|(_, hit)| {
            (target == Some(ui::a11y::hash_key(&self.panel_hit_identity(hit).0)))
                .then(|| hit.clone())
        });
        if let Some(hit) = hit {
            if self.read_only() && !matches!(hit, PanelHit::Close) {
                return Some(false);
            }
            return Some(match action {
                ui::a11y::Action::Focus => {
                    self.set_focus(true);
                    true
                }
                ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck => {
                    self.apply_panel_hit(state, hit);
                    true
                }
                _ => false,
            });
        }
        Some(self.perform_styles_field_action(target, action, state))
    }

    fn perform_styles_field_action(
        &mut self,
        target: Option<u64>,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.read_only() {
            return false;
        }
        if matches!(action, ui::a11y::Action::Focus) {
            self.set_focus(true);
        }
        let field = self.num_inputs.iter().find_map(|slot| {
            (target == Some(ui::a11y::hash_key(&slot.field.semantic_target())))
                .then_some(slot.field)
        });
        let performed = if let Some(field) = field {
            if matches!(action, ui::a11y::Action::Focus) {
                self.line_spacing.close();
                self.style_pick.close();
                Widget::set_focus(&mut self.line_spacing, false);
                Widget::set_focus(&mut self.style_pick, false);
                self.focus_hosted(Some(field), false);
                true
            } else {
                let Some(slot) = self.num_inputs.iter_mut().find(|slot| slot.field == field) else {
                    return false;
                };
                Widget::perform_action(&mut slot.input, action, state, &automerge::ROOT)
            }
        } else {
            if matches!(action, ui::a11y::Action::Focus) {
                self.focus_hosted(None, false);
                Widget::set_focus(&mut self.line_spacing, false);
                Widget::set_focus(&mut self.style_pick, false);
            }
            let menu = if target == Some(ui::a11y::hash_key(&"word-processor.styles:line-spacing"))
            {
                &mut self.line_spacing
            } else if target == Some(ui::a11y::hash_key(&"word-processor.styles:style")) {
                &mut self.style_pick
            } else {
                return false;
            };
            if matches!(action, ui::a11y::Action::Focus) {
                Widget::set_focus(menu, true);
                true
            } else {
                Widget::perform_action(menu, action, state, &automerge::ROOT)
            }
        };
        if performed {
            self.drain_hosted_fields(state);
            self.lifecycle.dirty = true;
        }
        performed
    }

    /// Assemble the full accessibility node tree: a `Role::Document` root holding
    /// one node per block (headings with level, paragraphs/list items with text,
    /// images with alt text, tables as `Role::Table`/`Role::Cell`), followed by
    /// a `Role::Toolbar` group of formatting buttons. [`Widget::accessibility`]
    /// returns this tree's root so the host's per-widget path surfaces the document.
    pub fn accessibility_tree(&self) -> A11yTreeNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        if self.styles_panel_full_window() {
            return self.styles_page_accessibility_tree();
        }
        let mut children: Vec<A11yTreeNode> = Vec::new();
        let mut i = 0usize;
        while let Some(b) = self.blocks.get(i) {
            if b.kind == "cell" {
                // Coalesce a run of consecutive cells sharing a `table` id into a
                // single Table node with the cells as Cell children.
                let table_id = cell_table(b);
                let mut cells: Vec<A11yTreeNode> = Vec::new();
                while let Some(c) = self.blocks.get(i) {
                    if c.kind != "cell" || cell_table(c) != table_id {
                        break;
                    }
                    cells.push(A11yTreeNode::leaf(
                        AccessNode::new(Role::Cell).with_value(c.text.clone()),
                    ));
                    i += 1;
                }
                children.push(A11yTreeNode {
                    node: AccessNode::new(Role::Table),
                    children: cells,
                });
                continue;
            }
            if let Some(node) = Self::block_access_node(b) {
                children.push(A11yTreeNode::leaf(node));
            }
            i += 1;
        }

        if let Some(toolbar) = self.toolbar_access_node() {
            children.push(toolbar);
        }
        if let Some(review) = self.review_access_node() {
            children.push(review);
        }
        if let Some(image_placement) = self.image_placement_access_node() {
            children.push(image_placement);
        }
        if let Some(palette) = self.compact_swatch_palette.accessibility() {
            children.push(A11yTreeNode::leaf(palette));
        }
        if let Some(find) = self.find.accessibility() {
            children.push(A11yTreeNode::leaf(find.with_bounds(self.find_band)));
        }

        A11yTreeNode {
            node: AccessNode::new(Role::Document).with_state(AccessState {
                focusable: true,
                focused: self.lifecycle.focused,
                multiline: true,
                ..Default::default()
            }),
            children,
        }
    }
}

/// Frame-level geometry resolved once per frame by
/// [`WordProcessorWidget::draw_frame_chrome`] and threaded through `draw`'s
/// other per-pass helpers below — split out so `draw` itself stays under the
/// line-count limit.
/// The canvas clip as a vertical band in the coordinates of a laid text item
/// whose top is at `ly` (before the canvas transform), with a margin for
/// decorations drawn just outside their line box. Decorations of lines outside
/// the band cannot reach the clip. A degenerate transform yields NaN bounds,
/// which cull nothing.
fn layout_clip_band(fg: &FrameGeometry, ly: f64) -> (f32, f32) {
    let local = fg.ctf.inverse().transform_rect_bbox(fg.canvas_clip);
    ((local.y0 - ly - 8.0) as f32, (local.y1 - ly + 8.0) as f32)
}

/// The spans that can intersect the visible source range (see
/// `RichTextLayout::source_range_within`); the others have no rectangle in
/// the band. Order is preserved.
fn spans_within(spans: &[(usize, usize)], visible: Option<(usize, usize)>) -> Vec<(usize, usize)> {
    match visible {
        Some((start, end)) => spans
            .iter()
            .copied()
            .filter(|&(span_start, span_end)| span_end >= start && span_start <= end)
            .collect(),
        None => Vec::new(),
    }
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

/// The top-left content y for page `i`, in unscaled content space.
fn page_content_top(fg: &FrameGeometry, i: usize) -> f64 {
    fg.page_top + num::f64_of_usize(i) * fg.page_pitch + fg.m.top
}

/// The bottom text-margin y for page `i`, in unscaled content space.
fn page_content_bottom(fg: &FrameGeometry, i: usize) -> f64 {
    fg.page_top + num::f64_of_usize(i) * fg.page_pitch + fg.letter_h - fg.m.bottom
}

/// Map estimated continuous body flow into the page stack, including margins
/// and inter-page gaps. Window selection and initial pagination must use the
/// same coordinate system or their error grows with every preceding page.
fn flow_to_page_stack(flow: f64, letter_h: f64, margins: Edges) -> f64 {
    let band_h = (letter_h - margins.top - margins.bottom).max(1.0);
    let flow = flow.max(0.0);
    (flow / band_h).floor() * (letter_h + PAGE_GAP) + margins.top + flow % band_h
}

/// Page margins and gaps map to the nearest body-flow boundary.
fn page_stack_to_flow(stack: f64, letter_h: f64, margins: Edges) -> f64 {
    let band_h = (letter_h - margins.top - margins.bottom).max(1.0);
    let pitch = letter_h + PAGE_GAP;
    let stack = stack.max(0.0);
    (stack / pitch).floor() * band_h + (stack % pitch - margins.top).clamp(0.0, band_h)
}

fn responsive_page_geometry(
    canvas_left: f64,
    canvas_right: f64,
    text_scale: f64,
    margins: Edges,
    mobile: bool,
    accessible: bool,
) -> (f64, Edges) {
    let page_w = if accessible {
        ((canvas_right - canvas_left - 24.0) / text_scale).max(120.0)
    } else if mobile {
        (canvas_right - canvas_left - 24.0).max(240.0)
    } else {
        MAX_PAGE_W
    };
    if !mobile && !accessible {
        return (page_w, margins);
    }
    let scale = (page_w / MAX_PAGE_W).min(1.0);
    (
        page_w,
        Edges {
            top: margins.top * scale,
            right: margins.right * scale,
            bottom: margins.bottom * scale,
            left: margins.left * scale,
        },
    )
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

impl WordProcessorWidget {
    /// Chrome bands above the canvas — toolbar, Find bar,
    /// ruler — plus the canvas/page geometry they reserve around. The first
    /// pass of [`Self::draw`].
    fn draw_frame_chrome(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) -> FrameGeometry {
        let tb_h = Self::toolbar_band_height(theme);
        self.layout_bar_height = Self::status_band_height(theme);
        let below = Rect::new(
            full.x0,
            full.y0 + tb_h,
            full.x1,
            (full.y1 - self.layout_bar_height).max(full.y0 + tb_h),
        );
        let column = self.resolve_drawers(theme, below);
        let available = column.width();
        let mobile_page = available < 600.0;
        let accessible_page = self.text_scale > 1.01;
        let page_right = self.page_area_right(theme, column, mobile_page || accessible_page);
        if self.view.auto_fit_page {
            // Phone-sized tiles use a responsive editing sheet instead of
            // shrinking an entire Letter page until its body copy is 7 px.
            // Wider canvases retain print-faithful page sizing and auto-fit.
            let fit = if accessible_page {
                self.text_scale
            } else if mobile_page {
                1.0
            } else {
                ((page_right - column.x0 - 32.0) / MAX_PAGE_W).clamp(ZOOM_MIN, 1.0)
            };
            self.zoom.set(fit);
        }
        let tb = Rect::new(full.x0, full.y0, full.x1, full.y0 + tb_h);
        self.tool_band = tb;
        self.draw_toolbar(scene, transform, text_renderer, theme, tb);

        // No bottom status strip: its readout floats as a transient component,
        // so the content spans down to the layout bar and
        // reclaims the strip's height. The layout bar now sits at the very bottom.
        let content_bottom = full.y1.max(full.y0 + tb_h);
        self.layout_bar_height = Self::status_band_height(theme);
        let layout_bar_top = (content_bottom - self.layout_bar_height).max(full.y0 + tb_h);
        // The page's column: what the docked outline and inspector leave.
        let canvas_left = column.x0;
        let canvas_right = column.x1.max(canvas_left + 1.0);
        // Reserve + draw the crash-recovery banner (when staged) and the Find
        // bar (when open), stacked directly under the toolbar. The find bar
        // is measured against the canvas width it will span first, so the
        // band it reserves (one row, or two when the controls would run off
        // the window) is the band it paints.
        let find_h = self.draw_find_bar(
            scene,
            transform,
            text_renderer,
            theme,
            &UtilityBarsGeometry {
                left: canvas_left,
                right: canvas_right,
                top: full.y0 + tb_h,
            },
        );
        // Reserve the horizontal ruler band (when on) out of the canvas top, below
        // the find bar. It is drawn with the *unscaled* `transform` so it stays a
        // fixed-height strip while the page scrolls/zooms; its tick + marker
        // positions are mapped through the zoom so they track the page edge.
        let ruler_h = if self.view.ruler_visible {
            Ruler::band_height(theme, text_renderer)
        } else {
            0.0
        };
        let canvas = Rect::new(
            canvas_left,
            full.y0 + tb_h + find_h + ruler_h,
            canvas_right,
            layout_bar_top,
        );
        self.zoom_viewport = canvas;

        // Zoom anchor: the canvas top, horizontally centered. The page+content
        // transform scales about this point, so the page stays centered and the
        // top of the (scrolled) content stays pinned to the canvas top as zoom
        // changes. The toolbar/find-bar/status bar are drawn with the unscaled
        // `transform` and are unaffected.
        self.zoom
            .set_anchor(Point::new(f64::midpoint(canvas.x0, page_right), canvas.y0));
        let ctf = self.content_transform(transform);

        // Per-side page geometry — every margin is independently customizable.
        // The page is a FIXED size (it does not resize with the window); the
        // window just shows more or less of it and centers it.
        let (page_w, m) = responsive_page_geometry(
            canvas_left,
            canvas_right,
            self.text_scale,
            self.margins,
            mobile_page,
            accessible_page,
        );
        // Centre in *content* space even when the page is wider than the
        // unscaled canvas. Scaling about the canvas centre then keeps both page
        // edges visible in auto-fit mode instead of pinning the left edge and
        // clipping the right.
        let page_x = f64::midpoint(canvas.x0, page_right) - page_w / 2.0;
        let page_top = canvas.y0 + PAGE_TOP;
        let content_x = page_x + m.left;
        let content_w = num::f32_of_f64((page_w - m.left - m.right).max(80.0));

        // Horizontal ruler band (drawn unscaled, above the canvas). Reads the
        // page edge geometry just computed, plus the caret paragraph's indent +
        // tab stops from the cached block views.
        if ruler_h > 0.0 {
            let band = Rect::new(
                canvas_left,
                full.y0 + tb_h + find_h,
                canvas_right,
                canvas.y0,
            );
            self.draw_ruler(scene, transform, text_renderer, theme, band, page_x, page_w);
        } else {
            self.ruler.clear();
        }
        // True pagination: each page is a fixed US-Letter sheet (height `letter_h`)
        // stacked with `PAGE_GAP` between sheets. A block lays into the current
        // page's text band [content_top, content_bottom]; when the *next whole
        // block* would not fit before the bottom text margin, the page advances and
        // the block starts at the next page's top margin — content is never split,
        // so a page-end never crosses a line of text.
        let letter_h = page_w * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        FrameGeometry {
            canvas,
            canvas_clip: transform.transform_rect_bbox(canvas),
            canvas_left,
            canvas_right,
            layout_bar_top,
            content_bottom,
            ctf,
            page_x,
            page_top,
            page_w,
            content_x,
            content_w,
            m,
            letter_h,
            page_pitch,
        }
    }

    /// The trailing edge of the area the page centres in. Review cards stand
    /// in a column beside the page: when a page centred in the whole
    /// `column` would leave too little room for them on its trailing side,
    /// the page moves over (and, fitted, shrinks) so the cards never cover
    /// the text. A phone-sized or large-text sheet keeps the whole width.
    fn page_area_right(&self, theme: &Theme, column: Rect, responsive: bool) -> f64 {
        if responsive || !self.review.shows_cards() {
            return column.x1;
        }
        let reserve = ReviewOverlay::column_reserve(theme);
        let zoom = if self.view.auto_fit_page {
            ((column.width() - 32.0) / MAX_PAGE_W).clamp(ZOOM_MIN, 1.0)
        } else {
            self.zoom.factor()
        };
        let beside = (column.width() - MAX_PAGE_W * zoom) / 2.0;
        if beside >= reserve {
            column.x1
        } else {
            (column.x1 - reserve).max(column.x0 + 1.0)
        }
    }

    /// Draw the shared Find bar and any failed-save banner in the document flow.
    /// Return the height reserved below the toolbar.
    fn draw_find_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        geometry: &UtilityBarsGeometry,
    ) -> f64 {
        let UtilityBarsGeometry {
            left: canvas_left,
            right: canvas_right,
            top: tb_bottom,
        } = *geometry;
        let mut utility_h = 0.0;
        // A failed save stays in view, in the flow above the page, until it
        // is resolved: an error banner, not a toast over the text.
        if self.save_error.is_some() {
            let sp = &theme.tokens.spacing;
            let (across, along) = (f64::from(sp.s3), f64::from(sp.s2));
            let title = t("toast-save-failed");
            let content = ui::widget::BannerContent::new(ui::widget::BannerKind::Error, &title)
                .dismissible(true);
            let width = (canvas_right - canvas_left - 2.0 * across).max(1.0);
            let h = ui::widget::Banner::height(&content, width, text_renderer, theme);
            let top = tb_bottom + utility_h + along;
            let rect = Rect::new(canvas_left + across, top, canvas_right - across, top + h);
            let hit = ui::widget::Banner::paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                &content,
                self.pointer,
            );
            self.save_error = Some(hit.dismiss.unwrap_or(Rect::ZERO));
            utility_h += h + 2.0 * along;
        }
        let find_h = self.find.height(theme);
        self.find_band = Rect::new(
            canvas_left,
            tb_bottom + utility_h,
            canvas_right,
            tb_bottom + utility_h + find_h,
        );
        if find_h > 0.0 {
            let presence = PresenceCtx::empty();
            presence.viewport.set(Some(self.rect));
            self.find.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &presence,
                self.find_band,
            );
        }
        utility_h + find_h
    }

    /// Pass 1 of [`Self::draw`]: shape blocks top-to-bottom and paginate them
    /// across discrete page sheets. Runs of consecutive `cell` blocks sharing a
    /// table id lay out as a grid; everything else stacks normally.
    fn paginate_blocks(
        &mut self,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
    ) -> PaginationResult {
        let virtualized = self.body_order.len() > VIRTUALIZE_BODY_AFTER;
        let mut blocks = std::mem::take(&mut self.blocks);
        // An input-method composition is shaped inside its paragraph, so the
        // text after the caret moves aside and wraps with it. The block is
        // swapped for its composed view only while it is laid out.
        let composing = self.compose_caret_block(&mut blocks);
        let mut laid: Vec<Laid> = Vec::with_capacity(blocks.len());
        // Keep the current layouts separate from a bounded recently visited
        // window. Returning to it can reuse shaping, while the canonical key
        // below still checks text, marks, style and wrapping before reuse.
        let mut old_cache = std::mem::take(&mut self.layout_cache);
        let previous_current: std::collections::HashSet<String> = if virtualized {
            old_cache.keys().cloned().collect()
        } else {
            std::collections::HashSet::new()
        };
        for (id, layout) in std::mem::take(&mut self.recent_layout_cache) {
            old_cache.entry(id).or_insert(layout);
        }
        let mut new_cache: HashMap<String, (u64, RichTextLayout)> =
            HashMap::with_capacity(blocks.len());
        self.dbg_layout_hits = 0;
        self.dbg_layout_misses = 0;
        let cell_pad = 8.0f64;
        // Pagination cursor: current page index, whether anything has landed on
        // it yet (the first item on a page never breaks, so an over-tall block
        // simply overflows its own page — acceptable for v1), the running
        // content y, the currently-active floated image (subsequent paragraphs
        // on its page are narrowed to flow beside it — coarse wrap, see
        // `FloatReserve`), and the per-level ordered-list counters.
        let band_h = (page_content_bottom(fg, 0) - page_content_top(fg, 0)).max(1.0);
        let estimated_before = if virtualized {
            num::f64_of_usize(self.body_window_start) * ESTIMATED_BLOCK_EXTENT
        } else {
            0.0
        };
        let initial_page = num::usize_of_f64((estimated_before / band_h).floor());
        let initial_y = fg.page_top + flow_to_page_stack(estimated_before, fg.letter_h, fg.m);
        let mut cur = PageCursor {
            cur_page: initial_page,
            max_object_page: 0,
            placed_on_page: self.body_window_start > 0,
            y: initial_y,
            active_float: None,
            ol_counts: Vec::new(),
        };
        let mut bi = 0;
        while let Some(b) = blocks.get(bi) {
            // Coarse-wrap only narrows *paragraph* blocks beside a float. A table or
            // another image cannot share the float column, so before laying one out
            // push the cursor below the active float and retire it — they stack under
            // the image as a full-width block rather than overlapping it.
            let next_is_paragraph = !b.kind.eq("image") && cell_table(b).is_none();
            if !next_is_paragraph
                && let Some(f) = cur.active_float.take()
                && f.page == cur.cur_page
            {
                cur.y = cur
                    .y
                    .max(f.bottom + f64::from(self.sheet.get("body").space_after));
            }
            if cell_table(b).is_some() {
                let mut table_layout = TableLayoutContext {
                    cell_padding: cell_pad,
                    text_renderer,
                    old_cache: &mut old_cache,
                    new_cache: &mut new_cache,
                };
                bi = self.paginate_table(&blocks, bi, fg, &mut table_layout, &mut laid, &mut cur);
            } else if b.kind == "image" {
                self.paginate_image(b, fg, text_renderer, &mut new_cache, &mut laid, &mut cur);
                bi += 1;
            } else {
                let mut paragraph = ParagraphPaginationContext {
                    text_renderer,
                    old_cache: &mut old_cache,
                    new_cache: &mut new_cache,
                    laid: &mut laid,
                    cursor: &mut cur,
                };
                self.paginate_paragraph(b, fg, &mut paragraph);
                bi += 1;
            }
        }

        // A virtual window only shapes the viewport band. Its tail still needs
        // a stable estimated page extent for scrollbar geometry, but it must not
        // cause a full-document render or cache allocation.
        let estimated_last_page = if virtualized {
            num::usize_of_f64(
                ((num::f64_of_usize(self.body_order.len()) * ESTIMATED_BLOCK_EXTENT) / band_h)
                    .floor(),
            )
        } else {
            0
        };
        if virtualized {
            let mut unused: Vec<_> = old_cache
                .into_iter()
                .filter(|(id, _)| !new_cache.contains_key(id))
                .collect();
            // Prefer the immediately previous viewport over older history.
            unused.sort_by_key(|(id, _)| !previous_current.contains(id));
            self.recent_layout_cache = unused.into_iter().take(VIEWPORT_BLOCK_WINDOW).collect();
        }
        if let Some(composing) = composing {
            Self::restore_composed_block(&mut laid, &mut blocks, composing);
        }
        PaginationResult {
            laid,
            cache: new_cache,
            cur_page: cur
                .cur_page
                .max(cur.max_object_page)
                .max(estimated_last_page),
            y: cur.y,
            blocks,
        }
    }

    /// Swap the caret block in `blocks` for its view with the input method's
    /// composition laid in, returning its index, the committed block and the
    /// composition; `None` (and `blocks` untouched) while nothing composes.
    fn compose_caret_block(
        &self,
        blocks: &mut [BlockView],
    ) -> Option<(usize, BlockView, EditCore)> {
        if self.inner.in_running_region() {
            return None;
        }
        let index = blocks.iter().position(|b| b.id == self.inner.caret.block)?;
        let block = blocks.get_mut(index)?;
        let core = self.inner.composition(&block.text)?;
        let composed = composed_block(block, &core);
        Some((index, std::mem::replace(block, composed), core))
    }

    /// Put the committed block taken by [`Self::compose_caret_block`] back,
    /// and record on its laid slices the committed text and the composition
    /// their shaped layout shows.
    fn restore_composed_block(
        laid: &mut [Laid],
        blocks: &mut [BlockView],
        (index, committed, core): (usize, BlockView, EditCore),
    ) {
        for l in laid.iter_mut().filter(|l| l.id == committed.id) {
            l.text.clone_from(&committed.text);
            l.composition = Some(core.clone());
        }
        if let Some(block) = blocks.get_mut(index) {
            *block = committed;
        }
    }

    /// One table (a run of consecutive `cell` blocks sharing a table id):
    /// measure every row first (so the whole table paginates as one unit,
    /// never split mid-row across a page boundary), then lay out row by row,
    /// breaking to a fresh page when a row would cross the bottom margin — the
    /// first row on a page never breaks, so an over-tall single row overflows
    /// its page (acceptable for v1). Returns the block index just past the
    /// table's last cell.
    fn paginate_table(
        &mut self,
        blocks: &[BlockView],
        bi: usize,
        fg: &FrameGeometry,
        layout: &mut TableLayoutContext<'_>,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
    ) -> usize {
        let Some(tid) = blocks.get(bi).and_then(cell_table) else {
            // The caller only calls this when `cell_table(b).is_some()`; if
            // that ever changes (or `bi` is out of range), skip this one
            // block rather than losing the pagination cursor.
            return bi + 1;
        };
        let mut bi = bi;
        cur.ol_counts.clear();
        let start = bi;
        while blocks.get(bi).and_then(cell_table).as_deref() == Some(tid.as_str()) {
            bi += 1;
        }
        let cells = blocks.get(start..bi).unwrap_or(&[]);
        let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
        let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
        let col_w = f64::from(fg.content_w) / num::f64_of_usize(ncols);
        // Measure every row first so the whole table can paginate as one
        // unit (it is never split mid-row across a page boundary).
        let rows = self.measure_table_rows(cells, ncols, nrows, col_w, layout);
        let table_h: f64 = rows.iter().map(|(h, _)| *h).sum();
        // Break to the next page if a manual page break is set on the
        // first cell, or the whole table would not fit.
        let force_break = cells.first().is_some_and(block_page_break_before);
        let item_top = if cur.placed_on_page
            && (force_break || cur.y + table_h > page_content_bottom(fg, cur.cur_page) + 0.5)
        {
            cur.cur_page += 1;
            page_content_top(fg, cur.cur_page)
        } else {
            cur.y
        };
        cur.placed_on_page = true;
        let mut ry = item_top;
        // Whether the current page already carries a row of this table —
        // the first row on a page never breaks (so an over-tall single row
        // overflows its page, acceptable for v1).
        let mut row_on_page = false;
        for (r, (row_h, row_items)) in rows.into_iter().enumerate() {
            // Split the table at a row boundary: if this whole row would
            // cross the page's bottom margin, move it to the next page's top
            // margin (rows that fit stay on this page). No header repeat.
            if row_on_page && ry + row_h > page_content_bottom(fg, cur.cur_page) + 0.5 {
                cur.cur_page += 1;
                ry = page_content_top(fg, cur.cur_page);
            }
            row_on_page = true;
            for (c, cell_h, cell) in row_items {
                let cell_x = fg.content_x + num::f64_of_usize(c) * col_w;
                let cbox = Rect::new(cell_x, ry, cell_x + col_w, ry + row_h);
                laid.push(Laid {
                    id: cell.id.clone(),
                    text: cell.text.clone(),
                    kind: "cell".into(),
                    size: 16.0,
                    y_top: ry + layout.cell_padding,
                    text_x: cell_x + layout.cell_padding,
                    width: col_w - 2.0 * layout.cell_padding,
                    height: cell_h,
                    layout_y: ry + layout.cell_padding,
                    clip: None,
                    bullet: None,
                    ink: PAPER_INK,
                    image_hash: None,
                    cell_box: Some(cbox),
                    cell_row: Some(r),
                    page: cur.cur_page,
                    composition: None,
                });
            }
            ry += row_h;
        }
        cur.y = ry + f64::from(self.sheet.get("body").space_after);
        bi
    }

    /// Measure every row of a table before laying it out, so the whole table
    /// can paginate as one unit (it is never split mid-row across a page
    /// boundary): the shaped (or cache-reused) layout for each populated
    /// cell, and each row's height (the tallest cell, or a minimum).
    fn measure_table_rows<'a>(
        &mut self,
        cells: &'a [BlockView],
        ncols: usize,
        nrows: usize,
        col_w: f64,
        layout: &mut TableLayoutContext<'_>,
    ) -> Vec<TableRow<'a>> {
        let mut rows: Vec<TableRow<'a>> = Vec::with_capacity(nrows);
        for r in 0..nrows {
            let mut row_items: Vec<(usize, f64, &BlockView)> = Vec::new();
            let mut row_h = 26.0f64;
            for c in 0..ncols {
                if let Some(cell) = cells.iter().find(|cl| cell_rc(cl) == (r, c)) {
                    let (runs, underlines, strikes) =
                        block_runs_markup(cell, self.review.is_visible());
                    let wrap_w = num::f32_of_f64((col_w - 2.0 * layout.cell_padding).max(24.0));
                    let align = block_align(cell, TextAlign::Left);
                    let key = shape_key(&ShapeKeyInput {
                        spans: &runs,
                        underlines: &underlines,
                        strikes: &strikes,
                        base_bold: false,
                        size: 16.0,
                        wrap_w,
                        align,
                        family: FontFamily::Sans,
                        line_height_mult: 1.0,
                    });
                    let cell_layout = match layout.old_cache.remove(&cell.id) {
                        Some((k, lay)) if k == key => {
                            self.dbg_layout_hits += 1;
                            lay
                        }
                        _ => {
                            self.dbg_layout_misses += 1;
                            RichTextLayout::shape_styled(
                                &mut layout.text_renderer.font_system,
                                &runs,
                                &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
                                Some(wrap_w),
                                align,
                                underlines,
                                strikes,
                            )
                        }
                    };
                    let cell_h = f64::from(cell_layout.height());
                    row_h = row_h.max(cell_h + 2.0 * layout.cell_padding);
                    layout.new_cache.insert(cell.id.clone(), (key, cell_layout));
                    row_items.push((c, cell_h, cell));
                }
            }
            rows.push((row_h, row_items));
        }
        rows
    }

    /// Resolve one image block's decoded intrinsic size and the on-screen
    /// display size to draw it at: an explicit `width`/`height` attr (set by
    /// importers or a resize handle) wins; otherwise the decoded intrinsic
    /// size is fit to the content column. Either way the final width is
    /// clamped to the content column and the height follows the chosen
    /// aspect ratio, so a requested-but-too-wide image scales down without
    /// distorting. Also seeds `new_cache` with the block's trivial empty
    /// layout (used only so pointer hit-testing resolves to offset 0; cheap
    /// enough to re-shape every frame rather than cache incrementally).
    fn resolve_image_display_size(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
    ) -> ResolvedImage {
        let hash = match b.attrs.get("blob") {
            Some(AttrVal::Str(s)) => s.clone(),
            _ => String::new(),
        };
        if !hash.is_empty() && !self.image_cache.contains_key(&hash) {
            let decoded = self
                .blobs
                .get(&hash)
                .and_then(|bytes| ui::widget::Image::from_encoded(bytes).ok());
            self.image_cache.insert(hash.clone(), decoded);
        }
        let intrinsic = self
            .image_cache
            .get(&hash)
            .and_then(|o| o.as_ref())
            .map(ui::widget::Image::intrinsic_size);
        let (draw_w, draw_h) = match image_explicit_size(b) {
            Some((ew, eh)) if ew > 0.0 && eh > 0.0 => {
                let w = ew.min(f64::from(fg.content_w));
                (w, (eh * (w / ew)).max(8.0))
            }
            _ => match intrinsic {
                Some((iw, ih)) if iw > 0 && ih > 0 => {
                    let w = f64::from(iw).min(f64::from(fg.content_w));
                    (w, (f64::from(ih) * (w / f64::from(iw))).max(8.0))
                }
                _ => (f64::from(fg.content_w).min(280.0), 90.0),
            },
        };
        // Image blocks carry a trivial empty layout (used only so pointer
        // hit-testing resolves to offset 0); it is cheap, so just re-shape
        // it every frame rather than caching it incrementally.
        let empty = RichTextLayout::shape_styled(
            &mut text_renderer.font_system,
            &[RichSpan::new("")],
            &RichTextLayout::block_style(false, 16.0, FontFamily::Sans, 1.0),
            Some(fg.content_w),
            TextAlign::Left,
            vec![],
            vec![],
        );
        new_cache.insert(b.id.clone(), (0, empty));
        ResolvedImage {
            hash,
            intrinsic,
            width: draw_w,
            height: draw_h,
        }
    }

    fn push_laid_image(
        laid: &mut Vec<Laid>,
        block: &BlockView,
        image: &ResolvedImage,
        placement: &ImagePlacement,
    ) {
        laid.push(Laid {
            id: block.id.clone(),
            text: String::new(),
            kind: "image".into(),
            size: 16.0,
            y_top: placement.y,
            text_x: placement.x,
            width: image.width,
            height: image.height,
            layout_y: placement.y,
            clip: None,
            bullet: None,
            ink: PAPER_INK,
            image_hash: image.intrinsic.map(|_| image.hash.clone()),
            cell_box: None,
            cell_row: None,
            page: placement.page,
            composition: None,
        });
    }

    /// One image block: resolve its decoded size, paginate it (a floated
    /// image reserves a column beside it instead of a full-width slot; see
    /// `FloatReserve`), and push its `Laid` entry.
    fn paginate_image(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
    ) {
        cur.ol_counts.clear();
        let image = self.resolve_image_display_size(b, fg, text_renderer, new_cache);
        // A user-positioned page object is intentionally out of normal flow:
        // text retains its natural cursor while the object is painted at its
        // durable page-relative coordinates. Imported `anchor=page` objects
        // without those coordinates keep the legacy centred-flow treatment
        // below, avoiding a layout change merely from opening an old file.
        let transient_position = self
            .object_drag
            .as_ref()
            .and_then(|drag| (drag.id == b.id).then_some((drag.x, drag.y, drag.page)));
        if image_anchor(b) == "page"
            && let Some((offset_x, offset_y, requested_page)) =
                transient_position.or_else(|| image_free_position(b))
        {
            let page = requested_page;
            let max_x = (f64::from(fg.content_w) - image.width).max(0.0);
            let max_y = (fg.letter_h - fg.m.top - fg.m.bottom - image.height).max(0.0);
            let img_x = fg.content_x + offset_x.clamp(0.0, max_x);
            let img_y = page_content_top(fg, page) + offset_y.clamp(0.0, max_y);
            Self::push_laid_image(
                laid,
                b,
                &image,
                &ImagePlacement {
                    x: img_x,
                    y: img_y,
                    page,
                },
            );
            // Keep enough sheets alive to show a deliberately later placement,
            // but leave `y` untouched so a free object cannot create phantom
            // blank paragraphs in the document flow.
            cur.max_object_page = cur.max_object_page.max(page);
            return;
        }
        // Break to the next page on a manual page break, or if the image
        // would not fit.
        let force_break = block_page_break_before(b);
        let item_top = if cur.placed_on_page
            && (force_break || cur.y + image.height > page_content_bottom(fg, cur.cur_page) + 0.5)
        {
            cur.cur_page += 1;
            page_content_top(fg, cur.cur_page)
        } else {
            cur.y
        };
        cur.placed_on_page = true;
        if image_floats(b) {
            // Floated image (anchor=paragraph/page, wrap=left/right): instead
            // of a full-width vertical slot, pin it to the left or right edge
            // of the content column and reserve that column so following
            // paragraphs flow beside it (coarse wrap — `FloatReserve`). `cur.y`
            // does NOT advance past the image: the next blocks start level
            // with its top and are narrowed until they clear its bottom.
            let left = image_wrap(b) == "left";
            let img_x = if left {
                fg.content_x
            } else {
                fg.content_x + f64::from(fg.content_w) - image.width
            };
            Self::push_laid_image(
                laid,
                b,
                &image,
                &ImagePlacement {
                    x: img_x,
                    y: item_top,
                    page: cur.cur_page,
                },
            );
            cur.active_float = Some(FloatReserve {
                left,
                reserve_w: image.width + FLOAT_GUTTER,
                bottom: item_top + image.height,
                page: cur.cur_page,
            });
            // Leave `cur.y` at the image top so following text wraps beside it.
            cur.y = item_top;
        } else {
            // Inline (default), or anchored with wrap none/both: a full-width
            // vertical slot. A non-inline-anchored image with no one-sided
            // wrap is centered in the column; inline images stay left-aligned.
            let centered = image_anchor(b) != "inline";
            let img_x = if centered {
                fg.content_x + ((f64::from(fg.content_w) - image.width) / 2.0).max(0.0)
            } else {
                fg.content_x
            };
            Self::push_laid_image(
                laid,
                b,
                &image,
                &ImagePlacement {
                    x: img_x,
                    y: item_top,
                    page: cur.cur_page,
                },
            );
            cur.y = item_top + image.height + f64::from(self.sheet.get("body").space_after);
        }
    }

    /// Resolve one paragraph-like block's style, list indent/bullet, coarse
    /// float-wrap reserve, and shaped (or cache-reused) layout — everything
    /// [`Self::paginate_paragraph`] needs before it can decide where the
    /// block lands.
    fn resolve_paragraph_shape(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        old_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        cur: &mut PageCursor,
    ) -> ParagraphShape {
        // Resolve the block's style (component-instance model). `size`
        // and `base_bold` are the style default; per-run `size` marks
        // override them in `block_runs`.
        let st = self.sheet.get(&block_style_key(b));
        let (size, base_bold) = (st.size, st.bold);
        // List items indent by nesting level; the bullet/number sits just
        // left of the text. Ordered items number per level.
        let (indent, bullet) = if b.kind == "list_item" {
            let level = list_level(b);
            let indent = f64::from(st.indent) + num::f64_of_usize(level) * LIST_INDENT_STEP;
            // Keep the ordered counters sized to the current depth; entering
            // a shallower level drops the deeper counters so they restart.
            cur.ol_counts.truncate(level + 1);
            if cur.ol_counts.len() <= level {
                cur.ol_counts.resize(level + 1, 0);
            }
            let bullet = if matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true))) {
                let n = cur.ol_counts.get_mut(level).map_or(0, |c| {
                    *c += 1;
                    *c
                });
                format!("{n}.")
            } else {
                if let Some(c) = cur.ol_counts.get_mut(level) {
                    *c = 0; // a bullet item resets this level's number
                }
                bullet_glyph(level).to_string()
            };
            (indent, Some(bullet))
        } else {
            cur.ol_counts.clear();
            // Style indent + the paragraph's own ruler indent attr.
            (f64::from(st.indent) + block_indent_attr(b), None)
        };
        // Coarse float-wrap: if a floated image is active on this page and
        // this block's top still falls within the image band, narrow the
        // text column by the reserved width and (for a left float) push the
        // text origin right so it sits beside the image. The block is shaped
        // at the reduced width, so all its line geometry already reflects the
        // narrowing — draw + hit-test only need the shared x-origin offset.
        let (float_reserve, float_offset) = match cur.active_float {
            Some(f)
                if f.page == cur.cur_page
                    && cur.y + f64::from(st.space_before) + 0.5 < f.bottom =>
            {
                // Subtract the reserved column from the wrap width; for a
                // left float, shift the text origin right by the same amount.
                (f.reserve_w, if f.left { f.reserve_w } else { 0.0 })
            }
            _ => (0.0, 0.0),
        };
        let wrap_w =
            (fg.content_w - num::f32_of_f64(indent) - num::f32_of_f64(float_reserve)).max(40.0);
        let (runs, underlines, strikes) = block_runs_markup(b, self.review.is_visible());
        let align = block_align(b, style_align(&st.align));
        let family = parse_font(&st.font);
        // The paragraph's line-spacing multiplier (1.0 = single) scales the
        // shaped line advance, so it is part of the shape key.
        let line_mult = block_line_spacing(b);
        let key = shape_key(&ShapeKeyInput {
            spans: &runs,
            underlines: &underlines,
            strikes: &strikes,
            base_bold,
            size,
            wrap_w,
            align,
            family,
            line_height_mult: line_mult,
        });
        // Reuse the shaped layout if nothing affecting shaping changed
        // (text, marks, resolved style, wrap width, or line spacing), else
        // re-shape.
        let layout = match old_cache.remove(&b.id) {
            Some((k, lay)) if k == key => {
                self.dbg_layout_hits += 1;
                lay
            }
            previous => {
                self.dbg_layout_misses += 1;
                // An edited block keeps the shaped lines it did not change
                // (its other paragraphs, or the untouched chunks of a huge
                // paragraph); any style or width change reshapes it all.
                RichTextLayout::shape_styled_reusing(
                    &mut text_renderer.font_system,
                    &runs,
                    &RichTextLayout::block_style(base_bold, size, family, line_mult),
                    Some(wrap_w),
                    align,
                    underlines,
                    strikes,
                    previous.map(|(_, layout)| layout),
                )
            }
        };
        ParagraphShape {
            st,
            indent,
            bullet,
            float_offset,
            wrap_w,
            key,
            layout,
        }
    }

    /// One paragraph-like block (a normal paragraph, heading, or list item):
    /// resolve its style/shape, decide whether it fits whole on the current
    /// page or needs to be split into per-page slices, and push its `Laid`
    /// entry/entries. Handles the paragraph side of float bookkeeping — a
    /// floated image only narrows blocks on its own page.
    fn paginate_paragraph(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        context: &mut ParagraphPaginationContext<'_>,
    ) {
        let ps = self.resolve_paragraph_shape(
            b,
            fg,
            context.text_renderer,
            context.old_cache,
            context.cursor,
        );
        let cur = &mut *context.cursor;
        let block_h = f64::from(ps.layout.height());
        // Place with the style's space-before as the leading gap, breaking
        // to the next page when the whole block would not fit before the
        // bottom text margin (the lead is dropped at the top of a fresh
        // page so the first paragraph hugs the top margin).
        let top_candidate = cur.y + f64::from(ps.st.space_before);
        // A manual page break forces a fresh page; otherwise break only
        // when the whole block would overflow the bottom text margin.
        let force_break = block_page_break_before(b);
        let item_top = if cur.placed_on_page
            && (force_break
                || top_candidate + block_h > page_content_bottom(fg, cur.cur_page) + 0.5)
        {
            cur.cur_page += 1;
            page_content_top(fg, cur.cur_page)
        } else {
            top_candidate
        };
        cur.placed_on_page = true;
        // If the page advanced past the float (page-break), the float no
        // longer applies to this block — drop the x-origin offset so the
        // block lays full-width on its new page. (The float only ever
        // narrows blocks on its own page.)
        let float_offset = if cur.active_float.is_some_and(|f| f.page == cur.cur_page) {
            ps.float_offset
        } else {
            0.0
        };
        let pl = ParagraphPlacement {
            text_x: fg.content_x + ps.indent + float_offset,
            width: f64::from(ps.wrap_w),
            size: ps.st.size,
            bullet: ps.bullet.clone(),
            ink: Color::from_rgb8(ps.st.color[0], ps.st.color[1], ps.st.color[2]),
            space_after: ps.st.space_after,
        };
        // Does the whole block fit on the current page from `item_top`? If
        // so this is the fast, unchanged single-`Laid` path. Otherwise the
        // block is split at line boundaries across as many pages as needed.
        if item_top + block_h <= page_content_bottom(fg, cur.cur_page) + 0.5 {
            context.laid.push(Laid {
                id: b.id.clone(),
                text: b.text.clone(),
                kind: b.kind.clone(),
                size: pl.size,
                y_top: item_top,
                text_x: pl.text_x,
                width: pl.width,
                height: block_h,
                layout_y: item_top,
                clip: None,
                bullet: pl.bullet.clone(),
                ink: pl.ink,
                image_hash: None,
                cell_box: None,
                cell_row: None,
                page: cur.cur_page,
                composition: None,
            });
            cur.y = item_top + block_h + f64::from(pl.space_after);
        } else {
            Self::emit_paragraph_slices(b, fg, &ps.layout, context.laid, cur, item_top, &pl);
        }
        context.new_cache.insert(b.id.clone(), (ps.key, ps.layout));
        // Float bookkeeping: a float only narrows blocks on its own page and
        // only while text is still beside the image. Clear it once the page
        // has advanced past the float, or once this block has flowed down to
        // (or past) the image's bottom — and in that latter case make sure
        // the cursor sits at least at the image bottom so the next full-width
        // block clears the image vertically rather than overlapping it.
        if let Some(f) = cur.active_float {
            if cur.cur_page > f.page {
                cur.active_float = None;
            } else if cur.y >= f.bottom {
                cur.y = cur.y.max(f.bottom + f64::from(ps.st.space_after));
                cur.active_float = None;
            }
        }
    }

    /// Intra-block pagination for one paragraph-like block that doesn't fit
    /// on the current page: emit one `Laid` slice per page the block spans.
    /// Each slice shares the one cached `layout` (one id, one cache entry);
    /// only the *visual* rendering is split. A slice's `layout_y` maps the
    /// layout origin so line geometry (`selection_rects`/`caret_geometry`/
    /// `hit`) still resolves, and its `clip` band keeps lines from other
    /// slices off this page.
    fn emit_paragraph_slices(
        b: &BlockView,
        fg: &FrameGeometry,
        layout: &RichTextLayout,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
        item_top: f64,
        pl: &ParagraphPlacement,
    ) {
        // Intra-block pagination: emit one `Laid` slice per page the
        // block spans. Each slice shares the one cached layout (one id,
        // one cache entry); only the *visual* rendering is split. A
        // slice's `layout_y` maps the layout origin so that line geometry
        // (`selection_rects`/`caret_geometry`/`hit`) still resolves, and
        // its `clip` band keeps lines from other slices off this page.
        let lines = layout_line_tops(layout, &b.text);
        let mut li = 0usize;
        let mut slice_top = item_top; // on-screen top of the current slice
        let mut first_slice = true;
        while let Some(&(first_line_top, _)) = lines.get(li) {
            let bottom = page_content_bottom(fg, cur.cur_page);
            // layout_y maps line `li`'s internal top to `slice_top`.
            let layout_y = slice_top - first_line_top;
            // Take every subsequent line that still fits on this page.
            let mut last = li;
            while let Some(&(nt, nh)) = lines.get(last + 1) {
                if layout_y + nt + nh > bottom + 0.5 {
                    break;
                }
                last += 1;
            }
            // Respect the shared paragraph widow/orphan policy. If the
            // next page would receive too few lines, leave enough there;
            // if this page cannot retain the minimum, move the paragraph
            // fragment wholesale instead of producing a one-line stub.
            let available = last - li + 1;
            let allowed = layout.fragment_line_count_from(li, available);
            if allowed == 0 && slice_top > page_content_top(fg, cur.cur_page) + 0.5 {
                cur.cur_page += 1;
                slice_top = page_content_top(fg, cur.cur_page);
                cur.y = slice_top;
                continue;
            }
            let allowed = allowed.max(1).min(available);
            last = li + allowed - 1;
            let Some(&(lt, lh)) = lines.get(last) else {
                // `last` is bounded above by the loop that grew it from
                // `li` (already a valid index) one lookahead at a time, so
                // this is unreachable; break defensively rather than panic.
                break;
            };
            let slice_bottom = layout_y + lt + lh;
            let band = Rect::new(pl.text_x, slice_top, pl.text_x + pl.width, slice_bottom);
            laid.push(Laid {
                id: b.id.clone(),
                text: b.text.clone(),
                kind: b.kind.clone(),
                size: pl.size,
                y_top: slice_top,
                text_x: pl.text_x,
                width: pl.width,
                height: slice_bottom - slice_top,
                layout_y,
                clip: Some(band),
                // The bullet/number belongs to the block's first line only.
                bullet: if first_slice { pl.bullet.clone() } else { None },
                ink: pl.ink,
                image_hash: None,
                cell_box: None,
                cell_row: None,
                page: cur.cur_page,
                composition: None,
            });
            li = last + 1;
            first_slice = false;
            if li < lines.len() {
                // More lines remain: spill onto the next page's top margin.
                cur.cur_page += 1;
                slice_top = page_content_top(fg, cur.cur_page);
                cur.y = slice_top;
            } else {
                cur.y = slice_bottom + f64::from(pl.space_after);
            }
        }
    }

    /// Pass 1's aftermath: the document's total extent, the clamped scroll
    /// offset, and shifting `laid` (+ its cell/clip rects) into screen space so
    /// hit-testing needs no further offset. Returns `(num_pages, sy)`.
    fn finish_scroll(
        &mut self,
        fg: &FrameGeometry,
        laid: &mut [Laid],
        cur_page: usize,
        y: f64,
    ) -> (usize, f64) {
        // Total document extent below `fg.page_top`: the stacked page sheets + gaps,
        // widened if an over-tall block overflowed its page's text band.
        let num_pages = cur_page + 1;
        let pages_extent =
            num::f64_of_usize(num_pages) * fg.letter_h + num::f64_of_usize(cur_page) * PAGE_GAP;
        let extent = pages_extent.max(y - fg.page_top);

        // Scrolling: clamp the offset to the overflow, then shift the laid-out
        // content (+ pages) into screen space so hit-testing needs no offset.
        // `scroll_y` is an offset in unscaled content space; on screen the content
        // is magnified by `zoom`, so the visible window covers `fg.canvas.height() /
        // zoom` of content — divide by zoom so the overflow (and thus max scroll)
        // grows when zoomed in and shrinks when zoomed out.
        self.scroll_viewport = fg.canvas.height() / self.zoom.factor();
        self.max_scroll =
            (extent + 2.0 * PAGE_TOP - fg.canvas.height() / self.zoom.factor()).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll);
        let sy = self.scroll_y;
        if sy != 0.0 {
            for l in laid.iter_mut() {
                l.y_top -= sy;
                l.layout_y -= sy;
                if let Some(bx) = l.cell_box.as_mut() {
                    *bx = Rect::new(bx.x0, bx.y0 - sy, bx.x1, bx.y1 - sy);
                }
                if let Some(c) = l.clip.as_mut() {
                    *c = Rect::new(c.x0, c.y0 - sy, c.x1, c.y1 - sy);
                }
            }
        }

        (num_pages, sy)
    }

    /// Push the canvas clip, then draw each discrete page sheet: shadow +
    /// paper, header/footer running bands (with the active edit-context band
    /// highlighted), and the default "Page N" indicator when there's no footer
    /// of its own. Returns the reference (first) page's rect in screen space.
    fn draw_page_sheets(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
        num_pages: usize,
        sy: f64,
    ) -> Rect {
        // Clip the document to the canvas so scrolled content never paints over
        // the toolbar / status bar.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &fg.canvas,
        );

        // Discrete page sheets (shadow + paper) stacked with `PAGE_GAP`. Each
        // sheet draws the page header inside its top-margin band and the footer
        // inside its bottom-margin band (page-number fields resolved to this
        // page's number). A subtle "Page N" indicator lives in the gap below the
        // sheet only when the document has no footer of its own. All through
        // `fg.ctf` (the zoom transform) so the page magnifies with the content.
        let header = std::mem::take(&mut self.header);
        let footer = std::mem::take(&mut self.footer);
        // Which running region (if any) is currently the active edit context, and
        // on which page its editable band lives (the first page is the reference —
        // the header/footer content is shared across pages, like Word).
        let edit_region = self.inner.region();
        let caret_block = self.inner.caret.block.clone();
        let band_composition = header
            .iter()
            .chain(&footer)
            .find(|b| self.inner.in_running_region() && b.id == caret_block)
            .and_then(|b| self.inner.composition(&b.text));
        self.region_laid.clear();
        // Drawing every sheet makes a long imported document O(page_count) per
        // frame even though the canvas clips all but a handful. Paint only the
        // visible sheets plus one neighbour for the inter-page indicator.
        let visible_top = sy + fg.canvas.y0;
        let visible_bottom = sy + fg.canvas.y1 / self.zoom.factor();
        let first_page = num::usize_of_f64(
            ((visible_top - fg.page_top) / fg.page_pitch)
                .floor()
                .max(0.0),
        )
        .min(num_pages.saturating_sub(1));
        let last_page = num::usize_of_f64(
            ((visible_bottom - fg.page_top) / fg.page_pitch)
                .ceil()
                .max(0.0),
        )
        .saturating_add(1)
        .min(num_pages);
        for i in first_page..last_page {
            self.draw_page_sheet(
                scene,
                text_renderer,
                fg,
                &PageSheetInput {
                    header: &header,
                    footer: &footer,
                    composition: band_composition
                        .as_ref()
                        .map(|core| (caret_block.as_str(), core)),
                    edit_region,
                    index: i,
                    scroll_y: sy,
                },
            );
        }
        self.header = header;
        self.footer = footer;
        // The visible page is the reference rect for guides and hit-testing.
        // Margins apply identically to every sheet, so retaining page zero here
        // would make guides unusable after scrolling through a long document.
        let page = Rect::new(
            fg.page_x,
            fg.page_top + num::f64_of_usize(first_page) * fg.page_pitch - sy,
            fg.page_x + fg.page_w,
            fg.page_top + num::f64_of_usize(first_page) * fg.page_pitch + fg.letter_h - sy,
        );
        self.page_rect = page;
        page
    }

    /// One page sheet (shadow + paper), its header/footer running bands (with
    /// the active edit-context band highlighted), and the default "Page N"
    /// indicator when there's no footer of its own.
    fn draw_page_sheet(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
        input: &PageSheetInput<'_>,
    ) {
        let PageSheetInput {
            header,
            footer,
            composition,
            edit_region,
            index: i,
            scroll_y: sy,
        } = *input;
        let ptop = fg.page_top + num::f64_of_usize(i) * fg.page_pitch - sy;
        let sheet = Rect::new(fg.page_x, ptop, fg.page_x + fg.page_w, ptop + fg.letter_h);
        // The sheet: lifted off the desk by the floating shadow, with an
        // edge derived from the paper so it separates from a light desk too.
        let radius = f64::from(self.paper_theme.tokens.radii.detail);
        ui::surface::shadow(scene, fg.ctf, sheet, radius, &self.chrome.paper_elevation);
        ui::surface::card(scene, fg.ctf, sheet, radius, self.chrome.paper);
        let page_no = i + 1;
        // Header band: the top margin, inset by the L/R text margins.
        let head_band = Rect::new(
            sheet.x0 + fg.m.left,
            sheet.y0,
            sheet.x1 - fg.m.right,
            sheet.y0 + fg.m.top,
        );
        // Footer band: the bottom margin, inset by the L/R text margins.
        let foot_band = Rect::new(
            sheet.x0 + fg.m.left,
            sheet.y1 - fg.m.bottom,
            sheet.x1 - fg.m.right,
            sheet.y1,
        );
        // Cache the reference-page band rects for the event handler's hit-test.
        if i == 0 {
            self.head_band_rect = head_band;
            self.foot_band_rect = foot_band;
        }
        let head_editing = i == 0 && edit_region == Region::Header;
        let foot_editing = i == 0 && edit_region == Region::Footer;
        let (_, head_laid) = Self::draw_running_band(
            scene,
            fg.ctf,
            text_renderer,
            &RunningBand {
                blocks: header,
                page_no,
                rect: head_band,
                is_footer: false,
                editing: head_editing,
                composition: composition.filter(|_| head_editing),
            },
        );
        let (drew_footer, foot_laid) = Self::draw_running_band(
            scene,
            fg.ctf,
            text_renderer,
            &RunningBand {
                blocks: footer,
                page_no,
                rect: foot_band,
                is_footer: true,
                editing: foot_editing,
                composition: composition.filter(|_| foot_editing),
            },
        );
        if head_editing {
            self.region_laid = head_laid;
            self.draw_active_band_chrome(scene, fg.ctf, text_renderer, head_band);
        }
        if foot_editing {
            self.region_laid = foot_laid;
            self.draw_active_band_chrome(scene, fg.ctf, text_renderer, foot_band);
        }
        // Default page indicator in the gap — only when no footer was drawn.
        if !drew_footer && !foot_editing {
            // Drawn in the inter-page gap on the desk: window ink.
            let label = t_args("page-indicator", &[arg("n", num::i64_of_usize(page_no))]);
            let style = self.desk_theme.text(TextRole::Caption).numeric();
            let (w, h) = text_renderer.measure_style(&label, None, &style);
            let lx = f64::midpoint(sheet.x0, sheet.x1) - f64::from(w) / 2.0;
            let ly = sheet.y1 + (PAGE_GAP - f64::from(h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &label,
                self.desk_theme.tokens.colors.text_dim,
                fg.ctf * Affine::translate((lx, ly)),
                None,
                &style,
            );
        }
    }

    /// Restore the block cache pass 1 took, then precompute pass 2's read-only
    /// inputs: whether the selection spans multiple cells, footnote citation
    /// numbers in document order, and (when spell-check is on) the misspelled
    /// spans for every body block near the viewport. Returns `(multi_sel,
    /// note_nums, spell_by_block)`.
    fn prepare_pass2_inputs(
        &mut self,
        blocks: Vec<BlockView>,
        laid: &[Laid],
        fg: &FrameGeometry,
    ) -> Pass2Inputs {
        // Restore the block cache before pass 2 so selection lookups resolve.
        self.blocks = blocks;
        let multi_sel = self.selection_blocks_cached().len() > 1;

        // Footnote numbering: assign citation numbers (1,2,3…) to notes in the
        // order their `note-ref` markers first appear in the body. This drives
        // both the in-text superscript and the bottom-of-page footnote list.
        let note_nums = note_numbering(&self.blocks);

        // Spell-check (squiggles): precompute the misspelled byte spans for every
        // body block *near the viewport* before pass 2, so the read-only pass-2
        // loop (which borrows `self` immutably) can look them up without a `&mut`
        // borrow conflict. Cheap: cached per block keyed by text (an edit
        // re-checks), and culled to a band around the visible canvas.
        let mut spell_by_block: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        if self.spellcheck_enabled() {
            // A generous band around the canvas (one screenful above/below) so a
            // block scrolling into view is already checked; everything else is
            // skipped to keep the pass cheap on long documents.
            let cull_lo = fg.canvas.y0 - fg.canvas.height();
            let cull_hi = fg.canvas.y1 + fg.canvas.height();
            // Collect the (id, text) of in-band, spellable body blocks first to end
            // the immutable borrow of `laid`/`self.blocks` before the `&mut self`
            // dictionary/cache calls below.
            let targets: Vec<(String, String)> = laid
                .iter()
                .filter(|l| {
                    !l.text.is_empty()
                        && l.kind != "image"
                        && l.kind != "fidelity"
                        && l.y_top + l.height >= cull_lo
                        && l.y_top <= cull_hi
                })
                .map(|l| (l.id.clone(), l.text.clone()))
                .collect();
            for (id, text) in targets {
                let spans = self.block_spell_spans(&id, &text);
                if !spans.is_empty() {
                    spell_by_block.insert(id, spans);
                }
            }
        }

        (multi_sel, note_nums, spell_by_block)
    }

    /// Pass 2 of [`Self::draw`]: paint cell borders, bullets, selection, the
    /// shaped text, decorations (find/comment/tracked-change highlights,
    /// spell-check squiggles, footnote superscripts) and the caret for every
    /// laid item. Collects the footnote/comment/change-card bookkeeping that
    /// the passes after this one render.
    fn draw_page_content(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        input: &PageContentInput<'_>,
    ) -> Pass2Markers {
        let PageContentInput {
            laid,
            cache: new_cache,
            multi_selection: multi_sel,
            spell_by_block,
            note_numbers: note_nums,
        } = *input;
        let mut page_notes: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut comment_markers: Vec<(String, f64, f64)> = Vec::new();
        let mut change_markers: Vec<(String, f64, f64)> = Vec::new();

        // Pass 2: draw cell borders, bullets, selection, text, caret. Everything
        // here renders through `fg.ctf` (the zoom transform) so page content scales
        // while the surrounding chrome stays at 100%.
        for l in laid {
            if l.kind == "image" {
                self.draw_laid_image(scene, fg, text_renderer, l);
                continue;
            }
            if let Some(bx) = l.cell_box {
                self.draw_laid_cell_box(scene, fg, bx, l.cell_row);
            }

            if l.kind == "fidelity" {
                Self::draw_laid_fidelity(
                    scene,
                    fg,
                    text_renderer,
                    l,
                    &self.chrome,
                    &self.paper_theme,
                );
                continue;
            }

            Self::draw_laid_bullet(scene, fg, text_renderer, l);

            // The shaped text lives in the layout cache (built in pass 1), keyed
            // by block id. Every normal/cell entry was inserted there.
            let Some((_, layout)) = new_cache.get(&l.id) else {
                continue;
            };
            // Layout-relative geometry is measured from `layout_y` (= the on-screen
            // y of the shaped layout's origin). For an unsplit block this equals
            // `y_top`; for a slice of a page-split block it is offset so the carried
            // lines land in this slice's band.
            let ly = l.layout_y;
            // A split slice clips its drawing to its own band so lines belonging to
            // the block's other slices (drawn from the same layout via `ly`) never
            // bleed onto this page. Unsplit blocks (`clip == None`) draw unclipped.
            let clipped = l.clip.is_some();
            if let Some(band) = l.clip {
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    fg.ctf,
                    &band,
                );
            }

            let item = LaidItemCtx { l, layout, ly };
            self.draw_laid_text(
                scene,
                fg,
                text_renderer,
                &item,
                multi_sel,
                &mut comment_markers,
            );
            self.draw_laid_change_decos(scene, fg, &item, &mut change_markers);
            self.draw_laid_notes_and_spellcheck(
                scene,
                fg,
                text_renderer,
                &item,
                NotesSpellcheckInput {
                    spell_by_block,
                    note_numbers: note_nums,
                    page_notes: &mut page_notes,
                },
            );

            if self.lifecycle.focused
                && l.id == self.inner.caret.block
                && !self.inner.has_selection()
            {
                let byte = l.caret_byte(self.inner.caret.offset);
                let (cx, cy, ch) = layout.caret_geometry(byte);
                let caret = Rect::new(
                    l.text_x + f64::from(cx),
                    ly + f64::from(cy),
                    l.text_x + f64::from(cx) + CARET_WIDTH,
                    ly + f64::from(cy) + f64::from(ch),
                );
                self.ime_rect = Some(fg.ctf.transform_rect_bbox(caret));
                scene.fill(
                    Fill::NonZero,
                    fg.ctf,
                    self.chrome.paper_accent,
                    None,
                    &caret,
                );
            }
            if clipped {
                scene.pop_layer();
            }
        }

        Pass2Markers {
            page_notes,
            comment_markers,
            change_markers,
        }
    }

    fn draw_laid_bullet(
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
    ) {
        if let Some(bul) = &l.bullet {
            // The marker column is right-aligned at the same gap for `9.`
            // and `10.` alike. A fixed start point makes multi-digit
            // counters grow into the paragraph and visibly collide at
            // large text. Measure with the exact font/scale used below,
            // then reserve the same perceived gap for every counter width.
            let marker = ui::text_render::TextStyle::content(l.size);
            let (marker_width, _) = text_renderer.measure_style(bul, None, &marker);
            let marker_x = list_marker_x(l.text_x, f64::from(marker_width));
            text_renderer.draw_style(
                scene,
                bul,
                l.ink,
                fg.ctf * Affine::translate((marker_x, l.y_top)),
                None,
                &marker,
            );
        }
    }

    /// A table cell's tinted background (header row / zebra stripe / plain)
    /// + border, for one pass-2 item.
    fn draw_laid_cell_box(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        bx: Rect,
        cell_row: Option<usize>,
    ) {
        // Document table styling on the sheet (not OS chrome): default header
        // tint, zebra and hairline the authored table carries.
        let fill = match cell_row {
            Some(0) => Color::from_rgb8(0xec, 0xee, 0xf3), // header row tint
            Some(r) if r % 2 == 0 => Color::from_rgb8(0xf6, 0xf6, 0xf9), // zebra
            _ => self.chrome.paper,
        };
        scene.fill(Fill::NonZero, fg.ctf, fill, None, &bx);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            fg.ctf,
            Color::from_rgb8(0xc4, 0xc6, 0xcc),
            None,
            &bx,
        );
    }

    /// One pass-2 image item: paint it (mirrored per `flip_h`/`flip_v`, or a
    /// placeholder if undecoded), then ring it with the selection chrome when
    /// it's the caret's block.
    fn draw_laid_image(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
    ) {
        let rect = Rect::new(l.text_x, l.y_top, l.text_x + l.width, l.y_top + l.height);
        // Mirror flips (`flip_h` / `flip_v` block attrs) are applied as a
        // scale-about-centre on the paint transform, so the image draws
        // mirrored in place without moving its slot.
        let options = image_paint_options(self.blocks.iter().find(|b| b.id == l.id));
        let image_transform = image_paint_transform(fg.ctf, rect, options);
        if let Some(hash) = &l.image_hash {
            if let Some(slot) = self.image_cache.get_mut(hash)
                && let Some(img) = slot
            {
                // Paint a scaled full-source rectangle through a clip matching
                // the stable display frame. This is source cropping, not a
                // resize: the frame and surrounding text layout never jump.
                let source = cropped_image_source(rect, options.crop);
                if image_crop_is_active(options.crop) {
                    scene.push_layer(
                        Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        1.0,
                        fg.ctf,
                        &rect,
                    );
                    img.paint_at(scene, image_transform, source);
                    scene.pop_layer();
                } else {
                    img.paint_at(scene, image_transform, rect);
                }
            }
        } else {
            Self::draw_missing_image(
                scene,
                fg.ctf,
                text_renderer,
                rect,
                &self.chrome,
                &self.paper_theme,
            );
        }
        // Selection chrome: when this image is the caret's block, ring it
        // with an accent border + corner resize handles so it reads as
        // "selected" (the panel then shows the resize / flip controls).
        if l.id == self.inner.caret.block {
            Self::draw_selected_image_chrome(
                scene,
                fg.ctf,
                rect,
                self.chrome.paper_accent,
                self.chrome.paper,
            );
        }
    }

    fn draw_missing_image(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        rect: Rect,
        chrome: &ChromeColors,
        paper: &Theme,
    ) {
        let rounded =
            vello::kurbo::RoundedRect::from_rect(rect, f64::from(paper.tokens.radii.detail));
        scene.fill(Fill::NonZero, transform, chrome.paper_tint, None, &rounded);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            chrome.paper_edge,
            None,
            &rounded,
        );
        let label = t("canvas-image-missing");
        let style = paper.text(TextRole::Body);
        let (_, h) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            paper.tokens.colors.text_dim,
            transform
                * Affine::translate((
                    rect.x0 + f64::from(paper.tokens.spacing.s3),
                    rect.center().y - f64::from(h) / 2.0,
                )),
            None,
            &style,
        );
    }

    fn draw_selected_image_chrome(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        accent: Color,
        handle_fill: Color,
    ) {
        scene.stroke(
            &vello::kurbo::Stroke::new(2.0),
            transform,
            accent,
            None,
            &rect,
        );
        let handle_radius = 5.0;
        for (x, y) in [
            (rect.x0, rect.y0),
            (rect.x1, rect.y0),
            (rect.x1, rect.y1),
            (rect.x0, rect.y1),
        ] {
            let handle = Rect::new(
                x - handle_radius,
                y - handle_radius,
                x + handle_radius,
                y + handle_radius,
            );
            scene.fill(Fill::NonZero, transform, handle_fill, None, &handle);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                accent,
                None,
                &handle,
            );
        }
    }

    /// One pass-2 "fidelity" (unrenderable embedded content) placeholder item.
    fn draw_laid_fidelity(
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
        chrome: &ChromeColors,
        paper: &Theme,
    ) {
        let box_rect = Rect::new(
            l.text_x,
            l.y_top + 2.0,
            (fg.page_x + fg.page_w - fg.m.right).min(l.text_x + 360.0),
            l.y_top + l.height - 2.0,
        );
        scene.fill(
            Fill::NonZero,
            fg.ctf,
            chrome.paper_tint,
            None,
            &vello::kurbo::RoundedRect::from_rect(box_rect, f64::from(paper.tokens.radii.detail)),
        );
        let label = t("canvas-embedded-content");
        let style = paper.text(TextRole::Body);
        let (_, h) = text_renderer.measure_style(&label, None, &style);
        text_renderer.draw_style(
            scene,
            &label,
            paper.tokens.colors.text_dim,
            fg.ctf
                * Affine::translate((
                    box_rect.x0 + f64::from(paper.tokens.spacing.s3),
                    box_rect.center().y - f64::from(h) / 2.0,
                )),
            None,
            &style,
        );
    }

    /// Selection highlight, find-match highlight, comment highlight (which
    /// also records `comment_markers` for the review-card pass), and the
    /// shaped text itself, for one pass-2 item.
    fn draw_laid_text(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        item: &LaidItemCtx,
        multi_sel: bool,
        comment_markers: &mut Vec<(String, f64, f64)>,
    ) {
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        if let Some((b0, b1)) = self
            .selection_bytes_for(&l.id, &l.text)
            .map(|(b0, b1)| (l.shown(b0), l.shown(b1)))
        {
            if let (Some(bx), true) = (l.cell_box, multi_sel) {
                // Multi-cell selection: highlight the whole cell (spreadsheet-style).
                scene.fill(Fill::NonZero, fg.ctf, self.paper_selection(), None, &bx);
            } else {
                for (rx0, ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, self.paper_selection(), None, &r);
                }
            }
        }

        // Find-match highlights (under the text).
        if self.find.is_open() {
            let ranges: Vec<(usize, usize)> = self
                .find
                .matches()
                .iter()
                .filter(|(bid, _, _)| bid == &l.id)
                .map(|(_, cs, ce)| (l.layout_byte(*cs), l.layout_byte(*ce)))
                .collect();
            let current = self
                .find
                .active_match()
                .filter(|(bid, _, _)| bid == &l.id)
                .map(|(_, cs, ce)| (char_to_byte(&l.text, *cs), char_to_byte(&l.text, *ce)));
            let (top, bottom) = layout_clip_band(fg, ly);
            let ranges = spans_within(&ranges, layout.source_range_within(top, bottom));
            for (range, rects) in ranges
                .iter()
                .zip(layout.selection_rects_many_within(&ranges, top, bottom))
            {
                for (rx0, ry0, rx1, ry1) in rects {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    ui::widget::find_bar::paint_find_match(
                        scene,
                        fg.ctf,
                        r,
                        current == Some(*range),
                        &self.paper_theme,
                    );
                }
            }
        }
        // Comment highlight bands (under the text): a pale-yellow band over
        // each `comment`-marked range. Mapped to on-screen rects via the same
        // `selection_rects` geometry as the selection highlight, so it tracks
        // wrapping. The comment's start line is recorded for the right-margin
        // indicator/card rendered after the loop. Only when markup is shown.
        if self.review.is_visible()
            && let Some(b) = self.blocks.iter().find(|b| b.id == l.id)
        {
            for (bs, be, cid) in block_comment_ranges(b) {
                let author = self.review.comment_author(&cid).unwrap_or("");
                let wash =
                    ReviewColors::comment_highlight(&self.paper_theme, self.chrome.paper, author);
                let rects = layout.selection_rects(l.shown(bs), l.shown(be));
                for &(rx0, ry0, rx1, ry1) in &rects {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, wash, None, &r);
                }
                // Anchor the marker at the start line's top, and remember
                // the range's right edge (max rect x1) for the inline tab.
                if let Some(&(_, ry0, _, _)) = rects.first() {
                    let right_x = rects.iter().map(|r| r.2).fold(f32::MIN, f32::max);
                    comment_markers.push((cid, ly + f64::from(ry0), l.text_x + f64::from(right_x)));
                }
            }
        }

        let text_transform = fg.ctf * Affine::translate((l.text_x, ly));
        #[cfg(test)]
        if self.reference_text_rendering {
            text_renderer.draw_rich(scene, layout, text_transform, l.ink);
            return;
        }
        // Keep the real canvas/slice layers and all paragraph bookkeeping.
        // Only omit glyph encoding whose ink cannot reach either clip.
        let clip = l.clip.map_or(fg.canvas_clip, |clip| {
            fg.canvas_clip.intersect(fg.ctf.transform_rect_bbox(clip))
        });
        text_renderer.draw_rich_in_clip(scene, layout, text_transform, l.ink, clip);
    }

    /// Tracked-change decorations (green underline for `ins`, red strike for
    /// `del`) over one pass-2 item's text, plus recording `change_markers`
    /// for the review-card pass. Drawn here (rather than via the shaped
    /// layout's own underline/strike lists) so the decoration colour matches
    /// the review-tinted glyphs — the toolkit paints shaped decorations in
    /// the block ink. Only when markup is shown.
    fn draw_laid_change_decos(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        item: &LaidItemCtx,
        change_markers: &mut Vec<(String, f64, f64)>,
    ) {
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        if self.review.is_visible()
            && let Some(b) = self.blocks.iter().find(|b| b.id == l.id)
        {
            let (ins_ranges, del_ranges) = block_change_decos(b);
            for &(b0, b1) in &ins_ranges {
                for (rx0, _ry0, rx1, ry1) in layout.selection_rects(l.shown(b0), l.shown(b1)) {
                    let uy = ly + f64::from(ry1) - 2.0;
                    let line = Rect::new(
                        l.text_x + f64::from(rx0),
                        uy,
                        l.text_x + f64::from(rx1),
                        uy + 1.5,
                    );
                    scene.fill(Fill::NonZero, fg.ctf, INSERTION_INK, None, &line);
                }
            }
            for &(b0, b1) in &del_ranges {
                for (rx0, ry0, rx1, ry1) in layout.selection_rects(l.shown(b0), l.shown(b1)) {
                    let my = ly + f64::from(f32::midpoint(ry0, ry1));
                    let line = Rect::new(
                        l.text_x + f64::from(rx0),
                        my,
                        l.text_x + f64::from(rx1),
                        my + 1.5,
                    );
                    scene.fill(Fill::NonZero, fg.ctf, DELETION_INK, None, &line);
                }
            }
            // Collect a review-card marker per change range (carrying the
            // change id so the card's Accept/Reject can act on it).
            for (bs, be, cid) in block_change_ranges(b) {
                let rects = layout.selection_rects(l.shown(bs), l.shown(be));
                if let Some(&(_, ry0, _, _)) = rects.first() {
                    let right_x = rects.iter().map(|r| r.2).fold(f32::MIN, f32::max);
                    change_markers.push((cid, ly + f64::from(ry0), l.text_x + f64::from(right_x)));
                }
            }
        }
    }

    /// Spell-check squiggles + note-reference superscripts over one pass-2
    /// item's text, recording `page_notes` for the footnote-area pass.
    fn draw_laid_notes_and_spellcheck(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        item: &LaidItemCtx,
        input: NotesSpellcheckInput<'_>,
    ) {
        let NotesSpellcheckInput {
            spell_by_block,
            note_numbers: note_nums,
            page_notes,
        } = input;
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        // Spell-check squiggles: a wavy red underline under each misspelled
        // span. The byte span maps to one-or-more on-screen rects via the same
        // `selection_rects` geometry the selection highlight uses (so it tracks
        // wrapping). The squiggle hugs just under each rect's bottom.
        if let Some(spans) = spell_by_block.get(&l.id) {
            // One pass over the paragraph's runs for all of its spans (a
            // pasted page can carry thousands of squiggles).
            let (top, bottom) = layout_clip_band(fg, ly);
            let shown: Vec<(usize, usize)>;
            let spans = if l.composition.is_some() {
                shown = spans
                    .iter()
                    .map(|&(s, e)| (l.shown(s), l.shown(e)))
                    .collect();
                &shown
            } else {
                spans
            };
            let ranges = spans_within(spans, layout.source_range_within(top, bottom));
            for rects in layout.selection_rects_many_within(&ranges, top, bottom) {
                for (rx0, _ry0, rx1, ry1) in rects {
                    let y = ly + f64::from(ry1) - 1.0;
                    draw_squiggle(
                        scene,
                        fg.ctf,
                        self.chrome.paper_danger,
                        l.text_x + f64::from(rx0),
                        l.text_x + f64::from(rx1),
                        y,
                    );
                }
            }
        }

        // Note-reference superscripts: each `note-ref` placeholder in this
        // block renders as a small superscript citation number, positioned at
        // the placeholder glyph's location (it occupies one U+FFFC char, which
        // the body run shapes as near-zero-width). The number is taken from the
        // document-order numbering; the page is recorded so the footnote text
        // renders at the bottom of this page.
        if !note_nums.is_empty()
            && let Some(b) = self.blocks.iter().find(|b| b.id == l.id)
        {
            for (ci, note_id) in block_note_refs(b) {
                let Some(&num) = note_nums.get(&note_id) else {
                    continue;
                };
                let bs = l.layout_byte(ci);
                let be = l.layout_byte(ci + 1);
                // Anchor at the placeholder's left edge; fall back to the
                // caret geometry if the glyph produced no selection rect.
                let placeholder_rect = layout.selection_rects(bs, be).into_iter().next();
                let (mx, my) = placeholder_rect.map_or_else(
                    || {
                        let (cx, cy, _) = layout.caret_geometry(bs);
                        (cx, cy)
                    },
                    |(rx0, ry0, _, _)| (rx0, ry0),
                );
                // Mask the U+FFFC object-replacement glyph (a box in some
                // fonts) with paper so only the superscript number shows.
                if let Some((rx0, ry0, rx1, ry1)) = placeholder_rect {
                    let mask = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, self.chrome.paper, None, &mask);
                }
                let label = num.to_string();
                // Superscript: smaller font, raised toward the line's top.
                let sup_size = (l.size * 0.7).max(8.0);
                text_renderer.draw_style(
                    scene,
                    &label,
                    l.ink,
                    fg.ctf
                        * Affine::translate((l.text_x + f64::from(mx), ly + f64::from(my) - 1.0)),
                    None,
                    &ui::text_render::TextStyle::content(sup_size),
                );
                page_notes.entry(l.page).or_default().push(num);
            }
        }
    }

    /// Footnote areas: a thin separator + numbered footnotes at the bottom of
    /// each page's text band, keyed by the page pass 2 recorded each marker
    /// against. Page assignment is approximate; drawn inside the page clip so
    /// it scrolls with the content. Endnotes are skipped (they belong at the
    /// document end, not per page).
    fn draw_footnotes(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        page_notes: &HashMap<usize, Vec<usize>>,
        note_nums: &HashMap<String, usize>,
        sy: f64,
    ) {
        if !page_notes.is_empty() {
            // number → footnote text, resolved once. Endnotes are skipped here
            // (they belong at the document end, not per page).
            let mut num_to_note: HashMap<usize, String> = HashMap::new();
            for n in &self.notes {
                if n.kind == tilo_doc::schema::NOTE_KIND_FOOTNOTE
                    && let Some(&num) = note_nums.get(&n.id)
                {
                    num_to_note.insert(num, note_text(n));
                }
            }
            for (page, nums) in page_notes {
                // Distinct, ascending footnote numbers on this page.
                let mut nums: Vec<usize> = nums
                    .iter()
                    .copied()
                    .filter(|n| num_to_note.contains_key(n))
                    .collect();
                nums.sort_unstable();
                nums.dedup();
                if nums.is_empty() {
                    continue;
                }
                let line_h = (f64::from(NOTE_CONTENT_PX) * 1.4).ceil();
                let area_h = num::f64_of_usize(nums.len()) * line_h;
                // Bottom of the text band on this page (above the footer margin).
                let band_bottom =
                    fg.page_top + num::f64_of_usize(*page) * fg.page_pitch + fg.letter_h
                        - fg.m.bottom
                        - sy;
                let sep_y = band_bottom - area_h - NOTE_AREA_GAP;
                let left = fg.page_x + fg.m.left;
                let right = fg.page_x + fg.page_w - fg.m.right;
                // Thin separator (about a third of the text width, like Word).
                let sep =
                    vello::kurbo::Line::new((left, sep_y), (left + (right - left) * 0.33, sep_y));
                scene.stroke(
                    &vello::kurbo::Stroke::new(0.75),
                    fg.ctf,
                    NOTE_SEP,
                    None,
                    &sep,
                );
                let mut ny = sep_y + NOTE_AREA_GAP * 0.6;
                for num in nums {
                    if let Some(text) = num_to_note.get(&num) {
                        let line = format!("{num}. {text}");
                        text_renderer.draw_style(
                            scene,
                            &line,
                            NOTE_INK,
                            fg.ctf * Affine::translate((left, ny)),
                            None,
                            &ui::text_render::TextStyle::content(NOTE_CONTENT_PX),
                        );
                        ny += line_h;
                    }
                }
            }
        }
    }

    /// The single-line "start typing" placeholder shown over an otherwise
    /// empty document.
    fn draw_empty_placeholder(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        text_renderer: &mut TextRenderer,
        first: Option<&Laid>,
    ) {
        if self.blocks.len() == 1
            && let Some(only) = self.blocks.first()
            && only.kind == "paragraph"
            && only.text.is_empty()
            && let Some(l) = first
        {
            // The hint starts past the caret, so the caret never stands on
            // its first letter, and is set in the paragraph's own size.
            let placeholder = t("canvas-empty-placeholder");
            let style = ui::text_render::TextStyle::content(l.size);
            let x = l.text_x + CARET_WIDTH + f64::from(self.paper_theme.tokens.spacing.s1);
            text_renderer.draw_style(
                scene,
                &placeholder,
                self.paper_theme.tokens.colors.text_tertiary,
                ctf * Affine::translate((x, l.y_top)),
                // The empty-state hint shares the authored paragraph's live
                // column width. At 200% text a one-line hint used to extend
                // beyond the responsive sheet, unlike real paragraph text.
                // Let the shared text renderer wrap it at that exact bound.
                Some(num::f32_of_f64((l.width - (x - l.text_x)).max(1.0))),
                &style,
            );
        }
    }

    /// Header/footer edit mode: veil the body (Word dims it while editing a
    /// running head) on every page, then draw the band caret on top so it
    /// stays crisp through the veil.
    fn draw_running_region_veil(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        num_pages: usize,
        sy: f64,
    ) {
        if self.inner.in_running_region() {
            let visible_top = sy + fg.canvas.y0;
            let visible_bottom = sy + fg.canvas.y1 / self.zoom.factor();
            let first_page = num::usize_of_f64(
                ((visible_top - fg.page_top) / fg.page_pitch)
                    .floor()
                    .max(0.0),
            )
            .min(num_pages.saturating_sub(1));
            let last_page = num::usize_of_f64(
                ((visible_bottom - fg.page_top) / fg.page_pitch)
                    .ceil()
                    .max(0.0),
            )
            .saturating_add(1)
            .min(num_pages);
            for i in first_page..last_page {
                let ptop = fg.page_top + num::f64_of_usize(i) * fg.page_pitch - sy;
                let body_top = ptop + fg.m.top;
                let body_bot = ptop + fg.letter_h - fg.m.bottom;
                let veil = Rect::new(
                    fg.page_x + fg.m.left,
                    body_top,
                    fg.page_x + fg.page_w - fg.m.right,
                    body_bot,
                );
                scene.fill(
                    Fill::NonZero,
                    fg.ctf,
                    with_alpha(self.chrome.paper, BODY_DIM_VEIL_ALPHA),
                    None,
                    &veil,
                );
            }
            self.draw_band_caret(scene, fg.ctf);
        }
    }

    /// The scrollbar thumb (right edge), shown only when the content
    /// overflows the canvas.
    fn draw_scrollbar(&self, scene: &mut Scene, transform: Affine, canvas: Rect) {
        if self.max_scroll > 0.0 {
            let gutter = f64::from(self.desk_theme.tokens.metrics.scrollbar_gutter);
            let track = Rect::new(canvas.x1 - gutter, canvas.y0, canvas.x1, canvas.y1);
            ui::widget::Scrollbar::vertical().draw(
                scene,
                transform,
                track,
                ui::widget::scrollbar::ScrollMetrics::new(
                    canvas.height() + self.max_scroll,
                    canvas.height(),
                    self.scroll_y,
                ),
                &self.desk_theme,
                false,
            );
        }
    }

    /// Remote collaborators' carets + selections, resolved by the single
    /// [`Self::remote_presence_overlays`] pass so on-screen geometry matches
    /// what the unit tests assert on. Selections paint under the text, carets
    /// over it, each in the peer's colour with a name pill. A no-op with no
    /// remote presence, so single-user rendering is unchanged.
    fn draw_remote_overlays(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        canvas_clip: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let (remote_carets, remote_selections) = self.remote_presence_overlays(presence);
        // A peer's just-typed run glows in their colour, fading; drawn first so it
        // sits under the selection and caret rather than competing with them.
        for (user, spans) in self.remote_typing_overlays(presence, ui::time::now()) {
            presence.draw_peer_typing(scene, theme, &user, &spans);
        }
        for sel in &remote_selections {
            for r in &sel.rects {
                presence.draw_peer_selection(scene, theme, ctf, &sel.user, *r);
            }
        }
        // Action-driven collaborators publish stable block/range anchors,
        // not fake mouse coordinates. Resolve those anchors through the same
        // shaped layouts as a person's selection and decorate the resulting
        // geometry with the authenticated actor label.
        let now = ui::time::now();
        for interaction in presence.interactions_for_resource("word-processor.document", now) {
            let ranges = match &interaction.anchor {
                tilo_actions::InteractionAnchor::Objects { ids } => ids
                    .iter()
                    .filter_map(|id| {
                        self.blocks
                            .iter()
                            .find(|block| block.id == *id)
                            .map(|block| (id.as_str(), 0, block.text.chars().count()))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let mut rects = self
                .semantic_text_rects(&interaction.anchor, ctf, canvas_clip)
                .unwrap_or_default();
            for (block, start, end) in ranges {
                let Some(l) = self.laid.iter().find(|laid| laid.id == block) else {
                    continue;
                };
                let Some((_, layout)) = self.layout_cache.get(block) else {
                    continue;
                };
                let (start, end) = (l.layout_byte(start), l.layout_byte(end));
                rects.extend(layout.selection_rects(start, end).into_iter().map(
                    |(x0, y0, x1, y1)| {
                        Rect::new(
                            l.text_x + f64::from(x0),
                            l.y_top + f64::from(y0),
                            l.text_x + f64::from(x1),
                            l.y_top + f64::from(y1),
                        )
                    },
                ));
            }
            if rects.is_empty() {
                continue;
            }
            // This pass runs after the page layer was popped. Clip semantic
            // highlights and their labels explicitly so they cannot reach
            // the toolbar, other panes, or the surrounding host chrome.
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &canvas_clip,
            );
            let user = UserId(interaction.actor.id.clone());
            for rect in &rects {
                presence.draw_peer_selection(scene, theme, ctf, &user, *rect);
            }
            // Label an actual visible span, never a union spanning page gaps.
            if let Some(bounds) = rects.first().copied() {
                let label = presence.display_name(&user);
                presence.draw_peer_outline_labeled(
                    &mut ui::paint::PaintCtx::new(scene, ctf, text_renderer, theme),
                    &user,
                    &label,
                    bounds,
                    3.0,
                );
            }
            scene.pop_layer();
        }
        for c in &remote_carets {
            presence.draw_peer_caret_labeled(
                scene,
                theme,
                text_renderer,
                ctf,
                &c.user,
                &c.label,
                c.rect,
            );
        }
    }
}

impl WordProcessorWidget {
    /// Drag-and-drop a file onto the window inserts it as an image at the
    /// **drop point**: seat the caret under the last pointer position (where
    /// the file was released) before inserting, so the image lands where the
    /// user dropped it rather than wherever the caret happened to be. winit's
    /// `DroppedFile` carries no coordinates, so the tracked pointer (updated
    /// on every `CursorMoved`, including during the drag-hover) is the drop
    /// location. A drop over the page body relocates the caret; a drop over
    /// chrome falls back to the existing caret.
    fn handle_dropped_file(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        path: &std::path::Path,
        handled: &mut bool,
    ) {
        if self.rect.contains(self.pointer) {
            let tb_h = self.tool_band.height();
            let over_body = self.pointer.y >= self.rect.y0 + tb_h
                && self.pointer.y <= self.rect.y1 - self.layout_bar_height
                && !(self.view.styles_panel_visible && self.panel_rect.contains(self.pointer))
                && !self.outline.contains(self.pointer);
            if over_body {
                self.caret_from_pointer(self.pointer_to_content(self.pointer), false);
            }
        }
        if self.insert_image_from_path(state, path) {
            *handled = true;
        }
    }

    /// Ctrl+wheel zooms a step per notch (scroll-up = zoom in); a plain wheel
    /// scrolls, clamped to the content overflow.
    fn handle_mouse_wheel(
        &mut self,
        delta: &MouseScrollDelta,
        scale_factor: f64,
        ctrl: bool,
        handled: &mut bool,
    ) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
            MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
        };
        // Over the inspector, the wheel scrolls its column.
        if self.styles_panel_visible() && self.panel_body.contains(self.pointer) {
            let next = (self.panel_scroll - dy).clamp(0.0, self.panel_max_scroll);
            if next.to_bits() != self.panel_scroll.to_bits() {
                self.panel_scroll = next;
                self.lifecycle.dirty = true;
            }
            *handled = true;
            return;
        }
        if ctrl {
            // Ctrl + wheel zooms (a step per notch of scroll), the standard
            // editor gesture. Scroll-up (positive dy) zooms in.
            let step = if dy > 0.0 { ZOOM_STEP } else { -ZOOM_STEP };
            if self.zoom_by(step) {
                *handled = true;
            }
            return;
        }
        let new = (self.scroll_y - dy).clamp(0.0, self.max_scroll);
        // Exact-change check (not "close enough"): any scroll delta, however
        // tiny, should mark the frame dirty, so bit-compare rather than use a
        // tolerance that could mask a real change.
        if new.to_bits() != self.scroll_y.to_bits() {
            self.scroll_y = new;
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    /// Track the pointer, update toolbar hover feedback, and continue
    /// whichever live drag is active (style-field scrub, ruler marker, layout
    /// guide, or a text-selection extend) — scrub takes priority, since a
    /// scrub in progress should never fall through to selection.
    fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
        scale_factor: f64,
        tb_h: f64,
        handled: &mut bool,
    ) {
        self.pointer = Point::new(x / scale_factor, y / scale_factor);
        let _ = tb_h;
        // Hover lifts on the chrome the editor paints itself.
        let hover_changed = self.outline.pointer_moved(self.pointer)
            | self.review.pointer_moved(self.pointer)
            | self.panel_rect.contains(self.pointer)
            | self.thread_panel.contains(self.pointer)
            | self
                .layout_chips
                .iter()
                .any(|(_, r)| r.contains(self.pointer));
        if hover_changed {
            self.lifecycle.dirty = true;
            *handled = true;
        }
        if false {
        } else if self.object_drag.is_some() {
            let cp = self.pointer_to_content(self.pointer);
            let Some(drag) = self.object_drag.as_mut() else {
                return;
            };
            let dx = cp.x - drag.start_pointer.x;
            let dy = cp.y - drag.start_pointer.y;
            if dx.abs() > 1.0 || dy.abs() > 1.0 {
                drag.moved = true;
            }
            drag.x = (drag.start_x + dx).clamp(0.0, 100_000.0);
            drag.y = (drag.start_y + dy).clamp(0.0, 100_000.0);
            self.lifecycle.dirty = true;
            *handled = true;
        } else if self.ruler.is_dragging() {
            // Live-drag a ruler marker (margin / indent / tab). Updates the
            // value for an instant preview; persisted on release.
            self.ruler_drag_move();
            self.lifecycle.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag {
            // Live-drag a layout guide (margin / border / spacing). Guides
            // live in unscaled page space, so map the pointer through the
            // inverse zoom first.
            self.apply_guide_drag(gd, self.pointer_to_content(self.pointer));
            self.lifecycle.dirty = true; // repaint every move so the preview is live
            *handled = true;
        } else if self.drag.drag().is_some() {
            // Extend the selection: keep the anchor, move the caret. The
            // `Laid` rects are unscaled, so hit-test in content space.
            self.caret_from_pointer(self.pointer_to_content(self.pointer), true);
            // Claim the move so the host App repaints (its redraw is gated
            // behind `handled`).
            *handled = true;
        }
    }

    /// Right-click: open the style-management menu when it lands on a
    /// styles-panel chip, else the editing/spell-check context menu when it
    /// lands on the page body (chrome — toolbar/panel/status bar — ignores
    /// the right button; it only hit-tests the left one).
    fn handle_right_press(&mut self, state: &AutoCommit, tb_h: f64, handled: &mut bool) {
        if !self.rect.contains(self.pointer) {
            return;
        }
        self.lifecycle.focused = true;
        // A right-click on a style chip in the styles panel opens the
        // style-management menu (Delete / Duplicate) before the generic
        // "panel is chrome, ignore" rule kicks in.
        if self.view.styles_panel_visible && self.panel_rect.contains(self.pointer) {
            if let Some(id) = self.style_chip_at(self.pointer) {
                self.style_menu.open(&id, self.pointer);
                self.style_menu.menu_mut().constrain_to(self.rect);
                self.lifecycle.dirty = true;
                *handled = true;
            }
            return;
        }
        let in_chrome = self.pointer.y < self.rect.y0 + tb_h
            || self.pointer.y > self.rect.y1 - self.layout_bar_height
            || (self.view.styles_panel_visible && self.panel_rect.contains(self.pointer))
            || self.outline.contains(self.pointer);
        if !in_chrome {
            let cp = self.pointer_to_content(self.pointer);
            let pos = [
                num::f32_of_f64(self.pointer.x),
                num::f32_of_f64(self.pointer.y),
            ];
            self.open_context_menu(state, cp, pos);
            *handled = true;
        }
    }

    /// Left press: route to an open menu first, then dismiss any staged
    /// toast, then (once the pointer is inside this widget and it's focused)
    /// walk the chrome regions in priority order, falling back to the page
    /// body — review affordances, layout guides, header/footer band switch,
    /// then the text-selection gesture.
    fn handle_left_press(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        tb_h: f64,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.handle_left_press_menu_routing(state, handled) {
            return;
        }
        // Any left press dismisses a staged toast (a transient cue).
        if self.toast.take().is_some() {
            self.lifecycle.dirty = true;
        }
        if !self.rect.contains(self.pointer) {
            return;
        }
        self.lifecycle.focused = true;
        if self.handle_left_press_panels(state, handled) {
            return;
        }
        if self.handle_left_press_find_ruler_toolbar(state, tb_h, handled) {
            return;
        }
        self.handle_left_press_body(state, shift, ctrl, handled);
    }

    /// While the context / "New" template / paragraph-style menu is open,
    /// route the left press to it: a row dispatches its action, a click
    /// outside dismisses it. Either way the click is consumed so it never
    /// reaches the page beneath. Returns whether a menu claimed the press.
    fn handle_left_press_menu_routing(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        // While the context menu is open, route the left press to it: a
        // row dispatches its action, a click outside dismisses it. Either
        // way the click is consumed so it never reaches the page beneath.
        if self.context_menu.is_open() {
            let point = [self.pointer.x, self.pointer.y];
            if let Some(idx) = self.context_menu.item_at(point) {
                self.dispatch_context_action(idx, state);
            } else {
                self.context_menu.close();
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Likewise route the press to the "New" template picker when open.
        if self.template_picker.is_open() {
            if let Some(template) = self.template_picker.click(self.pointer) {
                self.request_template(state, template);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // The paragraph-style menu (Delete / Duplicate) captures the press
        // the same way: a row runs its action, an outside click dismisses.
        if self.style_menu.is_open() {
            if let Some(action) = self.style_menu.click(self.pointer) {
                self.dispatch_style_menu(action, state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// A pending numeric-field commit, the styles panel, and the outline
    /// pane — in that priority order. Returns whether one of them claimed
    /// the press.
    fn handle_left_press_panels(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        // The failed-save banner's dismiss control.
        if self
            .save_error
            .is_some_and(|close| close.contains(self.pointer))
        {
            self.save_error = None;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // A drawer over the page dismisses on a press on its scrim, instead
        // of placing a caret through it.
        if !self.styles_panel_full_window() && self.inspector_drawer.press_dismisses(self.pointer) {
            self.view.styles_panel_visible = false;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if !self.styles_panel_full_window() && self.outline_drawer.press_dismisses(self.pointer) {
            if self.outline.is_open() {
                self.outline.toggle();
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Styles panel (right side): dispatch a click on its controls.
        if self.view.styles_panel_visible && self.panel_rect.contains(self.pointer) {
            if let Some(hit) = self
                .panel_hits
                .iter()
                .rev()
                .find(|(r, _)| r.contains(self.pointer))
                .map(|(_, h)| h.clone())
            {
                self.apply_panel_hit(state, hit);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Outline / navigation pane (left side): a click on an entry
        // scrolls that heading into view.
        if self.outline.contains(self.pointer) {
            if let Some(block_id) = self.outline.heading_at(self.pointer).map(str::to_owned) {
                self.scroll_to_block(&block_id);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// The Find bar's fields, the ruler band, the toolbar (zoom stepper +
    /// buttons), and the bottom layout bar — in that priority order. Returns
    /// whether one of them claimed the press.
    fn handle_left_press_find_ruler_toolbar(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        tb_h: f64,
        handled: &mut bool,
    ) -> bool {
        // Find bar (when open): dispatch clicks on its fields + controls.
        // The recovery banner (when shown) sits above the find bar, so the
        // find band starts below it.
        if self.find.is_open() && self.find_band.contains(self.pointer) {
            let _ = state;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Ruler band (when on): grab a marker to drag, else a click in the
        // page span adds a tab stop on the current paragraph.
        if self.view.ruler_visible && self.ruler.rect().contains(self.pointer) {
            self.ruler_press(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        if self.pointer.y < self.rect.y0 + tb_h {
            *handled = true;
            return true;
        }
        // Bottom layout bar sits at the very bottom; document status floats above it.
        let lbar_top = self.rect.y1 - self.layout_bar_height;
        if self.pointer.y >= lbar_top {
            if let Some(mode) = self
                .layout_chips
                .iter()
                .find(|(_, r)| r.contains(self.pointer))
                .map(|(m, _)| *m)
            {
                self.toggle_layout_mode(mode);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// The page-body left press: right-margin review affordances, then (in a
    /// layout-edit mode) grabbing a guide, then the header/footer
    /// edit-context switch, and finally the text-selection gesture.
    fn handle_left_press_body(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        // Page-content hit-tests work in unscaled content space; map the
        // pointer through the inverse zoom transform once here.
        let cp = self.pointer_to_content(self.pointer);
        // Right-margin review affordances (comment Resolve, change
        // Accept/Reject) are drawn through the zoom transform, so they
        // hit-test in content space too. A hit dispatches + consumes the
        // click before it can start a text selection.
        if let Some(action) = self.review.action_at(self.pointer) {
            self.dispatch_review_action(action, state);
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // In a layout-edit mode, grabbing a guide starts a drag instead of
        // a text selection.
        if self.layout_mode != LayoutMode::None
            && let Some(side) = self.hit_guide(cp)
        {
            let kind = if ctrl {
                DragKind::Proportional
            } else if shift {
                DragKind::Single
            } else {
                DragKind::Pair
            };
            self.guide_drag = Some(GuideDrag {
                mode: self.layout_mode,
                side,
                kind,
            });
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // Direct manipulation for free page objects. The laid rectangles and
        // pointer were both converted to unscaled content space, so this
        // selection never drifts at non-100% zoom.
        let free_image = self
            .laid
            .iter()
            .rev()
            .find(|laid| {
                laid.kind == "image"
                    && Rect::new(
                        laid.text_x,
                        laid.y_top,
                        laid.text_x + laid.width,
                        laid.y_top + laid.height,
                    )
                    .contains(cp)
            })
            .and_then(|image| {
                self.blocks
                    .iter()
                    .find(|block| block.id == image.id)
                    .filter(|block| image_anchor(block) == "page")
                    .and_then(image_free_position)
                    .map(|(x, y, page)| (image.id.clone(), x, y, page))
            });
        if let Some((id, x, y, page)) = free_image {
            self.inner.clear_anchor();
            self.inner.set_caret(Caret {
                block: id.clone(),
                offset: 0,
            });
            self.object_drag = Some(ObjectDrag {
                id,
                start_pointer: cp,
                start_x: x,
                start_y: y,
                page,
                x,
                y,
                moved: false,
            });
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // Edit-context switch: a click into a page header/footer band
        // focuses that running region (creating an empty paragraph if the
        // region is empty); a click in the body while editing a running
        // region returns to the body. This is the entry point to Word-style
        // header/footer editing.
        let band = self.band_at_pointer(cp);
        match band {
            Some(region) if region != self.inner.region() => {
                // Switch into the band. `focus_region` seats the caret on
                // the region's (possibly newly-created) first block; the
                // band lays out next frame, after which clicks refine the
                // exact offset.
                self.inner.focus_region(state, region);
                self.refresh_blocks(state);
                self.drag.press(self.pointer, shift);
                self.lifecycle.dirty = true;
                *handled = true;
                return;
            }
            None if self.inner.in_running_region() => {
                // Click in the body area exits header/footer editing.
                self.inner.focus_body(state);
                self.refresh_blocks(state);
                // Fall through so the same click also seats the body caret.
            }
            _ => {}
        }
        // Selection gesture (ui::DragSelect): shift extends; double/triple
        // click select word/paragraph; a plain press starts a fresh drag.
        let intent = self.drag.press(self.pointer, shift);
        self.caret_from_pointer(cp, intent.extend);
        match intent.granularity {
            Granularity::Word => self.inner.select_word(state),
            Granularity::Line => self.select_all_in_block(state),
            Granularity::Char => {}
        }
        self.lifecycle.dirty = true;
        *handled = true;
    }

    /// Left release: persist whichever live drag was active (style-field
    /// scrub, ruler marker, or layout guide) into the document, or — with no
    /// drag active — end a text-selection drag.
    fn handle_left_release(&mut self, state: &mut ui::widget::Edits<'_>, handled: &mut bool) {
        if false {
        } else if let Some(drag) = self.object_drag.take() {
            if drag.moved {
                self.perform_action(
                    state,
                    &crate::actions::set_image_placement_action(
                        &drag.id,
                        "page",
                        "none",
                        num::i64_of_f64(drag.x.round()),
                        num::i64_of_f64(drag.y.round()),
                        num::i64_of_usize(drag.page),
                    ),
                );
                self.refresh_blocks(state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
        } else if self.ruler.is_dragging() {
            // A ruler-marker drag persists on release (margins → page meta,
            // indents / tab stops → the caret block's attrs).
            self.ruler_drag_release(state);
            self.lifecycle.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag.take() {
            // A layout edit persists into the document on release: spacing
            // into the stylesheet, margins into the page-setup meta.
            match gd.mode {
                LayoutMode::Spacing => {
                    self.persist_stylesheet_via_actions(state);
                }
                LayoutMode::Margins => {
                    self.persist_margins(state);
                }
                LayoutMode::None => {}
            }
            self.lifecycle.dirty = true;
            *handled = true;
        } else if self.drag.release() {
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    /// A key press once the guard checks (focus, Escape, a numeric-field
    /// edit in progress, the Find bar, Ctrl+Enter's page break) and any
    /// Ctrl+<char> shortcut have both passed it through: motion keys extend
    /// or clear the selection, editing keys mutate the document, and any
    /// other printable text is inserted.
    fn autocorrect_before_boundary(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
    ) -> Option<DocumentAutocorrection> {
        if !ui::text_assistance::preferences().auto_correct || self.inner.has_selection() {
            return None;
        }
        let text = self.inner.current_text(state);
        let chars: Vec<char> = text.chars().collect();
        let end = self.inner.caret.offset.min(chars.len());
        let mut start = end;
        while start > 0
            && chars
                .get(start.saturating_sub(1))
                .is_some_and(|character| character.is_alphabetic() || *character == '\'')
        {
            start = start.saturating_sub(1);
        }
        if start == end {
            return None;
        }
        let original: String = chars.iter().skip(start).take(end - start).collect();
        let dict = self.dictionary();
        if dict.contains(&original) {
            return None;
        }
        let replacement = spellcheck::probable_correction(&self.proofing.language, &original)?;
        let block = self.inner.caret.block.clone();
        let region = self.inner.region_kind();
        self.splice_via_action(state, region, &block, start, end, &replacement);
        let replacement_end = start.saturating_add(replacement.chars().count());
        self.inner.caret.offset = replacement_end;
        Some(DocumentAutocorrection {
            block,
            start,
            end: replacement_end,
            boundary_end: replacement_end,
            original,
            replacement,
        })
    }

    /// Apply markdown-style paragraph prefixes through typed absolute actions.
    /// The two field-granular envelopes share the current input event's undo
    /// boundary; caret movement remains presentation-local.
    fn autoformat_on_space_via_actions(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let block = self.inner.caret.block.clone();
        let offset = self.inner.caret.offset;
        let Some(view) = self.blocks.iter().find(|entry| entry.id == block) else {
            return false;
        };
        if view.kind != "paragraph" {
            return false;
        }
        let prefix: String = view.text.chars().take(offset).collect();
        let (kind, level, ordered) = match prefix.as_str() {
            "-" | "*" | "+" => ("list_item", None, Some(false)),
            "1." | "1)" => ("list_item", None, Some(true)),
            "#" => ("heading", Some(1), None),
            "##" => ("heading", Some(2), None),
            "###" => ("heading", Some(3), None),
            _ => return false,
        };
        let region = self.inner.region_kind();
        self.splice_via_action(state, region, &block, 0, offset, "");
        self.perform_action(
            state,
            &crate::actions::set_paragraph_style_action(
                region,
                vec![block.clone()],
                kind,
                level,
                ordered,
            ),
        );
        self.inner.set_caret(Caret { block, offset: 0 });
        true
    }

    fn undo_document_autocorrection(&mut self, state: &mut ui::widget::Edits<'_>) -> bool {
        let Some(mut correction) = self.last_autocorrection.clone() else {
            return false;
        };
        // Enter commits a word by splitting the block. Immediate Backspace
        // first rejoins that empty/new block, then restores the spelling.
        if self.inner.caret.block != correction.block && self.inner.caret.offset == 0 {
            self.delete_via_action(state, true);
            correction.boundary_end = self.inner.caret.offset;
        }
        if self.inner.caret.block != correction.block
            || self.inner.caret.offset != correction.boundary_end
        {
            self.last_autocorrection = None;
            return false;
        }
        let text = self.inner.current_text(state);
        let corrected: String = text
            .chars()
            .skip(correction.start)
            .take(correction.end.saturating_sub(correction.start))
            .collect();
        if corrected != correction.replacement {
            self.last_autocorrection = None;
            return false;
        }
        let region = self.inner.region_kind();
        self.splice_via_action(
            state,
            region,
            &correction.block,
            correction.start,
            correction.boundary_end,
            &correction.original,
        );
        self.inner.caret.offset = correction
            .start
            .saturating_add(correction.original.chars().count());
        self.last_autocorrection = None;
        true
    }

    fn handle_key_press(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        logical_key: &Key,
        text: Option<&str>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.handle_key_press_guards(state, logical_key, ctrl, handled) {
            return;
        }
        if ctrl && let Key::Character(s) = logical_key {
            self.handle_ctrl_shortcut(state, s, shift, handled);
            return;
        }
        if !matches!(logical_key, Key::Named(NamedKey::Backspace)) {
            self.last_autocorrection = None;
        }
        let cached_revision_current = self.materialized.heads == state.base_heads()
            && self.materialized.pending_ops == state.pending_ops();
        let text_block = (cached_revision_current
            && !self.inner.has_selection()
            && matches!(self.inner.region(), Region::Body)
            && (text.is_some()
                || (matches!(logical_key, Key::Named(NamedKey::Backspace))
                    && self.inner.caret().offset > 0)))
            .then(|| self.inner.caret().block.clone());
        let effect = match logical_key {
            Key::Named(named) => self
                .handle_named_edit_key(state, *named, shift)
                .unwrap_or_else(|| {
                    if self.handle_navigation_key(state, *named, shift) {
                        KeyPressEffect::Motion
                    } else {
                        self.insert_key_text(state, text)
                    }
                }),
            _ => self.insert_key_text(state, text),
        };
        if effect != KeyPressEffect::Ignored {
            if effect == KeyPressEffect::Edit {
                // editing consumes/collapses any selection
                self.inner.clear_anchor();
            }
            if effect == KeyPressEffect::Edit {
                self.refresh_blocks_after_text_edit(state, text_block.as_deref());
            } else {
                self.refresh_blocks(state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
        }
    }

    fn handle_named_edit_key(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        key: NamedKey,
        shift: bool,
    ) -> Option<KeyPressEffect> {
        match key {
            NamedKey::Tab => Some(self.handle_tab_key(state, shift)),
            NamedKey::Enter => {
                let correction = self.autocorrect_before_boundary(state);
                self.enter_via_action(state);
                self.last_autocorrection = correction;
                Some(KeyPressEffect::Edit)
            }
            NamedKey::Backspace => {
                if !self.undo_document_autocorrection(state) {
                    self.delete_via_action(state, true);
                }
                Some(KeyPressEffect::Edit)
            }
            NamedKey::Delete => {
                self.delete_via_action(state, false);
                Some(KeyPressEffect::Edit)
            }
            NamedKey::Space => {
                let correction = self.autocorrect_before_boundary(state);
                if !self.autoformat_on_space_via_actions(state) {
                    self.insert_text_via_action(state, " ");
                }
                self.last_autocorrection = correction.map(|mut correction| {
                    correction.boundary_end = self.inner.caret.offset;
                    correction
                });
                Some(KeyPressEffect::Edit)
            }
            _ => None,
        }
    }

    fn handle_tab_key(&mut self, state: &mut ui::widget::Edits<'_>, shift: bool) -> KeyPressEffect {
        // Inside a table, Tab/Shift-Tab walk cells (and Tab in the last cell
        // appends a row). Paragraph tab stops insert a tab; otherwise use the
        // conventional four-space fallback.
        let table_target = self.caret_table_target();
        let append_row = !shift
            && table_target
                .as_ref()
                .is_some_and(|(_, row, column, rows, columns)| {
                    row.saturating_add(1) == *rows && column.saturating_add(1) == *columns
                });
        if append_row {
            if let Some((_, _, _, rows, _)) = table_target {
                self.edit_table_via_action(
                    state,
                    crate::actions::TableEdit::InsertRow { at: rows },
                );
            }
            KeyPressEffect::Motion
        } else if self.inner.tab_cell(state, !shift) {
            KeyPressEffect::Motion
        } else {
            let text = if self.inner.caret_tab_stops(state).is_empty() {
                "    "
            } else {
                "\t"
            };
            self.insert_text_via_action(state, text);
            KeyPressEffect::Edit
        }
    }

    fn handle_navigation_key(&mut self, state: &AutoCommit, key: NamedKey, shift: bool) -> bool {
        match key {
            NamedKey::ArrowLeft => {
                self.extend_or_clear(shift);
                self.inner.move_left(state);
            }
            NamedKey::ArrowRight => {
                self.extend_or_clear(shift);
                self.inner.move_right(state);
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown => {
                self.extend_or_clear(shift);
                let delta = if matches!(key, NamedKey::ArrowDown) {
                    1
                } else {
                    -1
                };
                if !self.inner.cell_vertical(state, delta > 0) {
                    self.inner.move_block_relative(state, delta);
                }
            }
            NamedKey::Home => {
                self.extend_or_clear(shift);
                self.inner.move_home();
            }
            NamedKey::End => {
                self.extend_or_clear(shift);
                self.inner.move_end(state);
            }
            _ => return false,
        }
        true
    }

    fn insert_key_text(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        text: Option<&str>,
    ) -> KeyPressEffect {
        let typed: String = text
            .map(|text| {
                text.chars()
                    .filter(|character| !character.is_control())
                    .collect()
            })
            .unwrap_or_default();
        if typed.is_empty() {
            return KeyPressEffect::Ignored;
        }
        let correction = typed
            .chars()
            .next()
            .filter(|character| character.is_ascii_punctuation() && *character != '\'')
            .and_then(|_| self.autocorrect_before_boundary(state));
        self.insert_text_via_action(state, &typed);
        self.last_autocorrection = correction.map(|mut correction| {
            correction.boundary_end = self.inner.caret.offset;
            correction
        });
        KeyPressEffect::Edit
    }

    /// Native word and visual-line selection use the toolkit's existing
    /// Unicode boundaries and shaped line geometry, preserving the anchor.
    fn handle_modified_navigation(
        &mut self,
        state: &AutoCommit,
        key: &Key,
        modifiers: Modifiers,
    ) -> bool {
        if !self.lifecycle.focused || self.editing_field.is_some() || self.find.is_open() {
            return false;
        }
        let Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) = key else {
            return false;
        };
        let mods = modifiers.state();
        let line = mods.super_key();
        let word = mods.alt_key() || mods.control_key();
        if !line && !word {
            return false;
        }
        let left = matches!(key, Key::Named(NamedKey::ArrowLeft));
        let text = self.inner.current_text(state);
        let offset = self.inner.caret.offset;
        let byte = char_to_byte(&text, offset);
        let destination = if line {
            let layout = if self.inner.region() == Region::Body {
                self.layout_cache
                    .get(&self.inner.caret.block)
                    .filter(|_| {
                        self.laid
                            .iter()
                            .any(|block| block.id == self.inner.caret.block && block.text == text)
                    })
                    .map(|(_, layout)| layout)
            } else {
                self.region_laid
                    .iter()
                    .find(|block| block.id == self.inner.caret.block && block.text == text)
                    .map(|block| &block.layout)
            };
            let edges = layout.map_or((0, text.len()), |layout| layout.visual_line_edges(byte));
            byte_to_char(&text, if left { edges.0 } else { edges.1 })
        } else if left {
            ui::text_selection::prev_word_boundary_chars(&text, offset)
        } else if mods.alt_key() {
            // Option+Right follows macOS word-end semantics; Ctrl+Right
            // retains the shared next-word-start convention on other hosts.
            let (_, end) = ui::text_selection::word_boundaries(&text, byte);
            let end = if end > byte {
                end
            } else {
                let next = ui::text_selection::next_word_boundary(&text, byte);
                ui::text_selection::word_boundaries(&text, next).1.max(next)
            };
            byte_to_char(&text, end)
        } else {
            ui::text_selection::next_word_boundary_chars(&text, offset)
        };
        self.extend_or_clear(mods.shift_key());
        if !line && destination == offset {
            if left {
                self.inner.move_left(state);
            } else {
                self.inner.move_right(state);
            }
        } else {
            self.inner.caret.offset = destination;
        }
        self.lifecycle.dirty = true;
        true
    }

    /// Early exits before a key press reaches editing: an unfocused widget,
    /// Escape (closing an open menu, or exiting header/footer editing), a
    /// numeric-field edit in progress, the Find bar capturing non-Ctrl keys,
    /// and Ctrl+Enter's manual page break. Returns whether one of these fully
    /// handled the event (the caller should stop dispatching further).
    fn handle_key_press_guards(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        logical_key: &Key,
        ctrl: bool,
        handled: &mut bool,
    ) -> bool {
        if !self.lifecycle.focused {
            return true;
        }
        // A free-object drag is a preview until release. Escape discards that
        // preview (rather than committing an accidental move) and leaves the
        // document/action history untouched.
        if matches!(logical_key, Key::Named(NamedKey::Escape)) && self.object_drag.is_some() {
            self.object_drag = None;
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Escape dismisses an open context / style menu before anything else.
        if matches!(logical_key, Key::Named(NamedKey::Escape))
            && (self.context_menu.is_open()
                || self.style_menu.is_open()
                || self.template_picker.is_open())
        {
            self.context_menu.close();
            self.style_menu.close();
            self.template_picker.close();
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // Escape exits header/footer editing, returning to the body.
        if self.inner.in_running_region() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.inner.focus_body(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        // While a style field has the keyboard, the document does not.
        if self.editing_field.is_some() {
            *handled = true;
            return true;
        }
        // Ctrl+Enter inserts a manual page break: the caret block splits
        // and the new block is marked to start on a fresh page. (Handled
        // here because `NamedKey::Enter` is not a `Key::Character`.)
        if ctrl && matches!(logical_key, Key::Named(NamedKey::Enter)) {
            self.page_break_via_action(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// Ctrl+<char> shortcuts while the keyboard has focus: save/open/find,
    /// spell-check/markup view toggles, zoom, page-number insert, and the
    /// formatting/selection shortcuts dispatched through the same actions the
    /// toolbar uses. Always consumes the event once `ctrl` holds a
    /// `Character` key, whether or not `s` matched a known shortcut (so an
    /// unrecognized Ctrl+<char> doesn't fall through to plain typing).
    fn handle_ctrl_shortcut(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        s: &str,
        shift: bool,
        handled: &mut bool,
    ) {
        // Save is not an editing action — don't mark the doc
        // modified again after writing it. Ctrl+Shift+S always
        // opens the host Save-As picker; a bare Ctrl+S saves to
        // the current path (or, when untitled, prompts via the
        // same picker rather than inventing a path).
        if s.eq_ignore_ascii_case("s") {
            self.invoke_effect(&if shift {
                WordProcessorEffect::SaveAs { path: None }
            } else {
                WordProcessorEffect::Save { path: None }
            });
            *handled = true;
            return;
        }
        // Ctrl+O opens a document in place of the current one.
        if s.eq_ignore_ascii_case("o") {
            self.invoke_effect(&WordProcessorEffect::OpenDocument { path: None });
            *handled = true;
            return;
        }
        if s.eq_ignore_ascii_case("f") {
            if self.find.is_open() {
                self.close_find();
            } else {
                self.open_find(state);
            }
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // Ctrl+Shift+L toggles the spell-check squiggles (view-only,
        // default ON). Never mutates the document.
        if shift && s.eq_ignore_ascii_case("l") {
            self.toggle_spellcheck();
            *handled = true;
            return;
        }
        // Ctrl+Shift+M toggles comment + tracked-change markup
        // (view-only, default ON). Never mutates the document.
        if shift && s.eq_ignore_ascii_case("m") {
            self.toggle_markup();
            *handled = true;
            return;
        }
        // Ctrl+Shift+P inserts a page-number field at the caret.
        // Primarily for header/footer editing (a running "Page N"),
        // but works in the body too (the field renderer substitutes
        // the page number per page).
        if shift && s.eq_ignore_ascii_case("p") {
            self.insert_page_number_via_action(state);
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
            *handled = true;
            return;
        }
        // Zoom: Ctrl+= / Ctrl++ (in), Ctrl+- (out), Ctrl+0 (reset
        // to 100%). These are view-only — they never mutate the doc,
        // so they return before the editing path below.
        match s {
            "=" | "+" => {
                self.zoom_by(ZOOM_STEP);
                *handled = true;
                return;
            }
            "-" | "_" => {
                self.zoom_by(-ZOOM_STEP);
                *handled = true;
                return;
            }
            "0" => {
                self.set_zoom(1.0);
                *handled = true;
                return;
            }
            _ => {}
        }
        match s.to_lowercase().as_str() {
            // Formatting shortcuts dispatch the same
            // `RichTextAction` the toolbar + Ctrl+K palette
            // do (one replayable source of truth).
            "b" => self.dispatch_toggle_mark(state, "strong"),
            "i" => self.dispatch_toggle_mark(state, "em"),
            "u" => self.dispatch_toggle_mark(state, "underline"),
            // Ctrl+. → superscript, Ctrl+, → subscript (mutually
            // exclusive; matches common word-processor bindings).
            "." | ">" => self.dispatch_exclusive_mark(state, "super", "sub"),
            "," | "<" => self.dispatch_exclusive_mark(state, "sub", "super"),
            "c" => {
                self.clipboard_copy(state);
            }
            "x" => {
                if self.clipboard_copy(state) {
                    self.insert_text_via_action(state, "");
                }
            }
            // Ctrl+V keeps formatting; Ctrl+Shift+V pastes plain text.
            "v" if shift => self.clipboard_paste_plain(state),
            "v" => self.clipboard_paste(state),
            "a" => self.select_all_via_primitive(state),
            // Ctrl+1/2/3 → heading levels. (Ctrl+0 is zoom-reset;
            // Body paragraph is reachable via the toolbar / panel.)
            "1" => self.dispatch_paragraph_style(state, "heading", Some(1), None),
            "2" => self.dispatch_paragraph_style(state, "heading", Some(2), None),
            "3" => self.dispatch_paragraph_style(state, "heading", Some(3), None),
            // z / y (undo/redo) are handled by the host App.
            _ => {}
        }
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        *handled = true;
    }
}

/// Whether `effect` answers its caller once its edit applied.
fn answers_later(effect: &WordProcessorEffect) -> bool {
    use ui::tilo_resource::Effect as _;
    let id = effect.id();
    WordProcessorEffect::declarations()
        .iter()
        .any(|declaration| declaration.id == id && declaration.is_job())
}

impl WordProcessorWidget {
    /// Apply a synchronous command or view operation. Returns `Ok(false)`
    /// when `type_id` is not this widget's.
    /// Invoke `effect` through the document's door, as the palette or an
    /// agent does: checked, and answered by [`Self::answer_effect`].
    fn invoke_effect(&mut self, effect: &WordProcessorEffect) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.effect(effect);
        }
        self.lifecycle.dirty = true;
    }

    /// Answer `effect`, however it was invoked.
    pub(crate) fn answer_effect(
        &mut self,
        effect: WordProcessorEffect,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
    ) -> ui::framework::EffectAnswer {
        use ui::framework::Effected;
        if let Some(answer) = self.answer_edit_effect(&effect, cx) {
            return answer;
        }
        if let Some(movement) = effect.view_operation()? {
            let doc = cx.document().ok_or("the document is not at hand")?.clone();
            // A delivered invocation by a participant (an agent task) moves
            // only that participant's view; without one it is this window's.
            match ui::invocation_scope::current_participant() {
                Some(participant) => self.move_participant_view(&participant, &movement, &doc)?,
                None => self.apply_local_view_operation(&doc, &movement)?,
            }
            self.lifecycle.dirty = true;
            return Ok(Effected::done());
        }
        match effect {
            WordProcessorEffect::OpenDocument { path: Some(path) } => {
                self.open_in_place(cx, std::path::Path::new(&path))?;
            }
            WordProcessorEffect::OpenDocument { path: None } => {
                return Ok(
                    cx.choose_path(open_request(), |editor, path, cx| match path {
                        Some(path) => {
                            editor.open_in_place(cx, &path)?;
                            Ok(Effected::done())
                        }
                        None => Err("the file picker was cancelled".into()),
                    }),
                );
            }
            WordProcessorEffect::Save { path } => {
                match path.map(PathBuf::from).or_else(|| self.path.clone()) {
                    Some(path) => self.save_now(cx, &path)?,
                    None => return Ok(self.choose_where_to_save(cx)),
                }
            }
            WordProcessorEffect::SaveAs { path: Some(path) } => {
                self.save_now(cx, std::path::Path::new(&path))?;
            }
            WordProcessorEffect::SaveAs { path: None } => {
                return Ok(self.choose_where_to_save(cx));
            }
            WordProcessorEffect::Find => {
                let doc = cx.document().ok_or("the document is not at hand")?.clone();
                self.open_find(&doc);
            }
            WordProcessorEffect::ZoomIn => {
                self.zoom_by(0.1);
            }
            WordProcessorEffect::ZoomOut => {
                self.zoom_by(-0.1);
            }
            WordProcessorEffect::ZoomReset => {
                self.set_zoom(1.0);
            }
            WordProcessorEffect::Spellcheck => self.toggle_spellcheck(),
            WordProcessorEffect::Markup => self.toggle_markup(),
            WordProcessorEffect::Styles => {
                self.view.styles_panel_visible = !self.view.styles_panel_visible;
            }
            WordProcessorEffect::Ruler => self.view.ruler_visible = !self.view.ruler_visible,
            WordProcessorEffect::Outline => self.outline.toggle(),
            other => return Err(format!("`{}` is not handled here", other.id())),
        }
        self.lifecycle.dirty = true;
        Ok(Effected::done())
    }

    /// Replace the document with the file at `path` (its format by
    /// extension), as one action Undo takes back; the file becomes where
    /// Save writes.
    fn open_in_place(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let bytes = tilo_files::file_at(path)
            .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
            .map_err(|error| format!("could not open {}: {error}", path.display()))?;
        let format = WpFormat::from_path(path);
        let action = crate::import_action(&bytes, format, self.inner.content_store())?;
        let dispatch = cx.dispatcher();
        dispatch.group(|| {
            dispatch.submit(crate::model::edit(&action));
            dispatch.invoke(crate::model::Document::ADOPT_COMMENTS.invoke());
        });
        self.set_file(path.to_path_buf(), format);
        self.scroll_y = 0.0;
        // The whole body changes; per-block caches keyed by id/text go stale.
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.spell_cache.clear();
        Ok(())
    }

    /// Write the document to `path` now, from the document lent to the
    /// effect.
    fn save_now(
        &mut self,
        cx: &ui::framework::EffectCx<crate::framework::Document, Self>,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let doc = cx.document().ok_or("the document is not at hand")?.clone();
        self.try_write_to_path(&doc, path, WpFormat::from_path(path))
    }

    /// Ask the person where to save with the host's chooser; the chosen file
    /// is written on the next sync, which answers the caller.
    fn choose_where_to_save(
        &self,
        cx: &mut ui::framework::EffectCx<crate::framework::Document, Self>,
    ) -> ui::framework::Effected {
        let suggested = self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "Untitled.md".to_string(),
            |f| f.to_string_lossy().into_owned(),
        );
        cx.choose_path(save_request(&suggested), |editor, path, cx| match path {
            Some(path) => {
                editor.pending_save = Some((path, cx.reply()));
                editor.lifecycle.dirty = true;
                Ok(ui::framework::Effected::Pending)
            }
            None => Err("the file picker was cancelled".into()),
        })
    }
}

impl Widget for WordProcessorWidget {
    ui::provides!(
        dyn ui::collaboration::participants::Participants,
        dyn ui::undo::LocalUndo,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::presence::capability::PublishesPresence
    );

    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::semantic_read::action::READERS
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        if self.styles_panel_full_window() {
            let viewport = self.panel_body.height();
            return Some(ui::widget::PrimaryScrollRegion {
                key: "styles",
                owner_generation: None,
                metrics: ui::widget::ScrollMetrics::new(
                    self.panel_max_scroll + viewport,
                    viewport,
                    self.panel_scroll,
                ),
            });
        }
        Some(ui::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: ui::widget::ScrollMetrics::new(
                self.max_scroll + self.scroll_viewport,
                self.scroll_viewport,
                self.scroll_y,
            ),
        })
    }

    #[expect(
        clippy::float_cmp,
        reason = "the clamped target is compared exactly: identical means the scroll did not move"
    )]
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        use ui::widget::PrimaryScrollResult;
        let Some(region) = self.primary_vertical_scroll() else {
            return PrimaryScrollResult::Unsupported;
        };
        let Some(next) = action.offset(region.metrics) else {
            return PrimaryScrollResult::Unsupported;
        };
        if next == region.metrics.offset {
            return PrimaryScrollResult::Unchanged(region);
        }
        if self.styles_panel_full_window() {
            self.panel_scroll = next;
        } else {
            self.scroll_y = next;
        }
        self.lifecycle.dirty = true;
        self.primary_vertical_scroll().map_or(
            PrimaryScrollResult::Unsupported,
            PrimaryScrollResult::Changed,
        )
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        let (state, _) = ui::doc::document_ref(state, path);
        let caret = self.inner.caret();
        let region = self.inner.region();
        // Body text reads only the caret block. Running-region text would
        // materialize an entire region, so omit it from this diagnostic path.
        let caret_text = matches!(region, Region::Body).then(|| self.inner.current_text(state));
        Some(serde_json::json!({
            "offset_y": self.scroll_y,
            "max_scroll": self.max_scroll,
            "body_window_start": self.body_window_start,
            "body_window_len": self.blocks.len(),
            "body_block_count": self.body_order.len(),
            "focused": self.lifecycle.focused,
            "caret_block": caret.block,
            "caret_offset": caret.offset,
            "caret_region": match region {
                Region::Body => "body",
                Region::Header => "header",
                Region::Footer => "footer",
            },
            "has_selection": self.inner.has_selection(),
            "caret_text": caret_text,
        }))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the editor binds, rebases and reconciles in one sync pass"
    )]
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        // The toolbar is painted inside the document's authored chrome rather
        // than exposed as another accessibility child. Its deferred native
        // View requests still need the shared retained lifecycle.
        self.tool_bar.sync(state, root);
        let (state, root) = ui::doc::document_ref(state, root);
        #[cfg(test)]
        if self.source.is_none() && self.dispatch.is_some() {
            self.source = ui::framework::testing::detached_live(state).ok();
        }
        // A participant who may only view (or History mode's past version)
        // keeps its caret on what is there.
        if self.read_only() {
            self.inner.bind_view_only(state, root);
        } else {
            self.inner.bind(state, root);
        }
        if let Some((path, reply)) = self.pending_save.take() {
            let format = WpFormat::from_path(&path);
            reply.finish(
                self.try_write_to_path(state, &path, format)
                    .map(|()| tilo_actions::ActionOutput::default()),
            );
        }
        self.refresh_blocks(state);
        // Other participants' views follow local and remote edits alike; a
        // participant's own applied edit hands over its next view.
        let now = ui::time::now();
        self.participants.adopt_outcomes(state, now);
        self.adopt_local_outcome(state);
        self.participants.rebase(state);
        self.participants.expire(now);
        self.resolve_bound_edits(state);
        self.follow_settings();
        // Reload the stylesheet from the doc unless a live, not-yet-persisted
        // local edit is in progress (a guide drag, a numeric scrub, or typing
        // into a panel field) — reloading would discard the live preview.
        if self.guide_drag.is_none() && self.editing_field.is_none() {
            self.sheet = self.inner.stylesheet(state);
        }
        // Reload the persisted page margins (document-level page setup) unless a
        // guide drag is mid-flight (which would clobber the live preview). Absent
        // from the document → keep the current/default margins.
        if self.guide_drag.is_none()
            && let Some([t, r, b, l]) = self.inner.page_margins(state)
        {
            self.margins = Edges {
                top: t,
                right: r,
                bottom: b,
                left: l,
            };
        }
        self.sync_toolbar();
        self.lifecycle.dirty = true;
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        self.tool_bar.settle(&mut edits.reborrow(), path);
        if let Some(action) = self.tool_fired.take() {
            self.tool_clicked(edits, action);
        }
        self.drain_image_choice(edits);
        if let Some(scenario) = self.staged_scenario.take() {
            self.run_staged_scenario(edits, &scenario);
        }
        self.refresh_blocks(edits.doc());
        self.sync_toolbar();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.window_history.tree_rebuilt();
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Authored chrome keeps its retained toolkit children available to the
    /// host's normal resource, custody and invocation traversal.
    fn children(&self) -> Vec<&dyn Widget> {
        if self.styles_panel_full_window() {
            return Vec::new();
        }
        let mut children: Vec<&dyn Widget> = vec![&self.tool_bar];
        if let Some(button) = self.window_history.button() {
            children.push(button);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.styles_panel_full_window() {
            return Vec::new();
        }
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.tool_bar];
        if let Some(button) = self.window_history.button_mut() {
            children.push(button);
        }
        children
    }

    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        // The formatting toolbar is already authored into the document's
        // synthetic semantics; History remains its own retained control.
        child_index == 0
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut changed = std::mem::take(&mut self.retained_tree_stale);
        for child in self.children_mut() {
            changed |= child.take_structure_dirty();
        }
        changed
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn needs_animation_frame(&self) -> bool {
        // Only the outline and inspector drawers move, while they open or
        // close.
        self.outline_drawer.is_animating() || self.inspector_drawer.is_animating()
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.context_menu.is_open()
            || self.style_menu.is_open()
            || self.template_picker.is_open()
            || self.tool_bar.is_overflow_open()
            || self.line_spacing.is_open()
            || self.style_pick.is_open()
        {
            Some(self.rect)
        } else {
            None
        }
    }

    fn traps_focus(&self) -> bool {
        self.modal_capture().is_some()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the frame compositor keeps its ordered page/chrome layers together"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.ime_rect = None;
        // Resolve the OS theme into this frame's chrome palette once; every
        // chrome helper below reads `self.chrome` so the whole editor follows
        // light/dark mode + the OS palette. (`draw_frame_chrome` resolves the
        // spacing tokens and the measured panel metrics alongside it.)
        self.chrome = ChromeColors::for_theme(theme);
        self.paper_theme = theme.on_ground(self.chrome.paper);
        self.desk_theme = theme.clone();
        self.text_scale = f64::from(theme.text_scale.clamp(0.75, 2.0));
        let full = self.rect;
        let fg = self.draw_frame_chrome(scene, transform, text_renderer, theme, full);

        // Pass 1: shape + paginate every block, then resolve scroll against the
        // resulting extent and shift `laid` into screen space.
        let PaginationResult {
            mut laid,
            cache: new_cache,
            cur_page,
            y,
            blocks,
        } = self.paginate_blocks(text_renderer, &fg);
        let (num_pages, sy) = self.finish_scroll(&fg, &mut laid, cur_page, y);

        // `draw_page_sheets` pushes the canvas clip (so scrolled content never
        // paints over the toolbar/status bar) before drawing the sheets; popped
        // once every page-space pass below — sheets, pass 2, review cards,
        // footnotes, placeholder, overlays — has run.
        let page = self.draw_page_sheets(scene, transform, text_renderer, &fg, num_pages, sy);

        let (multi_sel, note_nums, spell_by_block) = self.prepare_pass2_inputs(blocks, &laid, &fg);

        let markers = self.draw_page_content(
            scene,
            &fg,
            text_renderer,
            &PageContentInput {
                laid: &laid,
                cache: &new_cache,
                multi_selection: multi_sel,
                spell_by_block: &spell_by_block,
                note_numbers: &note_nums,
            },
        );
        self.review.draw_pips(
            &mut ui::paint::PaintCtx::new(scene, fg.ctf, text_renderer, theme),
            self.chrome.paper,
            &markers.comment_markers,
        );
        self.draw_footnotes(
            scene,
            &fg,
            text_renderer,
            &markers.page_notes,
            &note_nums,
            sy,
        );

        self.draw_empty_placeholder(scene, fg.ctf, text_renderer, laid.first());

        // Layout-edit overlay: draggable guides + live value labels for the
        // active mode (drawn over the content, still inside the page clip).
        if self.layout_mode != LayoutMode::None {
            self.draw_layout_overlay(scene, fg.ctf, text_renderer, page);
        }
        // Header/footer edit mode: veil the body + draw the band caret (checks
        // `self.inner.in_running_region()` itself). Drawn last so the veil
        // covers the body but the band + its caret stay crisp on top.
        self.draw_running_region_veil(scene, &fg, num_pages, sy);

        self.draw_review_column(
            scene,
            (transform, &fg),
            text_renderer,
            theme,
            presence,
            &markers,
        );

        scene.pop_layer();

        self.draw_scrollbar(scene, transform, fg.canvas);

        self.laid = laid;
        // Adopt this frame's cache (dropping the pruned entries for blocks that
        // no longer exist) so the next frame can reuse it.
        self.layout_cache = new_cache;

        self.draw_remote_overlays(
            scene,
            fg.ctf,
            fg.canvas_clip,
            text_renderer,
            theme,
            presence,
        );

        let lbar = Rect::new(
            fg.canvas_left,
            fg.layout_bar_top,
            fg.canvas_right,
            fg.content_bottom,
        );
        self.draw_layout_bar(scene, transform, text_renderer, theme, lbar);
        self.draw_drawers(scene, transform, text_renderer, theme);
    }

    // Where the caret and selection end up is what the edits bound to them
    // are built from.
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.route_event(event, ctx);
        self.resolve_bound_edits(ctx.state.doc());
    }

    /// Top-layer pass: paint the right-click context menu (when open) so it
    /// floats above the page + chrome. Delegates to the shared [`ContextMenu`].
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.staged.template_picker {
            self.staged.template_picker = false;
            self.open_template_picker();
        }
        if self.staged.toolbar_overflow {
            self.staged.toolbar_overflow = false;
            self.open_toolbar_overflow();
        }
        // Toast first, so an open menu stacks above it.
        self.draw_toast(scene, transform, text_renderer, theme);
        let toolbar_presence = PresenceCtx::empty();
        toolbar_presence
            .viewport
            .set(Some(self.floating_viewport()));
        Widget::draw_overlay(
            &mut self.tool_bar,
            scene,
            transform,
            text_renderer,
            theme,
            &toolbar_presence,
        );
        let viewport = self.floating_viewport();
        for menu in [
            &mut self.context_menu,
            self.template_picker.menu_mut(),
            self.style_menu.menu_mut(),
        ] {
            if menu.is_open() {
                menu.constrain_to(viewport);
            }
        }
        if self.context_menu.is_open() {
            Widget::draw(
                &mut self.context_menu,
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        if self.styles_panel_visible() {
            let local = if self.styles_panel_overlay() {
                theme.on_ground(ui::chrome::panel_ground(theme))
            } else {
                theme.in_region(ui::surface::Region::Inspector)
            };
            let overlay_presence = PresenceCtx::empty();
            overlay_presence
                .viewport
                .set(Some(self.floating_viewport()));
            for menu in [&mut self.line_spacing, &mut self.style_pick] {
                if menu.is_open() {
                    Widget::draw_overlay(
                        menu,
                        scene,
                        transform,
                        text_renderer,
                        &local,
                        &overlay_presence,
                    );
                }
            }
        }
        self.compact_swatch_palette
            .draw(scene, transform, text_renderer, theme, presence);
        self.template_picker
            .draw(scene, transform, text_renderer, theme, presence);
        self.style_menu
            .draw(scene, transform, text_renderer, theme, presence);
        if self.find.is_open() {
            self.find
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.color_picker.draw(
            crate::style_color_picker::DrawArgs {
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            },
            self.rect,
            self.panel_rect,
        );
    }

    fn name(&self) -> &'static str {
        "WordProcessor"
    }

    // A test reaches the editor its framework app hosts.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// The word processor's local accessibility semantics: the editable
    /// `Role::Document` root. Blocks render directly, not as child widgets, so
    /// the full subtree comes from [`WordProcessorWidget::accessibility_tree`]
    /// rather than `children()`; convert the carrier into real synthetic
    /// children so AccessKit, AT-SPI and the built-in reader all receive it.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut tree = self.accessibility_tree();
        if !self.styles_panel_full_window() {
            tree.node = tree.node.with_name(t("a11y-document-editor"));
        }
        Some(tree.into_access_node())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        fn published(node: &ui::a11y::AccessNode, target: &str) -> bool {
            node.action_target.as_deref() == Some(target)
                || node.children.iter().any(|child| published(child, target))
        }
        if !self
            .accessibility()
            .is_some_and(|node| published(&node, target))
        {
            return false;
        }
        self.perform_action_at(action, &[ui::a11y::hash_key(&target)], state, path)
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if let Some(result) = self.perform_styles_page_action(action, key_path, state) {
            return result;
        }
        if matches!(action, ui::a11y::Action::Focus) {
            if let Some(target) = key_path.last().copied() {
                let palette_target = if ui::a11y::hash_key(&"compact-swatch:clear") == target {
                    Some("compact-swatch:clear".to_string())
                } else {
                    (0..STYLE_PALETTE.len().max(HIGHLIGHT_PALETTE.len()))
                        .map(|index| format!("swatch:{index}"))
                        .find(|id| ui::a11y::hash_key(id) == target)
                };
                if let Some(id) = palette_target
                    && self.compact_swatch_palette.focus_target(&id, state, path)
                {
                    self.lifecycle.dirty = true;
                    return true;
                }
            }
            self.set_focus(true);
            return true;
        }
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        let Some(target) = key_path.last().copied() else {
            return false;
        };
        let compact_palette_target = if ui::a11y::hash_key(&"compact-swatch:clear") == target {
            Some("compact-swatch:clear".to_string())
        } else {
            (0..STYLE_PALETTE.len().max(HIGHLIGHT_PALETTE.len()))
                .map(|index| format!("swatch:{index}"))
                .find(|id| ui::a11y::hash_key(id) == target)
        };
        if let Some(id) = compact_palette_target
            && let Some((kind, choice)) = self
                .compact_swatch_palette
                .semantic_choice(&id, action, state, path)
        {
            self.apply_compact_palette_choice(state, kind, choice);
            return true;
        }
        if let Some(review_action) = self.review.hits().iter().find_map(|(_, review_action)| {
            (ui::a11y::hash_key(&review_action_target(review_action)) == target)
                .then(|| review_action.clone())
        }) {
            self.dispatch_review_action(review_action, state);
            return true;
        }
        if let Some(panel_hit) = self.panel_hits.iter().find_map(|(_, panel_hit)| {
            let id = match panel_hit {
                PanelHit::ImageInline => "inline",
                PanelHit::ImageFreePosition => "free",
                PanelHit::ImageWrapLeft => "left",
                PanelHit::ImageWrapRight => "right",
                _ => return None,
            };
            (ui::a11y::hash_key(&format!("word-processor.image-placement:{id}")) == target)
                .then(|| panel_hit.clone())
        }) {
            self.apply_panel_hit(state, panel_hit);
            return true;
        }
        let Some(tool) = self.toolbar.iter().find_map(|(tool, _)| {
            let id = format!("word-processor.toolbar:{}", tool_action_id(*tool));
            (ui::a11y::hash_key(&id) == target).then_some(*tool)
        }) else {
            return false;
        };

        // The AT route joins the exact same typed document-action/view path as
        // pointer activation. It must also establish the same history/autosave
        // boundary for mutations; otherwise screen-reader edits would silently
        // be second-class collaboration events.
        self.apply_tool(state, tool);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        true
    }

    fn test_id(&self) -> Option<&str> {
        Some("word_processor")
    }
    fn claims_raw_touch(&self, point: Point) -> bool {
        self.segmented_rects()
            .iter()
            .flatten()
            .any(|rect| rect.contains(point))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.segmented_touch.is_active()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.lifecycle.focused = focused;
        self.find.set_host_focused(focused);
        if !focused {
            // Blur any in-progress numeric edit; the next sync reloads the
            // persisted style values.
            self.focus_hosted(None, false);
        }
        self.lifecycle.dirty = true;
    }
    fn is_focused(&self) -> bool {
        self.lifecycle.focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.styles_panel_full_window() {
            self.num_inputs
                .iter()
                .find(|slot| {
                    Some(slot.field) == self.editing_field
                        && self.visible_styles_rect(slot.rect).is_some()
                })
                .and_then(|slot| Widget::ime_cursor_rect(&slot.input))
        } else if self.find.is_open() {
            self.find.ime_cursor_rect()
        } else {
            self.ime_rect
        }
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.styles_panel_full_window() {
            self.num_inputs
                .iter()
                .find(|slot| {
                    Some(slot.field) == self.editing_field
                        && self.visible_styles_rect(slot.rect).is_some()
                })
                .and_then(|slot| Widget::ime_purpose(&slot.input))
        } else if self.find.is_open() {
            self.find.ime_purpose()
        } else {
            Some(winit::window::ImePurpose::Normal)
        }
    }
    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.action_palette_request.take()
    }
    fn needs_redraw(&self) -> bool {
        self.lifecycle.dirty
    }
    fn mark_drawn(&mut self) {
        self.lifecycle.dirty = false;
    }
    /// One self-drawn taffy leaf filling its tile: its size follows the
    /// parent alone, so only the first layout is a size change. Pages,
    /// toolbars and panels are positioned inside `self.rect` by hand.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl ui::presence::capability::PublishesPresence for WordProcessorWidget {
    /// Outbound collaboration presence: while focused, broadcast the local caret
    /// (or selection range) keyed to its block. Polled by the App after each event
    /// and fanned out as `CollabEvent::RemotePresenceUpdate`. Offsets are bytes, so
    /// the receiver feeds them straight into `RichTextLayout::caret_geometry`/
    /// `selection_rects`.
    fn presence_payload(&self) -> Option<(PresenceKey, ui::PresencePayload)> {
        if !self.lifecycle.focused {
            return None;
        }
        let caret_block = &self.inner.caret.block;
        if caret_block.is_empty() {
            return None;
        }
        let key = block_presence_key(caret_block);
        // A selection within the caret's block takes priority over the bare
        // caret. (v1 collab presence reports the caret-block portion of a
        // multi-block selection, mirroring how the caret block is what the
        // peer's caret lands in.)
        if self.inner.has_selection()
            && let Some((_, s, e)) = self
                .selection_blocks_cached()
                .into_iter()
                .find(|(b, _, _)| b == caret_block)
            && let Some(block) = self.blocks.iter().find(|b| &b.id == caret_block)
            && s < e
        {
            let start = char_to_byte(&block.text, s);
            let end = char_to_byte(&block.text, e);
            return Some((key, ui::PresencePayload::Selection { start, end }));
        }
        // Bare caret: convert the char offset to a byte offset against the
        // caret block's text so it maps directly through the layout geometry.
        let pos = self
            .blocks
            .iter()
            .find(|b| &b.id == caret_block)
            .map_or(self.inner.caret.offset, |b| {
                char_to_byte(&b.text, self.inner.caret.offset)
            });
        Some((key, ui::PresencePayload::Caret { pos }))
    }
}

impl ui::resources::PublishesResources for WordProcessorWidget {
    // The document is the framework's publication; the editor adds only
    // what an edit answers its caller with.
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        _address: &tilo_actions::ActionAddress,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        Some(crate::semantic_read::changes::summary(doc, changes).into())
    }
}

/// The toolbar band is the window's header row: it holds the History control
/// the host offers, so the window draws no History bar above the editor.
impl ui::widget::WindowHeader for WordProcessorWidget {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        if self.window_history.control() != control {
            self.window_history.set_control(control);
            self.lifecycle.dirty = true;
        }
        self.retained_tree_stale |= self.window_history.take_rebuilt();
    }
}

impl ui::widget::controls::ActionControls for WordProcessorWidget {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        self.for_each_child(&mut |child| {
            bindings.extend(ui::widget::controls::control_action_bindings(child));
        });
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for child in self.children_mut() {
            ui::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

impl ui::undo::LocalUndo for WordProcessorWidget {
    fn handle_local_undo_request(&mut self, _request: ui::undo::UndoRequest) -> bool {
        // The host resolves Undo before dispatching keyboard events to us.
        // An open menu must capture that path as well as ordinary editing keys.
        self.modal_capture().is_some()
    }
}

impl ui::collaboration::participants::Participants for WordProcessorWidget {
    /// An agent task joined: its view is placed at the document end on the
    /// next sync (no document here) and published as presence from then on.
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        let joined = self.participants.join(participant.clone(), ui::time::now());
        self.lifecycle.dirty |= joined;
        joined
    }
    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.participants.remove(participant);
        self.lifecycle.dirty = true;
    }
    fn set_participant_label(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        WordProcessorWidget::set_participant_label(self, participant, label);
    }
    fn participant_presence(&self, doc: &AutoCommit) -> Vec<ui::ParticipantPresence> {
        WordProcessorWidget::participant_presence(self, doc)
    }
}

/// Lets the shell host the word processor in a tab directly, the same way
/// it hosts the terminal and the file explorer, instead of standing a
/// placeholder card in its place.
impl ui::widget::AppWidget for WordProcessorWidget {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's app-kind vocabulary has no separate "documents"
        // variant; a word processor is an editor of text, which is what
        // `Editor` names and what its tab glyph depicts.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &'static str {
        "Document"
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font document glyph, matching the convention the other
        // hosted apps use for their tab pill.
        ui::widget::IconDescriptor::Glyph("󰈬")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}

/// A comment's own text, its paragraphs joined.
fn thread_root_text(comment: &tilo_doc::snapshot::CommentView) -> String {
    comment
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lay `view` out in `x0..x1` from `top` so its text, not the editor's
/// padding around it, starts at `x0` (the edge the author's name above it
/// starts at). Returns its height.
fn lay_out_flush(
    view: &mut RichTextView,
    (x0, x1, top): (f64, f64, f64),
    tr: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    let height = view.layout(Rect::new(x0, top, x1, top), theme, tr);
    let inset = view
        .editor_mut()
        .blocks()
        .first()
        .map_or(0.0, |block| block.line.caret_rect(theme, 0).x0 - x0);
    if inset > 0.5 {
        view.layout(Rect::new(x0 - inset, top, x1 + inset, top), theme, tr)
    } else {
        height
    }
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

impl WordProcessorWidget {
    /// The margin column beside the page: a card per comment and tracked
    /// change, and the open thread expanded in place of its comment's card.
    fn draw_review_column(
        &mut self,
        scene: &mut Scene,
        (transform, fg): (Affine, &FrameGeometry),
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        markers: &Pass2Markers,
    ) {
        let to_window = self.zoom.transform(Affine::IDENTITY);
        let placement = ReviewPlacement {
            canvas: fg.canvas,
            page_x1: (to_window * Point::new(fg.page_x + fg.page_w, 0.0)).x,
            to_window,
        };
        let (_, width) = ReviewOverlay::column(theme, &placement);
        let thread = self
            .thread_comment
            .clone()
            .zip(self.comment_thread_height(width, tr, theme));
        self.review.draw_cards(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            &placement,
            (&markers.comment_markers, &markers.change_markers),
            thread.as_ref().map(|(id, height)| (id.as_str(), *height)),
        );
        if let Some(slot) = self.review.expanded_slot() {
            self.draw_comment_thread(scene, transform, tr, theme, presence, slot);
        } else {
            self.thread_panel = Rect::ZERO;
            self.thread_submit = Rect::ZERO;
            self.thread_cancel = Rect::ZERO;
        }
    }

    /// The comment whose thread is open, its replies in reading order.
    fn open_thread(&self) -> Option<tilo_doc::snapshot::CommentView> {
        let id = self.thread_comment.as_ref()?;
        let mut comment = self
            .review
            .comments()
            .iter()
            .find(|c| &c.id == id)
            .cloned()
            .or_else(|| self.thread_snapshot.clone())?;
        crate::review_overlay::sort_replies(&mut comment);
        Some(comment)
    }

    /// The open thread's card: its comment's card, expanded in place.
    fn thread_face(comment: &tilo_doc::snapshot::CommentView) -> CardFace<'_> {
        CardFace::new()
            .title(&comment.author)
            .leading_stripe(Tone::of_identity(&comment.author))
    }

    /// Lay out the open thread's card body from `top`, `x0..x1`: the
    /// comment, every reply shown (up to three) and the composer with its
    /// actions. Returns the body's height; positions the reply views and
    /// the composer.
    fn lay_out_thread_body(
        &mut self,
        comment: &tilo_doc::snapshot::CommentView,
        (x0, x1, top): (f64, f64, f64),
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let sp = &theme.tokens.spacing;
        let related = f64::from(sp.rhythm.related);
        let tight = f64::from(sp.rhythm.tight);
        let wrap = num::f32_of_f64((x1 - x0).max(1.0));
        let root = thread_root_text(comment);
        let mut y = top;
        y += f64::from(
            tr.measure_style(&root, Some(wrap), &theme.text(TextRole::Body))
                .1,
        );
        let replies: Vec<_> = comment.replies.iter().take(3).collect();
        self.thread_reply_views
            .resize_with(replies.len(), RichTextView::new);
        let who_style = theme.text(TextRole::Caption).strong();
        for (reply, view) in replies.iter().zip(&mut self.thread_reply_views) {
            y += related;
            y += f64::from(tr.measure_style(&reply.author, Some(wrap), &who_style).1) + tight;
            view.show_views(&reply.blocks);
            y += lay_out_flush(view, (x0, x1, y), tr, theme);
        }
        y += f64::from(sp.s3);
        // The composer stands in a field well, its actions under it.
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        y += field_h + f64::from(sp.s2) + ButtonFace::height(ControlSize::Small, theme);
        y - top
    }

    /// The open thread's height at `width`, the slot its comment's card
    /// gives up for it.
    fn comment_thread_height(
        &mut self,
        width: f64,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<f64> {
        let comment = self.open_thread()?;
        let face = Self::thread_face(&comment);
        let body = face.body_rect(tr, theme, Rect::new(0.0, 0.0, width, 1.0e4));
        let content = self.lay_out_thread_body(&comment, (body.x0, body.x1, body.y0), tr, theme);
        Some(face.height_for_body(tr, theme, width, content))
    }

    /// Draw the open thread in `slot`, where its comment's card stands in
    /// the margin: the same card, expanded with every reply and the reply
    /// composer, so the thread and the card are one object.
    fn draw_comment_thread(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        slot: Rect,
    ) {
        let Some(comment) = self.open_thread() else {
            self.close_comment_thread();
            return;
        };
        self.thread_panel = slot;
        let face = Self::thread_face(&comment);
        face.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, tr, theme),
            slot,
        );
        let body = face.body_rect(tr, theme, slot);
        self.lay_out_thread_body(&comment, (body.x0, body.x1, body.y0), tr, theme);
        let sp = &theme.tokens.spacing;
        let tight = f64::from(sp.rhythm.tight);
        let colors = &theme.tokens.colors;
        let wrap = num::f32_of_f64(body.width().max(1.0));
        let body_style = theme.text(TextRole::Body);
        let who_style = theme.text(TextRole::Caption).strong();
        let root = thread_root_text(&comment);
        tr.draw_style(
            scene,
            &root,
            colors.text_primary,
            transform * Affine::translate((body.x0, body.y0)),
            Some(wrap),
            &body_style,
        );
        // Each reply: its author over its text, both from the body's edge.
        for (reply, view) in comment
            .replies
            .iter()
            .take(3)
            .zip(&mut self.thread_reply_views)
        {
            let who_h = f64::from(tr.measure_style(&reply.author, Some(wrap), &who_style).1);
            tr.draw_style(
                scene,
                &reply.author,
                colors.text_primary,
                transform * Affine::translate((body.x0, view.rect().y0 - tight - who_h)),
                Some(wrap),
                &who_style,
            );
            view.draw(scene, transform, tr, theme);
        }

        // The composer in its field well, then Cancel and Send (the card's
        // one primary action) along the card's foot.
        let btn_h = ButtonFace::height(ControlSize::Small, theme);
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        let btn_top = body.y1 - btn_h;
        let field_top = btn_top - f64::from(sp.s2) - field_h;
        let field = Rect::new(body.x0, field_top, body.x1, field_top + field_h);
        ui::surface::card(
            scene,
            transform,
            field,
            f64::from(theme.tokens.radii.control),
            colors.surfaces.raised,
        );
        let line = f64::from(tr.measure_style("Ag", None, &body_style).1);
        let editor_y = field.center().y - line / 2.0;
        self.thread_reply.layout(
            Rect::new(field.x0, editor_y, field.x1, editor_y + line),
            theme,
            tr,
        );
        self.thread_reply
            .draw(scene, transform, tr, theme, presence);
        let cancel_label = t("review-cancel-reply");
        let submit_label = t("review-send-reply");
        let submit_w = ButtonFace::new(Rect::ZERO, &submit_label)
            .role(ButtonRole::Primary)
            .control_size(ControlSize::Small)
            .intrinsic_width(tr, theme);
        let cancel_w = ButtonFace::new(Rect::ZERO, &cancel_label)
            .control_size(ControlSize::Small)
            .intrinsic_width(tr, theme);
        self.thread_submit = Rect::new(body.x1 - submit_w, btn_top, body.x1, btn_top + btn_h);
        let cancel_x1 = self.thread_submit.x0 - f64::from(sp.s2);
        self.thread_cancel = Rect::new(cancel_x1 - cancel_w, btn_top, cancel_x1, btn_top + btn_h);
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(self.thread_cancel, &cancel_label)
                .control_size(ControlSize::Small)
                .hovered(self.thread_cancel.contains(self.pointer)),
        );
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(self.thread_submit, &submit_label)
                .role(ButtonRole::Primary)
                .control_size(ControlSize::Small)
                .hovered(self.thread_submit.contains(self.pointer)),
        );
    }

    fn block_index_cached(&self, block: &str) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == block)
    }

    /// Selection endpoints in document order, computed from the block cache
    /// (so it works during `draw`, which has no `state`).
    fn selection_span_cached(&self) -> Option<(Caret, Caret)> {
        crate::word_processor::ordered_selection_span(
            self.inner.selection_anchor.as_ref(),
            &self.inner.caret,
            |id| self.block_index_cached(id),
        )
    }

    /// Per-block selected char ranges spanning the selection (cache-based).
    fn selection_blocks_cached(&self) -> Vec<(String, usize, usize)> {
        let Some((start, end)) = self.selection_span_cached() else {
            return vec![];
        };
        crate::word_processor::selection_block_ranges(
            &start,
            &end,
            |id| self.block_index_cached(id),
            |i| {
                let b = self.blocks.get(i)?;
                Some((b.id.clone(), b.text.chars().count()))
            },
        )
    }

    fn selection_bytes_for(&self, block_id: &str, text: &str) -> Option<(usize, usize)> {
        let (_, s, e) = self
            .selection_blocks_cached()
            .into_iter()
            .find(|(b, _, _)| b == block_id)?;
        if s >= e {
            return None;
        }
        Some((char_to_byte(text, s), char_to_byte(text, e)))
    }

    /// Resolve every remote collaborator's caret + selection against this
    /// frame's laid-out blocks, into content-space rectangles ready to paint
    /// through the page content transform.
    ///
    /// Factored out of [`Widget::draw`] so it's directly assertable (a populated
    /// `PresenceCtx` yields the expected rect; an empty one yields nothing). Looks
    /// up presence under the same [`block_presence_key`] the local caret is
    /// broadcast under, then maps offsets through the same `caret_geometry`/
    /// `selection_rects` the local caret uses — so remote and local are
    /// pixel-identical, just recoloured.
    ///
    /// Meaningful only after `self.laid`/`self.layout_cache` are populated by a
    /// `draw` pass.
    pub(crate) fn remote_presence_overlays(
        &self,
        presence: &PresenceCtx,
    ) -> (Vec<RemoteCaretOverlay>, Vec<RemoteSelectionOverlay>) {
        let mut carets = Vec::new();
        let mut selections = Vec::new();
        // LIVE_V1 files carets in blocks whose scoped anchor exceeds the wire
        // bound (e.g. 130-byte worker-minted ids) under a digest stand-in key;
        // resolve those against this widget's own blocks. Only hashed when
        // such a key is present.
        let digests = presence
            .carets
            .keys()
            .chain(presence.selections.keys())
            .any(is_scope_digest_key);
        for l in &self.laid {
            let Some((_, layout)) = self.layout_cache.get(&l.id) else {
                continue;
            };
            let exact = block_presence_key(&l.id);
            let digest = digests.then(|| exact.digest_form()).flatten();
            for key in std::iter::once(exact).chain(digest) {
                // Remote selections (drawn under the text, like the local one).
                if let Some(list) = presence.selections.get(&key) {
                    for (user, range) in list {
                        if range.start >= range.end {
                            continue;
                        }
                        let rects: Vec<Rect> = layout
                            .selection_rects(l.shown(range.start), l.shown(range.end))
                            .into_iter()
                            .map(|(rx0, ry0, rx1, ry1)| {
                                Rect::new(
                                    l.text_x + f64::from(rx0),
                                    l.y_top + f64::from(ry0),
                                    l.text_x + f64::from(rx1),
                                    l.y_top + f64::from(ry1),
                                )
                            })
                            .collect();
                        if !rects.is_empty() {
                            selections.push(RemoteSelectionOverlay {
                                user: user.clone(),
                                rects,
                            });
                        }
                    }
                }

                // Remote carets (drawn over the text, like the local one).
                if let Some(list) = presence.carets.get(&key) {
                    for (user, byte_pos) in list {
                        let (cx, cy, ch) = layout.caret_geometry(l.shown(*byte_pos));
                        let rect = Rect::new(
                            l.text_x + f64::from(cx),
                            l.y_top + f64::from(cy),
                            l.text_x + f64::from(cx) + CARET_WIDTH,
                            l.y_top + f64::from(cy) + f64::from(ch),
                        );
                        let label = presence.display_name(user);
                        carets.push(RemoteCaretOverlay {
                            user: user.clone(),
                            rect,
                            label,
                        });
                    }
                }
            }
        }
        (carets, selections)
    }

    /// A peer's just-typed runs, mapped onto this frame's laid-out blocks.
    ///
    /// `PresencePayload::Typing` names the span a peer just inserted; it decays
    /// over `TYPING_FADE` so the glow follows them through the document instead
    /// of accumulating. Resolved from the same `self.laid` + `self.layout_cache`
    /// the caret/selection overlays use, so all three can't drift apart.
    ///
    /// This is the *ephemeral* half of authorship — the durable half ("this
    /// paragraph is hers") is `tilo_doc::author_runs`, already fed to
    /// [`ui::presence::PresenceCtx::draw_peer_authorship`] by
    /// [`ui::widget::block_editor`]; this widget doesn't call it yet.
    pub(crate) fn remote_typing_overlays(
        &self,
        presence: &PresenceCtx,
        now: ui::time::Instant,
    ) -> Vec<(UserId, Vec<(Rect, f32)>)> {
        let mut out = Vec::new();
        for l in &self.laid {
            let Some((_, layout)) = self.layout_cache.get(&l.id) else {
                continue;
            };
            let key = block_presence_key(&l.id);
            for (user, range, alpha) in presence.typing_marks_for_key(&key, now) {
                if range.start >= range.end {
                    continue;
                }
                let spans: Vec<(Rect, f32)> = layout
                    .selection_rects(l.shown(range.start), l.shown(range.end))
                    .into_iter()
                    .map(|(rx0, ry0, rx1, ry1)| {
                        (
                            Rect::new(
                                l.text_x + f64::from(rx0),
                                l.y_top + f64::from(ry0),
                                l.text_x + f64::from(rx1),
                                l.y_top + f64::from(ry1),
                            ),
                            alpha,
                        )
                    })
                    .collect();
                if !spans.is_empty() {
                    out.push((user, spans));
                }
            }
        }
        out
    }

    fn extend_or_clear(&mut self, shift: bool) {
        if shift {
            self.inner.set_anchor();
        } else {
            self.inner.clear_anchor();
        }
    }

    fn mark_active_in_selection(&self, name: &str) -> bool {
        let sel: Vec<_> = self
            .selection_blocks_cached()
            .into_iter()
            .filter(|(_, s, e)| s < e)
            .collect();
        if sel.is_empty() {
            return false;
        }
        sel.iter().all(|(bid, s, e)| {
            self.blocks
                .iter()
                .find(|b| &b.id == bid)
                .is_some_and(|b| text_runs::marks_cover_range(&b.marks, name, *s, *e))
        })
    }

    fn current_block_style(&self) -> (String, Option<i64>) {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block);
        let kind = b.map_or_else(|| "paragraph".into(), |b| b.kind.clone());
        let level = b.and_then(|b| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => Some(*n),
            _ => None,
        });
        (kind, level)
    }

    fn apply_tool(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        match action {
            // Every document-mutating format dispatches its matching
            // `RichTextAction` through the shared action path (the same
            // EncodedAction the Ctrl+K palette dispatches), so it replays
            // byte-identically and lands as one undo boundary.
            ToolAction::Bold => self.dispatch_toggle_mark(state, "strong"),
            ToolAction::Italic => self.dispatch_toggle_mark(state, "em"),
            ToolAction::Underline => self.dispatch_toggle_mark(state, "underline"),
            ToolAction::Strike => self.dispatch_toggle_mark(state, "strike"),
            ToolAction::TextColor(Some(c)) => self.dispatch_set_mark_value(
                state,
                "color",
                crate::actions::MarkVal::Str(crate::styles::color_to_hex(c)),
            ),
            // The neutral swatch clears any per-run colour, reverting to the
            // block's style ink.
            ToolAction::TextColor(None) => self.dispatch_clear_mark(state, "color"),
            ToolAction::Highlight(Some(c)) => self.dispatch_set_mark_value(
                state,
                "highlight",
                crate::actions::MarkVal::Str(crate::styles::color_to_hex(c)),
            ),
            // The "no highlight" swatch clears any per-run highlight band.
            ToolAction::Highlight(None) => self.dispatch_clear_mark(state, "highlight"),
            ToolAction::Super => self.dispatch_exclusive_mark(state, "super", "sub"),
            ToolAction::Sub => self.dispatch_exclusive_mark(state, "sub", "super"),
            ToolAction::Hyperlink => {
                if !self.selection_blocks_cached().is_empty() {
                    self.action_palette_request = Some(tilo_actions::ActionAddress::new(
                        "word-processor.caret",
                        "set-hyperlink",
                    ));
                }
            }
            ToolAction::FontSizeInc => {
                let next =
                    num::i64_of_f32((self.current_font_size() + 1.0).round().clamp(6.0, 200.0));
                self.dispatch_set_mark_value(state, "size", crate::actions::MarkVal::Int(next));
            }
            ToolAction::FontSizeDec => {
                let next =
                    num::i64_of_f32((self.current_font_size() - 1.0).round().clamp(6.0, 200.0));
                self.dispatch_set_mark_value(state, "size", crate::actions::MarkVal::Int(next));
            }
            ToolAction::FontCycle => {
                let name = match self.current_font().next() {
                    FontFamily::Sans => "sans",
                    FontFamily::Serif => "serif",
                    FontFamily::Mono => "mono",
                };
                self.dispatch_set_mark_value(
                    state,
                    "font",
                    crate::actions::MarkVal::Str(name.to_string()),
                );
            }
            ToolAction::InsertImage => self.request_image_insert(state),
            ToolAction::InsertTable => self.insert_table_via_action(state, 2, 2),
            table_action @ (ToolAction::TableRowAdd
            | ToolAction::TableRowDel
            | ToolAction::TableColAdd
            | ToolAction::TableColDel
            | ToolAction::TableDelete) => self.apply_table_tool(state, table_action),
            ToolAction::ToggleStyles => {
                self.view.styles_panel_visible = !self.view.styles_panel_visible;
                if self.view.styles_panel_visible {
                    self.inspector_drawer.open();
                } else {
                    self.inspector_drawer.close();
                }
            }
            ToolAction::ToggleRuler => self.view.ruler_visible = !self.view.ruler_visible,
            ToolAction::ToggleOutline => {
                self.outline.toggle();
                if self.outline.is_open() {
                    self.outline_drawer.open();
                } else {
                    self.outline_drawer.close();
                }
            }
            ToolAction::InsertToc => self.generate_toc(state),
            ToolAction::NewMenu => self.open_template_picker(),
            ToolAction::TextColorPalette => self.open_compact_swatch_palette(PaletteKind::Text),
            ToolAction::HighlightPalette => {
                self.open_compact_swatch_palette(PaletteKind::Highlight);
            }
        }
    }

    fn apply_table_tool(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        let Some((_, row, column, _, _)) = self.caret_table_target() else {
            if action == ToolAction::TableDelete {
                self.edit_table_via_action(state, crate::actions::TableEdit::Delete);
            }
            return;
        };
        let edit = match action {
            ToolAction::TableRowAdd => crate::actions::TableEdit::InsertRow {
                at: row.saturating_add(1),
            },
            ToolAction::TableRowDel => crate::actions::TableEdit::DeleteRow { row },
            ToolAction::TableColAdd => crate::actions::TableEdit::InsertColumn {
                at: column.saturating_add(1),
            },
            ToolAction::TableColDel => crate::actions::TableEdit::DeleteColumn { column },
            ToolAction::TableDelete => crate::actions::TableEdit::Delete,
            _ => return,
        };
        self.edit_table_via_action(state, edit);
    }

    /// Open the toolbar's More menu, as a person choosing it would.
    fn open_toolbar_overflow(&mut self) {
        let _ = Widget::perform_semantic_action(
            &mut self.tool_bar,
            "toolbar.overflow",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        );
        self.lifecycle.dirty = true;
    }

    /// Mutate the focused block's style in the live widget sheet (component-
    /// instance preview), then optionally persist it to the document.
    fn edit_focused_style(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        persist: bool,
        f: impl FnOnce(&mut crate::styles::StyleProps),
    ) {
        let Some(key) = self.focused_style_key() else {
            return;
        };
        if let Some(st) = self.sheet.styles.get_mut(&key) {
            f(st);
        }
        self.lifecycle.dirty = true;
        if persist {
            self.persist_stylesheet_via_actions(state);
        }
    }

    /// Dispatch a click on a styles-panel target.
    fn apply_panel_hit(&mut self, state: &mut ui::widget::Edits<'_>, hit: PanelHit) {
        match hit {
            PanelHit::Close => {
                self.view.styles_panel_visible = false;
                self.focus_hosted(None, false);
                self.lifecycle.dirty = true;
            }
            PanelHit::Pick(id) => {
                // A built-in style implies a structural kind, applied together as one
                // action/undo boundary: picking "Heading 1" yields a *real* heading
                // (present in outline/TOC), "List Item" a real list item — not merely
                // styled body text. Custom styles are pure visual presets; kind stays untouched.
                self.dispatch_apply_named_style(state, &id);
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
            }
            PanelHit::SetFont(font) => {
                self.edit_focused_style(state, true, |st| st.font = font.into());
            }
            PanelHit::ToggleBold => self.edit_focused_style(state, true, |st| st.bold = !st.bold),
            PanelHit::ToggleItalic => {
                self.edit_focused_style(state, true, |st| st.italic = !st.italic);
            }
            // Direct paragraph formatting on the current paragraph(s), not the
            // named style — only the panel exposes these, not the toolbar.
            PanelHit::ParaAlign(a) => {
                self.dispatch_block_attr(
                    state,
                    "align",
                    crate::actions::AttrVal::Str(a.to_string()),
                );
                self.refresh_blocks(state);
                self.lifecycle.dirty = true;
            }
            PanelHit::SetColor(c) => self.edit_focused_style(state, true, |st| st.color = c),
            PanelHit::OpenColorPicker => self.open_color_picker(),
            PanelHit::NewStyle => self.apply_panel_hit_new_style(state),
            PanelHit::ImageScale(factor) => self.apply_panel_hit_image_scale(state, factor),
            PanelHit::ImageFlipH => self.apply_panel_hit_image_flip(state, true),
            PanelHit::ImageFlipV => self.apply_panel_hit_image_flip(state, false),
            PanelHit::ImageCropInset => self.apply_panel_hit_image_crop(state, true),
            PanelHit::ImageCropReset => self.apply_panel_hit_image_crop(state, false),
            PanelHit::ImageInline => {
                self.apply_panel_hit_image_placement(state, "inline", "none");
            }
            PanelHit::ImageFreePosition => {
                self.apply_panel_hit_image_placement(state, "page", "none");
            }
            PanelHit::ImageWrapLeft => {
                self.apply_panel_hit_image_placement(state, "paragraph", "left");
            }
            PanelHit::ImageWrapRight => {
                self.apply_panel_hit_image_placement(state, "paragraph", "right");
            }
        }
    }

    /// `PanelHit::NewStyle`: duplicate the focused style under a fresh id.
    fn apply_panel_hit_new_style(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(key) = self.focused_style_key() else {
            return;
        };
        let base = self.sheet.get(&key);
        let new_id = self.sheet.fresh_custom_id();
        let mut props = base.clone();
        props.name = format!("{} copy", base.name);
        self.sheet.styles.insert(new_id.clone(), props);
        self.persist_stylesheet_via_actions(state);
        self.dispatch_apply_named_style(state, &new_id);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    /// `PanelHit::ImageScale`: scale the *current on-screen* size (aspect
    /// locked) so repeated presses compound predictably, seeding from the
    /// live doc size when set, else the laid display size.
    fn apply_panel_hit_image_scale(&mut self, state: &mut ui::widget::Edits<'_>, factor: f64) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let (cw, ch) = self
            .inner
            .image_size(state, &id)
            .or_else(|| self.selected_image_dims())
            .unwrap_or((240.0, 160.0));
        // Keep width + height in one action/undo boundary. Direct writes here
        // used to make a resize invisible to the action catalogue and could
        // replay as two independently stale attributes.
        let width = num::i64_of_f64((cw * factor).round().clamp(16.0, 4_000.0));
        let height = num::i64_of_f64((ch * factor).round().clamp(16.0, 4_000.0));
        self.perform_action(
            state,
            &crate::actions::set_image_size_action(&id, width, height),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    /// `PanelHit::ImageFlipH`/`ImageFlipV`: mirror the selected image about
    /// its horizontal (`horizontal`) or vertical axis.
    fn apply_panel_hit_image_flip(&mut self, state: &mut ui::widget::Edits<'_>, horizontal: bool) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let key = if horizontal { "flip_h" } else { "flip_v" };
        let active = self
            .blocks
            .iter()
            .find(|b| b.id == id)
            .is_some_and(|b| matches!(b.attrs.get(key), Some(AttrVal::Bool(true))));
        self.perform_action(
            state,
            &crate::actions::set_block_attr_action(
                crate::actions::RegionKind::Body,
                vec![id],
                key,
                crate::actions::AttrVal::Bool(!active),
            ),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    /// Crop or reset the selected image through one atomic action. The panel and
    /// Ctrl+K image resource intentionally calculate exactly the same 5% inset.
    fn apply_panel_hit_image_crop(&mut self, state: &mut ui::widget::Edits<'_>, inset: bool) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let Some(block) = self.blocks.iter().find(|b| b.id == id) else {
            return;
        };
        let edge = |key: &str| match block.attrs.get(key) {
            Some(AttrVal::Int(value)) => *value,
            _ => 0,
        };
        let (mut left, mut top, mut right, mut bottom) = (
            edge("crop_left"),
            edge("crop_top"),
            edge("crop_right"),
            edge("crop_bottom"),
        );
        if inset {
            // Preserve a visible 5% source window; at the limit the control is
            // a harmless no-op rather than writing an invalid crop.
            if left.saturating_add(right) > 8_500 || top.saturating_add(bottom) > 8_500 {
                return;
            }
            left += 250;
            top += 250;
            right += 250;
            bottom += 250;
        } else {
            left = 0;
            top = 0;
            right = 0;
            bottom = 0;
        }
        self.perform_action(
            state,
            &crate::actions::set_image_crop_action(&id, left, top, right, bottom),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    /// Choose one of the image's canonical placement modes. The free mode is
    /// page-relative and therefore does not participate in text flow; the two
    /// wrap modes preserve paragraph-relative flow.
    fn apply_panel_hit_image_placement(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        anchor: &str,
        wrap: &str,
    ) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        self.perform_action(
            state,
            &crate::actions::set_image_placement_action(&id, anchor, wrap, 0, 0, 0),
        );
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    // ---- colour picker popover --------------------------------------------

    /// Open the shared [`ColorPicker`] seeded with the focused style's colour.
    fn open_color_picker(&mut self) {
        let seed = self
            .focused_style_key()
            .map_or([0x1c, 0x1c, 0x20], |k| self.sheet.get(&k).color);
        self.color_picker.open(seed, self.pointer);
        self.lifecycle.dirty = true;
    }

    /// While the colour picker is open, route input to it (drag the SV square /
    /// hue strip / RGB sliders); an outside press or Escape closes it. A committed
    /// colour is applied live to the focused style. Returns whether the event was
    /// consumed (so the caller stops further dispatch).
    fn pump_color_picker(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        mods: Modifiers,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        handled: &mut bool,
    ) -> bool {
        let result = self
            .color_picker
            .handle_event(crate::style_color_picker::EventArgs {
                event,
                scale,
                modifiers: mods,
                state: state.reborrow(),
                root,
                host: self.rect,
                panel: self.panel_rect,
            });
        if let Some(rgb) = result.changed {
            self.edit_focused_style(state, true, |st| st.color = rgb);
        }
        if result.consumed {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer = Point::new(position.x / scale, position.y / scale);
            }
            self.lifecycle.dirty = true;
            *handled = true;
        }
        result.consumed
    }

    /// Open the compact toolbar's shared swatch grid. The labels describe the
    /// *formatting effect* rather than forcing assistive technology users to
    /// infer it from a hexadecimal value alone.
    fn open_compact_swatch_palette(&mut self, kind: PaletteKind) {
        self.context_menu.close();
        self.style_menu.close();
        self.template_picker.close();
        let (palette, title, clear_label): (&[[u8; 3]], String, String) = match kind {
            PaletteKind::Text => (
                &STYLE_PALETTE,
                t("picker-text-color"),
                t("a11y-text-color-default"),
            ),
            PaletteKind::Highlight => (
                &HIGHLIGHT_PALETTE,
                t("picker-highlight"),
                t("a11y-highlight-clear"),
            ),
        };
        let labels = swatch_names(kind);
        let command = match kind {
            PaletteKind::Text => ToolAction::TextColorPalette,
            PaletteKind::Highlight => ToolAction::HighlightPalette,
        };
        let anchor = self
            .toolbar
            .iter()
            .find(|(action, _)| *action == command)
            .map_or(self.pointer, |(_, rect)| {
                Point::new(rect.x0, self.tool_band.y1)
            });
        let current = match kind {
            PaletteKind::Text => self.current_text_color(),
            PaletteKind::Highlight => self.current_highlight_color(),
        };
        self.compact_swatch_palette.open(PaletteRequest {
            kind,
            selected: current.and_then(|rgb| palette.iter().position(|c| *c == rgb)),
            colors: palette,
            labels,
            title,
            clear_label,
            anchor,
            host: self.rect,
            theme: &self.desk_theme,
        });
        self.lifecycle.dirty = true;
    }

    /// Route compact swatch-grid input ahead of the page. A picked colour still
    /// uses the exact `ToolAction` path as the desktop toolbar, preserving one
    /// mutation/action implementation and one collaboration boundary.
    fn pump_compact_swatch_palette(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        state: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        handled: &mut bool,
    ) -> bool {
        if !self.compact_swatch_palette.is_open() {
            return false;
        }
        let choice = self
            .compact_swatch_palette
            .handle_event(event, scale, modifiers, state, root);
        if let Some((kind, choice)) = choice {
            self.apply_compact_palette_choice(state, kind, choice);
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale, position.y / scale);
        }
        self.lifecycle.dirty = true;
        *handled = true;
        true
    }

    /// Commit any compact-palette route through the same canonical formatting
    /// actions as its pointer, keyboard, and assistive-technology callers.
    fn apply_compact_palette_choice(
        &mut self,
        state: &mut ui::widget::Edits<'_>,
        kind: PaletteKind,
        choice: PaletteChoice,
    ) {
        let action = match (kind, choice) {
            (PaletteKind::Text, PaletteChoice::Color(color)) => ToolAction::TextColor(Some(color)),
            (PaletteKind::Text, PaletteChoice::Clear) => ToolAction::TextColor(None),
            (PaletteKind::Highlight, PaletteChoice::Color(color)) => {
                ToolAction::Highlight(Some(color))
            }
            (PaletteKind::Highlight, PaletteChoice::Clear) => ToolAction::Highlight(None),
        };
        self.apply_tool(state, action);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    /// The next/previous cell id within the caret cell's table, if any.
    /// The char position the toolbar reports font state for: just left of the
    /// caret (so it reflects the run you're typing into), else the caret.
    fn font_probe(&self) -> Option<(&BlockView, usize)> {
        let b = self
            .blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)?;
        let pos = self.inner.caret.offset.saturating_sub(1);
        Some((b, pos))
    }

    fn current_font(&self) -> FontFamily {
        self.font_probe()
            .and_then(|(b, pos)| char_font_family(pos, &b.marks))
            .unwrap_or_default()
    }

    /// The effective font size at the selection: the per-run `size` character
    /// mark under the caret if present, else the resolved paragraph style's
    /// size. Seeds the toolbar's A− / A+ nudges (which write a per-run mark).
    fn current_font_size(&self) -> f32 {
        if let Some(sz) = self
            .font_probe()
            .and_then(|(b, pos)| char_font_size(pos, &b.marks))
        {
            return sz;
        }
        self.focused_style_key()
            .map_or(16.5, |k| self.sheet.get(&k).size)
    }

    fn segmented_rects(&self) -> [Vec<Rect>; 3] {
        if self.modal_capture().is_some()
            || self.color_picker.is_open()
            || self.compact_swatch_palette.is_open()
        {
            return std::array::from_fn(|_| Vec::new());
        }
        let align = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| matches!(hit, PanelHit::ParaAlign(_)).then_some(*rect))
            .collect();
        let placement = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| {
                matches!(
                    hit,
                    PanelHit::ImageInline
                        | PanelHit::ImageFreePosition
                        | PanelHit::ImageWrapLeft
                        | PanelHit::ImageWrapRight
                )
                .then_some(*rect)
            })
            .collect();
        let font = self
            .panel_hits
            .iter()
            .filter_map(|(rect, hit)| matches!(hit, PanelHit::SetFont(_)).then_some(*rect))
            .collect();
        [align, placement, font]
    }

    fn handle_segmented_touch(
        &mut self,
        touch: &winit::event::Touch,
        scale: f64,
        edits: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let [align, placement, font] = self.segmented_rects();
        let current = self.current_align();
        let selected_align = ["left", "center", "right", "justify"]
            .iter()
            .position(|value| *value == current)
            .unwrap_or(ui::widget::toggle_group::NONE_CHOSEN);
        let image = self.selected_image_id();
        let selected_placement = self
            .blocks
            .iter()
            .find(|block| Some(&block.id) == image.as_ref())
            .map_or(ui::widget::toggle_group::NONE_CHOSEN, |block| {
                match (image_anchor(block), image_wrap(block)) {
                    ("page", _) => 1,
                    (_, "left") => 2,
                    (_, "right") => 3,
                    _ => 0,
                }
            });
        let selected_font = self.focused_style_key().map_or(0, |key| {
            ["sans", "serif", "mono"]
                .iter()
                .position(|value| *value == self.sheet.get(&key).font)
                .unwrap_or(0)
        });
        let groups = [
            SegmentedTouchGroup::new(0, &align, selected_align),
            SegmentedTouchGroup::new(1, &placement, selected_placement),
            SegmentedTouchGroup::new(2, &font, selected_font),
        ];
        let result = self.segmented_touch.handle(touch, scale, &groups);
        if let SegmentedTouchResult::Selected { group, index } = result {
            let hit = match group {
                0 => ["left", "center", "right", "justify"]
                    .get(index)
                    .copied()
                    .map(PanelHit::ParaAlign),
                1 => [
                    PanelHit::ImageInline,
                    PanelHit::ImageFreePosition,
                    PanelHit::ImageWrapLeft,
                    PanelHit::ImageWrapRight,
                ]
                .get(index)
                .cloned(),
                2 => ["sans", "serif", "mono"]
                    .get(index)
                    .copied()
                    .map(PanelHit::SetFont),
                _ => None,
            };
            if let Some(hit) = hit {
                self.apply_panel_hit(edits, hit);
            }
        }
        result.handled()
    }

    fn current_align(&self) -> String {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .and_then(|b| match b.attrs.get("align") {
                Some(AttrVal::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "left".into())
    }

    /// The caret block's line-spacing multiplier (from the cached blocks), or 1.0.
    /// Drives the toolbar chip label.
    fn current_line_spacing(&self) -> f64 {
        self.blocks
            .iter()
            .find(|b| b.id == self.inner.caret.block)
            .map_or(1.0, |b| f64::from(block_line_spacing(b)))
    }

    /// The per-run `highlight` colour covering the font-probe position (just left
    /// of the caret), if any — used to ring the active highlight swatch.
    fn current_highlight_color(&self) -> Option<[u8; 3]> {
        let (b, pos) = self.font_probe()?;
        char_highlight(pos, &b.marks)
    }

    fn clipboard_copy(&mut self, state: &AutoCommit) -> bool {
        if !self.inner.has_selection() {
            return false;
        }
        let (plain, blocks) = self.inner.copy_fragment(state);
        if !ui::text_selection::try_clipboard_copy(&plain) {
            return false;
        }
        self.rich_clip = Some((plain, blocks));
        true
    }

    /// Paste with formatting when the system clipboard still holds our last rich
    /// copy; otherwise paste the clipboard's plain text (e.g. from another app).
    fn clipboard_paste(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(sys) = clipboard_paste() else {
            return;
        };
        let rich = self
            .rich_clip
            .as_ref()
            .filter(|(plain, _)| plain == &sys)
            .map(|(_, blocks)| blocks.clone());
        match rich {
            Some(blocks) => self.paste_fragment_via_action(state, blocks),
            None => self.paste_plain_text_via_action(state, &sys),
        }
    }

    fn paste_plain_text_via_action(&mut self, state: &mut ui::widget::Edits<'_>, text: &str) {
        // The published `paste` primitive; a paste beyond its per-call text
        // bound (an agent limit, not a person's) keeps the direct fragment.
        let content = crate::actions::editing::PasteContent::Text(text.to_string());
        let view = self.local_edit_view();
        if let Ok((envelopes, next)) = crate::actions::editing::paste(&view, state, &content) {
            for action in &envelopes {
                self.perform_action(state, action);
            }
            self.adopt_local_view(&next, state);
            return;
        }
        let blocks = text
            .replace('\r', "")
            .split('\n')
            .map(|line| crate::word_processor::ClipBlock {
                kind: "paragraph".to_string(),
                text: line.to_string(),
                marks: Vec::new(),
                level: None,
                ordered: None,
                align: None,
                attrs: std::collections::BTreeMap::default(),
            })
            .collect();
        self.paste_fragment_via_action(state, blocks);
    }

    /// Paste plain text only (Ctrl+Shift+V), stripping all formatting.
    fn clipboard_paste_plain(&mut self, state: &mut ui::widget::Edits<'_>) {
        if let Some(t) = clipboard_paste() {
            self.paste_plain_text_via_action(state, &t);
        }
    }

    fn select_all_in_block(&mut self, state: &AutoCommit) {
        self.inner.move_home();
        self.inner.clear_anchor();
        self.inner.set_anchor();
        self.inner.move_end(state);
    }

    // ---- right-click context menu ------------------------------------------

    /// Char span `(start, end)` of the word at char-offset `off` in `text`
    /// (whitespace-delimited, matching `WordProcessor::select_word`). `start ==
    /// end` means the offset sits on whitespace (no word).
    fn word_span_at(text: &str, off: usize) -> (usize, usize) {
        let chars: Vec<char> = text.chars().collect();
        let off = off.min(chars.len());
        let is_word = |c: char| !c.is_whitespace();
        let mut s = off;
        while s > 0 && chars.get(s - 1).is_some_and(|&c| is_word(c)) {
            s -= 1;
        }
        let mut e = off;
        while chars.get(e).is_some_and(|&c| is_word(c)) {
            e += 1;
        }
        (s, e)
    }

    /// If the word at block-char-span `(s, e)` is flagged misspelled, return its
    /// up-to-`max` spelling suggestions. Empty vec when the word is fine. Uses a
    /// fresh `check` over just the word so it reflects the **current** dictionary
    /// (including freshly-added user words), not the stale per-block cache.
    fn suggestions_for_word(&mut self, word: &str) -> Option<Vec<String>> {
        if word.is_empty() {
            return None;
        }
        let language = self.proofing.language.clone();
        let dict = self.dictionary();
        if dict.contains(word) || spellcheck::spans_for_language(&language, word).is_empty() {
            return None;
        }
        Some(spellcheck::suggestions_for_language(&language, word, 5))
    }

    /// Build + open the right-click context menu at screen `pos`. Selects the word
    /// under the click when nothing is selected, then assembles rows: misspelled-word
    /// suggestions + "Add to dictionary" (if flagged) prepended to Cut/Copy/Paste/
    /// Select All. `context_actions` mirrors the rows for left-click dispatch by index.
    fn open_context_menu(&mut self, state: &AutoCommit, content_pt: Point, pos: [f32; 2]) {
        // Place the caret under the click; select the word when there's no
        // selection so Cut/Copy act on something.
        let selected_ranges = self.inner.selection_blocks(state);
        let previous_caret = self.inner.caret.clone();
        let previous_anchor = self.inner.selection_anchor.clone();
        self.caret_from_pointer(content_pt, false);
        // Raw click char offset (before `select_word` snaps the caret to the word
        // end) — used to test which tracked-change / comment range was clicked.
        let raw_click_off = self.inner.caret.offset;
        let block = self.inner.caret.block.clone();
        let block_text = self.inner.current_text(state);
        let inside_selection = selected_ranges.iter().any(|(id, start, end)| {
            *id == block && *start <= raw_click_off && raw_click_off <= *end
        });
        if inside_selection {
            self.inner.caret = previous_caret;
            self.inner.selection_anchor = previous_anchor;
        } else {
            self.inner.select_word(state);
        }

        // Identify the clicked word's block + char span for spell-check actions.
        let (wstart, wend) = Self::word_span_at(&block_text, raw_click_off);
        let word: String = block_text
            .chars()
            .skip(wstart)
            .take(wend - wstart)
            .collect();

        let mut menu = ContextMenu::new();
        menu.set_text_scale(crate::num::f32_of_f64(self.text_scale));
        menu.set_viewport(Some(self.rect));
        let mut actions: Vec<Option<CtxAction>> = Vec::new();

        self.push_review_rows(&mut menu, &mut actions, &block, wstart, wend, raw_click_off);
        self.push_spellcheck_rows(&mut menu, &mut actions, &block, &word, wstart, wend);
        self.push_clipboard_rows(&mut menu, &mut actions);

        menu.open_at(pos);
        menu.constrain_to(self.rect);
        self.context_menu = menu;
        self.context_actions = actions;
        self.lifecycle.dirty = true;
    }

    /// Context-menu rows offering Accept/Reject for a tracked-change range or
    /// Resolve for a comment range under the click, plus Accept All / Reject
    /// All whenever the document has any tracked change. The clicked word
    /// span `[wstart, wend)` (falling back to `raw_click_off` for a
    /// whitespace click) is matched against `block`'s `ins`/`del`/`comment`
    /// marks. A no-op unless the review overlay is visible.
    fn push_review_rows(
        &self,
        menu: &mut ContextMenu,
        actions: &mut Vec<Option<CtxAction>>,
        block: &str,
        wstart: usize,
        wend: usize,
        raw_click_off: usize,
    ) {
        if !self.review.is_visible() {
            return;
        }
        let mut review_rows = false;
        if let Some(b) = self.blocks.iter().find(|b| b.id == block) {
            for m in &b.marks {
                // The clicked word span `[wstart, wend)` must overlap the mark
                // (covers a click anywhere in the range, robust to the
                // word-snap). Fall back to the raw click point for whitespace.
                let overlaps = if wstart < wend {
                    wstart < m.end && m.start < wend
                } else {
                    raw_click_off >= m.start && raw_click_off < m.end
                };
                if !overlaps {
                    continue;
                }
                let Some(id) = m.value.as_str().map(std::string::ToString::to_string) else {
                    continue;
                };
                if m.name == tilo_doc::schema::MARK_INS || m.name == tilo_doc::schema::MARK_DEL {
                    let is_ins = m.name == tilo_doc::schema::MARK_INS;
                    let accept_key = if is_ins {
                        "review-accept-insertion"
                    } else {
                        "review-accept-deletion"
                    };
                    let reject_key = if is_ins {
                        "review-reject-insertion"
                    } else {
                        "review-reject-deletion"
                    };
                    menu.push(MenuItem::action(t(accept_key), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::AcceptChange(
                        id.clone(),
                    ))));
                    menu.push(MenuItem::action(t(reject_key), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::RejectChange(
                        id.clone(),
                    ))));
                    review_rows = true;
                } else if m.name == tilo_doc::schema::MARK_COMMENT {
                    menu.push(MenuItem::action(t("review-reply"), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::OpenCommentThread(
                        id.clone(),
                    ))));
                    menu.push(MenuItem::action(t("review-resolve-comment"), || {}));
                    actions.push(Some(CtxAction::Review(ReviewAction::ResolveComment(
                        id.clone(),
                    ))));
                    review_rows = true;
                }
            }
        }
        if !self.review.changes().is_empty() {
            menu.push(MenuItem::action(t("review-accept-all"), || {}));
            actions.push(Some(CtxAction::Review(ReviewAction::AcceptAllChanges)));
            menu.push(MenuItem::action(t("review-reject-all"), || {}));
            actions.push(Some(CtxAction::Review(ReviewAction::RejectAllChanges)));
            review_rows = true;
        }
        if review_rows {
            menu.push(MenuItem::separator());
            actions.push(None);
        }
    }

    /// Context-menu rows for the clicked word's spell-check suggestions (up
    /// to 3) + "Add to dictionary", when spell-check is on and the word is
    /// currently flagged.
    fn push_spellcheck_rows(
        &mut self,
        menu: &mut ContextMenu,
        actions: &mut Vec<Option<CtxAction>>,
        block: &str,
        word: &str,
        wstart: usize,
        wend: usize,
    ) {
        if !self.spellcheck_enabled() {
            return;
        }
        let Some(suggestions) = self.suggestions_for_word(word) else {
            return;
        };
        for sug in suggestions.iter().take(3) {
            menu.push(MenuItem::action(sug.clone(), || {}));
            actions.push(Some(CtxAction::ApplySuggestion {
                block: block.to_string(),
                start: wstart,
                end: wend,
                replacement: sug.clone(),
            }));
        }
        menu.push(MenuItem::action(t("menu-add-to-dictionary"), || {}));
        actions.push(Some(CtxAction::AddToDictionary(word.to_string())));
        menu.push(MenuItem::separator());
        actions.push(None);
    }

    /// Context-menu rows for Cut/Copy (when there's a selection), Paste
    /// (when the clipboard holds text) and always Select All.
    fn push_clipboard_rows(&self, menu: &mut ContextMenu, actions: &mut Vec<Option<CtxAction>>) {
        let has_sel = self.inner.has_selection();
        let can_paste = clipboard_paste().is_some_and(|s| !s.is_empty());
        if has_sel {
            menu.push(MenuItem::action(t("menu-cut"), || {}));
            actions.push(Some(CtxAction::Cut));
            menu.push(MenuItem::action(t("menu-copy"), || {}));
            actions.push(Some(CtxAction::Copy));
        }
        if can_paste {
            menu.push(MenuItem::action(t("menu-paste"), || {}));
            actions.push(Some(CtxAction::Paste));
        }
        if !menu.items().is_empty() {
            menu.push(MenuItem::separator());
            actions.push(None);
        }
        menu.push(MenuItem::action(t("menu-select-all"), || {}));
        actions.push(Some(CtxAction::SelectAll));
    }

    /// Dispatch the context-menu row at `idx` against the widget, then close the
    /// menu. Separator / out-of-range rows are ignored. Returns `true` when a
    /// row ran.
    fn dispatch_context_action(&mut self, idx: usize, state: &mut ui::widget::Edits<'_>) -> bool {
        let Some(Some(action)) = self.context_actions.get(idx).cloned() else {
            return false;
        };
        match action {
            CtxAction::Cut => {
                if self.clipboard_copy(state) {
                    self.insert_text_via_action(state, "");
                }
            }
            CtxAction::Copy => {
                self.clipboard_copy(state);
            }
            CtxAction::Paste => {
                self.clipboard_paste(state);
            }
            CtxAction::SelectAll => self.select_all_via_primitive(state),
            CtxAction::AddToDictionary(word) => {
                if self.proofing.personal.is_some() {
                    let result = ui::personal_dictionary::snapshot().and_then(|snapshot| {
                        ui::personal_dictionary::edit(
                            snapshot.revision,
                            ui::personal_dictionary::Edit::Add(ui::personal_dictionary::Word::new(
                                &word, None,
                            )?),
                        )
                    });
                    if let Err(error) = result {
                        eprintln!("word-processor: could not keep the shared word: {error}");
                    }
                    self.dict = None;
                    self.spell_cache.clear();
                } else if self.proofing.personal_words.add(&word) {
                    // Keep it with the person's settings, then build the
                    // dictionary again so every block re-checks and the
                    // squiggle clears on the next draw.
                    self.dict = None;
                    self.spell_cache.clear();
                }
            }
            CtxAction::ApplySuggestion {
                block,
                start,
                end,
                replacement,
            } => {
                // One replayable, attributable envelope through the executor
                // (the published apply-spelling-suggestion op builds the same
                // splice), never a direct CRDT write.
                let region = self.inner.region_kind();
                self.splice_via_action(state, region, &block, start, end, &replacement);
                self.refresh_blocks(state);
                // The block text changed; its cache entry is stale.
                self.spell_cache.remove(&block);
            }
            CtxAction::Review(action) => self.dispatch_review_action(action, state),
        }
        self.context_menu.close();
        self.lifecycle.dirty = true;
        true
    }

    // ---- paragraph-style management menu ----------------------------------

    /// The style id of the picker chip under `p` in the styles panel, if any.
    /// Reads the current frame's `panel_hits` (rebuilt every draw), so the chip
    /// rects are live at right-click time.
    fn style_chip_at(&self, p: Point) -> Option<String> {
        if self.style_pick_rect.contains(p) {
            return self.focused_style_key();
        }
        self.panel_hits.iter().find_map(|(r, h)| match h {
            PanelHit::Pick(id) if r.contains(p) => Some(id.clone()),
            _ => None,
        })
    }

    /// Apply a typed style-menu outcome. The component owns presentation and
    /// hit-testing; document mutation remains editor policy.
    fn dispatch_style_menu(&mut self, action: StyleMenuAction, state: &mut ui::widget::Edits<'_>) {
        match action {
            StyleMenuAction::Delete(id) => {
                if self.sheet.remove(&id) {
                    self.persist_stylesheet_via_actions(state);
                    self.refresh_blocks(state);
                    self.toast = Some((t("toast-style-deleted"), ToastKind::Success));
                }
            }
            StyleMenuAction::Duplicate(id) => {
                if let Some(mut properties) = self.sheet.styles.get(&id).cloned() {
                    let new_id = self.sheet.fresh_custom_id();
                    properties.name = format!("{} copy", properties.name);
                    self.sheet.styles.insert(new_id.clone(), properties);
                    self.persist_stylesheet_via_actions(state);
                    // Apply the copy to the caret block so the panel focuses it.
                    self.dispatch_apply_named_style(state, &new_id);
                    self.refresh_blocks(state);
                    self.toast = Some((t("toast-style-duplicated"), ToastKind::Success));
                }
            }
        }
        self.lifecycle.dirty = true;
    }

    // ---- new-from-template picker -----------------------------------------

    fn request_template(&mut self, state: &mut ui::widget::Edits<'_>, template: crate::Template) {
        self.apply_template(state, template);
    }

    /// Open the "New" picker under the New button (under the toolbar's
    /// leading edge while New sits in the More menu).
    fn open_template_picker(&mut self) {
        let gap = f64::from(self.desk_theme.tokens.spacing.s2);
        let anchor = self
            .toolbar
            .iter()
            .find(|(action, _)| *action == ToolAction::NewMenu)
            .map_or_else(
                || {
                    Rect::new(
                        self.tool_band.x0,
                        self.tool_band.y0,
                        self.tool_band.x0,
                        self.tool_band.y1,
                    )
                },
                |(_, rect)| *rect,
            );
        let anchor = Rect::new(anchor.x0, anchor.y0 - gap, anchor.x1, anchor.y1 + gap);
        self.template_picker
            .set_text_scale(crate::num::f32_of_f64(self.text_scale));
        self.template_picker
            .open_anchored(anchor, self.floating_viewport());
        self.lifecycle.dirty = true;
    }

    /// Where menus and pickers may float: the window less a small margin,
    /// so a floating panel never runs flush against the window's edge.
    fn floating_viewport(&self) -> Rect {
        self.rect
            .inset(-f64::from(self.desk_theme.tokens.spacing.s2))
    }

    /// Replace the open document's content with `template`'s Markdown, **in place**
    /// on the live `state` (preserving the host `ui::State` and its undo history /
    /// collaboration session), as a new untitled document. Re-seats the caret, refreshes the view, flags an
    /// undo point, and counts as a document-modifying edit (so autosave still
    /// protects the freshly-templated content).
    fn apply_template(&mut self, state: &mut ui::widget::Edits<'_>, template: crate::Template) {
        self.replace_body_via_action(state, template.content());
        self.path = None;
        self.format = Some(WpFormat::Markdown);
        self.scroll_y = 0.0;
        self.refresh_blocks(state);
        // The whole body changed; per-block caches keyed by id/text are stale.
        self.layout_cache.clear();
        self.recent_layout_cache.clear();
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
    }

    fn open_comment_thread(&mut self, comment: &str) {
        if !self.review.comments().iter().any(|item| item.id == comment) {
            self.toast = Some((t("review-thread-unavailable"), ToastKind::Error));
            return;
        }
        self.thread_comment = Some(comment.to_string());
        self.thread_snapshot = None;
        self.thread_reply.widget_mut().source_mut().clear();
        self.thread_reply.widget_mut().present_now();
        self.focus_thread_reply(true);
        self.lifecycle.dirty = true;
    }

    fn close_comment_thread(&mut self) {
        self.thread_comment = None;
        self.thread_snapshot = None;
        self.thread_panel = Rect::ZERO;
        self.thread_submit = Rect::ZERO;
        self.thread_cancel = Rect::ZERO;
        self.focus_thread_reply(false);
        self.lifecycle.dirty = true;
    }

    /// Put the caret in the reply field, or take it out.
    fn focus_thread_reply(&mut self, focused: bool) {
        let surface = self.thread_reply.widget_mut();
        surface.set_focus(focused);
        if focused {
            surface
                .editor_mut()
                .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        }
    }

    fn submit_comment_reply(&mut self, state: &mut ui::widget::Edits<'_>) {
        let Some(comment) = self.thread_comment.clone() else {
            return;
        };
        let draft = self.thread_reply.widget().source();
        if draft.is_empty() {
            return;
        }
        self.perform_verb(
            state,
            crate::model::DocumentAction::ReplyToComment(crate::model::document::ReplyToComment {
                comment: ui::tilo_resource::Id::from_raw(comment),
                text: draft.plain_text().trim().to_owned(),
                body: Some(draft.clip_blocks()),
            }),
        );
        self.thread_reply.widget_mut().source_mut().clear();
        self.thread_reply.widget_mut().present_now();
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
    }

    fn handle_comment_thread_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EditorEventCtx<'_>,
    ) -> bool {
        if self.thread_comment.is_none() {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.route_thread_reply(event, ctx);
            return false;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            if self.thread_submit.contains(self.pointer) {
                self.submit_comment_reply(&mut ctx.state);
                *ctx.handled = true;
                return true;
            }
            if self.thread_cancel.contains(self.pointer) {
                self.close_comment_thread();
                *ctx.handled = true;
                return true;
            }
            if !self.thread_panel.contains(self.pointer) {
                self.close_comment_thread();
                *ctx.handled = true;
                return true;
            }
            if self.thread_reply.rect().contains(self.pointer) {
                self.focus_thread_reply(true);
            }
        }
        self.route_thread_reply(event, ctx);
        match self.thread_reply.widget_mut().take_outcome() {
            Some(FieldOutcome::Submit) => self.submit_comment_reply(&mut ctx.state),
            Some(FieldOutcome::Cancel) => self.close_comment_thread(),
            None => {}
        }
        *ctx.handled = true;
        true
    }

    /// Give the reply field one event and bring it up to date.
    fn route_thread_reply(&mut self, event: &WindowEvent, ctx: &mut EditorEventCtx<'_>) {
        let mut handled = *ctx.handled;
        self.thread_reply.handle_event(
            event,
            ctx.scale_factor,
            &ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            &mut handled,
        );
        self.thread_reply.sync(ctx.state.doc(), ctx.path);
        self.lifecycle.dirty = true;
    }

    /// Act on a tracked-change / comment review affordance (from a margin-card
    /// button or a context-menu row): accept/reject a change, accept/reject all,
    /// or resolve (delete) a comment. Refreshes the materialized blocks +
    /// comments/changes so the acted-on item disappears, and flags an undo point.
    fn dispatch_review_action(&mut self, action: ReviewAction, state: &mut ui::widget::Edits<'_>) {
        use crate::model::{DocumentAction, document};
        let verb = match action {
            ReviewAction::AcceptChange(change) => {
                DocumentAction::AcceptChange(document::AcceptChange { change })
            }
            ReviewAction::RejectChange(change) => {
                DocumentAction::RejectChange(document::RejectChange { change })
            }
            ReviewAction::AcceptAllChanges => DocumentAction::AcceptAllChanges(().into()),
            ReviewAction::RejectAllChanges => DocumentAction::RejectAllChanges(().into()),
            ReviewAction::ResolveComment(comment) => {
                DocumentAction::ResolveComment(document::ResolveComment {
                    comment: ui::tilo_resource::Id::from_raw(comment),
                })
            }
            ReviewAction::OpenCommentThread(id) => {
                self.open_comment_thread(&id);
                return;
            }
        };
        self.perform_verb(state, verb);
        self.refresh_blocks(state);
        // Accepting/rejecting can change body text (an accepted deletion removes
        // glyphs, a rejected insertion removes them); the spell cache is keyed by
        // text so affected blocks re-check on the next draw — clear it wholesale
        // (cheap, and these are infrequent review actions).
        self.spell_cache.clear();
        self.lifecycle.dirty = true;
    }

    /// Test-visible labels of the currently-open context menu's rows
    /// (separators omitted), in order. Empty when the menu is closed.
    #[cfg(test)]
    fn context_menu_labels(&self) -> Vec<String> {
        if !self.context_menu.is_open() {
            return Vec::new();
        }
        self.context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect()
    }

    /// The status readouts on the trailing side of the status band.
    fn status_readouts(&self) -> Vec<String> {
        let words: usize = self
            .blocks
            .iter()
            .map(|block| block.text.split_whitespace().count())
            .sum();
        let chars: usize = self
            .blocks
            .iter()
            .map(|block| block.text.chars().count())
            .sum();
        vec![
            t_args("status-words", &[arg("words", num::i64_of_usize(words))]),
            t_args("status-chars", &[arg("chars", num::i64_of_usize(chars))]),
        ]
    }

    /// Route an event to the fields hosted in the inspector and the status
    /// band (the style property fields, the line-spacing menu and the zoom
    /// field): hover always, a press on one focuses it, typing goes to the
    /// focused one. A value they produce is applied here. Returns whether
    /// one of them took the event.
    fn route_hosted_fields(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = match event {
            WindowEvent::CursorMoved { position, .. } => {
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor)
            }
            _ => self.pointer,
        };
        let mut took = false;
        let panel = self.styles_panel_visible();
        // An open menu owns every event until it closes.
        let open_menu = [&mut self.style_pick, &mut self.line_spacing]
            .into_iter()
            .find(|menu| panel && menu.is_open())
            .map(|menu| Self::forward(menu, event, ctx));
        if open_menu.is_some() {
            self.drain_hosted_fields(&mut ctx.state);
            *ctx.handled = true;
            self.lifecycle.dirty = true;
            return true;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                if panel {
                    for slot in &mut self.num_inputs {
                        Self::forward(&mut slot.input, event, ctx);
                    }
                    Self::forward(&mut self.line_spacing, event, ctx);
                    Self::forward(&mut self.style_pick, event, ctx);
                }
                Self::forward(&mut self.zoom_input, event, ctx);
                if *ctx.handled {
                    self.lifecycle.dirty = true;
                }
                // Hover is shared with the rest of the editor.
                *ctx.handled = false;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => took = self.press_hosted(pointer, panel, event, ctx),
            WindowEvent::MouseInput { .. } => {
                for slot in &mut self.num_inputs {
                    if Widget::wants_pointer_capture(&slot.input) {
                        Self::forward(&mut slot.input, event, ctx);
                        took = true;
                    }
                }
                if Widget::wants_pointer_capture(&self.zoom_input) {
                    Self::forward(&mut self.zoom_input, event, ctx);
                    took = true;
                }
                for menu in [&mut self.line_spacing, &mut self.style_pick] {
                    if Widget::wants_pointer_capture(menu) {
                        Self::forward(menu, event, ctx);
                        took = true;
                    }
                }
            }
            WindowEvent::KeyboardInput { event: key, .. } => {
                let done = key.state == ElementState::Pressed
                    && matches!(
                        key.logical_key,
                        Key::Named(NamedKey::Enter | NamedKey::Escape | NamedKey::Tab)
                    );
                took = self.type_into_hosted(event, ctx);
                if took && done {
                    self.focus_hosted(None, false);
                }
            }
            WindowEvent::Ime(_) => took = self.type_into_hosted(event, ctx),
            _ => {}
        }
        self.drain_hosted_fields(&mut ctx.state);
        if took {
            *ctx.handled = true;
            self.lifecycle.dirty = true;
        }
        took
    }

    /// A press on a hosted field focuses it and goes to it; returns
    /// whether one took it.
    fn press_hosted(
        &mut self,
        pointer: Point,
        panel: bool,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let field = panel
            .then(|| {
                self.num_inputs
                    .iter()
                    .find(|slot| slot.rect.contains(pointer))
                    .map(|slot| slot.field)
            })
            .flatten();
        let zoom = !self.styles_panel_full_window() && self.zoom_rect.contains(pointer);
        self.focus_hosted(field, zoom);
        if let Some(field) = field {
            if let Some(slot) = self.num_inputs.iter_mut().find(|s| s.field == field) {
                Self::forward(&mut slot.input, event, ctx);
            }
        } else if zoom {
            Self::forward(&mut self.zoom_input, event, ctx);
        } else if panel && self.line_spacing_rect.contains(pointer) {
            Self::forward(&mut self.line_spacing, event, ctx);
        } else if panel && self.style_pick_rect.contains(pointer) {
            Self::forward(&mut self.style_pick, event, ctx);
        } else {
            return false;
        }
        true
    }

    /// Hand typing to the hosted field that has the keyboard; returns
    /// whether one had it.
    fn type_into_hosted(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let foreground = self.styles_panel_full_window();
        let body = self.panel_body;
        if let Some(field) = self.editing_field {
            let Some(slot) = self.num_inputs.iter_mut().find(|slot| {
                let visible = slot.rect.intersect(body);
                slot.field == field
                    && (!foreground || (visible.width() > 0.0 && visible.height() > 0.0))
            }) else {
                return false;
            };
            Self::forward(&mut slot.input, event, ctx);
            true
        } else if foreground
            && self.visible_styles_rect(self.line_spacing_rect).is_some()
            && Widget::is_focused(&self.line_spacing)
        {
            Self::forward(&mut self.line_spacing, event, ctx);
            true
        } else if foreground
            && self.visible_styles_rect(self.style_pick_rect).is_some()
            && Widget::is_focused(&self.style_pick)
        {
            Self::forward(&mut self.style_pick, event, ctx);
            true
        } else if !foreground && Widget::is_focused(&self.zoom_input) {
            Self::forward(&mut self.zoom_input, event, ctx);
            true
        } else {
            false
        }
    }

    /// Give keyboard focus to one hosted field (a style property, or the
    /// zoom field), taking it from the others; `None, false` releases it.
    fn focus_hosted(&mut self, field: Option<NumField>, zoom: bool) {
        for slot in &mut self.num_inputs {
            let on = Some(slot.field) == field;
            if Widget::is_focused(&slot.input) != on {
                Widget::set_focus(&mut slot.input, on);
            }
        }
        if Widget::is_focused(&self.zoom_input) != zoom {
            Widget::set_focus(&mut self.zoom_input, zoom);
        }
        self.editing_field = field;
    }

    /// Hand one event to a hosted widget.
    fn forward(widget: &mut dyn Widget, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        widget.handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
    }

    /// Apply what the hosted fields produced: a style property, a line
    /// spacing, or a zoom level.
    fn drain_hosted_fields(&mut self, state: &mut ui::widget::Edits<'_>) {
        let changes: Vec<(NumField, f64)> = self
            .num_inputs
            .iter_mut()
            .filter_map(|slot| slot.input.take_changed().map(|v| (slot.field, v)))
            .collect();
        for (field, value) in changes {
            self.edit_focused_style(state, true, |st| field.set(st, num::f32_of_f64(value)));
        }
        if let Some(index) = self.line_spacing_pick.take()
            && let Some(spacing) = block_layout::LINE_SPACINGS.get(index).copied()
        {
            self.dispatch_block_attr(
                state,
                "line_spacing",
                crate::actions::AttrVal::Float(spacing),
            );
            self.refresh_blocks(state);
            self.lifecycle.dirty = true;
        }
        if let Some(id) = self.style_pick_choice.take() {
            self.apply_panel_hit(state, PanelHit::Pick(id));
        }
        if let Some(percent) = self.zoom_input.take_changed() {
            self.set_zoom(percent / 100.0);
        }
    }

    /// The toolbar's commands for this frame, grouped: New; bold, italic
    /// and underline; type and colour; the rarer character formats;
    /// insertion (the table group turns into
    /// table editing inside a cell); then, at the trailing edge, the view
    /// toggles. Commands that do not fit go to the toolbar's More menu.
    fn toolbar_entries(&self) -> Vec<ToolEntry> {
        let mut entries = vec![
            ToolEntry::command(ToolAction::NewMenu, t("toolbar-new")),
            ToolEntry::Separator,
            ToolEntry::toggle(
                ToolAction::Bold,
                t("toolbar-bold"),
                self.mark_active_in_selection("strong"),
            ),
            ToolEntry::toggle(
                ToolAction::Italic,
                t("toolbar-italic"),
                self.mark_active_in_selection("em"),
            ),
            ToolEntry::toggle(
                ToolAction::Underline,
                t("toolbar-underline"),
                self.mark_active_in_selection("underline"),
            ),
            ToolEntry::Separator,
            ToolEntry::command(ToolAction::FontCycle, self.current_font().label()),
            ToolEntry::command(ToolAction::FontSizeDec, t("toolbar-size-down")),
            ToolEntry::command(ToolAction::FontSizeInc, t("toolbar-size-up")),
            ToolEntry::command(ToolAction::TextColorPalette, t("toolbar-text-color")),
            ToolEntry::command(ToolAction::HighlightPalette, t("toolbar-highlight")),
            ToolEntry::Separator,
            ToolEntry::toggle(
                ToolAction::Strike,
                t("toolbar-strikethrough"),
                self.mark_active_in_selection("strike"),
            ),
            ToolEntry::toggle(
                ToolAction::Super,
                t("toolbar-superscript"),
                self.mark_active_in_selection("super"),
            ),
            ToolEntry::toggle(
                ToolAction::Sub,
                t("toolbar-subscript"),
                self.mark_active_in_selection("sub"),
            ),
            ToolEntry::Separator,
        ];
        if !self.selection_blocks_cached().is_empty() {
            entries.push(ToolEntry::command(ToolAction::Hyperlink, t("toolbar-link")));
        }
        if self.current_block_style().0 == "cell" {
            entries.extend([
                ToolEntry::command(ToolAction::TableRowAdd, t("toolbar-row-add")),
                ToolEntry::command(ToolAction::TableRowDel, t("toolbar-row-del")),
                ToolEntry::command(ToolAction::TableColAdd, t("toolbar-col-add")),
                ToolEntry::command(ToolAction::TableColDel, t("toolbar-col-del")),
                ToolEntry::command(ToolAction::TableDelete, t("toolbar-table-delete")),
            ]);
        } else {
            entries.push(ToolEntry::command(
                ToolAction::InsertTable,
                t("toolbar-table"),
            ));
        }
        entries.extend([
            ToolEntry::command(ToolAction::InsertImage, t("toolbar-image")),
            ToolEntry::command(ToolAction::InsertToc, t("toolbar-toc")),
            ToolEntry::Spacer,
            ToolEntry::toggle(
                ToolAction::ToggleRuler,
                t("toolbar-ruler"),
                self.view.ruler_visible,
            ),
        ]);
        entries
    }

    /// Bring the hosted toolbar in line with this frame's commands: rebuilt
    /// when a command appears, goes or is renamed, otherwise only its toggles
    /// are set, so hover, focus and an open More menu survive.
    fn sync_toolbar(&mut self) {
        let entries = self.toolbar_entries();
        let same = entries.len() == self.tool_entries.len()
            && entries
                .iter()
                .zip(&self.tool_entries)
                .all(|(a, b)| a.same_shape(b));
        if same {
            for (index, (now, before)) in entries.iter().zip(&self.tool_entries).enumerate() {
                if let (
                    ToolEntry::Command {
                        active: Some(on), ..
                    },
                    ToolEntry::Command {
                        active: Some(was), ..
                    },
                ) = (now, before)
                    && on != was
                {
                    self.tool_bar.set_active(index, *on);
                }
            }
        } else {
            let mut bar = ui::widget::Toolbar::new(ui::widget::toolbar::Orientation::Horizontal);
            for entry in &entries {
                bar = match entry {
                    ToolEntry::Command {
                        action,
                        label,
                        active,
                    } => {
                        let fired = std::rc::Rc::clone(&self.tool_fired);
                        let action = *action;
                        let on_click = move || fired.set(Some(action));
                        let command = match (action.icon(), active) {
                            (Some(icon), Some(on)) => {
                                bar.push_icon_toggle(icon, label, *on, on_click)
                            }
                            (Some(icon), None) => bar.push_icon(icon, label, on_click),
                            (None, Some(on)) => bar.push_toggle(label, *on, on_click),
                            (None, None) => bar.push_button(label, on_click),
                        };
                        let id = format!("word.toolbar.{}", tool_action_id(action));
                        // Admission runs the control callback; settlement submits
                        // document edits through their existing typed dispatcher.
                        if action.edits_document() || action == ToolAction::InsertImage {
                            command.with_effect(&id)
                        } else {
                            command.with_view(&id)
                        }
                    }
                    ToolEntry::Separator => bar.push_separator(),
                    ToolEntry::Spacer => bar.push_spacer(),
                };
            }
            self.tool_bar = bar;
            self.retained_tree_stale = true;
        }
        self.tool_entries = entries;
    }

    /// Paint the outline and the inspector on their drawers: the region
    /// beside the page when docked, the floating material over a scrim when
    /// a drawer covers the page. The outline is drawn after `self.laid` is
    /// set so its current heading reflects this frame's pagination.
    fn draw_drawers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.outline_drawer.sidebar_rect().width() > 0.5 {
            self.outline_drawer.draw_scrim(scene, transform, theme);
            let local = self.outline_drawer.paint_panel(scene, transform, theme);
            let entries = self.outline_entries();
            let active = self.active_outline_block();
            self.outline.draw(
                &mut PaintContext::new(scene, transform, text_renderer),
                &local,
                self.outline_drawer.sidebar_rect(),
                &entries,
                active.as_deref(),
            );
        } else {
            self.outline.clear_frame();
        }
        if self.styles_panel_full_window() {
            // A compact inspector is a page, not floating chrome. Use the
            // same material and breakpoint as shared full-window dialogs.
            ui::floating_surface::paint_material(
                scene,
                transform,
                &self.rect,
                ui::floating_surface::Material::Opaque,
                theme,
            );
            let local = theme.on_ground(ui::chrome::panel_ground(theme));
            self.draw_styles_panel(scene, transform, text_renderer, &local, self.rect);
        } else if self.inspector_drawer.sidebar_rect().width() > 0.5 {
            self.inspector_drawer.draw_scrim(scene, transform, theme);
            let local = self.inspector_drawer.paint_panel(scene, transform, theme);
            let panel = self.inspector_drawer.sidebar_rect();
            self.draw_styles_panel(scene, transform, text_renderer, &local, panel);
        } else {
            self.panel_rect = Rect::ZERO;
            self.panel_body = Rect::ZERO;
            self.panel_hits.clear();
            for slot in &mut self.num_inputs {
                slot.rect = Rect::ZERO;
            }
            self.line_spacing_rect = Rect::ZERO;
            self.style_pick_rect = Rect::ZERO;
        }
    }

    /// Bring the outline and inspector drawers in line with what is shown,
    /// advance their reveal, and lay them out in `below` (everything under
    /// the toolbar). Returns the page's column.
    fn resolve_drawers(&mut self, theme: &Theme, below: Rect) -> Rect {
        for (drawer, open) in [
            (&mut self.outline_drawer, self.outline.is_open()),
            (&mut self.inspector_drawer, self.view.styles_panel_visible),
        ] {
            drawer.scale_for(theme);
            drawer.resolve(below);
            if drawer.is_open() != open {
                drawer.snap(open);
            }
            drawer.tick(theme);
        }
        let column = self.outline_drawer.resolve(below);
        self.inspector_drawer.resolve(column)
    }

    /// The toolbar band's height: the window's header row.
    fn toolbar_band_height(theme: &Theme) -> f64 {
        ui::chrome::HeaderRowFace::height(theme)
    }

    /// Lay out and paint the toolbar in its band `tb` (the band has no fill
    /// of its own, only a hairline under it), then record each visible
    /// command's rect for hit-testing and the accessibility tree.
    fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tb: Rect,
    ) {
        self.sync_toolbar();
        ui::widget::toolbar::paint_band(scene, transform, theme, tb);
        // The band is the window's header row: the outline's toggle at the
        // leading corner, the window's History control (when offered) at the
        // trailing one with the inspector's toggle before it, the commands
        // between them.
        let leading = self.outline_drawer.header(tb, theme);
        let trailing =
            self.inspector_drawer
                .header_with_history(tb, theme, self.window_history.is_shown());
        self.outline_toggle = self.outline_drawer.toggle_rect();
        self.inspector_toggle = self.inspector_drawer.toggle_rect();
        self.window_history_rect = trailing.history_rect();
        self.outline_drawer
            .paint_toggle(scene, transform, tr, theme);
        self.inspector_drawer
            .paint_toggle(scene, transform, tr, theme);
        if let Some(button) = self.window_history.button_mut() {
            button.apply_layout_external(self.window_history_rect);
            let presence = PresenceCtx::empty();
            Widget::draw(button, scene, transform, tr, theme, &presence);
        }
        let chip = leading.metrics().chip;
        let y0 = tb.center().y - chip / 2.0;
        let inner = Rect::new(
            leading.actions.x0,
            y0,
            trailing.actions.x1.max(leading.actions.x0),
            y0 + chip,
        );
        self.tool_bar.lay_out_in(inner, tr, theme);
        let presence = PresenceCtx::empty();
        presence.viewport.set(Some(self.floating_viewport()));
        Widget::draw(&mut self.tool_bar, scene, transform, tr, theme, &presence);
        self.toolbar.clear();
        self.toolbar
            .push((ToolAction::ToggleOutline, self.outline_toggle));
        let mut target = 0usize;
        for entry in self.tool_entries.iter().take(self.tool_bar.visible_count()) {
            if let ToolEntry::Command { action, .. } = entry {
                if let Some(rect) = self.tool_bar.synthetic_accessibility_rect(&[target]) {
                    self.toolbar.push((*action, rect));
                }
                target += 1;
            }
        }
        self.toolbar
            .push((ToolAction::ToggleStyles, self.inspector_toggle));
    }

    /// Hosted controls receive input first; the foreground Styles page
    /// consumes remaining text instead of editing the displaced document.
    fn route_styles_page_key(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.styles_panel_full_window() {
            return false;
        }
        match event {
            // Only application file commands may leave the foreground page.
            // Formatting, clipboard and deletion shortcuts belong to its
            // hosted fields (which have already received the event).
            WindowEvent::KeyboardInput { event: key, .. }
                if (ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key())
                    && matches!(&key.logical_key, Key::Character(key)
                        if key.eq_ignore_ascii_case("s") || key.eq_ignore_ascii_case("o")) =>
            {
                false
            }
            WindowEvent::KeyboardInput { event: key, .. } => {
                if key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
                {
                    self.apply_panel_hit(&mut ctx.state, PanelHit::Close);
                }
                *ctx.handled = true;
                true
            }
            WindowEvent::Ime(_)
            | WindowEvent::DroppedFile(_)
            | WindowEvent::HoveredFile(_)
            | WindowEvent::HoveredFileCancelled
            | WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                *ctx.handled = true;
                true
            }
            _ => false,
        }
    }

    /// Route an event to the toolbar: hover always, presses inside its band,
    /// the release that completes one, and everything while its More menu is
    /// open. A command it fires runs here. Returns whether the toolbar took
    /// the event.
    fn route_toolbar_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = match event {
            WindowEvent::CursorMoved { position, .. } => Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            )),
            _ => None,
        };
        // The window's History control and the drawers' toggles answer for
        // their own places in the row.
        if let Some(button) = self.window_history.button_mut() {
            let mut handled = false;
            button.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if button.needs_redraw() {
                self.lifecycle.dirty = true;
            }
            if handled && !matches!(event, WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
                return true;
            }
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. }
        ) {
            self.outline_drawer.handle_event(event, ctx);
            self.inspector_drawer.handle_event(event, ctx);
        }
        let menu_open = self.tool_bar.is_overflow_open();
        let in_band = pointer.unwrap_or(self.pointer);
        let claims = menu_open
            || match event {
                WindowEvent::CursorMoved { .. } => true,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => self.tool_band.contains(in_band),
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => self.tool_bar.wants_pointer_capture(),
                _ => false,
            };
        if !claims {
            return false;
        }
        if !menu_open
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            let toggle = if self.outline_toggle.contains(in_band) {
                Some(ToolAction::ToggleOutline)
            } else if self.inspector_toggle.contains(in_band) {
                Some(ToolAction::ToggleStyles)
            } else {
                None
            };
            if let Some(action) = toggle {
                self.tool_clicked(&mut ctx.state, action);
                *ctx.handled = true;
                return true;
            }
        }
        let mut handled = false;
        Widget::handle_event(
            &mut self.tool_bar,
            event,
            &mut ui::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        if handled
            || (matches!(event, WindowEvent::CursorMoved { .. })
                && (self.outline_toggle.contains(in_band)
                    || self.inspector_toggle.contains(in_band)))
        {
            self.lifecycle.dirty = true;
        }
        if let Some(action) = self.tool_fired.take() {
            self.tool_clicked(&mut ctx.state, action);
        }
        // Hover is shared with the rest of the editor; everything else the
        // toolbar takes is its own.
        if matches!(event, WindowEvent::CursorMoved { .. }) && !menu_open {
            return false;
        }
        *ctx.handled = true;
        true
    }

    /// Run a toolbar command chosen by pointer, keyboard or the More menu.
    fn tool_clicked(&mut self, state: &mut ui::widget::Edits<'_>, action: ToolAction) {
        self.apply_tool(state, action);
        self.refresh_blocks(state);
        self.lifecycle.dirty = true;
        if action.edits_document() {}
    }

    /// The per-run `color` covering the font-probe position (just left of the
    /// caret), if any — used to ring the active swatch.
    fn current_text_color(&self) -> Option<[u8; 3]> {
        let (b, pos) = self.font_probe()?;
        b.marks
            .iter()
            .find(|m| m.name == "color" && pos >= m.start && pos < m.end)
            .and_then(|m| m.value.as_str())
            .and_then(parse_hex_rgb)
    }
}

/// The editor as the Find bar's host: it searches the whole document and
/// replaces matches through the editor's own undoable splice.
struct EditorFind<'a, 'doc> {
    wp: &'a mut WordProcessorWidget,
    state: &'a mut ui::widget::Edits<'doc>,
}

impl FindHost for EditorFind<'_, '_> {
    fn search_blocks(&mut self) -> Vec<BlockView> {
        self.wp.search_blocks(self.state)
    }

    fn replace_matches(&mut self, matches: &[FindMatch], with: &str) -> usize {
        let mut matches = matches.to_vec();
        // Replace from the highest char offset down so earlier offsets in a
        // block stay valid.
        matches.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        for (block, start, end) in matches.iter().rev() {
            self.wp.splice_via_action(
                self.state,
                crate::actions::RegionKind::Body,
                block,
                *start,
                *end,
                with,
            );
        }
        if !matches.is_empty() {
            self.wp.refresh_blocks(self.state);
            self.wp.lifecycle.dirty = true;
        }
        matches.len()
    }
}
impl WordProcessorWidget {
    #[expect(
        clippy::too_many_lines,
        reason = "the widget's single event entry point routes each window event to overlays, panels and the editor in priority order"
    )]
    fn route_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.styles_panel_full_window() {
            // Displaced document overlays cannot act above the Styles page.
            self.context_menu.close();
            if self.thread_comment.is_some() {
                self.close_comment_thread();
            }
        }
        if let WindowEvent::Touch(touch) = event
            && self.handle_segmented_touch(touch, ctx.scale_factor, &mut ctx.state)
        {
            *ctx.handled = true;
            return;
        }
        let shift = ctx.modifiers.state().shift_key();
        let ctrl = ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
        // An open menu owns input until selection or dismissal. Forward
        // movement as well as presses so the shared menu can paint its hover;
        // keep navigation/typing from editing the document behind the popup.
        if self.template_picker.is_open()
            || self.context_menu.is_open()
            || self.style_menu.is_open()
        {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            let menu = if self.template_picker.is_open() {
                self.template_picker.menu_mut()
            } else if self.style_menu.is_open() {
                self.style_menu.menu_mut()
            } else {
                &mut self.context_menu
            };
            menu.handle_event(event, ctx);
            if let Some(template) = self.template_picker.take_selection() {
                self.request_template(&mut ctx.state, template);
            }
            if let Some(action) = self.style_menu.take_selection() {
                self.dispatch_style_menu(action, &mut ctx.state);
            }
            if let Some(index) = self.context_menu.take_selection() {
                self.dispatch_context_action(index, &mut ctx.state);
            }
            if matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::PinchGesture { .. }
                    | WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
            ) {
                self.lifecycle.dirty = true;
                *ctx.handled = true;
                return;
            }
        }
        if self.pump_compact_swatch_palette(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        ) {
            return;
        }
        // While the colour-picker popover is open it captures input (modal-ish):
        // route drags to it, apply committed colours, and let an outside-click /
        // Escape dismiss it before any page/panel hit-testing.
        if self.color_picker.is_open()
            && self.pump_color_picker(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
                ctx.handled,
            )
        {
            return;
        }
        // The expanded comment thread owns its reply field and modal-like
        // button surface. Keep it ahead of the editor and Find bar so typing
        // never leaks into the document beneath it.
        let comment_thread_handled = self.handle_comment_thread_event(
            event,
            &mut EditorEventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: ctx.handled,
            },
        );
        if comment_thread_handled {
            return;
        }
        if !self.styles_panel_full_window() && self.route_toolbar_event(event, ctx) {
            return;
        }
        if self.route_hosted_fields(event, ctx) {
            return;
        }
        if self.route_styles_page_key(event, ctx) {
            return;
        }
        let tb_h = self.tool_band.height();

        // Find/replace embeds the shared TextInput rather than maintaining a
        // second text editor. Feed it raw events first so IME, selection and
        // clipboard shortcuts retain the toolkit's exact behavior. Ctrl+F is
        // deliberately left for the editor-level toggle below.
        let over_find = self.find.is_open()
            && match event {
                WindowEvent::MouseInput { .. } => self.find_band.contains(self.pointer),
                _ => false,
            };
        // The bar takes typing while the editor has focus, the pointer over
        // its band, and hover; the wheel and gestures stay the page's.
        let for_find = match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => self.lifecycle.focused,
            WindowEvent::CursorMoved { .. } => true,
            WindowEvent::MouseInput { .. } => over_find || self.lifecycle.focused,
            _ => false,
        };
        if self.find.is_open() && for_find {
            let mut find_handled = false;
            self.find.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut find_handled,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            self.drive_find(&mut ctx.state);
            if over_find
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
                )
            {
                self.lifecycle.focused = true;
                self.find.set_host_focused(true);
                *ctx.focus_target = Some(ctx.node_id);
            }
            if !self.find.is_open() {
                self.lifecycle.dirty = true;
            }
            if find_handled || over_find {
                self.lifecycle.dirty = true;
                *ctx.handled = true;
                // Cursor movement still feeds the editor below so its shared
                // pointer coordinate remains current for subsequent clicks.
                if !matches!(event, WindowEvent::CursorMoved { .. }) {
                    return;
                }
            }
        }
        match event {
            // Drag-and-drop a file onto the window inserts it as an image at the
            // drop point (see `handle_dropped_file`).
            WindowEvent::DroppedFile(path) => {
                self.handle_dropped_file(&mut ctx.state, path, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Window groups offer unclaimed events to visible apps in
                // presentation order. Ctrl+wheel represents a browser
                // trackpad pinch, so an unfocused Writer must leave it for
                // the host-selected app rather than zoom in the background.
                if self.lifecycle.focused || !ctrl {
                    self.handle_mouse_wheel(delta, ctx.scale_factor, ctrl, ctx.handled);
                }
            }
            WindowEvent::PinchGesture { delta, phase, .. } => {
                // Native pinch follows the same host-ownership rule as the
                // browser's Ctrl+wheel form above. Pointer geometry still
                // determines where a focused Writer zooms.
                if self.lifecycle.focused && self.zoom_viewport.contains(self.pointer) {
                    *ctx.handled = true;
                    if delta.is_finite()
                        && *delta != 0.0
                        && *phase != winit::event::TouchPhase::Cancelled
                        && !self.color_picker.is_open()
                        && !self.compact_swatch_palette.is_open()
                        && !(self.styles_panel_overlay() && self.panel_rect.contains(self.pointer))
                    {
                        let before = self.zoom.factor();
                        let after = (before * (1.0 + delta).max(0.01)).clamp(ZOOM_MIN, ZOOM_MAX);
                        if self.set_zoom(after) {
                            // Keep the document row beneath the gesture steady.
                            // Horizontal alignment remains the centered-page convention.
                            let pointer_y = self.pointer.y - self.zoom.anchor().y;
                            self.scroll_y =
                                (self.scroll_y + pointer_y / before - pointer_y / after).max(0.0);
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(
                    position.x,
                    position.y,
                    ctx.scale_factor,
                    tb_h,
                    ctx.handled,
                );
            }
            // Right (secondary) press opens the editing / spell-check context
            // menu on the page body. Toolbar / panel / status-bar chrome ignore
            // it (they have their own hit-testing on the left button).
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let was_handled = *ctx.handled;
                self.handle_right_press(&ctx.state, tb_h, ctx.handled);
                if !was_handled && self.lifecycle.focused && self.rect.contains(self.pointer) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let was_handled = *ctx.handled;
                self.handle_left_press(&mut ctx.state, tb_h, shift, ctrl, ctx.handled);
                if !was_handled && self.lifecycle.focused && self.rect.contains(self.pointer) {
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_left_release(&mut ctx.state, ctx.handled),
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                self.inner.clear_preedit();
                self.handle_key_press(
                    &mut ctx.state,
                    &Key::Character(text.as_str().into()),
                    Some(text),
                    false,
                    false,
                    ctx.handled,
                );
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(text, cursor)) => {
                if text.is_empty() {
                    self.inner.clear_preedit();
                } else {
                    self.inner.set_preedit(text, *cursor);
                }
                self.lifecycle.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                self.inner.clear_preedit();
                self.lifecycle.dirty = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                if self.handle_modified_navigation(&ctx.state, logical_key, *ctx.modifiers) {
                    *ctx.handled = true;
                } else {
                    self.handle_key_press(
                        &mut ctx.state,
                        logical_key,
                        text.as_deref(),
                        shift,
                        ctrl,
                        ctx.handled,
                    );
                }
            }
            _ => {}
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

/// A path the windowing toolkit spooled a drag-and-drop payload into: lives
/// under `$XDG_RUNTIME_DIR` and is named `winit-dnd-…`. Only such files are
/// deleted after an insert — never a file the user chose.
fn is_dnd_spool_path(path: &std::path::Path) -> bool {
    let named_by_the_toolkit = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("winit-dnd-"));
    if !named_by_the_toolkit {
        return false;
    }
    // The toolkit spools into the first of these it can actually write
    // to — inside a sandbox `$XDG_RUNTIME_DIR` is set but is not always
    // writable — so a spool file is one that sits under any of them.
    // The name check above is what keeps this from ever matching a file
    // the user chose.
    ["XDG_RUNTIME_DIR", "XDG_CACHE_HOME", "HOME"]
        .into_iter()
        .filter_map(std::env::var_os)
        .filter(|v| !v.is_empty())
        .any(|dir| path.starts_with(dir))
        || tilo_paths::temp_dir().is_some_and(|dir| path.starts_with(dir))
}

#[cfg(test)]
#[path = "widget/mark_boundary_tests.rs"]
mod mark_boundary_tests;
