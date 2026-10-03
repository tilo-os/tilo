//! The one layout the identity ceremony screens share.
//!
//! Each screen is a *sheet*: the modal chrome of [`modal_dialog`] — scrim,
//! centred card, title row and action row — with the screen's own rows
//! stacked inside [`ModalLayout::body`] on the spacing scale. Before this
//! module every screen placed its content at absolute offsets from a card
//! whose size it chose for itself (`card.y0 + 240.0`), which is how seven
//! screens grew seven card sizes, four button heights, a scrim that stopped
//! at 720×580 inside a 1280×800 window, and a URL that ran off the card.
//! Now the screen owns its viewport, the card is sized from what the rows
//! measure, the rows are sized from the text they carry, and every button
//! is a [`Button`].
//!
//! Immediate-mode on purpose: the screens own their hit-testing and their
//! protocol state, so they paint through the shared *faces*
//! ([`Checkbox::paint`], [`ToggleGroup::paint`], and `Button::paint` via
//! the modal action row) rather than hosting retained widgets — the same
//! door [`modal_dialog::layout`] itself uses. The one exception is text entry: a
//! field is the shared [`TextInput`], embedded the way `ModalPrompt` embeds
//! it — placed by [`place_field`] / [`place_code_cell`] from the row plan,
//! drawn and fed events by the screen — so IME, clipboard, selection,
//! accessibility and `test_id`s come from the one widget that carries
//! them rather than from a painted box. Geometry is resolved in
//! `apply_layout` (against [`with_layout_renderer`], since that pass has
//! no renderer of its own) and again in `draw`, from one [`resolve`] over
//! the same rows, so the rects an event hits are never a paint behind the
//! state.
//!
//! Rows that outrun the card — a large text scale, a small window — scroll
//! inside the body ([`BodyScroll`]) and paint under a clip, so nothing
//! lands on the action row and nothing becomes unreachable.

use std::cell::RefCell;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::checkbox::{Checkbox, CheckboxFace};
use crate::widget::modal_dialog::{self, ModalAction, ModalLayout, ModalSpec};
use crate::widget::qr::QrWidget;
use crate::widget::text_input::TextInput;
use crate::widget::toggle_group::{SegmentedFace, SegmentedStyle, ToggleGroup};

/// Widest a ceremony card with a form in it is drawn — HIG §9.6's "560 for a
/// sheet with a form in it".
pub(super) const FORM_SHEET_WIDTH: f64 = 560.0;

/// The two recovery screens lay 24 words in four columns; at 560 a long
/// word and its ordinal ("16. bachelor") run into the next column, so those
/// two share one wider size. A word grid is not the paragraph
/// `MODAL_MAX_WIDTH`'s 520 exists to keep narrow.
pub(super) const GRID_SHEET_WIDTH: f64 = 680.0;

/// Columns in the recovery-word grid, shared by the share and verify
/// screens so the two read as one surface with three blanks punched in it.
pub(super) const WORD_GRID_COLUMNS: usize = 4;

thread_local! {
    /// The renderer the layout pass measures with. `apply_layout` receives
    /// no text renderer, and a screen's rows are sized by the text they
    /// carry, so the geometry events hit-test against is measured here —
    /// with the same fonts the frame's renderer shapes — rather than
    /// borrowed from the previous paint. One per thread, shared by every
    /// screen; the font system is the expensive part and is loaded once.
    static LAYOUT_TEXT: RefCell<TextRenderer> = RefCell::new(TextRenderer::new());
}

/// Run `f` with the layout-pass renderer at `theme`'s text scale, so its
/// measurements match the frame renderer the harness and app keep in step
/// with the theme.
pub(super) fn with_layout_renderer<R>(theme: &Theme, f: impl FnOnce(&mut TextRenderer) -> R) -> R {
    LAYOUT_TEXT.with(|cell| {
        let mut tr = cell.borrow_mut();
        tr.set_text_scale(theme.text_scale);
        f(&mut tr)
    })
}

/// Vertical scroll of a body whose rows outrun the card
/// (`ModalLayout::body_overflows`). The offset is clamped every layout, so
/// a body that fits again after a state change snaps back to its top.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct BodyScroll {
    offset: f64,
    max: f64,
    /// What one wheel line moves: the control height the rows were laid
    /// out with, so a line of the wheel is a row of the body.
    line_px: f64,
}

impl BodyScroll {
    #[cfg(test)]
    pub(super) fn offset(&self) -> f64 {
        self.offset
    }

    /// How far the rows can scroll; zero when the body fits.
    #[cfg(test)]
    pub(super) fn max(&self) -> f64 {
        self.max
    }

    /// Place `wanted` px of rows inside `body`, clamping the offset to what
    /// overflows. Returns the rect the rows are laid into — as tall as the
    /// rows, starting `offset` above the body when scrolled.
    pub(super) fn place(&mut self, body: Rect, wanted: f64, line_px: f64) -> Rect {
        self.max = (wanted - body.height()).max(0.0);
        self.line_px = line_px.max(1.0);
        self.offset = self.offset.clamp(0.0, self.max);
        Rect::new(
            body.x0,
            body.y0 - self.offset,
            body.x1,
            body.y0 - self.offset + wanted.max(0.0),
        )
    }

    /// A wheel tick: positive `y` scrolls toward the top, matching the
    /// toolkit's scroll convention. `true` when the offset moved.
    pub(super) fn wheel(&mut self, delta: &MouseScrollDelta) -> bool {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.line_px,
            MouseScrollDelta::PixelDelta(p) => p.y,
        };
        self.scroll_by(-dy)
    }

    /// `PageUp` / `PageDown` move a body height's worth of rows, so a
    /// keyboard user reaches a row the wheel would.
    pub(super) fn key(&mut self, key: &Key, body_height: f64) -> bool {
        match key {
            Key::Named(NamedKey::PageDown) => self.scroll_by(body_height),
            Key::Named(NamedKey::PageUp) => self.scroll_by(-body_height),
            _ => false,
        }
    }

    fn scroll_by(&mut self, dy: f64) -> bool {
        if self.max <= 0.0 {
            return false;
        }
        let next = (self.offset + dy).clamp(0.0, self.max);
        let moved = (next - self.offset).abs() > f64::EPSILON;
        self.offset = next;
        moved
    }

    /// The part of a row that is inside the visible body — its pointer
    /// target. `Rect::ZERO` once it has scrolled out, so a row under the
    /// action buttons can never take their click.
    pub(super) fn visible(body: Rect, rect: Rect) -> Rect {
        let shown = rect.intersect(body);
        if shown.is_zero_area() {
            Rect::ZERO
        } else {
            shown
        }
    }
}

/// What a screen has once its rows are placed: the sheet, and its own plan
/// of rects inside the sheet's body.
pub(super) struct Resolved<P> {
    pub sheet: Sheet,
    pub plan: P,
}

/// The one geometry pass, called from `apply_layout` and again from `draw`.
///
/// `plan` walks the screen's rows once over a cursor and returns its rects
/// plus the cursor's bottom; it is run unplaced to learn the body's height,
/// then over the placed body. `control_h` comes from a probe layout so a
/// field is one action button tall.
pub(super) fn resolve<P>(
    viewport: Rect,
    spec: &SheetSpec<'_>,
    scroll: &mut BodyScroll,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    mut plan: impl FnMut(Rows, f64, &mut TextRenderer, &Theme) -> (P, f64),
) -> Resolved<P> {
    let probe = Sheet::layout(viewport, spec, 0.0, text_renderer, theme);
    let control_h = probe.control_height(theme);
    let body_w = Sheet::body_width(viewport, spec.max_width, theme);
    let (_, wanted) = plan(Rows::measuring(body_w), control_h, text_renderer, theme);
    let sheet = Sheet::layout(viewport, spec, wanted, text_renderer, theme);
    let placed = scroll.place(sheet.body(), wanted, control_h);
    let (plan, _) = plan(Rows::new(placed), control_h, text_renderer, theme);
    Resolved { sheet, plan }
}

/// Type roles a sheet body uses. Each resolves to one size, weight and ink,
/// so two screens never disagree about what a label looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    /// The explanation under the title.
    Body,
    /// The name above a field or a group of controls.
    Label,
    /// A value the user typed or is reading back.
    Value,
    /// Status lines, hints, the clipboard confirmation.
    Caption,
    /// A validation or protocol failure.
    Error,
    /// The pairing code: large, so it can be read across a desk.
    Code,
}

impl Role {
    /// The theme role this sheet role is set in.
    pub(super) fn text_role(self) -> TextRole {
        match self {
            Role::Body | Role::Value => TextRole::Body,
            Role::Label => TextRole::Label,
            Role::Caption | Role::Error => TextRole::Caption,
            Role::Code => TextRole::LargeTitle,
        }
    }

    /// The resolved style: the theme role, with tabular figures for the
    /// pairing code.
    pub(super) fn style(self, theme: &Theme) -> TextStyle {
        let style = theme.text(self.text_role());
        if self == Role::Code {
            style.numeric()
        } else {
            style
        }
    }

    /// Ink that is legible on the card, from the same derivation the modal
    /// title uses, so a palette where `text_primary` fails against the card
    /// fixes every sheet at once.
    pub(super) fn color(self, theme: &Theme) -> Color {
        let (ink, dim) = modal_dialog::ink(theme);
        match self {
            Role::Value | Role::Code => ink,
            Role::Body | Role::Label | Role::Caption => dim,
            Role::Error => theme.tokens.colors.semantic.danger,
        }
    }
}

/// Horizontal placement of a line inside its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Align {
    Start,
    Center,
}

/// A top-down cursor over the body: rows take their measured height, gaps
/// come from the spacing scale, nothing is placed at an absolute offset.
/// Running the same sequence once over an unplaced body yields the body's
/// height; running it again over [`ModalLayout::body`] yields the rects.
#[derive(Clone, Copy, Debug)]
pub(super) struct Rows {
    x0: f64,
    x1: f64,
    y: f64,
}

impl Rows {
    pub(super) fn new(body: Rect) -> Self {
        Self {
            x0: body.x0,
            x1: body.x1,
            y: body.y0,
        }
    }

    /// An unplaced body of `width`, for the measuring pass.
    pub(super) fn measuring(width: f64) -> Self {
        Self::new(Rect::new(0.0, 0.0, width.max(0.0), 0.0))
    }

    pub(super) fn width(&self) -> f64 {
        (self.x1 - self.x0).max(0.0)
    }

    /// Take the next `height` and advance.
    pub(super) fn row(&mut self, height: f64) -> Rect {
        let r = Rect::new(self.x0, self.y, self.x1, self.y + height.max(0.0));
        self.y = r.y1;
        r
    }

    /// Skip a spacing-token gap.
    pub(super) fn gap(&mut self, px: f32) {
        self.y += f64::from(px.max(0.0));
    }

    /// Where the next row would start; after the last row, the body's
    /// height when the cursor was [`Self::measuring`].
    pub(super) fn bottom(&self) -> f64 {
        self.y
    }
}

/// What a screen asks the sheet to frame. The action row is the screen's
/// primary/secondary buttons, so they are laid out and painted by the modal
/// chrome — measured widths, platform order, one control height.
pub(super) struct SheetSpec<'a> {
    pub title: &'a str,
    pub max_width: f64,
    pub actions: &'a [ModalAction<'a>],
}

impl<'a> SheetSpec<'a> {
    fn modal(&self, body_height: f64) -> ModalSpec<'a> {
        ModalSpec::new(self.title)
            .actions(self.actions)
            .max_width(self.max_width)
            .closable(false)
            .dismiss_on_scrim(false)
            .body_height(body_height)
    }
}

/// The resolved sheet: the modal layout plus what the body rows need to
/// know about it.
#[derive(Clone, Debug)]
pub(super) struct Sheet {
    pub layout: ModalLayout,
}

impl Sheet {
    /// Width the body rows will be laid into, knowable before any row is
    /// measured: the card's width depends on the viewport and the sheet's
    /// maximum, never on its height.
    pub(super) fn body_width(viewport: Rect, max_width: f64, theme: &Theme) -> f64 {
        modal_dialog::body_width(viewport, max_width, theme)
    }

    /// Resolve the sheet for a body that measured `body_height` tall.
    pub(super) fn layout(
        viewport: Rect,
        spec: &SheetSpec<'_>,
        body_height: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Self {
        let modal = spec.modal(body_height);
        Self {
            layout: modal_dialog::layout(viewport, &modal, text_renderer, theme),
        }
    }

    pub(super) fn body(&self) -> Rect {
        self.layout.body
    }

    /// Start clipping to the body, so scrolled rows never paint over the
    /// title or the action row. Pair with `scene.pop_layer()`.
    pub(super) fn clip_body(&self, scene: &mut Scene, transform: Affine) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.layout.body,
        );
    }

    /// The action button at `index` in the spec's order; `Rect::ZERO` when
    /// the spec had none there.
    pub(super) fn action(&self, index: usize) -> Rect {
        self.layout
            .actions
            .get(index)
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    /// Height of a control in the body — the same height the action row's
    /// buttons take, read from the layout rather than restated, so a field
    /// and the button under it are one row tall. A sheet without actions
    /// falls back to the modal's own control size.
    pub(super) fn control_height(&self, theme: &Theme) -> f64 {
        self.layout
            .actions
            .iter()
            .map(Rect::height)
            .find(|h| *h > 0.0)
            .unwrap_or_else(|| modal_dialog::action_height(theme))
    }

    /// Scrim, card, title. Paint before the body.
    pub(super) fn paint_frame(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        spec: &SheetSpec<'_>,
    ) {
        let modal = spec.modal(0.0);
        modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &self.layout, &modal);
    }

    /// The action row. Paint after the body.
    pub(super) fn paint_actions(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        spec: &SheetSpec<'_>,
    ) {
        let modal = spec.modal(0.0);
        modal_dialog::paint_actions(scene, transform, text_renderer, theme, &self.layout, &modal);
    }
}

/// Height one line of `role` occupies, measured with the style it is drawn
/// in — never a literal, so a text-scale theme grows the row with the text.
pub(super) fn line_height(text_renderer: &mut TextRenderer, theme: &Theme, role: Role) -> f64 {
    let (_, h) = text_renderer.measure_style("Ag", None, &role.style(theme));
    f64::from(h)
}

/// Measure `text` in `role`, wrapped to `max_width` when given.
pub(super) fn measure(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    role: Role,
    text: &str,
    max_width: Option<f64>,
) -> (f64, f64) {
    let (w, h) = text_renderer.measure_style(text, max_width.map(narrow), &role.style(theme));
    (f64::from(w), f64::from(h))
}

/// A wrapped paragraph's height at `width` — what a body row reserves.
pub(super) fn paragraph_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    role: Role,
    text: &str,
    width: f64,
) -> f64 {
    measure(text_renderer, theme, role, text, Some(width)).1
}

/// Paint `text` in `role`, its measured line box vertically centred in
/// `rect`, wrapped to the rect's width when `wrap`. Measured and drawn with
/// one style, so the centring is exact rather than `(h - 13.0) / 2.0`.
pub(super) fn paint_text(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    text: &str,
    role: Role,
    align: Align,
    wrap: bool,
) {
    let color = role.color(ctx.theme);
    paint_text_in(ctx, rect, text, role, color, align, wrap);
}

/// [`paint_text`] with an explicit ink, for a placeholder or a value on a
/// fill the role's ink was not derived against.
pub(super) fn paint_text_in(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    text: &str,
    role: Role,
    color: Color,
    align: Align,
    wrap: bool,
) {
    if text.is_empty() || rect.width() <= 0.0 {
        return;
    }
    let max_width = wrap.then_some(rect.width());
    let (w, h) = measure(ctx.text, ctx.theme, role, text, max_width);
    let x = match align {
        Align::Start => rect.x0,
        Align::Center => rect.x0 + (rect.width() - w) / 2.0,
    };
    let y = rect.y0 + (rect.height() - h) / 2.0;
    let style = role.style(ctx.theme);
    ctx.text(text, &style, color, Point::new(x, y), max_width.map(narrow));
}

/// Seat an embedded [`TextInput`] in the row the plan gave it. The field
/// keeps the shared widget's own chrome (raised fill, `radii.control`, the
/// focus edge, the standard text inset); the sheet only decides where it
/// goes and that its text is the sheet's `Value` role, so a field and the
/// word beside it in a grid are one size.
pub(super) fn place_field(input: &mut TextInput, rect: Rect) {
    input.set_text_role(Role::Value.text_role());
    input.apply_layout_external(rect);
}

/// Whether `input`'s text differs from `last` — a real edit, through any
/// door (typed, pasted, IME-committed, an accessibility `SetValue`) — and,
/// if so, records the new text in `last`. A caret or selection change
/// alone (a click, an accessibility `SetSelection`) is not an edit, even
/// though the widget marks itself dirty for it, so screens key their side
/// effects — re-parsing a link, clearing a mismatch line, moving on to the
/// next code cell — off the text and never off the flag. The flag is
/// drained here so it never accumulates unread.
pub(super) fn edited(input: &mut TextInput, last: &mut String) -> bool {
    let _ = input.take_dirty();
    if input.value() == last.as_str() {
        return false;
    }
    last.clear();
    last.push_str(input.value());
    true
}

/// Side of one pairing-code cell: the `s12` square, grown to hold a `Code`
/// line with an `s2` breath above and below it. At a large text scale the
/// glyph used to fill the whole square; spacing tokens do not follow the
/// text scale, so the cell has to be measured against the glyph it holds.
pub(super) fn code_cell_side(text_renderer: &mut TextRenderer, theme: &Theme) -> f64 {
    let sp = &theme.tokens.spacing;
    let glyph = line_height(text_renderer, theme, Role::Code);
    f64::from(sp.s12).max(glyph + 2.0 * f64::from(sp.s2))
}

/// Seat a one-digit [`TextInput`] inside its code `cell`: plain chrome (the
/// cell frame is painted by [`paint_cell`], focused or not), the `Code`
/// role, and a rect just wide enough for the widest digit plus the caret,
/// centred on the cell — the shared widget starts its text on its left
/// edge, so centring the widget is what centres the digit.
pub(super) fn place_code_cell(
    input: &mut TextInput,
    cell: Rect,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) {
    input.set_plain_chrome(true);
    input.set_text_role(Role::Code.text_role());
    let (glyph_w, _) = measure(text_renderer, theme, Role::Code, "0", None);
    let w = (glyph_w + theme.caret_width).min(cell.width());
    let x0 = cell.x0 + (cell.width() - w) / 2.0;
    input.apply_layout_external(Rect::new(x0, cell.y0, x0 + w, cell.y1));
}

/// The box of a checkbox row: an `s6` square (the retained checkbox's own
/// size — the face rounds with `radii.control`, so a smaller square would turn
/// into a disc) at the row's start, centred on the row. Pure, so the layout
/// pass can place it without painting.
pub(super) fn check_box_rect(row: Rect, theme: &Theme) -> Rect {
    let side = f64::from(theme.tokens.spacing.s6).min(row.height());
    let top = row.y0 + (row.height() - side) / 2.0;
    Rect::new(row.x0, top, row.x0 + side, top + side)
}

/// A checkbox row: the shared checkbox face at the row's start, its label
/// one gap to the right, both centred on the row. Returns the box rect; the
/// caller keeps the whole row as the hit target, so the label toggles too.
pub(super) fn paint_check_row(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    checked: bool,
    hovered: bool,
    label: &str,
) -> Rect {
    // `s6` matches the retained checkbox's own box (the pointer minimum
    // target); the face rounds with `radii.control`, so a smaller square would
    // turn into a disc.
    let box_rect = check_box_rect(row, theme);
    Checkbox::paint(
        scene,
        transform,
        theme,
        &CheckboxFace::new(box_rect, checked).hovered(hovered),
    );
    let label_rect = Rect::new(
        box_rect.x1 + f64::from(theme.tokens.spacing.s2),
        row.y0,
        row.x1,
        row.y1,
    );
    paint_text(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        label_rect,
        label,
        Role::Value,
        Align::Start,
        false,
    );
    box_rect
}

/// Split `row` into `count` equal segments for a segmented control.
pub(super) fn segment_rects(row: Rect, count: usize) -> Vec<Rect> {
    if count == 0 || row.width() <= 0.0 {
        return Vec::new();
    }
    let n = f64::from(u32::try_from(count).unwrap_or(u32::MAX));
    let w = row.width() / n;
    (0..count)
        .map(|i| {
            let i = f64::from(u32::try_from(i).unwrap_or(u32::MAX));
            Rect::new(row.x0 + i * w, row.y0, row.x0 + (i + 1.0) * w, row.y1)
        })
        .collect()
}

/// A segmented choice through the shared face, labelled at the action
/// row's size so it reads as a control rather than as body text.
pub(super) fn paint_segments(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    segments: &[Rect],
    labels: &[&str],
    selected: usize,
    hovered: Option<usize>,
) {
    let face = SegmentedFace::new(segments, labels, selected)
        .style(SegmentedStyle::Solid)
        .radius(f64::from(theme.tokens.radii.control))
        .hovered(hovered);
    ToggleGroup::paint(scene, transform, text_renderer, theme, &face);
}

/// Break `text` in `role` into lines that fit `width`, at any character —
/// for a value that has no break opportunity of its own (an invite URL is
/// one token; the renderer's word wrap would leave it running off the
/// card, and eliding it would leave it unreadable). Greedy and measured:
/// each candidate line is shaped with the style it will be drawn in, and
/// the measure cache makes the second pass free.
pub(super) fn wrap_chars(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    role: Role,
    text: &str,
    width: f64,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        let mut candidate = current.clone();
        candidate.push(ch);
        let (w, _) = measure(text_renderer, theme, role, &candidate, None);
        if w > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current.push(ch);
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// A bare field box with no text — the frame a code digit or a QR
/// placeholder sits in. It restates the shared `TextInput`'s field chrome
/// (raised fill, active fill and the two-pixel focus edge while focused,
/// `radii.control`) so a code cell reads as the same control as the fields
/// around it while the digit inside is a plain-chrome [`TextInput`].
pub(super) fn paint_cell(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    focused: bool,
) {
    let colors = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control));
    let fill = if focused {
        colors.lift_on(colors.surfaces.raised, 1.0)
    } else {
        colors.surfaces.raised
    };
    scene.fill(Fill::NonZero, transform, fill, None, &shape);
    // Rest edge derived from the face painted (HIG §9.5), so a cell reads
    // as a field on the dark preset where `border_standard` vanished.
    let (edge, edge_w) = if focused {
        (colors.border_focus, 2.0)
    } else {
        (crate::surface::edge_on(fill), 1.0)
    };
    scene.stroke(&Stroke::new(edge_w), transform, edge, None, &shape);
}

/// The 24-word grid both recovery screens lay out: `WORD_GRID_COLUMNS`
/// columns across the body, every row one control tall (so the verify
/// screen's editable blanks fit the same cells), each cell an ordinal
/// column followed by the word column.
#[derive(Clone, Copy, Debug)]
pub(super) struct WordGrid {
    origin_x: f64,
    origin_y: f64,
    col_w: f64,
    row_h: f64,
    /// Width of the ordinal column: the widest ordinal ("24.") plus a gap.
    ordinal_w: f64,
}

impl WordGrid {
    /// Rows needed for `words` at `WORD_GRID_COLUMNS` per row.
    pub(super) fn rows_for(words: usize) -> usize {
        words.div_ceil(WORD_GRID_COLUMNS)
    }

    /// Height the grid takes for `words`, given the row height.
    pub(super) fn height(words: usize, row_h: f64) -> f64 {
        f64::from(u32::try_from(Self::rows_for(words)).unwrap_or(u32::MAX)) * row_h
    }

    /// Lay the grid into `area` (whose height came from [`Self::height`]).
    pub(super) fn new(
        area: Rect,
        row_h: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Self {
        let cols = f64::from(u32::try_from(WORD_GRID_COLUMNS).unwrap_or(u32::MAX));
        let (widest, _) = measure(text_renderer, theme, Role::Label, "24.", None);
        Self {
            origin_x: area.x0,
            origin_y: area.y0,
            col_w: area.width() / cols,
            row_h,
            ordinal_w: widest + f64::from(theme.tokens.spacing.s2),
        }
    }

    /// `(ordinal, word)` rects of the zero-based cell `i`. The word rect
    /// stops one `s2` short of the next column so adjacent words never
    /// touch.
    pub(super) fn cell(&self, i: usize, theme: &Theme) -> (Rect, Rect) {
        let row = f64::from(u32::try_from(i / WORD_GRID_COLUMNS).unwrap_or(u32::MAX));
        let col = f64::from(u32::try_from(i % WORD_GRID_COLUMNS).unwrap_or(u32::MAX));
        let x = self.origin_x + col * self.col_w;
        let y = self.origin_y + row * self.row_h;
        let ordinal = Rect::new(x, y, x + self.ordinal_w, y + self.row_h);
        let word = Rect::new(
            ordinal.x1,
            y,
            x + self.col_w - f64::from(theme.tokens.spacing.s2),
            y + self.row_h,
        );
        (ordinal, word)
    }
}

/// Paint a QR code's modules into `rect`, quiet zone included.
///
/// The two colours are the QR contract, not a palette choice: a scanner
/// needs dark modules on a light field whatever the theme, so a dark theme
/// must never paint a black-on-black code. `QrWidget::draw` fixes the same
/// two values for the same reason; this painter exists because the screens
/// place the code by rect rather than by taffy node. A payload the encoder
/// refused paints the widget's error inside a field cell, as
/// `QrWidget::draw` does, rather than a blank white square.
pub(super) fn paint_qr(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    qr: &QrWidget,
    rect: Rect,
) {
    if let Some(err) = qr.error() {
        paint_cell(scene, transform, theme, rect, false);
        let inset = f64::from(theme.tokens.spacing.s3);
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            rect.inset(-inset),
            &format!("QR error: {err}"),
            Role::Error,
            Align::Center,
            true,
        );
        return;
    }
    let light = Color::from_rgb8(0xFF, 0xFF, 0xFF);
    let dark = Color::from_rgb8(0, 0, 0);
    scene.fill(Fill::NonZero, transform, light, None, &rect);
    let total = qr.total_modules();
    if total == 0 {
        return;
    }
    let side = rect.width().min(rect.height()).max(0.0);
    let module = side / modules_to_f64(total);
    if module <= 0.0 {
        return;
    }
    let quiet_modules = qr.quiet_modules();
    let quiet = modules_to_f64(quiet_modules) * module;
    // The quiet zone runs around all four sides, so it is subtracted twice.
    let visible = total.saturating_sub(quiet_modules.saturating_mul(2));
    for y in 0..visible {
        for x in 0..visible {
            if !qr.module(x, y) {
                continue;
            }
            let px = rect.x0 + quiet + modules_to_f64(x) * module;
            let py = rect.y0 + quiet + modules_to_f64(y) * module;
            scene.fill(
                Fill::NonZero,
                transform,
                dark,
                None,
                &Rect::new(px, py, px + module, py + module),
            );
        }
    }
}

/// Widens a QR module index/count to `f64`; the QR spec caps a matrix at
/// 177 modules, so the `u32` step is lossless.
fn modules_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Narrows a bounded on-screen width to the renderer's `f32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a body row is a few hundred logical pixels, exact in f32"
)]
fn narrow(px: f64) -> f32 {
    px as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Widget;
    use crate::widget::modal_dialog::MODAL_MAX_WIDTH;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);

    fn themes() -> Vec<(&'static str, Theme)> {
        vec![
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high-contrast", Theme::high_contrast()),
            ("text-1.5", Theme::dark().with_text_scale(1.5)),
        ]
    }

    #[test]
    fn sheet_card_centres_clamps_and_measures_its_body() {
        fn body_width_is_what_the_modal_layout_hands_back() {
            let mut tr = TextRenderer::new();
            for (name, theme) in themes() {
                let actions = [ModalAction::primary("Continue")];
                let spec = SheetSpec {
                    title: "Title",
                    max_width: FORM_SHEET_WIDTH,
                    actions: &actions,
                };
                let predicted = Sheet::body_width(VIEWPORT, FORM_SHEET_WIDTH, &theme);
                let sheet = Sheet::layout(VIEWPORT, &spec, 120.0, &mut tr, &theme);
                assert!(
                    (sheet.body().width() - predicted).abs() < 0.01,
                    "{name}: predicted body width {predicted}, layout gave {}",
                    sheet.body().width()
                );
            }
        }

        fn card_centres_on_the_viewport_and_body_carries_the_measured_height() {
            let mut tr = TextRenderer::new();
            for (name, theme) in themes() {
                let actions = [
                    ModalAction::secondary("Back"),
                    ModalAction::primary("Continue"),
                ];
                let spec = SheetSpec {
                    title: "Title",
                    max_width: FORM_SHEET_WIDTH,
                    actions: &actions,
                };
                let body_h = 180.0;
                let sheet = Sheet::layout(VIEWPORT, &spec, body_h, &mut tr, &theme);
                let card = sheet.layout.card;
                assert!(
                    (card.center().x - VIEWPORT.center().x).abs() < 0.5
                        && (card.center().y - VIEWPORT.center().y).abs() < 0.5,
                    "{name}: card {card:?} is not centred on {VIEWPORT:?}"
                );
                assert!(
                    (sheet.body().height() - body_h).abs() < 0.5,
                    "{name}: body {} tall for a {body_h} measurement",
                    sheet.body().height()
                );
                assert!(
                    card.width()
                        <= MODAL_MAX_WIDTH.max(FORM_SHEET_WIDTH) * f64::from(theme.text_scale)
                            + 0.5,
                    "{name}: card wider than the sheet maximum"
                );
            }
        }

        fn a_small_viewport_clamps_the_card_and_reports_overflow() {
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let actions = [ModalAction::primary("Continue")];
            let spec = SheetSpec {
                title: "Title",
                max_width: GRID_SHEET_WIDTH,
                actions: &actions,
            };
            let small = Rect::new(0.0, 0.0, 480.0, 360.0);
            let sheet = Sheet::layout(small, &spec, 900.0, &mut tr, &theme);
            assert!(sheet.layout.body_overflows, "900 px of rows cannot fit 360");
            assert!(
                sheet.layout.card.x0 >= small.x0 && sheet.layout.card.x1 <= small.x1,
                "the card stays inside a narrow viewport"
            );
            assert!(
                sheet.action(0).y1 <= sheet.layout.card.y1,
                "the action row stays reachable inside the card"
            );
        }

        /// A body taller than the card scrolls: the offset clamps to what
        /// overflows, a wheel line is a control row, PageDown is a body, and a
        /// row scrolled under the action row has no pointer target.
        fn body_scroll_clamps_and_hides_rows_outside_the_body() {
            let body = Rect::new(100.0, 200.0, 600.0, 400.0);
            let mut scroll = BodyScroll::default();
            let placed = scroll.place(body, 500.0, 34.0);
            assert_eq!(scroll.max(), 300.0);
            assert_eq!(
                placed.y0, body.y0,
                "unscrolled rows start at the body's top"
            );
            assert_eq!(placed.height(), 500.0);

            assert!(scroll.wheel(&MouseScrollDelta::LineDelta(0.0, -2.0)));
            assert_eq!(scroll.offset(), 68.0, "two lines down is two control rows");
            assert!(!scroll.wheel(&MouseScrollDelta::LineDelta(0.0, 0.0)));
            assert!(scroll.key(&Key::Named(NamedKey::PageDown), body.height()));
            assert_eq!(scroll.offset(), 268.0);
            assert!(scroll.key(&Key::Named(NamedKey::PageDown), body.height()));
            assert_eq!(scroll.offset(), 300.0, "clamped at the overflow");
            assert!(!scroll.key(&Key::Named(NamedKey::PageDown), body.height()));
            let placed = scroll.place(body, 500.0, 34.0);
            assert_eq!(placed.y0, body.y0 - 300.0);

            let below = Rect::new(100.0, 420.0, 600.0, 454.0);
            assert_eq!(BodyScroll::visible(body, below), Rect::ZERO);
            let straddling = Rect::new(100.0, 380.0, 600.0, 414.0);
            assert_eq!(BodyScroll::visible(body, straddling).y1, body.y1);

            // A body that fits again snaps back and refuses to scroll.
            let placed = scroll.place(body, 150.0, 34.0);
            assert_eq!(scroll.offset(), 0.0);
            assert_eq!(placed.y0, body.y0);
            assert!(!scroll.wheel(&MouseScrollDelta::LineDelta(0.0, -1.0)));
        }

        /// `resolve` returns the same plan from the layout renderer and from a
        /// fresh frame renderer, so what `apply_layout` hit-tests is what
        /// `draw` paints.
        fn resolve_agrees_between_the_layout_renderer_and_a_frame_renderer() {
            let theme = Theme::dark();
            let actions = [ModalAction::primary("Continue")];
            let spec = SheetSpec {
                title: "Title",
                max_width: FORM_SHEET_WIDTH,
                actions: &actions,
            };
            let plan = |mut rows: Rows, control_h: f64, tr: &mut TextRenderer, theme: &Theme| {
                let a = rows.row(paragraph_height(
                    tr,
                    theme,
                    Role::Body,
                    "A sentence long enough to wrap inside a form sheet body once or twice.",
                    rows.width(),
                ));
                rows.gap(theme.tokens.spacing.s6);
                let b = rows.row(control_h);
                ((a, b), rows.bottom())
            };
            let mut scroll_a = BodyScroll::default();
            let from_layout = with_layout_renderer(&theme, |tr| {
                resolve(VIEWPORT, &spec, &mut scroll_a, tr, &theme, plan)
            });
            let mut frame = TextRenderer::new();
            let mut scroll_b = BodyScroll::default();
            let from_frame = resolve(VIEWPORT, &spec, &mut scroll_b, &mut frame, &theme, plan);
            assert_eq!(from_layout.plan, from_frame.plan);
            assert_eq!(from_layout.sheet.layout.card, from_frame.sheet.layout.card);
            assert_eq!(
                from_layout.plan.1.height(),
                from_layout.sheet.action(0).height()
            );
        }

        body_width_is_what_the_modal_layout_hands_back();
        card_centres_on_the_viewport_and_body_carries_the_measured_height();
        a_small_viewport_clamps_the_card_and_reports_overflow();
        body_scroll_clamps_and_hides_rows_outside_the_body();
        resolve_agrees_between_the_layout_renderer_and_a_frame_renderer();
    }

    #[test]
    fn sheet_rows_cells_and_segments_tile_without_overlap() {
        fn rows_stack_without_overlap_and_measure_their_own_height() {
            let mut rows = Rows::measuring(400.0);
            let a = rows.row(30.0);
            rows.gap(8.0);
            let b = rows.row(44.0);
            assert_eq!(a.y1, 30.0);
            assert_eq!(b.y0, 38.0);
            assert_eq!(b.y1, 82.0);
            assert_eq!(rows.bottom(), 82.0);
            assert_eq!(rows.width(), 400.0);
            let placed = Rows::new(Rect::new(100.0, 200.0, 500.0, 282.0));
            let mut placed_rows = placed;
            let a2 = placed_rows.row(30.0);
            assert_eq!((a2.x0, a2.y0, a2.x1), (100.0, 200.0, 500.0));
        }

        fn control_height_is_the_action_rows_button_height() {
            let mut tr = TextRenderer::new();
            for (name, theme) in themes() {
                let actions = [ModalAction::primary("Continue")];
                let spec = SheetSpec {
                    title: "Title",
                    max_width: FORM_SHEET_WIDTH,
                    actions: &actions,
                };
                let sheet = Sheet::layout(VIEWPORT, &spec, 60.0, &mut tr, &theme);
                assert!(
                    (sheet.control_height(&theme) - sheet.action(0).height()).abs() < 0.01,
                    "{name}: a body control must be one action button tall"
                );
            }
        }

        fn wrap_chars_breaks_an_unbreakable_token_into_lines_that_fit() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let token: String = "TILOINVITEBASE32".repeat(12);
            let width = 240.0;
            let lines = wrap_chars(&mut tr, &theme, Role::Value, &token, width);
            assert!(
                lines.len() > 1,
                "a 180-character token needs more than one line"
            );
            for line in &lines {
                let (w, _) = measure(&mut tr, &theme, Role::Value, line, None);
                assert!(w <= width + 0.01, "line {line:?} is {w} wide for {width}");
            }
            assert_eq!(lines.concat(), token, "every character is kept, in order");
        }

        fn word_grid_cells_tile_four_columns_and_never_touch() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let row_h = 34.0;
            let area = Rect::new(100.0, 300.0, 740.0, 300.0 + WordGrid::height(24, row_h));
            assert_eq!(WordGrid::rows_for(24), 6);
            assert_eq!(area.height(), 6.0 * row_h);
            let grid = WordGrid::new(area, row_h, &mut tr, &theme);
            let (o0, w0) = grid.cell(0, &theme);
            let (o1, w1) = grid.cell(1, &theme);
            let (o4, _) = grid.cell(4, &theme);
            assert_eq!(o0.x0, area.x0);
            assert_eq!(o0.y0, area.y0);
            assert_eq!(
                w0.x0, o0.x1,
                "the word starts where its ordinal column ends"
            );
            assert!(w0.x1 < o1.x0, "a word stops short of the next column");
            assert!((o1.x0 - o0.x0 - area.width() / 4.0).abs() < 1e-9);
            assert!(
                (o4.y0 - o0.y0 - row_h).abs() < 1e-9,
                "row 2 is one row down"
            );
            assert_eq!(w1.height(), row_h);
        }

        fn segments_tile_the_row_exactly() {
            let row = Rect::new(10.0, 20.0, 410.0, 54.0);
            let segs = segment_rects(row, 4);
            assert_eq!(segs.len(), 4);
            assert_eq!(segs[0].x0, row.x0);
            assert!((segs[3].x1 - row.x1).abs() < 1e-9);
            for pair in segs.windows(2) {
                assert!((pair[0].x1 - pair[1].x0).abs() < 1e-9, "segments abut");
                assert!((pair[0].width() - pair[1].width()).abs() < 1e-9);
            }
            assert!(segment_rects(row, 0).is_empty());
        }

        rows_stack_without_overlap_and_measure_their_own_height();
        control_height_is_the_action_rows_button_height();
        wrap_chars_breaks_an_unbreakable_token_into_lines_that_fit();
        word_grid_cells_tile_four_columns_and_never_touch();
        segments_tile_the_row_exactly();
    }

    #[test]
    fn sheet_text_roles_and_cells_scale_with_text() {
        /// An embedded field takes exactly the row the plan gave it and speaks
        /// the sheet's `Value` role, so it lines up with the grid words beside
        /// it; a code cell's digit widget is centred on its cell and never
        /// wider than it.
        fn embedded_fields_take_their_row_and_code_cells_centre_their_digit() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut input = TextInput::local();
            let row = Rect::new(100.0, 200.0, 620.0, 234.0);
            place_field(&mut input, row);
            assert_eq!(Widget::get_rect(&input), row);
            assert!(
                !input.is_plain(),
                "a field keeps the shared widget's chrome"
            );

            let mut digit = TextInput::local();
            let side = code_cell_side(&mut tr, &theme);
            assert!(
                (side - f64::from(theme.tokens.spacing.s12)).abs() < 1e-9,
                "at 1× the cell is s12"
            );
            let cell = Rect::new(100.0, 300.0, 100.0 + side, 300.0 + side);
            place_code_cell(&mut digit, cell, &mut tr, &theme);
            let r = Widget::get_rect(&digit);
            assert!(digit.is_plain(), "the cell frame is painted by paint_cell");
            assert!(
                (r.center().x - cell.center().x).abs() < 1e-9,
                "digit centred on its cell"
            );
            assert!(
                r.x0 > cell.x0 && r.x1 < cell.x1,
                "digit widget stays inside the cell"
            );
            assert_eq!((r.y0, r.y1), (cell.y0, cell.y1));
        }

        /// At 2× text the `Code` line outgrows an `s12` square, so the cell
        /// grows with the glyph instead of the glyph filling the frame.
        fn code_cell_grows_with_the_text_scale() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let side = code_cell_side(&mut tr, &theme);
            let glyph = line_height(&mut tr, &theme, Role::Code);
            assert!(side > f64::from(theme.tokens.spacing.s12));
            assert!(side >= glyph + 2.0 * f64::from(theme.tokens.spacing.s2));
        }

        fn roles_resolve_to_theme_roles_once() {
            let base = Theme::dark();
            let scaled = Theme::dark().with_text_scale(2.0);
            for role in [
                Role::Body,
                Role::Label,
                Role::Value,
                Role::Caption,
                Role::Error,
                Role::Code,
            ] {
                assert!(
                    (role.style(&scaled).size - role.style(&base).size * 2.0).abs() < 0.01,
                    "{role:?}: the role carries the text scale exactly once"
                );
            }
            assert!(Role::Code.style(&base).size > Role::Value.style(&base).size);
            assert!(Role::Value.style(&base).size > Role::Caption.style(&base).size);
        }

        fn line_height_grows_with_the_text_scale() {
            let mut tr = TextRenderer::new();
            let one = line_height(&mut tr, &Theme::dark(), Role::Value);
            let mut tr2 = TextRenderer::new();
            tr2.set_text_scale(1.5);
            let big = line_height(&mut tr2, &Theme::dark().with_text_scale(1.5), Role::Value);
            assert!(
                big > one,
                "a 1.5× renderer must measure a taller line ({one} → {big})"
            );
        }

        embedded_fields_take_their_row_and_code_cells_centre_their_digit();
        code_cell_grows_with_the_text_scale();
        roles_resolve_to_theme_roles_once();
        line_height_grows_with_the_text_scale();
    }
}
