//! `Autocomplete` — a text field with a suggestion dropdown that floats below
//! it.
//!
//! This is the shared, reusable "type-ahead" primitive: a real
//! [`TextInput`] (so it inherits selection, clipboard, IME, caret navigation
//! and theming for free — never a hand-rolled `String` buffer, per
//! `ui/HIG.md` §8) with a **suggestion popover** drawn in the overlay pass so
//! it composites *above* sibling content instead of being clipped by it.
//!
//! Any app attaches a **suggestion source** — a closure `Fn(&str) ->
//! Vec<Suggestion>` via [`with_suggester`](Autocomplete::with_suggester), or a
//! reactively-computed list via [`set_suggestions`](Autocomplete::set_suggestions).
//! The completions recompute whenever the typed text changes.
//!
//! ## Interaction
//!
//! * **Type** — recomputes suggestions and opens the dropdown.
//! * **Down / Up** — move the highlight through the list (opening it if closed).
//! * **Enter** — accept: the highlighted suggestion's value if one is
//!   highlighted, else the current typed text. Reports [`AutocompleteOutcome::Accepted`].
//! * **Tab** — *complete* the field to the highlighted (or first) suggestion
//!   without accepting, so the user can keep drilling down.
//! * **Click a row** — accept that suggestion.
//! * **Esc** — cancel, reporting [`AutocompleteOutcome::Cancelled`].
//! * **Click outside** — cancel (not consumed, so the click still reaches the
//!   host).
//!
//! The outcome is polled once via [`take_outcome`](Autocomplete::take_outcome)
//! (rising-edge, like [`TextInput::take_dirty`]): the host reads it after each
//! event to learn whether — and how — the edit ended.
//!
//! The editable path bar in the file manager is the canonical consumer, but the
//! widget knows nothing about paths: the suggestion source is entirely the
//! host's.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::{draw_icon, Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::{TextInput, Widget};

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
        Self { label: value.clone(), value, detail: None, icon: None }
    }

    /// A suggestion whose visible label differs from the inserted value.
    pub fn labelled(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self { value: value.into(), label: label.into(), detail: None, icon: None }
    }

    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

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
/// Height of the single non-interactive status row shown (loading / "no
/// matches") when a reactive host has opened the dropdown with no suggestions.
const STATUS_ROW_H: f64 = 34.0;
const DROPDOWN_GAP: f64 = 6.0;
const ROW_PAD_X: f64 = 12.0;
const ICON_PX: f64 = 16.0;
const DEFAULT_MAX_VISIBLE: usize = 8;

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
    /// A loading / "no matches" line shown in the dropdown when a reactive
    /// host has opened it but has no rows yet. `None` renders nothing.
    status: Option<String>,
    max_visible: usize,
    rect: Rect,
    last_pointer: Point,
    outcome: AutocompleteOutcome,
}

impl Autocomplete {
    pub fn new() -> Self {
        Self {
            id: String::new(),
            input: TextInput::local(),
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
        }
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self.input = std::mem::replace(&mut self.input, TextInput::local()).with_id(id);
        self
    }

    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.input.set_placeholder(hint);
        self
    }

    pub fn with_font_size(mut self, size: f32) -> Self {
        self.input.set_font_size(size);
        self
    }

    /// Attach a suggestion source computed from the current text.
    pub fn with_suggester<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Vec<Suggestion> + 'static,
    {
        self.suggester = Some(Box::new(f));
        self
    }

    /// Attach / replace the suggestion source after construction.
    pub fn set_suggester<F>(&mut self, f: F)
    where
        F: Fn(&str) -> Vec<Suggestion> + 'static,
    {
        self.suggester = Some(Box::new(f));
    }

    /// The most rows shown before the list scrolls its content (it clips, it
    /// does not grow without bound).
    pub fn with_max_visible(mut self, n: usize) -> Self {
        self.max_visible = n.max(1);
        self
    }

    /// Directly set the visible suggestions (for hosts that compute them
    /// reactively rather than through a [`Suggester`]). Re-clamps the highlight
    /// and clears any status line once real rows are available.
    pub fn set_suggestions(&mut self, suggestions: Vec<Suggestion>) {
        self.suggestions = suggestions;
        if !self.suggestions.is_empty() {
            self.status = None;
        }
        if let Some(h) = self.highlight {
            if h >= self.suggestions.len() {
                self.highlight = None;
            }
        }
    }

    /// Force the dropdown open. Reactive/async hosts call this after filling
    /// suggestions or a status line that arrived *after* the keystroke which
    /// requested them (a keystroke with no rows leaves the dropdown closed).
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Set a loading / "no matches" line shown in the dropdown while there are
    /// no suggestions to list. `None` clears it. Pair with [`open`](Autocomplete::open).
    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }

    /// Set the suggestions and, when the field is actively being edited, open
    /// the dropdown to show them (for reactive/async hosts that receive results
    /// after a round-trip). Closes the panel when the list is empty.
    pub fn set_suggestions_open(&mut self, suggestions: Vec<Suggestion>) {
        self.set_suggestions(suggestions);
        if self.is_editing() {
            self.open = !self.suggestions.is_empty();
        }
    }

    /// The current field text.
    pub fn text(&self) -> &str {
        self.input.value()
    }

    /// Replace the field text (does not open the dropdown or recompute).
    pub fn set_text(&mut self, s: &str) {
        self.input.set_text(s);
    }

    /// Enter edit mode: focus the field and recompute suggestions / open the
    /// dropdown.
    ///
    /// On the genuine **transition into editing** the whole value is selected,
    /// so the first keystroke replaces a pre-seeded value (e.g. the path bar's
    /// current path). Crucially this select-all runs **only** on that
    /// transition: a redundant `begin_edit` while the field is *already* being
    /// edited leaves the caret and selection untouched. Re-selecting here was
    /// the "first letter swallowed" bug — a focus/refresh path calling
    /// `begin_edit` again after the user typed one character would re-select
    /// that character, and the next keystroke would overwrite it, so the first
    /// letter silently vanished. An empty field selects nothing either way, so
    /// a freshly-focused empty search box always inserts normally with no
    /// spurious selection.
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

    /// Whether the field currently holds focus (is in an active edit).
    pub fn is_editing(&self) -> bool {
        self.input.is_focused()
    }

    /// Whether the suggestion dropdown is currently shown.
    pub fn is_open(&self) -> bool {
        self.open && !self.suggestions.is_empty()
    }

    /// The suggestions currently offered.
    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    /// The keyboard-highlighted suggestion, if one is armed.
    pub fn highlighted(&self) -> Option<&Suggestion> {
        self.highlight.and_then(|i| self.suggestions.get(i))
    }

    /// Pre-arm the keyboard highlight on a row (clamped to range; `None`
    /// clears). Reactive hosts use this to highlight the top match by default.
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

    /// Place the widget (and its embedded field) at `rect` without a taffy pass
    /// — for hosts embedding the field in their own immediate-mode chrome.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input.apply_layout_external(rect);
    }

    fn recompute(&mut self) {
        if let Some(f) = &self.suggester {
            self.suggestions = f(self.input.value());
        }
        if let Some(h) = self.highlight {
            if h >= self.suggestions.len() {
                self.highlight = None;
            }
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

    /// Complete the field to the highlighted (or first) suggestion, keeping the
    /// edit open so the user can drill further.
    fn complete_to_highlight(&mut self) {
        let idx = self.highlight.or(if self.suggestions.is_empty() { None } else { Some(0) });
        if let Some(i) = idx {
            if let Some(s) = self.suggestions.get(i) {
                let value = s.value.clone();
                self.input.set_text(&value);
                self.input.set_cursor_pos(value.chars().count());
                self.recompute();
                self.open = !self.suggestions.is_empty();
                self.highlight = None;
            }
        }
    }

    fn move_highlight(&mut self, delta: i64) {
        if self.suggestions.is_empty() {
            return;
        }
        self.open = true;
        let n = self.suggestions.len() as i64;
        let next = match self.highlight {
            None => {
                if delta > 0 {
                    0
                } else {
                    (n - 1) as usize
                }
            }
            Some(cur) => {
                let c = cur as i64 + delta;
                c.rem_euclid(n) as usize
            }
        };
        self.highlight = Some(next);
    }

    /// The dropdown panel rect (below the field), or `None` when it should not
    /// show. Shows for either real suggestion rows or a lone status line.
    fn dropdown_rect(&self) -> Option<Rect> {
        if !self.open || (self.suggestions.is_empty() && self.status.is_none()) {
            return None;
        }
        let h = if self.suggestions.is_empty() {
            STATUS_ROW_H + 8.0
        } else {
            self.suggestions.len().min(self.max_visible) as f64 * ROW_H + 8.0
        };
        Some(Rect::new(
            self.rect.x0,
            self.rect.y1 + DROPDOWN_GAP,
            self.rect.x1,
            self.rect.y1 + DROPDOWN_GAP + h,
        ))
    }

    fn row_rect(&self, panel: Rect, i: usize) -> Rect {
        let y0 = panel.y0 + 4.0 + i as f64 * ROW_H;
        Rect::new(panel.x0 + 4.0, y0, panel.x1 - 4.0, y0 + ROW_H)
    }

    /// The dropdown row under a logical point, if any.
    fn row_at(&self, pt: Point) -> Option<usize> {
        let panel = self.dropdown_rect()?;
        if !panel.contains(pt) {
            return None;
        }
        for i in 0..self.suggestions.len().min(self.max_visible) {
            if self.row_rect(panel, i).contains(pt) {
                return Some(i);
            }
        }
        None
    }
}

impl Default for Autocomplete {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Autocomplete {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::length(34.0) },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        // The field itself; the suggestion list floats in `draw_overlay`.
        self.input.apply_layout_external(self.rect);
        self.input.draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let Some(panel) = self.dropdown_rect() else { return };
        let tokens = &theme.tokens;
        let accent = tokens.colors.accent.stops[ACCENT_PRIMARY_IDX];
        let shape = RoundedRect::from_rect(panel, tokens.radii.md as f64);
        // Raised panel + hairline border, matching the toolkit's other popovers.
        scene.fill(Fill::NonZero, transform, tokens.colors.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), transform, tokens.colors.border_standard, None, &shape);

        let text_size = tokens.typography.scale.base;
        let detail_size = tokens.typography.scale.sm;

        // No rows yet: render the lone status line (loading / no matches).
        if self.suggestions.is_empty() {
            if let Some(status) = &self.status {
                let ty = panel.y0 + (panel.height() - text_size as f64) / 2.0;
                text_renderer.draw_text(
                    scene,
                    status,
                    text_size,
                    tokens.colors.text_dim,
                    transform * Affine::translate((panel.x0 + ROW_PAD_X + 4.0, ty)),
                    Some((panel.width() - 2.0 * (ROW_PAD_X + 4.0)).max(0.0) as f32),
                );
            }
            return;
        }
        for i in 0..self.suggestions.len().min(self.max_visible) {
            let s = &self.suggestions[i];
            let row = self.row_rect(panel, i);
            let armed = self.highlight == Some(i);
            let hovered = self.hover == Some(i) && !armed;
            if armed {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    accent,
                    None,
                    &RoundedRect::from_rect(row, tokens.radii.sm as f64),
                );
            } else if hovered {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    tokens.colors.surface_white_10,
                    None,
                    &RoundedRect::from_rect(row, tokens.radii.sm as f64),
                );
            }
            let on_accent = tokens.colors.on_accent(crate::tokens::colors::ON_FILL_MIN_CONTRAST);
            let text_color = if armed { on_accent } else { tokens.colors.text_primary };
            // The detail column is the quieter tier. Softening it used to be
            // done by dropping the white to 80% alpha, which on a bright
            // accent takes an already-marginal label below anything legible.
            // Soften toward the fill instead, then hold it to the floor.
            let dim_color = if armed {
                tokens.colors.ink_on_any(
                    &[accent],
                    crate::tokens::colors::over(accent, on_accent, 0.8),
                    crate::tokens::colors::ON_FILL_MIN_CONTRAST,
                )
            } else {
                tokens.colors.text_dim
            };
            let mut x = row.x0 + ROW_PAD_X;
            if let Some(icon) = s.icon {
                let iy = row.y0 + (ROW_H - ICON_PX) / 2.0;
                draw_icon(
                    scene,
                    transform,
                    Rect::new(x, iy, x + ICON_PX, iy + ICON_PX),
                    icon,
                    if armed { on_accent } else { tokens.colors.text_dim },
                    LUCIDE_DEFAULT_STROKE_PX,
                );
                x += ICON_PX + 10.0;
            }
            let ty = row.y0 + (ROW_H - text_size as f64) / 2.0;
            // Right-aligned detail first, so we can bound the label width.
            let mut label_x1 = row.x1 - ROW_PAD_X;
            if let Some(detail) = &s.detail {
                let (dw, _) = text_renderer.measure(detail, detail_size, None);
                let dx = row.x1 - ROW_PAD_X - dw as f64;
                text_renderer.draw_text(
                    scene,
                    detail,
                    detail_size,
                    dim_color,
                    transform * Affine::translate((dx, row.y0 + (ROW_H - detail_size as f64) / 2.0)),
                    None,
                );
                label_x1 = dx - 8.0;
            }
            text_renderer.draw_text(
                scene,
                &s.label,
                text_size,
                text_color,
                transform * Affine::translate((x, ty)),
                Some((label_x1 - x).max(0.0) as f32),
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.hover = self.row_at(self.last_pointer);
        }
        if !self.is_editing() {
            return;
        }
        match event {
            // Navigation / accept / cancel keys the field itself never consumes,
            // so the wrapper owns them. Intercept before forwarding.
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => match logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    self.move_highlight(1);
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.move_highlight(-1);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    let value = self
                        .highlighted()
                        .map(|s| s.value.clone())
                        .unwrap_or_else(|| self.input.value().to_string());
                    self.accept(value);
                    *handled = true;
                }
                Key::Named(NamedKey::Tab) if !self.suggestions.is_empty() => {
                    self.complete_to_highlight();
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel();
                    *handled = true;
                }
                _ => {
                    // A real keystroke: forward to the field, then recompute if
                    // the text changed.
                    self.input.handle_event(
                        event, scale_factor, modifiers, state, root, handled, focus_target,
                        node_id, taffy,
                    );
                    if self.input.take_dirty().is_some() {
                        self.recompute();
                        self.open = !self.suggestions.is_empty();
                        self.highlight = None;
                    }
                }
            },
            // A left press on a dropdown row accepts it.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.row_at(self.last_pointer).is_some() => {
                if let Some(i) = self.row_at(self.last_pointer) {
                    let value = self.suggestions[i].value.clone();
                    self.accept(value);
                    *handled = true;
                }
            }
            // A left press outside the field and the dropdown cancels. Not
            // consumed, so the click still reaches whatever is under it.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.rect.contains(self.last_pointer)
                && self.dropdown_rect().map(|r| !r.contains(self.last_pointer)).unwrap_or(true) =>
            {
                self.cancel();
            }
            _ => {
                self.input.handle_event(
                    event, scale_factor, modifiers, state, root, handled, focus_target, node_id,
                    taffy,
                );
                if self.input.take_dirty().is_some() {
                    self.recompute();
                    self.open = !self.suggestions.is_empty();
                    self.highlight = None;
                }
            }
        }
    }

    fn name(&self) -> &str {
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

    fn wants_pointer_capture(&self) -> bool {
        self.input.is_dragging()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.input.accessibility()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut doc = AutoCommit::new();
        ac.handle_event(
            ev, 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT, &mut handled,
            &mut focus_target, node, &taffy,
        );
        handled
    }

    // ---- reproduction: first-letter swallow --------------------------------

    #[test]
    fn freshly_focused_empty_field_keeps_every_letter() {
        // The reported bug: the first typed letter is swallowed. Root cause to
        // check: a spurious selection at focus that the first char replaces, so
        // the second char overwrites it — net first-letter loss.
        let mut ac = Autocomplete::new();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        drive(&mut ac, &char_key("a"));
        // After the very first char there must be NO active selection, or the
        // next char would replace it.
        assert_eq!(ac.input.selection_range_public(), None, "no selection after first char");
        drive(&mut ac, &char_key("b"));
        drive(&mut ac, &char_key("c"));
        assert_eq!(ac.text(), "abc", "every letter is kept (not \"bc\")");
    }

    #[test]
    fn begin_edit_on_empty_field_arms_no_selection() {
        // Focusing an empty field must not leave a whole-field selection armed.
        let mut ac = Autocomplete::new();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        assert_eq!(ac.input.selection_range_public(), None, "empty focus = no selection");
    }

    #[test]
    fn redundant_begin_edit_does_not_reselect_typed_text() {
        // The exact "first letter swallowed" symptom: a focus/refresh path calls
        // begin_edit AGAIN after the user has typed one character. It must not
        // re-select that character — otherwise the next keystroke replaces it and
        // the first letter is lost.
        let mut ac = suggester_paths();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        drive(&mut ac, &char_key("a")); // field now holds "a"
        assert_eq!(ac.text(), "a");
        // A redundant begin_edit while already editing must be inert w.r.t. the
        // selection (no whole-field re-select of the typed "a").
        ac.begin_edit();
        assert_eq!(ac.input.selection_range_public(), None, "typed text is not re-selected");
        drive(&mut ac, &char_key("l"));
        assert_eq!(ac.text(), "al", "second char appends — the first letter survives");
    }

    #[test]
    fn begin_edit_still_selects_a_preseeded_value_for_replace() {
        // The intended replace-on-type for a pre-filled field (the path bar seeds
        // the current path) is preserved: a genuine fresh entry selects all, so
        // the first keystroke replaces the seed.
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
        assert_eq!(ac.text(), "x", "the first keystroke replaces the selected seed");
    }

    #[test]
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

    #[test]
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
        // Enter accepts the highlighted suggestion.
        assert!(drive(&mut ac, &named(NamedKey::Enter)));
        assert_eq!(ac.take_outcome(), AutocompleteOutcome::Accepted("album".into()));
        assert_eq!(ac.text(), "album", "accepted value loaded into the field");
        assert!(!ac.is_open(), "accept closes the dropdown");
    }

    #[test]
    fn enter_without_a_highlight_commits_the_typed_text() {
        let mut ac = suggester_paths();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        drive(&mut ac, &char_key("b"));
        drive(&mut ac, &named(NamedKey::Enter));
        assert_eq!(ac.take_outcome(), AutocompleteOutcome::Accepted("b".into()));
    }

    #[test]
    fn escape_cancels() {
        let mut ac = suggester_paths();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        drive(&mut ac, &char_key("a"));
        assert!(drive(&mut ac, &named(NamedKey::Escape)));
        assert_eq!(ac.take_outcome(), AutocompleteOutcome::Cancelled);
        assert!(!ac.is_open());
    }

    #[test]
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
        assert_eq!(ac.take_outcome(), AutocompleteOutcome::Accepted("album".into()));
    }

    #[test]
    fn tab_completes_without_accepting() {
        let mut ac = suggester_paths();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        drive(&mut ac, &char_key("a")); // "alpha","album"
        // Tab fills the field with the first suggestion but does not commit.
        assert!(drive(&mut ac, &named(NamedKey::Tab)));
        assert_eq!(ac.text(), "alpha");
        assert_eq!(ac.take_outcome(), AutocompleteOutcome::None, "Tab is not an accept");
    }

    #[test]
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

    #[test]
    fn set_suggestions_supports_reactive_hosts() {
        let mut ac = Autocomplete::new();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        ac.set_suggestions(vec![Suggestion::labelled("/home/u/Docs", "Docs")]);
        ac.open = true;
        assert!(ac.is_open());
        assert_eq!(ac.suggestions()[0].value, "/home/u/Docs");
    }

    #[test]
    fn status_line_keeps_the_dropdown_open_with_no_rows() {
        // A reactive host (async suggestions) shows an honest loading / empty
        // line in the shared overlay while it has no rows to list yet.
        let mut ac = Autocomplete::new();
        ac.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 34.0));
        ac.begin_edit();
        assert!(ac.dropdown_rect().is_none(), "nothing to show yet");
        ac.set_status(Some("Searching…".into()));
        ac.open();
        let panel = ac.dropdown_rect().expect("status keeps the dropdown open");
        assert!(panel.height() > 0.0);
        // A lone status line offers no selectable rows.
        assert!(ac.highlighted().is_none());
        assert!(ac.row_at(panel.center()).is_none(), "the status line is not a row");
    }

    #[test]
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
        // Arrow-key navigation now works over the reactively-supplied rows.
        drive(&mut ac, &named(NamedKey::ArrowDown));
        assert_eq!(ac.highlighted().unwrap().value, "Paris");
        drive(&mut ac, &named(NamedKey::ArrowDown));
        assert_eq!(ac.highlighted().unwrap().value, "Parma");
    }
}
