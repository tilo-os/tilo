//! `Autocomplete` — a text field with a suggestion dropdown that floats below
//! it.
//!
//! A real [`TextInput`] (so it inherits selection, clipboard, IME, caret
//! navigation and theming for free, per `crates/sdk/ui-core/HIG.md` §8) with a suggestion
//! popover drawn in the overlay pass so it composites above sibling content.
//!
//! Any app attaches a suggestion source — a closure via
//! [`with_suggester`](Autocomplete::with_suggester), or a reactively-computed
//! list via [`set_suggestions`](Autocomplete::set_suggestions) — and
//! completions recompute whenever the typed text changes.
//!
//! Typing opens the dropdown; Down/Up move the highlight; Enter accepts (the
//! highlighted suggestion, or the typed text); Tab completes to the
//! highlight without accepting; Esc or an outside click cancels — the
//! outside click is not consumed, so it still reaches the host. The
//! outcome is polled once via [`take_outcome`](Autocomplete::take_outcome),
//! rising-edge like [`TextInput::take_dirty`].
//!
//! The editable path bar in the file manager is the canonical consumer, but
//! the widget knows nothing about paths.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, TextInputKind, Widget};

/// One completion offered in the dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The text inserted into the field when this suggestion is accepted.
    pub value: String,
    /// The label shown in the dropdown row. Defaults to `value` via
    /// [`Suggestion::new`].
    pub label: String,
    /// Optional dim secondary text shown right-aligned in the row (e.g. a type
    /// or a count).
    pub detail: Option<String>,
    /// Optional leading glyph.
    pub icon: Option<Icon>,
}

impl Suggestion {
    /// A suggestion whose visible label is its inserted value.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            label: value.clone(),
            value,
            detail: None,
            icon: None,
        }
    }

    /// A suggestion whose visible label differs from the inserted value.
    pub fn labelled(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: None,
            icon: None,
        }
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// How the most recent interaction ended. Returned once by
/// [`Autocomplete::take_outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutocompleteOutcome {
    /// Nothing ended since the last poll.
    None,
    /// The user accepted this value (Enter, or a click on a row).
    Accepted(String),
    /// The user cancelled (Esc, or a click outside).
    Cancelled,
}

/// A suggestion source: given the current text, return the completions to show.
pub type Suggester = Box<dyn Fn(&str) -> Vec<Suggestion>>;

const ROW_H: f64 = 32.0;
/// Height of the single non-interactive status row (loading / "no matches").
const STATUS_ROW_H: f64 = 34.0;
const DROPDOWN_GAP: f64 = 6.0;
const ROW_PAD_X: f64 = 12.0;
const ICON_PX: f64 = 16.0;
const DEFAULT_MAX_VISIBLE: usize = 8;

/// Widens a small UI count (row/suggestion index or count, always far
/// below `f64`'s exact-integer range) for layout math.
#[allow(
    clippy::cast_precision_loss,
    reason = "UI row/suggestion counts are always far below f64's exact-integer range"
)]
fn count_f64(n: usize) -> f64 {
    n as f64
}

/// Narrows a screen-space `f64` layout value to `f32` for text rendering;
/// every call site here is a bounded panel/row dimension, well within
/// `f32` range.
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space px value, always well within f32 range"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Converts a size from the theme's already-scaled typography tokens back to
/// the renderer's input space. `TextRenderer` applies its own accessibility
/// scale while shaping and measuring, so passing a scaled token through
/// unchanged would apply the preference twice.
fn renderer_font_size(theme_size: f32, renderer_scale: f32) -> f32 {
    theme_size / renderer_scale.max(f32::EPSILON)
}

pub struct Autocomplete {
    id: String,
    /// The real editable field — owns caret / selection / clipboard / IME.
    input: TextInput,
    suggester: Option<Suggester>,
    suggestions: Vec<Suggestion>,
    /// Keyboard-highlighted row (into `suggestions`); `None` = nothing armed,
    /// so Enter commits the typed text.
    highlight: Option<usize>,
    /// Mouse-hovered row, for the hover veneer.
    hover: Option<usize>,
    open: bool,
    /// Loading / "no matches" line for a reactive host with no rows yet.
    /// `None` renders nothing.
    status: Option<String>,
    max_visible: usize,
    rect: Rect,
    last_pointer: Point,
    outcome: AutocompleteOutcome,
    /// Last effective accessibility type scale. Dropdown geometry is queried
    /// during pointer routing, outside the draw call that supplies the theme,
    /// so retain it alongside the externally-applied field rectangle.
    text_scale: f64,
}

impl Autocomplete {
    pub fn new() -> Self {
        Self::with_input(TextInput::local())
    }

    fn with_input(input: TextInput) -> Self {
        Self {
            id: String::new(),
            input: input.with_kind(TextInputKind::Search),
            suggester: None,
            suggestions: Vec::new(),
            highlight: None,
            hover: None,
            open: false,
            status: None,
            max_visible: DEFAULT_MAX_VISIBLE,
            rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            outcome: AutocompleteOutcome::None,
            text_scale: 1.0,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn new_with_controlled_input(input: TextInput) -> Self {
        Self::with_input(input)
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self.input.set_id(id);
        self
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.input.set_placeholder(hint);
        self
    }

    #[must_use]
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.input.set_font_size(size);
        self
    }

    #[must_use]
    pub fn with_suggester<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Vec<Suggestion> + 'static,
    {
        self.suggester = Some(Box::new(f));
        self
    }

    pub fn set_suggester<F>(&mut self, f: F)
    where
        F: Fn(&str) -> Vec<Suggestion> + 'static,
    {
        self.suggester = Some(Box::new(f));
    }

    /// Rows beyond this clip rather than growing the panel without bound.
    #[must_use]
    pub fn with_max_visible(mut self, n: usize) -> Self {
        self.max_visible = n.max(1);
        self
    }

    /// For hosts that compute suggestions reactively rather than through a
    /// [`Suggester`]. Re-clamps the highlight and clears any status line.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.suggestions = suggestions;
        if !self.suggestions.is_empty() {
            self.status = None;
        }
        if let Some(h) = self.highlight
            && h >= self.suggestions.len()
        {
            self.highlight = None;
        }
    }

    /// For reactive/async hosts filling suggestions or a status line that
    /// arrived *after* the keystroke that requested them (which otherwise
    /// leaves the dropdown closed).
    pub fn open(&mut self) {
        self.open = true;
    }

    /// `None` clears it. Pair with [`open`](Autocomplete::open).
    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }

    /// Opens the dropdown when the field is actively being edited (for
    /// reactive/async hosts receiving results after a round-trip). Closes
    /// it when the list is empty.
    pub fn set_suggestions_open(&mut self, suggestions: Vec<Suggestion>) {
        self.set_suggestions(suggestions);
        if self.is_editing() {
            self.open = !self.suggestions.is_empty();
        }
    }

    pub fn text(&self) -> &str {
        self.input.value()
    }

    /// Replace the field text (does not open the dropdown or recompute).
    pub fn set_text(&mut self, s: &str) {
        self.input.set_text(s);
    }

    /// Select-all runs **only** on the genuine transition into editing —
    /// never on a redundant call while already focused (a focus/refresh
    /// path can trigger that mid-edit), which would let the next keystroke
    /// overwrite what the user just typed, silently dropping a character.
    pub fn begin_edit(&mut self) {
        let entering = !self.input.is_focused();
        self.input.set_focus(true);
        if entering {
            self.input.select_all();
        }
        self.outcome = AutocompleteOutcome::None;
        self.recompute();
        self.open = !self.suggestions.is_empty();
        self.highlight = None;
    }

    pub fn is_editing(&self) -> bool {
        self.input.is_focused()
    }

    pub fn is_open(&self) -> bool {
        self.open && !self.suggestions.is_empty()
    }

    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    pub fn highlighted(&self) -> Option<&Suggestion> {
        self.highlight.and_then(|i| self.suggestions.get(i))
    }

    /// Clamped to range; `None` clears. Reactive hosts use this to
    /// highlight the top match by default.
    pub fn set_highlight(&mut self, index: Option<usize>) {
        self.highlight = index.filter(|&i| i < self.suggestions.len());
    }

    /// Close the dropdown (keeps the field text + focus).
    pub fn close(&mut self) {
        self.open = false;
        self.highlight = None;
        self.status = None;
    }

    /// Rising-edge read of how the most recent interaction ended.
    pub fn take_outcome(&mut self) -> AutocompleteOutcome {
        std::mem::replace(&mut self.outcome, AutocompleteOutcome::None)
    }

    /// For hosts embedding the field in their own immediate-mode chrome.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input.apply_layout_external(rect);
    }

    fn recompute(&mut self) {
        if let Some(f) = &self.suggester {
            self.suggestions = f(self.input.value());
        }
        if let Some(h) = self.highlight
            && h >= self.suggestions.len()
        {
            self.highlight = None;
        }
    }

    /// Accept `value`: emit the outcome, load it into the field, close.
    fn accept(&mut self, value: String) {
        self.input.set_text(&value);
        self.open = false;
        self.highlight = None;
        self.status = None;
        self.outcome = AutocompleteOutcome::Accepted(value);
    }

    fn cancel(&mut self) {
        self.open = false;
        self.highlight = None;
        self.status = None;
        self.input.set_focus(false);
        self.outcome = AutocompleteOutcome::Cancelled;
    }

    /// Keeps the edit open so the user can drill further.
    fn complete_to_highlight(&mut self) {
        let idx = self.highlight.or(if self.suggestions.is_empty() {
            None
        } else {
            Some(0)
        });
        if let Some(i) = idx
            && let Some(s) = self.suggestions.get(i)
        {
            let value = s.value.clone();
            self.input.set_text(&value);
            self.input.set_cursor_pos(value.chars().count());
            self.recompute();
            self.open = !self.suggestions.is_empty();
            self.highlight = None;
        }
    }

    /// Only ever called with a single step (`true` = toward the end);
    /// wraps at both ends. Plain `usize` arithmetic (no `i64` round trip)
    /// keeps every step provably in range.
    fn move_highlight(&mut self, forward: bool) {
        if self.suggestions.is_empty() {
            return;
        }
        self.open = true;
        let len = self.suggestions.len();
        let last = len.saturating_sub(1);
        let next = match self.highlight {
            None => {
                if forward {
                    0
                } else {
                    last
                }
            }
            Some(cur) if forward => {
                let stepped = cur.saturating_add(1);
                if stepped >= len { 0 } else { stepped }
            }
            Some(cur) => cur.checked_sub(1).unwrap_or(last),
        };
        self.highlight = Some(next);
    }

    /// `None` when it should not show. Shows for real rows or a status line.
    fn dropdown_rect(&self) -> Option<Rect> {
        if !self.open || (self.suggestions.is_empty() && self.status.is_none()) {
            return None;
        }
        let h = if self.suggestions.is_empty() {
            STATUS_ROW_H * self.text_scale.max(1.0) + 8.0
        } else {
            count_f64(self.suggestions.len().min(self.max_visible)) * self.row_height() + 8.0
        };
        Some(Rect::new(
            self.rect.x0,
            self.rect.y1 + DROPDOWN_GAP,
            self.rect.x1,
            self.rect.y1 + DROPDOWN_GAP + h,
        ))
    }

    fn row_height(&self) -> f64 {
        ROW_H * self.text_scale.max(1.0)
    }

    fn row_rect(&self, panel: Rect, i: usize) -> Rect {
        let row_height = self.row_height();
        let y0 = panel.y0 + 4.0 + count_f64(i) * row_height;
        Rect::new(panel.x0 + 4.0, y0, panel.x1 - 4.0, y0 + row_height)
    }

    /// The dropdown row under a logical point, if any.
    fn row_at(&self, pt: Point) -> Option<usize> {
        let panel = self.dropdown_rect()?;
        if !panel.contains(pt) {
            return None;
        }
        (0..self.suggestions.len().min(self.max_visible))
            .find(|&i| self.row_rect(panel, i).contains(pt))
    }

    /// Paints one suggestion row: hover/armed background, optional icon,
    /// right-aligned detail, then the label clipped to what's left of the
    /// row. Split out of `draw_overlay` so that function stays a plain
    /// walk over the visible rows.
    fn draw_row_background(
        scene: &mut Scene,
        transform: Affine,
        tokens: &crate::tokens::Tokens,
        accent: vello::peniko::Color,
        row: Rect,
        armed: bool,
        hovered: bool,
    ) {
        let color = if armed {
            Some(accent)
        } else if hovered {
            Some(tokens.colors.overlay_medium)
        } else {
            None
        };
        if let Some(color) = color {
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(row, f64::from(tokens.radii.detail)),
            );
        }
    }

    fn draw_row(&self, ctx: &mut PaintCtx<'_>, row: Rect, i: usize, s: &Suggestion) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let tokens = &theme.tokens;
        let accent = tokens.colors.intent.inverted;
        let armed = self.highlight == Some(i);
        let hovered = self.hover == Some(i) && !armed;
        Self::draw_row_background(scene, transform, tokens, accent, row, armed, hovered);
        let on_accent = tokens.colors.intent.on_inverted;
        let text_color = if armed {
            on_accent
        } else {
            tokens.colors.text_primary
        };
        // 80% white alpha on a bright accent drops an already-marginal
        // label below legible, so soften toward the fill instead, held
        // to the contrast floor.
        let dim_color = if armed {
            tokens.colors.ink_on_any(
                &[accent],
                crate::tokens::colors::over(accent, on_accent, 0.8),
                crate::tokens::colors::ON_FILL_MIN_CONTRAST,
            )
        } else {
            tokens.colors.text_dim
        };
        let text_size =
            renderer_font_size(tokens.typography.scale.base, text_renderer.text_scale());
        let detail_size =
            renderer_font_size(tokens.typography.scale.sm, text_renderer.text_scale());
        let rendered_text_height = f64::from(text_renderer.measure(&s.label, text_size, None).1);
        let rendered_detail_height = s.detail.as_ref().map_or(0.0, |detail| {
            f64::from(text_renderer.measure(detail, detail_size, None).1)
        });
        let large_text = self.text_scale >= 1.5;
        let icon_px = ICON_PX * self.text_scale.min(1.5);
        let mut x = row.x0 + ROW_PAD_X;
        if let Some(icon) = s.icon {
            let iy = row.y0 + (row.height() - icon_px) / 2.0;
            draw_icon(
                scene,
                transform,
                Rect::new(x, iy, x + icon_px, iy + icon_px),
                icon,
                if armed {
                    on_accent
                } else {
                    tokens.colors.text_dim
                },
                LUCIDE_DEFAULT_STROKE_PX,
            );
            x += icon_px + 10.0;
        }
        let ty = if large_text && s.detail.is_some() {
            row.y0 + 7.0
        } else {
            row.y0 + (row.height() - rendered_text_height) / 2.0
        };
        // Right-aligned detail first, so we can bound the label width.
        let mut label_x1 = row.x1 - ROW_PAD_X;
        if let Some(detail) = &s.detail {
            let (dw, _) = text_renderer.measure(detail, detail_size, None);
            let detail_width = px((row.x1 - ROW_PAD_X - x).max(0.0));
            let dx = if large_text {
                x
            } else {
                row.x1 - ROW_PAD_X - f64::from(dw)
            };
            let detail = text_renderer.elide_to_width(detail, detail_size, detail_width);
            text_renderer.draw_text(
                scene,
                &detail,
                detail_size,
                dim_color,
                transform.pre_translate(Vec2::new(
                    dx,
                    if large_text {
                        row.y0 + row.height() / 2.0 + 2.0
                    } else {
                        row.y0 + (row.height() - rendered_detail_height) / 2.0
                    },
                )),
                None,
            );
            if !large_text {
                label_x1 = dx - 8.0;
            }
        }
        let label_width = px((label_x1 - x).max(0.0));
        let label = text_renderer.elide_to_width(&s.label, text_size, label_width);
        text_renderer.draw_text(
            scene,
            &label,
            text_size,
            text_color,
            transform.pre_translate(Vec2::new(x, ty)),
            None,
        );
    }
}

impl Default for Autocomplete {
    fn default() -> Self {
        Self::new()
    }
}

impl Autocomplete {
    /// The `KeyboardInput` arm of [`Widget::handle_event`], split out to
    /// keep that under the line cap. Navigation/accept/cancel keys are
    /// intercepted here since the field itself never consumes them;
    /// everything else forwards to the field and recomputes on change.
    fn handle_key_while_editing(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
        logical_key: &Key,
    ) {
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_highlight(true);
                *ctx.handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_highlight(false);
                *ctx.handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                let value = self
                    .highlighted()
                    .map_or_else(|| self.input.value().to_string(), |s| s.value.clone());
                self.accept(value);
                *ctx.handled = true;
            }
            Key::Named(NamedKey::Tab) if !self.suggestions.is_empty() => {
                self.complete_to_highlight();
                *ctx.handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                self.cancel();
                *ctx.handled = true;
            }
            _ => {
                // Forward to the field, then recompute if the text changed.
                self.input.handle_event(event, ctx);
                if self.input.take_dirty().is_some() {
                    self.recompute();
                    self.open = !self.suggestions.is_empty();
                    self.highlight = None;
                }
            }
        }
    }
}

impl Widget for Autocomplete {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(34.0 * theme.text_scale.max(1.0)),
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.input.apply_layout_external(self.rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.text_scale = f64::from(theme.text_scale);
        // The field itself; the suggestion list floats in `draw_overlay`.
        self.input.apply_layout_external(self.rect);
        self.input
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.text_scale = f64::from(theme.text_scale);
        let Some(panel) = self.dropdown_rect() else {
            return;
        };
        let tokens = &theme.tokens;
        let shape = RoundedRect::from_rect(panel, f64::from(tokens.radii.control));
        // The suggestion list is a Panel (HIG §9.1 / §9.5): panel ground and
        // panel edge, like the toolkit's other popovers.
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(panel, f64::from(tokens.radii.control)),
        );

        let text_size = renderer_type_size(text_renderer, tokens.typography.scale.base);

        // No rows yet: render the lone status line (loading / no matches).
        if self.suggestions.is_empty() {
            if let Some(status) = &self.status {
                let (_, status_h) = text_renderer.measure(status, text_size, None);
                let ty = panel.y0 + (panel.height() - f64::from(status_h)) / 2.0;
                text_renderer.draw_text(
                    scene,
                    status,
                    text_size,
                    tokens.colors.text_dim,
                    transform.pre_translate(Vec2::new(panel.x0 + ROW_PAD_X + 4.0, ty)),
                    Some(px((panel.width() - 2.0 * (ROW_PAD_X + 4.0)).max(0.0))),
                );
            }
            return;
        }
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        for (i, s) in self.suggestions.iter().enumerate().take(self.max_visible) {
            let row = self.row_rect(panel, i);
            self.draw_row(&mut ctx, row, i, s);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.hover = self.row_at(self.last_pointer);
        }
        if !self.is_editing() {
            return;
        }
        match event {
            // Navigation / accept / cancel keys the field itself never consumes,
            // so the wrapper owns them. Intercept before forwarding.
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key_while_editing(event, ctx, logical_key),
            // A left press on a dropdown row accepts it.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.row_at(self.last_pointer).is_some() => {
                if let Some(i) = self.row_at(self.last_pointer)
                    && let Some(s) = self.suggestions.get(i)
                {
                    let value = s.value.clone();
                    self.accept(value);
                    *ctx.handled = true;
                }
            }
            // A left press outside the field and the dropdown cancels. Not
            // consumed, so the click still reaches whatever is under it.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.rect.contains(self.last_pointer)
                && self
                    .dropdown_rect()
                    .is_none_or(|r| !r.contains(self.last_pointer)) =>
            {
                self.cancel();
            }
            _ => {
                self.input.handle_event(event, ctx);
                if self.input.take_dirty().is_some() {
                    self.recompute();
                    self.open = !self.suggestions.is_empty();
                    self.highlight = None;
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "Autocomplete"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("autocomplete")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else if self.row_at(point).is_some() {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.input.set_focus(focused);
        if !focused {
            self.open = false;
        }
    }

    fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.input)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.input.is_dragging()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Politeness, Role, hash_key};
        let mut node = self.input.accessibility()?;
        let id = self.test_id().unwrap_or("autocomplete");
        // This widget owns the embedded input and its synthetic result rows.
        // Publish one keyed route for all of them instead of leaking the
        // inner TextInput's otherwise unhandled action target.
        node.action_target = None;
        node.key = Some(hash_key(&id));
        node.bounds = Some(self.rect.into());
        node.state.expanded = Some(self.open);
        if let Some(panel) = self.dropdown_rect() {
            for (index, suggestion) in self.suggestions.iter().take(self.max_visible).enumerate() {
                let mut row = AccessNode::new(Role::Button)
                    .with_name(&suggestion.label)
                    .with_key(hash_key(&(
                        "autocomplete-suggestion",
                        suggestion.value.as_str(),
                    )))
                    .with_bounds(self.row_rect(panel, index))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.highlight == Some(index)),
                    );
                if let Some(detail) = &suggestion.detail {
                    row = row.with_description(detail);
                }
                node.children.push(row);
            }
            if self.suggestions.is_empty()
                && let Some(status) = &self.status
            {
                node.children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(status)
                        .with_live(Politeness::Polite)
                        .with_bounds(panel),
                );
            }
        }
        Some(node.with_keyed_action_routes(id))
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, path) = crate::doc::document(state, path);
        match action {
            Action::Focus | Action::Expand => {
                self.begin_edit();
                true
            }
            Action::Collapse => {
                self.cancel();
                true
            }
            _ => {
                let handled = Widget::perform_action(&mut self.input, action, state, path);
                if handled && matches!(action, Action::SetValue(_)) {
                    self.input.set_focus(true);
                    self.outcome = AutocompleteOutcome::None;
                    self.recompute();
                    self.open = true;
                    self.highlight = None;
                }
                handled
            }
        }
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::{Action, hash_key};
        let own_key = hash_key(&self.test_id().unwrap_or("autocomplete"));
        if key_path.is_empty() || key_path == [own_key] {
            return self.perform_action(action, state, path);
        }
        if !self.open || !matches!(action, Action::Activate) {
            return false;
        }
        let Some(value) = self
            .suggestions
            .iter()
            .take(self.max_visible)
            .find(|suggestion| {
                key_path.last()
                    == Some(&hash_key(&(
                        "autocomplete-suggestion",
                        suggestion.value.as_str(),
                    )))
            })
            .map(|suggestion| suggestion.value.clone())
        else {
            return false;
        };
        self.accept(value);
        true
    }
}

impl crate::widget::controls::ActionControls for Autocomplete {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        self.input.install_control_custody(domain, attachment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::Modifiers;

    #[test]
    fn suggestions_open_highlight_accept_or_cancel() {
        fn accessible_edits_update_suggestions_and_activate_the_selected_value() {
            use crate::a11y::{Action, hash_key};
            let mut ac = suggester_paths();
            let mut state = AutoCommit::new();
            assert!(ac.perform_action(
                &Action::SetValue("al".into()),
                &mut state,
                &automerge::ROOT
            ));
            assert_eq!(ac.suggestions().len(), 2);
            let node = ac.accessibility().unwrap();
            assert_eq!(node.children[1].name.as_deref(), Some("album"));
            let key = hash_key(&("autocomplete-suggestion", "album"));
            assert!(ac.perform_action_at(&Action::Activate, &[key], &mut state, &automerge::ROOT));
            assert_eq!(ac.text(), "album");
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::Accepted("album".into())
            );
            assert!(!ac.is_open());
            assert!(!ac.perform_action_at(&Action::Activate, &[key], &mut state, &automerge::ROOT));
        }

        fn typing_recomputes_and_opens_the_dropdown() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            // The field starts empty, so both letters insert normally (no select-all
            // to swallow the first one).
            drive(&mut ac, &char_key("a"));
            drive(&mut ac, &char_key("l"));
            assert_eq!(ac.text(), "al");
            let vals: Vec<&str> = ac.suggestions().iter().map(|s| s.value.as_str()).collect();
            assert_eq!(vals, ["alpha", "album"], "prefix filter applied");
            assert!(ac.is_open(), "dropdown opens as the user types");
        }

        fn arrows_move_highlight_and_enter_accepts_it() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a"));
            drive(&mut ac, &char_key("l"));
            // No highlight yet → Enter would commit the typed text. Arrow down arms
            // the first suggestion.
            assert!(ac.highlighted().is_none());
            assert!(drive(&mut ac, &named(NamedKey::ArrowDown)));
            assert_eq!(ac.highlighted().unwrap().value, "alpha");
            assert!(drive(&mut ac, &named(NamedKey::ArrowDown)));
            assert_eq!(ac.highlighted().unwrap().value, "album");
            assert!(drive(&mut ac, &named(NamedKey::Enter)));
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::Accepted("album".into())
            );
            assert_eq!(ac.text(), "album", "accepted value loaded into the field");
            assert!(!ac.is_open(), "accept closes the dropdown");
        }

        fn enter_without_a_highlight_commits_the_typed_text() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("b"));
            drive(&mut ac, &named(NamedKey::Enter));
            assert_eq!(ac.take_outcome(), AutocompleteOutcome::Accepted("b".into()));
        }

        fn escape_cancels() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a"));
            assert!(drive(&mut ac, &named(NamedKey::Escape)));
            assert_eq!(ac.take_outcome(), AutocompleteOutcome::Cancelled);
            assert!(!ac.is_open());
        }

        fn clicking_a_row_accepts_that_suggestion() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a")); // → "alpha","album"
            let panel = ac.dropdown_rect().expect("dropdown open");
            // Click the second row ("album").
            let row = ac.row_rect(panel, 1);
            let c = row.center();
            drive(&mut ac, &mouse_move(c.x, c.y));
            assert!(drive(&mut ac, &left_press()));
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::Accepted("album".into())
            );
        }

        fn tab_completes_without_accepting() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a")); // "alpha","album"
            // Tab fills the field with the first suggestion but does not commit.
            assert!(drive(&mut ac, &named(NamedKey::Tab)));
            assert_eq!(ac.text(), "alpha");
            assert_eq!(
                ac.take_outcome(),
                AutocompleteOutcome::None,
                "Tab is not an accept"
            );
        }

        fn click_outside_cancels_without_consuming() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a"));
            // Move far away, then press: cancels but is not swallowed.
            drive(&mut ac, &mouse_move(900.0, 900.0));
            let consumed = drive(&mut ac, &left_press());
            assert!(!consumed, "the outside click still reaches the host");
            assert_eq!(ac.take_outcome(), AutocompleteOutcome::Cancelled);
        }

        accessible_edits_update_suggestions_and_activate_the_selected_value();
        typing_recomputes_and_opens_the_dropdown();
        arrows_move_highlight_and_enter_accepts_it();
        enter_without_a_highlight_commits_the_typed_text();
        escape_cancels();
        clicking_a_row_accepts_that_suggestion();
        tab_completes_without_accepting();
        click_outside_cancels_without_consuming();
    }

    #[test]
    fn controlled_autocomplete_with_id_preserves_supplied_input() {
        let autocomplete = Autocomplete::new_with_controlled_input(
            TextInput::local_with_language("en_US")
                .with_spellcheck(false)
                .with_autocorrect(false)
                .with_initial_value("sentinel route"),
        )
        .with_id("route-search");

        assert_eq!(Widget::test_id(&autocomplete), Some("route-search"));
        let semantics = Widget::accessibility(&autocomplete).expect("text field semantics");
        // The autocomplete publishes one keyed route under its id for the
        // embedded input and its rows, never the inner input's own target.
        let keyed = semantics.action_target.as_deref().unwrap_or_default();
        assert!(
            keyed.starts_with("tilo.key-action:route-search:"),
            "{keyed}"
        );
        assert_eq!(semantics.semantic_key.as_deref(), Some(keyed));
        assert_eq!(autocomplete.text(), "sentinel route");
        assert_eq!(autocomplete.input.spell_language(), "en_US");
        assert!(!autocomplete.input.spellcheck_enabled());
        assert!(!autocomplete.input.autocorrect_enabled());
        assert_eq!(autocomplete.input.kind(), TextInputKind::Search);
    }

    fn suggester_paths() -> Autocomplete {
        Autocomplete::new().with_suggester(|q| {
            ["alpha", "album", "beta"]
                .iter()
                .filter(|s| s.starts_with(q))
                .map(|s| Suggestion::new(*s))
                .collect()
        })
    }

    fn char_key(s: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(winit::keyboard::SmolStr::new(s));
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(winit::keyboard::SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn named(k: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
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

    fn mouse_move(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn drive(ac: &mut Autocomplete, ev: &WindowEvent) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        let mut doc = AutoCommit::new();
        ac.handle_event(
            ev,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn begin_edit_selection_arms_only_for_preseeded_values() {
        fn freshly_focused_empty_field_keeps_every_letter() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a"));
            assert_eq!(
                ac.input.selection_range_public(),
                None,
                "no selection after first char"
            );
            drive(&mut ac, &char_key("b"));
            drive(&mut ac, &char_key("c"));
            assert_eq!(ac.text(), "abc", "every letter is kept (not \"bc\")");
        }

        fn begin_edit_on_empty_field_arms_no_selection() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            assert_eq!(
                ac.input.selection_range_public(),
                None,
                "empty focus = no selection"
            );
        }

        fn redundant_begin_edit_does_not_reselect_typed_text() {
            let mut ac = suggester_paths();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            drive(&mut ac, &char_key("a")); // field now holds "a"
            assert_eq!(ac.text(), "a");
            ac.begin_edit();
            assert_eq!(
                ac.input.selection_range_public(),
                None,
                "typed text is not re-selected"
            );
            drive(&mut ac, &char_key("l"));
            assert_eq!(
                ac.text(),
                "al",
                "second char appends — the first letter survives"
            );
        }

        fn begin_edit_still_selects_a_preseeded_value_for_replace() {
            // A genuine fresh entry into a pre-filled field (the path bar seeds
            // the current path) still selects all.
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.set_text("/home/user");
            ac.begin_edit();
            assert_eq!(
                ac.input.selection_range_public(),
                Some((0, "/home/user".len())),
                "fresh entry into a pre-seeded field selects the whole value",
            );
            drive(&mut ac, &char_key("x"));
            assert_eq!(
                ac.text(),
                "x",
                "the first keystroke replaces the selected seed"
            );
        }

        freshly_focused_empty_field_keeps_every_letter();
        begin_edit_on_empty_field_arms_no_selection();
        redundant_begin_edit_does_not_reselect_typed_text();
        begin_edit_still_selects_a_preseeded_value_for_replace();
    }

    #[test]
    fn reactive_suggestions_status_line_and_scaled_rows() {
        fn set_suggestions_supports_reactive_hosts() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            ac.set_suggestions(vec![Suggestion::labelled("/home/u/Docs", "Docs")]);
            ac.open = true;
            assert!(ac.is_open());
            assert_eq!(ac.suggestions()[0].value, "/home/u/Docs");
        }

        fn status_line_keeps_the_dropdown_open_with_no_rows() {
            // A reactive host shows a loading line while it has no rows yet.
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            assert!(ac.dropdown_rect().is_none(), "nothing to show yet");
            ac.set_status(Some("Searching…".into()));
            ac.open();
            let panel = ac.dropdown_rect().expect("status keeps the dropdown open");
            assert!(panel.height() > 0.0);
            assert!(ac.highlighted().is_none());
            assert!(
                ac.row_at(panel.center()).is_none(),
                "the status line is not a row"
            );
        }

        fn reactive_suggestions_after_open_replace_the_status_line() {
            // Results arriving async clear the status and become selectable rows.
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
            ac.begin_edit();
            ac.set_status(Some("Searching…".into()));
            ac.open();
            ac.set_suggestions(vec![Suggestion::new("Paris"), Suggestion::new("Parma")]);
            ac.open();
            assert!(ac.is_open());
            assert_eq!(ac.suggestions().len(), 2);
            drive(&mut ac, &named(NamedKey::ArrowDown));
            assert_eq!(ac.highlighted().unwrap().value, "Paris");
            drive(&mut ac, &named(NamedKey::ArrowDown));
            assert_eq!(ac.highlighted().unwrap().value, "Parma");
        }

        fn large_text_scales_dropdown_rows_and_hit_geometry_together() {
            let mut ac = Autocomplete::new();
            ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 68.0));
            ac.set_suggestions(vec![
                Suggestion::new("A very long destination name").with_detail("First detail"),
                Suggestion::new("Second destination").with_detail("Second detail"),
            ]);
            ac.open();
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let theme = Theme::dark().with_text_scale(2.0);
            text.set_text_scale(theme.text_scale);
            ac.draw_overlay(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::empty(),
            );
            let panel = ac.dropdown_rect().expect("large-text dropdown");
            let first = ac.row_rect(panel, 0);
            let second = ac.row_rect(panel, 1);
            assert_eq!(first.height(), ROW_H * 2.0);
            assert_eq!(first.y1, second.y0, "rows do not overlap");
            assert_eq!(ac.row_at(second.center()), Some(1));
        }

        fn scaled_theme_font_is_normalized_before_scaled_text_rendering() {
            let normal = Theme::dark();
            let large = Theme::dark().with_text_scale(2.0);

            assert_eq!(
                renderer_font_size(large.tokens.typography.scale.base, large.text_scale),
                normal.tokens.typography.scale.base,
                "theme and renderer must not both apply the accessibility text scale"
            );
            assert_eq!(
                renderer_font_size(large.tokens.typography.scale.sm, large.text_scale),
                normal.tokens.typography.scale.sm,
            );
        }

        set_suggestions_supports_reactive_hosts();
        status_line_keeps_the_dropdown_open_with_no_rows();
        reactive_suggestions_after_open_replace_the_status_line();
        large_text_scales_dropdown_rows_and_hit_geometry_together();
        scaled_theme_font_is_normalized_before_scaled_text_rendering();
    }
}
