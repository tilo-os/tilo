//! `WordProcessorWidget` — the `ui::Widget` for the word processor. Renders the
//! `qdoc` document as a centered page (à la LibreOffice Writer) using the
//! `RichTextLayout` primitive: word-wrapped paragraphs, headings, list bullets,
//! inline **bold**/*italic*/underline, a selection highlight, and a caret. Maps
//! pointer + keyboard onto the editing model (selection, formatting, save).

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use qdoc::document::MarkSpan;
use qdoc::snapshot::{AttrVal, BlockView, NoteView};
use ui::chrome::{self, LabelCue, ToolbarItem};
use ui::gesture::{DragSelect, Granularity};
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::PresenceCtx;

use crate::word_processor::{Caret, ClipBlock, WordProcessor};
use crate::WpFormat;

// Document surface colors (a white page is the familiar word-processor look,
// independent of the OS chrome theme).
const CANVAS: Color = Color::from_rgb8(0x2b, 0x2e, 0x34);
const PAPER: Color = Color::from_rgb8(0xfc, 0xfc, 0xfa);
const SHADOW: Color = Color::from_rgb8(0x16, 0x18, 0x1c);
const INK: Color = Color::from_rgb8(0x1c, 0x1c, 0x20);
const MUTED: Color = Color::from_rgb8(0x8a, 0x8a, 0x92);
const CARET_COLOR: Color = Color::from_rgb8(0x28, 0x5a, 0xc8);
const SELECTION: Color = Color::from_rgba8(0x46, 0x78, 0xe6, 0x4d);

const PAGE_TOP: f64 = 28.0;
const MAX_PAGE_W: f64 = 820.0;
/// Vertical gap between consecutive discrete pages (the canvas shows through it),
/// à la Google Docs. Together with `LETTER_RATIO` this drives true pagination.
const PAGE_GAP: f64 = 22.0;
const CARET_WIDTH: f64 = 2.0;
const TOOLBAR_H: f64 = ui::chrome::TOOLBAR_HEIGHT;
const STATUS_H: f64 = ui::chrome::STATUS_BAR_HEIGHT;
const LAYOUT_BAR_H: f64 = 32.0;
/// Width of the contextual styles panel reserved on the right of the canvas.
const STYLES_PANEL_W: f64 = 250.0;
const FIND_BAR_H: f64 = 42.0;
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
/// Panel chrome colors (consistent with the bottom layout bar).
const PANEL_BG: Color = Color::from_rgb8(0x20, 0x22, 0x28);
const PANEL_EDGE: Color = Color::from_rgb8(0x32, 0x35, 0x3e);
const PANEL_CHIP: Color = Color::from_rgb8(0x2c, 0x2f, 0x38);
const PANEL_FG: Color = Color::from_rgb8(0xc6, 0xca, 0xd2);
const PANEL_MUTED: Color = Color::from_rgb8(0x80, 0x84, 0x90);
const PANEL_FIELD: Color = Color::from_rgb8(0x16, 0x18, 0x1c);
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
/// Accent for active layout guides + handles.
const GUIDE: Color = Color::from_rgb8(0x46, 0x78, 0xe6);
/// Inactive guide line (semi-transparent accent).
const GUIDE_FAINT: Color = Color::from_rgba8(0x46, 0x78, 0xe6, 0x70);
/// How close (px) the pointer must be to a guide line to grab it.
const GUIDE_GRAB: f64 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolAction {
    Bold,
    Italic,
    Underline,
    Strike,
    H1,
    H2,
    Body,
    Bullet,
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignJustify,
    FontCycle,
    /// Apply a per-run text colour (the toolbar swatch row). `None` is the
    /// neutral/default — it clears any `color` run mark over the selection.
    TextColor(Option<[u8; 3]>),
    SizeUp,
    SizeDown,
    /// The size stepper's read-only value chip (between − and +). Display-only.
    SizeShow,
    /// Insert an image at the caret. Under the compositor an OS file-picker
    /// portal supplies the path; standalone there is no portal, so the button is
    /// a best-effort no-op (drag-and-drop is the primary insert path).
    InsertImage,
    InsertTable,
    TableRowAdd,
    TableRowDel,
    TableColAdd,
    TableColDel,
    TableDelete,
    ToggleStyles,
}

/// A clickable control in the Find & Replace bar.
#[derive(Clone, Copy, PartialEq, Debug)]
enum FindAction {
    FocusFind,
    FocusReplace,
    Prev,
    Next,
    Replace,
    ReplaceAll,
    Close,
    /// Toggle case-sensitive matching (the **Aa** chip).
    ToggleCase,
    /// Toggle whole-word matching (the **W** chip).
    ToggleWord,
    /// Toggle regular-expression matching (the **.*** chip).
    ToggleRegex,
}

/// A clickable control in the status bar (currently the zoom stepper).
#[derive(Clone, Copy, PartialEq, Debug)]
enum StatusAction {
    ZoomOut,
    ZoomIn,
    ZoomReset,
}

/// Zoom bounds + step for the page/content scale.
const ZOOM_MIN: f64 = 0.5;
const ZOOM_MAX: f64 = 3.0;
const ZOOM_STEP: f64 = 0.1;

/// Draw a text-input box (Find/Replace fields). A trailing `_` marks the focused
/// field's caret; an empty field shows the placeholder.
fn draw_field_box(scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, r: Rect, text: &str, placeholder: &str, focused: bool) {
    let border = if focused { GUIDE } else { Color::from_rgb8(0x44, 0x47, 0x52) };
    scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x16, 0x18, 0x1e), None, &vello::kurbo::RoundedRect::from_rect(r, 5.0));
    scene.stroke(&vello::kurbo::Stroke::new(if focused { 1.5 } else { 1.0 }), transform, border, None, &vello::kurbo::RoundedRect::from_rect(r, 5.0));
    let (shown, col) = if text.is_empty() && !focused {
        (placeholder.to_string(), MUTED)
    } else if focused {
        (format!("{text}_"), Color::from_rgb8(0xee, 0xef, 0xf2))
    } else {
        (text.to_string(), Color::from_rgb8(0xee, 0xef, 0xf2))
    };
    tr.draw_text(scene, &shown, 13.0, col, transform * Affine::translate((r.x0 + 8.0, (r.y0 + r.y1) / 2.0 - 8.0)), None);
}

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
        Self { top: v, right: v, bottom: v, left: v }
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
    fn label(self) -> &'static str {
        match self {
            NumField::Size => "Size",
            NumField::SpaceBefore => "Space before",
            NumField::SpaceAfter => "Space after",
            NumField::Indent => "Indent",
        }
    }
}

/// A clickable target inside the styles panel, resolved on press.
#[derive(Clone, Debug)]
enum PanelHit {
    /// Apply this style id to the selection / caret block.
    Pick(String),
    CycleFont,
    ToggleBold,
    ToggleItalic,
    SetAlign(&'static str),
    SetColor([u8; 3]),
    /// The value box of a numeric field — click to type, drag to scrub.
    Num(NumField),
    /// A +/- stepper on a numeric field.
    NumDelta(NumField, f32),
    /// Duplicate the current style under a fresh id and apply it.
    NewStyle,
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
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn char_to_byte(text: &str, char_off: usize) -> usize {
    text.char_indices().nth(char_off).map(|(b, _)| b).unwrap_or(text.len())
}
fn byte_to_char(text: &str, byte_off: usize) -> usize {
    let b = byte_off.min(text.len());
    text[..b].chars().count()
}

/// Whether the char range `[s, e)` in `chars` is bounded by word boundaries on
/// both sides — i.e. the char immediately before `s` and at `e` (if present) are
/// not word characters. Used by the Find bar's whole-word toggle.
fn is_word_match(chars: &[char], s: usize, e: usize) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let left_ok = s == 0 || !is_word(chars[s - 1]);
    let right_ok = e >= chars.len() || !is_word(chars[e]);
    left_ok && right_ok
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
    for i in 0..widths.len() {
        let sep = if group_starts[i] { TOOLBAR_SEP } else { 0.0 };
        let item_w = sep + widths[i] + TOOLBAR_GAP;
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

/// `(font size, whole-block bold, left indent, optional bullet)`. Size is the
/// style default; per-run `size` marks override it in `block_runs`.
fn char_has(i: usize, marks: &[MarkSpan], name: &str) -> bool {
    marks.iter().any(|m| m.name == name && i >= m.start && i < m.end)
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
        .map(|n| n as f32)
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
    if marks.iter().any(|m| m.name == "link" && i >= m.start && i < m.end) {
        return Some(LINK_RGB);
    }
    marks
        .iter()
        .find(|m| m.name == "color" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .and_then(parse_hex_rgb)
}

fn parse_hex_rgb(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ])
}

/// Whether a block forces a page break before it (the `page_break_before`
/// block attr). Honoured by the pagination cursor so the block starts a fresh
/// page even when it would otherwise fit on the current one.
fn block_page_break_before(b: &BlockView) -> bool {
    matches!(b.attrs.get("page_break_before"), Some(AttrVal::Bool(true)))
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

/// Split a block's text into styled runs (bold/italic/size/family) + underline
/// and strikethrough byte ranges.
fn block_runs(b: &BlockView) -> (Vec<RichSpan>, Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let chars: Vec<char> = b.text.chars().collect();
    let n = chars.len();
    // Run key: f32 size as bits so the tuple is comparable.
    let key = |i: usize| {
        (
            char_has(i, &b.marks, "strong"),
            char_has(i, &b.marks, "em"),
            char_font_size(i, &b.marks).map(f32::to_bits),
            char_font_family(i, &b.marks),
            char_color(i, &b.marks),
        )
    };
    let mut runs = Vec::new();
    let mut i = 0;
    while i < n {
        let k = key(i);
        let mut j = i;
        while j < n && key(j) == k {
            j += 1;
        }
        let (bold, italic, size_bits, family, color) = k;
        runs.push(RichSpan {
            text: chars[i..j].iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let deco = |name: &str| {
        b.marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| (char_to_byte(&b.text, m.start), char_to_byte(&b.text, m.end)))
            .collect::<Vec<_>>()
    };
    // Links render underlined as well as blue.
    let mut underlines = deco("underline");
    underlines.extend(deco("link"));
    (runs, underlines, deco("strike"))
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
            chars[i..j].iter().collect()
        };
        runs.push(RichSpan {
            text,
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
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

/// Assign citation numbers (1, 2, 3, …) to notes in *document order* — i.e. the
/// order their `note-ref` markers first appear across the body blocks. The
/// snapshot's `notes` are sorted by id, so this is the authoritative ordering for
/// rendering both the in-text superscript and the bottom-of-page list. Returns a
/// map `note_id → number`.
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
    let mut parts: Vec<&str> = note.blocks.iter().map(|b| b.text.as_str()).filter(|t| !t.is_empty()).collect();
    if parts.is_empty() {
        parts.push("");
    }
    parts.join(" ")
}

/// Draw a wavy red spell-check underline from `x0` to `x1` at baseline `y`, in the
/// given (already zoom-adjusted) transform. Approximated by short line segments
/// tracing a sine so it reads as a squiggle without per-glyph curve math.
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
    scene.stroke(&vello::kurbo::Stroke::new(1.0), transform, SQUIGGLE, None, &path);
}

/// Run the spell-checker over a block's text, returning the misspelled byte spans
/// `(start, end)`. Skips the U+FFFC field/note placeholders (they are not words).
fn spell_spans(dict: &spellcheck::Dictionary, text: &str) -> Vec<(usize, usize)> {
    spellcheck::check(dict, text).into_iter().map(|m| (m.start, m.end)).collect()
}

/// Hash everything that feeds into `RichTextLayout::shape` into a single `u64`.
///
/// This is the cache key for the per-block incremental layout cache: if two
/// frames produce the same key for a block, `shape` would produce a
/// byte-identical layout, so the previously-shaped `RichTextLayout` can be
/// reused verbatim. The inputs mirror `shape`'s arguments exactly — `spans`
/// (text + per-run bold/italic/size/family/colour), the underline/strike byte
/// ranges, the block-level bold/size/family/alignment, and the **wrap width**
/// (so a window or margin resize re-shapes). Block-level ink is intentionally
/// excluded: it is applied at draw time (`Laid::ink`), not baked into the
/// buffer, so it can change without invalidating the shaped layout.
fn shape_key(
    spans: &[RichSpan],
    underlines: &[(usize, usize)],
    strikes: &[(usize, usize)],
    base_bold: bool,
    size: f32,
    wrap_w: f32,
    align: TextAlign,
    family: FontFamily,
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
    }
    underlines.hash(&mut h);
    strikes.hash(&mut h);
    base_bold.hash(&mut h);
    size.to_bits().hash(&mut h);
    wrap_w.to_bits().hash(&mut h);
    (align as u8).hash(&mut h);
    (family as u8).hash(&mut h);
    h.finish()
}

/// A block laid out for the current frame; kept for click hit-testing. The
/// shaped `RichTextLayout` itself lives in `WordProcessorWidget::layout_cache`
/// (keyed by `id`) so it can be reused across frames; look it up by `id`.
struct Laid {
    id: String,
    text: String,
    kind: String,
    size: f32,
    y_top: f64,
    text_x: f64,
    width: f64,
    height: f64,
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

/// A list item's 0-based nesting level (absent attr → 0).
fn list_level(b: &BlockView) -> usize {
    match b.attrs.get("level") {
        Some(AttrVal::Int(n)) if *n > 0 => *n as usize,
        _ => 0,
    }
}

/// Per-nesting-level extra indent (px) added on top of the list style's indent.
const LIST_INDENT_STEP: f64 = 24.0;
/// Horizontal offset from the text's left edge to the bullet / number glyph.
const BULLET_GAP: f64 = 20.0;
/// US-Letter page height in inches (width is 8.5in → `MAX_PAGE_W` px).
const LETTER_RATIO: f64 = 11.0 / 8.5;

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
        Some(AttrVal::Int(n)) if *n >= 0 => *n as usize,
        _ => 0,
    };
    (get("row"), get("col"))
}

pub struct WordProcessorWidget {
    inner: WordProcessor,
    rect: Rect,
    focused: bool,
    dirty: bool,
    needs_commit: bool,
    pointer: Point,
    drag: DragSelect,
    path: Option<PathBuf>,
    format: Option<WpFormat>,
    /// Unsaved changes since the last save (drives the status-bar `●` indicator).
    modified: bool,
    /// Find & Replace bar state.
    find_open: bool,
    find_query: String,
    replace_query: String,
    /// Which find-bar field has keyboard focus (`false` = find, `true` = replace).
    find_on_replace: bool,
    /// All current matches as `(block_id, char_start, char_end)`.
    find_matches: Vec<(String, usize, usize)>,
    /// Index of the active match within `find_matches`.
    find_idx: usize,
    /// Clickable find-bar controls: `(action, rect)`.
    find_buttons: Vec<(FindAction, Rect)>,
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
    /// Incremental per-block shaped-text cache: block id → (shape key, shaped
    /// layout). Reused across frames when the key (text + marks + resolved
    /// style + wrap width) is unchanged, so only edited/resized blocks pay the
    /// cosmic-text shaping cost instead of every block every frame. Pruned to
    /// the live block set each frame (entries for deleted blocks are dropped).
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
    /// Page+content zoom factor (1.0 = 100%). Applied to the page transform and
    /// accounted for in hit-testing + scroll. Lives in widget state (a view
    /// preference), not the document. Clamped to `[ZOOM_MIN, ZOOM_MAX]`.
    zoom: f64,
    /// The screen-space anchor used for the content zoom transform this frame
    /// (top-center of the canvas). Cached so the event handler can map pointer
    /// positions from screen space back into unscaled content space.
    zoom_anchor: Point,
    /// Find-bar match modifiers. Default-off reproduces the original
    /// case-insensitive substring behaviour.
    find_case: bool,
    find_word: bool,
    find_regex: bool,
    /// Set when the current query is `find_regex` but does not compile — drives a
    /// subtle "bad pattern" cue and yields zero matches (never panics).
    find_regex_error: bool,
    /// Clickable status-bar controls (the zoom stepper): `(action, rect)`.
    status_buttons: Vec<(StatusAction, Rect)>,
    /// Number of toolbar rows the controls wrapped into this frame (>=1). Drives
    /// the canvas top offset so no control is ever clipped.
    toolbar_rows: usize,
    /// Footnotes / endnotes (materialized from the doc), sorted by note id; the
    /// in-text numbering is derived from `note-ref` document order, not this order.
    notes: Vec<NoteView>,
    /// The built-in English dictionary, loaded lazily on first spell-check pass
    /// (it is ~75k words, so building it eagerly would slow every cold start). A
    /// process-wide shared copy avoids re-parsing the word list per widget.
    dict: Option<std::sync::Arc<spellcheck::Dictionary>>,
    /// Whether spell-checking (the red squiggles) is enabled. Default ON.
    spellcheck_on: bool,
    /// Per-block spell-check cache: block id → (text, misspelled byte spans). Keyed
    /// by the block's text so an edit (which changes the text) invalidates the
    /// entry and re-checks; an unchanged block reuses the spans for free.
    spell_cache: HashMap<String, (String, Vec<(usize, usize)>)>,
}

/// One process-wide built-in dictionary, shared across every word-processor
/// widget so the ~75k-word list is parsed at most once per process.
fn shared_dictionary() -> std::sync::Arc<spellcheck::Dictionary> {
    use std::sync::OnceLock;
    static DICT: OnceLock<std::sync::Arc<spellcheck::Dictionary>> = OnceLock::new();
    DICT.get_or_init(|| std::sync::Arc::new(spellcheck::Dictionary::builtin())).clone()
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
            pointer: Point::ZERO,
            drag: DragSelect::new(),
            path: None,
            format: None,
            modified: false,
            find_open: false,
            find_query: String::new(),
            replace_query: String::new(),
            find_on_replace: false,
            find_matches: Vec::new(),
            find_idx: 0,
            find_buttons: Vec::new(),
            blobs: std::collections::BTreeMap::new(),
            image_cache: std::collections::HashMap::new(),
            hover_tool: None,
            blocks: Vec::new(),
            header: Vec::new(),
            footer: Vec::new(),
            laid: Vec::new(),
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
            page_rect: Rect::ZERO,
            layout_chips: Vec::new(),
            // Default the panel open: it is the primary surface for the
            // component-instance style model. Dismissable via the toolbar toggle.
            styles_panel_open: true,
            panel_rect: Rect::ZERO,
            panel_hits: Vec::new(),
            editing_field: None,
            edit_buf: String::new(),
            scrub: None,
            zoom: 1.0,
            zoom_anchor: Point::ZERO,
            find_case: false,
            find_word: false,
            find_regex: false,
            find_regex_error: false,
            status_buttons: Vec::new(),
            toolbar_rows: 1,
            notes: Vec::new(),
            dict: None,
            spellcheck_on: true,
            spell_cache: HashMap::new(),
        }
    }

    /// Lazily build (once) and return the shared spell-check dictionary.
    fn dictionary(&mut self) -> std::sync::Arc<spellcheck::Dictionary> {
        self.dict.get_or_insert_with(shared_dictionary).clone()
    }

    /// Misspelled byte spans for a body block, using `spell_cache` (keyed by the
    /// block's text so an edit re-checks and an unchanged block is free).
    fn block_spell_spans(&mut self, id: &str, text: &str) -> Vec<(usize, usize)> {
        if let Some((cached_text, spans)) = self.spell_cache.get(id) {
            if cached_text == text {
                return spans.clone();
            }
        }
        let dict = self.dictionary();
        let spans = spell_spans(&dict, text);
        self.spell_cache.insert(id.to_string(), (text.to_string(), spans.clone()));
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
        self.spellcheck_on
    }

    pub fn model(&self) -> &WordProcessor {
        &self.inner
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
                true
            }
            Err(e) => {
                eprintln!("word-processor: could not read image {}: {e}", path.display());
                false
            }
        }
    }

    /// Toolbar "Insert image" affordance. Under the compositor an OS file-picker
    /// portal would supply a path here; standalone there is no portal, so this is
    /// a best-effort no-op (it logs a hint). Drag-and-drop a file onto the window
    /// is the primary, portal-free insert path.
    fn request_image_insert(&mut self, _state: &mut AutoCommit) {
        // No file-picker portal is wired in the standalone binary. Rather than
        // block on one, point the user at the working path.
        eprintln!(
            "word-processor: Insert image — no file-picker portal available; \
             drag and drop an image file onto the window instead."
        );
    }

    fn save(&mut self, state: &AutoCommit) {
        // A brand-new (untitled) document has no path yet — fall back to
        // `~/Untitled.md` so Ctrl+S always saves *something* and the filename
        // then shows in the status bar (a proper Save-As dialog is a follow-up).
        if self.path.is_none() {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            self.path = Some(std::path::PathBuf::from(home).join("Untitled.md"));
            self.format = Some(WpFormat::Markdown);
        }
        if let (Some(path), Some(format), Some(c)) =
            (self.path.as_ref(), self.format, self.inner.containers.as_ref())
        {
            let dr = qdoc::DocRef { doc: state, c };
            if let Ok(bytes) = format.export(dr) {
                if std::fs::write(path, bytes).is_ok() {
                    self.modified = false;
                }
            }
        }
    }

    // ---- Find & Replace -----------------------------------------------------

    fn open_find(&mut self, state: &AutoCommit) {
        self.find_open = true;
        self.find_on_replace = false;
        // Seed the query from a single-line selection, if any.
        if let Some((bid, s, e)) = self.selection_blocks_cached().first().cloned() {
            if let Some(b) = self.blocks.iter().find(|b| b.id == bid) {
                let sel: String = b.text.chars().skip(s).take(e.saturating_sub(s)).collect();
                if !sel.is_empty() && !sel.contains('\n') {
                    self.find_query = sel;
                }
            }
        }
        self.find_recompute(state);
    }

    fn close_find(&mut self) {
        self.find_open = false;
        self.find_matches.clear();
    }

    // ---- Zoom ---------------------------------------------------------------

    /// Set the zoom factor (clamped) and request a repaint. Returns whether it
    /// changed.
    fn set_zoom(&mut self, z: f64) -> bool {
        let z = z.clamp(ZOOM_MIN, ZOOM_MAX);
        if (z - self.zoom).abs() > 1e-6 {
            self.zoom = z;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    fn zoom_by(&mut self, delta: f64) -> bool {
        // Round to the nearest step so the displayed percentage stays tidy.
        let raw = self.zoom + delta;
        let snapped = (raw / ZOOM_STEP).round() * ZOOM_STEP;
        self.set_zoom(snapped)
    }

    /// The content (page) transform for this frame: a scale about the cached
    /// top-center anchor. Everything inside the page clip draws through this so
    /// the toolbar / find-bar / status bar stay unscaled.
    fn content_transform(&self, base: Affine) -> Affine {
        base * Affine::scale_about(self.zoom, self.zoom_anchor)
    }

    /// Map a screen-space pointer into the unscaled content coordinate space the
    /// `Laid` rects live in (the inverse of `content_transform`'s scale-about).
    fn pointer_to_content(&self, p: Point) -> Point {
        let a = self.zoom_anchor;
        Point::new(a.x + (p.x - a.x) / self.zoom, a.y + (p.y - a.y) / self.zoom)
    }

    /// Recompute all matches for the current query, honouring the case-sensitive,
    /// whole-word, and regex toggles. Default (all off) = the original
    /// case-insensitive substring scan. An invalid regex sets `find_regex_error`
    /// and yields no matches (never panics).
    fn find_recompute(&mut self, state: &AutoCommit) {
        self.find_matches.clear();
        self.find_regex_error = false;
        if self.find_query.is_empty() {
            self.find_idx = 0;
            self.select_current_match(state);
            return;
        }
        if self.find_regex {
            self.find_recompute_regex();
        } else {
            self.find_recompute_literal();
        }
        if self.find_idx >= self.find_matches.len() {
            self.find_idx = 0;
        }
        self.select_current_match(state);
    }

    /// Literal substring scan over chars, honouring `find_case` and `find_word`.
    fn find_recompute_literal(&mut self) {
        // Build the comparison key once: lowercased when case-insensitive.
        let q: Vec<char> = if self.find_case {
            self.find_query.chars().collect()
        } else {
            self.find_query.to_lowercase().chars().collect()
        };
        if q.is_empty() {
            return;
        }
        for b in &self.blocks {
            // The haystack we *compare* against (possibly lowercased) and the raw
            // chars we test word-boundaries against share indices 1:1.
            let raw: Vec<char> = b.text.chars().collect();
            let hay: Vec<char> = if self.find_case {
                raw.clone()
            } else {
                b.text.to_lowercase().chars().collect()
            };
            if q.len() > hay.len() {
                continue;
            }
            let mut i = 0;
            while i + q.len() <= hay.len() {
                if hay[i..i + q.len()] == q[..] && (!self.find_word || is_word_match(&raw, i, i + q.len())) {
                    self.find_matches.push((b.id.clone(), i, i + q.len()));
                    i += q.len();
                } else {
                    i += 1;
                }
            }
        }
    }

    /// Regex scan. Compiles the query once (case-insensitive flag from
    /// `find_case`, whole-word via `\b…\b`). On a compile error, records the error
    /// and produces no matches.
    fn find_recompute_regex(&mut self) {
        let mut pattern = self.find_query.clone();
        if self.find_word {
            // Wrap in word boundaries so the whole-word toggle composes with regex.
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let re = match regex::RegexBuilder::new(&pattern).case_insensitive(!self.find_case).build() {
            Ok(re) => re,
            Err(_) => {
                self.find_regex_error = true;
                return;
            }
        };
        for b in &self.blocks {
            for m in re.find_iter(&b.text) {
                // Empty matches (e.g. `a*` on a gap) would loop / select nothing;
                // skip them so navigation stays meaningful.
                if m.start() == m.end() {
                    continue;
                }
                let cs = byte_to_char(&b.text, m.start());
                let ce = byte_to_char(&b.text, m.end());
                self.find_matches.push((b.id.clone(), cs, ce));
            }
        }
    }

    /// Select (so it highlights) + scroll to the active match.
    fn select_current_match(&mut self, _state: &AutoCommit) {
        if let Some((bid, s, e)) = self.find_matches.get(self.find_idx).cloned() {
            self.inner.clear_anchor();
            self.inner.set_caret(Caret { block: bid.clone(), offset: s });
            self.inner.set_anchor();
            self.inner.set_caret(Caret { block: bid.clone(), offset: e });
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

    fn find_jump(&mut self, delta: i32, state: &AutoCommit) {
        if self.find_matches.is_empty() {
            return;
        }
        let n = self.find_matches.len() as i32;
        self.find_idx = (((self.find_idx as i32 + delta) % n + n) % n) as usize;
        self.select_current_match(state);
    }

    fn replace_current(&mut self, state: &mut AutoCommit) {
        if let Some((bid, s, e)) = self.find_matches.get(self.find_idx).cloned() {
            let rep = self.replace_query.clone();
            self.inner.splice_block(state, &bid, s, e, &rep);
            self.refresh_blocks(state);
            self.needs_commit = true;
            self.find_recompute(state);
        }
    }

    fn replace_all(&mut self, state: &mut AutoCommit) {
        let rep = self.replace_query.clone();
        let mut matches = self.find_matches.clone();
        // Replace from the highest char offset down so earlier offsets stay valid.
        matches.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        for (bid, s, e) in matches.into_iter().rev() {
            self.inner.splice_block(state, &bid, s, e, &rep);
        }
        self.refresh_blocks(state);
        self.needs_commit = true;
        self.find_recompute(state);
    }

    fn find_action(&mut self, state: &mut AutoCommit, action: FindAction) {
        match action {
            FindAction::FocusFind => self.find_on_replace = false,
            FindAction::FocusReplace => self.find_on_replace = true,
            FindAction::Prev => self.find_jump(-1, state),
            FindAction::Next => self.find_jump(1, state),
            FindAction::Replace => self.replace_current(state),
            FindAction::ReplaceAll => self.replace_all(state),
            FindAction::Close => self.close_find(),
            FindAction::ToggleCase => {
                self.find_case = !self.find_case;
                self.find_recompute(state);
            }
            FindAction::ToggleWord => {
                self.find_word = !self.find_word;
                self.find_recompute(state);
            }
            FindAction::ToggleRegex => {
                self.find_regex = !self.find_regex;
                self.find_recompute(state);
            }
        }
    }

    fn draw_find_bar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, _theme: &Theme, bar: Rect) {
        self.find_buttons.clear();
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x24, 0x26, 0x2d), None, &bar);
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x33, 0x36, 0x40), None, &Rect::new(bar.x0, bar.y1 - 1.0, bar.x1, bar.y1));
        let cy = (bar.y0 + bar.y1) / 2.0;
        let fy0 = cy - 13.0;
        let mut x = bar.x0 + 12.0;

        let fr = Rect::new(x, fy0, x + 188.0, fy0 + 26.0);
        draw_field_box(scene, transform, tr, fr, &self.find_query, "Find", !self.find_on_replace);
        // A subtle red ring when a regex query fails to compile.
        if self.find_regex && self.find_regex_error {
            scene.stroke(&vello::kurbo::Stroke::new(1.5), transform, Color::from_rgb8(0xd0, 0x52, 0x4a), None, &vello::kurbo::RoundedRect::from_rect(fr, 5.0));
        }
        self.find_buttons.push((FindAction::FocusFind, fr));
        x += 196.0;

        // Match-mode toggles: Aa (case), W (whole word), .* (regex).
        for (action, label, w, on) in [
            (FindAction::ToggleCase, "Aa", 30.0, self.find_case),
            (FindAction::ToggleWord, "W", 26.0, self.find_word),
            (FindAction::ToggleRegex, ".*", 30.0, self.find_regex),
        ] {
            let r = Rect::new(x, fy0, x + w, fy0 + 26.0);
            let (bg, fg) = if on {
                (GUIDE, Color::WHITE)
            } else {
                (Color::from_rgb8(0x2a, 0x2d, 0x36), Color::from_rgb8(0xb6, 0xba, 0xc4))
            };
            scene.fill(Fill::NonZero, transform, bg, None, &vello::kurbo::RoundedRect::from_rect(r, 5.0));
            let tx = r.x0 + (w - label.chars().count() as f64 * 7.2).max(4.0) / 2.0;
            tr.draw_text(scene, label, 12.5, fg, transform * Affine::translate((tx, cy - 8.0)), None);
            self.find_buttons.push((action, r));
            x += w + 5.0;
        }
        x += 8.0;

        let rr = Rect::new(x, fy0, x + 188.0, fy0 + 26.0);
        draw_field_box(scene, transform, tr, rr, &self.replace_query, "Replace with", self.find_on_replace);
        self.find_buttons.push((FindAction::FocusReplace, rr));
        x += 196.0;

        let (count, count_col) = if self.find_query.is_empty() {
            (String::new(), MUTED)
        } else if self.find_regex && self.find_regex_error {
            ("Bad pattern".to_string(), Color::from_rgb8(0xd0, 0x6a, 0x62))
        } else if self.find_matches.is_empty() {
            ("No results".to_string(), MUTED)
        } else {
            (format!("{} / {}", self.find_idx + 1, self.find_matches.len()), MUTED)
        };
        tr.draw_text(scene, &count, 12.5, count_col, transform * Affine::translate((x, cy - 7.0)), None);
        x += 78.0;

        for (action, label, w) in [
            (FindAction::Prev, "↑", 28.0),
            (FindAction::Next, "↓", 28.0),
            (FindAction::Replace, "Replace", 72.0),
            (FindAction::ReplaceAll, "All", 40.0),
            (FindAction::Close, "✕", 28.0),
        ] {
            let r = Rect::new(x, fy0, x + w, fy0 + 26.0);
            scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x2e, 0x31, 0x3a), None, &vello::kurbo::RoundedRect::from_rect(r, 5.0));
            let tx = r.x0 + (w - label.chars().count() as f64 * 7.2).max(6.0) / 2.0;
            tr.draw_text(scene, label, 13.0, Color::from_rgb8(0xd2, 0xd5, 0xdc), transform * Affine::translate((tx, cy - 8.0)), None);
            self.find_buttons.push((action, r));
            x += w + 6.0;
        }
    }

    /// Handle a keypress while the Find bar is open. Returns whether it was
    /// consumed (so the document editor doesn't also see it).
    fn find_key(&mut self, state: &mut AutoCommit, key: &Key, typed: Option<&str>, shift: bool) -> bool {
        match key {
            Key::Named(NamedKey::Escape) => self.close_find(),
            Key::Named(NamedKey::Enter) => self.find_jump(if shift { -1 } else { 1 }, state),
            Key::Named(NamedKey::Tab) => self.find_on_replace = !self.find_on_replace,
            Key::Named(NamedKey::Backspace) => {
                let buf = if self.find_on_replace { &mut self.replace_query } else { &mut self.find_query };
                buf.pop();
                if !self.find_on_replace {
                    self.find_recompute(state);
                }
            }
            _ => {
                if let Some(t) = typed {
                    if !t.is_empty() && !t.chars().any(|c| c.is_control()) {
                        if self.find_on_replace {
                            self.replace_query.push_str(t);
                        } else {
                            self.find_query.push_str(t);
                            self.find_recompute(state);
                        }
                    }
                }
                // Consume all other keys while the bar is open (don't leak arrow
                // keys etc. to the document editor).
            }
        }
        true
    }

    fn refresh_blocks(&mut self, state: &AutoCommit) {
        if let Some(c) = self.inner.containers.as_ref() {
            let snap = qdoc::model::materialize(state, c);
            self.blocks = snap.blocks;
            self.blobs = snap.blobs;
            self.header = snap.header;
            self.footer = snap.footer;
            self.notes = snap.notes;
            // Drop spell-cache entries for blocks that no longer exist so the cache
            // stays bounded; surviving blocks keep their (text-keyed) spans.
            let live: std::collections::HashSet<&str> = self.blocks.iter().map(|b| b.id.as_str()).collect();
            self.spell_cache.retain(|id, _| live.contains(id.as_str()));
        }
    }

    /// Resolve a pointer position to a caret and apply it (optionally extending
    /// the selection for click-drag).
    fn caret_from_pointer(&mut self, p: Point, extend: bool) {
        // Find the laid block under the pointer: cells hit by their full box (so
        // the right column is chosen), normal blocks by their vertical band.
        let mut hit: Option<(String, usize)> = None;
        for l in &self.laid {
            let inside = match l.cell_box {
                Some(bx) => bx.contains(p),
                None => p.y >= l.y_top && p.y < l.y_top + l.height,
            };
            if inside {
                let byte = self
                    .layout_cache
                    .get(&l.id)
                    .map(|(_, lay)| lay.hit((p.x - l.text_x) as f32, (p.y - l.y_top) as f32))
                    .unwrap_or(0);
                hit = Some((l.id.clone(), byte_to_char(&l.text, byte)));
                break;
            }
        }
        if hit.is_none() {
            // above first / below last
            if let Some(l) = self.laid.first() {
                if p.y < l.y_top {
                    hit = Some((l.id.clone(), 0));
                }
            }
            if hit.is_none() {
                if let Some(l) = self.laid.last() {
                    let byte = self
                        .layout_cache
                        .get(&l.id)
                        .map(|(_, lay)| lay.hit((p.x - l.text_x) as f32, (l.height - 1.0) as f32))
                        .unwrap_or(0);
                    hit = Some((l.id.clone(), byte_to_char(&l.text, byte)));
                }
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

impl WordProcessorWidget {
    /// Toggle a layout-edit mode from a chip click.
    fn toggle_layout_mode(&mut self, mode: LayoutMode) {
        self.layout_mode = if self.layout_mode == mode { LayoutMode::None } else { mode };
    }

    /// Persist the current page margins into the document (page setup lives in the
    /// document so it survives save/reopen). Called on margin-guide drag release.
    fn persist_margins(&mut self, state: &mut AutoCommit) {
        self.inner.set_page_margins(state, [self.margins.top, self.margins.right, self.margins.bottom, self.margins.left]);
    }

    /// Draw the page header (or footer) blocks for one page sheet inside the
    /// given margin band. `blocks` is the materialized header/footer; `page_no`
    /// is this sheet's 1-based page number (substituted for page-number fields).
    /// `is_footer` anchors the stack to the band's bottom (footers) vs. its top
    /// (headers). Drawn through `ctf` so it magnifies with the page content.
    ///
    /// Returns whether anything was drawn (so the caller can fall back to the
    /// default page indicator only when no footer exists).
    #[allow(clippy::too_many_arguments)]
    fn draw_running_band(
        &self,
        scene: &mut Scene,
        ctf: Affine,
        tr: &mut TextRenderer,
        blocks: &[BlockView],
        page_no: usize,
        band: Rect,
        is_footer: bool,
    ) -> bool {
        if blocks.is_empty() || band.height() <= 2.0 {
            return false;
        }
        let wrap_w = band.width().max(40.0) as f32;
        // Header/footer run text is a touch smaller and muted, so it reads as
        // chrome rather than body copy (LibreOffice-style running heads).
        let size = 12.0f32;
        // Shape every block first so footers can be bottom-anchored.
        let mut shaped: Vec<RichTextLayout> = Vec::with_capacity(blocks.len());
        let mut total_h = 0.0f64;
        for b in blocks {
            let runs = running_head_runs(b, page_no);
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
            );
            total_h += layout.height() as f64;
            shaped.push(layout);
        }
        // Stack from the top of the band (headers) or just enough above the band
        // bottom to fit the footer's content (footers).
        let mut y = if is_footer { (band.y1 - total_h).max(band.y0) } else { band.y0 };
        let mut drew = false;
        for layout in &shaped {
            tr.draw_rich(scene, layout, ctf * Affine::translate((band.x0, y)), MUTED);
            y += layout.height() as f64;
            drew = true;
        }
        drew
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
        self.blocks.iter().find(|b| b.id == self.inner.caret.block).map(block_style_key)
    }

    /// The `space_after` of the focused block's style.
    fn focused_space_after(&self) -> f64 {
        self.focused_style_key().map(|k| self.sheet.get(&k).space_after as f64).unwrap_or(8.0)
    }

    /// `(x-center, y)` of the spacing grip — centered in the gap *after the
    /// focused block*, so dragging it edits that block's style (every block of
    /// the same style follows).
    fn spacing_handle(&self) -> Option<(f64, f64)> {
        let l = self.laid.iter().find(|l| l.id == self.inner.caret.block).or_else(|| self.laid.first())?;
        let cx = (self.page_rect.x0 + self.page_rect.x1) / 2.0;
        Some((cx, l.y_top + l.height + self.focused_space_after() / 2.0))
    }

    /// Which guide (if any) is under the pointer, for the active layout mode.
    fn hit_guide(&self, p: Point) -> Option<Side> {
        let page = self.page_rect;
        if let Some(e) = self.active_edges() {
            let mut best: Option<(Side, f64)> = None;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let along = if vert { p.y >= page.y0 && p.y <= page.y1 } else { p.x >= page.x0 && p.x <= page.x1 };
                let d = if vert { (p.x - c).abs() } else { (p.y - c).abs() };
                if along && d <= GUIDE_GRAB && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((side, d));
                }
            }
            return best.map(|(s, _)| s);
        }
        if self.layout_mode == LayoutMode::Spacing {
            if let Some((_, hy)) = self.spacing_handle() {
                if (p.y - hy).abs() <= GUIDE_GRAB + 4.0 && p.x >= page.x0 && p.x <= page.x1 {
                    return Some(Side::Bottom); // side unused for spacing
                }
            }
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
                    let after = (2.0 * (p.y - bottom)).clamp(0.0, 80.0) as f32;
                    if let Some(st) = self.sheet.styles.get_mut(&key) {
                        st.space_after = after;
                    }
                }
            }
            LayoutMode::None => {}
        }
    }

    /// Draggable guides + live value labels for the active layout mode, drawn
    /// over the page content.
    fn draw_layout_overlay(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, page: Rect) {
        if let Some(e) = self.active_edges() {
            let drag = self.guide_drag;
            for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
                let (vert, c) = guide_axis(page, e, side);
                let active = drag
                    .map(|g| match g.kind {
                        DragKind::Proportional => true,
                        DragKind::Pair => g.side == side || g.side.opposite() == side,
                        DragKind::Single => g.side == side,
                    })
                    .unwrap_or(false);
                let col = if active { GUIDE } else { GUIDE_FAINT };
                let t = if active { 2.0 } else { 1.0 };
                let line = if vert {
                    Rect::new(c - t / 2.0, page.y0, c + t / 2.0, page.y1)
                } else {
                    Rect::new(page.x0, c - t / 2.0, page.x1, c + t / 2.0)
                };
                scene.fill(Fill::NonZero, transform, col, None, &line);
                // grip dot at the line's midpoint
                let (gx, gy) = if vert { (c, (page.y0 + page.y1) / 2.0) } else { ((page.x0 + page.x1) / 2.0, c) };
                scene.fill(Fill::NonZero, transform, GUIDE, None, &vello::kurbo::Circle::new((gx, gy), 4.0));
                // value label, offset inside the page
                let label = format!("{}", e.get(side).round() as i64);
                let (lx, ly) = if vert { (c + 6.0, page.y0 + 16.0) } else { (page.x0 + 10.0, c + 5.0) };
                tr.draw_text(scene, &label, 12.0, GUIDE, transform * Affine::translate((lx, ly)), None);
            }
        } else if self.layout_mode == LayoutMode::Spacing {
            if let Some((hx, hy)) = self.spacing_handle() {
                let pill = Rect::new(hx - 20.0, hy - 3.0, hx + 20.0, hy + 3.0);
                scene.fill(Fill::NonZero, transform, GUIDE, None, &vello::kurbo::RoundedRect::from_rect(pill, 3.0));
                let label = format!("{} px", self.focused_space_after().round() as i64);
                tr.draw_text(scene, &label, 12.0, GUIDE, transform * Affine::translate((hx + 30.0, hy - 7.0)), None);
            }
        }
    }

    /// The bottom layout bar: mode chips + a live readout / hint.
    fn draw_layout_bar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, _theme: &Theme, bar: Rect) {
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x20, 0x22, 0x28), None, &bar);
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x32, 0x35, 0x3e), None, &Rect::new(bar.x0, bar.y0, bar.x1, bar.y0 + 1.0));

        let cy = (bar.y0 + bar.y1) / 2.0;
        let mut x = bar.x0 + 12.0;
        self.layout_chips.clear();
        for (mode, label) in [(LayoutMode::Margins, "Margins"), (LayoutMode::Spacing, "Spacing")] {
            let w = 16.0 + label.chars().count() as f64 * 7.6;
            let r = Rect::new(x, bar.y0 + 5.0, x + w, bar.y1 - 5.0);
            let active = self.layout_mode == mode;
            let bg = if active { GUIDE } else { Color::from_rgb8(0x2c, 0x2f, 0x38) };
            let fg = if active { Color::from_rgb8(0xff, 0xff, 0xff) } else { Color::from_rgb8(0xc6, 0xca, 0xd2) };
            scene.fill(Fill::NonZero, transform, bg, None, &vello::kurbo::RoundedRect::from_rect(r, 6.0));
            tr.draw_text(scene, label, 13.0, fg, transform * Affine::translate((x + 8.0, cy - 8.0)), None);
            self.layout_chips.push((mode, r));
            x += w + 8.0;
        }

        let readout = match self.layout_mode {
            LayoutMode::Margins => format!(
                "margins  L {}  T {}  R {}  B {}   ·   drag = opposite pair · Shift = one side · Ctrl = all proportional",
                self.margins.left.round(), self.margins.top.round(), self.margins.right.round(), self.margins.bottom.round()
            ),
            LayoutMode::Spacing => {
                let name = self.focused_style_key().map(|k| self.sheet.get(&k).name).unwrap_or_else(|| "Body".into());
                format!("{} spacing  {} px   ·   drag the handle — every “{}” block follows", name, self.focused_space_after().round(), name)
            }
            LayoutMode::None => "Page layout — pick Margins or Spacing, then drag on the page".to_string(),
        };
        tr.draw_text(scene, &readout, 12.5, Color::from_rgb8(0x9a, 0x9e, 0xa8), transform * Affine::translate((x + 12.0, cy - 8.0)), None);
    }

    /// The contextual styles panel: the focused block's style name, a style
    /// picker (apply any style to the selection), and editable property rows that
    /// mutate the *style* — every block of that style updates live.
    fn draw_styles_panel(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, panel: Rect) {
        self.panel_rect = panel;
        self.panel_hits.clear();
        // Background + left border.
        scene.fill(Fill::NonZero, transform, PANEL_BG, None, &panel);
        scene.fill(Fill::NonZero, transform, PANEL_EDGE, None, &Rect::new(panel.x0, panel.y0, panel.x0 + 1.0, panel.y1));
        // Clip content to the panel so nothing bleeds into the bars.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, transform, &panel);

        let pad = 14.0;
        let x0 = panel.x0 + pad;
        let right = panel.x1 - pad;
        let inner_w = right - x0;
        let mut y = panel.y0 + 14.0;

        tr.draw_text(scene, "STYLE", 10.5, PANEL_MUTED, transform * Affine::translate((x0, y)), None);
        y += 16.0;

        let Some(key) = self.focused_style_key() else {
            tr.draw_text(scene, "Place the caret in a paragraph", 12.5, PANEL_MUTED, transform * Affine::translate((x0, y)), Some(inner_w as f32));
            scene.pop_layer();
            return;
        };
        // Snapshot the focused style for drawing (read-only copy).
        let st = self.sheet.get(&key);

        tr.draw_text(scene, &st.name, 18.0, PANEL_FG, transform * Affine::translate((x0, y)), Some(inner_w as f32));
        y += 28.0;
        panel_divider(scene, transform, x0, right, y);
        y += 12.0;

        // ---- Style picker -------------------------------------------------
        tr.draw_text(scene, "PARAGRAPH STYLES", 10.5, PANEL_MUTED, transform * Affine::translate((x0, y)), None);
        y += 16.0;
        let order = self.sheet.order();
        let gap = 6.0;
        let col_w = (inner_w - gap) / 2.0;
        let chip_h = 24.0;
        for (i, id) in order.iter().enumerate() {
            let col = (i % 2) as f64;
            let row = (i / 2) as f64;
            let cx = x0 + col * (col_w + gap);
            let cy = y + row * (chip_h + gap);
            let r = Rect::new(cx, cy, cx + col_w, cy + chip_h);
            let name = self.sheet.get(id).name;
            draw_btn(scene, transform, tr, r, &name, id == &key, false);
            self.panel_hits.push((r, PanelHit::Pick(id.clone())));
        }
        let rows = order.len().div_ceil(2) as f64;
        y += rows * (chip_h + gap) + 6.0;
        panel_divider(scene, transform, x0, right, y);
        y += 12.0;

        // ---- Editable style properties ------------------------------------
        tr.draw_text(scene, "STYLE PROPERTIES", 10.5, PANEL_MUTED, transform * Affine::translate((x0, y)), None);
        y += 18.0;

        // Font (cycles sans → serif → mono).
        {
            tr.draw_text(scene, "Font", 12.5, PANEL_FG, transform * Affine::translate((x0, y + 5.0)), None);
            let fl = match st.font.as_str() {
                "serif" => "Serif",
                "mono" => "Mono",
                _ => "Sans",
            };
            let r = Rect::new(right - 96.0, y, right, y + 24.0);
            draw_btn(scene, transform, tr, r, fl, false, true);
            self.panel_hits.push((r, PanelHit::CycleFont));
            y += 30.0;
        }

        // Weight: Bold / Italic toggles.
        {
            tr.draw_text(scene, "Weight", 12.5, PANEL_FG, transform * Affine::translate((x0, y + 5.0)), None);
            let bw = 60.0;
            let rb = Rect::new(right - bw * 2.0 - 6.0, y, right - bw - 6.0, y + 24.0);
            let ri = Rect::new(right - bw, y, right, y + 24.0);
            draw_btn(scene, transform, tr, rb, "Bold", st.bold, true);
            draw_btn(scene, transform, tr, ri, "Italic", st.italic, true);
            self.panel_hits.push((rb, PanelHit::ToggleBold));
            self.panel_hits.push((ri, PanelHit::ToggleItalic));
            y += 30.0;
        }

        // Colour: a palette of swatches.
        {
            tr.draw_text(scene, "Colour", 12.5, PANEL_FG, transform * Affine::translate((x0, y + 5.0)), None);
            let n = STYLE_PALETTE.len();
            let sw_gap = 5.0;
            // Leave room on the left for the "Colour" label.
            let strip_w = (inner_w - 54.0).clamp(120.0, 196.0);
            let sw = (strip_w - sw_gap * (n as f64 - 1.0)) / n as f64;
            let sx0 = right - strip_w;
            for (i, c) in STYLE_PALETTE.iter().enumerate() {
                let sx = sx0 + i as f64 * (sw + sw_gap);
                let r = Rect::new(sx, y, sx + sw, y + 22.0);
                let col = Color::from_rgb8(c[0], c[1], c[2]);
                scene.fill(Fill::NonZero, transform, col, None, &vello::kurbo::RoundedRect::from_rect(r, 4.0));
                if *c == st.color {
                    scene.stroke(&vello::kurbo::Stroke::new(2.0), transform, Color::WHITE, None, &vello::kurbo::RoundedRect::from_rect(r, 4.0));
                }
                self.panel_hits.push((r, PanelHit::SetColor(*c)));
            }
            y += 30.0;
        }

        // Alignment: L / C / R.
        {
            tr.draw_text(scene, "Align", 12.5, PANEL_FG, transform * Affine::translate((x0, y + 5.0)), None);
            let bw = 40.0;
            let gap = 6.0;
            for (i, (label, val)) in [("L", "left"), ("C", "center"), ("R", "right")].iter().enumerate() {
                let rx = right - (3.0 - i as f64) * bw - (2.0 - i as f64) * gap;
                let r = Rect::new(rx, y, rx + bw, y + 24.0);
                draw_btn(scene, transform, tr, r, label, st.align == *val, true);
                self.panel_hits.push((r, PanelHit::SetAlign(val)));
            }
            y += 30.0;
        }

        // Numeric rows.
        for field in [NumField::Size, NumField::SpaceBefore, NumField::SpaceAfter, NumField::Indent] {
            self.draw_num_row(scene, transform, tr, field, field.get(&st), x0, right, y);
            y += 30.0;
        }

        panel_divider(scene, transform, x0, right, y);
        y += 12.0;

        // New style from this.
        {
            let r = Rect::new(x0, y, right, y + 26.0);
            draw_btn(scene, transform, tr, r, "+ New style from this", false, true);
            self.panel_hits.push((r, PanelHit::NewStyle));
        }

        scene.pop_layer();
    }

    /// One numeric property row: `Label    [−] [ value ] [+]`. The value box is
    /// click-to-type / drag-to-scrub; the steppers nudge by `field.step()`.
    fn draw_num_row(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, field: NumField, value: f32, x0: f64, right: f64, y: f64) {
        tr.draw_text(scene, field.label(), 12.5, PANEL_FG, transform * Affine::translate((x0, y + 5.0)), None);
        let plus = Rect::new(right - 24.0, y, right, y + 24.0);
        let vb = Rect::new(plus.x0 - 60.0, y, plus.x0 - 2.0, y + 24.0);
        let minus = Rect::new(vb.x0 - 26.0, y, vb.x0 - 2.0, y + 24.0);
        draw_btn(scene, transform, tr, minus, "−", false, true);
        draw_btn(scene, transform, tr, plus, "+", false, true);
        // Value box.
        let editing = self.editing_field == Some(field);
        let scrubbing = self.scrub.map(|s| s.field == field).unwrap_or(false);
        let border = if editing || scrubbing { GUIDE } else { PANEL_EDGE };
        scene.fill(Fill::NonZero, transform, PANEL_FIELD, None, &vello::kurbo::RoundedRect::from_rect(vb, 5.0));
        scene.stroke(&vello::kurbo::Stroke::new(if editing || scrubbing { 1.5 } else { 1.0 }), transform, border, None, &vello::kurbo::RoundedRect::from_rect(vb, 5.0));
        let text = if editing {
            format!("{}_", self.edit_buf)
        } else {
            fmt_num(value)
        };
        let approx = text.chars().count() as f64 * 7.0;
        let tx = (vb.x0 + vb.x1) / 2.0 - approx / 2.0;
        tr.draw_text(scene, &text, 12.5, PANEL_FG, transform * Affine::translate((tx, (vb.y0 + vb.y1) / 2.0 - 8.0)), None);
        self.panel_hits.push((vb, PanelHit::Num(field)));
        self.panel_hits.push((minus, PanelHit::NumDelta(field, -field.step())));
        self.panel_hits.push((plus, PanelHit::NumDelta(field, field.step())));
    }
}

/// A horizontal 1px divider line inside the panel.
fn panel_divider(scene: &mut Scene, transform: Affine, x0: f64, x1: f64, y: f64) {
    scene.fill(Fill::NonZero, transform, PANEL_EDGE, None, &Rect::new(x0, y, x1, y + 1.0));
}

/// A panel chip / button: rounded rect + label. `center` centers the label
/// (approximate), else left-pads it.
fn draw_btn(scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, r: Rect, label: &str, active: bool, center: bool) {
    let bg = if active { GUIDE } else { PANEL_CHIP };
    let fg = if active { Color::WHITE } else { PANEL_FG };
    scene.fill(Fill::NonZero, transform, bg, None, &vello::kurbo::RoundedRect::from_rect(r, 6.0));
    let approx = label.chars().count() as f64 * 6.6;
    let tx = if center { ((r.x0 + r.x1) / 2.0 - approx / 2.0).max(r.x0 + 4.0) } else { r.x0 + 8.0 };
    let ty = (r.y0 + r.y1) / 2.0 - 8.0;
    tr.draw_text(scene, label, 12.5, fg, transform * Affine::translate((tx, ty)), Some((r.width() - 6.0) as f32));
}

impl Widget for WordProcessorWidget {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.inner.bind(state, root);
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
        if self.guide_drag.is_none() {
            if let Some([t, r, b, l]) = self.inner.page_margins(state) {
                self.margins = Edges { top: t, right: r, bottom: b, left: l };
            }
        }
        self.dirty = true;
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let full = self.rect;
        // The toolbar may wrap to several rows on a narrow window; reserve its
        // actual (possibly multi-row) height so the canvas top sits below it and
        // no control is clipped. `draw_toolbar` re-derives the same packing, so
        // the reserved band and the drawn rows always agree.
        let tb_h = self.toolbar_height(full.width());
        let tb = Rect::new(full.x0, full.y0, full.x1, full.y0 + tb_h);
        self.draw_toolbar(scene, transform, text_renderer, theme, tb);

        let status_top = (full.y1 - STATUS_H).max(full.y0 + tb_h);
        let layout_bar_top = (status_top - LAYOUT_BAR_H).max(full.y0 + tb_h);
        // Reserve the styles panel out of the canvas so the page re-centers in the
        // remaining width (and scrolled content never paints under the panel).
        let canvas_right = if self.styles_panel_open {
            (full.x1 - STYLES_PANEL_W).max(full.x0 + 1.0)
        } else {
            full.x1
        };
        // Reserve the Find bar (when open) out of the canvas top.
        let find_h = if self.find_open { FIND_BAR_H } else { 0.0 };
        if find_h > 0.0 {
            let fbar = Rect::new(full.x0, full.y0 + tb_h, canvas_right, full.y0 + tb_h + find_h);
            self.draw_find_bar(scene, transform, text_renderer, theme, fbar);
        } else {
            self.find_buttons.clear();
        }
        let canvas = Rect::new(full.x0, full.y0 + tb_h + find_h, canvas_right, layout_bar_top);
        scene.fill(Fill::NonZero, transform, CANVAS, None, &canvas);

        // Zoom anchor: the canvas top, horizontally centered. The page+content
        // transform scales about this point, so the page stays centered and the
        // top of the (scrolled) content stays pinned to the canvas top as zoom
        // changes. The toolbar/find-bar/status bar are drawn with the unscaled
        // `transform` and are unaffected.
        self.zoom_anchor = Point::new((canvas.x0 + canvas.x1) / 2.0, canvas.y0);
        let ctf = self.content_transform(transform);

        // Per-side page geometry — every margin is independently customizable.
        // The page is a FIXED size (it does not resize with the window); the
        // window just shows more or less of it and centers it.
        let m = self.margins;
        let page_w = MAX_PAGE_W;
        let page_x = canvas.x0 + ((canvas.width() - page_w) / 2.0).max(0.0);
        let page_top = canvas.y0 + PAGE_TOP;
        let content_x = page_x + m.left;
        let content_w = (page_w - m.left - m.right).max(80.0) as f32;
        // True pagination: each page is a fixed US-Letter sheet (height `letter_h`)
        // stacked with `PAGE_GAP` between sheets. A block lays into the current
        // page's text band [content_top, content_bottom]; when the *next whole
        // block* would not fit before the bottom text margin, the page advances and
        // the block starts at the next page's top margin — content is never split,
        // so a page-end never crosses a line of text.
        let letter_h = page_w * LETTER_RATIO;
        let page_pitch = letter_h + PAGE_GAP;
        let page_content_top = |i: usize| page_top + i as f64 * page_pitch + m.top;
        let page_content_bottom = |i: usize| page_top + i as f64 * page_pitch + letter_h - m.bottom;

        // Pass 1: shape blocks top-to-bottom. Runs of consecutive `cell` blocks
        // sharing a table id lay out as a grid; everything else stacks normally.
        let blocks = std::mem::take(&mut self.blocks);
        let mut laid: Vec<Laid> = Vec::with_capacity(blocks.len());
        // Incremental layout cache: take last frame's shaped layouts so matching
        // blocks can be reused, and build this frame's cache afresh. Any entry
        // left in `old_cache` at the end (a block that no longer exists) is
        // dropped — this is the per-frame prune that keeps memory bounded.
        let mut old_cache = std::mem::take(&mut self.layout_cache);
        let mut new_cache: HashMap<String, (u64, RichTextLayout)> = HashMap::with_capacity(blocks.len());
        self.dbg_layout_hits = 0;
        self.dbg_layout_misses = 0;
        // Pagination cursor: current page index, whether anything has landed on it
        // yet (the first item on a page never breaks, so an over-tall block simply
        // overflows its own page — acceptable for v1), and the running content y.
        let mut cur_page = 0usize;
        let mut placed_on_page = false;
        let mut y = page_content_top(0);
        // Per-level ordered-list counters (index = nesting level); reset when a
        // non-list block is reached or a shallower level re-enters.
        let mut ol_counts: Vec<u32> = Vec::new();
        let cell_pad = 8.0f64;
        let mut bi = 0;
        while bi < blocks.len() {
            let b = &blocks[bi];
            if let Some(tid) = cell_table(b) {
                ol_counts.clear();
                let start = bi;
                while bi < blocks.len() && cell_table(&blocks[bi]).as_deref() == Some(tid.as_str()) {
                    bi += 1;
                }
                let cells = &blocks[start..bi];
                let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
                let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
                let col_w = content_w as f64 / ncols as f64;
                // Measure every row first so the whole table can paginate as one
                // unit (it is never split mid-row across a page boundary).
                let mut rows: Vec<(f64, Vec<(usize, f64, &BlockView)>)> = Vec::with_capacity(nrows);
                for r in 0..nrows {
                    let mut row_items: Vec<(usize, f64, &BlockView)> = Vec::new();
                    let mut row_h = 26.0f64;
                    for c in 0..ncols {
                        if let Some(cell) = cells.iter().find(|cl| cell_rc(cl) == (r, c)) {
                            let (runs, underlines, strikes) = block_runs(cell);
                            let wrap_w = (col_w - 2.0 * cell_pad).max(24.0) as f32;
                            let align = block_align(cell, TextAlign::Left);
                            let key = shape_key(&runs, &underlines, &strikes, false, 16.0, wrap_w, align, FontFamily::Sans);
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
                                    )
                                }
                            };
                            let cell_h = layout.height() as f64;
                            row_h = row_h.max(cell_h + 2.0 * cell_pad);
                            new_cache.insert(cell.id.clone(), (key, layout));
                            row_items.push((c, cell_h, cell));
                        }
                    }
                    rows.push((row_h, row_items));
                }
                let table_h: f64 = rows.iter().map(|(h, _)| *h).sum();
                // Break to the next page if a manual page break is set on the
                // first cell, or the whole table would not fit.
                let force_break = cells.first().map(block_page_break_before).unwrap_or(false);
                let item_top = if placed_on_page && (force_break || y + table_h > page_content_bottom(cur_page) + 0.5) {
                    cur_page += 1;
                    page_content_top(cur_page)
                } else {
                    y
                };
                placed_on_page = true;
                let mut ry = item_top;
                for (r, (row_h, row_items)) in rows.into_iter().enumerate() {
                    for (c, cell_h, cell) in row_items {
                        let cell_x = content_x + c as f64 * col_w;
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
                            bullet: None,
                            ink: INK,
                            image_hash: None,
                            cell_box: Some(cbox),
                            cell_row: Some(r),
                            page: cur_page,
                        });
                    }
                    ry += row_h;
                }
                y = ry + self.sheet.get("body").space_after as f64;
            } else if b.kind == "image" {
                ol_counts.clear();
                let hash = match b.attrs.get("blob") {
                    Some(AttrVal::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                if !hash.is_empty() && !self.image_cache.contains_key(&hash) {
                    let decoded = self.blobs.get(&hash).and_then(|bytes| ui::widget::Image::from_encoded(bytes).ok());
                    self.image_cache.insert(hash.clone(), decoded);
                }
                let intrinsic = self.image_cache.get(&hash).and_then(|o| o.as_ref()).map(|im| im.intrinsic_size());
                let (draw_w, draw_h) = match intrinsic {
                    Some((iw, ih)) if iw > 0 && ih > 0 => {
                        let w = (iw as f64).min(content_w as f64);
                        (w, (ih as f64 * (w / iw as f64)).max(8.0))
                    }
                    _ => ((content_w as f64).min(280.0), 90.0),
                };
                // Image blocks carry a trivial empty layout (used only so pointer
                // hit-testing resolves to offset 0); it is cheap, so just re-shape
                // it every frame rather than caching it incrementally.
                let empty = RichTextLayout::shape(&mut text_renderer.font_system, &[RichSpan::new("")], false, 16.0, content_w, TextAlign::Left, FontFamily::Sans, vec![], vec![]);
                new_cache.insert(b.id.clone(), (0, empty));
                // Break to the next page on a manual page break, or if the image
                // would not fit.
                let force_break = block_page_break_before(b);
                let item_top = if placed_on_page && (force_break || y + draw_h > page_content_bottom(cur_page) + 0.5) {
                    cur_page += 1;
                    page_content_top(cur_page)
                } else {
                    y
                };
                placed_on_page = true;
                laid.push(Laid {
                    id: b.id.clone(),
                    text: String::new(),
                    kind: "image".into(),
                    size: 16.0,
                    y_top: item_top,
                    text_x: content_x,
                    width: draw_w,
                    height: draw_h,
                    bullet: None,
                    ink: INK,
                    image_hash: intrinsic.map(|_| hash),
                    cell_box: None,
                    cell_row: None,
                    page: cur_page,
                });
                y = item_top + draw_h + self.sheet.get("body").space_after as f64;
                bi += 1;
            } else {
                // Resolve the block's style (component-instance model).
                let st = self.sheet.get(&block_style_key(b));
                let (size, base_bold) = (st.size, st.bold);
                // List items indent by nesting level; the bullet/number sits just
                // left of the text. Ordered items number per level.
                let (indent, bullet) = if b.kind == "list_item" {
                    let level = list_level(b);
                    let indent = st.indent as f64 + level as f64 * LIST_INDENT_STEP;
                    // Keep the ordered counters sized to the current depth; entering
                    // a shallower level drops the deeper counters so they restart.
                    ol_counts.truncate(level + 1);
                    if ol_counts.len() <= level {
                        ol_counts.resize(level + 1, 0);
                    }
                    let bullet = if matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true))) {
                        ol_counts[level] += 1;
                        format!("{}.", ol_counts[level])
                    } else {
                        ol_counts[level] = 0; // a bullet item resets this level's number
                        bullet_glyph(level).to_string()
                    };
                    (indent, Some(bullet))
                } else {
                    ol_counts.clear();
                    (st.indent as f64, None)
                };
                let wrap_w = (content_w - indent as f32).max(40.0);
                let (runs, underlines, strikes) = block_runs(b);
                let align = block_align(b, style_align(&st.align));
                let family = parse_font(&st.font);
                let key = shape_key(&runs, &underlines, &strikes, base_bold, size, wrap_w, align, family);
                // Reuse the shaped layout if nothing affecting shaping changed
                // (text, marks, resolved style, or wrap width), else re-shape.
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
                        )
                    }
                };
                let block_h = layout.height() as f64;
                new_cache.insert(b.id.clone(), (key, layout));
                // Place with the style's space-before as the leading gap, breaking
                // to the next page when the whole block would not fit before the
                // bottom text margin (the lead is dropped at the top of a fresh
                // page so the first paragraph hugs the top margin).
                let top_candidate = y + st.space_before as f64;
                // A manual page break forces a fresh page; otherwise break only
                // when the whole block would overflow the bottom text margin.
                let force_break = block_page_break_before(b);
                let item_top = if placed_on_page && (force_break || top_candidate + block_h > page_content_bottom(cur_page) + 0.5) {
                    cur_page += 1;
                    page_content_top(cur_page)
                } else {
                    top_candidate
                };
                placed_on_page = true;
                laid.push(Laid {
                    id: b.id.clone(),
                    text: b.text.clone(),
                    kind: b.kind.clone(),
                    size,
                    y_top: item_top,
                    text_x: content_x + indent,
                    width: wrap_w as f64,
                    height: block_h,
                    bullet,
                    ink: Color::from_rgb8(st.color[0], st.color[1], st.color[2]),
                    image_hash: None,
                    cell_box: None,
                    cell_row: None,
                    page: cur_page,
                });
                y = item_top + block_h + st.space_after as f64;
                bi += 1;
            }
        }
        // Total document extent below `page_top`: the stacked page sheets + gaps,
        // widened if an over-tall block overflowed its page's text band.
        let num_pages = cur_page + 1;
        let pages_extent = num_pages as f64 * letter_h + cur_page as f64 * PAGE_GAP;
        let extent = pages_extent.max(y - page_top);

        // Scrolling: clamp the offset to the overflow, then shift the laid-out
        // content (+ pages) into screen space so hit-testing needs no offset.
        // `scroll_y` is an offset in unscaled content space; on screen the content
        // is magnified by `zoom`, so the visible window covers `canvas.height() /
        // zoom` of content — divide by zoom so the overflow (and thus max scroll)
        // grows when zoomed in and shrinks when zoomed out.
        self.max_scroll = (extent + 2.0 * PAGE_TOP - canvas.height() / self.zoom).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll);
        let sy = self.scroll_y;
        if sy != 0.0 {
            for l in laid.iter_mut() {
                l.y_top -= sy;
                if let Some(bx) = l.cell_box.as_mut() {
                    *bx = Rect::new(bx.x0, bx.y0 - sy, bx.x1, bx.y1 - sy);
                }
            }
        }

        // Clip the document to the canvas so scrolled content never paints over
        // the toolbar / status bar.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, transform, &canvas);

        // Discrete page sheets (shadow + paper) stacked with `PAGE_GAP`. Each
        // sheet draws the page header inside its top-margin band and the footer
        // inside its bottom-margin band (page-number fields resolved to this
        // page's number). A subtle "Page N" indicator lives in the gap below the
        // sheet only when the document has no footer of its own. All through
        // `ctf` (the zoom transform) so the page magnifies with the content.
        let header = std::mem::take(&mut self.header);
        let footer = std::mem::take(&mut self.footer);
        for i in 0..num_pages {
            let ptop = page_top + i as f64 * page_pitch - sy;
            let sheet = Rect::new(page_x, ptop, page_x + page_w, ptop + letter_h);
            chrome::page::draw(scene, ctf, sheet, PAPER, SHADOW);
            let page_no = i + 1;
            // Header band: the top margin, inset by the L/R text margins.
            let head_band = Rect::new(sheet.x0 + m.left, sheet.y0, sheet.x1 - m.right, sheet.y0 + m.top);
            self.draw_running_band(scene, ctf, text_renderer, &header, page_no, head_band, false);
            // Footer band: the bottom margin, inset by the L/R text margins.
            let foot_band = Rect::new(sheet.x0 + m.left, sheet.y1 - m.bottom, sheet.x1 - m.right, sheet.y1);
            let drew_footer = self.draw_running_band(scene, ctf, text_renderer, &footer, page_no, foot_band, true);
            // Default page indicator in the gap — only when no footer was drawn.
            if !drew_footer {
                let label = format!("Page {page_no}");
                let approx = label.chars().count() as f64 * 6.4;
                let lx = (sheet.x0 + sheet.x1) / 2.0 - approx / 2.0;
                let ly = sheet.y1 + PAGE_GAP / 2.0 - 6.0;
                text_renderer.draw_text(scene, &label, 11.0, Color::from_rgb8(0x9a, 0x9e, 0xa8), ctf * Affine::translate((lx, ly)), None);
            }
        }
        self.header = header;
        self.footer = footer;
        // The first page is the reference rect for the margin-guide overlay
        // (margins apply to every page identically).
        let page = Rect::new(page_x, page_top - sy, page_x + page_w, page_top + letter_h - sy);
        self.page_rect = page;

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
        if self.spellcheck_on {
            // A generous band around the canvas (one screenful above/below) so a
            // block scrolling into view is already checked; everything else is
            // skipped to keep the pass cheap on long documents.
            let cull_lo = canvas.y0 - canvas.height();
            let cull_hi = canvas.y1 + canvas.height();
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

        // Per-page footnote bucket: page index → note numbers whose marker lands
        // on that page, collected during pass 2 and rendered after it.
        let mut page_notes: HashMap<usize, Vec<usize>> = HashMap::new();

        // Pass 2: draw cell borders, bullets, selection, text, caret. Everything
        // here renders through `ctf` (the zoom transform) so page content scales
        // while the surrounding chrome stays at 100%.
        for l in &laid {
            if l.kind == "image" {
                let rect = Rect::new(l.text_x, l.y_top, l.text_x + l.width, l.y_top + l.height);
                if let Some(hash) = &l.image_hash {
                    if let Some(slot) = self.image_cache.get_mut(hash) {
                        if let Some(img) = slot {
                            img.paint_at(scene, ctf, rect);
                        }
                    }
                } else {
                    // Missing or undecodable (e.g. non-PNG) image — placeholder.
                    scene.fill(Fill::NonZero, ctf, Color::from_rgb8(0xee, 0xee, 0xf2), None, &vello::kurbo::RoundedRect::from_rect(rect, 6.0));
                    scene.stroke(&vello::kurbo::Stroke::new(1.0), ctf, Color::from_rgb8(0xcc, 0xce, 0xd6), None, &vello::kurbo::RoundedRect::from_rect(rect, 6.0));
                    text_renderer.draw_text(scene, "🖼  image", 14.0, MUTED, ctf * Affine::translate((rect.x0 + 12.0, rect.y0 + rect.height() / 2.0 - 8.0)), None);
                }
                continue;
            }
            if let Some(bx) = l.cell_box {
                let fill = match l.cell_row {
                    Some(0) => Color::from_rgb8(0xec, 0xee, 0xf3), // header row tint
                    Some(r) if r % 2 == 0 => Color::from_rgb8(0xf6, 0xf6, 0xf9), // zebra
                    _ => PAPER,
                };
                scene.fill(Fill::NonZero, ctf, fill, None, &bx);
                scene.stroke(&vello::kurbo::Stroke::new(1.0), ctf, Color::from_rgb8(0xc4, 0xc6, 0xcc), None, &bx);
            }

            if l.kind == "fidelity" {
                let box_rect = Rect::new(l.text_x, l.y_top + 2.0, (page_x + page_w - m.right).min(l.text_x + 360.0), l.y_top + l.height - 2.0);
                scene.fill(Fill::NonZero, ctf, Color::from_rgb8(0xee, 0xee, 0xf2), None, &vello::kurbo::RoundedRect::from_rect(box_rect, 4.0));
                text_renderer.draw_text(scene, "⟦ embedded content ⟧", 14.0, MUTED, ctf * Affine::translate((l.text_x + 10.0, l.y_top + 6.0)), None);
                continue;
            }

            if let Some(bul) = &l.bullet {
                text_renderer.draw_text(scene, bul, l.size, l.ink, ctf * Affine::translate((l.text_x - BULLET_GAP, l.y_top)), None);
            }

            // The shaped text lives in the layout cache (built in pass 1), keyed
            // by block id. Every normal/cell entry was inserted there.
            let layout = match new_cache.get(&l.id) {
                Some((_, lay)) => lay,
                None => continue,
            };

            if let Some((b0, b1)) = self.selection_bytes_for(&l.id, &l.text) {
                if let (Some(bx), true) = (l.cell_box, multi_sel) {
                    // Multi-cell selection: highlight the whole cell (spreadsheet-style).
                    scene.fill(Fill::NonZero, ctf, SELECTION, None, &bx);
                } else {
                    for (rx0, ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
                        let r = Rect::new(l.text_x + rx0 as f64, l.y_top + ry0 as f64, l.text_x + rx1 as f64, l.y_top + ry1 as f64);
                        scene.fill(Fill::NonZero, ctf, SELECTION, None, &r);
                    }
                }
            }

            // Find-match highlights (under the text).
            if self.find_open {
                for (bid, cs, ce) in &self.find_matches {
                    if bid == &l.id {
                        let bs = char_to_byte(&l.text, *cs);
                        let be = char_to_byte(&l.text, *ce);
                        for (rx0, ry0, rx1, ry1) in layout.selection_rects(bs, be) {
                            let r = Rect::new(l.text_x + rx0 as f64, l.y_top + ry0 as f64, l.text_x + rx1 as f64, l.y_top + ry1 as f64);
                            scene.fill(Fill::NonZero, ctf, FIND_HL, None, &r);
                        }
                    }
                }
            }
            text_renderer.draw_rich(scene, layout, ctf * Affine::translate((l.text_x, l.y_top)), l.ink);

            // Spell-check squiggles: a wavy red underline under each misspelled
            // span. The byte span maps to one-or-more on-screen rects via the same
            // `selection_rects` geometry the selection highlight uses (so it tracks
            // wrapping). The squiggle hugs just under each rect's bottom.
            if let Some(spans) = spell_by_block.get(&l.id) {
                for &(b0, b1) in spans {
                    for (rx0, _ry0, rx1, ry1) in layout.selection_rects(b0, b1) {
                        let y = l.y_top + ry1 as f64 - 1.0;
                        draw_squiggle(scene, ctf, l.text_x + rx0 as f64, l.text_x + rx1 as f64, y);
                    }
                }
            }

            // Note-reference superscripts: each `note-ref` placeholder in this
            // block renders as a small superscript citation number, positioned at
            // the placeholder glyph's location (it occupies one U+FFFC char, which
            // the body run shapes as near-zero-width). The number is taken from the
            // document-order numbering; the page is recorded so the footnote text
            // renders at the bottom of this page.
            if !note_nums.is_empty() {
                if let Some(b) = self.blocks.iter().find(|b| b.id == l.id) {
                    for (ci, note_id) in block_note_refs(b) {
                        let Some(&num) = note_nums.get(&note_id) else { continue };
                        let bs = char_to_byte(&l.text, ci);
                        let be = char_to_byte(&l.text, ci + 1);
                        // Anchor at the placeholder's left edge; fall back to the
                        // caret geometry if the glyph produced no selection rect.
                        let placeholder_rect = layout.selection_rects(bs, be).into_iter().next();
                        let (mx, my) = placeholder_rect
                            .map(|(rx0, ry0, _, _)| (rx0, ry0))
                            .unwrap_or_else(|| {
                                let (cx, cy, _) = layout.caret_geometry(bs);
                                (cx, cy)
                            });
                        // Mask the U+FFFC object-replacement glyph (a box in some
                        // fonts) with paper so only the superscript number shows.
                        if let Some((rx0, ry0, rx1, ry1)) = placeholder_rect {
                            let mask = Rect::new(l.text_x + rx0 as f64, l.y_top + ry0 as f64, l.text_x + rx1 as f64, l.y_top + ry1 as f64);
                            scene.fill(Fill::NonZero, ctf, PAPER, None, &mask);
                        }
                        let label = num.to_string();
                        // Superscript: smaller font, raised toward the line's top.
                        let sup_size = (l.size * 0.7).max(8.0);
                        text_renderer.draw_text(
                            scene,
                            &label,
                            sup_size,
                            l.ink,
                            ctf * Affine::translate((l.text_x + mx as f64, l.y_top + my as f64 - 1.0)),
                            None,
                        );
                        page_notes.entry(l.page).or_default().push(num);
                    }
                }
            }

            if self.focused && l.id == self.inner.caret.block && !self.inner.has_selection() {
                let byte = char_to_byte(&l.text, self.inner.caret.offset);
                let (cx, cy, ch) = layout.caret_geometry(byte);
                let caret = Rect::new(l.text_x + cx as f64, l.y_top + cy as f64, l.text_x + cx as f64 + CARET_WIDTH, l.y_top + cy as f64 + ch as f64);
                scene.fill(Fill::NonZero, ctf, CARET_COLOR, None, &caret);
            }
        }

        // Footnote areas: for every page that carries a footnote marker, draw a
        // thin separator + the footnotes (numbered, in a smaller font) stacked at
        // the bottom of that page's text band, above the footer margin. Page
        // assignment is approximate — a footnote renders on the page where its
        // marker was laid. Rendered inside the page clip so it scrolls with the
        // content and never paints over the toolbar/status bar.
        if !page_notes.is_empty() {
            // number → footnote text, resolved once. Endnotes are skipped here
            // (they belong at the document end, not per page).
            let mut num_to_note: HashMap<usize, String> = HashMap::new();
            for n in &self.notes {
                if n.kind == qdoc::schema::NOTE_KIND_FOOTNOTE {
                    if let Some(&num) = note_nums.get(&n.id) {
                        num_to_note.insert(num, note_text(n));
                    }
                }
            }
            for (page, nums) in &page_notes {
                // Distinct, ascending footnote numbers on this page.
                let mut nums: Vec<usize> = nums.iter().copied().filter(|n| num_to_note.contains_key(n)).collect();
                nums.sort_unstable();
                nums.dedup();
                if nums.is_empty() {
                    continue;
                }
                let line_h = (NOTE_FONT as f64 * 1.4).ceil();
                let area_h = nums.len() as f64 * line_h;
                // Bottom of the text band on this page (above the footer margin).
                let band_bottom = page_top + *page as f64 * page_pitch + letter_h - m.bottom - sy;
                let sep_y = band_bottom - area_h - NOTE_AREA_GAP;
                let left = page_x + m.left;
                let right = page_x + page_w - m.right;
                // Thin separator (about a third of the text width, like Word).
                let sep = vello::kurbo::Line::new((left, sep_y), (left + (right - left) * 0.33, sep_y));
                scene.stroke(&vello::kurbo::Stroke::new(0.75), ctf, NOTE_SEP, None, &sep);
                let mut ny = sep_y + NOTE_AREA_GAP * 0.6;
                for num in nums {
                    if let Some(text) = num_to_note.get(&num) {
                        let line = format!("{num}. {text}");
                        text_renderer.draw_text(scene, &line, NOTE_FONT, NOTE_INK, ctf * Affine::translate((left, ny)), None);
                        ny += line_h;
                    }
                }
            }
        }

        // Empty-document placeholder.
        if self.blocks.len() == 1 && self.blocks[0].kind == "paragraph" && self.blocks[0].text.is_empty() {
            if let Some(l) = laid.first() {
                text_renderer.draw_text(
                    scene,
                    "Start writing…",
                    16.5,
                    Color::from_rgb8(0xbc, 0xbc, 0xc2),
                    ctf * Affine::translate((l.text_x, l.y_top)),
                    None,
                );
            }
        }

        // Layout-edit overlay: draggable guides + live value labels for the
        // active mode (drawn over the content, still inside the page clip).
        if self.layout_mode != LayoutMode::None {
            self.draw_layout_overlay(scene, ctf, text_renderer, page);
        }

        scene.pop_layer();

        // Scrollbar thumb (right edge) when the content overflows the canvas.
        if self.max_scroll > 0.0 {
            let track = canvas.height();
            let thumb_h = (track * track / (track + self.max_scroll)).max(28.0);
            let frac = self.scroll_y / self.max_scroll;
            let thumb_y = canvas.y0 + frac * (track - thumb_h);
            let thumb = Rect::new(canvas.x1 - 8.0, thumb_y + 2.0, canvas.x1 - 4.0, thumb_y + thumb_h - 2.0);
            scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x55, 0x58, 0x60), None, &vello::kurbo::RoundedRect::from_rect(thumb, 2.0));
        }

        self.laid = laid;
        // Adopt this frame's cache (dropping `old_cache`, i.e. pruning entries
        // for blocks that no longer exist) so the next frame can reuse it.
        self.layout_cache = new_cache;

        // Contextual styles panel on the right (between toolbar and layout bar).
        if self.styles_panel_open {
            let panel = Rect::new(canvas_right, full.y0 + tb_h, full.x1, layout_bar_top);
            self.draw_styles_panel(scene, transform, text_renderer, panel);
        } else {
            self.panel_rect = Rect::ZERO;
            self.panel_hits.clear();
        }

        let lbar = Rect::new(full.x0, layout_bar_top, full.x1, status_top);
        self.draw_layout_bar(scene, transform, text_renderer, theme, lbar);

        let bar = Rect::new(full.x0, status_top, full.x1, full.y1);
        self.draw_status_bar(scene, transform, text_renderer, theme, bar);
    }

    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &Modifiers, state: &mut AutoCommit, _path: &ObjId, handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {
        let shift = modifiers.state().shift_key();
        let ctrl = modifiers.state().control_key();
        // The toolbar may have wrapped to several rows; hit-test against its
        // actual bottom (`tb_h`), matching `draw`.
        let tb_h = self.toolbar_height(self.rect.width());
        match event {
            // Drag-and-drop a file onto the window inserts it as an image at the
            // caret. This is the primary (portal-free) image-insert path.
            WindowEvent::DroppedFile(path) => {
                if self.insert_image_from_path(state, path) {
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 48.0,
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
                if new != self.scroll_y {
                    self.scroll_y = new;
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Toolbar hover feedback — repaint only when the hovered button changes.
                let new_hover = if self.pointer.y < self.rect.y0 + tb_h {
                    self.toolbar.iter().find(|(_, r)| r.contains(self.pointer)).map(|(_, r)| *r)
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
                        let v = sc.field.clamp(sc.start_val + dx as f32 * sc.field.scrub_factor());
                        if let Some(key) = self.focused_style_key() {
                            if let Some(st) = self.sheet.styles.get_mut(&key) {
                                sc.field.set(st, v);
                            }
                        }
                        self.dirty = true;
                    }
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
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if !self.rect.contains(self.pointer) {
                    return;
                }
                self.focused = true;
                // A press anywhere commits a pending typed numeric edit.
                if self.editing_field.is_some() {
                    self.commit_num_edit(state);
                }
                // Styles panel (right side): dispatch a click on its controls.
                if self.styles_panel_open && self.panel_rect.contains(self.pointer) {
                    if let Some(hit) = self.panel_hits.iter().rev().find(|(r, _)| r.contains(self.pointer)).map(|(_, h)| h.clone()) {
                        self.apply_panel_hit(state, hit);
                    }
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // Find bar (when open): dispatch clicks on its fields + controls.
                if self.find_open {
                    let fbar_bot = self.rect.y0 + tb_h + FIND_BAR_H;
                    if self.pointer.y >= self.rect.y0 + tb_h && self.pointer.y < fbar_bot {
                        if let Some(action) = self.find_buttons.iter().find(|(_, r)| r.contains(self.pointer)).map(|(a, _)| *a) {
                            self.find_action(state, action);
                        }
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
                }
                if self.pointer.y < self.rect.y0 + tb_h {
                    // Reverse so the colour swatches (pushed after the chrome
                    // buttons) win a hit-test on any overlap at a crowded edge.
                    let clicked = self.toolbar.iter().rev().find(|(_, r)| r.contains(self.pointer)).map(|(a, _)| *a);
                    if let Some(action) = clicked {
                        self.apply_tool(state, action);
                        self.refresh_blocks(state);
                        self.dirty = true;
                        if action != ToolAction::ToggleStyles {
                            self.needs_commit = true;
                        }
                    }
                    *handled = true;
                    return;
                }
                if self.pointer.y > self.rect.y1 - STATUS_H {
                    // Status bar: the only clickable controls are the zoom stepper.
                    if let Some(action) = self.status_buttons.iter().find(|(_, r)| r.contains(self.pointer)).map(|(a, _)| *a) {
                        match action {
                            StatusAction::ZoomOut => self.zoom_by(-ZOOM_STEP),
                            StatusAction::ZoomIn => self.zoom_by(ZOOM_STEP),
                            StatusAction::ZoomReset => self.set_zoom(1.0),
                        };
                        self.dirty = true;
                        *handled = true;
                    }
                    return; // status bar text itself — not editable
                }
                // Bottom layout bar (mode chips), just above the status bar.
                let lbar_top = self.rect.y1 - STATUS_H - LAYOUT_BAR_H;
                if self.pointer.y >= lbar_top {
                    if let Some(mode) = self.layout_chips.iter().find(|(_, r)| r.contains(self.pointer)).map(|(m, _)| *m) {
                        self.toggle_layout_mode(mode);
                    }
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                // Page-content hit-tests work in unscaled content space; map the
                // pointer through the inverse zoom transform once here.
                let cp = self.pointer_to_content(self.pointer);
                // In a layout-edit mode, grabbing a guide starts a drag instead of
                // a text selection.
                if self.layout_mode != LayoutMode::None {
                    if let Some(side) = self.hit_guide(cp) {
                        let kind = if ctrl {
                            DragKind::Proportional
                        } else if shift {
                            DragKind::Single
                        } else {
                            DragKind::Pair
                        };
                        self.guide_drag = Some(GuideDrag { mode: self.layout_mode, side, kind });
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
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
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if let Some(sc) = self.scrub.take() {
                    if sc.moved {
                        // A scrub persists the style edit (every instance follows).
                        let sheet = self.sheet.clone();
                        self.inner.set_stylesheet(state, &sheet);
                        self.needs_commit = true;
                    } else {
                        // A plain click (no drag) focuses the field for typing.
                        self.editing_field = Some(sc.field);
                        let cur = self.focused_style_key().map(|k| sc.field.get(&self.sheet.get(&k))).unwrap_or(0.0);
                        self.edit_buf = fmt_num(cur);
                    }
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
            WindowEvent::KeyboardInput { event: KeyEvent { state: ElementState::Pressed, logical_key, text, .. }, .. } => {
                if !self.focused {
                    return;
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
                            let typed: String = text.as_ref().map(|t| t.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect()).unwrap_or_default();
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
                    return;
                }
                // While the Find bar is open, non-Ctrl keys edit its fields.
                if self.find_open && !ctrl {
                    let typed = text.as_deref();
                    if self.find_key(state, &logical_key, typed, shift) {
                        self.dirty = true;
                        *handled = true;
                        return;
                    }
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
                    return;
                }
                if ctrl {
                    if let Key::Character(s) = logical_key {
                        // Save is not an editing action — don't mark the doc
                        // modified again after writing it.
                        if s.eq_ignore_ascii_case("s") {
                            self.save(state);
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                        if s.eq_ignore_ascii_case("f") {
                            if self.find_open {
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
                        // Zoom: Ctrl+= / Ctrl++ (in), Ctrl+- (out), Ctrl+0 (reset
                        // to 100%). These are view-only — they never mutate the doc,
                        // so they return before the editing path below.
                        match s.as_str() {
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
                            "b" => self.inner.toggle_mark(state, "strong"),
                            "i" => self.inner.toggle_mark(state, "em"),
                            "u" => self.inner.toggle_mark(state, "underline"),
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
                            "1" => self.inner.set_paragraph_style(state, "heading", Some(1), None),
                            "2" => self.inner.set_paragraph_style(state, "heading", Some(2), None),
                            "3" => self.inner.set_paragraph_style(state, "heading", Some(3), None),
                            // z / y (undo/redo) are handled by the host App.
                            _ => {}
                        }
                        self.refresh_blocks(state);
                        self.dirty = true;
                        self.needs_commit = true;
                        *handled = true;
                        return;
                    }
                }

                let mut changed = true;
                let mut motion = false;
                match logical_key {
                    Key::Named(NamedKey::Tab) => {
                        // Inside a table, Tab/Shift-Tab walk cells (and Tab in the
                        // last cell appends a row). Elsewhere Tab inserts spaces.
                        if self.inner.tab_cell(state, !shift) {
                            motion = true; // tab_cell already cleared any selection
                        } else {
                            self.inner.insert_str(state, "    ");
                        }
                    }
                    Key::Named(NamedKey::Enter) => self.inner.enter(state),
                    Key::Named(NamedKey::Backspace) => self.inner.backspace(state),
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
                        if !self.inner.autoformat_on_space(state) {
                            self.inner.insert_str(state, " ");
                        }
                    }
                    _ => {
                        let typed: String = text
                            .as_ref()
                            .map(|t| t.chars().filter(|c| !c.is_control()).collect())
                            .unwrap_or_default();
                        if typed.is_empty() {
                            changed = false;
                        } else {
                            self.inner.insert_str(state, &typed);
                        }
                    }
                }
                if changed {
                    if !motion {
                        // editing consumes/collapses any selection
                        self.inner.clear_anchor();
                    }
                    self.refresh_blocks(state);
                    self.dirty = true;
                    self.needs_commit = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "WordProcessor"
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
    fn needs_commit(&self) -> bool {
        self.needs_commit
    }
    fn on_commit(&mut self) {
        self.needs_commit = false;
        self.modified = true;
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
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
        match (self.block_index_cached(&anchor.block), self.block_index_cached(&caret.block)) {
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
        let (Some(si), Some(ei)) = (self.block_index_cached(&start.block), self.block_index_cached(&end.block)) else {
            return vec![];
        };
        (si..=ei)
            .map(|i| {
                let b = &self.blocks[i];
                let blen = b.text.chars().count();
                let s = if i == si { start.offset } else { 0 };
                let e = if i == ei { end.offset } else { blen };
                (b.id.clone(), s, e)
            })
            .collect()
    }

    fn selection_bytes_for(&self, block_id: &str, text: &str) -> Option<(usize, usize)> {
        let (_, s, e) = self.selection_blocks_cached().into_iter().find(|(b, _, _)| b == block_id)?;
        if s >= e {
            return None;
        }
        Some((char_to_byte(text, s), char_to_byte(text, e)))
    }

    fn extend_or_clear(&mut self, shift: bool) {
        if shift {
            self.inner.set_anchor();
        } else {
            self.inner.clear_anchor();
        }
    }

    fn mark_active_in_selection(&self, name: &str) -> bool {
        let sel: Vec<_> = self.selection_blocks_cached().into_iter().filter(|(_, s, e)| s < e).collect();
        if sel.is_empty() {
            return false;
        }
        sel.iter().all(|(bid, s, e)| {
            self.blocks
                .iter()
                .find(|b| &b.id == bid)
                .map(|b| b.marks.iter().any(|m| m.name == name && m.start <= *s && m.end >= *e))
                .unwrap_or(false)
        })
    }

    fn current_block_style(&self) -> (String, Option<i64>) {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block);
        let kind = b.map(|b| b.kind.clone()).unwrap_or_else(|| "paragraph".into());
        let level = b.and_then(|b| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => Some(*n),
            _ => None,
        });
        (kind, level)
    }

    fn apply_tool(&mut self, state: &mut AutoCommit, action: ToolAction) {
        match action {
            ToolAction::Bold => self.inner.toggle_mark(state, "strong"),
            ToolAction::Italic => self.inner.toggle_mark(state, "em"),
            ToolAction::Underline => self.inner.toggle_mark(state, "underline"),
            ToolAction::Strike => self.inner.toggle_mark(state, "strike"),
            ToolAction::H1 => self.inner.set_paragraph_style(state, "heading", Some(1), None),
            ToolAction::H2 => self.inner.set_paragraph_style(state, "heading", Some(2), None),
            ToolAction::Body => self.inner.set_paragraph_style(state, "paragraph", None, None),
            ToolAction::Bullet => self.inner.set_paragraph_style(state, "list_item", None, Some(false)),
            ToolAction::AlignLeft => self.inner.set_align(state, "left"),
            ToolAction::AlignCenter => self.inner.set_align(state, "center"),
            ToolAction::AlignRight => self.inner.set_align(state, "right"),
            ToolAction::AlignJustify => self.inner.set_align(state, "justify"),
            ToolAction::TextColor(Some(c)) => self.inner.set_text_color(state, &crate::styles::color_to_hex(c)),
            // The neutral swatch clears any per-run colour, reverting to the
            // block's style ink.
            ToolAction::TextColor(None) => self.inner.clear_mark(state, "color"),
            ToolAction::FontCycle => {
                let name = match self.current_font().next() {
                    FontFamily::Sans => "sans",
                    FontFamily::Serif => "serif",
                    FontFamily::Mono => "mono",
                };
                self.inner.set_font_family(state, name);
            }
            ToolAction::SizeUp => {
                let s = ((self.current_size() + 2.0).round() as i64).min(200);
                self.inner.set_font_size(state, s);
            }
            ToolAction::SizeDown => {
                let s = ((self.current_size() - 2.0).round() as i64).max(6);
                self.inner.set_font_size(state, s);
            }
            ToolAction::SizeShow => {} // display-only value chip
            ToolAction::InsertImage => self.request_image_insert(state),
            ToolAction::InsertTable => self.inner.insert_table(state, 2, 2),
            ToolAction::TableRowAdd => self.inner.table_add_row(state),
            ToolAction::TableRowDel => self.inner.table_del_row(state),
            ToolAction::TableColAdd => self.inner.table_add_col(state),
            ToolAction::TableColDel => self.inner.table_del_col(state),
            ToolAction::TableDelete => self.inner.table_delete(state),
            ToolAction::ToggleStyles => self.styles_panel_open = !self.styles_panel_open,
        }
    }

    /// Mutate the focused block's style in the live widget sheet (component-
    /// instance preview), then optionally persist it to the document.
    fn edit_focused_style(&mut self, state: &mut AutoCommit, persist: bool, f: impl FnOnce(&mut crate::styles::StyleProps)) {
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
        let v = field.clamp(self.edit_buf.parse::<f32>().unwrap_or(0.0));
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
                self.inner.set_block_style(state, &id);
                self.refresh_blocks(state);
                self.needs_commit = true;
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
            PanelHit::ToggleItalic => self.edit_focused_style(state, true, |st| st.italic = !st.italic),
            PanelHit::SetAlign(a) => self.edit_focused_style(state, true, |st| st.align = a.into()),
            PanelHit::SetColor(c) => self.edit_focused_style(state, true, |st| st.color = c),
            PanelHit::NumDelta(field, d) => {
                self.edit_focused_style(state, true, |st| {
                    let nv = field.clamp(field.get(st) + d);
                    field.set(st, nv);
                });
            }
            PanelHit::Num(field) => {
                // Start a scrub candidate: a drag scrubs the value, a plain click
                // (no movement) focuses the field for typing (handled on release).
                let start_val = self.focused_style_key().map(|k| field.get(&self.sheet.get(&k))).unwrap_or(0.0);
                self.editing_field = None;
                self.scrub = Some(ScrubState { field, start_x: self.pointer.x, start_val, moved: false });
                self.dirty = true;
            }
            PanelHit::NewStyle => {
                let Some(key) = self.focused_style_key() else {
                    return;
                };
                let base = self.sheet.get(&key);
                let mut n = 1;
                let new_id = loop {
                    let cand = format!("custom{n}");
                    if !self.sheet.styles.contains_key(&cand) {
                        break cand;
                    }
                    n += 1;
                };
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
        }
    }

    /// The next/previous cell id within the caret cell's table, if any.
    /// The char position the toolbar reports font state for: just left of the
    /// caret (so it reflects the run you're typing into), else the caret.
    fn font_probe(&self) -> Option<(&BlockView, usize)> {
        let b = self.blocks.iter().find(|b| b.id == self.inner.caret.block)?;
        let pos = self.inner.caret.offset.saturating_sub(1);
        Some((b, pos))
    }

    fn current_font(&self) -> FontFamily {
        self.font_probe()
            .and_then(|(b, pos)| char_font_family(pos, &b.marks))
            .unwrap_or_default()
    }

    fn current_size(&self) -> f32 {
        match self.font_probe() {
            Some((b, pos)) => char_font_size(pos, &b.marks).unwrap_or_else(|| self.sheet.get(&block_style_key(b)).size),
            None => 16.5,
        }
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

    fn clipboard_copy(&mut self, state: &AutoCommit) {
        if !self.inner.has_selection() {
            return;
        }
        let (plain, blocks) = self.inner.copy_fragment(state);
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(plain.clone());
        }
        self.rich_clip = Some((plain, blocks));
    }

    /// Paste with formatting when the system clipboard still holds our last rich
    /// copy; otherwise paste the clipboard's plain text (e.g. from another app).
    fn clipboard_paste(&mut self, state: &mut AutoCommit) {
        let Some(sys) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()) else {
            return;
        };
        let rich = self.rich_clip.as_ref().filter(|(plain, _)| plain == &sys).map(|(_, blocks)| blocks.clone());
        match rich {
            Some(blocks) => self.inner.insert_rich(state, &blocks),
            None => self.inner.insert_multiline(state, &sys),
        }
    }

    /// Paste plain text only (Ctrl+Shift+V), stripping all formatting.
    fn clipboard_paste_plain(&mut self, state: &mut AutoCommit) {
        if let Some(t) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()) {
            self.inner.insert_multiline(state, &t);
        }
    }

    fn select_all_in_block(&mut self, state: &AutoCommit) {
        self.inner.move_home();
        self.inner.clear_anchor();
        self.inner.set_anchor();
        self.inner.move_end(state);
    }

    fn draw_status_bar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, bar: Rect) {
        let words: usize = self.blocks.iter().map(|b| b.text.split_whitespace().count()).sum();
        let chars: usize = self.blocks.iter().map(|b| b.text.chars().count()).sum();
        let left = format!("{words} words    {chars} characters");
        let fname = self.path.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("Untitled");
        let kind = match self.format {
            Some(WpFormat::Odt) => "ODF Text",
            Some(WpFormat::Docx) => "Word Document",
            Some(WpFormat::Markdown) => "Markdown",
            Some(WpFormat::Pdf) => "PDF",
            Some(WpFormat::Html) => "HTML",
            Some(WpFormat::Rtf) => "Rich Text",
            None => "",
        };
        let dot = if self.modified { "●  " } else { "" };
        let right = if kind.is_empty() {
            format!("{dot}{fname}")
        } else {
            format!("{dot}{fname}    ·    {kind}")
        };
        chrome::status_bar::draw(scene, transform, tr, theme, bar, &left, &right);
        // Overlay the zoom stepper (− value + …%) on top of the status bar,
        // centered. It's the only clickable status-bar control.
        self.draw_zoom_control(scene, transform, tr, bar);
    }

    /// Draw the status-bar zoom control: a − button, the current percentage, a +
    /// button, then a small "Reset" affordance when zoomed away from 100%. Each
    /// button is registered in `self.status_buttons` for click hit-testing.
    fn draw_zoom_control(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, bar: Rect) {
        self.status_buttons.clear();
        let cy = (bar.y0 + bar.y1) / 2.0;
        let bw = 18.0;
        let bh = 18.0;
        let y0 = cy - bh / 2.0;
        let pct = format!("{}%", (self.zoom * 100.0).round() as i32);
        // Layout: [−] [ 100% ] [+]  — centered horizontally in the bar.
        let val_w = 44.0;
        let total = bw + 4.0 + val_w + 4.0 + bw;
        let mut x = (bar.x0 + bar.x1) / 2.0 - total / 2.0;

        let btn = |scene: &mut Scene, tr: &mut TextRenderer, x: f64, label: &str| -> Rect {
            let r = Rect::new(x, y0, x + bw, y0 + bh);
            scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x33, 0x36, 0x40), None, &vello::kurbo::RoundedRect::from_rect(r, 4.0));
            let tx = r.x0 + (bw - label.chars().count() as f64 * 6.5).max(2.0) / 2.0;
            tr.draw_text(scene, label, 13.0, Color::from_rgb8(0xc8, 0xcc, 0xd4), transform * Affine::translate((tx, cy - 8.0)), None);
            r
        };

        let minus = btn(scene, tr, x, "−");
        self.status_buttons.push((StatusAction::ZoomOut, minus));
        x += bw + 4.0;

        // The percentage chip doubles as a reset target (click to return to 100%).
        let vr = Rect::new(x, y0, x + val_w, y0 + bh);
        let pw = pct.chars().count() as f64 * 6.8;
        tr.draw_text(scene, &pct, 12.5, Color::from_rgb8(0xd6, 0xd9, 0xe0), transform * Affine::translate((x + (val_w - pw) / 2.0, cy - 8.0)), None);
        self.status_buttons.push((StatusAction::ZoomReset, vr));
        x += val_w + 4.0;

        let plus = btn(scene, tr, x, "+");
        self.status_buttons.push((StatusAction::ZoomIn, plus));
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, tb: Rect) {
        let bold_a = self.mark_active_in_selection("strong");
        let italic_a = self.mark_active_in_selection("em");
        let under_a = self.mark_active_in_selection("underline");
        let strike_a = self.mark_active_in_selection("strike");
        let (kind, level) = self.current_block_style();
        let align = self.current_align();
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
            matches!(a, ToolAction::H1 | ToolAction::AlignLeft | ToolAction::FontCycle | ToolAction::InsertTable | ToolAction::InsertImage | ToolAction::TableRowAdd | ToolAction::ToggleStyles)
        };
        let panel_open = self.styles_panel_open;
        let mut specs: Vec<(ToolAction, ToolbarItem)> = Vec::new();
        let mut add = |a: ToolAction, label: &str, w: f64, active: bool| {
            specs.push((a, ToolbarItem::new(label, w).active(active).cue(cue(a)).group_start(group_start(a))));
        };
        add(ToolAction::Bold, "B", 34.0, bold_a);
        add(ToolAction::Italic, "I", 30.0, italic_a);
        add(ToolAction::Underline, "U", 32.0, under_a);
        add(ToolAction::Strike, "S", 30.0, strike_a);
        add(ToolAction::H1, "H1", 42.0, kind == "heading" && level == Some(1));
        add(ToolAction::H2, "H2", 42.0, kind == "heading" && level == Some(2));
        add(ToolAction::Body, "Body", 58.0, kind == "paragraph");
        add(ToolAction::Bullet, "• List", 66.0, kind == "list_item");
        add(ToolAction::AlignLeft, "L", 30.0, align == "left");
        add(ToolAction::AlignCenter, "C", 30.0, align == "center");
        add(ToolAction::AlignRight, "R", 30.0, align == "right");
        add(ToolAction::AlignJustify, "J", 30.0, align == "justify");
        add(ToolAction::FontCycle, font_label, 58.0, false);
        // Size stepper: − / current-value / +. The value chip is display-only.
        add(ToolAction::SizeDown, "A−", 34.0, false);
        add(ToolAction::SizeShow, &fmt_num(self.current_size()), 40.0, false);
        add(ToolAction::SizeUp, "A+", 34.0, false);
        // Contextual table section: editing controls inside a cell, else insert.
        if kind == "cell" {
            add(ToolAction::TableRowAdd, "+Row", 54.0, false);
            add(ToolAction::TableRowDel, "−Row", 54.0, false);
            add(ToolAction::TableColAdd, "+Col", 52.0, false);
            add(ToolAction::TableColDel, "−Col", 52.0, false);
            add(ToolAction::TableDelete, "✕", 30.0, false);
        } else {
            add(ToolAction::InsertTable, "⊞ Table", 78.0, false);
        }
        add(ToolAction::InsertImage, "🖼 Image", 80.0, false);
        add(ToolAction::ToggleStyles, "Styles", 64.0, panel_open);
        drop(add);

        let items: Vec<ToolbarItem> = specs.iter().map(|(_, item)| item.clone()).collect();

        // Responsive packing: when the controls don't fit `tb.width()`, wrap to
        // additional rows so nothing is ever clipped or unreachable. The packer's
        // width model mirrors `chrome::toolbar::draw`, so each row-slice it admits
        // is drawn by `draw` without overflow. `tb` is the FULL multi-row toolbar
        // rect (its height already reserves every row — see `toolbar_height`).
        let widths: Vec<f64> = items.iter().map(|it| it.width).collect();
        let starts: Vec<bool> = items.iter().map(|it| it.group_start).collect();
        let rows = pack_toolbar_rows(&widths, &starts, tb.width());
        self.toolbar_rows = rows.len();

        // Fill the whole toolbar background once (so inter-row gaps are toned
        // consistently), then draw each row's buttons at its own band.
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &tb);

        let row_h = TOOLBAR_H;
        let mut all_rects: Vec<Rect> = Vec::with_capacity(items.len());
        for (ri, (s, e)) in rows.iter().enumerate() {
            let ry0 = tb.y0 + ri as f64 * row_h;
            let row_rect = Rect::new(tb.x0, ry0, tb.x1, ry0 + row_h);
            let rects = chrome::toolbar::draw(scene, transform, tr, theme, row_rect, &items[*s..*e]);
            all_rects.extend(rects);
        }
        self.toolbar = specs.into_iter().map(|(a, _)| a).zip(all_rects).collect();

        // Per-run text-colour swatch row, appended at the right of the LAST
        // toolbar row. The chrome toolbar only draws text buttons, so the coloured
        // squares are painted directly here (and registered as `TextColor` click
        // targets) — no `ui` change needed. The leading neutral swatch clears the
        // run colour.
        let last_row_y = tb.y0 + (rows.len() as f64 - 1.0) * row_h;
        let swatch_band = Rect::new(tb.x0, last_row_y, tb.x1, last_row_y + row_h);
        self.draw_color_swatches(scene, transform, tr, swatch_band);

        // Subtle hover highlight over the button under the pointer.
        if let Some(hr) = self.hover_tool {
            let pad = Rect::new(hr.x0 - 2.0, hr.y0 + 1.0, hr.x1 + 2.0, hr.y1 - 1.0);
            scene.fill(Fill::NonZero, transform, Color::from_rgba8(0xff, 0xff, 0xff, 0x14), None, &vello::kurbo::RoundedRect::from_rect(pad, 6.0));
        }
    }

    /// The toolbar's full height for the current window width — `TOOLBAR_H` per
    /// wrapped row. Computed from the same packer `draw_toolbar` uses so the
    /// canvas top offset and the drawn rows always agree.
    fn toolbar_height(&self, width: f64) -> f64 {
        let widths: Vec<f64> = self.toolbar_item_widths();
        let starts: Vec<bool> = self.toolbar_item_group_starts();
        let rows = pack_toolbar_rows(&widths, &starts, width);
        rows.len() as f64 * TOOLBAR_H
    }

    /// The per-item widths fed to the toolbar packer. Mirrors the `add(...)`
    /// sequence in `draw_toolbar`; the contextual table/cell controls depend on
    /// the caret block kind, so this resolves the same branch.
    fn toolbar_item_widths(&self) -> Vec<f64> {
        let (kind, _) = self.current_block_style();
        let mut w = vec![
            34.0, 30.0, 32.0, 30.0, // B I U S
            42.0, 42.0, 58.0, 66.0, // H1 H2 Body • List
            30.0, 30.0, 30.0, 30.0, // L C R J
            58.0, // font
            34.0, 40.0, 34.0, // A− value A+
        ];
        if kind == "cell" {
            w.extend([54.0, 54.0, 52.0, 52.0, 30.0]); // +Row −Row +Col −Col ✕
        } else {
            w.push(78.0); // ⊞ Table
        }
        w.push(80.0); // 🖼 Image
        w.push(64.0); // Styles
        w
    }

    /// The per-item `group_start` flags, parallel to `toolbar_item_widths`.
    fn toolbar_item_group_starts(&self) -> Vec<bool> {
        let (kind, _) = self.current_block_style();
        // Indices that start a group in `draw_toolbar` (see `group_start`).
        let mut g = vec![
            false, false, false, false, // B I U S
            true, false, false, false, // H1(group) H2 Body • List
            true, false, false, false, // L(group) C R J
            true, // font(group)
            false, false, false, // size stepper
        ];
        if kind == "cell" {
            g.extend([true, false, false, false, false]); // +Row starts a group
        } else {
            g.push(true); // ⊞ Table starts a group
        }
        g.push(true); // 🖼 Image starts a group
        g.push(true); // Styles starts a group
        g
    }

    /// Draw the text-colour swatch row at the right edge of the toolbar and
    /// register each swatch as a `TextColor` click target in `self.toolbar`.
    /// The current run's colour gets a white selection ring. The first swatch is
    /// the neutral/default (an `A` glyph on the surface colour) — it clears any
    /// per-run `color` mark.
    fn draw_color_swatches(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, tb: Rect) {
        let cur = self.current_text_color();
        let cy = (tb.y0 + tb.y1) / 2.0;
        let sw = 20.0;
        let gap = 5.0;
        let n = STYLE_PALETTE.len() + 1; // +1 for the neutral default
        let total = n as f64 * sw + (n as f64 - 1.0) * gap;
        // Right-align the strip, leaving a small margin from the toolbar's edge.
        let mut x = (tb.x1 - 14.0 - total).max(tb.x0 + 14.0);
        let y0 = cy - sw / 2.0;

        // Neutral / default swatch: surface-toned square with an "A", clears colour.
        let nr = Rect::new(x, y0, x + sw, y0 + sw);
        scene.fill(Fill::NonZero, transform, Color::from_rgb8(0x3a, 0x3d, 0x46), None, &vello::kurbo::RoundedRect::from_rect(nr, 4.0));
        if cur.is_none() {
            scene.stroke(&vello::kurbo::Stroke::new(2.0), transform, Color::WHITE, None, &vello::kurbo::RoundedRect::from_rect(nr, 4.0));
        }
        tr.draw_text(scene, "A", 13.0, Color::from_rgb8(0xd6, 0xd8, 0xde), transform * Affine::translate((x + 5.5, y0 + 2.0)), None);
        self.toolbar.push((ToolAction::TextColor(None), nr));
        x += sw + gap;

        for c in STYLE_PALETTE.iter() {
            let r = Rect::new(x, y0, x + sw, y0 + sw);
            let col = Color::from_rgb8(c[0], c[1], c[2]);
            scene.fill(Fill::NonZero, transform, col, None, &vello::kurbo::RoundedRect::from_rect(r, 4.0));
            if cur == Some(*c) {
                scene.stroke(&vello::kurbo::Stroke::new(2.0), transform, Color::WHITE, None, &vello::kurbo::RoundedRect::from_rect(r, 4.0));
            }
            self.toolbar.push((ToolAction::TextColor(Some(*c)), r));
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
    use taffy::AvailableSpace;
    use ui::permissions::PermissionContext;

    fn pipeline_draw(wp: &mut WordProcessorWidget, state: &mut ui::State, w: u32, h: u32) {
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
                Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) },
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
        wp.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
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

    #[test]
    fn layout_cache_reuses_unchanged_blocks_and_reshapes_after_edit() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "First paragraph here");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "Second paragraph here");

        // Cold frame: the cache is empty, so every block is shaped (a miss).
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.dbg_layout_hits, 0, "cold draw cannot reuse anything");
        assert_eq!(wp.dbg_layout_misses, 2, "both paragraphs shaped on the first frame");
        assert_eq!(wp.layout_cache.len(), 2, "both blocks cached");

        // Warm frame, no edits: every block's key matches, so all are reused and
        // nothing is re-shaped — this is the per-frame O(n) shaping we eliminated.
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.dbg_layout_hits, 2, "unchanged re-draw reuses every cached layout");
        assert_eq!(wp.dbg_layout_misses, 0, "no re-shaping on an unchanged frame");

        // Edit one block (caret is in the 2nd paragraph): only that block's key
        // changes, so only it re-shapes; the untouched block is still reused.
        wp.model_mut().insert_str(&mut state.doc, "!");
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.dbg_layout_misses, 1, "only the edited block is re-shaped");
        assert_eq!(wp.dbg_layout_hits, 1, "the untouched block is still reused");

        // A margin change alters the wrap width (the page is fixed-width, so the
        // window size does not — only the margins do). Wrap width is part of the
        // key, so every block must re-shape at the new measure.
        wp.margins = Edges::uniform(140.0);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.dbg_layout_misses, 2, "a wrap-width change re-shapes every block");
        assert_eq!(wp.dbg_layout_hits, 0, "no block can be reused at a new wrap width");
    }

    #[test]
    fn find_and_replace_all_works() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "hello world hello there");
        wp.refresh_blocks(&state.doc);

        wp.find_query = "hello".to_string();
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 2, "case-insensitive search finds both");

        wp.replace_query = "hi".to_string();
        wp.replace_all(&mut state.doc);
        wp.refresh_blocks(&state.doc);
        assert_eq!(wp.blocks[0].text, "hi world hi there");
        assert!(wp.find_matches.is_empty(), "no matches remain after replace-all");
    }

    #[test]
    fn styles_panel_renders_hits_and_picks_a_style() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "A paragraph");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // The panel is open by default and renders clickable targets.
        assert!(wp.styles_panel_open);
        assert!(!wp.panel_hits.is_empty(), "panel rendered clickable targets");
        assert_eq!(wp.focused_style_key().as_deref(), Some("body"));

        // Picking the Quote style makes the caret block adopt it (component-instance).
        wp.apply_panel_hit(&mut state.doc, PanelHit::Pick("quote".into()));
        wp.refresh_blocks(&state.doc);
        assert_eq!(wp.focused_style_key().as_deref(), Some("quote"), "block adopted the picked style");
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
        assert_eq!(after, before + 5.0, "edited style size persisted to the document");

        // Both paragraphs are Body, so both re-render at the new size.
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert_eq!(wp.laid.len(), 2);
        assert!(wp.laid.iter().all(|l| (l.size - after).abs() < 0.01), "every Body block follows the style edit");
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
        assert_eq!(wp.model().stylesheet(&state.doc).get("body").size, 40.0, "typed value persisted");
    }

    #[test]
    fn new_style_from_this_duplicates_and_applies() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "para");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let n_before = wp.sheet.order().len();
        wp.apply_panel_hit(&mut state.doc, PanelHit::NewStyle);
        let sheet = wp.model().stylesheet(&state.doc);
        assert_eq!(sheet.order().len(), n_before + 1, "a new style id was added");
        wp.refresh_blocks(&state.doc);
        let key = wp.focused_style_key().unwrap();
        assert!(key.starts_with("custom"), "block adopted the duplicated style, got {key}");
    }

    #[test]
    fn styles_panel_toggles_off() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        assert!(wp.styles_panel_open, "panel defaults open");
        wp.apply_tool(&mut state.doc, ToolAction::ToggleStyles);
        assert!(!wp.styles_panel_open, "toolbar toggle dismisses the panel");
    }

    #[test]
    fn draw_renders_headings_and_bold() {
        use qdoc::{FormatProvider, MarkdownCodec};
        let mut state = ui::State::new();
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
            MarkdownCodec.import(b"# Title\n\nHello **bold** world\n", &mut dm).unwrap();
        }
        let mut wp = WordProcessorWidget::new();
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(wp.laid.len() >= 2);
        assert!(wp.blocks.iter().any(|b| b.kind == "heading"));
        assert!(wp.blocks.iter().any(|b| b.marks.iter().any(|m| m.name == "strong")));
    }

    /// Build a doc directly in `state` from a closure over a `DocMut`.
    fn with_doc(state: &mut ui::State, f: impl FnOnce(&mut qdoc::DocMut)) {
        let c = qdoc::model::init(&mut state.doc, &state.root);
        let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
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
        assert!(lb.text_x > la.text_x && lc.text_x > lb.text_x, "deeper levels indent further");
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
            for (txt, lvl) in [("one", 0i64), ("two", 0), ("two-a", 1), ("two-b", 1), ("three", 0)] {
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
        let num = |t: &str| wp.laid.iter().find(|l| l.text == t).and_then(|l| l.bullet.clone());
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
            wp.model_mut().insert_str(&mut state.doc, &format!("Paragraph number {i} carries enough words to fill a line."));
            wp.model_mut().enter(&mut state.doc);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // The canvas top sits below the (wrapped) toolbar — the real offset the
        // pagination cursor starts from.
        let page_top = wp.toolbar_height(1000.0) + PAGE_TOP;
        let pct = |i: usize| page_top + i as f64 * page_pitch + mar; // page content top
        let pcb = |i: usize| page_top + i as f64 * page_pitch + letter_h - mar; // bottom margin
        let page_of = |y: f64| ((y - page_top) / page_pitch).floor() as i64;

        // The content spans more than one page.
        let max_page = wp.laid.iter().map(|l| page_of(l.y_top)).max().unwrap();
        assert!(max_page >= 1, "a long document must occupy more than one page (got {max_page})");

        // No laid block crosses a page boundary: each sits wholly within its page's
        // text band, so a page-end never bisects a line of text.
        for l in &wp.laid {
            let i = page_of(l.y_top).max(0) as usize;
            assert!(l.y_top >= pct(i) - 1.0, "block starts at/below its page's top margin");
            assert!(l.y_top + l.height <= pcb(i) + 1.0, "block ends above its page's bottom margin (no straddle)");
        }

        // The first block that lands on page 2 starts exactly at the page-2 top
        // margin, and the block before it (in document order) is still on page 1.
        let idx = wp.laid.iter().position(|l| page_of(l.y_top) == 1).expect("some block lands on page 2");
        assert!(idx > 0, "page 2 is preceded by page-1 content");
        assert!((wp.laid[idx].y_top - pct(1)).abs() < 1.0, "the first page-2 block hugs the page-2 top margin");
        assert_eq!(page_of(wp.laid[idx - 1].y_top), 0, "the block before it is on page 1");
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
            let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
            let f = dm.insert_block_in(qdoc::model::Region::Footer, None, "paragraph");
            dm.splice_text_in(qdoc::model::Region::Footer, &f, 0, 0, "Page ");
            dm.insert_page_number_field(qdoc::model::Region::Footer, &f, 5);
        }
        // Body long enough to span two pages.
        for i in 0..120 {
            wp.model_mut().insert_str(&mut state.doc, &format!("Paragraph number {i} carries enough words to fill a line."));
            wp.model_mut().enter(&mut state.doc);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // The footer is loaded from the snapshot and spans >1 page.
        assert_eq!(wp.footer.len(), 1, "the footer block is materialized into the widget");
        let max_page = wp.laid.iter().map(|l| ((l.y_top + wp.scroll_y - (wp.toolbar_height(1000.0) + PAGE_TOP)) / (MAX_PAGE_W * LETTER_RATIO + PAGE_GAP)).floor() as i64).max().unwrap();
        assert!(max_page >= 1, "the document spans more than one page (got {max_page})");

        // The production substitution path renders the page number per page: the
        // joined run text for the footer block is "Page 1" on page 1, "Page 2" on
        // page 2 — the placeholder is gone and the digits are present.
        let join = |page_no: usize| running_head_runs(&wp.footer[0], page_no).iter().map(|r| r.text.clone()).collect::<String>();
        assert_eq!(join(1), "Page 1", "page 1 footer resolves to 'Page 1'");
        assert_eq!(join(2), "Page 2", "page 2 footer resolves to 'Page 2'");
        assert!(!join(2).contains(qdoc::schema::FIELD_PLACEHOLDER), "the field placeholder is substituted, not rendered literally");
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
        wp.model_mut().insert_str(&mut state.doc, "First paragraph on page one.");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "Second paragraph still on page one.");
        // Ctrl+Enter behaviour: split + mark the new block to break before.
        wp.model_mut().insert_page_break(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "This paragraph is forced onto page two.");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let page_top = wp.toolbar_height(1000.0) + PAGE_TOP;
        let pct = |i: usize| page_top + i as f64 * page_pitch + mar; // page content top
        let page_of = |y: f64| ((y - page_top) / page_pitch).floor() as i64;

        let broken = wp.laid.iter().find(|l| l.text.starts_with("This paragraph is forced")).expect("the broken block is laid out");
        assert_eq!(page_of(broken.y_top), 1, "the page-break block lands on page 2");
        assert!((broken.y_top - pct(1)).abs() < 1.0, "it hugs the page-2 top margin (laid at a page top)");

        // The two earlier paragraphs are still on page 1 (the break was manual, not
        // an overflow): proving the attr — not a full page of text — caused it.
        let first = wp.laid.iter().find(|l| l.text.starts_with("First paragraph")).unwrap();
        let second = wp.laid.iter().find(|l| l.text.starts_with("Second paragraph")).unwrap();
        assert_eq!(page_of(first.y_top), 0, "the first paragraph stays on page 1");
        assert_eq!(page_of(second.y_top), 0, "the second paragraph stays on page 1");
    }

    #[test]
    fn page_margins_persist_across_reload() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // A document with no recorded page setup uses the default margins.
        assert!((wp.margins.left - 72.0).abs() < 1e-6, "fresh doc uses default margins");

        // Edit margins (as a margin-guide drag would) and persist via the same
        // production path the drag-release handler uses.
        wp.margins = Edges { top: 33.0, right: 44.0, bottom: 55.0, left: 66.0 };
        wp.persist_margins(&mut state.doc);

        // A fresh widget bound to the *same document* reloads the persisted page
        // setup on sync — proving margins now live in the document, not the widget.
        let mut wp2 = WordProcessorWidget::new();
        wp2.sync(&mut state.doc, &state.root);
        assert!((wp2.margins.top - 33.0).abs() < 1e-6, "top margin survived reload");
        assert!((wp2.margins.right - 44.0).abs() < 1e-6, "right margin survived reload");
        assert!((wp2.margins.bottom - 55.0).abs() < 1e-6, "bottom margin survived reload");
        assert!((wp2.margins.left - 66.0).abs() < 1e-6, "left margin survived reload");
    }

    /// Inserting an image via the model and running the full draw pipeline lays
    /// out an `image` block and does not panic (decode → layout → draw).
    #[test]
    fn inserted_image_lays_out_and_draws() {
        // A minimal 2×3 RGB PNG (matches the model-test fixture).
        const TINY_PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2,
            0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
            159, 1, 140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174,
            66, 96, 130,
        ];
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "caption above");
        wp.model_mut().insert_image_bytes(&mut state.doc, TINY_PNG);
        // Re-sync so the widget's block/blob caches see the new image block.
        wp.sync(&mut state.doc, &state.root);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(wp.laid.iter().any(|l| l.kind == "image"), "an image block was laid out");
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
        assert!((id_ctf.as_coeffs()[0] - 1.0).abs() < 1e-9, "100% zoom does not scale");

        // Zoom in: the content transform now scales by `zoom` about the anchor,
        // so a point at the anchor is fixed and points below it move down.
        assert!(wp.set_zoom(2.0));
        let ctf = wp.content_transform(base);
        let coeffs = ctf.as_coeffs();
        assert!((coeffs[0] - 2.0).abs() < 1e-9 && (coeffs[3] - 2.0).abs() < 1e-9, "zoom doubles the scale");
        // The anchor maps to itself.
        let a = wp.zoom_anchor;
        let mapped_anchor = ctf * a;
        assert!((mapped_anchor.x - a.x).abs() < 1e-6 && (mapped_anchor.y - a.y).abs() < 1e-6, "the anchor is fixed under zoom");
        // A point one content-unit below the anchor lands two screen-units below.
        let p = Point::new(a.x, a.y + 100.0);
        let mp = ctf * p;
        assert!((mp.y - (a.y + 200.0)).abs() < 1e-6, "content below the anchor is magnified downward");
    }

    #[test]
    fn click_maps_to_the_same_caret_under_zoom() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "first paragraph");
        wp.model_mut().enter(&mut state.doc);
        wp.model_mut().insert_str(&mut state.doc, "second paragraph");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        // A content-space point inside the second block (its `Laid` rect lives in
        // unscaled content coordinates regardless of zoom).
        let l2 = wp.laid.iter().find(|l| l.text == "second paragraph").expect("2nd block laid out");
        let target_block = l2.id.clone();
        let content_pt = Point::new(l2.text_x + 30.0, l2.y_top + l2.height / 2.0);

        // Sanity: clicking that content point lands the caret in the 2nd block.
        wp.caret_from_pointer(content_pt, false);
        assert_eq!(wp.inner.caret.block, target_block, "baseline content-space click hits the 2nd block");
        let off_unzoomed = wp.inner.caret.offset;

        // Now zoom to 175%. The screen position of that same content point is the
        // content transform applied to it. Feeding the screen point back through
        // the production inverse (`pointer_to_content`) must recover the click,
        // so `handle_event`'s caret hit-test lands on the same block + offset.
        assert!(wp.set_zoom(1.75));
        pipeline_draw(&mut wp, &mut state, 1000, 700); // recompute `zoom_anchor`
        let screen_pt = wp.content_transform(Affine::IDENTITY) * content_pt;
        let recovered = wp.pointer_to_content(screen_pt);
        assert!((recovered.x - content_pt.x).abs() < 1e-6 && (recovered.y - content_pt.y).abs() < 1e-6, "inverse recovers the content point");
        wp.caret_from_pointer(recovered, false);
        assert_eq!(wp.inner.caret.block, target_block, "zoomed click still hits the 2nd block");
        assert_eq!(wp.inner.caret.offset, off_unzoomed, "zoomed click lands on the same caret offset");
    }

    #[test]
    fn zoom_clamps_and_resets() {
        let mut wp = WordProcessorWidget::new();
        assert!((wp.zoom - 1.0).abs() < 1e-9, "default zoom is 100%");
        // Clamp at the bounds.
        for _ in 0..100 {
            wp.zoom_by(-ZOOM_STEP);
        }
        assert!((wp.zoom - ZOOM_MIN).abs() < 1e-6, "zoom clamps to the minimum");
        for _ in 0..100 {
            wp.zoom_by(ZOOM_STEP);
        }
        assert!((wp.zoom - ZOOM_MAX).abs() < 1e-6, "zoom clamps to the maximum");
        wp.set_zoom(1.0);
        assert!((wp.zoom - 1.0).abs() < 1e-9, "reset returns to 100%");
    }

    #[test]
    fn zoom_in_grows_the_scrollable_range() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        for i in 0..40 {
            wp.model_mut().insert_str(&mut state.doc, &format!("Paragraph {i} with several words to fill the line."));
            wp.model_mut().enter(&mut state.doc);
        }
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        let scroll_1x = wp.max_scroll;
        assert!(scroll_1x > 0.0, "the document already overflows at 100%");
        wp.set_zoom(2.0);
        pipeline_draw(&mut wp, &mut state, 1000, 700);
        assert!(wp.max_scroll > scroll_1x, "zooming in enlarges the content, so there is more to scroll");
    }

    // ---- Find toggles -------------------------------------------------------

    #[test]
    fn find_case_sensitive_excludes_wrong_case() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Hello hello HELLO");
        wp.refresh_blocks(&state.doc);

        // Case-insensitive (default): all three forms match.
        wp.find_query = "hello".to_string();
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 3, "case-insensitive finds every form");

        // Case-sensitive: only the exact-case "hello" matches.
        wp.find_case = true;
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 1, "case-sensitive excludes Hello and HELLO");
        let (bid, s, e) = wp.find_matches[0].clone();
        let block = wp.blocks.iter().find(|b| b.id == bid).unwrap();
        let matched: String = block.text.chars().skip(s).take(e - s).collect();
        assert_eq!(matched, "hello", "the single match is the exact-case form");
    }

    #[test]
    fn find_whole_word_excludes_substrings() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "cat category catalog cat");
        wp.refresh_blocks(&state.doc);

        wp.find_query = "cat".to_string();
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 4, "substring search matches inside category/catalog too");

        wp.find_word = true;
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 2, "whole-word matches only the standalone 'cat' tokens");
    }

    #[test]
    fn find_regex_matches_and_bad_pattern_is_safe() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "a1 b22 c333");
        wp.refresh_blocks(&state.doc);

        wp.find_regex = true;
        wp.find_query = r"\d+".to_string();
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 3, "regex finds each digit run");
        assert!(!wp.find_regex_error, "a valid pattern records no error");

        // An invalid regex must not panic; it records the error and yields nothing.
        wp.find_query = "(".to_string();
        wp.find_recompute(&state.doc);
        assert!(wp.find_matches.is_empty(), "an invalid pattern yields no matches");
        assert!(wp.find_regex_error, "the invalid pattern is flagged");

        // Toggling regex back off restores literal substring matching of "(".
        wp.find_regex = false;
        wp.model_mut().insert_str(&mut state.doc, ""); // no-op keep caret
        wp.find_query = "1".to_string();
        wp.find_recompute(&state.doc);
        assert!(!wp.find_regex_error, "leaving regex mode clears the error flag");
        assert_eq!(wp.find_matches.len(), 1, "literal search resumes");
    }

    #[test]
    fn find_toggle_actions_recompute() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        wp.model_mut().insert_str(&mut state.doc, "Word word");
        wp.refresh_blocks(&state.doc);
        wp.find_query = "word".to_string();
        wp.find_recompute(&state.doc);
        assert_eq!(wp.find_matches.len(), 2);

        // The production toggle action both flips the flag and recomputes.
        wp.find_action(&mut state.doc, FindAction::ToggleCase);
        assert!(wp.find_case, "the action flipped the case flag");
        assert_eq!(wp.find_matches.len(), 1, "recompute ran with the new flag");
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
        assert!(rows.len() > 1, "items exceeding the width wrap to >1 row (got {})", rows.len());
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
            assert!(x <= usable + 1.0 || e - s == 1, "row [{s},{e}) overflows usable width");
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
        assert!(narrow > wide, "a narrow window reserves a taller (wrapped) toolbar");
        assert!((wide - TOOLBAR_H).abs() < 1e-9, "a wide window keeps a single-row toolbar");
        assert_eq!(narrow % TOOLBAR_H, 0.0, "the toolbar height is a whole number of rows");
        assert!(narrow >= 2.0 * TOOLBAR_H, "760px wraps to at least two rows");
    }

    // ---- spell-check squiggles --------------------------------------------

    #[test]
    fn misspelled_word_yields_a_squiggle_span_at_its_x_range() {
        let mut state = ui::State::new();
        let mut wp = WordProcessorWidget::new();
        wp.sync(&mut state.doc, &state.root);
        // "teh" is a classic misspelling flagged by the builtin dictionary; the
        // surrounding words are valid, so exactly one span is expected.
        wp.model_mut().insert_str(&mut state.doc, "teh quick brown fox");
        pipeline_draw(&mut wp, &mut state, 1000, 700);

        let l = wp.laid.iter().find(|l| l.text == "teh quick brown fox").expect("paragraph laid out");
        let id = l.id.clone();
        let text = l.text.clone();

        // The production spell path computes the misspelled byte spans for this
        // block (the same call `draw` makes to feed `draw_squiggle`).
        let spans = wp.block_spell_spans(&id, &text);
        assert_eq!(spans, vec![(0usize, 3usize)], "exactly ‘teh’ at bytes [0,3) is flagged");

        // The span maps to a concrete, positive on-screen x-range via the cached
        // shaped layout — exactly the geometry `draw` passes to `draw_squiggle`.
        let (_, layout) = wp.layout_cache.get(&id).expect("block layout cached after draw");
        let rects = layout.selection_rects(0, 3);
        assert!(!rects.is_empty(), "the misspelled span resolves to at least one rect");
        let (x0, _, x1, _) = rects[0];
        assert!(x1 > x0, "the squiggle covers a non-empty width: x0={x0} x1={x1}");
        // ‘teh’ sits at the very start of the line, so its rect hugs the left edge.
        assert!(x0 < 4.0, "the first word's squiggle starts at the line's left edge: x0={x0}");
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
        assert!(wp.dict.is_none(), "spell-check off skips dictionary construction");
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
        assert!(wp.block_spell_spans(&id, "world").is_empty(), "the corrected text re-checks clean");
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
            let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, body);
            // Place a footnote reference marker at `ref_idx` and fill the note body.
            let note_id = dm.insert_footnote(&p, ref_idx);
            let nb = dm.insert_note_block(&note_id, None, "paragraph").expect("note block");
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
        assert_eq!(wp.notes[0].kind, qdoc::schema::NOTE_KIND_FOOTNOTE, "it is a footnote");

        // The body block carries exactly one note-ref marker (the production scan
        // `draw` uses to place the superscript), and document-order numbering
        // assigns it citation number 1.
        let body = wp.blocks.iter().find(|b| b.text.contains("Hello")).expect("body block");
        let refs = block_note_refs(body);
        assert_eq!(refs.len(), 1, "one note-ref marker in the body");
        let (ref_ci, note_id) = &refs[0];
        assert_eq!(*ref_ci, 5, "marker sits at the reference index (the placeholder char)");
        let nums = note_numbering(&wp.blocks);
        assert_eq!(nums.get(note_id.as_str()), Some(&1), "first-referenced note is numbered 1");

        // The footnote text that the page note-area renders ("N. <text>") is laid
        // — assert the note text is present (not the rendered pixels).
        let text = note_text(&wp.notes[0]);
        assert_eq!(text, "A clarifying footnote.", "the footnote body text is materialized");
        let rendered_line = format!("{}. {}", nums[note_id.as_str()], text);
        assert_eq!(rendered_line, "1. A clarifying footnote.", "the page note-area line is well-formed");
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
            let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &c, ids: &mut ids };
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
        assert_eq!(nums.get(n_first.as_str()), Some(&1), "the earlier marker is #1 by document order");
        assert_eq!(nums.get(n_second.as_str()), Some(&2), "the later marker is #2");
    }
}
