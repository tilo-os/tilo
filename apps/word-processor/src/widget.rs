//! `WordProcessorWidget` — the `ui::Widget` for the word processor. Renders the
//! `qdoc` document as a centered page (à la `LibreOffice` Writer) using the
//! `RichTextLayout` primitive: word-wrapped paragraphs, headings, list bullets,
//! inline **bold**/*italic*/underline, a selection highlight, and a caret. Maps
//! pointer + keyboard onto the editing model (selection, formatting, save).

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use qdoc::document::MarkSpan;
use qdoc::model::Region;
use qdoc::snapshot::{AttrVal, BlockView, NoteView};
use ui::chrome::{self, LabelCue, ToolbarItem};
use ui::gesture::{DragSelect, Granularity};
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
// Provides `.r()/.g()/.b()` accessors on `vello` `Color` (used to derive
// theme-tinted translucent chrome tones).
use ui::ColorCompat;
use ui::icons::Icon;
use ui::permissions::UserId;
use ui::widget::Widget;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::{PathSegment, PresenceCtx, PresenceKey, clipboard_copy, clipboard_paste};

use crate::WpFormat;
use crate::find_bar::{FindBar, FindEffect, FindPalette};
use crate::i18n::{arg, t, t_args};
use crate::num;
use crate::outline_pane::{OutlinePalette, OutlinePane};
#[cfg(test)]
use crate::recovery_banner::AUTOSAVE_EDIT_THRESHOLD;
use crate::recovery_banner::{RecoveryBanner, RecoveryDecision, RecoveryPalette};
use crate::review_overlay::{
    COMMENT_HIGHLIGHT, DELETION_INK, DELETION_RGB, INSERTION_INK, INSERTION_RGB, ReviewAction,
    ReviewOverlay,
};
#[cfg(test)]
use crate::ruler::RulerMarker;
use crate::ruler::{Ruler, RulerColors, RulerEffect, RulerModel};
use crate::style_color_picker::StyleColorPicker;
use crate::style_menu::{StyleMenu, StyleMenuAction};
use crate::template_picker::TemplatePicker;
use crate::word_processor::{Caret, ClipBlock, WordProcessor};
#[cfg(test)]
use crate::zoom_control::MAX as ZOOM_MAX;
use crate::zoom_control::{MIN as ZOOM_MIN, STEP as ZOOM_STEP, ZoomControl};
use ui::portal;

/// Convert the model's `(block, start, end)` ranges into serializable
/// [`Span`](crate::actions::Span)s for an action payload.
fn to_spans(ranges: Vec<(String, usize, usize)>) -> Vec<crate::actions::Span> {
    ranges
        .into_iter()
        .map(|(block, start, end)| crate::actions::Span { block, start, end })
        .collect()
}

/// Why a portal file-dialog was opened: the drain path routes the chosen file to
/// the matching action. One request is in flight at a time (`portal_rx`), so a
/// single purpose is enough to disambiguate the outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortalPurpose {
    /// Insert the chosen image at the caret (toolbar Image button).
    InsertImage,
    /// Open the chosen document in place, replacing the current one (Ctrl+O).
    OpenDocument,
    /// Save the current document to the chosen path (Save As / first save of an
    /// untitled document).
    SaveAs,
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
/// Selection highlight painted on paper (a translucent accent so text reads
/// through it). Tied to paper, not the theme, so it always contrasts the sheet.
const PAPER_SELECTION: Color = Color::from_rgba8(0x46, 0x78, 0xe6, 0x4d);
/// Caret colour on paper.
const PAPER_CARET: Color = Color::from_rgb8(0x28, 0x5a, 0xc8);
/// Translucent accent wash behind the header/footer band that is the active edit
/// context (Word-style header/footer edit mode), so the focused band stands out.
const ACTIVE_BAND_FILL: Color = Color::from_rgba8(0x46, 0x78, 0xe6, 0x14);
/// Accent edge (dashed outline + the "Header"/"Footer" tab) of the active band.
const ACTIVE_BAND_EDGE: Color = Color::from_rgb8(0x46, 0x78, 0xe6);
/// A translucent veil drawn over the body content while a header/footer is being
/// edited, so the body visibly recedes (like Word dimming the body in that mode).
const BODY_DIM_VEIL: Color = Color::from_rgba8(0xfc, 0xfc, 0xfa, 0x66);

/// OS-theme-driven chrome palette, resolved once per frame from the [`Theme`]
/// (see [`ChromeColors::for_theme`]) so chrome honours light/dark mode. Paper
/// colours (the sheet + its ink) are NOT here — see the `PAPER_*` consts above.
/// + ink on it) are deliberately *not* here — see the `PAPER_*` consts above.
#[derive(Clone, Copy)]
struct ChromeColors {
    /// Canvas behind the page (the desk). `surfaces.base`.
    canvas: Color,
    /// Bar / panel surface (toolbar, status bar, find bar, layout bar, side
    /// panels). `surfaces.raised`.
    bar: Color,
    /// A slightly recessed surface for input fields / value boxes. `surfaces.base`.
    field: Color,
    /// A raised "chip" tone for inactive buttons/chips. Between base and raised.
    chip: Color,
    /// Hairline separators, borders, panel edges, the scrollbar track tone.
    border: Color,
    /// Primary chrome ink (button labels, panel headings).
    text: Color,
    /// Dimmed chrome ink (hints, secondary labels, match counts).
    text_dim: Color,
    /// Accent fill for active controls / layout guides (the "guide" blue).
    accent: Color,
    /// A translucent accent for inactive guide lines.
    accent_faint: Color,
    /// Ink that reads on top of an `accent` fill (active button labels).
    on_accent: Color,
    /// Scrollbar thumb tone.
    scrollbar: Color,
    /// The paper sheet, derived from the theme's lightest surface when it is
    /// genuinely light, else [`PAPER_DEFAULT`]. Always a light tone so the
    /// dark-authored document ink stays readable.
    paper: Color,
    /// How far the paper sheet is lifted off the desk behind it.
    paper_elevation: ui::tokens::Shadow,
}

impl ChromeColors {
    /// Map the OS [`Theme`] onto the editor's chrome palette. Only the closest
    /// available token is used for each slot — no new theme tokens are invented.
    fn for_theme(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        // Prefer the theme's lightest neutral when actually light; dark themes
        // keep the canonical light paper so document ink still contrasts.
        // `neutral[10]` is the lightest ramp slot.
        let lightest = c.neutral[10];
        let paper = if luminance(lightest) >= 0.85 {
            lightest
        } else {
            PAPER_DEFAULT
        };
        Self {
            canvas: c.surfaces.base,
            bar: c.surfaces.raised,
            field: c.surfaces.base,
            chip: c.neutral[2],
            border: c.border_standard,
            text: c.text_primary,
            text_dim: c.text_dim,
            accent: theme.accent_primary,
            accent_faint: with_alpha(theme.accent_primary, 0x70),
            on_accent: Color::WHITE,
            scrollbar: c.neutral[4],
            paper,
            paper_elevation: theme.tokens.elevation.e3,
        }
    }
}

/// Relative luminance (0..1) of a colour — used only to decide whether a theme's
/// "lightest" surface is light enough to serve as paper.
fn luminance(c: Color) -> f64 {
    let r = f64::from(c.r()) / 255.0;
    let g = f64::from(c.g()) / 255.0;
    let b = f64::from(c.b()) / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Return `base` with its alpha replaced (0..=255). Keeps the RGB, used for the
/// translucent accent guide tint.
fn with_alpha(base: Color, alpha: u8) -> Color {
    Color::from_rgba8(base.r(), base.g(), base.b(), alpha)
}

const PAGE_TOP: f64 = 28.0;
const MAX_PAGE_W: f64 = 820.0;
/// Height of the horizontal ruler band carved out of the canvas top (below the
/// toolbar / recovery banner / find bar, above the page).
const RULER_H: f64 = 24.0;
/// Vertical gap between consecutive discrete pages (the canvas shows through it),
/// à la Google Docs. Together with `LETTER_RATIO` this drives true pagination.
const PAGE_GAP: f64 = 22.0;
const CARET_WIDTH: f64 = 2.0;
const TOOLBAR_H: f64 = ui::chrome::TOOLBAR_HEIGHT;
const LAYOUT_BAR_H: f64 = 32.0;
/// Width of the contextual styles panel reserved on the right of the canvas.
const STYLES_PANEL_W: f64 = 250.0;
/// Width of the document outline / navigation pane reserved on the LEFT of the
/// canvas (mirrors the styles panel on the right).
const OUTLINE_PANEL_W: f64 = 230.0;
/// Find-match highlight (amber, semi-transparent).
const FIND_HL: Color = Color::from_rgba8(0xff, 0xc8, 0x4a, 0x80);
/// Spell-check squiggle colour (a familiar red).
const SQUIGGLE: Color = Color::from_rgb8(0xd0, 0x32, 0x2a);
/// Amplitude (px) of the wavy spell-check underline.
const SQUIGGLE_AMP: f64 = 1.6;
/// Horizontal period (px) of one full wave of the spell-check underline.
const SQUIGGLE_PERIOD: f64 = 4.0;
/// Thin separator above the footnote area at the bottom of a page.
const NOTE_SEP: Color = Color::from_rgb8(0xb8, 0xb8, 0xbe);
/// Footnote body text colour (slightly muted vs. body ink).
const NOTE_INK: Color = Color::from_rgb8(0x3a, 0x3a, 0x40);
/// Footnote text size (smaller than body) and the gap above the separator.
const NOTE_FONT: f32 = 11.0;
const NOTE_AREA_GAP: f64 = 10.0;

// Panel + guide chrome colours are theme-driven — see `ChromeColors`, not new consts.
/// A small color palette for the style-colour picker.
const STYLE_PALETTE: [[u8; 3]; 7] = [
    [0x1c, 0x1c, 0x20], // ink
    [0x55, 0x57, 0x60], // muted grey
    [0xb0, 0x3a, 0x2e], // red
    [0x1a, 0x56, 0xc4], // blue
    [0x2e, 0x7d, 0x46], // green
    [0xb5, 0x7e, 0x10], // amber
    [0x7a, 0x3a, 0xa0], // purple
];
/// A small palette of highlight (text background) colours for the swatch row.
/// Familiar word-processor highlighter hues (yellow, green, cyan, pink, orange).
const HIGHLIGHT_PALETTE: [[u8; 3]; 5] = [
    [0xff, 0xf3, 0x5c], // yellow
    [0x9b, 0xf0, 0x6a], // green
    [0x6c, 0xe0, 0xf0], // cyan
    [0xff, 0x9a, 0xd0], // pink
    [0xff, 0xb1, 0x4d], // orange
];
// Layout guides + handles use the theme accent — see `chrome.accent`/`chrome.accent_faint`.
/// How close (px) the pointer must be to a guide line to grab it.
const GUIDE_GRAB: f64 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToolAction {
    Bold,
    Italic,
    Underline,
    Strike,
    FontCycle,
    /// Nudge the selection's font size down one step (a per-run `size` character
    /// mark). Direct formatting on the selection — the style's *default* size
    /// lives separately in the side panel's "Paragraph style" Size field.
    FontSizeDec,
    /// Nudge the selection's font size up one step (per-run `size` mark).
    FontSizeInc,
    /// Apply a per-run text colour (the toolbar swatch row). `None` is the
    /// neutral/default — it clears any `color` run mark over the selection.
    TextColor(Option<[u8; 3]>),
    /// Apply a per-run highlight / background colour (the highlight swatch row).
    /// `None` is the neutral/default — it clears any `highlight` run mark.
    Highlight(Option<[u8; 3]>),
    /// Toggle superscript (`super` run mark) over the selection; clears `sub`.
    Super,
    /// Toggle subscript (`sub` run mark) over the selection; clears `super`.
    Sub,
    // NB: paragraph-level controls (alignment, line spacing) are NOT toolbar
    // actions — they live in the side panel's "This paragraph" section, so the
    // toolbar carries only direct character formatting + structural actions.
    /// Insert an image at the caret via the host file-chooser portal (an OS
    /// picker; drag-and-drop is the secondary insert path).
    InsertImage,
    InsertTable,
    TableRowAdd,
    TableRowDel,
    TableColAdd,
    TableColDel,
    TableDelete,
    ToggleStyles,
    /// Toggle the horizontal ruler band (margins / indents / tab stops).
    ToggleRuler,
    /// Toggle the document outline / navigation pane (left side).
    ToggleOutline,
    /// Toggle the version-history strip under the canvas.
    ToggleHistory,
    /// Generate (or regenerate) a table of contents from the document headings.
    InsertToc,
    /// Open the "New ▾" template picker menu (lists every [`crate::Template`];
    /// a selection replaces the open document with the chosen template in place).
    NewMenu,
}

/// Width of the toolbar's right-aligned zoom stepper cluster `[−] [100%] [+]`
/// (= `bw + gap + val_w + gap + bw` in `draw_zoom_control`). Reserved on the
/// toolbar's last row, right of the colour swatches, so buttons never pack under it.
/// Gap between the zoom stepper cluster and the colour-swatch strips on the
/// toolbar's last row.
const ZOOM_SWATCH_GAP: f64 = 16.0;

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
    /// Style value units per pixel of horizontal drag.
    fn scrub_factor(self) -> f32 {
        match self {
            NumField::Size => 0.1,
            _ => 0.25,
        }
    }
    /// Stepper (+/-) increment.
    fn step(self) -> f32 {
        match self {
            NumField::Size => 1.0,
            _ => 2.0,
        }
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
    /// Apply this style id to the selection / caret block.
    Pick(String),
    /// Set the alignment of the **current paragraph(s)** (an instance `align`
    /// attr — direct formatting, not the style default). The "This paragraph"
    /// section, distinct from the named-style editor below it.
    ParaAlign(&'static str),
    /// Cycle the **current paragraph(s)** line spacing (1.0 → 1.15 → 1.5 → 2.0).
    ParaLineSpacing,
    CycleFont,
    ToggleBold,
    ToggleItalic,
    SetColor([u8; 3]),
    /// Open the shared colour-picker popover to choose an arbitrary colour for
    /// the focused style (beyond the quick-preset swatches).
    OpenColorPicker,
    /// The value box of a numeric field — click to type, drag to scrub.
    Num(NumField),
    /// A +/- stepper on a numeric field.
    NumDelta(NumField, f32),
    /// Duplicate the current style under a fresh id and apply it.
    NewStyle,
    /// Scale the selected image by a factor (a resize stepper: `0.9` shrinks,
    /// `1.1` grows). Keeps the aspect ratio.
    ImageScale(f64),
    /// Mirror the selected image horizontally (left↔right).
    ImageFlipH,
    /// Mirror the selected image vertically (top↔bottom).
    ImageFlipV,
}

/// An in-progress drag-to-scrub on a numeric field.
#[derive(Clone, Copy)]
struct ScrubState {
    field: NumField,
    start_x: f64,
    start_val: f32,
    moved: bool,
}

/// Format a style value: integer when whole, else one decimal.
fn fmt_num(v: f32) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}", num::i64_of_f32(v.round()))
    } else {
        format!("{v:.1}")
    }
}

fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices()
        .nth(char_off)
        .map_or(text.len(), |(b, _)| b)
}
fn byte_to_char(text: &str, byte_off: usize) -> usize {
    let b = byte_off.min(text.len());
    // Iterator-based rather than `text[..b]`: `b` isn't guaranteed to land on
    // a char boundary, and this counts every char that starts before it
    // without ever slicing through one.
    text.char_indices().take_while(|&(i, _)| i < b).count()
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
fn block_presence_key(block_id: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".to_string()),
            PathSegment::Field(block_id.to_string()),
        ],
    }
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

/// Combined width the right-aligned colour + highlight swatch strips occupy on
/// the toolbar's last row. The strips are painted directly (not as chrome
/// `ToolbarItem`s), so the packer can't see them — it reserves this width as a
/// trailing phantom item instead, keeping button rows from extending under the
/// swatches. Mirrors the geometry in `draw_color_swatches` / `draw_highlight_swatches`.
fn swatch_reserve_width() -> f64 {
    let sw = 20.0;
    let gap = 5.0;
    let n_color = num::f64_of_usize(STYLE_PALETTE.len() + 1); // +1 neutral/default swatch
    let n_hl = num::f64_of_usize(HIGHLIGHT_PALETTE.len() + 1); // +1 "no highlight" swatch
    let color = n_color * sw + (n_color - 1.0) * gap;
    let hl = n_hl * sw + (n_hl - 1.0) * gap;
    // right edge margin + colour strip + inter-strip gap + highlight strip
    14.0 + color + 10.0 + hl
}

/// Left padding `chrome::toolbar::draw` applies before the first item.
const TOOLBAR_PAD: f64 = 14.0;
/// Gap `chrome::toolbar::draw` places after each item.
const TOOLBAR_GAP: f64 = 6.0;
/// Extra width a group separator inserts before a `group_start` item.
const TOOLBAR_SEP: f64 = 14.0;
/// Right margin reserved when packing rows (room for the colour-swatch strip /
/// breathing space at the toolbar's right edge).
const TOOLBAR_RIGHT_PAD: f64 = 18.0;
/// Below this width, the bottom chrome shows only document counts. The layout
/// hint stays available through its two explicit chips, but would compete with
/// the status otherwise.
const COMPACT_LAYOUT_BAR_BREAKPOINT: f64 = 760.0;

/// Pack toolbar items into rows that each fit within `avail_w` (the toolbar's
/// usable width). Returns the index ranges `[start, end)` for each row. The
/// width model mirrors `chrome::toolbar::draw` exactly so a row that this packer
/// admits is one that `draw` lays out without clipping. Every row holds at least
/// one item (an over-wide single item simply occupies its own row).
fn pack_toolbar_rows(widths: &[f64], group_starts: &[bool], avail_w: f64) -> Vec<(usize, usize)> {
    let mut rows = Vec::new();
    let usable = (avail_w - TOOLBAR_PAD - TOOLBAR_RIGHT_PAD).max(1.0);
    let mut row_start = 0usize;
    let mut x = 0.0f64; // running width consumed within the current row (post-pad)
    for (i, (&w, &group_start)) in widths.iter().zip(group_starts.iter()).enumerate() {
        let sep = if group_start { TOOLBAR_SEP } else { 0.0 };
        let item_w = sep + w + TOOLBAR_GAP;
        // If this isn't the first item on the row and it would overflow, wrap.
        if i > row_start && x + item_w > usable {
            rows.push((row_start, i));
            row_start = i;
            x = 0.0;
        }
        x += item_w;
    }
    rows.push((row_start, widths.len()));
    rows
}

/// Whether char `i` of a block is covered by a boolean mark named `name`.
fn char_has(i: usize, marks: &[MarkSpan], name: &str) -> bool {
    marks
        .iter()
        .any(|m| m.name == name && i >= m.start && i < m.end)
}

fn parse_family(s: &str) -> FontFamily {
    match s {
        "serif" => FontFamily::Serif,
        "mono" => FontFamily::Mono,
        _ => FontFamily::Sans,
    }
}

/// The font-size run mark covering char `i`, if any. The editor writes the
/// `size` Int mark (the codec contract's character format); `fontSize` is the
/// legacy name some imported (ODT/DOCX) documents still carry, so both resolve.
fn char_font_size(i: usize, marks: &[MarkSpan]) -> Option<f32> {
    marks
        .iter()
        .find(|m| (m.name == "size" || m.name == "fontSize") && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_int())
        .map(num::f32_of_i64)
}

/// The font-family run mark covering char `i`, if any. The editor writes the
/// `font` Str mark; `fontFamily` is the legacy imported-document name — both
/// resolve so codec-imported runs keep their family.
fn char_font_family(i: usize, marks: &[MarkSpan]) -> Option<FontFamily> {
    marks
        .iter()
        .find(|m| (m.name == "font" || m.name == "fontFamily") && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .map(parse_family)
}

/// Link blue used for `link`-marked text.
const LINK_RGB: [u8; 3] = [0x1a, 0x56, 0xc4];

/// The text colour covering char `i`: a `link` mark → link blue, else a
/// `color` mark's `#RRGGBB`.
fn char_color(i: usize, marks: &[MarkSpan]) -> Option<[u8; 3]> {
    if marks
        .iter()
        .any(|m| m.name == "link" && i >= m.start && i < m.end)
    {
        return Some(LINK_RGB);
    }
    marks
        .iter()
        .find(|m| m.name == "color" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .and_then(parse_hex_rgb)
}

/// The highlight (background) colour covering char `i`: a `highlight` mark's
/// `#RRGGBB`, if any. Maps to [`RichSpan::background`] (a band behind the run).
fn char_highlight(i: usize, marks: &[MarkSpan]) -> Option<[u8; 3]> {
    marks
        .iter()
        .find(|m| m.name == "highlight" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .and_then(parse_hex_rgb)
}

/// The baseline shift covering char `i`: `super`/`sub` boolean run marks map to
/// [`ui::richtext::BaselineShift`] (`super` wins if both are somehow present).
fn char_baseline(i: usize, marks: &[MarkSpan]) -> ui::richtext::BaselineShift {
    if char_has(i, marks, "super") {
        ui::richtext::BaselineShift::Super
    } else if char_has(i, marks, "sub") {
        ui::richtext::BaselineShift::Sub
    } else {
        ui::richtext::BaselineShift::None
    }
}

fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    // `h.len() == 6` alone doesn't rule out multi-byte UTF-8 (a 3-byte char
    // plus 3 ASCII bytes also has `.len() == 6`); `is_ascii` above guarantees
    // every byte offset below is a char boundary, so `.get` never misses.
    Some([
        u8::from_str_radix(h.get(0..2)?, 16).ok()?,
        u8::from_str_radix(h.get(2..4)?, 16).ok()?,
        u8::from_str_radix(h.get(4..6)?, 16).ok()?,
    ])
}

/// Whether a block forces a page break before it (the `page_break_before`
/// block attr). Honoured by the pagination cursor so the block starts a fresh
/// page even when it would otherwise fit on the current one.
fn block_page_break_before(b: &BlockView) -> bool {
    matches!(b.attrs.get("page_break_before"), Some(AttrVal::Bool(true)))
}

/// A block's line-spacing multiplier (the `line_spacing` Float attr), defaulting
/// to `1.0` when unset. Fed to `RichTextLayout::shape` as `line_height_mult`,
/// scaling the line advance (1.5 = one-and-a-half, 2.0 = double).
fn block_line_spacing(b: &BlockView) -> f32 {
    match b.attrs.get("line_spacing") {
        Some(AttrVal::Float(f)) if *f > 0.0 => num::f32_of_f64(*f),
        Some(AttrVal::Int(n)) if *n > 0 => num::f32_of_i64(*n),
        _ => 1.0,
    }
}

/// The cycle of presented line-spacing multipliers.
const LINE_SPACINGS: [f64; 4] = [1.0, 1.15, 1.5, 2.0];

/// Next multiplier in the line-spacing cycle (wraps 2.0 → 1.0). The nearest
/// preset to `cur` is the anchor, so an imported odd value still cycles cleanly.
/// `cur` may be a non-finite value from a malformed import or a remote peer's
/// action; `partial_cmp` returning `None` for a NaN distance falls back to
/// `Equal` rather than panicking, so the cycle just anchors on the first preset.
fn next_line_spacing(cur: f64) -> f64 {
    let idx = LINE_SPACINGS
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (**a - cur)
                .abs()
                .partial_cmp(&(**b - cur).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(0, |(i, _)| i);
    LINE_SPACINGS
        .get((idx + 1) % LINE_SPACINGS.len())
        .copied()
        .unwrap_or(1.0)
}

/// A short label for a line-spacing multiplier (the toolbar chip text).
fn line_spacing_label(mult: f64) -> String {
    if (mult - mult.round()).abs() < 0.005 {
        format!("⇕ {}", num::i64_of_f64(mult.round()))
    } else {
        format!("⇕ {mult:.2}")
    }
}

/// A block's own left/hanging indent (px) from its `indent` attr — added on top
/// of the style indent by the layout pass. `0.0` when unset.
fn block_indent_attr(b: &BlockView) -> f64 {
    match b.attrs.get("indent") {
        Some(AttrVal::Float(f)) => *f,
        Some(AttrVal::Int(n)) => num::f64_of_i64(*n),
        _ => 0.0,
    }
}

/// An image block's `anchor` attr (`inline` | `paragraph` | `page`), defaulting
/// to `inline`. `inline` flows the image in its own full-width vertical slot;
/// `paragraph`/`page` let it float so following text wraps beside it.
fn image_anchor(b: &BlockView) -> &str {
    match b.attrs.get("anchor") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "inline",
    }
}

/// An image block's `wrap` attr (`none` | `left` | `right` | `both`), defaulting
/// to `none`. Only meaningful for a floated (paragraph/page-anchored) image:
/// `left` floats the image to the left edge with text beside it on the right (and
/// vice-versa for `right`); `both`/`none` keep the image in a centered full-width
/// slot.
fn image_wrap(b: &BlockView) -> &str {
    match b.attrs.get("wrap") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "none",
    }
}

/// An image block's explicit display size `(width, height)` in px from its
/// `width`/`height` attrs (set by the ODF/OOXML importers and the model on
/// insert). `None` when either dimension is missing, so callers fall back to the
/// decoded intrinsic size. Accepts `Int` or `Float`.
fn image_explicit_size(b: &BlockView) -> Option<(f64, f64)> {
    let dim = |key: &str| match b.attrs.get(key) {
        Some(AttrVal::Int(n)) if *n > 0 => Some(num::f64_of_i64(*n)),
        Some(AttrVal::Float(f)) if *f > 0.0 => Some(*f),
        _ => None,
    };
    Some((dim("width")?, dim("height")?))
}

/// True when an image block is floated (anchor paragraph/page) with a one-sided
/// wrap (`left`/`right`) — the case where following paragraphs are narrowed and
/// placed beside the image rather than below it.
fn image_floats(b: &BlockView) -> bool {
    let anchored = matches!(image_anchor(b), "paragraph" | "page");
    anchored && matches!(image_wrap(b), "left" | "right")
}

/// Pick a block's horizontal alignment from its `align` attribute.
fn block_align(b: &BlockView, default: TextAlign) -> TextAlign {
    match b.attrs.get("align") {
        Some(AttrVal::Str(s)) if s == "center" => TextAlign::Center,
        Some(AttrVal::Str(s)) if s == "right" => TextAlign::Right,
        Some(AttrVal::Str(s)) if s == "left" => TextAlign::Left,
        _ => default,
    }
}

/// Map a style's font / align strings to the ui enums.
fn parse_font(name: &str) -> FontFamily {
    match name {
        "serif" => FontFamily::Serif,
        "mono" => FontFamily::Mono,
        _ => FontFamily::Sans,
    }
}
fn style_align(s: &str) -> TextAlign {
    match s {
        "center" => TextAlign::Center,
        "right" => TextAlign::Right,
        _ => TextAlign::Left,
    }
}

/// Resolve the style id for a block from its kind / level / explicit `style` attr.
fn block_style_key(b: &BlockView) -> String {
    let level = match b.attrs.get("level") {
        Some(AttrVal::Int(n)) => Some(*n),
        _ => None,
    };
    let explicit = match b.attrs.get("style") {
        Some(AttrVal::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    crate::styles::style_key_for(&b.kind, level, explicit)
}

/// A block's styled runs plus its underline and strikethrough byte ranges:
/// `(runs, underline_ranges, strike_ranges)`.
type BlockRuns = (Vec<RichSpan>, Vec<(usize, usize)>, Vec<(usize, usize)>);

/// A measured table row within [`WordProcessorWidget::paginate_blocks`]'s
/// table-layout branch: `(row_height, cells)`, each cell as `(col,
/// cell_height, block)`.
type TableRow<'a> = (f64, Vec<(usize, f64, &'a BlockView)>);

/// Split a block's text into styled runs (bold/italic/size/family) + underline
/// and strikethrough byte ranges.
fn block_runs(block: &BlockView) -> BlockRuns {
    let chars: Vec<char> = block.text.chars().collect();
    let len = chars.len();
    // Run key: f32 size as bits so the tuple is comparable. Highlight (background)
    // and super/subscript baseline join the key so a change in either splits runs.
    let key = |i: usize| {
        (
            char_has(i, &block.marks, "strong"),
            char_has(i, &block.marks, "em"),
            char_font_size(i, &block.marks).map(f32::to_bits),
            char_font_family(i, &block.marks),
            char_color(i, &block.marks),
            char_highlight(i, &block.marks),
            char_baseline(i, &block.marks),
        )
    };
    let mut runs = Vec::new();
    let mut i = 0;
    while i < len {
        let k = key(i);
        let mut j = i;
        while j < len && key(j) == k {
            j += 1;
        }
        let (bold, italic, size_bits, family, color, background, baseline) = k;
        runs.push(RichSpan {
            text: chars.get(i..j).unwrap_or(&[]).iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background,
            baseline,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let deco = |name: &str| {
        block
            .marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| {
                (
                    char_to_byte(&block.text, m.start),
                    char_to_byte(&block.text, m.end),
                )
            })
            .collect::<Vec<_>>()
    };
    // Links render underlined as well as blue.
    let mut underlines = deco("underline");
    underlines.extend(deco("link"));
    (runs, underlines, deco("strike"))
}

/// Whether char `i` of a block is covered by a mark named `name` (an `ins`/`del`
/// tracked-change mark, or a `comment` annotation mark).
fn char_in_mark(i: usize, marks: &[MarkSpan], name: &str) -> bool {
    marks
        .iter()
        .any(|m| m.name == name && i >= m.start && i < m.end)
}

/// Split a block's text into styled runs like [`block_runs`], additionally
/// tinting `ins`/`del` tracked-change runs in the review colour (green/red).
/// Underline/strike decorations are drawn separately in the body pass. Identical
/// to [`block_runs`] when `show_markup` is false.
fn block_runs_markup(block: &BlockView, show_markup: bool) -> BlockRuns {
    if !show_markup {
        return block_runs(block);
    }
    let chars: Vec<char> = block.text.chars().collect();
    let len = chars.len();
    // An `ins` run is green, a `del` run red.
    let track_color = |i: usize| -> Option<[u8; 3]> {
        if char_in_mark(i, &block.marks, qdoc::schema::MARK_INS) {
            Some(INSERTION_RGB)
        } else if char_in_mark(i, &block.marks, qdoc::schema::MARK_DEL) {
            Some(DELETION_RGB)
        } else {
            None
        }
    };
    // The run key now also folds in the tracked-change review colour, which
    // overrides any explicit `color` mark over the same range (review colour
    // wins for legibility).
    let key = |i: usize| {
        (
            char_has(i, &block.marks, "strong"),
            char_has(i, &block.marks, "em"),
            char_font_size(i, &block.marks).map(f32::to_bits),
            char_font_family(i, &block.marks),
            track_color(i).or_else(|| char_color(i, &block.marks)),
            char_highlight(i, &block.marks),
            char_baseline(i, &block.marks),
        )
    };
    let mut runs = Vec::new();
    let mut i = 0;
    while i < len {
        let k = key(i);
        let mut j = i;
        while j < len && key(j) == k {
            j += 1;
        }
        let (bold, italic, size_bits, family, color, background, baseline) = k;
        runs.push(RichSpan {
            text: chars.get(i..j).unwrap_or(&[]).iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background,
            baseline,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let deco = |name: &str| {
        block
            .marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| {
                (
                    char_to_byte(&block.text, m.start),
                    char_to_byte(&block.text, m.end),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut underlines = deco("underline");
    underlines.extend(deco("link"));
    let strikes = deco("strike");
    // NOTE: the `ins`/`del` review decorations (underline for ins, strike for
    // del) are *not* added here — they are drawn in the body pass in the review
    // colour (`draw_rich` paints these decorations in the block ink, which would
    // mismatch the coloured glyphs). Keeping them out of the shaped layout also
    // keeps `block_runs`'s underline/strike contract unchanged.
    (runs, underlines, strikes)
}

/// The `(ins_ranges, del_ranges)` byte ranges in a block covered by an `ins`
/// tracked-insertion mark and a `del` tracked-deletion mark respectively.
type ChangeDecoRanges = (Vec<(usize, usize)>, Vec<(usize, usize)>);

/// `prepare_pass2_inputs`'s `(multi_sel, note_nums, spell_by_block)` result:
/// whether the selection spans multiple cells, footnote citation numbers
/// keyed by block id, and (when spell-check is on) misspelled byte spans
/// keyed by block id.
type Pass2Inputs = (
    bool,
    HashMap<String, usize>,
    HashMap<String, Vec<(usize, usize)>>,
);

/// The `(byte_start, byte_end)` ranges in a block covered by an `ins` tracked-
/// insertion mark and a `del` tracked-deletion mark respectively. Used by the
/// body draw pass to paint the review underline (ins) / strike (del).
fn block_change_decos(b: &BlockView) -> ChangeDecoRanges {
    let collect = |name: &str| {
        b.marks
            .iter()
            .filter(|m| m.name == name && m.end > m.start)
            .map(|m| (char_to_byte(&b.text, m.start), char_to_byte(&b.text, m.end)))
            .collect::<Vec<_>>()
    };
    (
        collect(qdoc::schema::MARK_INS),
        collect(qdoc::schema::MARK_DEL),
    )
}

/// Every tracked change anchored in a block, as `(byte_start, byte_end,
/// change_id)` in source order — for both `ins` and `del` marks. The change id
/// comes from the mark value (the key the `changes` container uses). Drives the
/// right-margin Accept/Reject review cards (which need the id to act on).
fn block_change_ranges(b: &BlockView) -> Vec<(usize, usize, String)> {
    b.marks
        .iter()
        .filter(|m| {
            (m.name == qdoc::schema::MARK_INS || m.name == qdoc::schema::MARK_DEL)
                && m.end > m.start
        })
        .filter_map(|m| {
            let id = m.value.as_str()?.to_string();
            Some((
                char_to_byte(&b.text, m.start),
                char_to_byte(&b.text, m.end),
                id,
            ))
        })
        .collect()
}

/// Every `comment` annotation anchored in a block, as `(byte_start, byte_end,
/// comment_id)` in source order. The body pass paints a pale highlight band over
/// the range and a margin indicator at the start.
fn block_comment_ranges(b: &BlockView) -> Vec<(usize, usize, String)> {
    b.marks
        .iter()
        .filter(|m| m.name == qdoc::schema::MARK_COMMENT && m.end > m.start)
        .filter_map(|m| {
            let id = m.value.as_str()?.to_string();
            Some((
                char_to_byte(&b.text, m.start),
                char_to_byte(&b.text, m.end),
                id,
            ))
        })
        .collect()
}

/// Whether char `i` of `b` is a `page-number` field placeholder (a U+FFFC
/// covered by a `field` mark valued `page-number`).
fn char_is_page_number_field(i: usize, marks: &[MarkSpan]) -> bool {
    marks.iter().any(|m| {
        m.name == qdoc::schema::MARK_FIELD
            && i >= m.start
            && i < m.end
            && m.value.as_str() == Some(qdoc::schema::FIELD_PAGE_NUMBER)
    })
}

/// Build the `RichSpan`s for a header/footer block with every `page-number`
/// field placeholder substituted by `page_no` (the digits inherit the field
/// char's run formatting). Underline/strike are dropped — running heads keep it
/// simple — but bold/italic/size/family/colour per-run are preserved.
fn running_head_runs(b: &BlockView, page_no: usize) -> Vec<RichSpan> {
    let chars: Vec<char> = b.text.chars().collect();
    let n = chars.len();
    let key = |i: usize| {
        (
            char_has(i, &b.marks, "strong"),
            char_has(i, &b.marks, "em"),
            char_font_size(i, &b.marks).map(f32::to_bits),
            char_font_family(i, &b.marks),
            char_color(i, &b.marks),
        )
    };
    // Coalesce runs by (formatting key, is-field): a field char always sits in
    // its own run so the page number inherits exactly that char's formatting and
    // never merges with adjacent literal text.
    let run_key = |i: usize| (key(i), char_is_page_number_field(i, &b.marks));
    let mut runs: Vec<RichSpan> = Vec::new();
    let mut i = 0;
    while i < n {
        let rk = run_key(i);
        let mut j = i;
        while j < n && run_key(j) == rk {
            j += 1;
        }
        let ((bold, italic, size_bits, family, color), is_field) = rk;
        let text: String = if is_field {
            // One run per field char (each is its own placeholder); emit the page
            // number once per placeholder char so a literal repeat still works.
            page_no.to_string().repeat(j - i)
        } else {
            chars.get(i..j).unwrap_or(&[]).iter().collect()
        };
        runs.push(RichSpan {
            text,
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background: None,
            baseline: ui::richtext::BaselineShift::None,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    runs
}

/// Like [`running_head_runs`] but for the band being *edited*: every char maps
/// 1:1 to a caret offset; a `page-number` placeholder shows as `#`, not digits.
fn running_head_runs_editing(b: &BlockView) -> Vec<RichSpan> {
    let chars: Vec<char> = b.text.chars().collect();
    let n = chars.len();
    let key = |i: usize| {
        (
            char_has(i, &b.marks, "strong"),
            char_has(i, &b.marks, "em"),
            char_font_size(i, &b.marks).map(f32::to_bits),
            char_font_family(i, &b.marks),
            char_color(i, &b.marks),
        )
    };
    let run_key = |i: usize| (key(i), char_is_page_number_field(i, &b.marks));
    let mut runs: Vec<RichSpan> = Vec::new();
    let mut i = 0;
    while i < n {
        let rk = run_key(i);
        let mut j = i;
        while j < n && run_key(j) == rk {
            j += 1;
        }
        let ((bold, italic, size_bits, family, color), is_field) = rk;
        // One glyph per char so offsets stay aligned; a field char shows as `#`.
        let text: String = if is_field {
            "#".repeat(j - i)
        } else {
            chars.get(i..j).unwrap_or(&[]).iter().collect()
        };
        runs.push(RichSpan {
            text,
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background: None,
            baseline: ui::richtext::BaselineShift::None,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    runs
}

/// The note ids referenced by char `i` of `b` via a `note-ref` mark (covering
/// exactly the placeholder char), in source order. Usually 0 or 1 per char.
fn char_note_ref(i: usize, marks: &[MarkSpan]) -> Option<&str> {
    marks
        .iter()
        .find(|m| m.name == qdoc::schema::MARK_NOTE_REF && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
}

/// Every `note-ref` marker in a block, as `(char_index, note_id)` in source
/// order. The char index is the placeholder char (U+FFFC) the marker covers.
fn block_note_refs(b: &BlockView) -> Vec<(usize, String)> {
    let n = b.text.chars().count();
    (0..n)
        .filter_map(|i| char_note_ref(i, &b.marks).map(|id| (i, id.to_string())))
        .collect()
}

/// Assign citation numbers (1, 2, 3, …) to notes in *document order* (first
/// `note-ref` appearance), the authoritative order for both in-text + page list.
fn note_numbering(blocks: &[BlockView]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    let mut next = 1usize;
    for b in blocks {
        for (_, id) in block_note_refs(b) {
            map.entry(id).or_insert_with(|| {
                let n = next;
                next += 1;
                n
            });
        }
    }
    map
}

/// Concatenate a note's body blocks into one display string (paragraphs joined by
/// a space). Footnotes are usually a single short paragraph.
fn note_text(note: &NoteView) -> String {
    let mut parts: Vec<&str> = note
        .blocks
        .iter()
        .map(|b| b.text.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    if parts.is_empty() {
        parts.push("");
    }
    parts.join(" ")
}

/// Draw a wavy red spell-check underline from `x0` to `x1` at baseline `y`, in the
/// given (already zoom-adjusted) transform. Approximated by short line segments
/// tracing a sine so it reads as a squiggle without per-glyph curve math.
#[allow(
    clippy::while_float,
    reason = "x advances by the fixed positive SQUIGGLE_PERIOD/2.0 each turn \
              (clamped to x1), so this is bounded by (x1-x0)/step, not float \
              imprecision"
)]
fn draw_squiggle(scene: &mut Scene, transform: Affine, x0: f64, x1: f64, y: f64) {
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
        SQUIGGLE,
        None,
        &path,
    );
}

/// Run the spell-checker over a block's text, returning the misspelled byte spans
/// `(start, end)`. Skips the U+FFFC field/note placeholders (they are not words).
///
/// Spans only. This is called while drawing the paragraph the caret is in, which
/// means on every keystroke, and a correction nobody has asked for is not worth
/// searching the dictionary for. The right-click menu asks for one when it has
/// a word to ask about — see `suggestions_for_word`.
fn spell_spans(dict: &spellcheck::Dictionary, language: &str, text: &str) -> Vec<(usize, usize)> {
    let mut spans = spellcheck::spans_for_language(language, text);
    spans.retain(|&(start, end)| {
        text.get(start..end)
            .is_some_and(|word| !dict.contains(word))
    });
    spans
}

/// Hash everything that feeds into `RichTextLayout::shape` into a single `u64`.
///
/// Cache key for the per-block incremental layout cache: matching keys across
/// frames mean `shape` would reproduce a byte-identical layout, so the previous
/// `RichTextLayout` is reused verbatim. Inputs mirror `shape`'s arguments exactly,
/// including **wrap width** (so a resize re-shapes); block-level ink is excluded
/// since it applies at draw time (`Laid::ink`), not baked into the shaped buffer.
#[allow(clippy::too_many_arguments)] // mirrors `RichTextLayout::shape`'s own argument list
fn shape_key(
    spans: &[RichSpan],
    underlines: &[(usize, usize)],
    strikes: &[(usize, usize)],
    base_bold: bool,
    size: f32,
    wrap_w: f32,
    align: TextAlign,
    family: FontFamily,
    line_height_mult: f32,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spans.len().hash(&mut h);
    for s in spans {
        s.text.hash(&mut h);
        s.bold.hash(&mut h);
        s.italic.hash(&mut h);
        s.size.map(f32::to_bits).hash(&mut h);
        s.family.map(|f| f as u8).hash(&mut h);
        s.color.hash(&mut h);
        // Highlight band + baseline shift change the shaped layout (the latter
        // shapes super/subscript runs at a reduced size), so fold them in.
        s.background.hash(&mut h);
        (s.baseline as u8).hash(&mut h);
    }
    underlines.hash(&mut h);
    strikes.hash(&mut h);
    base_bold.hash(&mut h);
    size.to_bits().hash(&mut h);
    wrap_w.to_bits().hash(&mut h);
    (align as u8).hash(&mut h);
    (family as u8).hash(&mut h);
    line_height_mult.to_bits().hash(&mut h);
    h.finish()
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
    text: String,
    /// Top-left origin of the run in content space.
    origin: Point,
    layout: RichTextLayout,
}

/// A list item's 0-based nesting level (absent attr → 0).
fn list_level(b: &BlockView) -> usize {
    match b.attrs.get("level") {
        Some(AttrVal::Int(n)) if *n > 0 => num::usize_of_i64(*n),
        _ => 0,
    }
}

/// Per-nesting-level extra indent (px) added on top of the list style's indent.
const LIST_INDENT_STEP: f64 = 24.0;
/// Horizontal offset from the text's left edge to the bullet / number glyph.
const BULLET_GAP: f64 = 20.0;
/// US-Letter page height in inches (width is 8.5in → `MAX_PAGE_W` px).
const LETTER_RATIO: f64 = 11.0 / 8.5;
/// Gap between a floated image's edge and the text column that wraps beside
/// it, in [`WordProcessorWidget::paginate_blocks`]'s float-reserve math.
const FLOAT_GUTTER: f64 = 12.0;

/// The bullet glyph for an unordered item at `level` (cycles by depth).
fn bullet_glyph(level: usize) -> &'static str {
    match level % 3 {
        0 => "•",
        1 => "◦",
        _ => "▪",
    }
}

fn cell_table(b: &BlockView) -> Option<String> {
    match b.attrs.get("table") {
        Some(AttrVal::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn cell_rc(b: &BlockView) -> (usize, usize) {
    let get = |k: &str| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) if *n >= 0 => num::usize_of_i64(*n),
        _ => 0,
    };
    (get("row"), get("col"))
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag on the top-level editor \
              state (focus/dirty/undo-commit bookkeeping, per-panel visibility, \
              spell-check/markup view toggles, --shots staging) — not a set of \
              options that combine, so a sub-struct or bitflags wouldn't read any \
              better; matches the same call made for MailApp/TubeApp/CodeApp"
)]
pub struct WordProcessorWidget {
    inner: WordProcessor,
    rect: Rect,
    focused: bool,
    dirty: bool,
    needs_commit: bool,
    /// Framework-owned action dispatch: brackets the undo boundary and dispatches
    /// each action through the executor (so a test can prove parity with Ctrl+K).
    actions: ui::ActionHost,
    pointer: Point,
    drag: DragSelect,
    path: Option<PathBuf>,
    format: Option<WpFormat>,
    /// Unsaved changes since the last save (drives the status-bar `●` indicator).
    modified: bool,
    /// Find & Replace owns its query modes, match cursor, drawing, and hit map.
    find: FindBar,
    /// Image blobs (blake3 hash → bytes), refreshed from the document, plus a
    /// lazily-decoded image cache (`None` = decode failed / unsupported format).
    blobs: std::collections::BTreeMap<String, Vec<u8>>,
    image_cache: std::collections::HashMap<String, Option<ui::widget::Image>>,
    /// The toolbar button the pointer is currently over (for hover feedback).
    hover_tool: Option<Rect>,
    blocks: Vec<BlockView>,
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
    /// Diagnostics for the layout cache, recomputed each `draw`: number of
    /// blocks whose layout was reused (`hits`) vs re-shaped (`misses`).
    dbg_layout_hits: usize,
    dbg_layout_misses: usize,
    toolbar: Vec<(ToolAction, Rect)>,
    /// Last rich copy: `(plain_text, blocks)`. Paste uses it when the system
    /// clipboard still holds this exact plain text.
    rich_clip: Option<(String, Vec<ClipBlock>)>,
    /// Vertical scroll offset (px) and the max scroll (content overflow), updated
    /// each frame from the laid-out content height.
    scroll_y: f64,
    max_scroll: f64,
    /// The document's stylesheet (component-instance styles). Authoritative copy
    /// in the widget; reloaded from the doc when not mid-drag, persisted on edit.
    sheet: crate::styles::StyleSheet,
    /// Page margins — fully customizable by dragging guides on the page.
    margins: Edges,
    layout_mode: LayoutMode,
    /// Active guide drag (the side being dragged), if any.
    guide_drag: Option<GuideDrag>,
    /// Whether the horizontal ruler band is shown (toggled via the toolbar
    /// "Ruler" button + the View bottom-bar chip). Default ON — it is the primary
    /// surface for margin / indent / tab-stop editing.
    ruler_on: bool,
    /// Horizontal ruler component: painting, geometry and marker drag lifecycle.
    ruler: Ruler,
    /// The page rect in screen space (post-scroll), cached each frame so the
    /// event handler can hit-test layout guides against it.
    page_rect: Rect,
    /// The bottom layout bar's clickable chips: `(mode, rect)`.
    layout_chips: Vec<(LayoutMode, Rect)>,
    /// Whether the contextual styles panel is shown (dismissable via the toolbar
    /// "Styles" toggle).
    styles_panel_open: bool,
    /// The styles panel rect in screen space, cached each frame for hit-testing.
    panel_rect: Rect,
    /// Clickable targets inside the styles panel, rebuilt each draw.
    panel_hits: Vec<(Rect, PanelHit)>,
    /// The numeric field currently being typed into (click-to-type), if any.
    editing_field: Option<NumField>,
    /// Text buffer for the field being typed into.
    edit_buf: String,
    /// An active drag-to-scrub on a numeric field, if any.
    scrub: Option<ScrubState>,
    /// Local page scale plus its toolbar drawing and hit-testing.
    zoom: ZoomControl,
    /// Follow the available canvas width until the user explicitly operates a
    /// zoom control. This makes a narrow/tiled editor readable on first open.
    auto_fit_page: bool,
    /// Number of toolbar rows the controls wrapped into this frame (>=1). Drives
    /// the canvas top offset so no control is ever clipped.
    toolbar_rows: usize,
    /// Footnotes / endnotes (materialized from the doc), sorted by note id; the
    /// in-text numbering is derived from `note-ref` document order, not this order.
    notes: Vec<NoteView>,
    /// The English dictionary, loaded lazily on first spell-check pass (it is
    /// ~75k words, so building it eagerly would slow every cold start). Cloned
    /// once from the process-wide shared built-in list (parsed at most once per
    /// process), then **owned** by this widget so "Add to dictionary" can splice
    /// user words into it via [`spellcheck::Dictionary::add_word`]. Persisted
    /// user words are layered in via `with_user_dict` on first build.
    dict: Option<spellcheck::Dictionary>,
    /// Whether spell-checking (the red squiggles) is enabled. Default ON.
    spellcheck_on: bool,
    /// Document/session dictionary selected for multilingual checking.
    spell_language: String,
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
    /// Comment/tracked-change visibility, materialized items, card drawing, and
    /// frame-local action hit targets. Document mutation remains editor policy.
    review: ReviewOverlay,
    /// Document-outline visibility, drawing, and frame-local hit geometry.
    outline: OutlinePane,
    /// Version-history strip under the canvas: the document's own change graph
    /// rendered as a scrubber. Toggled from the toolbar's "History" button.
    history: crate::history_panel::HistoryPanel,
    /// OS-theme-driven chrome palette, resolved at the top of every `draw` from
    /// the passed-in [`Theme`]. Every chrome helper reads this instead of
    /// hard-coded colours, so the editor honours light/dark mode + the OS
    /// palette. Initialised to the dark theme's mapping for off-frame use.
    chrome: ChromeColors,
    /// Crash-recovery persistence and its Restore/Discard surface.
    recovery: RecoveryBanner,
    /// The "New ▾" picker owns its shared menu, lifecycle, and typed outcomes.
    template_picker: TemplatePicker,
    /// An in-flight host file request: `Some` means the XDG chooser is open on a
    /// worker thread; the path is drained each frame in [`Self::drain_portal`].
    /// [`WordProcessorWidget::drain_portal`]. `None` = idle.
    portal_rx: Option<portal::PortalChannel>,
    /// What the in-flight [`Self::portal_rx`] request is for (open a document,
    /// save the current one, or insert an image). Set alongside `portal_rx`.
    portal_purpose: Option<PortalPurpose>,
    resource_file_invocation: Option<qos_actions::InvocationId>,
    resource_file_result: Option<qos_actions::InvocationUpdate>,
    pending_resource_open: Option<(qos_actions::InvocationId, std::path::PathBuf)>,
    // ---- paragraph-style context menu -------------------------------------
    /// Right-click style-menu lifecycle, drawing, hit-testing, and typed outcome.
    style_menu: StyleMenu,
    // ---- transient status toast -------------------------------------------
    /// A transient operation result (e.g. save confirmation or failure),
    /// drawn as a toast in the overlay pass. Dismissed on the next interaction.
    toast: Option<(String, ToastKind)>,
    /// The style inspector's shared color-picker popover. It owns its lifecycle,
    /// layout, drawing, and modal input routing; this editor applies its result.
    color_picker: StyleColorPicker,
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
    pub fn new() -> Self {
        Self {
            inner: WordProcessor::new(),
            rect: Rect::ZERO,
            focused: true,
            dirty: true,
            needs_commit: false,
            actions: ui::ActionHost::new(),
            pointer: Point::ZERO,
            drag: DragSelect::new(),
            path: None,
            format: None,
            modified: false,
            find: FindBar::default(),
            blobs: std::collections::BTreeMap::new(),
            image_cache: std::collections::HashMap::new(),
            hover_tool: None,
            blocks: Vec::new(),
            header: Vec::new(),
            footer: Vec::new(),
            laid: Vec::new(),
            region_laid: Vec::new(),
            head_band_rect: Rect::ZERO,
            foot_band_rect: Rect::ZERO,
            layout_cache: HashMap::new(),
            dbg_layout_hits: 0,
            dbg_layout_misses: 0,
            toolbar: Vec::new(),
            rich_clip: None,
            scroll_y: 0.0,
            max_scroll: 0.0,
            sheet: crate::styles::StyleSheet::default(),
            margins: Edges::uniform(72.0),
            layout_mode: LayoutMode::None,
            guide_drag: None,
            ruler_on: true,
            ruler: Ruler::default(),
            page_rect: Rect::ZERO,
            layout_chips: Vec::new(),
            // Keep the writing surface primary on first launch. The complete
            // style inspector is one click away and becomes an overlay on a
            // narrow tile rather than permanently consuming half the canvas.
            styles_panel_open: false,
            panel_rect: Rect::ZERO,
            panel_hits: Vec::new(),
            editing_field: None,
            edit_buf: String::new(),
            scrub: None,
            zoom: ZoomControl::new(),
            auto_fit_page: true,
            toolbar_rows: 1,
            notes: Vec::new(),
            dict: None,
            spellcheck_on: true,
            spell_language: spellcheck::default_language(),
            last_autocorrection: None,
            spell_cache: HashMap::new(),
            context_menu: ContextMenu::new(),
            context_actions: Vec::new(),
            review: ReviewOverlay::default(),
            outline: OutlinePane::default(),
            history: crate::history_panel::HistoryPanel::new(),
            // Overwritten at the top of every `draw` from the live theme.
            chrome: ChromeColors::for_theme(&Theme::dark()),
            recovery: RecoveryBanner::default(),
            template_picker: TemplatePicker::default(),
            portal_rx: None,
            portal_purpose: None,
            resource_file_invocation: None,
            resource_file_result: None,
            pending_resource_open: None,
            style_menu: StyleMenu::default(),
            toast: None,
            color_picker: StyleColorPicker::default(),
        }
    }

    // ---- qOS action dispatch ------------------------------------------------

    /// Dispatch one [`WordProcessorAction`](crate::actions::WordProcessorAction) —
    /// the single CRDT-mutation path shared with the Ctrl+K palette — through the
    /// qOS action executor. Document heads before the event's first action are
    /// stashed so the host records **one** undo boundary bracketing the whole event.
    fn perform_action(
        &mut self,
        state: &mut AutoCommit,
        action: &crate::actions::WordProcessorAction,
    ) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::WORD_PROCESSOR_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Writer", "Writer", action),
        );
    }

    /// The context Ctrl+K needs to build the document's resource tree: the caret
    /// anchor for structural inserts and the blocks a block-level format targets.
    /// `None` until the document is bound (a fresh, unsynced widget).
    pub(crate) fn wp_resource_ctx(&self) -> Option<crate::actions::WpResourceCtx> {
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
            region: self.inner.region_kind(),
        })
    }

    /// Test hook: the last action an affordance dispatched through
    /// [`Self::perform_action`], so a test can compare it to a palette spec.
    #[cfg(test)]
    pub(crate) fn last_dispatched_for_test(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    // ---- formatting affordances → actions -----------------------------------

    /// Toggle an inline mark (bold/italic/underline/strike) over the selection,
    /// as one replayable action. A toggle with no selection is a no-op.
    fn dispatch_toggle_mark(&mut self, state: &mut AutoCommit, name: &str) {
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
    fn dispatch_exclusive_mark(&mut self, state: &mut AutoCommit, name: &str, sibling: &str) {
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
        state: &mut AutoCommit,
        name: &str,
        value: crate::actions::MarkVal,
    ) {
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
    fn dispatch_clear_mark(&mut self, state: &mut AutoCommit, name: &str) {
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
        state: &mut AutoCommit,
        kind: &str,
        level: Option<i64>,
        ordered: Option<bool>,
    ) {
        let blocks = self.inner.paragraph_style_targets(state);
        if blocks.is_empty() {
            return;
        }
        let action = crate::actions::set_paragraph_style_action(blocks, kind, level, ordered);
        self.perform_action(state, &action);
    }

    /// Apply a named paragraph style (a style-picker pick) over the current
    /// paragraph targets as one replayable action: the style attr plus, for a
    /// built-in, the structural kind / level / ordered.
    fn dispatch_apply_named_style(&mut self, state: &mut AutoCommit, id: &str) {
        let blocks = self.inner.paragraph_style_targets(state);
        if blocks.is_empty() {
            return;
        }
        let (kind, level, ordered) = match crate::styles::structural_kind_for(id) {
            Some((k, l, o)) => (Some(k), l, o),
            None => (None, None, None),
        };
        let action = crate::actions::apply_named_style_action(blocks, id, kind, level, ordered);
        self.perform_action(state, &action);
    }

    /// Set a block attribute (alignment / line-spacing / style) over the current
    /// target blocks, as one replayable action.
    fn dispatch_block_attr(
        &mut self,
        state: &mut AutoCommit,
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
    fn insert_table_via_action(&mut self, state: &mut AutoCommit, rows: usize, cols: usize) {
        let after = self.inner.caret().block.clone();
        let before: std::collections::HashSet<String> = self.body_cell_ids(state);
        self.perform_action(
            state,
            &crate::actions::insert_table_action(&after, rows, cols),
        );
        // The first cell of the freshly-created table = the new `cell` block at
        // (row 0, col 0). Seat the caret there so typing continues in the table.
        if let Some(c) = self.inner.containers.clone() {
            let snap = qdoc::model::materialize(state, &c);
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

    /// The ids of every `cell` block currently in the body.
    fn body_cell_ids(&self, state: &AutoCommit) -> std::collections::HashSet<String> {
        match self.inner.containers.clone() {
            Some(c) => qdoc::model::materialize(state, &c)
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
        self.dict.get_or_insert_with(|| {
            let mut d = (*shared_dictionary()).clone();
            d.load_user_dict();
            d
        })
    }

    /// Misspelled byte spans for a body block, using `spell_cache` (keyed by the
    /// block's text so an edit re-checks and an unchanged block is free).
    fn block_spell_spans(&mut self, id: &str, text: &str) -> Vec<(usize, usize)> {
        if let Some((cached_text, spans)) = self.spell_cache.get(id)
            && cached_text == text
        {
            return spans.clone();
        }
        let language = self.spell_language.clone();
        let dict = self.dictionary();
        let spans = spell_spans(dict, &language, text);
        self.spell_cache
            .insert(id.to_string(), (text.to_string(), spans.clone()));
        spans
    }

    /// Toggle the spell-check squiggles on/off (default ON). View-only; forces a
    /// redraw.
    pub fn toggle_spellcheck(&mut self) {
        self.spellcheck_on = !self.spellcheck_on;
        self.dirty = true;
    }

    /// Whether the spell-check squiggles are currently enabled.
    pub fn spellcheck_enabled(&self) -> bool {
        self.spellcheck_on && ui::text_assistance::preferences().check_spelling
    }

    pub fn set_spell_language(&mut self, language: &str) {
        self.spell_language = language.to_string();
        self.spell_cache.clear();
        self.dirty = true;
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
        self.dirty = true;
    }

    /// Whether comment + tracked-change markup is currently drawn.
    pub fn markup_enabled(&self) -> bool {
        self.review.is_visible()
    }

    pub fn model(&self) -> &WordProcessor {
        &self.inner
    }
    /// Open or close the version-history strip — the same thing the toolbar's
    /// History button does. Public so a host, a headless scenario, or the
    /// `--shots` renderer can put the editor in that state without
    /// synthesising a toolbar click.
    pub fn toggle_history(&mut self, doc: &mut AutoCommit) {
        self.apply_tool(doc, ToolAction::ToggleHistory);
    }

    /// True while the version-history strip is open.
    pub fn history_open(&self) -> bool {
        self.history.is_open()
    }

    /// The version rows currently on the history strip, oldest first.
    pub fn history_entries(&self) -> &[ui::HistoryEntry] {
        self.history.entries()
    }

    /// Put the responsive style inspector in a deterministic state for
    /// examples, visual regression shots and embedders.
    pub fn set_styles_panel_open(&mut self, open: bool) {
        self.styles_panel_open = open;
        self.dirty = true;
    }

    /// Stage a representative non-destructive error toast for visual/a11y
    /// regression shots. Runtime failures use the same toast surface.
    pub fn stage_error_for_shot(&mut self) {
        self.toast = Some((t("toast-save-failed"), ToastKind::Error));
        self.dirty = true;
    }

    pub fn model_mut(&mut self) -> &mut WordProcessor {
        &mut self.inner
    }

    pub fn set_file(&mut self, path: PathBuf, format: WpFormat) {
        self.path = Some(path);
        self.format = Some(format);
    }

    /// Read an image file and insert it at the caret. Returns whether it inserted
    /// (false if the file couldn't be read). The bytes are stored in the doc's
    /// content-addressed blob store; the renderer/exporter decode them on demand.
    fn insert_image_from_path(&mut self, state: &mut AutoCommit, path: &std::path::Path) -> bool {
        match std::fs::read(path) {
            Ok(bytes) => {
                self.inner.insert_image_bytes(state, &bytes);
                self.refresh_blocks(state);
                self.modified = true;
                self.needs_commit = true;
                self.dirty = true;
                eprintln!(
                    "word-processor: inserted image from {} ({} bytes)",
                    path.display(),
                    bytes.len()
                );
                // A drop arrives as a file the toolkit spooled into our
                // runtime dir; once the bytes live in the document the
                // spool copy has no owner.
                if is_dnd_spool_path(path) {
                    let _ = std::fs::remove_file(path);
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

    /// Toolbar "Insert image" affordance. Opens the host **XDG file-chooser
    /// portal** (per the qOS HIG the app never draws its own picker): the D-Bus
    /// round-trip + host dialog run on a worker thread ([`portal::PickRequest`]);
    /// the chosen path comes back over a channel drained each frame in
    /// [`Self::drain_portal`], which inserts the image at the caret. A second
    /// request while one is already in flight is ignored.
    fn request_image_insert(&mut self, _state: &mut AutoCommit) {
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
        self.portal_purpose = Some(PortalPurpose::InsertImage);
    }

    /// **Open** a document through the host portal: pick a `.docx`/`.odt`/`.md`
    /// (etc.) file; the chosen path is drained next frame and replaces the
    /// current document in place. A request while one is already in flight is
    /// ignored.
    fn request_open_document(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.portal_rx = Some(
            portal::PickRequest::open("Open document", "Open")
                .filter(
                    portal::FileFilter::new("Documents")
                        .glob("*.docx")
                        .glob("*.odt")
                        .glob("*.md")
                        .glob("*.markdown")
                        .glob("*.html")
                        .glob("*.htm")
                        .glob("*.rtf"),
                )
                .thread_name("wp-portal-open")
                .send(),
        );
        self.portal_purpose = Some(PortalPurpose::OpenDocument);
    }

    /// **Save As**: pick a destination through the host portal. The format is
    /// chosen from the extension the user types/selects (`.docx` → Word, `.odt`
    /// → `OpenDocument`, …); the current document is written there on drain and
    /// becomes the document's path. Also the fallback first-save path for an
    /// untitled document (no more silent `~/Untitled.md`).
    fn request_save_as(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        // Pre-fill the current file name, or a sensible untitled default.
        let suggested = self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "Untitled.md".to_string(),
            |f| f.to_string_lossy().into_owned(),
        );
        self.portal_rx = Some(
            portal::PickRequest::save("Save as", "Save", &suggested)
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
                .send(),
        );
        self.portal_purpose = Some(PortalPurpose::SaveAs);
    }

    /// Drain a completed portal request, if any, and route the chosen file by the
    /// request's [`PortalPurpose`]: insert an image, open a document in place, or
    /// save the current document there. A cancel or portal failure leaves the
    /// document untouched. Called once per frame from `sync`; draining clears the
    /// in-flight channel so another request can start.
    fn drain_portal(&mut self, state: &mut AutoCommit) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                let purpose = self.portal_purpose.take();
                self.portal_rx = None;
                // A single-select open/save reports `Opened`; anything else
                // (cancelled, or a portal failure) leaves the document alone.
                let result = if let ui::portal::PortalOutcome::Opened(path) = outcome {
                    match purpose {
                        Some(PortalPurpose::InsertImage) => {
                            self.insert_image_from_path(state, &path);
                            Ok(())
                        }
                        Some(PortalPurpose::OpenDocument) => {
                            self.try_open_document_in_place(state, &path)
                        }
                        Some(PortalPurpose::SaveAs) => {
                            let format = WpFormat::from_path(&path);
                            self.set_file(path, format);
                            self.try_write_to_current_path(state)
                        }
                        None => Ok(()),
                    }
                } else if let portal::PortalOutcome::Failed(reason) = outcome {
                    eprintln!("word-processor: portal error: {reason}");
                    self.toast = Some((t("toast-file-dialog-failed"), ToastKind::Error));
                    Err(reason)
                } else {
                    Err("file picker was cancelled".into())
                };
                if let Some(invocation) = self.resource_file_invocation.take() {
                    self.resource_file_result = Some(match result {
                        Ok(()) => qos_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: qos_actions::ActionOutput::default(),
                        },
                        Err(message) if message == "file picker was cancelled" => {
                            qos_actions::InvocationUpdate::Cancelled {
                                invocation,
                                message: Some(message),
                            }
                        }
                        Err(message) => qos_actions::InvocationUpdate::Failed {
                            invocation,
                            error: qos_actions::ActionFailure {
                                code: "word_processor.file_operation_failed".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        },
                    });
                }
            }
            // Still open (Empty) — leave the channel in place; a hangup
            // (worker gone without sending) clears it so we don't leak.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                self.portal_purpose = None;
                if let Some(invocation) = self.resource_file_invocation.take() {
                    self.resource_file_result = Some(qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "word_processor.portal_worker_lost".into(),
                            message: "file picker stopped without returning a result".into(),
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
        }
    }

    /// Replace the current document in place with the file at `path` (format by
    /// extension), re-seat the caret, and adopt the path/format. Returns whether
    /// the document was replaced (a read or import failure leaves it untouched).
    fn try_open_document_in_place(
        &mut self,
        state: &mut AutoCommit,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                return Err(format!("could not open {}: {e}", path.display()));
            }
        };
        let format = WpFormat::from_path(path);
        if !self.inner.replace_with_format(state, &bytes, format) {
            return Err(format!("could not parse {}", path.display()));
        }
        self.set_file(path.to_path_buf(), format);
        self.refresh_blocks(state);
        // The whole body changed; per-block caches keyed by id/text are stale.
        self.layout_cache.clear();
        self.spell_cache.clear();
        // A freshly-opened document is not "modified" — it matches its file.
        self.modified = false;
        self.needs_commit = true;
        self.dirty = true;
        self.clear_recovery();
        Ok(())
    }

    /// Ctrl+S: save to the current path, or — for an untitled document — open the
    /// **Save As** portal instead of silently inventing a path.
    fn save(&mut self, state: &AutoCommit) {
        if self.path.is_none() {
            self.request_save_as();
            return;
        }
        self.write_to_current_path(state);
    }

    /// Write the current document to its already-set path/format via the qdoc
    /// codec. Assumes `path` + `format` are set (the Save-As drain sets them, and
    /// a document opened with a path already has them).
    fn write_to_current_path(&mut self, state: &AutoCommit) {
        if let Err(error) = self.try_write_to_current_path(state) {
            eprintln!("word-processor: {error}");
        }
    }

    fn try_write_to_current_path(&mut self, state: &AutoCommit) -> Result<(), String> {
        if let (Some(path), Some(format), Some(c)) = (
            self.path.as_ref(),
            self.format,
            self.inner.containers.as_ref(),
        ) {
            let dr = qdoc::DocRef { doc: state, c };
            match format.export(dr) {
                Ok(bytes) => {
                    std::fs::write(path, bytes).map_err(|e| {
                        eprintln!("word-processor: could not write {}: {e}", path.display());
                        self.toast = Some((t("toast-save-failed"), ToastKind::Error));
                        format!("could not write {}: {e}", path.display())
                    })?;
                    self.modified = false;
                    self.toast = Some((t("toast-saved"), ToastKind::Success));
                }
                Err(e) => {
                    eprintln!("word-processor: export failed: {e}");
                    self.toast = Some((t("toast-save-failed"), ToastKind::Error));
                    return Err(format!("export failed: {e}"));
                }
            }
        } else {
            return Err("document has no writable path or initialized body".into());
        }
        // A successful real save supersedes any recovery copy — drop it so a later
        // session doesn't re-offer stale unsaved changes.
        self.clear_recovery();
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
        let effect = self.find.open(seed.as_deref(), &self.blocks);
        self.apply_find_effect(state, effect);
    }

    fn close_find(&mut self) {
        self.find.close();
    }

    // ---- Zoom ---------------------------------------------------------------

    /// Set the zoom factor (clamped) and request a repaint. Returns whether it
    /// changed.
    fn set_zoom(&mut self, z: f64) -> bool {
        self.auto_fit_page = false;
        if self.zoom.set(z) {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    fn zoom_by(&mut self, delta: f64) -> bool {
        self.auto_fit_page = false;
        let changed = self.zoom.step(delta);
        self.dirty |= changed;
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

    fn scroll_to_block(&mut self, bid: &str) {
        if let Some(l) = self.laid.iter().find(|l| l.id == bid) {
            // l.y_top is screen-space (post-scroll); recover content space.
            let content_y = l.y_top + self.scroll_y;
            self.scroll_y = (content_y - 180.0).clamp(0.0, self.max_scroll);
        }
    }

    fn apply_find_effect(&mut self, state: &AutoCommit, effect: FindEffect) {
        if effect == FindEffect::SelectActive {
            self.select_current_match(state);
        }
    }

    fn apply_find_effect_mut(&mut self, state: &mut AutoCommit, effect: FindEffect) {
        match effect {
            FindEffect::None => {}
            FindEffect::SelectActive => self.select_current_match(state),
            FindEffect::ReplaceCurrent => {
                let Some((bid, start, end)) = self.find.active_match().cloned() else {
                    return;
                };
                let replacement = self.find.replacement().to_string();
                self.inner
                    .splice_block(state, &bid, start, end, &replacement);
                self.refresh_blocks(state);
                self.needs_commit = true;
                let effect = self.find.refresh(&self.blocks);
                self.apply_find_effect(state, effect);
            }
            FindEffect::ReplaceAll => {
                let replacement = self.find.replacement().to_string();
                let mut matches = self.find.matches().to_vec();
                // Replace from the highest char offset down so earlier offsets
                // in a block stay valid.
                matches.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
                for (bid, start, end) in matches.into_iter().rev() {
                    self.inner
                        .splice_block(state, &bid, start, end, &replacement);
                }
                self.refresh_blocks(state);
                self.needs_commit = true;
                let effect = self.find.refresh(&self.blocks);
                self.apply_find_effect(state, effect);
            }
        }
    }

    #[cfg(test)]
    fn replace_all(&mut self, state: &mut AutoCommit) {
        self.apply_find_effect_mut(state, FindEffect::ReplaceAll);
    }

    fn refresh_blocks(&mut self, state: &AutoCommit) {
        if let Some(c) = self.inner.containers.as_ref() {
            let snap = qdoc::model::materialize(state, c);
            self.blocks = snap.blocks;
            self.blobs = snap.blobs;
            self.header = snap.header;
            self.footer = snap.footer;
            self.notes = snap.notes;
            self.review.replace(snap.comments, snap.changes);
            // Drop spell-cache entries for blocks that no longer exist so the cache
            // stays bounded; surviving blocks keep their (text-keyed) spans.
            let live: std::collections::HashSet<&str> =
                self.blocks.iter().map(|b| b.id.as_str()).collect();
            self.spell_cache.retain(|id, _| live.contains(id.as_str()));
        }
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
                hit = Some((rl.id.clone(), byte_to_char(&rl.text, byte)));
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
                hit = Some((rl.id.clone(), byte_to_char(&rl.text, byte)));
            }
        }
        if let Some((block, offset)) = hit {
            if extend {
                self.inner.set_anchor();
            } else {
                self.inner.clear_anchor();
            }
            self.inner.set_caret(Caret { block, offset });
            self.dirty = true;
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
                hit = Some((l.id.clone(), byte_to_char(&l.text, byte)));
                break;
            }
        }
        if hit.is_none() {
            // above first / below last
            if let Some(l) = self.laid.first()
                && p.y < l.y_top
            {
                hit = Some((l.id.clone(), 0));
            }
            if hit.is_none()
                && let Some(l) = self.laid.last()
            {
                // Below the last slice → caret to the layout's final line. Hit at
                // the layout's own bottom (not the slice band height, which for a
                // split last slice covers only its carried-over lines).
                let byte = self.layout_cache.get(&l.id).map_or(0, |(_, lay)| {
                    lay.hit(num::f32_of_f64(p.x - l.text_x), lay.height() - 1.0)
                });
                hit = Some((l.id.clone(), byte_to_char(&l.text, byte)));
            }
        }
        if let Some((block, offset)) = hit {
            if extend {
                self.inner.set_anchor();
            } else {
                self.inner.clear_anchor();
            }
            self.inner.set_caret(Caret { block, offset });
            self.dirty = true;
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

/// The shared draw arguments [`WordProcessorWidget::draw_styles_panel`]
/// threads through its per-section helpers, so each one carries a handful of
/// positions rather than the whole argument list.
struct PanelDrawCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    tr: &'a mut TextRenderer,
    cc: ChromeColors,
}

impl WordProcessorWidget {
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
    fn persist_margins(&mut self, state: &mut AutoCommit) {
        self.inner.set_page_margins(
            state,
            [
                self.margins.top,
                self.margins.right,
                self.margins.bottom,
                self.margins.left,
            ],
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
    #[allow(clippy::too_many_arguments)]
    fn draw_running_band(
        scene: &mut Scene,
        ctf: Affine,
        tr: &mut TextRenderer,
        blocks: &[BlockView],
        page_no: usize,
        band: Rect,
        is_footer: bool,
        editing: bool,
    ) -> (bool, Vec<RegionLaid>) {
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
            let runs = if editing {
                running_head_runs_editing(b)
            } else {
                running_head_runs(b, page_no)
            };
            let align = block_align(b, TextAlign::Left);
            let layout = RichTextLayout::shape(
                &mut tr.font_system,
                &runs,
                false,
                size,
                wrap_w,
                align,
                FontFamily::Sans,
                vec![],
                vec![],
                1.0,
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
        scene.fill(Fill::NonZero, ctf, ACTIVE_BAND_FILL, None, &pad);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [4.0, 3.0]),
            ctf,
            ACTIVE_BAND_EDGE,
            None,
            &pad,
        );
        let label = if self.inner.region() == Region::Header {
            t("band-header")
        } else {
            t("band-footer")
        };
        let approx = num::f64_of_usize(label.chars().count()) * 6.4 + 12.0;
        let tab = Rect::new(
            band.x0 - 2.0,
            band.y0 - 15.0,
            band.x0 - 2.0 + approx,
            band.y0 - 1.0,
        );
        scene.fill(
            Fill::NonZero,
            ctf,
            ACTIVE_BAND_EDGE,
            None,
            &vello::kurbo::RoundedRect::from_rect(tab, 2.0),
        );
        // White label on the blue accent tab (a fixed contrast pair, not theme ink,
        // since the tab colour is fixed).
        tr.draw_text(
            scene,
            &label,
            10.0,
            Color::WHITE,
            ctf * Affine::translate((tab.x0 + 6.0, tab.y0 + 1.5)),
            None,
        );
    }

    /// Render the caret (and any selection highlight) inside the active
    /// header/footer band, using the laid geometry captured in `region_laid`. The
    /// band caret mirrors the body caret (same `caret_geometry` machinery) so it
    /// blinks-free but lands exactly between glyphs.
    fn draw_band_caret(&self, scene: &mut Scene, ctf: Affine) {
        if !self.focused {
            return;
        }
        let caret = self.inner.caret();
        // Selection highlight (single-band, char range) when present.
        if let Some((s, e)) = self.inner.band_selection_range()
            && let Some(rl) = self.region_laid.iter().find(|r| r.id == s.0)
        {
            let sb = char_to_byte(&rl.text, s.1);
            let eb = char_to_byte(&rl.text, e.1);
            for (rx0, ry0, rx1, ry1) in rl.layout.selection_rects(sb, eb) {
                let r = Rect::new(
                    rl.origin.x + f64::from(rx0),
                    rl.origin.y + f64::from(ry0),
                    rl.origin.x + f64::from(rx1),
                    rl.origin.y + f64::from(ry1),
                );
                scene.fill(Fill::NonZero, ctf, PAPER_SELECTION, None, &r);
            }
        }
        if let Some(rl) = self.region_laid.iter().find(|r| r.id == caret.block) {
            let byte = char_to_byte(&rl.text, caret.offset);
            let (cx, cy, ch) = rl.layout.caret_geometry(byte);
            let c = Rect::new(
                rl.origin.x + f64::from(cx),
                rl.origin.y + f64::from(cy),
                rl.origin.x + f64::from(cx) + CARET_WIDTH,
                rl.origin.y + f64::from(cy) + f64::from(ch),
            );
            scene.fill(Fill::NonZero, ctf, PAPER_CARET, None, &c);
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
        band: Rect,
        page_x: f64,
        page_w: f64,
    ) {
        let model = self.ruler_model(band, page_x, page_w);
        let colors = RulerColors {
            bar: self.chrome.bar,
            field: self.chrome.field,
            paper: self.chrome.paper,
            border: self.chrome.border,
            text: self.chrome.text,
            text_dim: self.chrome.text_dim,
            accent: self.chrome.accent,
            accent_faint: self.chrome.accent_faint,
        };
        self.ruler.draw(scene, transform, text, colors, &model);
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

    fn ruler_press(&mut self, state: &mut AutoCommit) {
        let model = self.current_ruler_model();
        if let Some(RulerEffect::AddTab(value)) = self.ruler.press(self.pointer, &model) {
            self.inner.add_tab_stop(state, value);
            self.refresh_blocks(state);
            self.needs_commit = true;
            self.modified = true;
            self.note_edit_for_autosave(state);
        }
    }

    fn ruler_drag_move(&mut self) {
        let model = self.current_ruler_model();
        if let Some(effect) = self.ruler.drag_to(self.pointer, &model) {
            self.apply_ruler_preview(effect);
        }
    }

    fn ruler_drag_release(&mut self, state: &mut AutoCommit) {
        let Some(effect) = self.ruler.release() else {
            return;
        };
        match effect {
            RulerEffect::CommitMargins => self.persist_margins(state),
            RulerEffect::CommitIndentLeft => {
                let value = self.caret_block_indent();
                let block = self.inner.caret.block.clone();
                self.inner.set_block_indent(state, &block, value);
            }
            RulerEffect::CommitIndentFirst => {
                let value = self.caret_block_first_line_indent();
                let block = self.inner.caret.block.clone();
                self.inner.set_block_first_line_indent(state, &block, value);
            }
            RulerEffect::CommitTabs { remove } => {
                let mut stops = self.caret_block_tab_stops();
                if let Some(index) = remove.filter(|index| *index < stops.len()) {
                    stops.remove(index);
                }
                let block = self.inner.caret.block.clone();
                self.inner.set_block_tab_stops(state, &block, &stops);
            }
            _ => return,
        }
        self.refresh_blocks(state);
        self.needs_commit = true;
        self.modified = true;
        self.note_edit_for_autosave(state);
    }

    /// Update the caret block's `indent` attr in the cached block views only (a
    /// live drag preview before the release persists it).
    fn set_caret_block_indent_view(&mut self, v: f64) {
        let id = self.inner.caret.block.clone();
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            b.attrs
                .insert("indent".to_string(), AttrVal::Float(v.max(0.0)));
        }
    }

    /// Update the caret block's `first_line_indent` attr in the cached block views.
    fn set_caret_block_first_line_view(&mut self, v: f64) {
        let id = self.inner.caret.block.clone();
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
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
        let accent = self.chrome.accent;
        if let Some(e) = self.active_edges() {
            let drag = self.guide_drag;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let active = drag.is_some_and(|g| match g.kind {
                    DragKind::Proportional => true,
                    DragKind::Pair => g.side == side || g.side.opposite() == side,
                    DragKind::Single => g.side == side,
                });
                let col = if active {
                    accent
                } else {
                    self.chrome.accent_faint
                };
                let t = if active { 2.0 } else { 1.0 };
                let line = if vert {
                    Rect::new(c - t / 2.0, page.y0, c + t / 2.0, page.y1)
                } else {
                    Rect::new(page.x0, c - t / 2.0, page.x1, c + t / 2.0)
                };
                scene.fill(Fill::NonZero, transform, col, None, &line);
                // grip dot at the line's midpoint
                let (gx, gy) = if vert {
                    (c, f64::midpoint(page.y0, page.y1))
                } else {
                    (f64::midpoint(page.x0, page.x1), c)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &vello::kurbo::Circle::new((gx, gy), 4.0),
                );
                // value label, offset inside the page
                let label = format!("{}", num::i64_of_f64(e.get(side).round()));
                let (lx, ly) = if vert {
                    (c + 6.0, page.y0 + 16.0)
                } else {
                    (page.x0 + 10.0, c + 5.0)
                };
                tr.draw_text(
                    scene,
                    &label,
                    12.0,
                    accent,
                    transform * Affine::translate((lx, ly)),
                    None,
                );
            }
        } else if self.layout_mode == LayoutMode::Spacing
            && let Some((hx, hy)) = self.spacing_handle()
        {
            let pill = Rect::new(hx - 20.0, hy - 3.0, hx + 20.0, hy + 3.0);
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &vello::kurbo::RoundedRect::from_rect(pill, 3.0),
            );
            let label = format!("{} px", num::i64_of_f64(self.focused_space_after().round()));
            tr.draw_text(
                scene,
                &label,
                12.0,
                accent,
                transform * Affine::translate((hx + 30.0, hy - 7.0)),
                None,
            );
        }
    }

    /// The bottom layout bar: mode chips + a live readout / hint.
    fn draw_layout_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        _theme: &Theme,
        bar: Rect,
    ) {
        let cc = self.chrome;
        scene.fill(Fill::NonZero, transform, cc.bar, None, &bar);
        scene.fill(
            Fill::NonZero,
            transform,
            cc.border,
            None,
            &Rect::new(bar.x0, bar.y0, bar.x1, bar.y0 + 1.0),
        );

        let cy = f64::midpoint(bar.y0, bar.y1);
        let mut x = bar.x0 + 12.0;
        self.layout_chips.clear();
        for (mode, label) in [
            (LayoutMode::Margins, t("layout-margins")),
            (LayoutMode::Spacing, t("layout-spacing")),
        ] {
            let w = 16.0 + num::f64_of_usize(label.chars().count()) * 7.6;
            let r = Rect::new(x, bar.y0 + 5.0, x + w, bar.y1 - 5.0);
            let active = self.layout_mode == mode;
            let bg = if active { cc.accent } else { cc.chip };
            let fg = if active { cc.on_accent } else { cc.text_dim };
            scene.fill(
                Fill::NonZero,
                transform,
                bg,
                None,
                &vello::kurbo::RoundedRect::from_rect(r, 6.0),
            );
            tr.draw_text(
                scene,
                &label,
                13.0,
                fg,
                transform * Affine::translate((x + 8.0, cy - 8.0)),
                None,
            );
            self.layout_chips.push((mode, r));
            x += w + 8.0;
        }

        let readout = match self.layout_mode {
            LayoutMode::Margins => t_args(
                "layout-margins-readout",
                &[
                    arg("l", num::i64_of_f64(self.margins.left.round())),
                    arg("t", num::i64_of_f64(self.margins.top.round())),
                    arg("r", num::i64_of_f64(self.margins.right.round())),
                    arg("b", num::i64_of_f64(self.margins.bottom.round())),
                ],
            ),
            LayoutMode::Spacing => {
                let name = self.focused_style_key().map_or_else(
                    || t("style-body"),
                    |k| crate::styles::style_display_name(&k, &self.sheet.get(&k)),
                );
                t_args(
                    "layout-spacing-readout",
                    &[
                        arg("name", name),
                        arg("px", num::i64_of_f64(self.focused_space_after().round())),
                    ],
                )
            }
            LayoutMode::None => t("layout-none-hint"),
        };
        if bar.width() >= COMPACT_LAYOUT_BAR_BREAKPOINT {
            tr.draw_text(
                scene,
                &readout,
                12.5,
                cc.text_dim,
                transform * Affine::translate((x + 12.0, cy - 8.0)),
                None,
            );
        }
        // Document status is always docked in chrome. A floating surface can
        // overlap the paper at any zoom, not merely in narrow tiles.
        let status = self.status_count_label();
        if let Some(status_rect) = Self::layout_bar_status_rect(bar, x + 12.0, &status) {
            tr.draw_text(
                scene,
                &status,
                12.5,
                cc.text_dim,
                transform * Affine::translate((status_rect.x0, cy - 8.0)),
                None,
            );
        }
    }

    /// Whether the styles panel is drawn this frame.
    fn styles_panel_visible(&self) -> bool {
        self.styles_panel_open
    }

    /// Narrow tiles present the inspector as a temporary rail over the page;
    /// wide windows reserve space for it. This keeps the document useful at
    /// phone/three-column widths without making advanced formatting unreachable.
    fn styles_panel_overlay(&self) -> bool {
        self.styles_panel_open && self.rect.width() < STYLES_PANEL_W + MAX_PAGE_W + 48.0
    }

    /// The contextual paragraph panel, in two clearly-labelled sections: **This
    /// paragraph** (direct formatting — alignment + line spacing instance attrs)
    /// and **Paragraph style** (the named-style editor: a picker + property rows
    /// that mutate the *style*, so every block of that style updates live).
    fn draw_styles_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        panel: Rect,
    ) {
        let cc = self.chrome;
        self.panel_rect = panel;
        self.panel_hits.clear();
        // Background + left border.
        scene.fill(Fill::NonZero, transform, cc.bar, None, &panel);
        scene.fill(
            Fill::NonZero,
            transform,
            cc.border,
            None,
            &Rect::new(panel.x0, panel.y0, panel.x0 + 1.0, panel.y1),
        );
        // Clip content to the panel so nothing bleeds into the bars.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &panel,
        );

        let pad = 14.0;
        let x0 = panel.x0 + pad;
        let right = panel.x1 - pad;
        let inner_w = right - x0;
        let mut y = panel.y0 + 14.0;

        // When an image is selected, the panel becomes an image inspector
        // (resize + flip) instead of the paragraph editor.
        if self.selected_image_id().is_some() {
            self.draw_image_inspector(scene, transform, tr, x0, right, y);
            scene.pop_layer();
            return;
        }

        let Some(key) = self.focused_style_key() else {
            tr.draw_text(
                scene,
                &t("panel-caret-hint"),
                12.5,
                cc.text_dim,
                transform * Affine::translate((x0, y)),
                Some(num::f32_of_f64(inner_w)),
            );
            scene.pop_layer();
            return;
        };
        // A snapshot of the focused style, threaded through every panel
        // section below that reads it.
        let st = self.sheet.get(&key);

        let mut ctx = PanelDrawCtx {
            scene,
            transform,
            tr,
            cc,
        };
        y = self.draw_panel_this_paragraph(&mut ctx, x0, right, y);
        y = self.draw_panel_style_header(&mut ctx, x0, right, y, &key, &st);
        y = self.draw_panel_font_weight(&mut ctx, x0, right, y, &st);
        y = self.draw_panel_colour(&mut ctx, x0, right, y, &st);

        // Numeric rows (style defaults: size, spacing before/after, indent).
        for field in [
            NumField::Size,
            NumField::SpaceBefore,
            NumField::SpaceAfter,
            NumField::Indent,
        ] {
            self.draw_num_row(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                field,
                field.get(&st),
                x0,
                right,
                y,
            );
            y += 30.0;
        }

        panel_divider(ctx.scene, ctx.transform, &ctx.cc, x0, right, y);
        y += 12.0;

        // New style from this.
        {
            let r = Rect::new(x0, y, right, y + 26.0);
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                r,
                &t("panel-new-style"),
                false,
                true,
            );
            self.panel_hits.push((r, PanelHit::NewStyle));
        }

        ctx.scene.pop_layer();
    }

    /// "This paragraph" section: alignment (L/C/R/J) and line-spacing cycling
    /// chip on the current paragraph(s) — direct/instance formatting, not the
    /// named style below it. Returns the `y` past it.
    fn draw_panel_this_paragraph(
        &mut self,
        ctx: &mut PanelDrawCtx,
        x0: f64,
        right: f64,
        mut y: f64,
    ) -> f64 {
        // ================= This paragraph (direct formatting) =============
        ctx.tr.draw_text(
            ctx.scene,
            &t("panel-this-paragraph"),
            10.5,
            ctx.cc.text_dim,
            ctx.transform * Affine::translate((x0, y)),
            None,
        );
        y += 18.0;

        // Alignment: L / C / R / J — the current paragraph(s), via an instance
        // `align` attr (not the style default).
        {
            let cur = self.current_align();
            // Compact single-glyph buttons: a long localized label ("Ausrichtung"
            // in German) must not collide with the L button, so the four glyphs are
            // narrow — moving the strip right to open a gap after the label.
            let bw = 28.0;
            let gap = 6.0;
            let strip_left = right - 4.0 * bw - 3.0 * gap;
            ctx.tr.draw_text(
                ctx.scene,
                &t("panel-align"),
                12.5,
                ctx.cc.text,
                ctx.transform * Affine::translate((x0, y + 5.0)),
                None,
            );
            for (i, (label, val)) in [
                ("L", "left"),
                ("C", "center"),
                ("R", "right"),
                ("J", "justify"),
            ]
            .iter()
            .enumerate()
            {
                let rx = strip_left + num::f64_of_usize(i) * (bw + gap);
                let r = Rect::new(rx, y, rx + bw, y + 24.0);
                draw_btn(
                    ctx.scene,
                    ctx.transform,
                    ctx.tr,
                    &ctx.cc,
                    r,
                    label,
                    cur == *val,
                    true,
                );
                self.panel_hits.push((r, PanelHit::ParaAlign(val)));
            }
            y += 30.0;
        }

        // Line spacing: a cycling chip (1.0 → 1.15 → 1.5 → 2.0) on the current
        // paragraph(s).
        {
            ctx.tr.draw_text(
                ctx.scene,
                &t("panel-line-spacing"),
                12.5,
                ctx.cc.text,
                ctx.transform * Affine::translate((x0, y + 5.0)),
                None,
            );
            let label = line_spacing_label(self.current_line_spacing());
            let r = Rect::new(right - 96.0, y, right, y + 24.0);
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                r,
                &label,
                false,
                true,
            );
            self.panel_hits.push((r, PanelHit::ParaLineSpacing));
            y += 30.0;
        }

        panel_divider(ctx.scene, ctx.transform, &ctx.cc, x0, right, y);
        y += 12.0;
        y
    }

    /// "Paragraph style" section: the focused style's display name + hint,
    /// and the style-picker grid (click a chip to apply it). Returns the `y`
    /// past it.
    fn draw_panel_style_header(
        &mut self,
        ctx: &mut PanelDrawCtx,
        x0: f64,
        right: f64,
        mut y: f64,
        key: &str,
        st: &crate::styles::StyleProps,
    ) -> f64 {
        // ================= Paragraph style (the named preset) =============
        let inner_w = right - x0;
        // `st` is a snapshot of the focused style (read-only copy), taken once
        // by the caller and threaded through every panel section that needs it.
        let display_name = crate::styles::style_display_name(key, st);
        ctx.tr.draw_text(
            ctx.scene,
            &t("panel-style"),
            10.5,
            ctx.cc.text_dim,
            ctx.transform * Affine::translate((x0, y)),
            None,
        );
        y += 16.0;
        ctx.tr.draw_text(
            ctx.scene,
            &display_name,
            18.0,
            ctx.cc.text,
            ctx.transform * Affine::translate((x0, y)),
            Some(num::f32_of_f64(inner_w)),
        );
        y += 24.0;
        // Caption: name the preset semantics explicitly so this section reads as
        // "edit the shared style" — distinct from the toolbar, which formats only
        // the current selection. This is what keeps the two homes unambiguous.
        ctx.tr.draw_text(
            ctx.scene,
            &t_args("panel-style-hint", &[arg("name", display_name)]),
            11.0,
            ctx.cc.text_dim,
            ctx.transform * Affine::translate((x0, y)),
            Some(num::f32_of_f64(inner_w)),
        );
        y += 22.0;

        // ---- Style picker -------------------------------------------------
        let order = self.sheet.order();
        let gap = 6.0;
        let col_w = (inner_w - gap) / 2.0;
        let chip_h = 24.0;
        for (i, id) in order.iter().enumerate() {
            let col = num::f64_of_usize(i % 2);
            let row = num::f64_of_usize(i / 2);
            let cx = x0 + col * (col_w + gap);
            let cy = y + row * (chip_h + gap);
            let r = Rect::new(cx, cy, cx + col_w, cy + chip_h);
            let name = crate::styles::style_display_name(id, &self.sheet.get(id));
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                r,
                &name,
                id.as_str() == key,
                false,
            );
            self.panel_hits.push((r, PanelHit::Pick(id.clone())));
        }
        let rows = num::f64_of_usize(order.len().div_ceil(2));
        y += rows * (chip_h + gap) + 6.0;
        panel_divider(ctx.scene, ctx.transform, &ctx.cc, x0, right, y);
        y += 12.0;
        y
    }

    /// Editable style-default properties, part 1: font family (cycling chip)
    /// and Bold/Italic toggles. Returns the `y` past it.
    fn draw_panel_font_weight(
        &mut self,
        ctx: &mut PanelDrawCtx,
        x0: f64,
        right: f64,
        mut y: f64,
        st: &crate::styles::StyleProps,
    ) -> f64 {
        // ---- Editable style properties (defaults for every block of this style;
        // alignment + line spacing are the direct controls above, not here). The
        // "PARAGRAPH STYLE" header + style name already frame this as the preset
        // editor, so no separate sub-header is needed.

        // Font (cycles sans → serif → mono).
        {
            ctx.tr.draw_text(
                ctx.scene,
                &t("panel-font"),
                12.5,
                ctx.cc.text,
                ctx.transform * Affine::translate((x0, y + 5.0)),
                None,
            );
            let fl = t(match st.font.as_str() {
                "serif" => "panel-font-serif",
                "mono" => "panel-font-mono",
                _ => "panel-font-sans",
            });
            let r = Rect::new(right - 96.0, y, right, y + 24.0);
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                r,
                &fl,
                false,
                true,
            );
            self.panel_hits.push((r, PanelHit::CycleFont));
            y += 30.0;
        }

        // Weight: Bold / Italic toggles.
        {
            ctx.tr.draw_text(
                ctx.scene,
                &t("panel-weight"),
                12.5,
                ctx.cc.text,
                ctx.transform * Affine::translate((x0, y + 5.0)),
                None,
            );
            let bw = 60.0;
            let rb = Rect::new(right - bw * 2.0 - 6.0, y, right - bw - 6.0, y + 24.0);
            let ri = Rect::new(right - bw, y, right, y + 24.0);
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                rb,
                &t("panel-bold"),
                st.bold,
                true,
            );
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                ri,
                &t("panel-italic"),
                st.italic,
                true,
            );
            self.panel_hits.push((rb, PanelHit::ToggleBold));
            self.panel_hits.push((ri, PanelHit::ToggleItalic));
            y += 30.0;
        }
        y
    }

    /// Editable style-default properties, part 2: the colour swatch strip +
    /// "Custom…" affordance that opens the shared colour-picker popover.
    /// Returns the `y` past it.
    fn draw_panel_colour(
        &mut self,
        ctx: &mut PanelDrawCtx,
        x0: f64,
        right: f64,
        mut y: f64,
        st: &crate::styles::StyleProps,
    ) -> f64 {
        // Colour: quick-preset swatches + a "Custom…" affordance that opens the
        // shared `ColorPicker` popover for an arbitrary colour.
        let inner_w = right - x0;
        {
            ctx.tr.draw_text(
                ctx.scene,
                &t("panel-colour"),
                12.5,
                ctx.cc.text,
                ctx.transform * Affine::translate((x0, y + 5.0)),
                None,
            );
            let n = STYLE_PALETTE.len();
            let sw_gap = 5.0;
            // Leave room on the left for the "Colour" label.
            let strip_w = (inner_w - 54.0).clamp(120.0, 196.0);
            let sw = (strip_w - sw_gap * (num::f64_of_usize(n) - 1.0)) / num::f64_of_usize(n);
            let sx0 = right - strip_w;
            for (i, c) in STYLE_PALETTE.iter().enumerate() {
                let sx = sx0 + num::f64_of_usize(i) * (sw + sw_gap);
                let r = Rect::new(sx, y, sx + sw, y + 22.0);
                let col = Color::from_rgb8(c[0], c[1], c[2]);
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    col,
                    None,
                    &vello::kurbo::RoundedRect::from_rect(r, 4.0),
                );
                if *c == st.color {
                    ctx.scene.stroke(
                        &vello::kurbo::Stroke::new(2.0),
                        ctx.transform,
                        Color::WHITE,
                        None,
                        &vello::kurbo::RoundedRect::from_rect(r, 4.0),
                    );
                }
                self.panel_hits.push((r, PanelHit::SetColor(*c)));
            }
            y += 30.0;
            // "Custom…" — shows the current colour chip and opens the full picker.
            let chip = Rect::new(x0, y, x0 + 22.0, y + 22.0);
            let cur = Color::from_rgb8(st.color[0], st.color[1], st.color[2]);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                cur,
                None,
                &vello::kurbo::RoundedRect::from_rect(chip, 4.0),
            );
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                ctx.transform,
                ctx.cc.border,
                None,
                &vello::kurbo::RoundedRect::from_rect(chip, 4.0),
            );
            let btn = Rect::new(chip.x1 + 8.0, y, right, y + 22.0);
            draw_btn(
                ctx.scene,
                ctx.transform,
                ctx.tr,
                &ctx.cc,
                btn,
                &t("panel-colour-custom"),
                self.color_picker.is_open(),
                true,
            );
            self.panel_hits.push((btn, PanelHit::OpenColorPicker));
            self.panel_hits.push((chip, PanelHit::OpenColorPicker));
            y += 30.0;
        }
        y
    }

    /// The image inspector shown in the styles panel when an image is selected:
    /// the current display size, a Size stepper (Smaller / Larger, aspect-locked)
    /// and Flip toggles (Horizontal / Vertical). Registers its controls as
    /// `panel_hits` so the panel's existing left-click routing dispatches them.
    fn draw_image_inspector(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        x0: f64,
        right: f64,
        y0: f64,
    ) {
        let cc = self.chrome;
        let inner_w = right - x0;
        let mut y = y0;

        tr.draw_text(
            scene,
            &t("panel-image"),
            10.5,
            cc.text_dim,
            transform * Affine::translate((x0, y)),
            None,
        );
        y += 22.0;

        // Current display dimensions (from this frame's laid geometry).
        let (dw, dh) = self.selected_image_dims().unwrap_or((0.0, 0.0));
        let dims = format!(
            "{} × {} px",
            num::i64_of_f64(dw.round()),
            num::i64_of_f64(dh.round())
        );
        tr.draw_text(
            scene,
            &dims,
            15.0,
            cc.text,
            transform * Affine::translate((x0, y)),
            None,
        );
        y += 26.0;
        panel_divider(scene, transform, &cc, x0, right, y);
        y += 12.0;

        // ---- Size (aspect-locked scale steppers) --------------------------
        // Leave a label column on the left; the two buttons share the right side.
        tr.draw_text(
            scene,
            &t("panel-image-size"),
            12.5,
            cc.text,
            transform * Affine::translate((x0, y + 5.0)),
            None,
        );
        let bw = ((inner_w - 60.0) / 2.0 - 6.0).clamp(48.0, 92.0);
        let rs = Rect::new(right - bw * 2.0 - 6.0, y, right - bw - 6.0, y + 26.0);
        let rl = Rect::new(right - bw, y, right, y + 26.0);
        draw_btn(
            scene,
            transform,
            tr,
            &cc,
            rs,
            &t("panel-image-smaller"),
            false,
            true,
        );
        draw_btn(
            scene,
            transform,
            tr,
            &cc,
            rl,
            &t("panel-image-larger"),
            false,
            true,
        );
        self.panel_hits.push((rs, PanelHit::ImageScale(0.9)));
        self.panel_hits.push((rl, PanelHit::ImageScale(1.1)));
        y += 34.0;

        self.draw_panel_image_flip(scene, transform, tr, x0, right, y, bw);
    }

    /// The image inspector's Flip section: Horizontal / Vertical mirror
    /// toggles, reflecting the selected image's current `flip_h`/`flip_v`
    /// attrs. `bw` is the button width the Size section above already
    /// computed, so the two rows of steppers line up.
    #[allow(clippy::too_many_arguments)]
    fn draw_panel_image_flip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        x0: f64,
        right: f64,
        y: f64,
        bw: f64,
    ) {
        let cc = self.chrome;
        let img_id = self.selected_image_id();
        let (fh, fv) = self
            .blocks
            .iter()
            .find(|b| Some(&b.id) == img_id.as_ref())
            .map_or((false, false), |b| {
                (
                    matches!(b.attrs.get("flip_h"), Some(AttrVal::Bool(true))),
                    matches!(b.attrs.get("flip_v"), Some(AttrVal::Bool(true))),
                )
            });
        tr.draw_text(
            scene,
            &t("panel-image-flip"),
            12.5,
            cc.text,
            transform * Affine::translate((x0, y + 5.0)),
            None,
        );
        let rh = Rect::new(right - bw * 2.0 - 6.0, y, right - bw - 6.0, y + 26.0);
        let rv = Rect::new(right - bw, y, right, y + 26.0);
        draw_btn(
            scene,
            transform,
            tr,
            &cc,
            rh,
            &t("panel-image-flip-h"),
            fh,
            true,
        );
        draw_btn(
            scene,
            transform,
            tr,
            &cc,
            rv,
            &t("panel-image-flip-v"),
            fv,
            true,
        );
        self.panel_hits.push((rh, PanelHit::ImageFlipH));
        self.panel_hits.push((rv, PanelHit::ImageFlipV));
    }

    /// One numeric property row: `Label    [−] [ value ] [+]`. The value box is
    /// click-to-type / drag-to-scrub; the steppers nudge by `field.step()`.
    #[allow(clippy::too_many_arguments)] // each param is a distinct draw input; no natural grouping
    fn draw_num_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        field: NumField,
        value: f32,
        x0: f64,
        right: f64,
        y: f64,
    ) {
        let cc = self.chrome;
        tr.draw_text(
            scene,
            &field.label(),
            12.5,
            cc.text,
            transform * Affine::translate((x0, y + 5.0)),
            None,
        );
        let plus = Rect::new(right - 24.0, y, right, y + 24.0);
        let vb = Rect::new(plus.x0 - 60.0, y, plus.x0 - 2.0, y + 24.0);
        let minus = Rect::new(vb.x0 - 26.0, y, vb.x0 - 2.0, y + 24.0);
        draw_btn(scene, transform, tr, &cc, minus, "−", false, true);
        draw_btn(scene, transform, tr, &cc, plus, "+", false, true);
        // Value box.
        let editing = self.editing_field == Some(field);
        let scrubbing = self.scrub.is_some_and(|s| s.field == field);
        let border = if editing || scrubbing {
            cc.accent
        } else {
            cc.border
        };
        scene.fill(
            Fill::NonZero,
            transform,
            cc.field,
            None,
            &vello::kurbo::RoundedRect::from_rect(vb, 5.0),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(if editing || scrubbing { 1.5 } else { 1.0 }),
            transform,
            border,
            None,
            &vello::kurbo::RoundedRect::from_rect(vb, 5.0),
        );
        let text = if editing {
            format!("{}_", self.edit_buf)
        } else {
            fmt_num(value)
        };
        let approx = num::f64_of_usize(text.chars().count()) * 7.0;
        let tx = f64::midpoint(vb.x0, vb.x1) - approx / 2.0;
        tr.draw_text(
            scene,
            &text,
            12.5,
            cc.text,
            transform * Affine::translate((tx, f64::midpoint(vb.y0, vb.y1) - 8.0)),
            None,
        );
        self.panel_hits.push((vb, PanelHit::Num(field)));
        self.panel_hits
            .push((minus, PanelHit::NumDelta(field, -field.step())));
        self.panel_hits
            .push((plus, PanelHit::NumDelta(field, field.step())));
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
    fn generate_toc(&mut self, state: &mut AutoCommit) {
        // Map heading block id → 1-based page from the last laid-out frame.
        let page_of: HashMap<&str, usize> = self
            .laid
            .iter()
            .map(|l| (l.id.as_str(), l.page + 1))
            .collect();
        let entries: Vec<crate::word_processor::TocEntry> = self
            .outline_entries()
            .into_iter()
            .map(|h| {
                let page = page_of.get(h.id.as_str()).copied();
                crate::word_processor::TocEntry {
                    level: h.level,
                    text: h.text,
                    page,
                }
            })
            .collect();
        if entries.is_empty() {
            // Nothing to build a TOC from — tell the user *why* nothing happened.
            self.toast = Some((t("toast-toc-empty"), ToastKind::Info));
            self.dirty = true;
            return;
        }
        let n = entries.len();
        self.inner.generate_toc(state, &entries);
        self.refresh_blocks(state);
        self.needs_commit = true;
        self.dirty = true;
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
            self.rect.y1 - LAYOUT_BAR_H,
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

/// A horizontal 1px divider line inside the panel.
fn panel_divider(
    scene: &mut Scene,
    transform: Affine,
    cc: &ChromeColors,
    x0: f64,
    x1: f64,
    y: f64,
) {
    scene.fill(
        Fill::NonZero,
        transform,
        cc.border,
        None,
        &Rect::new(x0, y, x1, y + 1.0),
    );
}

/// A panel chip / button: rounded rect + label. `center` centers the label
/// (approximate), else left-pads it.
#[allow(clippy::too_many_arguments)] // each param is a distinct draw input; no natural grouping
fn draw_btn(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    cc: &ChromeColors,
    r: Rect,
    label: &str,
    active: bool,
    center: bool,
) {
    let bg = if active { cc.accent } else { cc.chip };
    let fg = if active { cc.on_accent } else { cc.text };
    scene.fill(
        Fill::NonZero,
        transform,
        bg,
        None,
        &vello::kurbo::RoundedRect::from_rect(r, 6.0),
    );
    let approx = num::f64_of_usize(label.chars().count()) * 6.6;
    let tx = if center {
        (f64::midpoint(r.x0, r.x1) - approx / 2.0).max(r.x0 + 4.0)
    } else {
        r.x0 + 8.0
    };
    let ty = f64::midpoint(r.y0, r.y1) - 8.0;
    tr.draw_text(
        scene,
        label,
        12.5,
        fg,
        transform * Affine::translate((tx, ty)),
        Some(num::f32_of_f64(r.width() - 6.0)),
    );
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
        ToolAction::ToggleHistory => "toggle-history".into(),
        ToolAction::InsertToc => "insert-table-of-contents".into(),
        ToolAction::NewMenu => "new-from-template".into(),
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
/// toolbar labels are terse glyphs ("B", "H1", "🖼 Image") tuned for sighted
/// users; a screen reader needs the spelled-out verb instead.
fn tool_action_a11y_label(a: ToolAction) -> String {
    let key = match a {
        ToolAction::Bold => "a11y-bold",
        ToolAction::Italic => "a11y-italic",
        ToolAction::Underline => "a11y-underline",
        ToolAction::Strike => "a11y-strikethrough",
        ToolAction::Super => "a11y-superscript",
        ToolAction::Sub => "a11y-subscript",
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
        ToolAction::ToggleHistory => "a11y-version-history",
        ToolAction::InsertToc => "a11y-insert-toc",
        ToolAction::NewMenu => "a11y-new-from-template",
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
    }
}

fn review_action_a11y_label(
    action: &ReviewAction,
    changes: &[qdoc::snapshot::ChangeView],
) -> String {
    use qdoc::snapshot::ChangeKind;
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
        let buttons: Vec<A11yTreeNode> = self
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
                        | ToolAction::ToggleHistory
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
                            ToolAction::ToggleStyles => self.styles_panel_open,
                            ToolAction::ToggleRuler => self.ruler_on,
                            ToolAction::ToggleOutline => self.outline.is_open(),
                            ToolAction::ToggleHistory => self.history.is_open(),
                            _ => false,
                        }),
                        ..Default::default()
                    });
                A11yTreeNode::leaf(node)
            })
            .collect();
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

    /// Assemble the full accessibility node tree: a [`Role::Document`] root holding
    /// one node per block (headings with level, paragraphs/list items with text,
    /// images with alt text, tables as [`Role::Table`]/[`Role::Cell`]), followed by
    /// a [`Role::Toolbar`] group of formatting buttons. [`Widget::accessibility`]
    /// returns this tree's root so the host's per-widget path surfaces the document.
    pub fn accessibility_tree(&self) -> A11yTreeNode {
        use ui::a11y::{AccessNode, AccessState, Role};

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

        A11yTreeNode {
            node: AccessNode::new(Role::Document).with_state(AccessState {
                focusable: true,
                focused: self.focused,
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
struct FrameGeometry {
    canvas: Rect,
    canvas_left: f64,
    canvas_right: f64,
    tb_h: f64,
    layout_bar_top: f64,
    history_top: f64,
    history_h: f64,
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
    /// Chrome bands above the canvas — toolbar, recovery banner, Find bar,
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
        if self.auto_fit_page {
            let mut available = full.width();
            if self.styles_panel_visible() && !self.styles_panel_overlay() {
                available -= STYLES_PANEL_W;
            }
            if self.outline.is_open() {
                available -= OUTLINE_PANEL_W;
            }
            let fit = ((available - 32.0) / MAX_PAGE_W).clamp(ZOOM_MIN, 1.0);
            self.zoom.set(fit);
        }
        // The toolbar may wrap to several rows on a narrow window; reserve its
        // actual (possibly multi-row) height so the canvas top sits below it and
        // no control is clipped. `draw_toolbar` re-derives the same packing, so
        // the reserved band and the drawn rows always agree.
        let tb_h = self.toolbar_height(full.width());
        let tb = Rect::new(full.x0, full.y0, full.x1, full.y0 + tb_h);
        self.draw_toolbar(scene, transform, text_renderer, theme, tb);

        // No bottom status strip: its readout floats as a transient component,
        // so the content spans down to the layout bar and
        // reclaims the strip's height. The layout bar now sits at the very bottom.
        let content_bottom = full.y1.max(full.y0 + tb_h);
        let layout_bar_top = (content_bottom - LAYOUT_BAR_H).max(full.y0 + tb_h);
        // Version-history strip: a band between the canvas and the layout bar,
        // so the page shrinks rather than the history painting over the text.
        let history_h = self.history.band_height();
        let history_top = (layout_bar_top - history_h).max(full.y0 + tb_h);
        // Reserve the styles panel out of the canvas so the page re-centers in the
        // remaining width (and scrolled content never paints under the panel).
        let canvas_right = if self.styles_panel_visible() && !self.styles_panel_overlay() {
            (full.x1 - STYLES_PANEL_W).max(full.x0 + 1.0)
        } else {
            full.x1
        };
        // Reserve the outline / navigation pane out of the canvas LEFT (mirrors the
        // styles panel on the right) so the page re-centers and scrolled content
        // never paints under the pane.
        let canvas_left = if self.outline.is_open() {
            (full.x0 + OUTLINE_PANEL_W).min(canvas_right - 1.0)
        } else {
            full.x0
        };
        // Reserve + draw the crash-recovery banner (when staged) and the Find
        // bar (when open), stacked directly under the toolbar.
        let (recovery_h, find_h) = self.draw_recovery_and_find_bars(
            scene,
            transform,
            text_renderer,
            theme,
            canvas_left,
            canvas_right,
            full.y0 + tb_h,
        );
        // Reserve the horizontal ruler band (when on) out of the canvas top, below
        // the find bar. It is drawn with the *unscaled* `transform` so it stays a
        // fixed-height strip while the page scrolls/zooms; its tick + marker
        // positions are mapped through the zoom so they track the page edge.
        let ruler_h = if self.ruler_on { RULER_H } else { 0.0 };
        let canvas = Rect::new(
            canvas_left,
            full.y0 + tb_h + recovery_h + find_h + ruler_h,
            canvas_right,
            history_top,
        );
        scene.fill(Fill::NonZero, transform, self.chrome.canvas, None, &canvas);

        // Zoom anchor: the canvas top, horizontally centered. The page+content
        // transform scales about this point, so the page stays centered and the
        // top of the (scrolled) content stays pinned to the canvas top as zoom
        // changes. The toolbar/find-bar/status bar are drawn with the unscaled
        // `transform` and are unaffected.
        self.zoom
            .set_anchor(Point::new(f64::midpoint(canvas.x0, canvas.x1), canvas.y0));
        let ctf = self.content_transform(transform);

        // Per-side page geometry — every margin is independently customizable.
        // The page is a FIXED size (it does not resize with the window); the
        // window just shows more or less of it and centers it.
        let m = self.margins;
        let page_w = MAX_PAGE_W;
        // Centre in *content* space even when the page is wider than the
        // unscaled canvas. Scaling about the canvas centre then keeps both page
        // edges visible in auto-fit mode instead of pinning the left edge and
        // clipping the right.
        let page_x = f64::midpoint(canvas.x0, canvas.x1) - page_w / 2.0;
        let page_top = canvas.y0 + PAGE_TOP;
        let content_x = page_x + m.left;
        let content_w = num::f32_of_f64((page_w - m.left - m.right).max(80.0));

        // Horizontal ruler band (drawn unscaled, above the canvas). Reads the
        // page edge geometry just computed, plus the caret paragraph's indent +
        // tab stops from the cached block views.
        if ruler_h > 0.0 {
            let band = Rect::new(
                canvas_left,
                full.y0 + tb_h + recovery_h + find_h,
                canvas_right,
                canvas.y0,
            );
            self.draw_ruler(scene, transform, text_renderer, band, page_x, page_w);
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
            canvas_left,
            canvas_right,
            tb_h,
            layout_bar_top,
            history_top,
            history_h,
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

    /// Reserve + draw the crash-recovery banner (when an offer is staged) and
    /// the Find bar (when open), stacked directly under the toolbar at
    /// `tb_bottom`. Returns `(recovery_h, find_h)` — the heights the caller
    /// reserves out of the canvas top.
    #[allow(clippy::too_many_arguments)]
    fn draw_recovery_and_find_bars(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        canvas_left: f64,
        canvas_right: f64,
        tb_bottom: f64,
    ) -> (f64, f64) {
        let recovery_h = self.recovery.height();
        if recovery_h > 0.0 {
            let rbar = Rect::new(canvas_left, tb_bottom, canvas_right, tb_bottom + recovery_h);
            let cc = self.chrome;
            self.recovery.draw(
                scene,
                transform,
                text_renderer,
                RecoveryPalette {
                    border: cc.border,
                    chip: cc.chip,
                    text: cc.text,
                    on_accent: cc.on_accent,
                    accent: theme.accent_primary,
                    notice: with_alpha(theme.accent_primary, 0x22),
                },
                rbar,
            );
        }
        let find_h = self.find.height();
        if find_h > 0.0 {
            let fbar = Rect::new(
                canvas_left,
                tb_bottom + recovery_h,
                canvas_right,
                tb_bottom + recovery_h + find_h,
            );
            let cc = self.chrome;
            self.find.draw(
                scene,
                transform,
                text_renderer,
                theme,
                &PresenceCtx::empty(),
                FindPalette {
                    bar: cc.bar,
                    chip: cc.chip,
                    border: cc.border,
                    text: cc.text,
                    text_dim: cc.text_dim,
                    accent: cc.accent,
                    on_accent: cc.on_accent,
                    danger: theme.tokens.colors.semantic.danger,
                },
                fbar,
            );
        }
        (recovery_h, find_h)
    }

    /// Pass 1 of [`Self::draw`]: shape blocks top-to-bottom and paginate them
    /// across discrete page sheets. Runs of consecutive `cell` blocks sharing a
    /// table id lay out as a grid; everything else stacks normally.
    /// Pass 1 of [`Self::draw`]: shape blocks top-to-bottom and paginate them
    /// across discrete page sheets. Runs of consecutive `cell` blocks sharing a
    /// table id lay out as a grid; everything else stacks normally.
    fn paginate_blocks(
        &mut self,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
    ) -> PaginationResult {
        let blocks = std::mem::take(&mut self.blocks);
        let mut laid: Vec<Laid> = Vec::with_capacity(blocks.len());
        // Incremental layout cache: take last frame's shaped layouts so matching
        // blocks can be reused, and build this frame's cache afresh. Any entry
        // left in `old_cache` at the end (a block that no longer exists) is
        // dropped — this is the per-frame prune that keeps memory bounded.
        let mut old_cache = std::mem::take(&mut self.layout_cache);
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
        let mut cur = PageCursor {
            cur_page: 0,
            placed_on_page: false,
            y: page_content_top(fg, 0),
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
                bi = self.paginate_table(
                    &blocks,
                    bi,
                    fg,
                    cell_pad,
                    text_renderer,
                    &mut old_cache,
                    &mut new_cache,
                    &mut laid,
                    &mut cur,
                );
            } else if b.kind == "image" {
                self.paginate_image(b, fg, text_renderer, &mut new_cache, &mut laid, &mut cur);
                bi += 1;
            } else {
                self.paginate_paragraph(
                    b,
                    fg,
                    text_renderer,
                    &mut old_cache,
                    &mut new_cache,
                    &mut laid,
                    &mut cur,
                );
                bi += 1;
            }
        }

        PaginationResult {
            laid,
            cache: new_cache,
            cur_page: cur.cur_page,
            y: cur.y,
            blocks,
        }
    }

    /// One table (a run of consecutive `cell` blocks sharing a table id):
    /// measure every row first (so the whole table paginates as one unit,
    /// never split mid-row across a page boundary), then lay out row by row,
    /// breaking to a fresh page when a row would cross the bottom margin — the
    /// first row on a page never breaks, so an over-tall single row overflows
    /// its page (acceptable for v1). Returns the block index just past the
    /// table's last cell.
    #[allow(clippy::too_many_arguments)]
    fn paginate_table(
        &mut self,
        blocks: &[BlockView],
        bi: usize,
        fg: &FrameGeometry,
        cell_pad: f64,
        text_renderer: &mut TextRenderer,
        old_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
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
        let rows = self.measure_table_rows(
            cells,
            ncols,
            nrows,
            col_w,
            cell_pad,
            text_renderer,
            old_cache,
            new_cache,
        );
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
                    y_top: ry + cell_pad,
                    text_x: cell_x + cell_pad,
                    width: col_w - 2.0 * cell_pad,
                    height: cell_h,
                    layout_y: ry + cell_pad,
                    clip: None,
                    bullet: None,
                    ink: PAPER_INK,
                    image_hash: None,
                    cell_box: Some(cbox),
                    cell_row: Some(r),
                    page: cur.cur_page,
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
    #[allow(clippy::too_many_arguments)]
    fn measure_table_rows<'a>(
        &mut self,
        cells: &'a [BlockView],
        ncols: usize,
        nrows: usize,
        col_w: f64,
        cell_pad: f64,
        text_renderer: &mut TextRenderer,
        old_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
    ) -> Vec<TableRow<'a>> {
        let mut rows: Vec<TableRow<'a>> = Vec::with_capacity(nrows);
        for r in 0..nrows {
            let mut row_items: Vec<(usize, f64, &BlockView)> = Vec::new();
            let mut row_h = 26.0f64;
            for c in 0..ncols {
                if let Some(cell) = cells.iter().find(|cl| cell_rc(cl) == (r, c)) {
                    let (runs, underlines, strikes) =
                        block_runs_markup(cell, self.review.is_visible());
                    let wrap_w = num::f32_of_f64((col_w - 2.0 * cell_pad).max(24.0));
                    let align = block_align(cell, TextAlign::Left);
                    let key = shape_key(
                        &runs,
                        &underlines,
                        &strikes,
                        false,
                        16.0,
                        wrap_w,
                        align,
                        FontFamily::Sans,
                        1.0,
                    );
                    let layout = match old_cache.remove(&cell.id) {
                        Some((k, lay)) if k == key => {
                            self.dbg_layout_hits += 1;
                            lay
                        }
                        _ => {
                            self.dbg_layout_misses += 1;
                            RichTextLayout::shape(
                                &mut text_renderer.font_system,
                                &runs,
                                false,
                                16.0,
                                wrap_w,
                                align,
                                FontFamily::Sans,
                                underlines,
                                strikes,
                                1.0,
                            )
                        }
                    };
                    let cell_h = f64::from(layout.height());
                    row_h = row_h.max(cell_h + 2.0 * cell_pad);
                    new_cache.insert(cell.id.clone(), (key, layout));
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
    /// Returns `(blob_hash, intrinsic_size, draw_w, draw_h)`.
    fn resolve_image_display_size(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
    ) -> (String, Option<(u32, u32)>, f64, f64) {
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
        let empty = RichTextLayout::shape(
            &mut text_renderer.font_system,
            &[RichSpan::new("")],
            false,
            16.0,
            fg.content_w,
            TextAlign::Left,
            FontFamily::Sans,
            vec![],
            vec![],
            1.0,
        );
        new_cache.insert(b.id.clone(), (0, empty));
        (hash, intrinsic, draw_w, draw_h)
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
        let (hash, intrinsic, draw_w, draw_h) =
            self.resolve_image_display_size(b, fg, text_renderer, new_cache);
        // Break to the next page on a manual page break, or if the image
        // would not fit.
        let force_break = block_page_break_before(b);
        let item_top = if cur.placed_on_page
            && (force_break || cur.y + draw_h > page_content_bottom(fg, cur.cur_page) + 0.5)
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
                fg.content_x + f64::from(fg.content_w) - draw_w
            };
            laid.push(Laid {
                id: b.id.clone(),
                text: String::new(),
                kind: "image".into(),
                size: 16.0,
                y_top: item_top,
                text_x: img_x,
                width: draw_w,
                height: draw_h,
                layout_y: item_top,
                clip: None,
                bullet: None,
                ink: PAPER_INK,
                image_hash: intrinsic.map(|_| hash),
                cell_box: None,
                cell_row: None,
                page: cur.cur_page,
            });
            cur.active_float = Some(FloatReserve {
                left,
                reserve_w: draw_w + FLOAT_GUTTER,
                bottom: item_top + draw_h,
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
                fg.content_x + ((f64::from(fg.content_w) - draw_w) / 2.0).max(0.0)
            } else {
                fg.content_x
            };
            laid.push(Laid {
                id: b.id.clone(),
                text: String::new(),
                kind: "image".into(),
                size: 16.0,
                y_top: item_top,
                text_x: img_x,
                width: draw_w,
                height: draw_h,
                layout_y: item_top,
                clip: None,
                bullet: None,
                ink: PAPER_INK,
                image_hash: intrinsic.map(|_| hash),
                cell_box: None,
                cell_row: None,
                page: cur.cur_page,
            });
            cur.y = item_top + draw_h + f64::from(self.sheet.get("body").space_after);
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
        let key = shape_key(
            &runs,
            &underlines,
            &strikes,
            base_bold,
            size,
            wrap_w,
            align,
            family,
            line_mult,
        );
        // Reuse the shaped layout if nothing affecting shaping changed
        // (text, marks, resolved style, wrap width, or line spacing), else
        // re-shape.
        let layout = match old_cache.remove(&b.id) {
            Some((k, lay)) if k == key => {
                self.dbg_layout_hits += 1;
                lay
            }
            _ => {
                self.dbg_layout_misses += 1;
                RichTextLayout::shape(
                    &mut text_renderer.font_system,
                    &runs,
                    base_bold,
                    size,
                    wrap_w,
                    align,
                    family,
                    underlines,
                    strikes,
                    line_mult,
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
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors emit_paragraph_slices below: one call site threading \
                  the shared pagination cursor, both layout caches, and the \
                  output vec through the single-paragraph pagination step"
    )]
    fn paginate_paragraph(
        &mut self,
        b: &BlockView,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        old_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        new_cache: &mut HashMap<String, (u64, RichTextLayout)>,
        laid: &mut Vec<Laid>,
        cur: &mut PageCursor,
    ) {
        let ps = self.resolve_paragraph_shape(b, fg, text_renderer, old_cache, cur);
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
            laid.push(Laid {
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
            });
            cur.y = item_top + block_h + f64::from(pl.space_after);
        } else {
            Self::emit_paragraph_slices(b, fg, &ps.layout, laid, cur, item_top, &pl);
        }
        new_cache.insert(b.id.clone(), (ps.key, ps.layout));
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
        self.region_laid.clear();
        for i in 0..num_pages {
            self.draw_page_sheet(
                scene,
                text_renderer,
                fg,
                &header,
                &footer,
                edit_region,
                i,
                sy,
            );
        }
        self.header = header;
        self.footer = footer;
        // The first page is the reference rect for the margin-guide overlay
        // (margins apply to every page identically).
        let page = Rect::new(
            fg.page_x,
            fg.page_top - sy,
            fg.page_x + fg.page_w,
            fg.page_top + fg.letter_h - sy,
        );
        self.page_rect = page;
        page
    }

    /// One page sheet (shadow + paper), its header/footer running bands (with
    /// the active edit-context band highlighted), and the default "Page N"
    /// indicator when there's no footer of its own.
    #[allow(clippy::too_many_arguments)]
    fn draw_page_sheet(
        &mut self,
        scene: &mut Scene,
        text_renderer: &mut TextRenderer,
        fg: &FrameGeometry,
        header: &[BlockView],
        footer: &[BlockView],
        edit_region: Region,
        i: usize,
        sy: f64,
    ) {
        let ptop = fg.page_top + num::f64_of_usize(i) * fg.page_pitch - sy;
        let sheet = Rect::new(fg.page_x, ptop, fg.page_x + fg.page_w, ptop + fg.letter_h);
        chrome::page::draw(
            scene,
            fg.ctf,
            sheet,
            self.chrome.paper,
            &self.chrome.paper_elevation,
        );
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
            header,
            page_no,
            head_band,
            false,
            head_editing,
        );
        let (drew_footer, foot_laid) = Self::draw_running_band(
            scene,
            fg.ctf,
            text_renderer,
            footer,
            page_no,
            foot_band,
            true,
            foot_editing,
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
            let label = t_args("page-indicator", &[arg("n", num::i64_of_usize(page_no))]);
            let approx = num::f64_of_usize(label.chars().count()) * 6.4;
            let lx = f64::midpoint(sheet.x0, sheet.x1) - approx / 2.0;
            let ly = sheet.y1 + PAGE_GAP / 2.0 - 6.0;
            // Drawn in the inter-page gap on the canvas — chrome, so themed.
            text_renderer.draw_text(
                scene,
                &label,
                11.0,
                self.chrome.text_dim,
                fg.ctf * Affine::translate((lx, ly)),
                None,
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
    #[allow(clippy::too_many_arguments)]
    fn draw_page_content(
        &mut self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        laid: &[Laid],
        new_cache: &HashMap<String, (u64, RichTextLayout)>,
        multi_sel: bool,
        spell_by_block: &HashMap<String, Vec<(usize, usize)>>,
        note_nums: &HashMap<String, usize>,
    ) -> Pass2Markers {
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
                Self::draw_laid_fidelity(scene, fg, text_renderer, l);
                continue;
            }

            if let Some(bul) = &l.bullet {
                text_renderer.draw_text(
                    scene,
                    bul,
                    l.size,
                    l.ink,
                    fg.ctf * Affine::translate((l.text_x - BULLET_GAP, l.y_top)),
                    None,
                );
            }

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
                spell_by_block,
                note_nums,
                &mut page_notes,
            );

            if self.focused && l.id == self.inner.caret.block && !self.inner.has_selection() {
                let byte = char_to_byte(&l.text, self.inner.caret.offset);
                let (cx, cy, ch) = layout.caret_geometry(byte);
                let caret = Rect::new(
                    l.text_x + f64::from(cx),
                    ly + f64::from(cy),
                    l.text_x + f64::from(cx) + CARET_WIDTH,
                    ly + f64::from(cy) + f64::from(ch),
                );
                scene.fill(Fill::NonZero, fg.ctf, PAPER_CARET, None, &caret);
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

    /// A table cell's tinted background (header row / zebra stripe / plain)
    /// + border, for one pass-2 item.
    fn draw_laid_cell_box(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        bx: Rect,
        cell_row: Option<usize>,
    ) {
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
        let (fh, fv) = self
            .blocks
            .iter()
            .find(|b| b.id == l.id)
            .map_or((false, false), |b| {
                (
                    matches!(b.attrs.get("flip_h"), Some(AttrVal::Bool(true))),
                    matches!(b.attrs.get("flip_v"), Some(AttrVal::Bool(true))),
                )
            });
        let itf = if fh || fv {
            let cx = f64::midpoint(rect.x0, rect.x1);
            let cy = f64::midpoint(rect.y0, rect.y1);
            fg.ctf
                * Affine::translate((cx, cy))
                * Affine::scale_non_uniform(
                    if fh { -1.0 } else { 1.0 },
                    if fv { -1.0 } else { 1.0 },
                )
                * Affine::translate((-cx, -cy))
        } else {
            fg.ctf
        };
        if let Some(hash) = &l.image_hash {
            if let Some(slot) = self.image_cache.get_mut(hash)
                && let Some(img) = slot
            {
                img.paint_at(scene, itf, rect);
            }
        } else {
            // Missing or undecodable (e.g. non-PNG) image — placeholder.
            scene.fill(
                Fill::NonZero,
                fg.ctf,
                Color::from_rgb8(0xee, 0xee, 0xf2),
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, 6.0),
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                fg.ctf,
                Color::from_rgb8(0xcc, 0xce, 0xd6),
                None,
                &vello::kurbo::RoundedRect::from_rect(rect, 6.0),
            );
            text_renderer.draw_text(
                scene,
                &t("canvas-image-missing"),
                14.0,
                PAPER_MUTED,
                fg.ctf * Affine::translate((rect.x0 + 12.0, rect.y0 + rect.height() / 2.0 - 8.0)),
                None,
            );
        }
        // Selection chrome: when this image is the caret's block, ring it
        // with an accent border + corner resize handles so it reads as
        // "selected" (the panel then shows the resize / flip controls).
        if l.id == self.inner.caret.block {
            let accent = self.chrome.accent;
            scene.stroke(&vello::kurbo::Stroke::new(2.0), fg.ctf, accent, None, &rect);
            let handle_r = 5.0;
            for (handle_x, handle_y) in [
                (rect.x0, rect.y0),
                (rect.x1, rect.y0),
                (rect.x0, rect.y1),
                (rect.x1, rect.y1),
            ] {
                let handle_rect = Rect::new(
                    handle_x - handle_r,
                    handle_y - handle_r,
                    handle_x + handle_r,
                    handle_y + handle_r,
                );
                scene.fill(Fill::NonZero, fg.ctf, Color::WHITE, None, &handle_rect);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.5),
                    fg.ctf,
                    accent,
                    None,
                    &handle_rect,
                );
            }
        }
    }

    /// One pass-2 "fidelity" (unrenderable embedded content) placeholder item.
    fn draw_laid_fidelity(
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        l: &Laid,
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
            Color::from_rgb8(0xee, 0xee, 0xf2),
            None,
            &vello::kurbo::RoundedRect::from_rect(box_rect, 4.0),
        );
        text_renderer.draw_text(
            scene,
            &t("canvas-embedded-content"),
            14.0,
            PAPER_MUTED,
            fg.ctf * Affine::translate((l.text_x + 10.0, l.y_top + 6.0)),
            None,
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
        if let Some((b0, b1)) = self.selection_bytes_for(&l.id, &l.text) {
            if let (Some(bx), true) = (l.cell_box, multi_sel) {
                // Multi-cell selection: highlight the whole cell (spreadsheet-style).
                scene.fill(Fill::NonZero, fg.ctf, PAPER_SELECTION, None, &bx);
            } else {
                for (rx0, ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, PAPER_SELECTION, None, &r);
                }
            }
        }

        // Find-match highlights (under the text).
        if self.find.is_open() {
            for (bid, cs, ce) in self.find.matches() {
                if bid == &l.id {
                    let bs = char_to_byte(&l.text, *cs);
                    let be = char_to_byte(&l.text, *ce);
                    for (rx0, ry0, rx1, ry1) in layout.selection_rects(bs, be) {
                        let r = Rect::new(
                            l.text_x + f64::from(rx0),
                            ly + f64::from(ry0),
                            l.text_x + f64::from(rx1),
                            ly + f64::from(ry1),
                        );
                        scene.fill(Fill::NonZero, fg.ctf, FIND_HL, None, &r);
                    }
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
                let rects = layout.selection_rects(bs, be);
                for &(rx0, ry0, rx1, ry1) in &rects {
                    let r = Rect::new(
                        l.text_x + f64::from(rx0),
                        ly + f64::from(ry0),
                        l.text_x + f64::from(rx1),
                        ly + f64::from(ry1),
                    );
                    scene.fill(Fill::NonZero, fg.ctf, COMMENT_HIGHLIGHT, None, &r);
                }
                // Anchor the marker at the start line's top, and remember
                // the range's right edge (max rect x1) for the inline tab.
                if let Some(&(_, ry0, _, _)) = rects.first() {
                    let right_x = rects.iter().map(|r| r.2).fold(f32::MIN, f32::max);
                    comment_markers.push((cid, ly + f64::from(ry0), l.text_x + f64::from(right_x)));
                }
            }
        }

        text_renderer.draw_rich(
            scene,
            layout,
            fg.ctf * Affine::translate((l.text_x, ly)),
            l.ink,
        );
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
                for (rx0, _ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
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
                for (rx0, ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
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
                let rects = layout.selection_rects(bs, be);
                if let Some(&(_, ry0, _, _)) = rects.first() {
                    let right_x = rects.iter().map(|r| r.2).fold(f32::MIN, f32::max);
                    change_markers.push((cid, ly + f64::from(ry0), l.text_x + f64::from(right_x)));
                }
            }
        }
    }

    /// Spell-check squiggles + note-reference superscripts over one pass-2
    /// item's text, recording `page_notes` for the footnote-area pass.
    #[allow(clippy::too_many_arguments)]
    fn draw_laid_notes_and_spellcheck(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        text_renderer: &mut TextRenderer,
        item: &LaidItemCtx,
        spell_by_block: &HashMap<String, Vec<(usize, usize)>>,
        note_nums: &HashMap<String, usize>,
        page_notes: &mut HashMap<usize, Vec<usize>>,
    ) {
        let (l, layout, ly) = (item.l, item.layout, item.ly);
        // Spell-check squiggles: a wavy red underline under each misspelled
        // span. The byte span maps to one-or-more on-screen rects via the same
        // `selection_rects` geometry the selection highlight uses (so it tracks
        // wrapping). The squiggle hugs just under each rect's bottom.
        if let Some(spans) = spell_by_block.get(&l.id) {
            for &(b0, b1) in spans {
                for (rx0, _ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
                    let y = ly + f64::from(ry1) - 1.0;
                    draw_squiggle(
                        scene,
                        fg.ctf,
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
                let bs = char_to_byte(&l.text, ci);
                let be = char_to_byte(&l.text, ci + 1);
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
                text_renderer.draw_text(
                    scene,
                    &label,
                    sup_size,
                    l.ink,
                    fg.ctf
                        * Affine::translate((l.text_x + f64::from(mx), ly + f64::from(my) - 1.0)),
                    None,
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
                if n.kind == qdoc::schema::NOTE_KIND_FOOTNOTE
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
                let line_h = (f64::from(NOTE_FONT) * 1.4).ceil();
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
                        text_renderer.draw_text(
                            scene,
                            &line,
                            NOTE_FONT,
                            NOTE_INK,
                            fg.ctf * Affine::translate((left, ny)),
                            None,
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
            let placeholder = t("canvas-empty-placeholder");
            text_renderer.draw_text(
                scene,
                &placeholder,
                16.5,
                PAPER_MUTED,
                ctf * Affine::translate((l.text_x, l.y_top)),
                None,
            );
        }
    }

    /// Header/footer edit mode: veil the body (Word dims it while editing a
    /// running head) on every page, then draw the band caret on top so it
    /// stays crisp through the veil.
    fn draw_running_region_veil(
        &self,
        scene: &mut Scene,
        fg: &FrameGeometry,
        num_pages: usize,
        sy: f64,
    ) {
        if self.inner.in_running_region() {
            for i in 0..num_pages {
                let ptop = fg.page_top + num::f64_of_usize(i) * fg.page_pitch - sy;
                let body_top = ptop + fg.m.top;
                let body_bot = ptop + fg.letter_h - fg.m.bottom;
                let veil = Rect::new(
                    fg.page_x + fg.m.left,
                    body_top,
                    fg.page_x + fg.page_w - fg.m.right,
                    body_bot,
                );
                scene.fill(Fill::NonZero, fg.ctf, BODY_DIM_VEIL, None, &veil);
            }
            self.draw_band_caret(scene, fg.ctf);
        }
    }

    /// The scrollbar thumb (right edge), shown only when the content
    /// overflows the canvas.
    fn draw_scrollbar(&self, scene: &mut Scene, transform: Affine, canvas: Rect) {
        if self.max_scroll > 0.0 {
            let track = canvas.height();
            let thumb_h = (track * track / (track + self.max_scroll)).max(28.0);
            let frac = self.scroll_y / self.max_scroll;
            let thumb_y = canvas.y0 + frac * (track - thumb_h);
            let thumb = Rect::new(
                canvas.x1 - 8.0,
                thumb_y + 2.0,
                canvas.x1 - 4.0,
                thumb_y + thumb_h - 2.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                self.chrome.scrollbar,
                None,
                &vello::kurbo::RoundedRect::from_rect(thumb, 2.0),
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
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let (remote_carets, remote_selections) = self.remote_presence_overlays(presence);
        // A peer's just-typed run glows in their colour, fading; drawn first so it
        // sits under the selection and caret rather than competing with them.
        for (user, spans) in self.remote_typing_overlays(presence, std::time::Instant::now()) {
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
        let now = std::time::Instant::now();
        for interaction in presence.interactions_for_resource("word-processor.document", now) {
            let ranges = match &interaction.anchor {
                qos_actions::InteractionAnchor::Text { ranges, .. } => ranges
                    .iter()
                    .map(|range| {
                        (
                            range.block.as_str(),
                            usize::try_from(range.start).unwrap_or(usize::MAX),
                            usize::try_from(range.end).unwrap_or(usize::MAX),
                        )
                    })
                    .collect::<Vec<_>>(),
                qos_actions::InteractionAnchor::Objects { ids } => ids
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
            let mut rects = Vec::new();
            for (block, start, end) in ranges {
                let Some(l) = self.laid.iter().find(|laid| laid.id == block) else {
                    continue;
                };
                let Some((_, layout)) = self.layout_cache.get(block) else {
                    continue;
                };
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
            let user = UserId(interaction.actor.id.clone());
            for rect in &rects {
                presence.draw_peer_selection(scene, theme, ctf, &user, *rect);
            }
            if let Some(bounds) = rects
                .iter()
                .copied()
                .reduce(|bounds, rect| bounds.union(rect))
            {
                let label = presence.display_name(&user);
                presence.draw_peer_outline_labeled(
                    scene,
                    theme,
                    text_renderer,
                    ctf,
                    &user,
                    &label,
                    bounds,
                    3.0,
                );
            }
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
    /// Route input to the open version-history strip when it should capture
    /// this event (pointer inside its band, mid-drag, or its name field wants
    /// the keyboard). A restore/rename still lands its document-side effects
    /// even when the strip doesn't claim the whole event. Returns whether the
    /// strip fully handled it (the caller should stop dispatching further).
    fn handle_history_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if !self.history.is_open() {
            return false;
        }
        let at = match event {
            WindowEvent::CursorMoved { position, .. } => {
                Point::new(position.x / scale_factor, position.y / scale_factor)
            }
            _ => self.pointer,
        };
        // The strip also takes the keyboard outright while its name field is
        // up: a name that could only be typed with the pointer held still
        // over the band would be unusable.
        if !(self.history.band().contains(at)
            || self.history.is_dragging()
            || self.history.wants_keyboard())
        {
            return false;
        }
        let outcome = self
            .history
            .handle_event(event, scale_factor, &modifiers, state, path);
        if outcome.restored {
            self.needs_commit = true;
            self.history.follow_document();
            self.refresh_blocks(state);
        }
        if outcome.named {
            // Naming a version is a document write; it commits with the
            // same author and timestamp as an edit, and lands on the undo
            // stack so it can be taken back the usual way.
            self.needs_commit = true;
        }
        outcome.handled
    }

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
        state: &mut AutoCommit,
        path: &std::path::Path,
        handled: &mut bool,
    ) {
        if self.rect.contains(self.pointer) {
            let tb_h = self.toolbar_height(self.rect.width());
            let over_body = self.pointer.y >= self.rect.y0 + tb_h
                && self.pointer.y <= self.rect.y1 - LAYOUT_BAR_H
                && !(self.styles_panel_open && self.panel_rect.contains(self.pointer))
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
            self.dirty = true;
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
        // Toolbar hover feedback — repaint only when the hovered button changes.
        let new_hover = if self.pointer.y < self.rect.y0 + tb_h {
            self.toolbar
                .iter()
                .find(|(_, r)| r.contains(self.pointer))
                .map(|(_, r)| *r)
        } else {
            None
        };
        if new_hover != self.hover_tool {
            self.hover_tool = new_hover;
            self.dirty = true;
            *handled = true;
        }
        if let Some(sc) = self.scrub {
            // Drag-to-scrub a numeric style field (live preview for every
            // block of that style).
            let dx = self.pointer.x - sc.start_x;
            if dx.abs() > 3.0 {
                if let Some(s) = self.scrub.as_mut() {
                    s.moved = true;
                }
                let v = sc
                    .field
                    .clamp(sc.start_val + num::f32_of_f64(dx) * sc.field.scrub_factor());
                if let Some(key) = self.focused_style_key()
                    && let Some(st) = self.sheet.styles.get_mut(&key)
                {
                    sc.field.set(st, v);
                }
                self.dirty = true;
            }
            *handled = true;
        } else if self.ruler.is_dragging() {
            // Live-drag a ruler marker (margin / indent / tab). Updates the
            // value for an instant preview; persisted on release.
            self.ruler_drag_move();
            self.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag {
            // Live-drag a layout guide (margin / border / spacing). Guides
            // live in unscaled page space, so map the pointer through the
            // inverse zoom first.
            self.apply_guide_drag(gd, self.pointer_to_content(self.pointer));
            self.dirty = true; // repaint every move so the preview is live
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
        self.focused = true;
        // A right-click on a style chip in the styles panel opens the
        // style-management menu (Delete / Duplicate) before the generic
        // "panel is chrome, ignore" rule kicks in.
        if self.styles_panel_open && self.panel_rect.contains(self.pointer) {
            if let Some(id) = self.style_chip_at(self.pointer) {
                self.style_menu.open(&id, self.pointer);
                self.dirty = true;
                *handled = true;
            }
            return;
        }
        let in_chrome = self.pointer.y < self.rect.y0 + tb_h
            || self.pointer.y > self.rect.y1 - LAYOUT_BAR_H
            || (self.styles_panel_open && self.panel_rect.contains(self.pointer))
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
        state: &mut AutoCommit,
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
            self.dirty = true;
        }
        if !self.rect.contains(self.pointer) {
            return;
        }
        self.focused = true;
        if self.handle_left_press_recovery_and_panels(state, handled) {
            return;
        }
        if self.handle_left_press_find_ruler_toolbar(state, tb_h, handled) {
            return;
        }
        self.handle_left_press_body(state, shift, ctrl, handled);
    }

    /// While the context / "New ▾" template / paragraph-style menu is open,
    /// route the left press to it: a row dispatches its action, a click
    /// outside dismisses it. Either way the click is consumed so it never
    /// reaches the page beneath. Returns whether a menu claimed the press.
    fn handle_left_press_menu_routing(
        &mut self,
        state: &mut AutoCommit,
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
            self.dirty = true;
            *handled = true;
            return true;
        }
        // Likewise route the press to the "New ▾" template picker when open.
        if self.template_picker.is_open() {
            if let Some(template) = self.template_picker.click(self.pointer) {
                self.apply_template(state, template);
            }
            self.dirty = true;
            *handled = true;
            return true;
        }
        // The paragraph-style menu (Delete / Duplicate) captures the press
        // the same way: a row runs its action, an outside click dismisses.
        if self.style_menu.is_open() {
            if let Some(action) = self.style_menu.click(self.pointer) {
                self.dispatch_style_menu(action, state);
            }
            self.dirty = true;
            *handled = true;
            return true;
        }
        false
    }

    /// The crash-recovery banner's Restore/Discard, a pending numeric-field
    /// commit, the styles panel, and the outline pane — in that priority
    /// order. Returns whether one of them claimed the press.
    fn handle_left_press_recovery_and_panels(
        &mut self,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        // Crash-recovery banner (just under the toolbar): a click on
        // Restore / Discard resolves the offer before any other hit-test.
        if let Some(decision) = self.recovery.handle_click(self.pointer) {
            if let RecoveryDecision::Restore(markdown) = decision
                && self.inner.replace_with_markdown(state, &markdown)
            {
                self.refresh_blocks(state);
                self.layout_cache.clear();
                self.spell_cache.clear();
                self.modified = true;
                self.needs_commit = true;
            }
            self.dirty = true;
            *handled = true;
            return true;
        }
        // A press anywhere commits a pending typed numeric edit.
        if self.editing_field.is_some() {
            self.commit_num_edit(state);
        }
        // A narrow-window inspector is a modal-ish rail: clicking the dimmed
        // document dismisses it instead of placing a caret through the overlay.
        if self.styles_panel_overlay() && !self.panel_rect.contains(self.pointer) {
            self.styles_panel_open = false;
            self.dirty = true;
            *handled = true;
            return true;
        }
        // Styles panel (right side): dispatch a click on its controls.
        if self.styles_panel_open && self.panel_rect.contains(self.pointer) {
            if let Some(hit) = self
                .panel_hits
                .iter()
                .rev()
                .find(|(r, _)| r.contains(self.pointer))
                .map(|(_, h)| h.clone())
            {
                self.apply_panel_hit(state, hit);
            }
            self.dirty = true;
            *handled = true;
            return true;
        }
        // Outline / navigation pane (left side): a click on an entry
        // scrolls that heading into view.
        if self.outline.contains(self.pointer) {
            if let Some(block_id) = self.outline.heading_at(self.pointer).map(str::to_owned) {
                self.scroll_to_block(&block_id);
            }
            self.dirty = true;
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
        state: &mut AutoCommit,
        tb_h: f64,
        handled: &mut bool,
    ) -> bool {
        // Find bar (when open): dispatch clicks on its fields + controls.
        // The recovery banner (when shown) sits above the find bar, so the
        // find band starts below it.
        if self.find.is_open() {
            let recovery_h = self.recovery.height();
            let fbar_top = self.rect.y0 + tb_h + recovery_h;
            let fbar_bot = fbar_top + crate::find_bar::HEIGHT;
            if self.pointer.y >= fbar_top && self.pointer.y < fbar_bot {
                let effect = self.find.click(self.pointer, &self.blocks);
                self.apply_find_effect_mut(state, effect);
                self.dirty = true;
                *handled = true;
                return true;
            }
        }
        // Ruler band (when on): grab a marker to drag, else a click in the
        // page span adds a tab stop on the current paragraph.
        if self.ruler_on && self.ruler.rect().contains(self.pointer) {
            self.ruler_press(state);
            self.dirty = true;
            *handled = true;
            return true;
        }
        if self.pointer.y < self.rect.y0 + tb_h {
            // The right-aligned zoom component owns its hit targets and scale.
            if self.zoom.click(self.pointer) {
                self.auto_fit_page = false;
                self.dirty = true;
                *handled = true;
                return true;
            }
            // Reverse so the colour swatches (pushed after the chrome
            // buttons) win a hit-test on any overlap at a crowded edge.
            let clicked = self
                .toolbar
                .iter()
                .rev()
                .find(|(_, r)| r.contains(self.pointer))
                .map(|(a, _)| *a);
            if let Some(action) = clicked {
                self.apply_tool(state, action);
                self.refresh_blocks(state);
                self.dirty = true;
                // View toggles (panels) + opening the New menu are not undo
                // points; everything else (including TOC generation) is.
                if !matches!(
                    action,
                    ToolAction::ToggleStyles
                        | ToolAction::ToggleRuler
                        | ToolAction::ToggleOutline
                        | ToolAction::ToggleHistory
                        | ToolAction::NewMenu
                ) {
                    self.needs_commit = true;
                    self.modified = true;
                    self.note_edit_for_autosave(state);
                }
            }
            *handled = true;
            return true;
        }
        // Bottom layout bar sits at the very bottom; document status floats above it.
        let lbar_top = self.rect.y1 - LAYOUT_BAR_H;
        if self.pointer.y >= lbar_top {
            if let Some(mode) = self
                .layout_chips
                .iter()
                .find(|(_, r)| r.contains(self.pointer))
                .map(|(m, _)| *m)
            {
                self.toggle_layout_mode(mode);
            }
            self.dirty = true;
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
        state: &mut AutoCommit,
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
        if let Some(action) = self.review.action_at(cp) {
            self.dispatch_review_action(action, state);
            self.dirty = true;
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
            self.dirty = true;
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
                self.dirty = true;
                self.needs_commit = true;
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
        self.dirty = true;
        *handled = true;
    }

    /// Left release: persist whichever live drag was active (style-field
    /// scrub, ruler marker, or layout guide) into the document, or — with no
    /// drag active — end a text-selection drag.
    fn handle_left_release(&mut self, state: &mut AutoCommit, handled: &mut bool) {
        if let Some(sc) = self.scrub.take() {
            if sc.moved {
                // A scrub persists the style edit (every instance follows).
                let sheet = self.sheet.clone();
                self.inner.set_stylesheet(state, &sheet);
                self.needs_commit = true;
            } else {
                // A plain click (no drag) focuses the field for typing.
                self.editing_field = Some(sc.field);
                let cur = self
                    .focused_style_key()
                    .map_or(0.0, |k| sc.field.get(&self.sheet.get(&k)));
                self.edit_buf = fmt_num(cur);
            }
            self.dirty = true;
            *handled = true;
        } else if self.ruler.is_dragging() {
            // A ruler-marker drag persists on release (margins → page meta,
            // indents / tab stops → the caret block's attrs).
            self.ruler_drag_release(state);
            self.dirty = true;
            *handled = true;
        } else if let Some(gd) = self.guide_drag.take() {
            // A layout edit persists into the document on release: spacing
            // into the stylesheet, margins into the page-setup meta.
            match gd.mode {
                LayoutMode::Spacing => {
                    let sheet = self.sheet.clone();
                    self.inner.set_stylesheet(state, &sheet);
                    self.needs_commit = true;
                }
                LayoutMode::Margins => {
                    self.persist_margins(state);
                    self.needs_commit = true;
                }
                LayoutMode::None => {}
            }
            self.dirty = true;
            *handled = true;
        } else if self.drag.release() {
            self.dirty = true;
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
        state: &mut AutoCommit,
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
        let replacement = spellcheck::probable_correction(&self.spell_language, &original)?;
        let block = self.inner.caret.block.clone();
        self.inner
            .splice_block(state, &block, start, end, &replacement);
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

    fn undo_document_autocorrection(&mut self, state: &mut AutoCommit) -> bool {
        let Some(mut correction) = self.last_autocorrection.clone() else {
            return false;
        };
        // Enter commits a word by splitting the block. Immediate Backspace
        // first rejoins that empty/new block, then restores the spelling.
        if self.inner.caret.block != correction.block && self.inner.caret.offset == 0 {
            self.inner.backspace(state);
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
        self.inner.splice_block(
            state,
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

    #[allow(
        clippy::too_many_lines,
        reason = "the guards are already split into handle_key_press_guards; what is \
                  left is one match whose arms are the key dispatch table, and a table \
                  reads better whole than cut in half"
    )]
    fn handle_key_press(
        &mut self,
        state: &mut AutoCommit,
        logical_key: &Key,
        text: Option<&str>,
        shift: bool,
        ctrl: bool,
        handled: &mut bool,
    ) {
        if self.handle_key_press_guards(state, logical_key, text, ctrl, handled) {
            return;
        }
        if ctrl && let Key::Character(s) = logical_key {
            self.handle_ctrl_shortcut(state, s, shift, handled);
            return;
        }
        if !matches!(logical_key, Key::Named(NamedKey::Backspace)) {
            self.last_autocorrection = None;
        }
        let mut changed = true;
        let mut motion = false;
        match logical_key {
            Key::Named(NamedKey::Tab) => {
                // Inside a table, Tab/Shift-Tab walk cells (and Tab in the
                // last cell appends a row). Otherwise, when the paragraph has
                // ruler tab stops, Tab advances to the next one (recording a
                // tab char); with no stops it falls back to inserting spaces.
                if self.inner.tab_cell(state, !shift) {
                    motion = true; // tab_cell already cleared any selection
                } else if self.inner.tab_advance(state) {
                    // Caret advanced to the next tab stop.
                } else {
                    self.inner.insert_str(state, "    ");
                }
            }
            Key::Named(NamedKey::Enter) => {
                let correction = self.autocorrect_before_boundary(state);
                self.inner.enter(state);
                self.last_autocorrection = correction;
            }
            Key::Named(NamedKey::Backspace) => {
                if !self.undo_document_autocorrection(state) {
                    self.inner.backspace(state);
                }
            }
            Key::Named(NamedKey::Delete) => self.inner.delete_forward(state),
            Key::Named(NamedKey::ArrowLeft) => {
                self.extend_or_clear(shift);
                self.inner.move_left(state);
                motion = true;
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.extend_or_clear(shift);
                self.inner.move_right(state);
                motion = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.extend_or_clear(shift);
                // Inside a table, Up moves to the cell directly above; the
                // grid is block-granular so this matches block-relative motion.
                if !self.inner.cell_vertical(state, false) {
                    self.inner.move_block_relative(state, -1);
                }
                motion = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.extend_or_clear(shift);
                if !self.inner.cell_vertical(state, true) {
                    self.inner.move_block_relative(state, 1);
                }
                motion = true;
            }
            Key::Named(NamedKey::Home) => {
                self.extend_or_clear(shift);
                self.inner.move_home();
                motion = true;
            }
            Key::Named(NamedKey::End) => {
                self.extend_or_clear(shift);
                self.inner.move_end(state);
                motion = true;
            }
            Key::Named(NamedKey::Space) => {
                let correction = self.autocorrect_before_boundary(state);
                if !self.inner.autoformat_on_space(state) {
                    self.inner.insert_str(state, " ");
                }
                self.last_autocorrection = correction.map(|mut correction| {
                    correction.boundary_end = self.inner.caret.offset;
                    correction
                });
            }
            _ => {
                let typed: String = text
                    .map(|t| t.chars().filter(|c| !c.is_control()).collect())
                    .unwrap_or_default();
                if typed.is_empty() {
                    changed = false;
                } else {
                    let correction = typed
                        .chars()
                        .next()
                        .filter(|character| character.is_ascii_punctuation() && *character != '\'')
                        .and_then(|_| self.autocorrect_before_boundary(state));
                    self.inner.insert_str(state, &typed);
                    self.last_autocorrection = correction.map(|mut correction| {
                        correction.boundary_end = self.inner.caret.offset;
                        correction
                    });
                }
            }
        }
        if changed {
            if !motion {
                // editing consumes/collapses any selection
                self.inner.clear_anchor();
                // A real content edit (not caret motion) advances the
                // autosave debounce — may write a recovery copy.
                self.modified = true;
                self.note_edit_for_autosave(state);
            }
            self.refresh_blocks(state);
            self.dirty = true;
            self.needs_commit = true;
            *handled = true;
        }
    }

    /// Early exits before a key press reaches editing: an unfocused widget,
    /// Escape (closing an open menu, or exiting header/footer editing), a
    /// numeric-field edit in progress, the Find bar capturing non-Ctrl keys,
    /// and Ctrl+Enter's manual page break. Returns whether one of these fully
    /// handled the event (the caller should stop dispatching further).
    fn handle_key_press_guards(
        &mut self,
        state: &mut AutoCommit,
        logical_key: &Key,
        text: Option<&str>,
        ctrl: bool,
        handled: &mut bool,
    ) -> bool {
        if !self.focused {
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
            self.dirty = true;
            *handled = true;
            return true;
        }
        // Escape exits header/footer editing, returning to the body.
        if self.inner.in_running_region() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.inner.focus_body(state);
            self.refresh_blocks(state);
            self.dirty = true;
            *handled = true;
            return true;
        }
        // While typing into a numeric style field, capture digits + editing
        // keys (Enter commits, Esc cancels, Backspace deletes).
        if self.editing_field.is_some() {
            match logical_key {
                Key::Named(NamedKey::Enter) => self.commit_num_edit(state),
                Key::Named(NamedKey::Escape) => {
                    self.editing_field = None;
                    self.edit_buf.clear();
                }
                Key::Named(NamedKey::Backspace) => {
                    self.edit_buf.pop();
                    self.apply_edit_buf_live();
                }
                _ => {
                    let typed: String = text
                        .map(|t| {
                            t.chars()
                                .filter(|c| c.is_ascii_digit() || *c == '.')
                                .collect()
                        })
                        .unwrap_or_default();
                    for ch in typed.chars() {
                        if ch == '.' && self.edit_buf.contains('.') {
                            continue;
                        }
                        self.edit_buf.push(ch);
                    }
                    self.apply_edit_buf_live();
                }
            }
            self.dirty = true;
            *handled = true;
            return true;
        }
        // Ctrl+Enter inserts a manual page break: the caret block splits
        // and the new block is marked to start on a fresh page. (Handled
        // here because `NamedKey::Enter` is not a `Key::Character`.)
        if ctrl && matches!(logical_key, Key::Named(NamedKey::Enter)) {
            self.inner.insert_page_break(state);
            self.refresh_blocks(state);
            self.dirty = true;
            self.needs_commit = true;
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
        state: &mut AutoCommit,
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
            if shift {
                self.request_save_as();
            } else {
                self.save(state);
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        // Ctrl+O opens a document through the host portal, in
        // place of the current one.
        if s.eq_ignore_ascii_case("o") {
            self.request_open_document();
            self.dirty = true;
            *handled = true;
            return;
        }
        if s.eq_ignore_ascii_case("f") {
            if self.find.is_open() {
                self.close_find();
            } else {
                self.open_find(state);
            }
            self.dirty = true;
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
            self.inner.insert_page_number(state);
            self.refresh_blocks(state);
            self.dirty = true;
            self.needs_commit = true;
            self.modified = true;
            self.note_edit_for_autosave(state);
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
            // `WordProcessorAction` the toolbar + Ctrl+K palette
            // do (one replayable source of truth).
            "b" => self.dispatch_toggle_mark(state, "strong"),
            "i" => self.dispatch_toggle_mark(state, "em"),
            "u" => self.dispatch_toggle_mark(state, "underline"),
            // Ctrl+. → superscript, Ctrl+, → subscript (mutually
            // exclusive; matches common word-processor bindings).
            "." | ">" => self.dispatch_exclusive_mark(state, "super", "sub"),
            "," | "<" => self.dispatch_exclusive_mark(state, "sub", "super"),
            "c" => self.clipboard_copy(state),
            "x" => {
                self.clipboard_copy(state);
                self.inner.delete_selection(state);
            }
            // Ctrl+V keeps formatting; Ctrl+Shift+V pastes plain text.
            "v" if shift => self.clipboard_paste_plain(state),
            "v" => self.clipboard_paste(state),
            "a" => self.inner.select_all(state),
            // Ctrl+1/2/3 → heading levels. (Ctrl+0 is zoom-reset;
            // Body paragraph is reachable via the toolbar / panel.)
            "1" => self.dispatch_paragraph_style(state, "heading", Some(1), None),
            "2" => self.dispatch_paragraph_style(state, "heading", Some(2), None),
            "3" => self.dispatch_paragraph_style(state, "heading", Some(3), None),
            // z / y (undo/redo) are handled by the host App.
            _ => {}
        }
        self.refresh_blocks(state);
        self.dirty = true;
        self.needs_commit = true;
        self.modified = true;
        self.note_edit_for_autosave(state);
        *handled = true;
    }
}

impl Widget for WordProcessorWidget {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::WORD_PROCESSOR_OPERATION_TYPE]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        use crate::actions::WordProcessorOperation;

        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            WordProcessorOperation::Open { .. }
            | WordProcessorOperation::Save { .. }
            | WordProcessorOperation::SaveAs { .. } => {
                return Err("file operations require the lifecycle-capable operation host".into());
            }
            WordProcessorOperation::Find => self.open_find(doc),
            WordProcessorOperation::ZoomIn => {
                self.zoom_by(0.1);
            }
            WordProcessorOperation::ZoomOut => {
                self.zoom_by(-0.1);
            }
            WordProcessorOperation::ToggleSpellcheck => self.toggle_spellcheck(),
            WordProcessorOperation::ToggleMarkup => self.toggle_markup(),
            WordProcessorOperation::ToggleStyles => {
                self.styles_panel_open = !self.styles_panel_open;
            }
            WordProcessorOperation::ToggleRuler => self.ruler_on = !self.ruler_on,
            WordProcessorOperation::ToggleOutline => self.outline.toggle(),
            WordProcessorOperation::ToggleHistory => self.history.toggle(),
        }
        self.dirty = true;
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        widget_path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use crate::actions::WordProcessorOperation;

        if kind == qos_actions::OperationKind::Document {
            return Ok(None);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        let (destination, force_picker) = match operation {
            WordProcessorOperation::Open { path } => {
                if self.portal_rx.is_some() || self.pending_resource_open.is_some() {
                    return Err("another file operation is already running".into());
                }
                if let Some(path) = path {
                    self.pending_resource_open = Some((invocation, std::path::PathBuf::from(path)));
                } else {
                    self.request_open_document();
                    self.resource_file_invocation = Some(invocation);
                }
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            WordProcessorOperation::Save { path } => (path, false),
            WordProcessorOperation::SaveAs { path } => (path, true),
            other => {
                let encoded = serde_json::to_vec(&other).map_err(|error| error.to_string())?;
                return self
                    .invoke_resource_operation_v2(kind, type_id, &encoded, doc, widget_path)
                    .map(|result| result.map(ui::widget::ResourceOperationStart::Completed));
            }
        };

        if self.portal_rx.is_some() || self.pending_resource_open.is_some() {
            return Err("another file operation is already running".into());
        }
        if let Some(destination) = destination {
            let destination = std::path::PathBuf::from(destination);
            let format = WpFormat::from_path(&destination);
            self.set_file(destination, format);
            self.try_write_to_current_path(doc)?;
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                qos_actions::ActionOutput::default(),
            )))
        } else if self.path.is_some() && !force_picker {
            self.try_write_to_current_path(doc)?;
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                qos_actions::ActionOutput::default(),
            )))
        } else {
            self.request_save_as();
            self.resource_file_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        if self
            .resource_file_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_file_result.take()
        } else {
            None
        }
    }

    /// The document being edited is a record; its identity is its path. Two
    /// editors on two different files in one room announce two different
    /// records, so they are never handed each other's text — "share this
    /// document" means the record, not merely "share this app instance".
    ///
    /// An untitled document names no record until it is saved and has a path.
    fn record_intent(&self) -> Option<ui::RecordCheckout> {
        self.path.as_ref().map(|p| {
            ui::RecordCheckout::new(ui::RecordId::for_file(p), ui::RecordKind::new("document"))
        })
    }

    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.inner.bind(state, root);
        if let Some((invocation, open_path)) = self.pending_resource_open.take() {
            self.resource_file_result =
                Some(match self.try_open_document_in_place(state, &open_path) {
                    Ok(()) => qos_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: qos_actions::ActionOutput::default(),
                    },
                    Err(message) => qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "word_processor.open_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    },
                });
        }
        // Fold in a completed host portal request (open / save / image insert),
        // if the portal worker finished.
        self.drain_portal(state);
        self.refresh_blocks(state);
        // Reload the stylesheet from the doc unless a live, not-yet-persisted
        // local edit is in progress (a guide drag, a numeric scrub, or typing
        // into a panel field) — reloading would discard the live preview.
        if self.guide_drag.is_none() && self.scrub.is_none() && self.editing_field.is_none() {
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
        // Re-derive the version-history track when the change graph has moved.
        // Cheap when it hasn't: `refresh` compares heads first.
        if self.history.is_open() {
            self.history.refresh(state);
        }
        self.dirty = true;
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see the
        // `TaffyTreeExt` doc comment in `ui::widget::mod`), and
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so there is nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn needs_animation_frame(&self) -> bool {
        // Status is persistent bottom chrome, so it has no independent fade
        // lifecycle to animate while the editor is otherwise idle.
        false
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
        // Resolve the OS theme into this frame's chrome palette once; every
        // chrome helper below reads `self.chrome` so the whole editor follows
        // light/dark mode + the OS palette.
        self.chrome = ChromeColors::for_theme(theme);
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
            &laid,
            &new_cache,
            multi_sel,
            &spell_by_block,
            &note_nums,
        );
        self.review.draw(
            scene,
            text_renderer,
            fg.canvas,
            fg.ctf,
            markers.comment_markers,
            markers.change_markers,
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

        scene.pop_layer();

        self.draw_scrollbar(scene, transform, fg.canvas);

        self.laid = laid;
        // Adopt this frame's cache (dropping the pruned entries for blocks that
        // no longer exist) so the next frame can reuse it.
        self.layout_cache = new_cache;

        self.draw_remote_overlays(scene, fg.ctf, text_renderer, theme, presence);

        // Contextual styles panel on the right (between toolbar and layout bar).
        if self.styles_panel_visible() {
            let overlay = self.styles_panel_overlay();
            if overlay {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(0, 0, 0, 90),
                    None,
                    &Rect::new(full.x0, full.y0 + fg.tb_h, full.x1, fg.layout_bar_top),
                );
            }
            let panel = Rect::new(
                if overlay {
                    (full.x1 - STYLES_PANEL_W).max(full.x0)
                } else {
                    fg.canvas_right
                },
                full.y0 + fg.tb_h,
                full.x1,
                fg.layout_bar_top,
            );
            self.draw_styles_panel(scene, transform, text_renderer, panel);
        } else {
            self.panel_rect = Rect::ZERO;
            self.panel_hits.clear();
        }

        // Document outline / navigation pane on the left (between toolbar and
        // layout bar). Drawn after `self.laid` is set so the active-heading
        // highlight reflects this frame's pagination.
        if self.outline.is_open() {
            let outline_band =
                Rect::new(full.x0, full.y0 + fg.tb_h, fg.canvas_left, fg.history_top);
            let entries = self.outline_entries();
            let active = self.active_outline_block();
            let cc = self.chrome;
            self.outline.draw(
                scene,
                transform,
                text_renderer,
                outline_band,
                &entries,
                active.as_deref(),
                OutlinePalette {
                    background: cc.bar,
                    border: cc.border,
                    chip: cc.chip,
                    text: cc.text,
                    text_dim: cc.text_dim,
                },
            );
        } else {
            self.outline.clear_frame();
        }

        if fg.history_h > 0.0 {
            let band = Rect::new(full.x0, fg.history_top, full.x1, fg.layout_bar_top);
            self.history
                .draw(scene, transform, text_renderer, theme, presence, band);
        }

        let lbar = Rect::new(full.x0, fg.layout_bar_top, full.x1, fg.content_bottom);
        self.draw_layout_bar(scene, transform, text_renderer, theme, lbar);
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
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key();
        // While the colour-picker popover is open it captures input (modal-ish):
        // route drags to it, apply committed colours, and let an outside-click /
        // Escape dismiss it before any page/panel hit-testing.
        if self.color_picker.is_open()
            && self.pump_color_picker(event, scale_factor, *modifiers, state, path, handled)
        {
            return;
        }
        // The version-history strip captures pointer input over its own band (and
        // keeps capturing mid-drag). A restore flags `needs_commit`, handing the
        // commit to `App::commit_action` so it lands on the undo stack stamped and
        // attributed like a typed edit.
        if self.handle_history_event(event, scale_factor, *modifiers, state, path) {
            *handled = true;
            return;
        }
        // The toolbar may have wrapped to several rows; hit-test against its
        // actual bottom (`tb_h`), matching `draw`.
        let tb_h = self.toolbar_height(self.rect.width());

        // Find/replace embeds the shared TextInput rather than maintaining a
        // second text editor. Feed it raw events first so IME, selection and
        // clipboard shortcuts retain the toolkit's exact behavior. Ctrl+F is
        // deliberately left for the editor-level toggle below.
        if self.focused && self.find.is_open() {
            let mut find_handled = false;
            let effect = self.find.handle_event(
                event,
                scale_factor,
                *modifiers,
                state,
                path,
                &mut find_handled,
                focus_target,
                node_id,
                taffy,
                &self.blocks,
            );
            self.apply_find_effect_mut(state, effect);
            if find_handled {
                self.dirty = true;
                *handled = true;
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
            WindowEvent::DroppedFile(path) => self.handle_dropped_file(state, path, handled),
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, scale_factor, ctrl, handled);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y, scale_factor, tb_h, handled);
            }
            // Right (secondary) press opens the editing / spell-check context
            // menu on the page body. Toolbar / panel / status-bar chrome ignore
            // it (they have their own hit-testing on the left button).
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self.handle_right_press(state, tb_h, handled),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_left_press(state, tb_h, shift, ctrl, handled),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_left_release(state, handled),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => self.handle_key_press(state, logical_key, text.as_deref(), shift, ctrl, handled),
            _ => {}
        }
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
        // Toast first, so an open menu stacks above it.
        self.draw_toast(scene, transform, text_renderer, theme);
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
        self.template_picker
            .draw(scene, transform, text_renderer, theme, presence);
        self.style_menu
            .draw(scene, transform, text_renderer, theme, presence);
        if self.find.is_open() {
            self.find
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        let cc = self.chrome;
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
            cc.bar,
            cc.border,
        );
    }

    /// Outbound collaboration presence: while focused, broadcast the local caret
    /// (or selection range) keyed to its block. Polled by the App after each event
    /// and fanned out as `CollabEvent::RemotePresenceUpdate`. Offsets are bytes, so
    /// the receiver feeds them straight into `RichTextLayout::caret_geometry`/
    /// `selection_rects`.
    fn presence_payload(&self) -> Option<(PresenceKey, ui::PresencePayload)> {
        if !self.focused {
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

    fn name(&self) -> &'static str {
        "WordProcessor"
    }

    /// The word processor's local accessibility semantics: the editable
    /// [`Role::Document`] root. Blocks render directly, not as child widgets, so
    /// the full subtree comes from [`WordProcessorWidget::accessibility_tree`]
    /// rather than `children()`; convert the carrier into real synthetic
    /// children so AccessKit, AT-SPI and the built-in reader all receive it.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut tree = self.accessibility_tree();
        tree.node = tree.node.with_name(t("a11y-document-editor"));
        Some(tree.into_access_node())
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Focus) {
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
        if let Some(review_action) = self.review.hits().iter().find_map(|(_, review_action)| {
            (ui::a11y::hash_key(&review_action_target(review_action)) == target)
                .then(|| review_action.clone())
        }) {
            self.dispatch_review_action(review_action, state);
            self.modified = true;
            self.note_edit_for_autosave(state);
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
        self.dirty = true;
        if !matches!(
            tool,
            ToolAction::ToggleStyles
                | ToolAction::ToggleRuler
                | ToolAction::ToggleOutline
                | ToolAction::ToggleHistory
                | ToolAction::NewMenu
        ) {
            self.needs_commit = true;
            self.modified = true;
            self.note_edit_for_autosave(state);
        }
        true
    }

    fn test_id(&self) -> Option<&str> {
        Some("word_processor")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.find.set_host_focused(focused);
        if !focused {
            // Blur any in-progress numeric edit; the next sync reloads the
            // persisted style values.
            self.editing_field = None;
            self.edit_buf.clear();
            self.scrub = None;
        }
        self.dirty = true;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.find.is_open() {
            self.find.ime_cursor_rect()
        } else {
            None
        }
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.find.is_open() {
            self.find.ime_purpose()
        } else {
            None
        }
    }
    fn needs_commit(&self) -> bool {
        self.needs_commit
    }
    fn take_commit_context(&mut self) -> Option<qos_actions::ActionContext> {
        self.needs_commit.then(|| {
            let resource_id = self.path.as_ref().map_or_else(
                || "word-processor.document:untitled".to_string(),
                |path| format!("word-processor.document:{}", path.display()),
            );
            let resource_label = self
                .path
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("Untitled document");
            qos_actions::ActionContext::for_resource(
                "Edit document",
                resource_id,
                "word_processor.document",
                resource_label,
            )
            .with_app("org.qos.Writer", "Writer")
        })
    }
    fn on_commit(&mut self) {
        self.needs_commit = false;
        self.modified = true;
    }
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// a [`WordProcessorAction`](crate::actions) through
    /// [`Self::perform_action`], so it records one undo boundary bracketing the
    /// affordance's change (the button-driven twin of the Ctrl+K palette).
    /// Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
    /// Publish the resource tree Ctrl+K walks (delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`]).
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
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

impl WordProcessorWidget {
    fn block_index_cached(&self, block: &str) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == block)
    }

    /// Selection endpoints in document order, computed from the block cache
    /// (so it works during `draw`, which has no `state`).
    fn selection_span_cached(&self) -> Option<(Caret, Caret)> {
        let anchor = self.inner.selection_anchor.clone()?;
        let caret = self.inner.caret.clone();
        if anchor == caret {
            return None;
        }
        match (
            self.block_index_cached(&anchor.block),
            self.block_index_cached(&caret.block),
        ) {
            (Some(pa), Some(pc)) => {
                if (pa, anchor.offset) <= (pc, caret.offset) {
                    Some((anchor, caret))
                } else {
                    Some((caret, anchor))
                }
            }
            _ => None,
        }
    }

    /// Per-block selected char ranges spanning the selection (cache-based).
    fn selection_blocks_cached(&self) -> Vec<(String, usize, usize)> {
        let Some((start, end)) = self.selection_span_cached() else {
            return vec![];
        };
        let (Some(si), Some(ei)) = (
            self.block_index_cached(&start.block),
            self.block_index_cached(&end.block),
        ) else {
            return vec![];
        };
        (si..=ei)
            .filter_map(|i| {
                let b = self.blocks.get(i)?;
                let blen = b.text.chars().count();
                let s = if i == si { start.offset } else { 0 };
                let e = if i == ei { end.offset } else { blen };
                Some((b.id.clone(), s, e))
            })
            .collect()
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
        for l in &self.laid {
            let Some((_, layout)) = self.layout_cache.get(&l.id) else {
                continue;
            };
            let key = block_presence_key(&l.id);

            // Remote selections (drawn under the text, like the local one).
            if let Some(list) = presence.selections.get(&key) {
                for (user, range) in list {
                    if range.start >= range.end {
                        continue;
                    }
                    let rects: Vec<Rect> = layout
                        .selection_rects(range.start, range.end)
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
                    let (cx, cy, ch) = layout.caret_geometry(*byte_pos);
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
    /// paragraph is hers") is `qdoc::author_runs`, already fed to
    /// [`ui::presence::PresenceCtx::draw_peer_authorship`] by
    /// [`ui::widget::block_editor`]; this widget doesn't call it yet.
    pub(crate) fn remote_typing_overlays(
        &self,
        presence: &PresenceCtx,
        now: std::time::Instant,
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
                    .selection_rects(range.start, range.end)
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
            self.blocks.iter().find(|b| &b.id == bid).is_some_and(|b| {
                b.marks
                    .iter()
                    .any(|m| m.name == name && m.start <= *s && m.end >= *e)
            })
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

    fn apply_tool(&mut self, state: &mut AutoCommit, action: ToolAction) {
        match action {
            // Every document-mutating format dispatches its matching
            // `WordProcessorAction` through the shared action path (the same
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
            ToolAction::TableRowAdd => self.inner.table_add_row(state),
            ToolAction::TableRowDel => self.inner.table_del_row(state),
            ToolAction::TableColAdd => self.inner.table_add_col(state),
            ToolAction::TableColDel => self.inner.table_del_col(state),
            ToolAction::TableDelete => self.inner.table_delete(state),
            ToolAction::ToggleStyles => self.styles_panel_open = !self.styles_panel_open,
            ToolAction::ToggleRuler => self.ruler_on = !self.ruler_on,
            ToolAction::ToggleOutline => self.outline.toggle(),
            ToolAction::ToggleHistory => {
                self.history.toggle();
                if self.history.is_open() {
                    // Derive the timeline the moment the strip opens, rather
                    // than waiting for the next document change.
                    self.history.refresh(state);
                }
            }
            ToolAction::InsertToc => self.generate_toc(state),
            ToolAction::NewMenu => self.open_template_picker(),
        }
    }

    /// Mutate the focused block's style in the live widget sheet (component-
    /// instance preview), then optionally persist it to the document.
    fn edit_focused_style(
        &mut self,
        state: &mut AutoCommit,
        persist: bool,
        f: impl FnOnce(&mut crate::styles::StyleProps),
    ) {
        let Some(key) = self.focused_style_key() else {
            return;
        };
        if let Some(st) = self.sheet.styles.get_mut(&key) {
            f(st);
        }
        self.dirty = true;
        if persist {
            let sheet = self.sheet.clone();
            self.inner.set_stylesheet(state, &sheet);
            self.needs_commit = true;
        }
    }

    /// Apply the typed buffer to the editing field as a live preview (no persist).
    fn apply_edit_buf_live(&mut self) {
        let Some(field) = self.editing_field else {
            return;
        };
        // A typed "nan"/"inf"/"-inf" parses successfully as f32 but is not a usable
        // style value (it would corrupt layout math and, if the style is ever
        // JSON-exported, panic inside serde_json) — treat it like any other
        // unparseable input and fall back to 0.0.
        let v = field.clamp(
            self.edit_buf
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite())
                .unwrap_or(0.0),
        );
        let Some(key) = self.focused_style_key() else {
            return;
        };
        if let Some(st) = self.sheet.styles.get_mut(&key) {
            field.set(st, v);
        }
        self.dirty = true;
    }

    /// Commit the typed numeric edit to the document and blur the field.
    fn commit_num_edit(&mut self, state: &mut AutoCommit) {
        if self.editing_field.is_none() {
            return;
        }
        self.apply_edit_buf_live();
        let sheet = self.sheet.clone();
        self.inner.set_stylesheet(state, &sheet);
        self.needs_commit = true;
        self.editing_field = None;
        self.edit_buf.clear();
        self.dirty = true;
    }

    /// Dispatch a click on a styles-panel target.
    fn apply_panel_hit(&mut self, state: &mut AutoCommit, hit: PanelHit) {
        match hit {
            PanelHit::Pick(id) => {
                // A built-in style implies a structural kind, applied together as one
                // action/undo boundary: picking "Heading 1" yields a *real* heading
                // (present in outline/TOC), "List Item" a real list item — not merely
                // styled body text. Custom styles are pure visual presets; kind stays untouched.
                self.dispatch_apply_named_style(state, &id);
                self.refresh_blocks(state);
                self.needs_commit = true;
                self.modified = true;
                self.dirty = true;
            }
            PanelHit::CycleFont => {
                self.edit_focused_style(state, true, |st| {
                    st.font = match st.font.as_str() {
                        "sans" => "serif",
                        "serif" => "mono",
                        _ => "sans",
                    }
                    .into();
                });
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
                self.needs_commit = true;
                self.modified = true;
                self.dirty = true;
            }
            PanelHit::ParaLineSpacing => {
                let next = next_line_spacing(self.inner.caret_line_spacing(state));
                self.dispatch_block_attr(
                    state,
                    "line_spacing",
                    crate::actions::AttrVal::Float(next),
                );
                self.refresh_blocks(state);
                self.needs_commit = true;
                self.modified = true;
                self.dirty = true;
            }
            PanelHit::SetColor(c) => self.edit_focused_style(state, true, |st| st.color = c),
            PanelHit::OpenColorPicker => self.open_color_picker(),
            PanelHit::NumDelta(field, d) => {
                self.edit_focused_style(state, true, |st| {
                    let nv = field.clamp(field.get(st) + d);
                    field.set(st, nv);
                });
            }
            PanelHit::Num(field) => self.apply_panel_hit_num_start(field),
            PanelHit::NewStyle => self.apply_panel_hit_new_style(state),
            PanelHit::ImageScale(factor) => self.apply_panel_hit_image_scale(state, factor),
            PanelHit::ImageFlipH => self.apply_panel_hit_image_flip(state, true),
            PanelHit::ImageFlipV => self.apply_panel_hit_image_flip(state, false),
        }
    }

    /// `PanelHit::Num`: start a scrub candidate — a drag scrubs the value, a
    /// plain click (no movement) focuses the field for typing (handled on
    /// release).
    fn apply_panel_hit_num_start(&mut self, field: NumField) {
        let start_val = self
            .focused_style_key()
            .map_or(0.0, |k| field.get(&self.sheet.get(&k)));
        self.editing_field = None;
        self.scrub = Some(ScrubState {
            field,
            start_x: self.pointer.x,
            start_val,
            moved: false,
        });
        self.dirty = true;
    }

    /// `PanelHit::NewStyle`: duplicate the focused style under a fresh id.
    fn apply_panel_hit_new_style(&mut self, state: &mut AutoCommit) {
        let Some(key) = self.focused_style_key() else {
            return;
        };
        let base = self.sheet.get(&key);
        let new_id = self.sheet.fresh_custom_id();
        let mut props = base.clone();
        props.name = format!("{} copy", base.name);
        self.sheet.styles.insert(new_id.clone(), props);
        let sheet = self.sheet.clone();
        self.inner.set_stylesheet(state, &sheet);
        self.inner.set_block_style(state, &new_id);
        self.refresh_blocks(state);
        self.needs_commit = true;
        self.dirty = true;
    }

    /// `PanelHit::ImageScale`: scale the *current on-screen* size (aspect
    /// locked) so repeated presses compound predictably, seeding from the
    /// live doc size when set, else the laid display size.
    fn apply_panel_hit_image_scale(&mut self, state: &mut AutoCommit, factor: f64) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        let (cw, ch) = self
            .inner
            .image_size(state, &id)
            .or_else(|| self.selected_image_dims())
            .unwrap_or((240.0, 160.0));
        self.inner
            .set_image_size(state, &id, cw * factor, ch * factor);
        self.refresh_blocks(state);
        self.needs_commit = true;
        self.modified = true;
        self.dirty = true;
    }

    /// `PanelHit::ImageFlipH`/`ImageFlipV`: mirror the selected image about
    /// its horizontal (`horizontal`) or vertical axis.
    fn apply_panel_hit_image_flip(&mut self, state: &mut AutoCommit, horizontal: bool) {
        let Some(id) = self.selected_image_id() else {
            return;
        };
        self.inner.flip_image(state, &id, horizontal);
        self.refresh_blocks(state);
        self.needs_commit = true;
        self.modified = true;
        self.dirty = true;
    }

    // ---- colour picker popover --------------------------------------------

    /// Open the shared [`ColorPicker`] seeded with the focused style's colour.
    fn open_color_picker(&mut self) {
        let seed = self
            .focused_style_key()
            .map_or([0x1c, 0x1c, 0x20], |k| self.sheet.get(&k).color);
        self.color_picker.open(seed, self.pointer);
        self.dirty = true;
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
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) -> bool {
        let result = self.color_picker.handle_event(
            event,
            scale,
            mods,
            state,
            root,
            self.rect,
            self.panel_rect,
        );
        if let Some(rgb) = result.changed {
            self.edit_focused_style(state, true, |st| st.color = rgb);
        }
        if result.consumed {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.pointer = Point::new(position.x / scale, position.y / scale);
            }
            self.dirty = true;
            *handled = true;
        }
        result.consumed
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

    fn clipboard_copy(&mut self, state: &AutoCommit) {
        if !self.inner.has_selection() {
            return;
        }
        let (plain, blocks) = self.inner.copy_fragment(state);
        clipboard_copy(&plain);
        self.rich_clip = Some((plain, blocks));
    }

    /// Paste with formatting when the system clipboard still holds our last rich
    /// copy; otherwise paste the clipboard's plain text (e.g. from another app).
    fn clipboard_paste(&mut self, state: &mut AutoCommit) {
        let Some(sys) = clipboard_paste() else {
            return;
        };
        let rich = self
            .rich_clip
            .as_ref()
            .filter(|(plain, _)| plain == &sys)
            .map(|(_, blocks)| blocks.clone());
        match rich {
            Some(blocks) => self.inner.insert_rich(state, &blocks),
            None => self.inner.insert_multiline(state, &sys),
        }
    }

    /// Paste plain text only (Ctrl+Shift+V), stripping all formatting.
    fn clipboard_paste_plain(&mut self, state: &mut AutoCommit) {
        if let Some(t) = clipboard_paste() {
            self.inner.insert_multiline(state, &t);
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
        let language = self.spell_language.clone();
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
        let had_selection = self.inner.has_selection();
        self.caret_from_pointer(content_pt, false);
        // Raw click char offset (before `select_word` snaps the caret to the word
        // end) — used to test which tracked-change / comment range was clicked.
        let raw_click_off = self.inner.caret.offset;
        if !had_selection {
            self.inner.select_word(state);
        }

        // Identify the clicked word's block + char span for spell-check actions.
        let block = self.inner.caret.block.clone();
        let block_text = self.inner.current_text(state);
        let click_off = self.inner.caret.offset; // post-select_word: end of the word
        let (wstart, wend) = Self::word_span_at(&block_text, click_off);
        let word: String = block_text
            .chars()
            .skip(wstart)
            .take(wend - wstart)
            .collect();

        let mut menu = ContextMenu::new();
        let mut actions: Vec<Option<CtxAction>> = Vec::new();

        self.push_review_rows(&mut menu, &mut actions, &block, wstart, wend, raw_click_off);
        self.push_spellcheck_rows(&mut menu, &mut actions, &block, &word, wstart, wend);
        self.push_clipboard_rows(&mut menu, &mut actions);

        menu.open_at(pos);
        self.context_menu = menu;
        self.context_actions = actions;
        self.dirty = true;
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
                if m.name == qdoc::schema::MARK_INS || m.name == qdoc::schema::MARK_DEL {
                    let is_ins = m.name == qdoc::schema::MARK_INS;
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
                } else if m.name == qdoc::schema::MARK_COMMENT {
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
    fn dispatch_context_action(&mut self, idx: usize, state: &mut AutoCommit) -> bool {
        let Some(Some(action)) = self.context_actions.get(idx).cloned() else {
            return false;
        };
        match action {
            CtxAction::Cut => {
                self.clipboard_copy(state);
                self.inner.delete_selection(state);
                self.needs_commit = true;
            }
            CtxAction::Copy => self.clipboard_copy(state),
            CtxAction::Paste => {
                self.clipboard_paste(state);
                self.needs_commit = true;
            }
            CtxAction::SelectAll => self.inner.select_all(state),
            CtxAction::AddToDictionary(word) => {
                let dict = self.dictionary();
                if dict.add_word(&word) {
                    // Persist best-effort, then drop the spell cache so every
                    // block re-checks against the updated dictionary and the
                    // squiggle clears on the next draw.
                    let _ = dict.save_user_dict();
                    self.spell_cache.clear();
                }
            }
            CtxAction::ApplySuggestion {
                block,
                start,
                end,
                replacement,
            } => {
                self.inner
                    .splice_block(state, &block, start, end, &replacement);
                self.refresh_blocks(state);
                // The block text changed; its cache entry is stale.
                self.spell_cache.remove(&block);
                self.needs_commit = true;
            }
            CtxAction::Review(action) => self.dispatch_review_action(action, state),
        }
        self.context_menu.close();
        self.dirty = true;
        true
    }

    // ---- paragraph-style management menu ----------------------------------

    /// The style id of the picker chip under `p` in the styles panel, if any.
    /// Reads the current frame's `panel_hits` (rebuilt every draw), so the chip
    /// rects are live at right-click time.
    fn style_chip_at(&self, p: Point) -> Option<String> {
        self.panel_hits.iter().find_map(|(r, h)| match h {
            PanelHit::Pick(id) if r.contains(p) => Some(id.clone()),
            _ => None,
        })
    }

    /// Apply a typed style-menu outcome. The component owns presentation and
    /// hit-testing; document mutation remains editor policy.
    fn dispatch_style_menu(&mut self, action: StyleMenuAction, state: &mut AutoCommit) {
        match action {
            StyleMenuAction::Delete(id) => {
                if self.inner.delete_style(state, &id) {
                    self.sheet = self.inner.stylesheet(state);
                    self.refresh_blocks(state);
                    self.needs_commit = true;
                    self.modified = true;
                    self.toast = Some((t("toast-style-deleted"), ToastKind::Success));
                }
            }
            StyleMenuAction::Duplicate(id) => {
                if let Some(new_id) = self.inner.duplicate_style(state, &id) {
                    // Apply the copy to the caret block so the panel focuses it.
                    self.inner.set_block_style(state, &new_id);
                    self.sheet = self.inner.stylesheet(state);
                    self.refresh_blocks(state);
                    self.needs_commit = true;
                    self.modified = true;
                    self.toast = Some((t("toast-style-duplicated"), ToastKind::Success));
                }
            }
        }
        self.dirty = true;
    }

    // ---- new-from-template picker -----------------------------------------

    /// Open the "New ▾" picker below the toolbar's leading edge.
    fn open_template_picker(&mut self) {
        // Anchor below the toolbar's left edge (where the "New ▾" button lives).
        let anchor_x = self
            .toolbar
            .first()
            .map_or(self.rect.x0 + 8.0, |(_, r)| r.x0);
        let anchor_y = self.rect.y0 + self.toolbar_height(self.rect.width());
        self.template_picker.open_at(Point::new(anchor_x, anchor_y));
        self.dirty = true;
    }

    /// Replace the open document's content with `template`'s Markdown, **in place**
    /// on the live `state` (preserving the host `ui::State` and its undo history /
    /// collaboration session). Re-seats the caret, refreshes the view, flags an
    /// undo point, and counts as a document-modifying edit (so autosave still
    /// protects the freshly-templated content).
    fn apply_template(&mut self, state: &mut AutoCommit, template: crate::Template) {
        if self
            .inner
            .replace_with_markdown(state, template.content().as_bytes())
        {
            self.refresh_blocks(state);
            // The whole body changed; per-block caches keyed by id/text are stale.
            self.layout_cache.clear();
            self.spell_cache.clear();
            self.modified = true;
            self.needs_commit = true;
            self.dirty = true;
            // A wholesale content swap is a strong autosave trigger.
            self.note_edit_for_autosave(state);
        }
    }

    // ---- autosave / crash-recovery ----------------------------------------

    /// Recovery path for the currently open document (exposed for focused tests).
    #[cfg(test)]
    fn recovery_path(&self) -> PathBuf {
        self.recovery.path_for(self.path.as_deref())
    }

    /// Point this widget's recovery store at `dir`.
    pub fn set_recovery_dir(&mut self, dir: PathBuf) {
        self.recovery.set_base_dir(dir);
    }

    /// Export one Markdown snapshot and hand it to the recovery component when
    /// the edit debounce reaches its threshold.
    fn note_edit_for_autosave(&mut self, state: &AutoCommit) {
        let inner = &self.inner;
        let document_path = self.path.clone();
        self.recovery.note_edit(document_path.as_deref(), || {
            inner.containers.as_ref().and_then(|c| {
                WpFormat::Markdown
                    .export(qdoc::DocRef { doc: state, c })
                    .ok()
            })
        });
    }

    /// Delete a recovery copy superseded by a successful open/save.
    fn clear_recovery(&mut self) {
        self.recovery.clear(self.path.as_deref());
    }

    /// Stage a recovery offer for the currently open document, if one exists.
    pub fn check_for_recovery(&mut self) -> bool {
        let staged = self.recovery.check(self.path.as_deref());
        if staged {
            self.dirty = true;
        }
        staged
    }

    /// Act on a tracked-change / comment review affordance (from a margin-card
    /// button or a context-menu row): accept/reject a change, accept/reject all,
    /// or resolve (delete) a comment. Refreshes the materialized blocks +
    /// comments/changes so the acted-on item disappears, and flags an undo point.
    fn dispatch_review_action(&mut self, action: ReviewAction, state: &mut AutoCommit) {
        let typed = match action {
            ReviewAction::AcceptChange(id) => {
                crate::actions::WordProcessorAction::AcceptChanges { id: Some(id) }
            }
            ReviewAction::RejectChange(id) => {
                crate::actions::WordProcessorAction::RejectChanges { id: Some(id) }
            }
            ReviewAction::AcceptAllChanges => {
                crate::actions::WordProcessorAction::AcceptChanges { id: None }
            }
            ReviewAction::RejectAllChanges => {
                crate::actions::WordProcessorAction::RejectChanges { id: None }
            }
            ReviewAction::ResolveComment(id) => {
                crate::actions::WordProcessorAction::ResolveComment { id }
            }
        };
        self.perform_action(state, &typed);
        self.refresh_blocks(state);
        // Accepting/rejecting can change body text (an accepted deletion removes
        // glyphs, a rejected insertion removes them); the spell cache is keyed by
        // text so affected blocks re-check on the next draw — clear it wholesale
        // (cheap, and these are infrequent review actions).
        self.spell_cache.clear();
        self.needs_commit = true;
        self.dirty = true;
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

    /// The compact bottom-chrome variant only carries the immediate editing
    /// facts; filename and save state remain available from the title surface.
    fn status_count_label(&self) -> String {
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
        t_args(
            "status-count",
            &[
                arg("words", num::i64_of_usize(words)),
                arg("chars", num::i64_of_usize(chars)),
            ],
        )
    }

    /// Allocate the docked document-status label wholly inside `bar`, after the
    /// layout chips and hint. Returning `None` at extremely narrow widths is
    /// preferable to either overlapping a control or painting over the page.
    fn layout_bar_status_rect(bar: Rect, after_x: f64, label: &str) -> Option<Rect> {
        let width = num::f64_of_usize(label.chars().count()) * 7.0;
        let x0 = bar.x1 - 12.0 - width;
        (x0 >= after_x).then_some(Rect::new(x0, bar.y0, x0 + width, bar.y1))
    }

    /// Draw the zoom component in the toolbar's last row.
    fn draw_zoom_control(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        row: Rect,
    ) {
        self.zoom.draw(
            scene,
            transform,
            tr,
            row,
            self.chrome.chip,
            self.chrome.text,
        );
    }

    /// Build the toolbar's button spec list (action, label, width,
    /// active-state) for this frame: formatting toggles, the contextual table
    /// section (editing controls inside a cell, else "Insert table"), and the
    /// panel/pane toggles. Paragraph-style presets/controls live in the side
    /// panel, not here — see [`ToolAction`].
    fn toolbar_button_specs(&self) -> Vec<(ToolAction, ToolbarItem)> {
        let bold_a = self.mark_active_in_selection("strong");
        let italic_a = self.mark_active_in_selection("em");
        let under_a = self.mark_active_in_selection("underline");
        let strike_a = self.mark_active_in_selection("strike");
        let super_a = self.mark_active_in_selection("super");
        let sub_a = self.mark_active_in_selection("sub");
        // Only the caret block *kind* is needed now (to switch in the contextual
        // table controls). Paragraph-style choices (Body / Heading / List) live in
        // the side panel's style picker, not the toolbar — see `draw_styles_panel`.
        let (kind, _level) = self.current_block_style();
        let font_label = self.current_font().label();

        // Build the toolbar spec; the ui::chrome toolbar component owns the look
        // and hands back each button's clickable rect (zipped to its action).
        let cue = |a: ToolAction| match a {
            ToolAction::Bold => LabelCue::Bold,
            ToolAction::Underline => LabelCue::Underline,
            ToolAction::Strike => LabelCue::Strike,
            _ => LabelCue::None,
        };
        let group_start = |a: ToolAction| {
            matches!(
                a,
                ToolAction::NewMenu
                    | ToolAction::Bold
                    | ToolAction::FontCycle
                    | ToolAction::InsertTable
                    | ToolAction::InsertImage
                    | ToolAction::TableRowAdd
                    | ToolAction::ToggleStyles
                    | ToolAction::ToggleOutline
            )
        };
        let panel_open = self.styles_panel_open;
        let mut specs: Vec<(ToolAction, ToolbarItem)> = Vec::new();
        let mut add = |a: ToolAction, label: &str, w: f64, active: bool| {
            specs.push((
                a,
                ToolbarItem::new(label, w)
                    .active(active)
                    .cue(cue(a))
                    .group_start(group_start(a)),
            ));
        };
        // New-from-template picker (leads the toolbar). Active while its menu is open.
        // Single-glyph faces (B/I/U/S, x²/x₂, H1/H2, A−/A+, ✕) are symbols, not
        // words, and stay as literals; the spelled-out names live in the a11y
        // labels (see `tool_action_a11y_label`).
        add(
            ToolAction::NewMenu,
            &t("toolbar-new"),
            64.0,
            self.template_picker.is_open(),
        );
        add(ToolAction::Bold, "B", 34.0, bold_a);
        add(ToolAction::Italic, "I", 30.0, italic_a);
        add(ToolAction::Underline, "U", 32.0, under_a);
        add(ToolAction::Strike, "S", 30.0, strike_a);
        add(ToolAction::Super, "x²", 34.0, super_a);
        add(ToolAction::Sub, "x₂", 34.0, sub_a);
        // Paragraph-style presets (H1/H2/Body/List) and paragraph controls
        // (alignment, line spacing) are the side panel's job, not the toolbar —
        // see `ToolAction`. Quick headings remain on Ctrl+1/2/3.
        add(ToolAction::FontCycle, font_label, 58.0, false);
        add(ToolAction::FontSizeDec, "A−", 34.0, false);
        add(ToolAction::FontSizeInc, "A+", 34.0, false);
        // Contextual table section: editing controls inside a cell, else insert.
        if kind == "cell" {
            add(ToolAction::TableRowAdd, &t("toolbar-row-add"), 54.0, false);
            add(ToolAction::TableRowDel, &t("toolbar-row-del"), 54.0, false);
            add(ToolAction::TableColAdd, &t("toolbar-col-add"), 52.0, false);
            add(ToolAction::TableColDel, &t("toolbar-col-del"), 52.0, false);
            add(ToolAction::TableDelete, "✕", 30.0, false);
        } else {
            add(ToolAction::InsertTable, &t("toolbar-table"), 78.0, false);
        }
        // The Image button is drawn as a vector glyph (a lucide `Image` icon,
        // overlaid below after the chrome lays the toolbar out) with no text
        // label — replacing the old emoji-prefixed "🖼 Image" caption. The empty
        // label keeps the chrome button blank so only the icon shows.
        add(ToolAction::InsertImage, "", 40.0, false);
        add(
            ToolAction::ToggleStyles,
            &t("toolbar-styles"),
            64.0,
            panel_open,
        );
        add(
            ToolAction::ToggleRuler,
            &t("toolbar-ruler"),
            60.0,
            self.ruler_on,
        );
        // Navigation: outline pane toggle + table-of-contents generator.
        add(
            ToolAction::ToggleOutline,
            &t("toolbar-outline"),
            72.0,
            self.outline.is_open(),
        );
        add(
            ToolAction::ToggleHistory,
            &t("toolbar-history"),
            68.0,
            self.history.is_open(),
        );
        add(ToolAction::InsertToc, &t("toolbar-toc"), 48.0, false);
        specs
    }

    fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tb: Rect,
    ) {
        let specs = self.toolbar_button_specs();
        let items: Vec<ToolbarItem> = specs.iter().map(|(_, item)| item.clone()).collect();

        // Responsive packing: when the controls don't fit `tb.width()`, wrap to
        // additional rows so nothing is ever clipped or unreachable. The packer's
        // width model mirrors `chrome::toolbar::draw`, so each row-slice it admits
        // is drawn by `draw` without overflow. `tb` is the FULL multi-row toolbar
        // rect (its height already reserves every row — see `toolbar_height`).
        let mut widths: Vec<f64> = items.iter().map(|it| it.width).collect();
        let mut starts: Vec<bool> = items.iter().map(|it| it.group_start).collect();
        // Reserve the right-aligned swatch strips + zoom stepper (drawn directly,
        // not as chrome buttons) as a trailing phantom item so no button row
        // extends under them; `toolbar_item_widths` mirrors this exactly.
        widths.push(swatch_reserve_width() + ZoomControl::WIDTH + ZOOM_SWATCH_GAP);
        starts.push(true);
        let rows = pack_toolbar_rows(&widths, &starts, tb.width());
        self.toolbar_rows = rows.len();

        // Fill the whole toolbar background once (so inter-row gaps are toned
        // consistently), then draw each row's buttons at its own band.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &tb,
        );

        let row_h = TOOLBAR_H;
        let mut all_rects: Vec<Rect> = Vec::with_capacity(items.len());
        for (ri, (s, e)) in rows.iter().enumerate() {
            let ry0 = tb.y0 + num::f64_of_usize(ri) * row_h;
            let row_rect = Rect::new(tb.x0, ry0, tb.x1, ry0 + row_h);
            // The trailing swatch reservation is a phantom (index == items.len()),
            // drawn separately below — clamp it out of the real button slice.
            let e_btn = (*e).min(items.len());
            if *s < e_btn
                && let Some(row_items) = items.get(*s..e_btn)
            {
                let rects = chrome::toolbar::draw(scene, transform, tr, theme, row_rect, row_items);
                all_rects.extend(rects);
            }
        }
        self.toolbar = specs.into_iter().map(|(a, _)| a).zip(all_rects).collect();

        // Paint the vector "Insert image" glyph (a lucide `Image` icon) centred in
        // its blank toolbar button — the button carries no text label, so the
        // glyph *is* its face. Drawn here (not by the chrome toolbar, which only
        // renders text) so the emoji-free icon reads at the OS chrome ink.
        if let Some((_, r)) = self
            .toolbar
            .iter()
            .find(|(a, _)| matches!(a, ToolAction::InsertImage))
        {
            let s = 18.0;
            let cx = f64::midpoint(r.x0, r.x1);
            let cy = f64::midpoint(r.y0, r.y1);
            let glyph = Rect::new(cx - s / 2.0, cy - s / 2.0, cx + s / 2.0, cy + s / 2.0);
            Icon::Image.draw(scene, transform, glyph, self.chrome.text);
        }

        // Per-run text-colour swatch row, appended at the right of the LAST
        // toolbar row. The chrome toolbar only draws text buttons, so the coloured
        // squares are painted directly here (and registered as `TextColor` click
        // targets) — no `ui` change needed. The leading neutral swatch clears the
        // run colour.
        let last_row_y = tb.y0 + (num::f64_of_usize(rows.len()) - 1.0) * row_h;
        // Zoom stepper: right-aligned on the last toolbar row (its width reserved
        // as part of the trailing packer phantom below, so buttons never run under
        // it). The colour/highlight swatch strips pack to its left.
        let last_row = Rect::new(tb.x0, last_row_y, tb.x1, last_row_y + row_h);
        self.draw_zoom_control(scene, transform, tr, last_row);
        let swatch_x1 = (tb.x1 - ZoomControl::WIDTH - ZOOM_SWATCH_GAP).max(tb.x0);
        let swatch_band = Rect::new(tb.x0, last_row_y, swatch_x1, last_row_y + row_h);
        let text_left = self.draw_color_swatches(scene, transform, tr, swatch_band);
        // Highlight swatch strip, packed just left of the text-colour strip.
        let hl_band = Rect::new(
            tb.x0,
            last_row_y,
            (text_left - 10.0).max(tb.x0),
            last_row_y + row_h,
        );
        self.draw_highlight_swatches(scene, transform, tr, hl_band);

        // Subtle hover highlight over the button under the pointer. A low-alpha
        // tint of the primary text colour reads as a veneer in both light
        // (dark-on-light) and dark (light-on-dark) modes.
        if let Some(hr) = self.hover_tool {
            let pad = Rect::new(hr.x0 - 2.0, hr.y0 + 1.0, hr.x1 + 2.0, hr.y1 - 1.0);
            let veneer = with_alpha(self.chrome.text, 0x14);
            scene.fill(
                Fill::NonZero,
                transform,
                veneer,
                None,
                &vello::kurbo::RoundedRect::from_rect(pad, 6.0),
            );
        }
    }

    /// The toolbar's full height for the current window width — `TOOLBAR_H` per
    /// wrapped row. Computed from the same packer `draw_toolbar` uses so the
    /// canvas top offset and the drawn rows always agree.
    fn toolbar_height(&self, width: f64) -> f64 {
        let widths: Vec<f64> = self.toolbar_item_widths();
        let starts: Vec<bool> = self.toolbar_item_group_starts();
        let rows = pack_toolbar_rows(&widths, &starts, width);
        num::f64_of_usize(rows.len()) * TOOLBAR_H
    }

    /// The per-item widths fed to the toolbar packer. Mirrors the `add(...)`
    /// sequence in `draw_toolbar`; the contextual table/cell controls depend on
    /// the caret block kind, so this resolves the same branch.
    fn toolbar_item_widths(&self) -> Vec<f64> {
        let (kind, _) = self.current_block_style();
        let mut w = vec![
            64.0, // New ▾
            34.0, 30.0, 32.0, 30.0, // B I U S
            34.0, 34.0, // x² x₂
            58.0, // font family
            34.0, 34.0, // A− A+ (font size)
        ];
        if kind == "cell" {
            w.extend([54.0, 54.0, 52.0, 52.0, 30.0]); // +Row −Row +Col −Col ✕
        } else {
            w.push(78.0); // ⊞ Table
        }
        w.push(40.0); // Image (vector icon, no label)
        w.push(64.0); // Styles
        w.push(60.0); // Ruler
        w.push(72.0); // Outline
        w.push(68.0); // History
        w.push(48.0); // TOC
        // right-aligned colour + highlight swatches, plus the zoom stepper cluster
        // (mirrors the reservation in `draw_toolbar`).
        w.push(swatch_reserve_width() + ZoomControl::WIDTH + ZOOM_SWATCH_GAP);
        w
    }

    /// The per-item `group_start` flags, parallel to `toolbar_item_widths`.
    fn toolbar_item_group_starts(&self) -> Vec<bool> {
        let (kind, _) = self.current_block_style();
        // Indices that start a group in `draw_toolbar` (see `group_start`).
        let mut g = vec![
            true, // New ▾ (group)
            true, false, false, false, // B(group) I U S
            false, false, // x² x₂
            true,  // font family(group)
            false, false, // A− A+ (same group as font)
        ];
        if kind == "cell" {
            g.extend([true, false, false, false, false]); // +Row starts a group
        } else {
            g.push(true); // ⊞ Table starts a group
        }
        g.push(true); // Image starts a group
        g.push(true); // Styles starts a group
        g.push(false); // Ruler (same group as Styles)
        g.push(true); // Outline starts a group (navigation)
        g.push(false); // History (same navigation group)
        g.push(false); // TOC
        g.push(true); // swatch reservation starts a group (see `swatch_reserve_width`)
        g
    }

    /// Draw the text-colour swatch row at the right edge of the toolbar and
    /// register each swatch as a `TextColor` click target in `self.toolbar`.
    /// The current run's colour gets a white selection ring. The first swatch is
    /// the neutral/default (an `A` glyph on the surface colour) — it clears any
    /// per-run `color` mark.
    fn draw_color_swatches(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        tb: Rect,
    ) -> f64 {
        let cur = self.current_text_color();
        let cy = f64::midpoint(tb.y0, tb.y1);
        let sw = 20.0;
        let gap = 5.0;
        let n = STYLE_PALETTE.len() + 1; // +1 for the neutral default
        let total = num::f64_of_usize(n) * sw + (num::f64_of_usize(n) - 1.0) * gap;
        // Right-align the strip, leaving a small margin from the toolbar's edge.
        let strip_left = (tb.x1 - 14.0 - total).max(tb.x0 + 14.0);
        let mut x = strip_left;
        let y0 = cy - sw / 2.0;

        // Neutral / default swatch: surface-toned square with an "A", clears colour.
        let nr = Rect::new(x, y0, x + sw, y0 + sw);
        scene.fill(
            Fill::NonZero,
            transform,
            self.chrome.chip,
            None,
            &vello::kurbo::RoundedRect::from_rect(nr, 4.0),
        );
        if cur.is_none() {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                Color::WHITE,
                None,
                &vello::kurbo::RoundedRect::from_rect(nr, 4.0),
            );
        }
        tr.draw_text(
            scene,
            "A",
            13.0,
            self.chrome.text,
            transform * Affine::translate((x + 5.5, y0 + 2.0)),
            None,
        );
        self.toolbar.push((ToolAction::TextColor(None), nr));
        x += sw + gap;

        for c in &STYLE_PALETTE {
            let r = Rect::new(x, y0, x + sw, y0 + sw);
            let col = Color::from_rgb8(c[0], c[1], c[2]);
            scene.fill(
                Fill::NonZero,
                transform,
                col,
                None,
                &vello::kurbo::RoundedRect::from_rect(r, 4.0),
            );
            if cur == Some(*c) {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    Color::WHITE,
                    None,
                    &vello::kurbo::RoundedRect::from_rect(r, 4.0),
                );
            }
            self.toolbar.push((ToolAction::TextColor(Some(*c)), r));
            x += sw + gap;
        }
        strip_left
    }

    /// Draw the highlight (text-background) swatch row, right-aligned within
    /// `tb`, and register each swatch as a `Highlight` click target. The leading
    /// neutral swatch (a crossed marker on the surface tone) clears any highlight.
    /// The current run's highlight gets a white selection ring. Skips drawing if
    /// the band is too narrow (the toolbar is tightly packed).
    fn draw_highlight_swatches(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        tb: Rect,
    ) {
        let cur = self.current_highlight_color();
        let cy = f64::midpoint(tb.y0, tb.y1);
        let sw = 20.0;
        let gap = 5.0;
        let n = HIGHLIGHT_PALETTE.len() + 1; // +1 for the neutral "no highlight"
        let total = num::f64_of_usize(n) * sw + (num::f64_of_usize(n) - 1.0) * gap;
        // Not enough room beside the text-colour strip: skip (the buttons + Styles
        // panel remain the path to highlight). Avoids overlapping the chrome.
        if tb.width() < total + 14.0 {
            return;
        }
        let strip_left = (tb.x1 - total).max(tb.x0 + 4.0);
        let mut x = strip_left;
        let y0 = cy - sw / 2.0;

        // Neutral / "no highlight" swatch: surface-toned square with a "▱" hint.
        let nr = Rect::new(x, y0, x + sw, y0 + sw);
        scene.fill(
            Fill::NonZero,
            transform,
            self.chrome.chip,
            None,
            &vello::kurbo::RoundedRect::from_rect(nr, 4.0),
        );
        if cur.is_none() {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                Color::WHITE,
                None,
                &vello::kurbo::RoundedRect::from_rect(nr, 4.0),
            );
        }
        tr.draw_text(
            scene,
            "▱",
            12.0,
            self.chrome.text,
            transform * Affine::translate((x + 4.0, y0 + 2.5)),
            None,
        );
        self.toolbar.push((ToolAction::Highlight(None), nr));
        x += sw + gap;

        for c in &HIGHLIGHT_PALETTE {
            let r = Rect::new(x, y0, x + sw, y0 + sw);
            let col = Color::from_rgb8(c[0], c[1], c[2]);
            scene.fill(
                Fill::NonZero,
                transform,
                col,
                None,
                &vello::kurbo::RoundedRect::from_rect(r, 4.0),
            );
            if cur == Some(*c) {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    Color::WHITE,
                    None,
                    &vello::kurbo::RoundedRect::from_rect(r, 4.0),
                );
            }
            self.toolbar.push((ToolAction::Highlight(Some(*c)), r));
            x += sw + gap;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use taffy::AvailableSpace;
    use ui::permissions::PermissionContext;

    #[test]
    fn native_document_edit_has_writer_provenance() {
        let mut wp = WordProcessorWidget::new();
        wp.needs_commit = true;

        let context = wp.take_commit_context().expect("native edit provenance");
        assert_eq!(context.action_label, "Edit document");
        assert_eq!(
            context.app.as_ref().map(|app| app.id.as_str()),
            Some("org.qos.Writer")
        );
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.kind.as_str()),
            Some("word_processor.document")
        );
    }

    /// Two windows on two files are two records, each identified by its file's
    /// path — what the broker routes on, and what stops two people editing
    /// unrelated documents in one room from merging.
    #[test]
    fn each_open_file_is_its_own_record() {
        let mut report = WordProcessorWidget::new();
        assert!(
            report.record_intent().is_none(),
            "an untitled document names no record"
        );
        report.set_file(PathBuf::from("/work/report.odt"), WpFormat::Markdown);

        let mut minutes = WordProcessorWidget::new();
        minutes.set_file(PathBuf::from("/work/minutes.odt"), WpFormat::Markdown);

        let a = report
            .record_intent()
            .expect("an open file names its record");
        let b = minutes.record_intent().expect("and so does the other");
        assert_eq!(a.id, ui::RecordId::for_file("/work/report.odt"));
        assert_eq!(a.kind, ui::RecordKind::new("document"));
        assert_ne!(a.id, b.id, "two files are never one record");
        assert_eq!(
            a.id.file_path().as_deref(),
            Some(std::path::Path::new("/work/report.odt")),
            "a file record's identity is the path itself, not a handle to it"
        );
    }

    pub(super) fn pipeline_draw(
        wp: &mut WordProcessorWidget,
        state: &mut ui::State,
        w: u32,
        h: u32,
    ) {
        pipeline_draw_themed(wp, state, w, h, &Theme::dark());
    }

    /// Like [`pipeline_draw`] but renders under an explicit [`Theme`] so tests
    /// can exercise the real five-pass draw path under light/dark/high-contrast
    /// palettes. Returns the widget's resolved chrome palette for inspection.
    fn pipeline_draw_themed(
        wp: &mut WordProcessorWidget,
        state: &mut ui::State,
        w: u32,
        h: u32,
        theme: &Theme,
    ) -> ChromeColors {
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        wp.sync(&mut state.doc, &state.root);
        let child = wp.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(num::f32_of_u32(w)),
                        height: Dimension::length(num::f32_of_u32(h)),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(num::f32_of_u32(w)),
                        height: AvailableSpace::Definite(num::f32_of_u32(h)),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        wp.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        wp.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            &presence,
            &permission,
        );
        // `draw` resolves the chrome palette from `theme` into `wp.chrome`.
        wp.chrome
    }

    /// Build a small populated doc (heading + paragraph) so a draw exercises the
    /// toolbar, panels, page, body ink, selection, and bars in one pass.
    fn populated() -> (ui::State, WordProcessorWidget) {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Title");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "Body text here");
        // Open the find bar + outline pane so their chrome is drawn too.
        wp.find.open(None, &wp.blocks);
        wp.outline.toggle();
        (state, wp)
    }

    /// `ChromeColors::for_theme` is the single production mapping `draw` uses.
    /// Assert every chrome slot is *sourced from the theme token* (not a literal)
    /// and that swapping the theme actually changes the sampled chrome surfaces —
    /// proving the chrome follows the OS palette.
    #[test]
    fn chrome_colors_are_sourced_from_theme_tokens() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let cc = ChromeColors::for_theme(&theme);
            let c = &theme.tokens.colors;
            assert_eq!(cc.canvas, c.surfaces.base, "canvas = surfaces.base");
            assert_eq!(cc.bar, c.surfaces.raised, "bar = surfaces.raised");
            assert_eq!(cc.border, c.border_standard, "border = border_standard");
            assert_eq!(cc.text, c.text_primary, "text = text_primary");
            assert_eq!(cc.text_dim, c.text_dim, "text_dim = text_dim");
            assert_eq!(cc.accent, theme.accent_primary, "accent = accent_primary");
        }
    }

    #[test]
    fn swapping_theme_changes_sampled_chrome() {
        let dark = ChromeColors::for_theme(&Theme::dark());
        let light = ChromeColors::for_theme(&Theme::light());
        // The desk/canvas and bar surfaces must differ between light and dark —
        // if the chrome were hard-coded these would be identical.
        assert_ne!(
            (dark.canvas.r(), dark.canvas.g(), dark.canvas.b()),
            (light.canvas.r(), light.canvas.g(), light.canvas.b()),
            "canvas tracks the theme",
        );
        assert_ne!(
            (dark.text.r(), dark.text.g(), dark.text.b()),
            (light.text.r(), light.text.g(), light.text.b()),
            "chrome text tracks the theme",
        );
    }

    /// The paper sheet is deliberately a *light* tone in every theme so the
    /// dark-authored document ink stays readable (see the `PAPER_*` convention).
    #[test]
    fn paper_stays_light_for_readable_ink_in_every_theme() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let cc = ChromeColors::for_theme(&theme);
            assert!(
                luminance(cc.paper) >= 0.85,
                "paper must stay light (got luminance {:.2}) so ink contrasts",
                luminance(cc.paper),
            );
            // The default paper ink is dark — confirm it actually contrasts.
            assert!(
                luminance(cc.paper) - luminance(PAPER_INK) > 0.5,
                "paper/ink contrast must be ample",
            );
        }
    }

    /// The whole five-pass draw path must run (and stash the themed chrome on the
    /// widget) without panicking under light, dark, AND high-contrast palettes.
    #[test]
    fn draw_path_reads_theme_and_does_not_panic_in_any_mode() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let (mut state, mut wp) = populated();
            let cc = pipeline_draw_themed(&mut wp, &mut state, 1100, 760, &theme);
            // The widget's live chrome must equal the theme's mapping — i.e. the
            // production `draw` resolved it from the passed-in `theme`.
            assert_eq!(cc.canvas, theme.tokens.colors.surfaces.base);
            assert_eq!(cc.bar, theme.tokens.colors.surfaces.raised);
            assert_eq!(cc.accent, theme.accent_primary);
        }
    }

    #[test]
    fn draw_lays_out_all_blocks() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Hello");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "World");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.laid.len(), 2, "draw laid out both paragraphs");
    }

    /// Select `[start, end)` chars of the (single) caret block.
    fn select_range(wp: &mut WordProcessorWidget, start: usize, end: usize) {
        let block = wp.model().caret.block.clone();
        wp.model_mut().set_caret(Caret {
            block: block.clone(),
            offset: start,
        });
        wp.model_mut().set_anchor();
        wp.model_mut().set_caret(Caret { block, offset: end });
    }

    /// The `EncodedAction` a resource's immediate `ActionSpec` carries.
    fn spec_encoded(res: &qos_actions::Resource, id: &str) -> qos_actions::EncodedAction {
        let spec = res
            .actions
            .iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("spec {id}"));
        match &spec.input {
            qos_actions::ActionInput::Immediate(enc) => enc.clone(),
            qos_actions::ActionInput::Prompted { .. } => panic!("{id} is immediate"),
        }
    }

    /// One source of truth on the **live widget path**: the Insert-Table toolbar
    /// button dispatches the byte-identical `EncodedAction` the Ctrl+K palette's
    /// `insert-table` spec builds for the same document.
    #[test]
    fn toolbar_insert_table_matches_palette_spec() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Intro");
        wp.refresh_blocks(&state.doc);

        // What the palette would dispatch, resolved before the button fires.
        let resources = Widget::resources(&wp, &state.doc);
        let from_palette = spec_encoded(&resources[0], "insert-table");

        // The toolbar button drives the same operation.
        wp.apply_tool(&mut state.doc, ToolAction::InsertTable);
        let from_toolbar = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("the toolbar dispatched an action");

        assert_eq!(from_toolbar.type_id, from_palette.type_id);
        assert_eq!(
            from_toolbar.payload, from_palette.payload,
            "toolbar affordance and palette spec build the same EncodedAction"
        );
        // And the affordance really created the table (a 2x2 = four cells).
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            wp.blocks.iter().filter(|b| b.kind == "cell").count(),
            4,
            "the toolbar affordance inserted the table"
        );
    }

    /// The styles panel's paragraph-align affordance dispatches an action, arms
    /// exactly one undo boundary for the host, and matches the palette spec.
    #[test]
    fn panel_align_dispatches_action_and_arms_one_undo_boundary() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Body text");
        wp.refresh_blocks(&state.doc);
        let before = state.doc.get_heads();

        wp.apply_panel_hit(&mut state.doc, PanelHit::ParaAlign("center"));
        let dispatched = wp
            .last_dispatched_for_test()
            .cloned()
            .expect("the panel dispatched an action");

        // The affordance mutated the document and armed one undo bracket at the
        // pre-affordance heads (what the host records as a single undo boundary).
        assert_ne!(
            state.doc.get_heads(),
            before,
            "the affordance mutated the document"
        );
        let bracket = Widget::take_committed_action(&mut wp).expect("an undo bracket was armed");
        assert_eq!(
            bracket, before,
            "the boundary brackets the pre-affordance heads"
        );
        assert!(
            Widget::take_committed_action(&mut wp).is_none(),
            "the bracket is rising-edge (cleared on read)"
        );

        // It is the same EncodedAction the palette's align-center spec builds.
        let resources = Widget::resources(&wp, &state.doc);
        let from_palette = spec_encoded(&resources[0], "align-center");
        assert_eq!(
            dispatched.payload, from_palette.payload,
            "panel align and palette spec build the same EncodedAction"
        );
        // The block really carries the centre alignment.
        wp.refresh_blocks(&state.doc);
        let aligned = wp
            .blocks
            .iter()
            .any(|b| matches!(b.attrs.get("align"), Some(AttrVal::Str(s)) if s == "center"));
        assert!(aligned, "the affordance set the paragraph alignment");
    }

    #[test]
    fn highlight_mark_yields_background_span() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hello world");
        select_range(&mut wp, 0, 5);
        wp.model_mut().set_highlight(&mut state.doc, "#fff35c");
        wp.refresh_blocks(&state.doc);
        let b = wp
            .blocks
            .iter()
            .find(|b| b.text == "hello world")
            .expect("block present");
        let (runs, _, _) = block_runs(b);
        // The first run ("hello") carries the highlight as a background band.
        let first = &runs[0];
        assert_eq!(first.text, "hello");
        assert_eq!(
            first.background,
            Some([0xff, 0xf3, 0x5c]),
            "highlight maps to RichSpan.background"
        );
        // The tail run (" world") has no highlight.
        assert!(
            runs.iter()
                .any(|r| r.text.contains("world") && r.background.is_none()),
            "unhighlighted tail has no band"
        );
    }

    #[test]
    fn super_mark_yields_super_baseline() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "x2");
        select_range(&mut wp, 1, 2); // the "2"
        wp.model_mut().toggle_super(&mut state.doc);
        wp.refresh_blocks(&state.doc);
        let b = wp
            .blocks
            .iter()
            .find(|b| b.text == "x2")
            .expect("block present");
        let (runs, _, _) = block_runs(b);
        let sup = runs.iter().find(|r| r.text == "2").expect("the '2' run");
        assert_eq!(
            sup.baseline,
            ui::richtext::BaselineShift::Super,
            "super mark maps to BaselineShift::Super"
        );
        let base = runs.iter().find(|r| r.text == "x").expect("the 'x' run");
        assert_eq!(
            base.baseline,
            ui::richtext::BaselineShift::None,
            "unmarked run stays on the baseline"
        );
    }

    #[test]
    fn line_spacing_2x_shapes_a_taller_layout() {
        // A wrapped multi-line paragraph: single vs double spacing on the SAME
        // production path (`block_line_spacing` → `RichTextLayout::shape`'s
        // line_height_mult). Double spacing must produce a strictly taller layout.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let long = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua";
        wp.model_mut().insert_str(&mut state.doc, long);

        // Single-spaced layout via the production draw pass.
        pipeline_draw(&mut wp, &mut state, 700, 700);
        let single_h = wp
            .laid
            .iter()
            .find(|l| l.text == long)
            .map(|l| l.height)
            .expect("laid block");
        assert_eq!(
            block_line_spacing(wp.blocks.iter().find(|b| b.text == long).unwrap()),
            1.0,
            "default 1.0"
        );

        // Now set double spacing and redraw on the same path.
        wp.model_mut().set_line_spacing(&mut state.doc, 2.0);
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            block_line_spacing(wp.blocks.iter().find(|b| b.text == long).unwrap()),
            2.0,
            "line_spacing=2.0 read from the block attr the layout pass uses"
        );
        pipeline_draw(&mut wp, &mut state, 700, 700);
        let double_h = wp
            .laid
            .iter()
            .find(|l| l.text == long)
            .map(|l| l.height)
            .expect("laid block");
        assert!(
            double_h > single_h * 1.5,
            "double line spacing yields a much taller layout: single={single_h}, double={double_h}"
        );
    }

    #[test]
    fn layout_cache_reuses_unchanged_blocks_and_reshapes_after_edit() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "First paragraph here");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "Second paragraph here");

        // Cold frame: the cache is empty, so every block is shaped (a miss).
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.dbg_layout_hits, 0, "cold draw cannot reuse anything");
        assert_eq!(
            wp.dbg_layout_misses, 2,
            "both paragraphs shaped on the first frame"
        );
        assert_eq!(wp.layout_cache.len(), 2, "both blocks cached");

        // Warm frame, no edits: every block's key matches, so all are reused and
        // nothing is re-shaped — this is the per-frame O(n) shaping we eliminated.
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(
            wp.dbg_layout_hits, 2,
            "unchanged re-draw reuses every cached layout"
        );
        assert_eq!(
            wp.dbg_layout_misses, 0,
            "no re-shaping on an unchanged frame"
        );

        // Edit one block (caret is in the 2nd paragraph): only that block's key
        // changes, so only it re-shapes; the untouched block is still reused.
        wp.model_mut().insert_str(&mut state.doc, "!");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(
            wp.dbg_layout_misses, 1,
            "only the edited block is re-shaped"
        );
        assert_eq!(wp.dbg_layout_hits, 1, "the untouched block is still reused");

        // A margin change alters the wrap width (the page is fixed-width, so the
        // window size does not — only the margins do). Wrap width is part of the
        // key, so every block must re-shape at the new measure.
        wp.margins = Edges::uniform(140.0);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(
            wp.dbg_layout_misses, 2,
            "a wrap-width change re-shapes every block"
        );
        assert_eq!(
            wp.dbg_layout_hits, 0,
            "no block can be reused at a new wrap width"
        );
    }

    #[test]
    fn find_and_replace_all_works() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "hello world hello there");
        wp.refresh_blocks(&state.doc);

        wp.find.set_queries_for_tests("hello", "hi");
        wp.find.open(None, &wp.blocks);
        assert_eq!(
            wp.find.matches().len(),
            2,
            "case-insensitive search finds both"
        );

        wp.replace_all(&mut state.doc);
        wp.refresh_blocks(&state.doc);
        assert_eq!(wp.blocks[0].text, "hi world hi there");
        assert!(
            wp.find.matches().is_empty(),
            "no matches remain after replace-all"
        );
    }

    #[test]
    fn styles_panel_renders_hits_and_picks_a_style() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "A paragraph");
        wp.styles_panel_open = true;
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // An explicitly-open panel renders clickable targets.
        assert!(wp.styles_panel_open);
        assert!(
            !wp.panel_hits.is_empty(),
            "panel rendered clickable targets"
        );
        assert_eq!(wp.focused_style_key().as_deref(), Some("body"));

        // Picking the Quote style makes the caret block adopt it (component-instance).
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("quote".into()));
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            wp.focused_style_key().as_deref(),
            Some("quote"),
            "block adopted the picked style"
        );
    }

    #[test]
    fn editing_a_style_updates_every_instance() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "first");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "second");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let before = wp.model().stylesheet(&state.doc).get("body").size;
        // Bump the body style's size via the panel stepper.
        wp.apply_panel_hit(&mut state.doc, PanelHit::NumDelta(NumField::Size, 5.0));
        let after = wp.model().stylesheet(&state.doc).get("body").size;
        assert_eq!(
            after,
            before + 5.0,
            "edited style size persisted to the document"
        );

        // Both paragraphs are Body, so both re-render at the new size.
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.laid.len(), 2);
        assert!(
            wp.laid.iter().all(|l| (l.size - after).abs() < 0.01),
            "every Body block follows the style edit"
        );
    }

    #[test]
    fn typing_into_a_numeric_field_commits() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "x");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // Click-to-type: focus the Size field, type "40".
        wp.editing_field = Some(NumField::Size);
        wp.edit_buf = "40".into();
        wp.apply_edit_buf_live();
        // Live preview mutated the widget sheet (not yet the document).
        assert_eq!(wp.sheet.get("body").size, 40.0);
        wp.commit_num_edit(&mut state.doc);
        assert!(wp.editing_field.is_none(), "Enter blurs the field");
        assert_eq!(
            wp.model().stylesheet(&state.doc).get("body").size,
            40.0,
            "typed value persisted"
        );
    }

    #[test]
    fn new_style_from_this_duplicates_and_applies() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        wp.set_styles_panel_open(true);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let n_before = wp.sheet.order().len();
        wp.apply_panel_hit(&mut state.doc, PanelHit::NewStyle);
        let sheet = wp.model().stylesheet(&state.doc);
        assert_eq!(
            sheet.order().len(),
            n_before + 1,
            "a new style id was added"
        );
        wp.refresh_blocks(&state.doc);
        let key = wp.focused_style_key().unwrap();
        assert!(
            key.starts_with("custom"),
            "block adopted the duplicated style, got {key}"
        );
    }

    #[test]
    fn styles_panel_toggles_off() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        assert!(
            !wp.styles_panel_open,
            "the first-launch writing surface is uncluttered"
        );
        wp.apply_tool(&mut state.doc, ToolAction::ToggleStyles);
        assert!(wp.styles_panel_open, "toolbar toggle reveals the panel");
        wp.apply_tool(&mut state.doc, ToolAction::ToggleStyles);
        assert!(!wp.styles_panel_open, "toolbar toggle dismisses the panel");
    }

    // ---- toolbar ⇄ panel reorganization: paragraph controls belong to the
    // side panel; the toolbar is direct character formatting only (no dupes) ----

    /// The toolbar carries only direct character formatting + insert/view
    /// controls; paragraph-style presets live in the side panel's style picker.
    /// Asserted on `ToolAction` variants — those variants no longer exist, so a
    /// regression re-adding them fails to compile against the allowed-set below.
    #[test]
    fn toolbar_drops_paragraph_controls_and_keeps_character_formatting() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        pipeline_draw(&mut wp, &mut state, 1200, 760);

        let actions: Vec<ToolAction> = wp.toolbar.iter().map(|(a, _)| *a).collect();
        // Character formatting + the direct font-size nudges are present.
        for kept in [
            ToolAction::Bold,
            ToolAction::Italic,
            ToolAction::Underline,
            ToolAction::Strike,
            ToolAction::Super,
            ToolAction::Sub,
            ToolAction::FontCycle,
            ToolAction::FontSizeInc,
            ToolAction::FontSizeDec,
        ] {
            assert!(
                actions.contains(&kept),
                "{kept:?} stays on the toolbar; toolbar={actions:?}"
            );
        }
        // Insert / view structural actions stay.
        assert!(
            actions.contains(&ToolAction::InsertTable),
            "table insert stays on the toolbar"
        );
        assert!(
            actions.contains(&ToolAction::ToggleStyles),
            "styles-panel toggle stays on the toolbar"
        );

        // Every toolbar action is direct character formatting or an insert/view
        // action — NONE switches the paragraph style/kind. (Alignment + line
        // spacing AND the Body/Heading/List style picker live in the side panel;
        // see `panel_owns_paragraph_alignment_and_the_style_editor` and
        // `panel_style_picker_switches_paragraph_kind`.)
        let is_char_or_structural = |a: &ToolAction| {
            matches!(
                a,
                ToolAction::Bold
                    | ToolAction::Italic
                    | ToolAction::Underline
                    | ToolAction::Strike
                    | ToolAction::Super
                    | ToolAction::Sub
                    | ToolAction::FontCycle
                    | ToolAction::FontSizeInc
                    | ToolAction::FontSizeDec
                    | ToolAction::TextColor(_)
                    | ToolAction::Highlight(_)
                    | ToolAction::InsertImage
                    | ToolAction::InsertTable
                    | ToolAction::TableRowAdd
                    | ToolAction::TableRowDel
                    | ToolAction::TableColAdd
                    | ToolAction::TableColDel
                    | ToolAction::TableDelete
                    | ToolAction::ToggleStyles
                    | ToolAction::ToggleRuler
                    | ToolAction::ToggleOutline
                    | ToolAction::ToggleHistory
                    | ToolAction::InsertToc
                    | ToolAction::NewMenu
            )
        };
        for a in &actions {
            assert!(
                is_char_or_structural(a),
                "unexpected paragraph-level control on the toolbar: {a:?}"
            );
        }
    }

    /// Alignment now applies to the current paragraph through the side panel's
    /// "This paragraph" section — an instance `align` attr on the caret block,
    /// read back on the production layout path — and does NOT edit the style.
    #[test]
    fn alignment_applies_to_the_paragraph_via_the_panel() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "centre me");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(
            wp.current_align(),
            "left",
            "paragraphs are left-aligned by default"
        );

        // Apply Center on the real dispatch path the panel registers.
        wp.apply_panel_hit(&mut state.doc, PanelHit::ParaAlign("center"));
        assert_eq!(
            wp.current_align(),
            "center",
            "the current paragraph is now centred"
        );
        // It is a per-paragraph (instance) attr, not an edit of the Body style.
        assert_eq!(
            wp.model().stylesheet(&state.doc).get("body").align,
            "left",
            "the Body style default alignment is untouched — the panel edited the paragraph, not the preset"
        );
    }

    /// Line spacing likewise cycles the current paragraph from the panel (1.0 →
    /// 1.15), on the same `caret_line_spacing` path the layout pass reads.
    #[test]
    fn line_spacing_applies_to_the_paragraph_via_the_panel() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "spaced");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(
            (wp.model().caret_line_spacing(&state.doc) - 1.0).abs() < 1e-9,
            "single spacing by default"
        );

        wp.apply_panel_hit(&mut state.doc, PanelHit::ParaLineSpacing);
        assert!(
            (wp.model().caret_line_spacing(&state.doc) - 1.15).abs() < 1e-9,
            "cycled to the next preset"
        );
    }

    /// The panel registers the paragraph-alignment controls (all four) + line
    /// spacing every frame AND still exposes the named-style editor — the two
    /// clearly-labelled halves ("this paragraph" vs "this style"), so alignment
    /// is not duplicated between the toolbar and the panel.
    #[test]
    fn panel_owns_paragraph_alignment_and_the_style_editor() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        wp.set_styles_panel_open(true);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let has = |want: &PanelHit| wp.panel_hits.iter().any(|(_, h)| h == want);
        for a in ["left", "center", "right", "justify"] {
            assert!(
                has(&PanelHit::ParaAlign(a)),
                "the panel owns paragraph alignment `{a}`"
            );
        }
        assert!(
            has(&PanelHit::ParaLineSpacing),
            "the panel owns paragraph line spacing"
        );
        // The named-style editor is still present in the same panel.
        assert!(
            has(&PanelHit::ToggleBold),
            "the style editor stays (weight toggle present)"
        );
        assert!(
            wp.panel_hits
                .iter()
                .any(|(_, h)| matches!(h, PanelHit::Num(NumField::Size))),
            "the style Size field stays"
        );
    }

    /// The panel's style picker is the authoritative paragraph-style control.
    /// Picking a built-in style switches the block *kind* too — "Heading 1"
    /// yields a real heading that shows up in the outline, "Body" reverts to a
    /// paragraph. Runs the production `apply_panel_hit` → `Pick` dispatch,
    /// asserting on the materialized block kind, not a helper.
    #[test]
    fn panel_style_picker_switches_paragraph_kind() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "make me a heading");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let block_kind = |wp: &WordProcessorWidget, st: &ui::State| -> (String, Option<i64>) {
            let c = wp.model().containers.clone().unwrap();
            let b = qdoc::model::materialize(&st.doc, &c)
                .blocks
                .into_iter()
                .next()
                .unwrap();
            let level = match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => Some(*n),
                _ => None,
            };
            (b.kind, level)
        };

        // A fresh paragraph starts as `paragraph` and is absent from the outline.
        assert_eq!(block_kind(&wp, &state).0, "paragraph");
        assert!(wp.outline_entries().is_empty(), "no headings yet");

        // Pick "Heading 1" from the panel → a REAL heading (kind + level), and it
        // now appears in the document outline / TOC source.
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("heading1".into()));
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(
            block_kind(&wp, &state),
            ("heading".into(), Some(1)),
            "picker made it a level-1 heading"
        );
        assert_eq!(
            wp.outline_entries().len(),
            1,
            "the heading is now in the outline"
        );

        // Pick "List Item" → a real list item (not a heading anymore).
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("list".into()));
        assert_eq!(
            block_kind(&wp, &state).0,
            "list_item",
            "picker made it a list item"
        );
        assert!(
            wp.outline_entries().is_empty(),
            "a list item is not a heading"
        );

        // Pick "Body" → back to a plain paragraph.
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("body".into()));
        assert_eq!(
            block_kind(&wp, &state).0,
            "paragraph",
            "picker restored the paragraph kind"
        );
    }

    /// Direct font size is character formatting on the selection — the toolbar's
    /// A+ writes a per-run `size` mark, distinct from (and leaving untouched) the
    /// style's default size in the panel.
    #[test]
    fn font_size_applies_to_the_selection_from_the_toolbar() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Hello");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let style_size = wp.model().stylesheet(&state.doc).get("body").size;
        let base = wp.current_font_size();
        assert!(
            (base - style_size).abs() < 0.01,
            "with no run mark the effective size is the style default"
        );

        wp.apply_tool(&mut state.doc, ToolAction::FontSizeInc);
        wp.refresh_blocks(&state.doc);
        let up = wp.current_font_size();
        assert!(up > base, "A+ enlarges the selection: {base} -> {up}");
        // The nudge is a per-run character mark, not an edit of the Body style.
        assert!(
            (wp.model().stylesheet(&state.doc).get("body").size - style_size).abs() < 0.01,
            "the Body style default size is untouched — direct formatting only"
        );
    }

    /// Character formatting from the toolbar still applies to the selection
    /// (Bold over a select-all range), unchanged by the reorganization.
    #[test]
    fn bold_still_applies_to_the_selection_from_the_toolbar() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "bold me");
        wp.model_mut().select_all(&state.doc);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(!wp.mark_active_in_selection("strong"), "not bold yet");

        wp.apply_tool(&mut state.doc, ToolAction::Bold);
        wp.refresh_blocks(&state.doc);
        assert!(
            wp.mark_active_in_selection("strong"),
            "the whole selection is now bold"
        );
    }

    #[test]
    fn draw_renders_headings_and_bold() {
        use qdoc::{FormatProvider, MarkdownCodec};
        let mut state = ui::State::new();
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            MarkdownCodec
                .import(b"# Title\n\nHello **bold** world\n", &mut dm)
                .unwrap();
        }
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(wp.laid.len() >= 2);
        assert!(wp.blocks.iter().any(|b| b.kind == "heading"));
        assert!(
            wp.blocks
                .iter()
                .any(|b| b.marks.iter().any(|m| m.name == "strong"))
        );
    }

    /// Build a doc directly in `state` from a closure over a `DocMut`.
    fn with_doc(state: &mut ui::State, f: impl FnOnce(&mut qdoc::DocMut)) {
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        f(&mut dm);
    }

    #[test]
    fn nested_list_items_indent_and_pick_glyph_by_level() {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let a = dm.insert_block(None, "list_item");
            dm.splice_text(&a, 0, 0, "a");
            let b = dm.insert_block(Some(&a), "list_item");
            dm.splice_text(&b, 0, 0, "b");
            dm.set_attr(&b, "level", 1_i64);
            let cc = dm.insert_block(Some(&b), "list_item");
            dm.splice_text(&cc, 0, 0, "c");
            dm.set_attr(&cc, "level", 2_i64);
        });
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        let la = wp.laid.iter().find(|l| l.text == "a").unwrap();
        let lb = wp.laid.iter().find(|l| l.text == "b").unwrap();
        let lc = wp.laid.iter().find(|l| l.text == "c").unwrap();
        assert!(
            lb.text_x > la.text_x && lc.text_x > lb.text_x,
            "deeper levels indent further"
        );
        // Unordered glyphs cycle • ◦ ▪ by depth.
        assert_eq!(la.bullet.as_deref(), Some("•"));
        assert_eq!(lb.bullet.as_deref(), Some("◦"));
        assert_eq!(lc.bullet.as_deref(), Some("▪"));
    }

    #[test]
    fn ordered_list_items_number_per_level() {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let mut prev: Option<String> = None;
            for (txt, lvl) in [
                ("one", 0i64),
                ("two", 0),
                ("two-a", 1),
                ("two-b", 1),
                ("three", 0),
            ] {
                let id = dm.insert_block(prev.as_deref(), "list_item");
                dm.splice_text(&id, 0, 0, txt);
                dm.set_attr(&id, "ordered", true);
                if lvl > 0 {
                    dm.set_attr(&id, "level", lvl);
                }
                prev = Some(id);
            }
        });
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 900);
        let num = |t: &str| {
            wp.laid
                .iter()
                .find(|l| l.text == t)
                .and_then(|l| l.bullet.clone())
        };
        assert_eq!(num("one").as_deref(), Some("1."));
        assert_eq!(num("two").as_deref(), Some("2."));
        // The nested ordered level restarts its own counter.
        assert_eq!(num("two-a").as_deref(), Some("1."));
        assert_eq!(num("two-b").as_deref(), Some("2."));
        // Returning to level 0 continues the outer numbering.
        assert_eq!(num("three").as_deref(), Some("3."));
    }

    #[test]
    fn long_document_paginates_across_discrete_pages() {
        // Geometry mirrors `draw`: a fresh widget keeps the default uniform 72px
        // margins, find bar closed (find_h = 0), so canvas top = the toolbar's
        // actual (possibly multi-row) height. The toolbar wraps on a narrow
        // window, so derive `page_top` from the same `toolbar_height` the
        // production `draw` uses rather than hard-coding a single-row height.
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // Enough paragraphs to overflow a single US-Letter page.
        for i in 0..120 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Paragraph number {i} carries enough words to fill a line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // The canvas top sits below the (wrapped) toolbar AND the ruler band
        // (on by default) — the real offset the pagination cursor starts from.
        let ruler_h = if wp.ruler_on { RULER_H } else { 0.0 };
        let page_top = wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        // The content spans more than one page.
        let max_page = wp.laid.iter().map(|l| page_of(l.y_top)).max().unwrap();
        assert!(
            max_page >= 1,
            "a long document must occupy more than one page (got {max_page})"
        );

        // No laid block crosses a page boundary: each sits wholly within its page's
        // text band, so a page-end never bisects a line of text.
        for l in &wp.laid {
            let i = num::usize_of_i64(page_of(l.y_top).max(0));
            assert!(
                l.y_top >= pct(i) - 1.0,
                "block starts at/below its page's top margin"
            );
            assert!(
                l.y_top + l.height <= pcb(i) + 1.0,
                "block ends above its page's bottom margin (no straddle)"
            );
        }

        // The first block that lands on page 2 starts exactly at the page-2 top
        // margin, and the block before it (in document order) is still on page 1.
        let idx = wp
            .laid
            .iter()
            .position(|l| page_of(l.y_top) == 1)
            .expect("some block lands on page 2");
        assert!(idx > 0, "page 2 is preceded by page-1 content");
        assert!(
            (wp.laid[idx].y_top - pct(1)).abs() < 1.0,
            "the first page-2 block hugs the page-2 top margin"
        );
        assert_eq!(
            page_of(wp.laid[idx - 1].y_top),
            0,
            "the block before it is on page 1"
        );
    }

    /// A SINGLE paragraph taller than one page is split at a line boundary across
    /// pages (intra-block pagination): the lines that fit stay on page 1, the rest
    /// flow to page 2. The block stays one logical block (one id, one cache entry);
    /// only its visual rendering is split. Asserts on the production layout
    /// geometry (`wp.laid` slices), not a helper.
    #[test]
    fn single_paragraph_splits_at_line_boundary_across_pages() {
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // ONE paragraph with enough words to wrap to well over a page of lines.
        let mut para = String::new();
        for i in 0..400 {
            write!(para, "word{i} ").unwrap();
        }
        let para = para.trim_end().to_string();
        wp.model_mut().insert_str(&mut state.doc, &para);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let ruler_h = if wp.ruler_on { RULER_H } else { 0.0 };
        let page_top = wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        // The one paragraph produced multiple laid slices, all sharing its id.
        let bid = wp.blocks[0].id.clone();
        let slices: Vec<&Laid> = wp.laid.iter().filter(|l| l.id == bid).collect();
        assert!(
            slices.len() >= 2,
            "a too-tall paragraph splits into >=2 page slices (got {})",
            slices.len()
        );
        // One logical block, one layout cache entry — only the rendering is split.
        assert_eq!(
            wp.layout_cache.len(),
            1,
            "the split block keeps exactly one cache entry"
        );
        assert!(
            wp.layout_cache.contains_key(&bid),
            "the cache entry is keyed by the single block id"
        );

        // The first slice is on page 1 and its last line ends ABOVE the page-1
        // bottom text margin — the page boundary never bisects a line.
        let s0 = slices[0];
        assert_eq!(page_of(s0.y_top), 0, "the first slice is on page 1");
        assert!(
            s0.y_top + s0.height <= pcb(0) + 1.0,
            "page-1 slice ends above the bottom margin (no straddle): bottom={} margin={}",
            s0.y_top + s0.height,
            pcb(0)
        );

        // The second slice's first carried line starts at the page-2 TOP margin.
        let s1 = slices[1];
        assert_eq!(page_of(s1.y_top), 1, "the carried-over slice is on page 2");
        assert!(
            (s1.y_top - pct(1)).abs() < 1.0,
            "the first carried line hugs the page-2 top margin: top={} margin={}",
            s1.y_top,
            pct(1)
        );

        // The carried slice maps the layout origin ABOVE its band (so an internal
        // line N draws at `layout_y + line_top`), and clips to its own band.
        assert!(
            s1.layout_y < s1.y_top,
            "the carried slice's layout origin sits above its page-2 band"
        );
        assert!(
            s1.clip.is_some(),
            "a split slice clips its drawing to its band"
        );
        assert!(
            s0.clip.is_some(),
            "the first slice of a split block also clips its band"
        );
    }

    /// Caret hit-testing on a line that MOVED to page 2 by intra-block pagination
    /// returns the right offset: a pointer over the carried line resolves to the
    /// byte that the production layout places at that on-screen position. Drives
    /// the production `caret_from_pointer` path, asserts on its result.
    #[test]
    fn caret_hit_on_a_carried_line_returns_the_right_offset() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let mut para = String::new();
        for i in 0..400 {
            write!(para, "word{i} ").unwrap();
        }
        let para = para.trim_end().to_string();
        wp.model_mut().insert_str(&mut state.doc, &para);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let bid = wp.blocks[0].id.clone();
        // The carried-over slice (the first one on page 2 for this block).
        let s1 = wp
            .laid
            .iter()
            .filter(|l| l.id == bid)
            .find(|l| l.page == 1)
            .cloned()
            .map(|l| (l.text_x, l.layout_y, l.y_top, l.height))
            .expect("the paragraph spills onto page 2");
        let (text_x, layout_y, band_top, band_h) = s1;

        // Pick a byte whose shaped position falls inside the page-2 band — i.e. a
        // character that was carried over. Walk the layout's caret geometry to find
        // the first byte whose on-screen y lands in slice 2's band.
        let (key_byte, gx, gy) = {
            let (_, layout) = wp.layout_cache.get(&bid).expect("cached layout");
            let mut found = None;
            let mut byte = 0usize;
            while byte <= para.len() {
                let (cx, cy, _ch) = layout.caret_geometry(byte);
                let screen_y = layout_y + f64::from(cy);
                if screen_y >= band_top - 0.5 && screen_y < band_top + band_h {
                    // A non-leading byte on this line (so click x is unambiguous).
                    found = Some((byte, f64::from(cx), f64::from(cy)));
                    break;
                }
                // Advance to the next char boundary.
                byte += 1;
                while byte < para.len() && !para.is_char_boundary(byte) {
                    byte += 1;
                }
            }
            found.expect("some byte renders on the page-2 carried band")
        };
        let expected_char = byte_to_char(&para, key_byte);

        // Click a few px to the right of that glyph's left edge, on its line — a
        // point unambiguously inside one character — using the slice's on-screen
        // mapping (`text_x` for x, `layout_y + glyph_top` for y).
        let click = Point::new(text_x + gx + 2.0, layout_y + gy + 4.0);
        // The click must really be inside the page-2 band (sanity on the geometry).
        assert!(
            click.y >= band_top - 0.5 && click.y < band_top + band_h,
            "click lands in the carried band"
        );
        wp.caret_from_pointer(click, false);

        // The resolved caret is in the same (single) block, at the carried char.
        assert_eq!(
            wp.inner.caret.block, bid,
            "hit resolves to the one logical block"
        );
        let got = num::i64_of_usize(wp.inner.caret.offset);
        assert!(
            (got - num::i64_of_usize(expected_char)).abs() <= 1,
            "carried-line hit resolves to the right offset: got {got}, expected ~{expected_char}"
        );
    }

    #[test]
    fn footer_page_number_field_resolves_per_page() {
        // A footer "Page <field>" must render "Page 1" on page 1 and "Page 2" on
        // page 2 — the field placeholder substitutes the rendered page number.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // Build a footer with a page-number field via the production model API.
        let c = wp.model().containers.clone().unwrap();
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let f = dm.insert_block_in(qdoc::model::Region::Footer, None, "paragraph");
            dm.splice_text_in(qdoc::model::Region::Footer, &f, 0, 0, "Page ");
            dm.insert_page_number_field(qdoc::model::Region::Footer, &f, 5);
        }
        // Body long enough to span two pages.
        for i in 0..120 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Paragraph number {i} carries enough words to fill a line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // The footer is loaded from the snapshot and spans >1 page.
        assert_eq!(
            wp.footer.len(),
            1,
            "the footer block is materialized into the widget"
        );
        let ruler_h = if wp.ruler_on { RULER_H } else { 0.0 };
        let max_page = wp
            .laid
            .iter()
            .map(|l| {
                ((l.y_top + wp.scroll_y - (wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP))
                    / (MAX_PAGE_W * LETTER_RATIO + PAGE_GAP))
                    .floor() as i64
            })
            .max()
            .unwrap();
        assert!(
            max_page >= 1,
            "the document spans more than one page (got {max_page})"
        );

        // The production substitution path renders the page number per page: the
        // joined run text for the footer block is "Page 1" on page 1, "Page 2" on
        // page 2 — the placeholder is gone and the digits are present.
        let join = |page_no: usize| {
            running_head_runs(&wp.footer[0], page_no)
                .iter()
                .map(|r| r.text.clone())
                .collect::<String>()
        };
        assert_eq!(join(1), "Page 1", "page 1 footer resolves to 'Page 1'");
        assert_eq!(join(2), "Page 2", "page 2 footer resolves to 'Page 2'");
        assert!(
            !join(2).contains(qdoc::schema::FIELD_PLACEHOLDER),
            "the field placeholder is substituted, not rendered literally"
        );
    }

    /// Header / footer materialized text via the production snapshot path.
    fn header_text(wp: &WordProcessorWidget, state: &ui::State) -> Vec<String> {
        let c = wp.model().containers.clone().unwrap();
        qdoc::model::materialize_region(&state.doc, &c, qdoc::model::Region::Header)
            .into_iter()
            .map(|b| b.text)
            .collect()
    }
    fn body_text(wp: &WordProcessorWidget, state: &ui::State) -> Vec<String> {
        let c = wp.model().containers.clone().unwrap();
        qdoc::model::materialize(&state.doc, &c)
            .blocks
            .into_iter()
            .map(|b| b.text)
            .collect()
    }

    /// Drive a real named-key press (e.g. Escape) through `handle_event`.
    fn press_named(wp: &mut WordProcessorWidget, state: &mut ui::State, key: NamedKey) {
        use winit::keyboard::{NativeKeyCode, PhysicalKey};
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.logical_key = Key::Named(key);
        ev.text = None;
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        wp.handle_event(
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// End-to-end through the **production** event path: click into the header
    /// band → the edit context switches to Header → typed characters land in the
    /// header container (asserted via `materialize_region`), not the body. Then a
    /// click back in the body returns the context to Body, and typing edits the
    /// body again.
    #[test]
    fn clicking_header_band_routes_typing_into_the_header() {
        let (mut state, mut wp) = bound();
        // Put some content in the body first (proves it is not what gets edited).
        wp.model_mut().insert_str(&mut state.doc, "body text");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(
            wp.model().region(),
            Region::Body,
            "starts in the body context"
        );

        // The cached header-band rect is content space; map its centre to a screen
        // point the way the production pointer→content mapping inverts (identity at
        // zoom 1.0, but go through the helper to be exact).
        let hb = wp.head_band_rect;
        assert!(hb.height() > 0.0, "header band rect was cached by draw");
        let centre_content = Point::new(f64::midpoint(hb.x0, hb.x1), f64::midpoint(hb.y0, hb.y1));
        let screen = Point::new(
            wp.content_x_to_screen(centre_content.x),
            wp.zoom.anchor().y + (centre_content.y - wp.zoom.anchor().y) * wp.zoom.factor(),
        );

        left_click(&mut wp, &mut state, screen);
        assert_eq!(
            wp.model().region(),
            Region::Header,
            "clicking the header band focuses the Header context"
        );

        // Type into the header — production keyboard path.
        for ch in "My Title".chars() {
            type_char(&mut wp, &mut state, ch);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // Assert on the materialized HEADER snapshot (the production path).
        assert_eq!(
            header_text(&wp, &state),
            vec!["My Title".to_string()],
            "typed text landed in the header container"
        );
        assert_eq!(
            body_text(&wp, &state),
            vec!["body text".to_string()],
            "the body is untouched"
        );
        // The band laid out the header block, so a caret can render in it.
        assert!(
            wp.region_laid.iter().any(|r| r.text == "My Title"),
            "the header band is laid out for the active context"
        );

        // Escape returns to the body; typing then edits the body again.
        press_named(&mut wp, &mut state, NamedKey::Escape);
        assert_eq!(
            wp.model().region(),
            Region::Body,
            "Escape exits header editing"
        );
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        wp.model_mut().move_end(&state.doc);
        for ch in "!".chars() {
            type_char(&mut wp, &mut state, ch);
        }
        assert_eq!(
            body_text(&wp, &state),
            vec!["body text!".to_string()],
            "after Escape, typing edits the body"
        );
        assert_eq!(
            header_text(&wp, &state),
            vec!["My Title".to_string()],
            "the header keeps its content"
        );
    }

    /// Rasterize one frame while the header is the active edit context and write a
    /// PNG (so the header-edit affordance — band wash + dashed outline + caret —
    /// can be eyeballed). Renders through the real five-pass draw; asserts a
    /// non-trivial image was produced. Output path is overridable via
    /// `$QOS_WP_HEADER_SHOT` for manual inspection.
    #[test]
    fn header_edit_mode_renders_a_frame() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(
            &mut state.doc,
            "The body paragraph recedes while the header is edited.",
        );
        // Focus the header and give it content through the production model API.
        wp.model_mut().focus_region(&mut state.doc, Region::Header);
        for ch in "Draft — Confidential".chars() {
            wp.model_mut().insert_str(&mut state.doc, &ch.to_string());
        }
        let (w, h) = (1000u32, 700u32);
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        wp.sync(&mut state.doc, &state.root);
        let child = wp.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(num::f32_of_u32(w)),
                        height: Dimension::length(num::f32_of_u32(h)),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(num::f32_of_u32(w)),
                        height: AvailableSpace::Definite(num::f32_of_u32(h)),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        wp.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        wp.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &permission,
        );
        // The header band laid out for the active context (so its caret + chrome
        // are part of this frame).
        assert!(
            wp.region_laid.iter().any(|r| r.text.contains("Draft")),
            "header band laid for the active context"
        );

        let out = std::env::var("QOS_WP_HEADER_SHOT")
            .unwrap_or_else(|_| format!("{}/header-edit-mode.png", std::env::temp_dir().display()));
        ui::offscreen::OffscreenRenderer::new(w, h).render_png(
            &scene,
            Color::from_rgb8(0x20, 0x22, 0x28),
            std::path::Path::new(&out),
        );
        let meta = std::fs::metadata(&out).expect("a PNG frame was written");
        assert!(
            meta.len() > 2000,
            "the rendered frame is a non-trivial PNG ({} bytes at {out})",
            meta.len()
        );
    }

    #[test]
    fn manual_page_break_starts_block_on_a_fresh_page() {
        // Geometry mirrors `draw` (default 72px margins, find bar closed).
        let mar = 72.0;
        let letter_h = MAX_PAGE_W * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // Two short paragraphs that both fit on page 1, then force a break so the
        // third starts on a fresh page even though it would otherwise fit.
        wp.model_mut()
            .insert_str(&mut state.doc, "First paragraph on page one.");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "Second paragraph still on page one.");
        // Ctrl+Enter behaviour: split + mark the new block to break before.
        wp.model_mut().insert_page_break(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "This paragraph is forced onto page two.");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let ruler_h = if wp.ruler_on { RULER_H } else { 0.0 };
        let page_top = wp.toolbar_height(1000.0) + ruler_h + PAGE_TOP;
        let pct = |i: usize| page_top + num::f64_of_usize(i) * page_pitch + mar; // page content top
        let page_of = |y: f64| num::i64_of_f64(((y - page_top) / page_pitch).floor());

        let broken = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("This paragraph is forced"))
            .expect("the broken block is laid out");
        assert_eq!(
            page_of(broken.y_top),
            1,
            "the page-break block lands on page 2"
        );
        assert!(
            (broken.y_top - pct(1)).abs() < 1.0,
            "it hugs the page-2 top margin (laid at a page top)"
        );

        // The two earlier paragraphs are still on page 1 (the break was manual, not
        // an overflow): proving the attr — not a full page of text — caused it.
        let first = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("First paragraph"))
            .unwrap();
        let second = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("Second paragraph"))
            .unwrap();
        assert_eq!(
            page_of(first.y_top),
            0,
            "the first paragraph stays on page 1"
        );
        assert_eq!(
            page_of(second.y_top),
            0,
            "the second paragraph stays on page 1"
        );
    }

    #[test]
    fn page_margins_persist_across_reload() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // A document with no recorded page setup uses the default margins.
        assert!(
            (wp.margins.left - 72.0).abs() < 1e-6,
            "fresh doc uses default margins"
        );

        // Edit margins (as a margin-guide drag would) and persist via the same
        // production path the drag-release handler uses.
        wp.margins = Edges {
            top: 33.0,
            right: 44.0,
            bottom: 55.0,
            left: 66.0,
        };
        wp.persist_margins(&mut state.doc);

        // A fresh widget bound to the *same document* reloads the persisted page
        // setup on sync — proving margins now live in the document, not the widget.
        let mut wp2 = WordProcessorWidget::new();
        wp2.sync(&mut state.doc, &state.root);
        assert!(
            (wp2.margins.top - 33.0).abs() < 1e-6,
            "top margin survived reload"
        );
        assert!(
            (wp2.margins.right - 44.0).abs() < 1e-6,
            "right margin survived reload"
        );
        assert!(
            (wp2.margins.bottom - 55.0).abs() < 1e-6,
            "bottom margin survived reload"
        );
        assert!(
            (wp2.margins.left - 66.0).abs() < 1e-6,
            "left margin survived reload"
        );
    }

    /// Inserting an image via the model and running the full draw pipeline lays
    /// out an `image` block and does not panic (decode → layout → draw).
    #[test]
    fn inserted_image_lays_out_and_draws() {
        // A minimal 2×3 RGB PNG (matches the model-test fixture).
        const TINY_PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3,
            8, 2, 0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73,
            69, 78, 68, 174, 66, 96, 130,
        ];
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "caption above");
        wp.model_mut().insert_image_bytes(&mut state.doc, TINY_PNG);
        // Re-sync so the widget's block/blob caches see the new image block.
        wp.sync(&mut state.doc, &state.root);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(
            wp.laid.iter().any(|l| l.kind == "image"),
            "an image block was laid out"
        );
    }

    /// An image block carrying explicit `width`/`height` attrs renders at that size
    /// (the requested width, clamped to the content column) — not auto-fit to the
    /// intrinsic decoded size. Asserts on the production `Laid` geometry.
    #[test]
    fn explicit_image_size_is_honored() {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let hash = dm.put_blob(A11Y_TINY_PNG);
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "caption");
            let img = dm.insert_block(Some(&p), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            // Request a 200×120 display size — far larger than the 2×3 intrinsic.
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 120_i64);
        });
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1100, 900);

        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        assert!(
            (img.width - 200.0).abs() < 0.5,
            "image uses its explicit width, got {}",
            img.width
        );
        assert!(
            (img.height - 120.0).abs() < 0.5,
            "image uses its explicit height, got {}",
            img.height
        );
    }

    /// A `paragraph`-anchored, `wrap=right` floated image pins to the RIGHT edge of
    /// the content column, and the paragraph that follows is laid at a reduced width
    /// (beside the image) keeping its left x-origin. Asserts on production geometry.
    #[test]
    fn right_floated_image_narrows_following_paragraph() {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            // A reference full-width paragraph (above the float) to read the content
            // column geometry from, then the floated image, then the wrapped text.
            let r = dm.insert_block(None, "paragraph");
            dm.splice_text(&r, 0, 0, "reference paragraph at full content width");
            let hash = dm.put_blob(A11Y_TINY_PNG);
            let img = dm.insert_block(Some(&r), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "right");
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 160_i64);
            let beside = dm.insert_block(Some(&img), "paragraph");
            dm.splice_text(
                &beside,
                0,
                0,
                "this paragraph should flow beside the floated image to its left",
            );
        });
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1100, 900);

        let reference = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("reference"))
            .expect("ref laid");
        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        let beside = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("this paragraph"))
            .expect("wrapped para laid");

        // Content column: the reference paragraph spans it fully.
        let content_left = reference.text_x;
        let content_right = reference.text_x + reference.width;

        // The image sits at the RIGHT edge of the content column.
        let img_right = img.text_x + img.width;
        assert!(
            (img_right - content_right).abs() < 1.0,
            "right-floated image hugs the content right edge: img_right={img_right} content_right={content_right}"
        );
        assert!(
            img.text_x > content_left + 1.0,
            "the image is offset toward the right, not at the left edge"
        );

        // The wrapped paragraph keeps its LEFT origin (right float → text stays left)
        // and is narrowed to clear the image.
        assert!(
            (beside.text_x - content_left).abs() < 0.5,
            "right-float text keeps its left x-origin: {} vs {}",
            beside.text_x,
            content_left
        );
        assert!(
            beside.width < reference.width - 100.0,
            "wrapped paragraph is narrowed beside the image: beside={} reference={}",
            beside.width,
            reference.width
        );
        // Specifically: reduced by ~ image width + gutter.
        assert!(
            beside.width <= content_right - content_left - 200.0 + 0.5,
            "narrowed by at least the image width"
        );
    }

    /// A `wrap=left` floated image pins to the LEFT edge and pushes the following
    /// paragraph's text origin to the RIGHT (beside the image), still narrowed.
    #[test]
    fn left_floated_image_offsets_following_paragraph_right() {
        let mut state = ui::State::new();
        with_doc(&mut state, |dm| {
            let r = dm.insert_block(None, "paragraph");
            dm.splice_text(&r, 0, 0, "reference paragraph at full content width");
            let hash = dm.put_blob(A11Y_TINY_PNG);
            let img = dm.insert_block(Some(&r), "image");
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "left");
            dm.set_attr(&img, "width", 200_i64);
            dm.set_attr(&img, "height", 160_i64);
            let beside = dm.insert_block(Some(&img), "paragraph");
            dm.splice_text(
                &beside,
                0,
                0,
                "this paragraph should flow to the right of the floated image",
            );
        });
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1100, 900);

        let reference = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("reference"))
            .expect("ref laid");
        let img = wp
            .laid
            .iter()
            .find(|l| l.kind == "image")
            .expect("image laid out");
        let beside = wp
            .laid
            .iter()
            .find(|l| l.text.starts_with("this paragraph"))
            .expect("wrapped para laid");

        let content_left = reference.text_x;
        // The image sits at the LEFT edge.
        assert!(
            (img.text_x - content_left).abs() < 1.0,
            "left-floated image hugs the content left edge"
        );
        // The wrapped paragraph's origin is pushed right (beside the image) and is narrowed.
        assert!(
            beside.text_x > content_left + 199.0,
            "left-float text is offset right past the image: {} vs {}",
            beside.text_x,
            content_left
        );
        assert!(
            beside.width < reference.width - 100.0,
            "wrapped paragraph is narrowed beside the image"
        );
        // The narrowed text still ends within the content column (no overflow off the right edge).
        assert!(
            beside.text_x + beside.width <= content_left + reference.width + 0.5,
            "narrowed text fits within the content column"
        );
    }

    // ---- Zoom ---------------------------------------------------------------

    #[test]
    fn zoom_scales_the_content_transform_about_the_anchor() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "zoom me");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // At 100% the content transform is the identity scale (no magnification).
        let base = Affine::IDENTITY;
        let id_ctf = wp.content_transform(base);
        assert!(
            (id_ctf.as_coeffs()[0] - 1.0).abs() < 1e-9,
            "100% zoom does not scale"
        );

        // Zoom in: the content transform now scales by `zoom` about the anchor,
        // so a point at the anchor is fixed and points below it move down.
        assert!(wp.set_zoom(2.0));
        let ctf = wp.content_transform(base);
        let coeffs = ctf.as_coeffs();
        assert!(
            (coeffs[0] - 2.0).abs() < 1e-9 && (coeffs[3] - 2.0).abs() < 1e-9,
            "zoom doubles the scale"
        );
        // The anchor maps to itself.
        let a = wp.zoom.anchor();
        let mapped_anchor = ctf * a;
        assert!(
            (mapped_anchor.x - a.x).abs() < 1e-6 && (mapped_anchor.y - a.y).abs() < 1e-6,
            "the anchor is fixed under zoom"
        );
        // A point one content-unit below the anchor lands two screen-units below.
        let p = Point::new(a.x, a.y + 100.0);
        let mp = ctf * p;
        assert!(
            (mp.y - (a.y + 200.0)).abs() < 1e-6,
            "content below the anchor is magnified downward"
        );
    }

    #[test]
    fn click_maps_to_the_same_caret_under_zoom() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "first paragraph");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut()
            .insert_str(&mut state.doc, "second paragraph");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // A content-space point inside the second block (its `Laid` rect lives in
        // unscaled content coordinates regardless of zoom).
        let l2 = wp
            .laid
            .iter()
            .find(|l| l.text == "second paragraph")
            .expect("2nd block laid out");
        let target_block = l2.id.clone();
        let content_pt = Point::new(l2.text_x + 30.0, l2.y_top + l2.height / 2.0);

        // Sanity: clicking that content point lands the caret in the 2nd block.
        wp.caret_from_pointer(content_pt, false);
        assert_eq!(
            wp.inner.caret.block, target_block,
            "baseline content-space click hits the 2nd block"
        );
        let off_unzoomed = wp.inner.caret.offset;

        // Now zoom to 175%. The screen position of that same content point is the
        // content transform applied to it. Feeding the screen point back through
        // the production inverse (`pointer_to_content`) must recover the click,
        // so `handle_event`'s caret hit-test lands on the same block + offset.
        assert!(wp.set_zoom(1.75));
        pipeline_draw(&mut wp, &mut state, 1000, 700); // recompute `zoom_anchor`
        let screen_pt = wp.content_transform(Affine::IDENTITY) * content_pt;
        let recovered = wp.pointer_to_content(screen_pt);
        assert!(
            (recovered.x - content_pt.x).abs() < 1e-6 && (recovered.y - content_pt.y).abs() < 1e-6,
            "inverse recovers the content point"
        );
        wp.caret_from_pointer(recovered, false);
        assert_eq!(
            wp.inner.caret.block, target_block,
            "zoomed click still hits the 2nd block"
        );
        assert_eq!(
            wp.inner.caret.offset, off_unzoomed,
            "zoomed click lands on the same caret offset"
        );
    }

    #[test]
    fn zoom_clamps_and_resets() {
        let mut wp = WordProcessorWidget::new();
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "default zoom is 100%"
        );
        // Clamp at the bounds.
        for _ in 0..100 {
            wp.zoom_by(-ZOOM_STEP);
        }
        assert!(
            (wp.zoom.factor() - ZOOM_MIN).abs() < 1e-6,
            "zoom clamps to the minimum"
        );
        for _ in 0..100 {
            wp.zoom_by(ZOOM_STEP);
        }
        assert!(
            (wp.zoom.factor() - ZOOM_MAX).abs() < 1e-6,
            "zoom clamps to the maximum"
        );
        wp.set_zoom(1.0);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "reset returns to 100%"
        );
    }

    /// The zoom stepper lives right-aligned in the toolbar. Driving its buttons
    /// through the real event handler must zoom out/in/reset, proving
    /// the component's hit-testing fires correctly from the toolbar band.
    #[test]
    fn toolbar_zoom_stepper_drives_zoom_from_its_new_home() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // Every zoom button sits inside the toolbar band, not a bottom strip.
        let tb_h = wp.toolbar_height(1000.0);
        let rects = wp.zoom.hit_rects();
        assert_eq!(rects.len(), 3, "the −/%/+ stepper registered three targets");
        for r in &rects {
            assert!(
                r.y1 <= tb_h + 1.0,
                "zoom button is in the toolbar (rect={r:?}, toolbar_h={tb_h})"
            );
            assert!(
                r.x1 <= wp.rect.x1,
                "zoom button is within the window (rect={r:?})"
            );
        }

        // Component order is −, percentage/reset, +.
        let center = |wp: &WordProcessorWidget, index: usize| -> Point {
            let rects = wp.zoom.hit_rects();
            let r = &rects[index];
            Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
        };

        // Click + : zoom in one step.
        let p = center(&wp, 2);
        left_click(&mut wp, &mut state, p);
        assert!(
            (wp.zoom.factor() - (1.0 + ZOOM_STEP)).abs() < 1e-6,
            "clicking + zooms in one step: {}",
            wp.zoom.factor()
        );

        // Click − : back to 100%.
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let p = center(&wp, 0);
        left_click(&mut wp, &mut state, p);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-6,
            "clicking − zooms out one step: {}",
            wp.zoom.factor()
        );

        // Click the percentage chip: reset to 100% from any zoom.
        wp.set_zoom(1.6);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let p = center(&wp, 1);
        left_click(&mut wp, &mut state, p);
        assert!(
            (wp.zoom.factor() - 1.0).abs() < 1e-9,
            "clicking the % chip resets to 100%: {}",
            wp.zoom.factor()
        );
    }

    #[test]
    fn zoom_in_grows_the_scrollable_range() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        for i in 0..40 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Paragraph {i} with several words to fill the line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let scroll_1x = wp.max_scroll;
        assert!(scroll_1x > 0.0, "the document already overflows at 100%");
        wp.set_zoom(2.0);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(
            wp.max_scroll > scroll_1x,
            "zooming in enlarges the content, so there is more to scroll"
        );
    }

    // ---- Responsive toolbar -------------------------------------------------

    #[test]
    fn toolbar_packs_into_one_row_when_it_fits() {
        // A generous width fits every control on a single row.
        let widths = vec![34.0, 30.0, 32.0, 30.0, 42.0, 42.0, 58.0];
        let starts = vec![false, false, false, false, true, false, false];
        let rows = pack_toolbar_rows(&widths, &starts, 1400.0);
        assert_eq!(rows.len(), 1, "all items fit one row at 1400px");
        assert_eq!(rows[0], (0, widths.len()));
    }

    #[test]
    fn toolbar_wraps_to_more_than_one_row_when_items_exceed_width() {
        // Items whose total far exceeds the available width must wrap.
        let widths = vec![100.0; 12]; // 12 × 100 = 1200px of buttons + gaps + pads
        let starts = vec![false; 12];
        let rows = pack_toolbar_rows(&widths, &starts, 400.0);
        assert!(
            rows.len() > 1,
            "items exceeding the width wrap to >1 row (got {})",
            rows.len()
        );
        // Every item is assigned exactly once, in order, with no gaps.
        assert_eq!(rows.first().unwrap().0, 0);
        assert_eq!(rows.last().unwrap().1, widths.len());
        for w in rows.windows(2) {
            assert_eq!(w[0].1, w[1].0, "row ranges are contiguous");
        }
        // No row's packed width exceeds the usable width (mirrors `draw`).
        let usable = 400.0 - TOOLBAR_PAD - TOOLBAR_RIGHT_PAD;
        for (s, e) in &rows {
            let mut x = 0.0;
            for i in *s..*e {
                x += widths[i] + TOOLBAR_GAP + if starts[i] { TOOLBAR_SEP } else { 0.0 };
            }
            // A row is allowed to overflow only if it holds a single over-wide item.
            assert!(
                x <= usable + 1.0 || e - s == 1,
                "row [{s},{e}) overflows usable width"
            );
        }
    }

    #[test]
    fn real_toolbar_wraps_on_a_narrow_window_and_sets_canvas_offset() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hi");
        // The full inline-formatting toolbar is wide; on a 760px window it must
        // wrap, and the reserved canvas-top height grows by whole rows.
        let narrow = wp.toolbar_height(760.0);
        let wide = wp.toolbar_height(1600.0);
        assert!(
            narrow > wide,
            "a narrow window reserves a taller (wrapped) toolbar"
        );
        assert!(
            (wide - TOOLBAR_H).abs() < 1e-9,
            "a wide window keeps a single-row toolbar"
        );
        assert_eq!(
            narrow % TOOLBAR_H,
            0.0,
            "the toolbar height is a whole number of rows"
        );
        assert!(
            narrow >= 2.0 * TOOLBAR_H,
            "760px wraps to at least two rows"
        );
    }

    #[test]
    fn document_status_stays_in_bottom_chrome_at_narrow_and_wide_zoomed_widths() {
        let (mut state, mut wp) = populated();
        for (width, zoom) in [(700_u32, 0.81), (1100, 1.0)] {
            wp.auto_fit_page = false;
            wp.zoom.set(zoom);
            pipeline_draw(&mut wp, &mut state, width, 900);
            let bar = Rect::new(0.0, 868.0, f64::from(width), 900.0);
            let label = wp.status_count_label();
            let rect = WordProcessorWidget::layout_bar_status_rect(bar, 210.0, &label)
                .expect("the count fits after the layout controls");
            assert!(
                rect.y0 >= bar.y0 && rect.y1 <= bar.y1 && rect.x1 <= bar.x1,
                "{width}px status is entirely inside the bottom chrome: {rect:?}"
            );
            assert!(
                rect.y0 >= wp.rect.y1 - LAYOUT_BAR_H,
                "{width}px status cannot overlap the page even at {zoom:.0}% zoom"
            );
        }
    }

    #[test]
    fn narrow_first_open_fits_the_page_until_the_user_zooms() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "Readable narrow document");
        pipeline_draw(&mut wp, &mut state, 640, 800);
        assert!(
            wp.zoom.factor() < 1.0,
            "narrow canvas automatically fits paper"
        );

        let ctf = wp.content_transform(Affine::IDENTITY);
        let left = ctf * Point::new(wp.page_rect.x0, wp.page_rect.y0);
        let right = ctf * Point::new(wp.page_rect.x1, wp.page_rect.y0);
        assert!(
            left.x >= wp.rect.x0 - 1.0,
            "left paper edge remains visible"
        );
        assert!(
            right.x <= wp.rect.x1 + 1.0,
            "right paper edge remains visible"
        );

        wp.set_zoom(1.0);
        pipeline_draw(&mut wp, &mut state, 640, 800);
        assert_eq!(wp.zoom.factor(), 1.0, "explicit zoom disables auto-fit");
    }

    /// The swatch strips are right-aligned on the last toolbar row, painted
    /// outside the chrome button packer, which reserves their width as a
    /// phantom item. At a width where buttons alone would fill one row, the
    /// last button (TOC) must not overlap the swatches — proving the
    /// reservation works on the real draw path, not just in the width model.
    #[test]
    fn toolbar_swatches_never_overlap_the_buttons() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hi");
        // 1200px: buttons alone fit one row, so the naive layout would right-align
        // the swatches straight over the trailing buttons.
        pipeline_draw(&mut wp, &mut state, 1200, 760);

        let rect_of =
            |a: fn(&ToolAction) -> bool| wp.toolbar.iter().find(|(x, _)| a(x)).map(|(_, r)| *r);
        let toc = rect_of(|a| matches!(a, ToolAction::InsertToc)).expect("TOC button laid out");
        // The neutral text-colour swatch is the left-most colour swatch.
        let swatch = wp
            .toolbar
            .iter()
            .filter(|(a, _)| matches!(a, ToolAction::TextColor(_)))
            .map(|(_, r)| *r)
            .min_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap())
            .expect("colour swatches laid out");

        let disjoint_x = toc.x1 <= swatch.x0 + 0.5;
        let different_rows = toc.y1 <= swatch.y0 + 0.5 || swatch.y1 <= toc.y0 + 0.5;
        assert!(
            disjoint_x || different_rows,
            "TOC button {toc:?} must not overlap the colour swatches {swatch:?}"
        );
    }

    // ---- spell-check squiggles --------------------------------------------

    #[test]
    fn misspelled_word_yields_a_squiggle_span_at_its_x_range() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // "teh" is a classic misspelling flagged by the builtin dictionary; the
        // surrounding words are valid, so exactly one span is expected.
        wp.model_mut()
            .insert_str(&mut state.doc, "teh quick brown fox");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let l = wp
            .laid
            .iter()
            .find(|l| l.text == "teh quick brown fox")
            .expect("paragraph laid out");
        let id = l.id.clone();
        let text = l.text.clone();

        // The production spell path computes the misspelled byte spans for this
        // block (the same call `draw` makes to feed `draw_squiggle`).
        let spans = wp.block_spell_spans(&id, &text);
        assert_eq!(
            spans,
            vec![(0usize, 3usize)],
            "exactly ‘teh’ at bytes [0,3) is flagged"
        );

        // The span maps to a concrete, positive on-screen x-range via the cached
        // shaped layout — exactly the geometry `draw` passes to `draw_squiggle`.
        let (_, layout) = wp
            .layout_cache
            .get(&id)
            .expect("block layout cached after draw");
        let rects = layout.selection_rects(0, 3);
        assert!(
            !rects.is_empty(),
            "the misspelled span resolves to at least one rect"
        );
        let (x0, _, x1, _) = rects[0];
        assert!(
            x1 > x0,
            "the squiggle covers a non-empty width: x0={x0} x1={x1}"
        );
        // ‘teh’ sits at the very start of the line, so its rect hugs the left edge.
        assert!(
            x0 < 4.0,
            "the first word's squiggle starts at the line's left edge: x0={x0}"
        );
    }

    #[test]
    fn spellcheck_toggle_off_suppresses_the_squiggle_pass() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "teh");
        assert!(wp.spellcheck_enabled(), "spell-check defaults ON");
        wp.toggle_spellcheck();
        assert!(!wp.spellcheck_enabled(), "toggle turns it OFF");
        // With the toggle off, a draw must not even build the dictionary (the lazy
        // OnceLock stays untouched for this widget — the per-widget handle is None).
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(
            wp.dict.is_none(),
            "spell-check off skips dictionary construction"
        );
    }

    #[test]
    fn document_autocorrect_is_reversible_at_a_space_boundary() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "teh");

        let correction = wp
            .autocorrect_before_boundary(&mut state.doc)
            .expect("‘teh’ is a high-confidence correction");
        wp.model_mut().insert_str(&mut state.doc, " ");
        wp.last_autocorrection = Some(DocumentAutocorrection {
            boundary_end: wp.inner.caret.offset,
            ..correction
        });
        assert_eq!(wp.inner.current_text(&state.doc), "the ");

        assert!(wp.undo_document_autocorrection(&mut state.doc));
        assert_eq!(wp.inner.current_text(&state.doc), "teh");
    }

    #[test]
    fn shared_clipboard_preserves_rich_paste_and_plain_paste_semantics() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        // Copy without a selection remains a no-op.
        clipboard_copy("sentinel");
        wp.clipboard_copy(&state.doc);
        assert_eq!(clipboard_paste().as_deref(), Some("sentinel"));

        wp.inner.insert_str(&mut state.doc, "hello");

        // Copy a bold selection. The system-facing clipboard carries plain text;
        // the widget's matching sidecar carries the qdoc marks.
        wp.inner.caret.offset = 0;
        wp.inner.set_anchor();
        wp.inner.caret.offset = 5;
        wp.inner.toggle_mark(&mut state.doc, "strong");
        wp.inner.caret.offset = 0;
        wp.inner.clear_anchor();
        wp.inner.set_anchor();
        wp.inner.caret.offset = 5;
        wp.clipboard_copy(&state.doc);
        assert_eq!(clipboard_paste().as_deref(), Some("hello"));

        // A normal paste of the unchanged clipboard finds the sidecar and keeps
        // the mark.
        wp.inner.clear_anchor();
        wp.inner.move_end(&state.doc);
        wp.clipboard_paste(&mut state.doc);
        let containers = wp.inner.containers.clone().expect("bound document");
        let block = wp.inner.caret.block.clone();
        assert_eq!(wp.inner.current_text(&state.doc), "hellohello");
        assert!(qdoc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            5,
            10,
            "strong"
        ));

        // Paste-as-plain reads through the same shared backend but deliberately
        // ignores that matching rich sidecar. Use a fresh paragraph so the
        // destination's own active character style cannot affect the assertion.
        wp.inner.enter(&mut state.doc);
        wp.clipboard_paste_plain(&mut state.doc);
        let block = wp.inner.caret.block.clone();
        assert_eq!(wp.inner.current_text(&state.doc), "hello");
        assert!(!qdoc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            0,
            5,
            "strong"
        ));

        // Replacing the clipboard from another app invalidates the sidecar key,
        // so an ordinary paste also falls back to plain text.
        clipboard_copy("external");
        wp.clipboard_paste(&mut state.doc);
        assert_eq!(wp.inner.current_text(&state.doc), "helloexternal");
        assert!(!qdoc::model::has_mark_over(
            &state.doc,
            &containers,
            &block,
            5,
            13,
            "strong"
        ));
    }

    // ---- right-click context menu -----------------------------------------

    /// A content-space point at the very start of the first laid block's text
    /// (so a right-click lands on its first word).
    fn first_block_start_point(wp: &WordProcessorWidget) -> Point {
        let l = wp.laid.first().expect("at least one laid block");
        Point::new(l.text_x + 1.0, l.y_top + l.height / 2.0)
    }

    #[test]
    fn right_click_on_misspelled_word_offers_add_to_dictionary_and_clears_squiggle() {
        // Assertions below are literal English menu strings; pin the locale so
        // the test doesn't depend on the host's `LANG` (nextest runs each test
        // in its own process, so this can't leak into a sibling test).
        crate::i18n::set_locale("en-US");
        // Redirect the persisted user-dict to a throwaway dir so the test never
        // writes the real `~/.config/qos/userdict.txt`. (The in-memory add is
        // what the assertions below check; persistence is best-effort.)
        let tmp = std::env::temp_dir().join(format!("qos-userdict-test-{}", std::process::id()));
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
        }

        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // "qwordz" is not a real word → flagged. Followed by a valid word.
        wp.model_mut().insert_str(&mut state.doc, "qwordz quick");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let id = wp.blocks[0].id.clone();
        assert_eq!(
            wp.block_spell_spans(&id, "qwordz quick"),
            vec![(0usize, 6usize)],
            "precondition: ‘qwordz’ is flagged"
        );

        // Right-click on the first word.
        let pt = first_block_start_point(&wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        let labels = wp.context_menu_labels();
        assert!(
            labels.iter().any(|l| l == "Add to dictionary"),
            "misspelled word offers ‘Add to dictionary’, got {labels:?}"
        );

        // Invoke "Add to dictionary".
        let add_idx = wp
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Add to dictionary"))
            .unwrap();
        assert_eq!(
            wp.context_actions[add_idx],
            Some(CtxAction::AddToDictionary("qwordz".to_string()))
        );
        wp.dispatch_context_action(add_idx, &mut state.doc);
        assert!(!wp.context_menu.is_open(), "menu closed after dispatch");

        // Re-check: the squiggle is gone (the cache was invalidated and the word
        // is now in the user dictionary).
        assert!(
            wp.block_spell_spans(&id, "qwordz quick").is_empty(),
            "after Add to dictionary, ‘qwordz’ is no longer flagged"
        );
        assert!(
            wp.dictionary().contains("qwordz"),
            "the word is recorded in the (owned) dictionary"
        );
    }

    #[test]
    fn right_click_suggestion_replaces_the_word() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // "teh" suggests "the".
        wp.model_mut().insert_str(&mut state.doc, "teh end");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let pt = first_block_start_point(&wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        // Find a suggestion row whose action replaces [0,3) with "the".
        let the_idx = wp.context_actions.iter().position(|a| {
            matches!(a, Some(CtxAction::ApplySuggestion { replacement, .. }) if replacement == "the")
        });
        let Some(idx) = the_idx else {
            panic!(
                "expected a ‘the’ suggestion row, labels: {:?}",
                wp.context_menu_labels()
            );
        };
        wp.dispatch_context_action(idx, &mut state.doc);
        // The block text now reads "the end".
        let id = wp.blocks[0].id.clone();
        let text = wp.model().block_texts(&state.doc);
        assert_eq!(
            text[0], "the end",
            "suggestion replaced ‘teh’ with ‘the’ (block {id})"
        );
    }

    #[test]
    fn right_click_with_no_selection_selects_word_and_menu_has_copy() {
        // Assertions below are literal English menu strings; pin the locale so
        // the test doesn't depend on the host's `LANG` (nextest runs each test
        // in its own process, so this can't leak into a sibling test).
        crate::i18n::set_locale("en-US");
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hello world");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(
            !wp.model().has_selection(),
            "precondition: nothing selected"
        );

        let pt = first_block_start_point(&wp);
        wp.open_context_menu(
            &state.doc,
            pt,
            [num::f32_of_f64(pt.x), num::f32_of_f64(pt.y)],
        );
        // The clicked-word fallback selected "hello".
        assert!(
            wp.model().has_selection(),
            "a word was selected by the fallback"
        );
        assert_eq!(wp.model().selection_text(&state.doc), "hello");
        // With a selection, the menu has Cut + Copy.
        let labels = wp.context_menu_labels();
        assert!(labels.contains(&"Copy".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Cut".to_string()), "got {labels:?}");
        assert!(labels.contains(&"Select All".to_string()), "got {labels:?}");

        // item_at + dispatch maps the Copy row to the Copy action.
        let copy_idx = wp
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Copy"))
            .unwrap();
        assert_eq!(wp.context_actions[copy_idx], Some(CtxAction::Copy));
    }

    #[test]
    fn spell_cache_reuses_unchanged_blocks_and_rechecks_after_edit() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "wrold");
        let id = wp.blocks[0].id.clone();

        // First check flags ‘wrold’.
        let spans = wp.block_spell_spans(&id, "wrold");
        assert_eq!(spans, vec![(0, 5)], "‘wrold’ flagged");
        // A second check with identical text reuses the cache (same result).
        assert_eq!(wp.block_spell_spans(&id, "wrold"), vec![(0, 5)]);
        // Correcting the text re-checks (the key is the text) → no misspelling.
        assert!(
            wp.block_spell_spans(&id, "world").is_empty(),
            "the corrected text re-checks clean"
        );
    }

    // ---- footnote rendering -----------------------------------------------

    /// Author a doc with one body paragraph that references a single footnote
    /// (placeholder + `note-ref` mark), whose content is `note_body`. Returns the
    /// constructed `ui::State`.
    fn author_doc_with_footnote(body: &str, ref_idx: usize, note_body: &str) -> ui::State {
        let mut state = ui::State::new();
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, body);
            // Place a footnote reference marker at `ref_idx` and fill the note body.
            let note_id = dm.insert_footnote(&p, ref_idx);
            let nb = dm
                .insert_note_block(&note_id, None, "paragraph")
                .expect("note block");
            dm.splice_note_text(&note_id, &nb, 0, 0, note_body);
        }
        state.doc.commit();
        state
    }

    #[test]
    fn footnote_renders_superscript_marker_and_note_text_in_page_area() {
        // Body "Hello world" with a footnote reference after "Hello" (idx 5).
        let mut state = author_doc_with_footnote("Hello world", 5, "A clarifying footnote.");
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        // The note materialized into the widget as a footnote.
        assert_eq!(wp.notes.len(), 1, "one note materialized");
        assert_eq!(
            wp.notes[0].kind,
            qdoc::schema::NOTE_KIND_FOOTNOTE,
            "it is a footnote"
        );

        // The body block carries exactly one note-ref marker (the production scan
        // `draw` uses to place the superscript), and document-order numbering
        // assigns it citation number 1.
        let body = wp
            .blocks
            .iter()
            .find(|b| b.text.contains("Hello"))
            .expect("body block");
        let refs = block_note_refs(body);
        assert_eq!(refs.len(), 1, "one note-ref marker in the body");
        let (ref_ci, note_id) = &refs[0];
        assert_eq!(
            *ref_ci, 5,
            "marker sits at the reference index (the placeholder char)"
        );
        let nums = note_numbering(&wp.blocks);
        assert_eq!(
            nums.get(note_id.as_str()),
            Some(&1),
            "first-referenced note is numbered 1"
        );

        // The footnote text that the page note-area renders ("N. <text>") is laid
        // — assert the note text is present (not the rendered pixels).
        let text = note_text(&wp.notes[0]);
        assert_eq!(
            text, "A clarifying footnote.",
            "the footnote body text is materialized"
        );
        let rendered_line = format!("{}. {}", nums[note_id.as_str()], text);
        assert_eq!(
            rendered_line, "1. A clarifying footnote.",
            "the page note-area line is well-formed"
        );
    }

    #[test]
    fn note_numbering_follows_document_order_not_note_id() {
        // Two footnote references in one paragraph; the FIRST reference (by source
        // position) must be number 1 regardless of the notes' id sort order.
        let mut state = ui::State::new();
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        let (n_first, n_second);
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "alpha beta");
            // Insert the LATER reference first (at idx 10, end) then the EARLIER one
            // (at idx 5), so document order != insertion order.
            n_second = dm.insert_footnote(&p, 10);
            n_first = dm.insert_footnote(&p, 5);
            let b2 = dm.insert_note_block(&n_second, None, "paragraph").unwrap();
            dm.splice_note_text(&n_second, &b2, 0, 0, "second");
            let b1 = dm.insert_note_block(&n_first, None, "paragraph").unwrap();
            dm.splice_note_text(&n_first, &b1, 0, 0, "first");
        }
        state.doc.commit();

        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        let nums = note_numbering(&wp.blocks);
        assert_eq!(
            nums.get(n_first.as_str()),
            Some(&1),
            "the earlier marker is #1 by document order"
        );
        assert_eq!(
            nums.get(n_second.as_str()),
            Some(&2),
            "the later marker is #2"
        );
    }

    /// Build a single-paragraph document with `text`, returning the drawn widget
    /// plus its `ui::State` and the paragraph's block id. The caller mutates the
    /// doc (adding comments/changes) via a `DocMut`, then re-draws and inspects.
    fn doc_with_paragraph(
        text: &str,
    ) -> (
        WordProcessorWidget,
        ui::State,
        qdoc::Containers,
        qdoc::IdGen,
        String,
    ) {
        let mut state = ui::State::new();
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        let bid;
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            bid = dm.insert_block(None, "paragraph");
            dm.splice_text(&bid, 0, 0, text);
        }
        state.doc.commit();
        let wp = WordProcessorWidget::new();
        (wp, state, c, ids, bid)
    }

    #[test]
    fn comment_range_computes_a_highlight_rect() {
        // "hello world" with a comment over the word "world" (chars 6..11).
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.add_comment(&bid, 6, 11, "Ada", "Reword this");
        }
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        // The comment is materialized and anchored to the body range.
        assert_eq!(
            wp.review.comments().len(),
            1,
            "the comment is materialized into the widget"
        );
        assert_eq!(wp.review.comments()[0].author, "Ada");

        // The production helper recovers the commented byte range from the block's
        // `comment` mark (this is exactly what the draw pass calls).
        let b = wp
            .blocks
            .iter()
            .find(|b| b.id == bid)
            .expect("block present");
        let ranges = block_comment_ranges(b);
        assert_eq!(ranges.len(), 1, "one comment range over the block");
        let (bs, be, cid) = &ranges[0];
        assert_eq!((*bs, *be), (6, 11), "byte range covers 'world'");
        assert_eq!(
            cid,
            &wp.review.comments()[0].id,
            "range carries the comment id"
        );

        // The draw pass maps that byte range → on-screen highlight rects via the
        // shaped layout's `selection_rects`. Assert a non-empty rect with width.
        let (_, lay) = wp.layout_cache.get(&bid).expect("shaped layout cached");
        let rects = lay.selection_rects(*bs, *be);
        assert!(
            !rects.is_empty(),
            "the commented range computes a highlight rect"
        );
        assert!(
            rects[0].2 > rects[0].0,
            "the highlight rect has positive width"
        );
    }

    #[test]
    fn tracked_insert_underlines_colored_and_delete_strikes() {
        // "keep gone": flag "keep" (0..4) as a tracked insertion and "gone" (5..9)
        // as a tracked deletion, via the production tracked-change helpers.
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let (ins_id, del_id);
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            // "keep" is already in the block; mark it as a tracked insertion.
            ins_id = dm.insert_change(qdoc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(qdoc::ChangeKind::Ins, &bid, 0, 4, &ins_id);
            // "gone" stays in the doc but is flagged for deletion.
            del_id = dm.track_delete(&bid, 5, 9, "Ada", "");
        }
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        // Both changes materialize with their kinds.
        assert_eq!(
            wp.review.changes().len(),
            2,
            "both tracked changes materialized"
        );
        let ins = wp
            .review
            .changes()
            .iter()
            .find(|ch| ch.id == ins_id)
            .expect("ins change");
        let del = wp
            .review
            .changes()
            .iter()
            .find(|ch| ch.id == del_id)
            .expect("del change");
        assert_eq!(ins.kind, qdoc::ChangeKind::Ins);
        assert_eq!(del.kind, qdoc::ChangeKind::Del);

        let b = wp
            .blocks
            .iter()
            .find(|b| b.id == bid)
            .expect("block present");

        // Decoration ranges (the production draw path): ins → underline, del → strike.
        let (ins_ranges, del_ranges) = block_change_decos(b);
        assert_eq!(
            ins_ranges,
            vec![(0, 4)],
            "the insertion range underlines 'keep'"
        );
        assert_eq!(
            del_ranges,
            vec![(5, 9)],
            "the deletion range strikes 'gone'"
        );

        // Both decoration ranges resolve to real on-screen rects via the layout.
        let (_, lay) = wp.layout_cache.get(&bid).expect("shaped layout cached");
        assert!(
            !lay.selection_rects(0, 4).is_empty(),
            "insertion underline has geometry"
        );
        assert!(
            !lay.selection_rects(5, 9).is_empty(),
            "deletion strike has geometry"
        );

        // Glyph tinting: `block_runs_markup` colours the ins run green and the del
        // run red (the standard review styling). With markup OFF those runs are
        // plain (no review colour).
        let (runs_on, _, _) = block_runs_markup(b, true);
        assert!(
            runs_on
                .iter()
                .any(|r| r.text.contains("keep") && r.color == Some(INSERTION_RGB)),
            "the inserted run is tinted with the insertion review colour"
        );
        assert!(
            runs_on
                .iter()
                .any(|r| r.text.contains("gone") && r.color == Some(DELETION_RGB)),
            "the deleted run is tinted with the deletion review colour"
        );
        let (runs_off, _, _) = block_runs_markup(b, false);
        assert!(
            runs_off
                .iter()
                .all(|r| r.color != Some(INSERTION_RGB) && r.color != Some(DELETION_RGB)),
            "with markup hidden, tracked-change runs render as plain text"
        );
    }

    /// Re-materialize the document straight from the CRDT (the production
    /// snapshot path) so a test asserts on what every reader sees — not on the
    /// widget's cached copy.
    fn snapshot(state: &ui::State, c: &qdoc::Containers) -> qdoc::snapshot::DocSnapshot {
        qdoc::model::materialize(&state.doc, c)
    }

    #[test]
    fn accepting_tracked_insertion_clears_its_ins_mark() {
        // "keep gone": flag "keep" (0..4) as a tracked insertion.
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let ins_id = {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let id = dm.insert_change(qdoc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(qdoc::ChangeKind::Ins, &bid, 0, 4, &id);
            id
        };
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        // Before: the change is reported in the materialized snapshot, and the
        // block carries a decoration range over "keep".
        let before = snapshot(&state, &c);
        assert!(
            before.changes.iter().any(|ch| ch.id == ins_id),
            "the insertion is a tracked change before accept"
        );
        let b = before.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            block_change_decos(b).0,
            vec![(0, 4)],
            "the insertion underlines 'keep' before accept"
        );

        // Accept via the production review-dispatch path (the same call the
        // margin ✓ button + context menu use).
        wp.dispatch_review_action(ReviewAction::AcceptChange(ins_id.clone()), &mut state.doc);

        // After: the text is kept, but the `ins` mark + change record are gone —
        // the run is no longer reported as a tracked change anywhere.
        let after = snapshot(&state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep gone",
            "accepting an insertion keeps the inserted text"
        );
        assert!(
            !after.changes.iter().any(|ch| ch.id == ins_id),
            "the change record is dropped after accept"
        );
        assert!(
            block_change_decos(b).0.is_empty(),
            "no `ins` decoration remains after accept"
        );
        assert!(
            !b.marks.iter().any(|m| m.name == qdoc::schema::MARK_INS),
            "the `ins` mark is cleared from the block after accept"
        );
    }

    #[test]
    fn accepting_tracked_deletion_removes_the_struck_text() {
        // "keep gone": flag "gone" (5..9) as a tracked deletion (retained-but-struck).
        // qdoc semantics: ACCEPTING a deletion applies it (removes the struck text);
        // REJECTING keeps the text and drops the `del` mark. We assert both arms
        // on the materialized snapshot (the production read path).
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let del_id = {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.track_delete(&bid, 5, 9, "Ada", "")
        };
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        // Before: the struck text is still present + reported as a change.
        let before = snapshot(&state, &c);
        assert!(
            before.changes.iter().any(|ch| ch.id == del_id),
            "the deletion is a tracked change before accept"
        );
        assert_eq!(
            before.blocks.iter().find(|b| b.id == bid).unwrap().text,
            "keep gone"
        );

        // Accept the deletion: the struck text is removed.
        wp.dispatch_review_action(ReviewAction::AcceptChange(del_id.clone()), &mut state.doc);

        let after = snapshot(&state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep ",
            "accepting a deletion removes the struck text"
        );
        assert!(
            !after.changes.iter().any(|ch| ch.id == del_id),
            "the change record is dropped after accept"
        );
        assert!(
            block_change_decos(b).1.is_empty(),
            "no `del` strike decoration remains after accept"
        );
    }

    #[test]
    fn rejecting_tracked_deletion_keeps_the_text_and_drops_the_mark() {
        // The inverse arm: rejecting a tracked deletion keeps the text (the author
        // chose NOT to delete) and clears the `del` strike.
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let del_id = {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.track_delete(&bid, 5, 9, "Ada", "")
        };
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        wp.dispatch_review_action(ReviewAction::RejectChange(del_id.clone()), &mut state.doc);

        let after = snapshot(&state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(b.text, "keep gone", "rejecting a deletion keeps the text");
        assert!(
            !after.changes.iter().any(|ch| ch.id == del_id),
            "the change record is dropped after reject"
        );
        assert!(
            block_change_decos(b).1.is_empty(),
            "the `del` strike decoration is cleared after reject"
        );
    }

    #[test]
    fn deleting_comment_removes_it_and_clears_the_highlight() {
        // "hello world" with a comment over "world" (chars 6..11).
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        let cid = {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.add_comment(&bid, 6, 11, "Ada", "Reword this")
        };
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);

        // Before: the comment is in the snapshot and its highlight range is present.
        let before = snapshot(&state, &c);
        assert!(
            before.comments.iter().any(|cm| cm.id == cid),
            "the comment exists before resolve"
        );
        let b = before.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            block_comment_ranges(b).len(),
            1,
            "the highlight range exists before resolve"
        );

        // Resolve (delete) via the production review-dispatch path.
        wp.dispatch_review_action(ReviewAction::ResolveComment(cid.clone()), &mut state.doc);

        // After: gone from `comments`, and the anchoring `comment` mark (the
        // highlight band) is cleared from the block. Body text is untouched.
        let after = snapshot(&state, &c);
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert!(
            !after.comments.iter().any(|cm| cm.id == cid),
            "the comment is removed from the snapshot after resolve"
        );
        assert!(
            block_comment_ranges(b).is_empty(),
            "the comment highlight is cleared after resolve"
        );
        assert_eq!(
            b.text, "hello world",
            "resolving a comment leaves the commented text intact"
        );
    }

    #[test]
    fn accept_all_clears_every_tracked_change() {
        // Two changes: an insertion over "keep" and a deletion over "gone".
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            let ins = dm.insert_change(qdoc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(qdoc::ChangeKind::Ins, &bid, 0, 4, &ins);
            dm.track_delete(&bid, 5, 9, "Ada", "");
        }
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1000, 900);
        assert_eq!(
            snapshot(&state, &c).changes.len(),
            2,
            "two tracked changes before Accept All"
        );

        wp.dispatch_review_action(ReviewAction::AcceptAllChanges, &mut state.doc);

        // Every change is gone; accepting the insertion kept "keep" and accepting
        // the deletion removed "gone".
        let after = snapshot(&state, &c);
        assert!(
            after.changes.is_empty(),
            "no tracked changes remain after Accept All"
        );
        let b = after.blocks.iter().find(|b| b.id == bid).unwrap();
        assert_eq!(
            b.text, "keep ",
            "Accept All keeps insertions and applies deletions"
        );
        assert!(
            b.marks
                .iter()
                .all(|m| m.name != qdoc::schema::MARK_INS && m.name != qdoc::schema::MARK_DEL)
        );
    }

    #[test]
    fn margin_card_buttons_register_review_hits() {
        // A drawn frame with one comment + one tracked change populates clickable
        // affordances in `review_hits` (the production hit list the left-click
        // handler dispatches through).
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("keep gone");
        let (cid, ins_id);
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            cid = dm.add_comment(&bid, 0, 4, "Ada", "look");
            ins_id = dm.insert_change(qdoc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(qdoc::ChangeKind::Ins, &bid, 5, 9, &ins_id);
        }
        state.doc.commit();
        pipeline_draw(&mut wp, &mut state, 1200, 900);

        let has = |pred: &dyn Fn(&ReviewAction) -> bool| {
            wp.review
                .hits()
                .iter()
                .any(|(r, a)| pred(a) && r.width() > 0.0 && r.height() > 0.0)
        };
        assert!(
            has(&|a| matches!(a, ReviewAction::ResolveComment(id) if id == &cid)),
            "a Resolve affordance for the comment"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::AcceptChange(id) if id == &ins_id)),
            "an Accept ✓ affordance for the change"
        );
        assert!(
            has(&|a| matches!(a, ReviewAction::RejectChange(id) if id == &ins_id)),
            "a Reject ✗ affordance for the change"
        );

        let tree = wp.accessibility_tree();
        let review = tree
            .children
            .iter()
            .find(|child| {
                child.node.role == ui::a11y::Role::Group
                    && child.node.name.as_deref() == Some(t("a11y-group-review").as_str())
            })
            .expect("painted review controls are exported as one semantic group");
        assert_eq!(review.children.len(), 3);
        assert!(review.children.iter().all(|child| {
            child.node.role == ui::a11y::Role::Button
                && child.node.bounds.is_some()
                && child.node.action_target.is_some()
                && child.node.state.focusable
        }));

        let resolve_target = review_action_target(&ReviewAction::ResolveComment(cid.clone()));
        let root = state.root.clone();
        assert!(Widget::perform_action_at(
            &mut wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&resolve_target)],
            &mut state.doc,
            &root,
        ));
        assert!(
            snapshot(&state, &c)
                .comments
                .iter()
                .all(|comment| comment.id != cid),
            "screen-reader activation resolves through the typed document action"
        );
    }

    #[test]
    fn markup_toggle_hides_comment_ranges_and_changes() {
        let (mut wp, mut state, c, mut ids, bid) = doc_with_paragraph("hello world");
        {
            let mut dm = qdoc::DocMut {
                doc: &mut state.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.add_comment(&bid, 0, 5, "Ada", "note");
            let ci = dm.insert_change(qdoc::ChangeKind::Ins, "Ada", "");
            dm.add_change_mark(qdoc::ChangeKind::Ins, &bid, 6, 11, &ci);
        }
        state.doc.commit();

        // Default ON: a comment marker is collected for the right-margin card.
        assert!(wp.markup_enabled(), "markup defaults ON");
        pipeline_draw(&mut wp, &mut state, 1000, 900);
        let b = wp.blocks.iter().find(|b| b.id == bid).unwrap().clone();
        assert_eq!(
            block_comment_ranges(&b).len(),
            1,
            "comment range present in the data"
        );

        // Toggle OFF: the markup-gated runs lose their review colour, proving the
        // draw path suppresses tracked-change styling.
        wp.toggle_markup();
        assert!(!wp.markup_enabled(), "toggle flips the flag");
        let (runs_off, _, _) = block_runs_markup(&b, wp.review.is_visible());
        assert!(
            runs_off.iter().all(|r| r.color != Some(INSERTION_RGB)),
            "no review colour is applied while markup is hidden"
        );
    }

    // ---- outline / navigation pane + table of contents ----------------------

    /// Build a document with several headings (some on later pages) interleaved
    /// with body paragraphs, returning `(wp, state)` already drawn at `w×h`.
    fn doc_with_headings(w: u32, h: u32) -> (WordProcessorWidget, ui::State) {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        let add_heading =
            |wp: &mut WordProcessorWidget, st: &mut ui::State, text: &str, level: i64| {
                wp.model_mut().insert_str(&mut st.doc, text);
                wp.model_mut()
                    .set_paragraph_style(&mut st.doc, "heading", Some(level), None);
                wp.model_mut().enter(&mut st.doc);
                wp.model_mut()
                    .set_paragraph_style(&mut st.doc, "paragraph", None, None);
            };
        add_heading(&mut wp, &mut state, "Introduction", 1);
        for i in 0..30 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Intro body line {i} with enough words to fill the line."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        add_heading(&mut wp, &mut state, "Background", 2);
        for i in 0..30 {
            wp.model_mut().insert_str(
                &mut state.doc,
                &format!("Background body line {i} with enough words to fill it."),
            );
            wp.model_mut().enter(&mut state.doc);
        }
        add_heading(&mut wp, &mut state, "Conclusion", 1);
        wp.model_mut().insert_str(&mut state.doc, "The end.");
        pipeline_draw(&mut wp, &mut state, w, h);
        (wp, state)
    }

    #[test]
    fn outline_has_one_entry_per_heading_with_levels_in_order() {
        let (wp, _state) = doc_with_headings(1000, 700);
        let entries = wp.outline_entries();
        assert_eq!(
            entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["Introduction", "Background", "Conclusion"],
            "one entry per heading in document order"
        );
        assert_eq!(
            entries.iter().map(|e| e.level).collect::<Vec<_>>(),
            vec![1, 2, 1],
            "levels preserved"
        );
    }

    /// Drive a real left-click at `pt` through the production `handle_event`.
    pub(super) fn left_click(wp: &mut WordProcessorWidget, state: &mut ui::State, pt: Point) {
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        // Move the pointer, then press.
        wp.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: (pt.x, pt.y).into(),
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        wp.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    #[test]
    fn clicking_an_outline_entry_scrolls_the_heading_into_view() {
        let (mut wp, mut state) = doc_with_headings(1000, 700);
        // Open the outline pane and redraw so its clickable entries are built.
        wp.apply_tool(&mut state.doc, ToolAction::ToggleOutline);
        assert!(wp.outline.is_open(), "outline toggled on");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // The last heading ("Conclusion") is far down; it should be off-screen at
        // the top of the document.
        let entries = wp.outline_entries();
        let (entry_index, concl) = entries
            .iter()
            .enumerate()
            .find(|(_, heading)| heading.text == "Conclusion")
            .expect("conclusion heading");
        assert_eq!(wp.scroll_y, 0.0, "starts scrolled to the top");

        // The pane lays entries immediately below its header. Click the centre
        // of the selected row through the production event path rather than
        // reaching into the component's private hit map.
        let row_y =
            wp.toolbar_height(1000.0) + 14.0 + 18.0 + num::f64_of_usize(entry_index) * 24.0 + 12.0;
        let pt = Point::new(30.0, row_y);
        left_click(&mut wp, &mut state, pt);
        assert!(
            wp.scroll_y > 0.0,
            "clicking a far-down heading scrolls the document"
        );

        // After redrawing at the new scroll, the heading is laid within the canvas.
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let canvas_top = wp.toolbar_height(1000.0);
        // The bottom status strip is gone (its readout floats as a bubble); the
        // canvas now runs down to the top of the layout bar.
        let canvas_bottom = wp.rect.y1 - LAYOUT_BAR_H;
        let laid = wp
            .laid
            .iter()
            .find(|l| l.id == concl.id)
            .expect("conclusion is laid out after scrolling to it");
        assert!(
            laid.y_top >= canvas_top && laid.y_top <= canvas_bottom,
            "the clicked heading is visible in the canvas (y_top={}, canvas=[{canvas_top},{canvas_bottom}])",
            laid.y_top
        );
    }

    #[test]
    fn generate_toc_inserts_one_marked_block_per_heading() {
        let (mut wp, mut state) = doc_with_headings(1000, 700);
        let heading_count = wp.outline_entries().len();
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);

        let c = wp.model().containers.clone().unwrap();
        let snap = qdoc::model::materialize(&state.doc, &c);
        let toc: Vec<_> = snap
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .collect();
        assert_eq!(
            toc.len(),
            heading_count,
            "one toc-marked block per heading (one line each)"
        );
        // The TOC sits at the document top, in heading order, each line carrying
        // the heading text.
        assert!(
            snap.blocks[0].text.contains("Introduction"),
            "TOC begins at the top with the first heading"
        );
        assert!(
            toc.iter().any(|b| b.text.contains("Conclusion")),
            "every heading is listed"
        );
        assert!(wp.needs_commit, "TOC generation is an undo point");
    }

    #[test]
    fn regenerating_toc_replaces_rather_than_duplicating() {
        let (mut wp, mut state) = doc_with_headings(1000, 700);
        let heading_count = wp.outline_entries().len();
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        // Regenerate.
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);

        let c = wp.model().containers.clone().unwrap();
        let snap = qdoc::model::materialize(&state.doc, &c);
        let toc_count = snap
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .count();
        assert_eq!(
            toc_count, heading_count,
            "regenerating replaces the prior TOC (no duplicate)"
        );
        // The headings themselves are untouched (still derive the same outline).
        assert_eq!(
            wp.outline_entries().len(),
            heading_count,
            "headings unchanged by TOC regeneration"
        );
    }

    // ---- new-from-template picker -----------------------------------------

    /// The materialized first block of the document in `state`.
    fn first_block(wp: &WordProcessorWidget, state: &ui::State) -> Option<BlockView> {
        let c = wp.model().containers.clone()?;
        qdoc::model::materialize(&state.doc, &c)
            .blocks
            .into_iter()
            .next()
    }

    /// A blank, bound widget over a fresh `ui::State`.
    fn bound() -> (ui::State, WordProcessorWidget) {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        (state, wp)
    }

    /// The "New ▾" toolbar button is present and registered as a click target.
    #[test]
    fn new_menu_button_is_in_the_toolbar() {
        let (mut state, mut wp) = bound();
        pipeline_draw(&mut wp, &mut state, 1200, 700);
        assert!(
            wp.toolbar.iter().any(|(a, _)| *a == ToolAction::NewMenu),
            "the New ▾ button is a registered toolbar action"
        );
    }

    /// Selecting from the "New ▾" picker replaces the document content with that
    /// template's first heading through the production pointer-routing path.
    #[test]
    fn selecting_a_template_replaces_the_document_content() {
        let (mut state, mut wp) = bound();
        // Author some pre-existing content so we can prove it's actually replaced.
        wp.model_mut()
            .insert_str(&mut state.doc, "Pre-existing rough draft");
        wp.refresh_blocks(&state.doc);
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("Pre-existing rough draft".to_string()),
            "the document starts with the user's own text"
        );

        // Draw once to establish the real toolbar anchor, then open the picker
        // via the toolbar action.
        pipeline_draw(&mut wp, &mut state, 1200, 700);
        wp.apply_tool(&mut state.doc, ToolAction::NewMenu);
        assert!(wp.template_picker.is_open(), "the New ▾ picker opens");

        // Select "Report" (its first heading is a known, stable string). Shared
        // ContextMenu rows start after 6 px padding and are 28 px tall.
        let report_idx = crate::Template::all()
            .iter()
            .position(|t| *t == crate::Template::Report)
            .unwrap();
        let anchor_x = wp.toolbar.first().map(|(_, rect)| rect.x0).unwrap();
        let anchor_y = wp.rect.y0 + wp.toolbar_height(wp.rect.width());
        wp.pointer = Point::new(
            anchor_x + 10.0,
            anchor_y + 6.0 + num::f64_of_usize(report_idx) * 28.0 + 1.0,
        );
        let mut handled = false;
        assert!(wp.handle_left_press_menu_routing(&mut state.doc, &mut handled));
        assert!(handled);
        assert!(
            !wp.template_picker.is_open(),
            "the picker closes after a selection"
        );

        let first = first_block(&wp, &state).expect("the templated document has a first block");
        assert_eq!(
            first.kind, "heading",
            "the report template opens with a heading"
        );
        assert_eq!(
            first.text, "Report Title",
            "content is replaced by the template's first heading"
        );
        // The old draft text is gone — the body is the template, not an append.
        let c = wp.model().containers.clone().unwrap();
        let all_text: String = qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect();
        assert!(
            !all_text.contains("rough draft"),
            "the prior content was replaced, not appended"
        );
        assert!(wp.modified, "a template swap marks the document modified");
    }

    // ---- autosave / crash-recovery ----------------------------------------

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "editor-wp-recovery-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Drive a real keypress (single typed character) through the production
    /// `handle_event` keyboard path. winit's `KeyEvent` has a private
    /// `platform_specific` field, so (per the shell's `e2e_shell_flows` pattern)
    /// zero-init the whole struct and overwrite the public fields we read.
    fn type_char(wp: &mut WordProcessorWidget, state: &mut ui::State, ch: char) {
        use winit::keyboard::{NativeKeyCode, PhysicalKey, SmolStr};
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        // SAFETY: identical to the shell's KeyEvent synthesis — all public fields
        // tolerate the zero bit pattern (Option → None, enums → first variant),
        // and we overwrite the ones `handle_event` reads (`logical_key`/`text`/
        // `state`). The private `platform_specific` field is plain/zero-sized.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.logical_key = Key::Character(SmolStr::new(ch.to_string()));
        ev.text = Some(SmolStr::new(ch.to_string()));
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        wp.handle_event(
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// Type a mixed-case string through the production `handle_event`
    /// keyboard path, then read the document back through the production
    /// `materialize` path.
    ///
    /// This guards two *distinct* failures that a rendered frame showed
    /// and no test caught. The first is the document coming back empty
    /// after a scripted typing run — nothing the driver sent ever reached
    /// the CRDT, which made every downstream "the peer sees what was
    /// typed" claim vacuously true over an empty string. The second is
    /// the document coming back lowercased: a key source that resolves a
    /// character to a bare keycode loses the shift, so `Hello World`
    /// lands as `hello world`. Asserting an exact mixed-case string
    /// catches both; asserting merely "non-empty" catches only the first.
    #[test]
    fn typed_text_lands_in_the_document_with_its_capitals() {
        const TYPED: &str = "Hello World";

        let (mut state, mut wp) = bound();
        assert_eq!(
            body_text(&wp, &state).concat(),
            "",
            "a fresh document starts empty, so the assertion below is about \
             what typing added and nothing else",
        );

        for ch in TYPED.chars() {
            type_char(&mut wp, &mut state, ch);
        }

        assert_eq!(
            body_text(&wp, &state).concat(),
            TYPED,
            "the document holds exactly the characters that were typed, \
             capitals included",
        );
    }

    /// A document editor shows the document. The contextual styles panel
    /// is an aid to that, not a co-equal half of the window: at every
    /// width the shell can hand this app, the canvas left for the
    /// document must be wider than the panel.
    ///
    /// Regression guard for a frame in which the panel took roughly half of a
    /// tiled word-processor window, so
    /// the thing the app exists to show was the smaller half of it.
    ///
    /// Measured on the *canvas*, not on `page_rect`: the page sheet is a
    /// fixed nominal width that stays ~816px however narrow the window
    /// gets (it is clipped and scrolled, not shrunk), so comparing it
    /// against the panel would pass at every width and prove nothing.
    /// `panel_rect` and `rect` are both written by the production draw
    /// pass, and `panel_rect.x0` *is* the canvas's right edge.
    #[test]
    fn the_document_canvas_is_wider_than_the_styles_panel_at_every_tile_width() {
        // Widths a one-, two- and three-column strip hands an app on
        // ordinary displays — a 1280-wide screen split three ways is the
        // narrow end, and is where the panel currently wins.
        for (w, h) in [(1400_u32, 900_u32), (960, 800), (640, 700), (426, 700)] {
            let (mut state, mut wp) = bound();
            wp.styles_panel_open = true;
            pipeline_draw(&mut wp, &mut state, w, h);
            assert!(
                wp.styles_panel_open,
                "precondition: the explicitly-open panel is the one under test",
            );
            assert!(
                !wp.outline.is_open(),
                "precondition: the left-hand outline pane is closed, so the \
                 canvas is bounded only by the panel under test",
            );
            let panel = wp.panel_rect.width();
            let canvas = if wp.styles_panel_overlay() {
                wp.rect.width()
            } else {
                wp.rect.width() - panel
            };
            assert!(
                canvas > panel,
                "at {w}x{h} the canvas left for the document ({canvas:.0}px) \
                 must be wider than the styles panel ({panel:.0}px) — an \
                 editor whose subject is the smaller half of its own window \
                 is showing the wrong thing",
            );
        }
    }

    /// Autosave: typing past the edit threshold (through the production keyboard
    /// path) writes a recovery copy to the widget's recovery dir. A real save then
    /// clears it. Asserts on file existence (the production path), not pixels.
    #[test]
    fn autosave_writes_recovery_then_save_clears_it() {
        let dir = temp_dir("autosave");
        let (mut state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        // Point the document at a real (temp) save target so `save` succeeds and
        // `recovery_path` is keyed off a stable document path.
        let doc_path = dir.join("doc.md");
        wp.set_file(doc_path.clone(), WpFormat::Markdown);

        let recovery_path = wp.recovery_path();
        assert!(!recovery_path.exists(), "no recovery file before any edits");

        // Type more than AUTOSAVE_EDIT_THRESHOLD characters through `handle_event`.
        for _ in 0..(AUTOSAVE_EDIT_THRESHOLD as usize + 2) {
            type_char(&mut wp, &mut state, 'x');
        }
        assert!(
            recovery_path.exists(),
            "a recovery copy is written once the edit threshold is crossed ({})",
            recovery_path.display()
        );
        // The recovery copy is a faithful Markdown snapshot (re-imports our text).
        let recovered = std::fs::read(&recovery_path).expect("read recovery copy");
        assert!(
            String::from_utf8_lossy(&recovered).contains('x'),
            "the recovery copy holds the typed content"
        );

        // A real save supersedes and removes the recovery copy.
        wp.save(&state.doc);
        assert!(doc_path.exists(), "the document saved to its real path");
        assert!(
            !recovery_path.exists(),
            "a successful save clears the recovery copy"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `check_for_recovery` surfaces the banner (Restore/Discard) when a recovery
    /// copy exists for the open document; Restore re-imports its content in place.
    #[test]
    fn check_for_recovery_surfaces_banner_and_restore_loads_it() {
        let dir = temp_dir("restore");
        let (mut state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        let doc_path = dir.join("notes.md");
        wp.set_file(doc_path, WpFormat::Markdown);

        // Stage a recovery copy on disk (as a previous, crashed session would).
        let rp = wp.recovery_path();
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, b"# Recovered Heading\n\nUnsaved work\n").unwrap();

        assert!(
            wp.check_for_recovery(),
            "a present recovery copy stages an offer"
        );
        assert!(wp.recovery.is_active(), "the recovery banner is now active");

        // Draw so the banner's Restore/Discard buttons are registered, then click
        // Restore through the production handler.
        pipeline_draw(&mut wp, &mut state, 1200, 700);
        let rrect = wp
            .recovery
            .restore_button_rect()
            .expect("Restore button is drawn");
        left_click(
            &mut wp,
            &mut state,
            Point::new(
                f64::midpoint(rrect.x0, rrect.x1),
                f64::midpoint(rrect.y0, rrect.y1),
            ),
        );

        assert!(
            !wp.recovery.is_active(),
            "the banner is dismissed after Restore"
        );
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("Recovered Heading".to_string()),
            "Restore re-imported the recovery copy into the live document"
        );
        assert!(!rp.exists(), "the recovery file is consumed by Restore");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Discard dismisses the banner and deletes the recovery file without touching
    /// the document.
    #[test]
    fn discard_recovery_removes_the_file_and_banner() {
        let dir = temp_dir("discard");
        let (mut state, mut wp) = bound();
        wp.set_recovery_dir(dir.clone());
        wp.set_file(dir.join("d.md"), WpFormat::Markdown);
        wp.model_mut().insert_str(&mut state.doc, "Keep me");
        wp.refresh_blocks(&state.doc);

        let rp = wp.recovery_path();
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, b"# Stale\n").unwrap();
        assert!(wp.check_for_recovery());

        pipeline_draw(&mut wp, &mut state, 1200, 700);
        let drect = wp
            .recovery
            .discard_button_rect()
            .expect("Discard button is drawn");
        left_click(
            &mut wp,
            &mut state,
            Point::new(
                f64::midpoint(drect.x0, drect.x1),
                f64::midpoint(drect.y0, drect.y1),
            ),
        );

        assert!(!wp.recovery.is_active(), "Discard dismisses the banner");
        assert!(!rp.exists(), "Discard deletes the recovery file");
        assert_eq!(
            first_block(&wp, &state).map(|b| b.text),
            Some("Keep me".to_string()),
            "Discard leaves the open document untouched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The recovery path is stable for a given document and distinct per document.
    #[test]
    fn widget_recovery_path_is_stable_and_per_document() {
        let dir = temp_dir("stable");
        let mut a = WordProcessorWidget::new();
        a.set_recovery_dir(dir.clone());
        a.set_file(std::path::PathBuf::from("/docs/a.md"), WpFormat::Markdown);
        let pa1 = a.recovery_path();
        let pa2 = a.recovery_path();
        assert_eq!(pa1, pa2, "stable across calls for the same document");
        assert!(
            pa1.starts_with(&dir),
            "honours the widget's recovery dir override"
        );

        let mut b = WordProcessorWidget::new();
        b.set_recovery_dir(dir);
        b.set_file(std::path::PathBuf::from("/docs/b.md"), WpFormat::Markdown);
        assert_ne!(
            pa1,
            b.recovery_path(),
            "different documents get different recovery files"
        );
    }

    // ---- horizontal ruler ----------------------------------------------------

    /// Drive a real Tab keypress through the production `handle_event` keyboard
    /// path (no `text`, just the named key).
    fn press_tab(wp: &mut WordProcessorWidget, state: &mut ui::State) {
        use winit::keyboard::{NativeKeyCode, PhysicalKey};
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.logical_key = Key::Named(NamedKey::Tab);
        ev.text = None;
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        wp.handle_event(
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// The ruler is on by default and toggles off/on via the toolbar action.
    #[test]
    fn ruler_defaults_on_and_toggles() {
        let (mut state, mut wp) = bound();
        assert!(wp.ruler_on, "ruler defaults ON (discoverable)");
        wp.apply_tool(&mut state.doc, ToolAction::ToggleRuler);
        assert!(!wp.ruler_on, "toolbar toggle hides the ruler");
        wp.apply_tool(&mut state.doc, ToolAction::ToggleRuler);
        assert!(wp.ruler_on, "toggles back on");
    }

    /// The ruler computes correct marker x-positions from the margins + the caret
    /// paragraph's indent, and those positions scale with zoom about the anchor.
    #[test]
    fn ruler_marker_positions_track_margins_indent_and_zoom() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "para");
        // Set a known left/hanging indent + first-line indent on the caret block.
        let block = wp.model().caret().block.clone();
        wp.model_mut()
            .set_block_indent(&mut state.doc, &block, 40.0);
        wp.model_mut()
            .set_block_first_line_indent(&mut state.doc, &block, 18.0);
        pipeline_draw(&mut wp, &mut state, 1200, 760);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        assert!(
            (page_w - MAX_PAGE_W).abs() < 1e-6,
            "page is the fixed US-Letter width"
        );

        // At zoom 1.0 content-x == screen-x (anchor scale is identity).
        assert!((wp.zoom.factor() - 1.0).abs() < 1e-6, "default zoom");
        let markers = wp.ruler_markers(page_x, page_w);
        let mx = |m: RulerMarker| {
            markers
                .iter()
                .find(|(k, _)| *k == m)
                .map(|(_, x)| *x)
                .unwrap()
        };

        let m = wp.margins;
        assert!(
            (mx(RulerMarker::MarginLeft) - (page_x + m.left)).abs() < 1e-6,
            "left margin marker at the left text edge"
        );
        assert!(
            (mx(RulerMarker::MarginRight) - (page_x + page_w - m.right)).abs() < 1e-6,
            "right margin marker at the right text edge"
        );
        assert!(
            (mx(RulerMarker::IndentLeft) - (page_x + m.left + 40.0)).abs() < 1e-6,
            "left-indent marker offset by the block indent"
        );
        assert!(
            (mx(RulerMarker::IndentFirst) - (page_x + m.left + 40.0 + 18.0)).abs() < 1e-6,
            "first-line marker offset by indent + first-line indent"
        );

        // Zooming in moves the markers further from the anchor by the zoom factor.
        wp.set_zoom(2.0);
        pipeline_draw(&mut wp, &mut state, 1200, 760);
        let anchor = wp.zoom.anchor().x;
        let zoomed = wp.ruler_markers(wp.page_rect.x0, wp.page_rect.width());
        let zx = |m: RulerMarker| {
            zoomed
                .iter()
                .find(|(k, _)| *k == m)
                .map(|(_, x)| *x)
                .unwrap()
        };
        // The left-margin marker's screen position = anchor + (content - anchor)*zoom.
        let content_left = wp.page_rect.x0 + wp.margins.left;
        let expect = anchor + (content_left - anchor) * 2.0;
        assert!(
            (zx(RulerMarker::MarginLeft) - expect).abs() < 1e-6,
            "marker scales with zoom about the anchor"
        );
    }

    /// Dragging the left-margin marker on the ruler updates `self.margins.left`
    /// (with a live preview) and persists it through the production path.
    #[test]
    fn dragging_left_margin_marker_updates_margins_left() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "para");
        pipeline_draw(&mut wp, &mut state, 1200, 760);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let before = wp.margins.left;

        // Grab the left-margin marker (press on its screen x within the band).
        let markers = wp.ruler_markers(page_x, page_w);
        let marker_x = markers
            .iter()
            .find(|(k, _)| *k == RulerMarker::MarginLeft)
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(
            marker_x,
            f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1),
        );
        wp.ruler_press(&mut state.doc);
        assert_eq!(
            wp.ruler.drag_marker(),
            Some(RulerMarker::MarginLeft),
            "grabbed the left-margin marker"
        );

        // Drag it right (toward the page center): a wider left margin.
        let target_content_x = page_x + 120.0; // 120 px from the page's left edge
        wp.pointer = Point::new(wp.content_x_to_screen(target_content_x), wp.pointer.y);
        wp.ruler_drag_move();
        assert!(
            (wp.margins.left - 120.0).abs() < 1.0,
            "live preview updated margins.left to ~120 (got {})",
            wp.margins.left
        );
        assert_ne!(wp.margins.left, before, "the left margin actually changed");

        // Release persists into the document (a fresh widget reloads the value).
        wp.ruler_drag_release(&mut state.doc);
        assert!(!wp.ruler.is_dragging(), "drag ended on release");
        let mut wp2 = WordProcessorWidget::new();
        wp2.sync(&mut state.doc, &state.root);
        assert!(
            (wp2.margins.left - wp.margins.left).abs() < 1e-6,
            "the dragged left margin persisted to the document"
        );
    }

    /// Dragging the left-indent marker sets the caret block's `indent` attr, which
    /// reflows the paragraph's text-left (`text_x`) by that amount.
    #[test]
    fn dragging_indent_marker_sets_block_indent_and_reflows() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "indent me");
        pipeline_draw(&mut wp, &mut state, 1200, 760);
        let block = wp.model().caret().block.clone();
        let x_before = wp.laid.iter().find(|l| l.id == block).unwrap().text_x;

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let markers = wp.ruler_markers(page_x, page_w);
        let mxl = markers
            .iter()
            .find(|(k, _)| *k == RulerMarker::IndentLeft)
            .map(|(_, x)| *x)
            .unwrap();
        // The hanging/left-indent wedge lives in the lower third of the band.
        wp.pointer = Point::new(mxl, wp.ruler.rect().y1 - 2.0);
        wp.ruler_press(&mut state.doc);
        assert_eq!(wp.ruler.drag_marker(), Some(RulerMarker::IndentLeft));

        // Drag the indent marker 60 px right of the left text margin.
        let target = page_x + wp.margins.left + 60.0;
        wp.pointer = Point::new(wp.content_x_to_screen(target), wp.pointer.y);
        wp.ruler_drag_move();
        wp.ruler_drag_release(&mut state.doc);

        // The block's `indent` attr was persisted (~60 px).
        let stored = wp.model().block_indent(&state.doc, &block);
        assert!(
            (stored - 60.0).abs() < 1.0,
            "indent persisted to the block attr (got {stored})"
        );

        // A redraw reflows the paragraph's text-left by the new indent.
        pipeline_draw(&mut wp, &mut state, 1200, 760);
        let x_after = wp.laid.iter().find(|l| l.id == block).unwrap().text_x;
        assert!(
            (x_after - x_before - 60.0).abs() < 1.0,
            "text reflowed right by the indent (before {x_before}, after {x_after})"
        );
    }

    /// Clicking the ruler in the page span adds a tab stop on the caret paragraph;
    /// pressing Tab then advances the caret to it (instead of inserting spaces).
    #[test]
    fn clicking_ruler_adds_tab_stop_and_tab_advances_to_it() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "ab");
        pipeline_draw(&mut wp, &mut state, 1200, 760);
        let block = wp.model().caret().block.clone();
        assert!(
            wp.model().block_tab_stops(&state.doc, &block).is_empty(),
            "no tab stops initially"
        );

        // Click on the ruler, well inside the text region (no marker there).
        let page_x = wp.page_rect.x0;
        let tab_content_x = page_x + wp.margins.left + 200.0;
        wp.pointer = Point::new(
            wp.content_x_to_screen(tab_content_x),
            f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1),
        );
        wp.ruler_press(&mut state.doc);
        let stops = wp.model().block_tab_stops(&state.doc, &block);
        assert_eq!(stops.len(), 1, "a tab stop was stored on the block");
        assert!(
            (stops[0] - 200.0).abs() < 2.0,
            "the stop is ~200 px from the left text margin (got {})",
            stops[0]
        );

        // Caret at end of "ab". Tab advances to the stop (records a tab char).
        wp.model_mut().move_end(&state.doc);
        let off_before = wp.model().caret().offset;
        press_tab(&mut wp, &mut state);
        let off_after = wp.model().caret().offset;
        assert_eq!(
            off_after,
            off_before + 1,
            "Tab advanced the caret by one (to the tab stop), not four spaces"
        );
        // The advance is recorded as a single tab char (not spaces).
        assert!(
            wp.model().current_text(&state.doc).contains('\t'),
            "a tab character records the advance"
        );
        assert!(
            !wp.model().current_text(&state.doc).contains("    "),
            "Tab did not fall back to inserting spaces"
        );
    }

    /// With no tab stops, Tab keeps the legacy behaviour (inserts spaces) so the
    /// key is never a dead no-op.
    #[test]
    fn tab_without_stops_inserts_spaces() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "x");
        pipeline_draw(&mut wp, &mut state, 1200, 760);
        press_tab(&mut wp, &mut state);
        assert!(
            wp.model().current_text(&state.doc).contains("    "),
            "no stops → Tab inserts spaces as before"
        );
    }

    /// Dragging a tab marker below the ruler band removes the stop ("drag off").
    #[test]
    fn dragging_tab_marker_off_removes_it() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "para");
        let block = wp.model().caret().block.clone();
        wp.model_mut()
            .set_block_tab_stops(&mut state.doc, &block, &[150.0]);
        pipeline_draw(&mut wp, &mut state, 1200, 760);

        let page_x = wp.page_rect.x0;
        let page_w = wp.page_rect.width();
        let markers = wp.ruler_markers(page_x, page_w);
        let tx = markers
            .iter()
            .find(|(k, _)| matches!(k, RulerMarker::Tab(0)))
            .map(|(_, x)| *x)
            .unwrap();
        wp.pointer = Point::new(tx, f64::midpoint(wp.ruler.rect().y0, wp.ruler.rect().y1));
        wp.ruler_press(&mut state.doc);
        assert!(
            matches!(wp.ruler.drag_marker(), Some(RulerMarker::Tab(0))),
            "grabbed the tab marker"
        );

        // Drag below the band → flagged for removal, then release removes it.
        wp.pointer = Point::new(tx, wp.ruler.rect().y1 + 40.0);
        wp.ruler_drag_move();
        assert!(
            wp.ruler.dropping_tab(),
            "dragging below the band flags drop-off"
        );
        wp.ruler_drag_release(&mut state.doc);
        assert!(
            wp.model().block_tab_stops(&state.doc, &block).is_empty(),
            "the tab stop was removed"
        );
    }

    // ---- Accessibility ------------------------------------------------------

    /// A 2×3 RGB PNG fixture (shared with the image-layout test).
    const A11Y_TINY_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2,
        0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174,
        66, 96, 130,
    ];

    /// The accessibility tree's root is a focusable, multiline [`Role::Document`].
    #[test]
    fn accessibility_root_is_a_multiline_document() {
        use ui::a11y::Role;
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();
        assert_eq!(tree.node.role, Role::Document);
        assert!(tree.node.state.multiline, "the document is multiline");
        assert!(tree.node.state.focusable, "the document is focusable");

        // The document's `focused` flag is derived from the widget's focus state,
        // not stored separately, so it tracks `set_focus`.
        Widget::set_focus(&mut wp, false);
        assert!(
            !wp.accessibility_tree().node.state.focused,
            "blurred editor → not focused"
        );
        Widget::set_focus(&mut wp, true);
        assert!(
            wp.accessibility_tree().node.state.focused,
            "focused editor → focused"
        );

        // The trait method returns the root *and* its synthetic subtree; this is
        // the exact host path consumed by AccessKit/AT-SPI.
        let trait_root = Widget::accessibility(&wp).expect("editor exposes a document node");
        assert_eq!(trait_root.role, Role::Document);
        assert_eq!(trait_root.name.as_deref(), Some("Document editor"));
        assert_eq!(
            trait_root.children.len(),
            tree.children.len(),
            "Widget::accessibility() exports every private tree child"
        );
    }

    /// A heading + a paragraph yield a heading node (correct level + text) and a
    /// paragraph node carrying the paragraph text, under the document root.
    #[test]
    fn accessibility_tree_has_heading_and_paragraph_nodes() {
        use ui::a11y::Role;
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);

        // First block: a level-1 heading "Title".
        wp.model_mut().insert_str(&mut state.doc, "Title");
        wp.model_mut()
            .set_paragraph_style(&mut state.doc, "heading", Some(1), None);
        wp.model_mut().enter(&mut state.doc);
        // Second block: a body paragraph.
        wp.model_mut()
            .set_paragraph_style(&mut state.doc, "paragraph", None, None);
        wp.model_mut().insert_str(&mut state.doc, "Body text here");
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();

        let heading = tree
            .children
            .iter()
            .find(|c| matches!(c.node.role, Role::Heading { .. }))
            .expect("a heading node is present");
        assert_eq!(
            heading.node.role,
            Role::Heading { level: 1 },
            "level-1 heading"
        );
        assert_eq!(
            heading.node.name.as_deref(),
            Some("Title"),
            "heading carries its text"
        );

        let paragraph = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Paragraph)
            .expect("a paragraph node is present");
        assert_eq!(
            paragraph.node.value.as_deref(),
            Some("Body text here"),
            "paragraph carries its text",
        );
    }

    /// An image block whose `alt` attr is set (as the ODT/DOCX importers do)
    /// yields a [`Role::Image`] node carrying that alt text. Asserted on the
    /// production node-builder (`block_access_node`) with a directly-built
    /// [`BlockView`], since setting an arbitrary block attr is not on the model's
    /// public surface.
    #[test]
    fn accessibility_image_node_carries_alt() {
        use qdoc::snapshot::{AttrVal, BlockView};
        use std::collections::BTreeMap;
        use ui::a11y::Role;

        let mut attrs = BTreeMap::new();
        attrs.insert("blob".to_string(), AttrVal::Str("deadbeef".into()));
        attrs.insert("alt".to_string(), AttrVal::Str("a tiny dot".into()));
        let img = BlockView {
            id: "img1".into(),
            kind: "image".into(),
            text: String::new(),
            marks: Vec::new(),
            attrs,
            children: Vec::new(),
        };

        let node = WordProcessorWidget::block_access_node(&img).expect("image yields a node");
        assert_eq!(node.role, Role::Image);
        assert_eq!(
            node.name.as_deref(),
            Some("a tiny dot"),
            "image node carries its alt text"
        );

        // With no `alt` attr the block's own text (a caption) is the fallback name.
        let mut captioned = img;
        captioned.attrs.remove("alt");
        captioned.text = "Figure 1".into();
        let cap_node = WordProcessorWidget::block_access_node(&captioned).unwrap();
        assert_eq!(
            cap_node.name.as_deref(),
            Some("Figure 1"),
            "caption text is the alt fallback"
        );

        // And a genuinely-inserted image surfaces as an Image node in the full tree.
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "before");
        wp.model_mut()
            .insert_image_bytes(&mut state.doc, A11Y_TINY_PNG);
        wp.sync(&mut state.doc, &state.root);
        let tree = wp.accessibility_tree();
        assert!(
            tree.children.iter().any(|c| c.node.role == Role::Image),
            "an inserted image appears as an image node in the document tree",
        );
    }

    /// A table's cells are surfaced as [`Role::Cell`] children of a single
    /// [`Role::Table`] node (not as loose top-level blocks).
    #[test]
    fn accessibility_tree_groups_table_cells_under_a_table() {
        use ui::a11y::Role;
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_table(&mut state.doc, 2, 2);
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();
        let table = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Table)
            .expect("a table node is present");
        assert_eq!(table.children.len(), 4, "the 2×2 table has four cell nodes");
        assert!(
            table.children.iter().all(|c| c.node.role == Role::Cell),
            "every table child is a cell",
        );
        // No loose cell nodes leaked to the document root.
        assert!(
            !tree.children.iter().any(|c| c.node.role == Role::Cell),
            "cells are not top-level document children",
        );
    }

    /// Once a frame has been drawn (the toolbar is laid out), the tree carries a
    /// [`Role::Toolbar`] group of button nodes with spelled-out accessible names.
    #[test]
    fn accessibility_tree_exposes_toolbar_buttons() {
        use ui::a11y::Role;

        // Assertions below are literal English button names; pin the locale so
        // the test doesn't depend on the host's `LANG` (nextest runs each test
        // in its own process, so this can't leak into a sibling test).
        crate::i18n::set_locale("en-US");
        let (mut state, mut wp) = populated();
        pipeline_draw(&mut wp, &mut state, 1100, 760);
        // Re-sync so the tree reflects the drawn toolbar.
        wp.sync(&mut state.doc, &state.root);

        let tree = wp.accessibility_tree();
        let toolbar = tree
            .children
            .iter()
            .find(|c| c.node.role == Role::Toolbar)
            .expect("a toolbar group is present after a draw");
        assert!(!toolbar.children.is_empty(), "the toolbar has button nodes");
        // The buttons read with human names, not the terse visible glyphs.
        assert!(
            toolbar
                .children
                .iter()
                .filter_map(|c| c.node.name.as_deref())
                .any(|n| n == "Bold"),
            "the Bold button is named for a screen reader"
        );
        assert!(
            toolbar
                .children
                .iter()
                .any(|c| matches!(c.node.role, Role::Button | Role::ToggleButton)),
            "the toolbar contains actionable button nodes",
        );

        let exported = Widget::accessibility(&wp).expect("exported semantics");
        let exported_toolbar = exported
            .children
            .iter()
            .find(|node| node.role == Role::Toolbar)
            .expect("toolbar reaches the host tree");
        assert!(exported_toolbar.children.iter().all(|node| {
            node.bounds.is_some() && node.action_target.is_some() && node.state.focusable
        }));
    }

    #[test]
    fn accessibility_activation_uses_the_real_toolbar_path() {
        let (mut state, mut wp) = populated();
        pipeline_draw(&mut wp, &mut state, 1100, 760);
        assert!(!wp.styles_panel_open);
        let target = "word-processor.toolbar:toggle-styles".to_string();
        assert!(Widget::perform_action_at(
            &mut wp,
            &ui::a11y::Action::Activate,
            &[ui::a11y::hash_key(&target)],
            &mut state.doc,
            &state.root,
        ));
        assert!(
            wp.styles_panel_open,
            "AT activation reaches the same view toggle as a pointer click"
        );
    }

    // ---- collaboration: remote presence rendering --------------------------

    use ui::permissions::UserId;
    use ui::presence::SelectionRange;

    /// The id of the first laid-out block (after a draw) — the block a remote
    /// peer's caret/selection will be keyed to.
    fn first_block_id(wp: &WordProcessorWidget) -> String {
        wp.laid
            .first()
            .expect("at least one block was laid out")
            .id
            .clone()
    }

    /// A populated, focused widget that has been drawn once (so `self.laid` +
    /// `self.layout_cache` are live and `remote_presence_overlays` is
    /// meaningful).
    fn drawn_focused() -> (ui::State, WordProcessorWidget) {
        let (mut state, mut wp) = populated();
        wp.set_focus(true);
        pipeline_draw(&mut wp, &mut state, 1100, 760);
        (state, wp)
    }

    /// An empty `PresenceCtx` (single-user / no broker) yields no remote
    /// caret/selection overlays — proving the collab render path is a strict
    /// no-op when there are no peers, so single-user editing is unchanged.
    #[test]
    fn remote_presence_is_noop_without_peers() {
        let (_state, wp) = drawn_focused();
        let presence = PresenceCtx::empty();
        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(carets.is_empty(), "no remote carets without peers");
        assert!(selections.is_empty(), "no remote selections without peers");
    }

    /// The pill over a peer's caret shows who is editing, not which machine —
    /// the room's assigned name for the peer's transport id, never the raw id
    /// itself (which reads as noise like `peer@5537458103208628978`).
    #[test]
    fn a_named_peers_caret_is_labelled_with_their_name() {
        let (_state, wp) = drawn_focused();
        let block_id = first_block_id(&wp);
        let peer = "peer@5537458103208628978";

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret(peer.into(), block_presence_key(&block_id), 3);
        presence.set_display_name(ui::permissions::UserId(peer.into()), "bob");

        let (carets, _) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].label, "bob");
        assert_eq!(
            carets[0].user.0, peer,
            "the id stays the identity — it is what the caret is coloured by",
        );
    }

    /// And a peer nobody named keeps its id on the pill: not blank, and
    /// not a person who does not exist. An unlabelled caret is honest;
    /// an invented name is a claim about who is in the room.
    #[test]
    fn an_unnamed_peers_caret_is_labelled_with_its_id() {
        let (_state, wp) = drawn_focused();
        let block_id = first_block_id(&wp);
        let peer = "peer@5537458103208628978";

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret(peer.into(), block_presence_key(&block_id), 3);

        let (carets, _) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1);
        assert_eq!(carets[0].label, peer);
        assert!(!carets[0].label.trim().is_empty());
    }

    /// A `PresenceCtx` carrying a remote collaborator's caret (under the same
    /// per-block key the widget broadcasts the local caret under) yields exactly
    /// one remote-caret overlay, positioned inside the laid block — proving
    /// `draw` consumes presence and maps a remote byte offset onto on-screen
    /// caret geometry via the production `caret_geometry` path.
    #[test]
    fn remote_caret_is_resolved_to_on_screen_rect() {
        let (_state, wp) = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        // Place "alice"'s caret 3 bytes into the first block.
        presence.set_remote_caret("alice".into(), block_presence_key(&block_id), 3);

        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 1, "one remote caret resolved");
        assert!(
            selections.is_empty(),
            "a bare caret yields no selection rects"
        );
        let c = &carets[0];
        assert_eq!(c.user, UserId("alice".into()));
        // The caret rect must be a real, non-degenerate vertical bar (the
        // `CARET_WIDTH`-wide, line-tall geometry produced by `caret_geometry`),
        // and it must land within the block's laid rectangle.
        let block = wp.laid.iter().find(|l| l.id == block_id).unwrap();
        assert!(
            (c.rect.width() - CARET_WIDTH).abs() < 1e-6,
            "caret is CARET_WIDTH wide: {:?}",
            c.rect
        );
        assert!(
            c.rect.height() > 1.0,
            "caret has a real height: {:?}",
            c.rect
        );
        assert!(
            c.rect.x0 >= block.text_x - 0.5,
            "caret sits at/after the block's text edge"
        );
        assert!(
            c.rect.y0 >= block.y_top - 0.5,
            "caret sits within the block vertically"
        );
    }

    /// A remote selection range yields one-or-more non-degenerate highlight
    /// rectangles (a single-line selection is one rect), mapped through the same
    /// `selection_rects` geometry the local selection uses.
    #[test]
    fn remote_selection_is_resolved_to_highlight_rects() {
        let (_state, wp) = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        // "bob" selects bytes [0, 4) of the first block.
        presence.set_remote_selection("bob".into(), block_presence_key(&block_id), 0, 4);

        let (carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(carets.is_empty(), "a selection yields no bare caret");
        assert_eq!(selections.len(), 1, "one remote selection resolved");
        let s = &selections[0];
        assert_eq!(s.user, UserId("bob".into()));
        assert!(
            !s.rects.is_empty(),
            "the selection produced at least one highlight rect"
        );
        for r in &s.rects {
            assert!(
                r.width() > 0.0 && r.height() > 0.0,
                "highlight rect is non-degenerate: {r:?}"
            );
        }
    }

    /// A degenerate (empty) remote selection range produces no highlight rects —
    /// the render guards against a zero-width selection painting a stray band.
    #[test]
    fn empty_remote_selection_produces_no_rects() {
        let (_state, wp) = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        presence.selections.insert(
            block_presence_key(&block_id),
            vec![(UserId("bob".into()), SelectionRange { start: 2, end: 2 })],
        );

        let (_carets, selections) = wp.remote_presence_overlays(&presence);
        assert!(
            selections.is_empty(),
            "an empty selection range paints nothing"
        );
    }

    /// Two peers on DIFFERENT blocks each get their own caret overlay, attributed
    /// to the right user — the per-block key keeps them from colliding.
    #[test]
    fn distinct_peers_on_distinct_blocks_resolve_independently() {
        let (_state, wp) = drawn_focused();
        assert!(
            wp.laid.len() >= 2,
            "the populated doc lays out at least two blocks"
        );
        let b0 = wp.laid[0].id.clone();
        let b1 = wp.laid[1].id.clone();

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("alice".into(), block_presence_key(&b0), 1);
        presence.set_remote_caret("bob".into(), block_presence_key(&b1), 1);

        let (carets, _selections) = wp.remote_presence_overlays(&presence);
        assert_eq!(carets.len(), 2, "both peers' carets resolved");
        let users: Vec<&str> = carets.iter().map(|c| c.user.0.as_str()).collect();
        assert!(
            users.contains(&"alice") && users.contains(&"bob"),
            "got {users:?}"
        );
        // The two carets land in different blocks (different vertical bands).
        let ya = carets.iter().find(|c| c.user.0 == "alice").unwrap().rect.y0;
        let yb = carets.iter().find(|c| c.user.0 == "bob").unwrap().rect.y0;
        assert!(
            (ya - yb).abs() > 1.0,
            "carets in different blocks have distinct y positions"
        );
    }

    /// The full `draw_with_presence` path consumes a populated `PresenceCtx`
    /// without panicking AND the resolver agrees — i.e. the production draw and
    /// the asserted helper are the same path, not a parallel re-implementation.
    #[test]
    fn draw_with_presence_renders_remote_caret_through_production_path() {
        let (mut state, mut wp) = drawn_focused();
        let block_id = first_block_id(&wp);

        let mut presence = PresenceCtx::empty();
        presence.set_remote_caret("charlie".into(), block_presence_key(&block_id), 2);

        // Run the REAL five-pass draw with the populated presence (the same call
        // the App makes each frame). It must paint without panicking.
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        wp.sync(&mut state.doc, &state.root);
        let child = wp.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1100.0),
                        height: Dimension::length(760.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(1100.0),
                        height: AvailableSpace::Definite(760.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        wp.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        wp.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &presence,
            &permission,
        );

        // After the production draw, the resolver (which the draw itself called)
        // reports the same single remote caret — proving the draw consumed it.
        let (carets, _sel) = wp.remote_presence_overlays(&presence);
        assert_eq!(
            carets.len(),
            1,
            "the production draw consumed the remote caret"
        );
        assert_eq!(carets[0].user, UserId("charlie".into()));
    }

    // ---- collaboration: outbound presence (presence_payload) ---------------

    /// While focused, the widget reports its local caret as outbound presence,
    /// keyed to the caret's block and carrying a BYTE offset (so a peer can feed
    /// it straight into the layout geometry). The App polls this each frame and
    /// broadcasts the diff to peers.
    #[test]
    fn presence_payload_reports_focused_caret_in_bytes() {
        let (mut state, mut wp) = populated();
        wp.set_focus(true);
        // Move the caret to char offset 3 of the caret block.
        let block = wp.model().caret().block.clone();
        wp.model_mut().set_caret(Caret {
            block: block.clone(),
            offset: 3,
        });
        // A draw populates the block cache (needed for char→byte conversion).
        pipeline_draw(&mut wp, &mut state, 1100, 760);

        match wp.presence_payload() {
            Some((key, ui::PresencePayload::Caret { pos })) => {
                assert_eq!(
                    key,
                    block_presence_key(&block),
                    "keyed to the caret's block"
                );
                // The caret block text is ASCII here, so byte offset == char offset.
                assert_eq!(pos, 3, "caret reported at byte 3");
            }
            other => panic!("expected a Caret payload, got {other:?}"),
        }
    }

    /// A selection takes priority over the bare caret in the outbound payload,
    /// reported as a byte range over the caret block.
    #[test]
    fn presence_payload_reports_selection_when_present() {
        let (mut state, mut wp) = populated();
        wp.set_focus(true);
        pipeline_draw(&mut wp, &mut state, 1100, 760);
        // Select chars [1, 4) of the caret block.
        select_range(&mut wp, 1, 4);
        // Re-draw so the selection is reflected in the cached block views.
        pipeline_draw(&mut wp, &mut state, 1100, 760);

        let block = wp.model().caret().block.clone();
        match wp.presence_payload() {
            Some((key, ui::PresencePayload::Selection { start, end })) => {
                assert_eq!(key, block_presence_key(&block));
                assert_eq!((start, end), (1, 4), "selection reported as a byte range");
            }
            other => panic!("expected a Selection payload, got {other:?}"),
        }
    }

    /// An unfocused widget broadcasts no presence (so a background editor doesn't
    /// flood peers with a stale caret).
    #[test]
    fn presence_payload_is_none_when_unfocused() {
        let (mut state, mut wp) = populated();
        pipeline_draw(&mut wp, &mut state, 1100, 760);
        wp.set_focus(false);
        assert!(
            wp.presence_payload().is_none(),
            "no presence while unfocused"
        );
    }

    // ---- image insert via the host file-chooser portal --------------------

    /// A minimal, real 2×3 PNG used by the image tests (decodes cleanly so the
    /// insert path records intrinsic dimensions).
    const IMG_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2,
        0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174,
        66, 96, 130,
    ];

    /// Write `IMG_PNG` to a unique temp file and return its path.
    fn temp_png(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("wp-img-{tag}-{}.png", std::process::id()));
        std::fs::write(&p, IMG_PNG).expect("write temp png");
        p
    }

    fn image_block_count(wp: &WordProcessorWidget, state: &ui::State) -> usize {
        let c = wp.model().containers.clone().unwrap();
        qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .filter(|b| b.kind == "image")
            .count()
    }

    /// Pressing the toolbar Image button asks the **host portal** for a file (it
    /// does NOT fabricate a path); nothing is inserted until the pick resolves.
    #[test]
    fn image_button_opens_the_host_portal() {
        let (mut state, mut wp) = bound();
        assert!(
            wp.portal_rx.is_none(),
            "no portal request before the button is pressed"
        );
        wp.apply_tool(&mut state.doc, ToolAction::InsertImage);
        assert!(
            wp.portal_rx.is_some(),
            "the Image button opens the host portal chooser"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            0,
            "no image inserted until the portal returns a file"
        );
    }

    /// A resolved portal pick (a chosen file) is folded in on the next frame's
    /// `sync` drain and inserts a real image block referencing the file's bytes.
    #[test]
    fn a_resolved_portal_pick_inserts_an_image() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "before");
        let path = temp_png("pick");
        // Feed the exact per-frame drain path a chosen-file outcome for an image.
        wp.portal_rx = Some(portal::inject_for_test(portal::PortalOutcome::Opened(
            path.clone(),
        )));
        wp.portal_purpose = Some(PortalPurpose::InsertImage);
        wp.sync(&mut state.doc, &state.root);
        let _ = std::fs::remove_file(&path);
        assert!(
            wp.portal_rx.is_none(),
            "a resolved portal request clears the channel"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            1,
            "the picked file inserts one image block"
        );
    }

    /// A cancelled portal pick inserts nothing and clears the in-flight channel.
    #[test]
    fn a_cancelled_portal_pick_inserts_nothing() {
        let (mut state, mut wp) = bound();
        wp.portal_rx = Some(portal::inject_for_test(portal::PortalOutcome::Cancelled));
        wp.portal_purpose = Some(PortalPurpose::InsertImage);
        wp.sync(&mut state.doc, &state.root);
        assert!(
            wp.portal_rx.is_none(),
            "a cancel clears the in-flight channel"
        );
        assert_eq!(
            image_block_count(&wp, &state),
            0,
            "a cancel inserts nothing"
        );
    }

    /// A docx opened → edited → **Save As** to a new `.docx` writes a *real* Word
    /// file (an OOXML ZIP, `PK` signature) that round-trips back through the
    /// opener with the edit intact — proving the portal Save-As path drives the
    /// qdoc docx codec at the user-chosen path.
    #[test]
    fn save_as_writes_a_real_docx_that_round_trips() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();
        let src = dir.join(format!("wp-src-{pid}.docx"));
        let dest = dir.join(format!("wp-dest-{pid}.docx"));

        // Author a real source .docx via the codec (a seeded doc → save_document).
        {
            let mut state = ui::State::new();
            with_doc(&mut state, |dm| {
                let b = dm.insert_block(None, "paragraph");
                dm.splice_text(&b, 0, 0, "Hello docx");
            });
            crate::save_document(&state, &src, WpFormat::Docx).expect("write source docx");
        }
        assert!(
            std::fs::read(&src).unwrap().starts_with(b"PK"),
            "the source is a real docx (OOXML zip)"
        );

        // Open it into a running editor and confirm the text came through.
        let (mut state, mut wp, fmt) = crate::open_document(&src).expect("open source docx");
        assert_eq!(fmt, WpFormat::Docx);
        wp.sync(&mut state.doc, &state.root);
        assert!(
            body_text(&wp, &state)
                .iter()
                .any(|t| t.contains("Hello docx")),
            "opened body carries the source text"
        );

        // Edit, then Save As to a NEW .docx through the real portal drain path.
        wp.model_mut().focus_body(&mut state.doc); // seat the caret on a real block
        wp.model_mut().insert_str(&mut state.doc, "EDITED ");
        wp.portal_rx = Some(portal::inject_for_test(portal::PortalOutcome::Opened(
            dest.clone(),
        )));
        wp.portal_purpose = Some(PortalPurpose::SaveAs);
        wp.sync(&mut state.doc, &state.root);

        // Save As adopted the chosen path/format and cleared the in-flight request.
        assert!(wp.portal_rx.is_none(), "the Save-As request drained");
        assert_eq!(
            wp.path.as_deref(),
            Some(dest.as_path()),
            "path is now the Save-As target"
        );
        assert_eq!(
            wp.format,
            Some(WpFormat::Docx),
            "format follows the chosen extension"
        );
        assert!(
            matches!(wp.toast.as_ref(), Some((_, ToastKind::Success))),
            "a completed save is visibly confirmed"
        );

        // The written file is a real docx (ZIP signature) — not markdown ...
        let written = std::fs::read(&dest).expect("Save As wrote the destination");
        assert!(
            written.starts_with(b"PK"),
            "Save As wrote a real docx zip, not a markdown fallback"
        );
        // ... and it round-trips back through the opener with both texts intact.
        let (mut state2, mut wp2, fmt2) =
            crate::open_document(&dest).expect("reopen the saved docx");
        assert_eq!(fmt2, WpFormat::Docx);
        wp2.sync(&mut state2.doc, &state2.root); // bind containers before reading
        let reopened = body_text(&wp2, &state2).join("\n");
        assert!(
            reopened.contains("Hello docx"),
            "original text survives: {reopened:?}"
        );
        assert!(
            reopened.contains("EDITED"),
            "the edit survives: {reopened:?}"
        );

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn failed_save_is_visible_and_keeps_the_document_modified() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "unsaved work");
        wp.modified = true;
        // Writing bytes to a directory reliably fails without relying on file
        // permissions (the test suite may run as a privileged user).
        wp.set_file(std::env::temp_dir(), WpFormat::Markdown);
        wp.write_to_current_path(&state.doc);

        assert!(wp.modified, "a failed save never clears the dirty state");
        assert!(
            matches!(wp.toast.as_ref(), Some((_, ToastKind::Error))),
            "the user sees an error toast instead of an stderr-only failure"
        );
    }

    /// A resolved **Open** pick replaces the running document in place: a blank
    /// editor, handed a chosen `.docx` on the drain path, adopts that file's text,
    /// path, and format — the Ctrl+O path, exercised without a live portal.
    #[test]
    fn open_pick_replaces_the_document_in_place() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("wp-open-{}.docx", std::process::id()));
        {
            let mut state = ui::State::new();
            with_doc(&mut state, |dm| {
                let b = dm.insert_block(None, "paragraph");
                dm.splice_text(&b, 0, 0, "Opened via portal");
            });
            crate::save_document(&state, &src, WpFormat::Docx).expect("write source docx");
        }

        // Start from a blank, untitled editor.
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "stale content");
        assert!(wp.path.is_none(), "untitled before opening");

        // Feed the exact per-frame drain path an Open outcome.
        wp.portal_rx = Some(portal::inject_for_test(portal::PortalOutcome::Opened(
            src.clone(),
        )));
        wp.portal_purpose = Some(PortalPurpose::OpenDocument);
        wp.sync(&mut state.doc, &state.root);

        let body = body_text(&wp, &state).join("\n");
        assert!(
            body.contains("Opened via portal"),
            "opened file's text is now shown: {body:?}"
        );
        assert!(
            !body.contains("stale content"),
            "the previous document was replaced"
        );
        assert_eq!(
            wp.path.as_deref(),
            Some(src.as_path()),
            "opened path adopted"
        );
        assert_eq!(wp.format, Some(WpFormat::Docx), "opened format adopted");

        let _ = std::fs::remove_file(&src);
    }

    /// Dropping an image file onto the page inserts it at the **drop point**: the
    /// caret is first seated under the pointer, so the image lands next to the
    /// paragraph the file was dropped on — not wherever the caret happened to be.
    #[test]
    fn dropped_image_inserts_at_the_drop_point() {
        let (mut wp, mut state) = doc_with_headings(1000, 700);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        // Pick a body paragraph well down the page to drop onto.
        let target = wp
            .laid
            .iter()
            .find(|l| l.kind == "paragraph" && l.y_top > wp.rect.y0 + 200.0)
            .expect("a body paragraph to drop onto")
            .clone();
        let drop_pt = Point::new(target.text_x + 20.0, target.y_top + target.height / 2.0);

        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        // Move the pointer over the drop target (as a real drag-hover would), then
        // drop the file there.
        wp.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: (drop_pt.x, drop_pt.y).into(),
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        let path = temp_png("drop");
        wp.handle_event(
            &WindowEvent::DroppedFile(path.clone()),
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            image_block_count(&wp, &state),
            1,
            "the dropped file inserts one image"
        );

        // The image landed adjacent to the drop-target paragraph — its document
        // index is right after the paragraph the pointer was over.
        let c = wp.model().containers.clone().unwrap();
        let order: Vec<String> = qdoc::model::blocks_in_order(&state.doc, &c);
        let img_idx = qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .position(|b| b.kind == "image")
            .and_then(|i| order.get(i).cloned());
        let target_idx = order
            .iter()
            .position(|id| id == &target.id)
            .expect("drop target in order");
        let img_pos = order
            .iter()
            .position(|id| Some(id) == img_idx.as_ref())
            .expect("image in order");
        assert!(
            img_pos == target_idx + 1 || img_pos == target_idx,
            "image inserted at the drop point (target idx {target_idx}, image idx {img_pos})"
        );
    }

    // ---- image resize / flip inspector ------------------------------------

    /// Insert an image (via the model), select it (caret on the image block), draw
    /// so the panel builds its controls, then return the widget + state ready for
    /// the resize / flip tests.
    fn doc_with_selected_image() -> (WordProcessorWidget, ui::State, String) {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "caption");
        wp.model_mut().insert_image_bytes(&mut state.doc, IMG_PNG);
        wp.sync(&mut state.doc, &state.root);
        let c = wp.model().containers.clone().unwrap();
        let img_id = qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .find(|b| b.kind == "image")
            .expect("image inserted")
            .id
            .clone();
        // Select the image: seat the caret on its block.
        wp.model_mut().set_caret(crate::word_processor::Caret {
            block: img_id.clone(),
            offset: 0,
        });
        wp.set_styles_panel_open(true);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        (wp, state, img_id)
    }

    /// A selected image exposes resize + flip controls in the panel.
    #[test]
    fn selected_image_shows_resize_and_flip_controls() {
        let (wp, _state, _id) = doc_with_selected_image();
        assert!(wp.selected_image_id().is_some(), "the image is selected");
        let has = |want: &PanelHit| wp.panel_hits.iter().any(|(_, h)| h == want);
        assert!(
            wp.panel_hits
                .iter()
                .any(|(_, h)| matches!(h, PanelHit::ImageScale(_))),
            "resize steppers present"
        );
        assert!(
            has(&PanelHit::ImageFlipH),
            "flip-horizontal control present"
        );
        assert!(has(&PanelHit::ImageFlipV), "flip-vertical control present");
    }

    /// Clicking the panel's "Larger" control resizes the selected image (its
    /// explicit width grows), driven through the production panel-hit path.
    #[test]
    fn selected_image_resizes_from_the_panel() {
        let (mut wp, mut state, id) = doc_with_selected_image();
        let (larger_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| matches!(h, PanelHit::ImageScale(f) if *f > 1.0))
            .expect("a 'Larger' resize control")
            .clone();
        let pt = Point::new(
            f64::midpoint(larger_rect.x0, larger_rect.x1),
            f64::midpoint(larger_rect.y0, larger_rect.y1),
        );
        left_click_state(&mut wp, &mut state, pt);
        let (w, _h) = wp
            .model()
            .image_size(&state.doc, &id)
            .expect("image now has an explicit size");
        assert!(
            w > 2.0,
            "the image grew beyond its 2px intrinsic width (w={w})"
        );
    }

    /// Clicking the panel's "Flip Horizontal" control mirrors the selected image
    /// (sets its `flip_h` attr) through the production path.
    #[test]
    fn selected_image_flips_from_the_panel() {
        let (mut wp, mut state, id) = doc_with_selected_image();
        let (flip_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| *h == PanelHit::ImageFlipH)
            .expect("a flip-horizontal control")
            .clone();
        let pt = Point::new(
            f64::midpoint(flip_rect.x0, flip_rect.x1),
            f64::midpoint(flip_rect.y0, flip_rect.y1),
        );
        left_click_state(&mut wp, &mut state, pt);
        let c = wp.model().containers.clone().unwrap();
        let flipped = qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .find(|b| b.id == id)
            .is_some_and(|b| matches!(b.attrs.get("flip_h"), Some(qdoc::AttrVal::Bool(true))));
        assert!(
            flipped,
            "the image's flip_h attr is set after clicking Flip Horizontal"
        );
    }

    /// A left click at `pt` through the production `handle_event`, taking `ui::State`.
    fn left_click_state(wp: &mut WordProcessorWidget, state: &mut ui::State, pt: Point) {
        left_click(wp, state, pt);
    }

    // ---- paragraph-style delete / duplicate context menu ------------------

    /// A doc with one custom paragraph style applied to its (only) paragraph.
    fn doc_with_custom_style() -> (WordProcessorWidget, ui::State, String) {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut()
            .insert_str(&mut state.doc, "styled paragraph");
        // Duplicate Body into a fresh custom style and apply it to the caret block.
        let new_id = wp
            .model_mut()
            .duplicate_style(&mut state.doc, "body")
            .expect("duplicate body");
        wp.model_mut().set_block_style(&mut state.doc, &new_id);
        wp.sync(&mut state.doc, &state.root);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        (wp, state, new_id)
    }

    /// Deleting a custom style removes it from the document's sheet and reassigns
    /// the block that used it (so no dangling style ref remains).
    #[test]
    fn deleting_a_custom_style_removes_it() {
        let (mut wp, mut state, custom_id) = doc_with_custom_style();
        assert!(
            wp.sheet.styles.contains_key(&custom_id),
            "custom style present before delete"
        );

        // Apply the typed outcome produced by the extracted menu component.
        wp.dispatch_style_menu(StyleMenuAction::Delete(custom_id.clone()), &mut state.doc);
        wp.sync(&mut state.doc, &state.root);

        assert!(
            !wp.sheet.styles.contains_key(&custom_id),
            "custom style removed from the sheet"
        );
        assert!(
            !qdoc::model::style_ids(&state.doc, &wp.model().containers.clone().unwrap())
                .iter()
                .any(|s| s == &custom_id),
            "custom style removed from the document"
        );
        // The block no longer references the deleted style.
        let c = wp.model().containers.clone().unwrap();
        let dangling = qdoc::model::materialize(&state.doc, &c).blocks.iter().any(
            |b| matches!(b.attrs.get("style"), Some(qdoc::AttrVal::Str(s)) if s == &custom_id),
        );
        assert!(!dangling, "no block still references the deleted style");
    }

    /// A built-in style cannot be deleted through the model even if asked.
    #[test]
    fn builtin_style_is_never_deleted() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "x");
        assert!(
            !wp.model_mut().delete_style(&mut state.doc, "body"),
            "built-in delete is refused"
        );
        wp.sync(&mut state.doc, &state.root);
        assert!(
            wp.sheet.styles.contains_key("body"),
            "Body survives a delete attempt"
        );
    }

    // ---- font size lives in exactly one place -----------------------------

    /// Font size has a single home: the side inspector's "Size" field. The
    /// toolbar carries no size control (its old A−/value/A+ stepper was removed).
    #[test]
    fn font_size_lives_in_exactly_one_place() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "Body text");
        wp.styles_panel_open = true;
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let panel_size_fields = wp
            .panel_hits
            .iter()
            .filter(|(_, h)| matches!(h, PanelHit::Num(NumField::Size)))
            .count();
        assert_eq!(
            panel_size_fields, 1,
            "the inspector is the single home for font size"
        );

        // The toolbar is a flat action list; none of its entries is a size control.
        assert!(
            wp.toolbar
                .iter()
                .any(|(a, _)| matches!(a, ToolAction::InsertImage)),
            "the toolbar was drawn (has the Image button)"
        );
        // (Size variants were removed from ToolAction entirely, so the toolbar
        // *cannot* carry a font-size control — this is the structural guarantee.)
    }

    // ---- table-of-contents feedback ---------------------------------------

    /// Generating a TOC leaves a visible confirmation toast (the button used to
    /// give no feedback at all).
    #[test]
    fn toc_action_sets_a_confirmation_toast() {
        let (mut wp, mut state) = doc_with_headings(1000, 700);
        assert!(wp.toast.is_none(), "no toast before the action");
        wp.apply_tool(&mut state.doc, ToolAction::InsertToc);
        assert!(
            wp.toast.is_some(),
            "generating a TOC surfaces a confirmation toast"
        );
        // And it actually produced TOC lines.
        let c = wp.model().containers.clone().unwrap();
        let toc = qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .filter(|b| crate::word_processor::block_is_toc(b))
            .count();
        assert!(toc > 0, "the TOC action inserts visible TOC lines");
    }

    // ---- shared ColorPicker adoption --------------------------------------

    /// The style inspector's "Custom…" affordance opens the shared `ColorPicker`
    /// popover, and dragging within it applies a colour to the focused style —
    /// proving the toolkit widget is adopted (not a bespoke re-implementation).
    #[test]
    fn color_picker_popover_applies_a_colour_to_the_focused_style() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "colour me");
        wp.styles_panel_open = true;
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // Click the "Custom…" affordance to open the picker.
        let (open_rect, _) = wp
            .panel_hits
            .iter()
            .find(|(_, h)| *h == PanelHit::OpenColorPicker)
            .expect("a 'Custom…' colour affordance in the inspector")
            .clone();
        left_click(
            &mut wp,
            &mut state,
            Point::new(
                f64::midpoint(open_rect.x0, open_rect.x1),
                f64::midpoint(open_rect.y0, open_rect.y1),
            ),
        );
        assert!(
            wp.color_picker.is_open(),
            "the shared ColorPicker popover opened"
        );

        // Lay the picker out and drive a press into the top-left of its SV square
        // (high value, some saturation) so it commits a fresh colour.
        wp.color_picker.layout(wp.rect, wp.panel_rect);
        let pr = wp.color_picker.rect();
        let sv_pt = Point::new(pr.x0 + 20.0, pr.y0 + 20.0);
        let style_before = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        // CursorMoved (to set the picker's internal cursor) then press.
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        wp.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: (sv_pt.x, sv_pt.y).into(),
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        wp.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        let style_after = wp.focused_style_key().map(|k| wp.sheet.get(&k).color);
        assert_ne!(
            style_before, style_after,
            "dragging in the ColorPicker changed the focused style's colour"
        );
    }

    /// A click outside the open picker dismisses it.
    #[test]
    fn color_picker_popover_dismisses_on_outside_click() {
        let (mut state, mut wp) = bound();
        wp.model_mut().insert_str(&mut state.doc, "x");
        wp.styles_panel_open = true;
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        wp.open_color_picker();
        assert!(wp.color_picker.is_open());
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        // Click far away from the popover (top-left corner of the canvas).
        left_click(&mut wp, &mut state, Point::new(80.0, 400.0));
        assert!(
            !wp.color_picker.is_open(),
            "an outside click closes the colour picker"
        );
    }

    // ---- visual review shots ----------------------------------------------
    // A guarded renderer that writes PNGs of the fixed surfaces for eyeballing.
    // Set `QOS_WP_REVIEW_SHOTS=<dir>` to emit them; a normal `cargo test` run
    // leaves it a no-op (no GPU work, no files).

    /// Run the full five-pass + overlay draw and write a PNG to `dir/name.png`.
    fn render_review_png(
        wp: &mut WordProcessorWidget,
        state: &mut ui::State,
        w: u32,
        h: u32,
        dir: &std::path::Path,
        name: &str,
    ) {
        use ui::offscreen::OffscreenRenderer;
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        wp.sync(&mut state.doc, &state.root);
        let child = wp.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(num::f32_of_u32(w)),
                        height: Dimension::length(num::f32_of_u32(h)),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(num::f32_of_u32(w)),
                        height: AvailableSpace::Definite(num::f32_of_u32(h)),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        wp.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        wp.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
            &permission,
        );
        wp.draw_overlay(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        std::fs::create_dir_all(dir).ok();
        OffscreenRenderer::new(w, h).render_png(
            &scene,
            Color::from_rgb8(0x20, 0x22, 0x28),
            dir.join(format!("{name}.png")),
        );
    }

    #[test]
    fn render_review_shots() {
        let Some(dir) = std::env::var_os("QOS_WP_REVIEW_SHOTS") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let (w, h) = (1200u32, 800u32);

        // 1. Fixed toolbar: vector Image icon, no duplicated font-size stepper.
        {
            let (mut state, mut wp) = bound();
            wp.model_mut()
                .insert_str(&mut state.doc, "The quick brown fox");
            wp.model_mut().enter(&mut state.doc);
            wp.model_mut().insert_str(
                &mut state.doc,
                "Body paragraph with some words to fill the line nicely.",
            );
            render_review_png(&mut wp, &mut state, w, h, &dir, "toolbar");
        }

        // 2a. Style context menu on a CUSTOM style: Delete + Duplicate.
        {
            let (mut wp, mut state, custom_id) = doc_with_custom_style();
            // Anchor the menu over the custom chip in the panel.
            let anchor = wp
                .panel_hits
                .iter()
                .find(|(_, hh)| matches!(hh, PanelHit::Pick(id) if id == &custom_id))
                .map_or(Point::new(950.0, 240.0), |(r, _)| {
                    Point::new(r.x0 + 10.0, r.y1 + 2.0)
                });
            wp.style_menu.open(&custom_id, anchor);
            render_review_png(&mut wp, &mut state, w, h, &dir, "style-menu-custom");
        }

        // 2b. Style context menu on a BUILT-IN style: Duplicate only.
        {
            let (mut wp, mut state, _id) = doc_with_custom_style();
            let anchor = wp
                .panel_hits
                .iter()
                .find(|(_, hh)| matches!(hh, PanelHit::Pick(id) if id == "body"))
                .map_or(Point::new(950.0, 240.0), |(r, _)| {
                    Point::new(r.x0 + 10.0, r.y1 + 2.0)
                });
            wp.style_menu.open("body", anchor);
            render_review_png(&mut wp, &mut state, w, h, &dir, "style-menu-builtin");
        }

        // 3. Selected image with resize + flip controls in the panel. Give it a
        //    visible display size (the fixture PNG is 2×3) so the selection box +
        //    handles read in the shot.
        {
            let (mut wp, mut state, id) = doc_with_selected_image();
            wp.model_mut()
                .set_image_size(&mut state.doc, &id, 300.0, 200.0);
            wp.model_mut().flip_image(&mut state.doc, &id, true);
            render_review_png(&mut wp, &mut state, w, h, &dir, "image-selected");
        }

        // 4. Generated table of contents (+ its confirmation toast).
        {
            let (mut wp, mut state) = doc_with_headings(w, h);
            wp.apply_tool(&mut state.doc, ToolAction::InsertToc);
            render_review_png(&mut wp, &mut state, w, h, &dir, "toc");
        }

        // 5. The shared ColorPicker popover, opened from the style inspector.
        {
            let (mut state, mut wp) = bound();
            wp.model_mut()
                .insert_str(&mut state.doc, "colour this paragraph");
            wp.styles_panel_open = true;
            pipeline_draw(&mut wp, &mut state, w, h);
            wp.open_color_picker();
            render_review_png(&mut wp, &mut state, w, h, &dir, "color-picker");
        }

        // 6. Comment and tracked-change margin cards from the extracted review
        // component, including its Resolve and Accept/Reject action faces.
        {
            let (mut wp, mut state, c, mut ids, bid) =
                doc_with_paragraph("Keep this passage and review the ending.");
            {
                let mut doc = qdoc::DocMut {
                    doc: &mut state.doc,
                    c: &c,
                    ids: &mut ids,
                };
                doc.add_comment(&bid, 5, 17, "Ada", "This needs a source.");
                let change = doc.insert_change(qdoc::ChangeKind::Ins, "Lin", "");
                doc.add_change_mark(qdoc::ChangeKind::Ins, &bid, 29, 39, &change);
            }
            state.doc.commit();
            render_review_png(&mut wp, &mut state, w, h, &dir, "review-overlay");
        }
    }
}

// ---- version-history strip ------------------------------------------------
//
// These drive the real production path end to end: the toolbar action that
// opens the strip, the strip's own hit-testing through `handle_event`, and the
// restore that writes a forward change into the live document.
#[cfg(test)]
mod history_strip_tests {
    use super::tests::pipeline_draw;
    use super::*;

    /// A full left click: move, press, release. The release matters — pressing
    /// on the scrubber's track latches the handle to the pointer, so a
    /// press-only helper leaves the widget mid-drag and the next cursor move
    /// drags the version out from under the test.
    fn click(wp: &mut WordProcessorWidget, state: &mut ui::State, pt: Point) {
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut send = |wp: &mut WordProcessorWidget, ev: WindowEvent| {
            wp.handle_event(
                &ev,
                1.0,
                &mods,
                &mut state.doc,
                &state.root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        };
        send(
            wp,
            WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: (pt.x, pt.y).into(),
            },
        );
        send(
            wp,
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
        );
        send(
            wp,
            WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            },
        );
    }

    /// Document text as the reader sees it, joined across blocks.
    fn body(state: &ui::State) -> String {
        let c = qdoc::model::open(&state.doc, &state.root).expect("qdoc schema");
        qdoc::model::materialize(&state.doc, &c)
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// An editor holding a document with three committed versions, authored by
    /// two identities — the second edit arrives from a peer, merged the way
    /// collaboration merges it.
    fn editor_with_history(w: u32, h: u32) -> (WordProcessorWidget, ui::State) {
        let mut state = ui::State::new();
        state
            .doc
            .set_actor(automerge::ActorId::from("alice".as_bytes()));
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        state.commit_action(None, None);

        wp.model_mut().insert_str(&mut state.doc, "first");
        state.commit_action(None, None);

        // Bob forks from Alice's state, edits under his own actor, and syncs
        // back — the same shape `load_incremental` produces on a live peer.
        let mut bob = state.doc.fork();
        bob.set_actor(automerge::ActorId::from("bob".as_bytes()));
        {
            let c = qdoc::model::open(&bob, &state.root).expect("qdoc schema");
            let blocks = qdoc::model::materialize(&bob, &c).blocks;
            let mut ids = qdoc::IdGen::new(bob.get_actor().to_hex_string());
            let mut dm = qdoc::DocMut {
                doc: &mut bob,
                c: &c,
                ids: &mut ids,
            };
            dm.splice_text(&blocks[0].id, 5, 0, " and second");
        }
        qdoc::history::commit_stamped(&mut bob);
        state.doc.merge(&mut bob).expect("peers merge");

        pipeline_draw(&mut wp, &mut state, w, h);
        (wp, state)
    }

    /// The toolbar's History button opens a strip whose ticks are the
    /// document's own versions, attributed to the identities that made them.
    /// No seeded demo data anywhere in this path.
    #[test]
    fn the_history_strip_shows_this_documents_real_versions() {
        let (mut wp, mut state) = editor_with_history(1000, 700);
        assert!(!wp.history.is_open(), "the strip starts closed");

        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(wp.history.is_open(), "the toolbar action opens it");

        let entries = wp.history.entries();
        assert!(
            entries.len() >= 3,
            "schema init plus two authored edits: {:?}",
            entries
                .iter()
                .map(|e| (&e.author.name, &e.summary))
                .collect::<Vec<_>>()
        );
        let names: Vec<&str> = entries.iter().map(|e| e.author.name.as_str()).collect();
        assert!(
            names.contains(&"alice") && names.contains(&"bob"),
            "both identities are named from the change graph, got {names:?}"
        );
        let summaries: Vec<&str> = entries.iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.contains(&"+5 -0 chars"),
            "alice typed five characters: {summaries:?}"
        );
        assert!(
            summaries.contains(&"+11 -0 chars"),
            "bob typed eleven: {summaries:?}"
        );
    }

    /// Opening the strip shrinks the canvas rather than painting over the page.
    #[test]
    fn opening_the_strip_reserves_a_band_out_of_the_canvas() {
        let (mut wp, mut state) = editor_with_history(1000, 700);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let band = wp.history.band();
        assert!(band.height() > 100.0, "the strip has a real band: {band:?}");
        assert!(
            band.y1 <= wp.rect.y1 - LAYOUT_BAR_H,
            "the strip sits above the bottom layout bar"
        );
        assert_eq!(band.width(), wp.rect.width(), "full-width strip");

        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        assert_eq!(
            wp.history.band_height(),
            0.0,
            "closing gives the space back"
        );
    }

    /// THE END-TO-END CLAIM. Click a tick, click Restore, and the document
    /// holds the earlier content — with the restore appended to history as a
    /// new version rather than history being rewritten.
    #[test]
    fn clicking_restore_returns_the_earlier_text_as_a_new_version() {
        let (mut wp, mut state) = editor_with_history(1000, 700);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(body(&state), "first and second", "precondition");

        let before: Vec<_> = wp.history.entries().iter().map(|e| e.id).collect();
        // The tick for the version whose summary is alice's five characters.
        let target = wp
            .history
            .entries()
            .iter()
            .position(|e| e.summary == "+5 -0 chars")
            .expect("alice's edit is on the track");

        // Click that tick on the track, then click Restore — both through the
        // production `handle_event`.
        let y = wp.history.track_center_y();
        let tick_pt = Point::new(wp.history.tick_x(target), y);
        click(&mut wp, &mut state, tick_pt);
        assert_eq!(
            wp.history.current_tick(),
            target,
            "the click moved the handle"
        );

        let rb = wp.history.restore_button_rect().center();
        click(&mut wp, &mut state, rb);

        assert!(wp.needs_commit(), "the restore asks the host to commit it");
        // This is what `App::commit_action` does on the next frame.
        state.commit_action(None, None);
        wp.on_commit();

        assert_eq!(body(&state), "first", "the earlier content is back");

        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let after: Vec<_> = wp.history.entries().iter().map(|e| e.id).collect();
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the restore is a new version, not a rewind"
        );
        for id in &before {
            assert!(
                after.contains(id),
                "every earlier version is still on the track: history was rewritten"
            );
        }
    }

    /// Preview reads the past and leaves the present alone.
    #[test]
    fn clicking_preview_shows_the_old_text_without_editing() {
        let (mut wp, mut state) = editor_with_history(1000, 700);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let target = wp
            .history
            .entries()
            .iter()
            .position(|e| e.summary == "+5 -0 chars")
            .expect("alice's edit is on the track");
        let y = wp.history.track_center_y();
        let tick_pt = Point::new(wp.history.tick_x(target), y);
        click(&mut wp, &mut state, tick_pt);
        let preview_pt = wp.history.preview_button_rect().center();
        click(&mut wp, &mut state, preview_pt);

        assert_eq!(wp.history.preview_text(), Some("first"));
        assert_eq!(body(&state), "first and second", "previewing edits nothing");
        assert!(!wp.needs_commit(), "previewing is not an edit");
    }

    /// A click on the strip must not fall through to the page and move the
    /// caret.
    #[test]
    fn the_strip_captures_clicks_instead_of_seating_the_caret() {
        let (mut wp, mut state) = editor_with_history(1000, 700);
        wp.apply_tool(&mut state.doc, ToolAction::ToggleHistory);
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let caret_before = wp.model().caret().clone();
        let y = wp.history.track_center_y();
        let tick_pt = Point::new(wp.history.tick_x(0), y);
        click(&mut wp, &mut state, tick_pt);
        assert_eq!(
            *wp.model().caret(),
            caret_before,
            "a click on the strip does not reseat the caret in the page"
        );
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
        || path.starts_with(std::env::temp_dir())
}
