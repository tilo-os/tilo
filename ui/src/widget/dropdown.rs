//! [`Dropdown`] — a reusable, CRDT-backed single-select control.
//!
//! Closed it reads like a button: the current selection plus a chevron. Open
//! it reveals a scrollable list that composites *on top of* following content
//! via the App's top-layer [`Widget::draw_overlay`] pass — a real overlay,
//! not an inline expander that shoves siblings down.
//!
//! Like [`crate::widget::TextInput`], the chosen value can live in either an
//! explicit durable or process-local model. [`Dropdown::new`] takes a CRDT
//! `value_path` and splices selection into the Automerge `Text` object there;
//! [`Dropdown::from_state`] binds a typed [`crate::LocalState<String>`]. In
//! both forms `sync` mirrors model changes back without app-owned fan-out.
//!
//! Keyboard (while focused): Enter/Space/Down open; Up/Down move the
//! highlight; Enter selects + closes; Esc dismisses; printable characters
//! drive an incremental, case-insensitive substring filter (Backspace edits
//! it) so hundred-entry lists stay usable without endless scrolling.
//!
//! While open the widget reports [`Widget::wants_pointer_capture`] so its
//! container routes all events to it exclusively — how the out-of-layout
//! list becomes hit-testable and click-outside dismissal works.

use std::collections::HashMap;

use crate::widget::TaffyTreeExt;
use automerge::{
    AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange};
use crate::text_render::{TextRenderer, TextStyle};
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};

/// Height of the closed control (matches `TextInput` / `SegChoice`).
const CONTROL_HEIGHT: f64 = 40.0;
const ITEM_HEIGHT: f64 = 32.0;
const MAX_LIST_HEIGHT: f64 = 240.0;
const LIST_GAP: f64 = 4.0;
const PAD_X: f64 = 10.0;
const CHEVRON_W: f64 = 30.0;
const FONT: f32 = 15.0;
const LINE_PX: f32 = 60.0;

type SelectionChangeCallback = Box<dyn FnMut(&str)>;

/// Widens a row/option count to `f64` for layout math, through `u32` (exact
/// in `f64`) with a saturating fallback rather than an `as` cast — a
/// dropdown never approaches `u32::MAX` options.
fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Narrows a bounded screen-space `f64` (row/pixel geometry) to `f32` for
/// vello/taffy; dropdown content stays far below `f32::MAX`.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space pixel values in this widget never approach f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` is pure
/// `f64` matrix math with no overflow trap, so it cannot panic regardless of
/// operand magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (open/hover/focus/enabled/etc), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct Dropdown {
    id: String,
    /// Fluent message id spoken independently from the selected value.
    accessible_name: Option<String>,
    options: Vec<String>,
    value_path: String,
    local_state: Option<crate::local_state::LocalBinding<String>>,
    on_change: Option<SelectionChangeCallback>,
    action: Option<ActionEmitter<String>>,
    placeholder: String,

    /// Mirror of the CRDT value at `value_path`. Empty when unset.
    selected: String,
    open: bool,
    /// Position (0-based) into the *filtered* list of the highlighted row.
    highlight: usize,
    /// Incremental substring filter; empty shows every option.
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    scroll: f32,

    /// One-shot: align `highlight` to the selected option on the next
    /// `sync`. Set by the `open()` builder, where `selected` isn't known
    /// until `sync` reads the document.
    align_highlight_on_sync: bool,

    rect: Rect,
    is_focused: bool,
    is_hovered: bool,
    last_cursor: Point,

    /// When `false`: skipped by focus, rejects input, renders dimmed.
    /// Default `true`.
    enabled: bool,
}

impl Dropdown {
    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    /// Construct a dropdown over `options`, writing the chosen value into
    /// the CRDT `Text` object at `value_path`.
    pub fn new(options: Vec<String>, value_path: &str) -> Self {
        Self {
            id: String::new(),
            accessible_name: None,
            options,
            value_path: value_path.to_string(),
            local_state: None,
            on_change: None,
            action: None,
            placeholder: String::new(),
            selected: String::new(),
            open: false,
            highlight: 0,
            filter: String::new(),
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
            scroll: 0.0,
            align_highlight_on_sync: false,
            rect: Rect::ZERO,
            is_focused: false,
            is_hovered: false,
            last_cursor: Point::ZERO,
            enabled: true,
        }
    }

    /// Create a dropdown bound to typed process-local selected state.
    ///
    /// User selection updates `state`; mutations through any clone update
    /// this dropdown on the next synchronization pass. The selected string is
    /// intentionally not restricted to `options`, so asynchronously loaded
    /// choices can preserve their model value while options catch up.
    pub fn from_state(options: Vec<String>, state: &crate::LocalState<String>) -> Self {
        let (selected, binding) = crate::local_state::LocalBinding::new(state);
        Self {
            selected,
            local_state: Some(binding),
            ..Self::new(options, "")
        }
    }

    /// Observe selections produced by user interaction.
    ///
    /// Model synchronization and [`Self::set_selected`] do not invoke this
    /// callback.
    #[must_use]
    pub fn on_change(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(callback));
        }
        self
    }

    /// Route every committed selection through an exported resource action.
    #[must_use]
    pub fn on_action(mut self, binding: qos_actions::ActionBinding<String>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.on_change = None;
        self
    }

    /// Stable identifier for the e2e harness (`Widget::test_id`).
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    /// Name the dropdown for assistive technology.
    ///
    /// The selected option remains the accessible value, so changing the
    /// selection never changes the label.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Text shown (dimmed) when no value is selected yet.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_string();
        self
    }

    /// Set the enabled state at build time; see the `enabled` field.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Imperatively flip the enabled state. Disabling an open dropdown
    /// also closes it.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.close();
        }
    }

    /// Start opened. Used for headless `--shots` of the open state; the
    /// highlight is aligned to the selected option on the first `sync`.
    #[must_use]
    pub fn open(mut self) -> Self {
        self.open = true;
        self.align_highlight_on_sync = true;
        self
    }

    /// Conditionally start opened — convenience for callers that decide
    /// at build time (e.g. a shot seed).
    #[must_use]
    pub fn open_if(self, cond: bool) -> Self {
        if cond { self.open() } else { self }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn selected(&self) -> &str {
        &self.selected
    }

    /// Set the selected model value programmatically.
    ///
    /// A bound [`crate::LocalState`] is updated immediately; this does not
    /// invoke [`Self::on_change`].
    pub fn set_selected(&mut self, selected: impl Into<String>) {
        self.selected = selected.into();
        if self.action.is_none() {
            self.publish_local_state();
        }
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.local_state {
            binding.publish(&self.selected);
        }
    }

    /// Place the dropdown at an absolute rect without going through taffy —
    /// the counterpart to [`TextInput::apply_layout_external`]. The open
    /// list still paints in the overlay pass, so only the closed control's
    /// box is caller-driven; focus / a11y / CRDT-bound selection are unaffected.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }
    /// Position (0-based) of the highlighted row in the filtered list.
    pub fn highlight_index(&self) -> usize {
        self.highlight
    }
    /// Current filter query (empty when not filtering).
    pub fn filter_query(&self) -> &str {
        &self.filter
    }

    /// Indices into `self.options` matching the current filter, in order.
    fn filtered(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            (0..self.options.len()).collect()
        } else {
            let q = self.filter.to_lowercase();
            self.options
                .iter()
                .enumerate()
                .filter(|(_, o)| o.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        }
    }

    /// Number of options currently visible (after filtering).
    pub fn visible_count(&self) -> usize {
        self.filtered().len()
    }

    /// Read the value at `value_path` (Text object or scalar string).
    fn read_value(state: &AutoCommit, root: &ObjId, path: &str) -> Option<String> {
        match state.get(root, path) {
            Ok(Some((Value::Object(ObjType::Text), obj))) => state.text(&obj).ok(),
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(t) => Some(t.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Position of the selected option within `filtered`, or 0.
    fn selected_pos(&self, filtered: &[usize]) -> usize {
        filtered
            .iter()
            .position(|&i| self.options.get(i).is_some_and(|o| *o == self.selected))
            .unwrap_or(0)
    }

    /// Height of the open list box for `n` filtered rows (>=1 so the
    /// "no matches" row has a box to live in).
    fn list_box_height(n: usize) -> f64 {
        (count_f64(n.max(1)) * ITEM_HEIGHT).min(MAX_LIST_HEIGHT)
    }

    /// Absolute rect of the open list (drawn below the control).
    fn list_rect(&self, n: usize) -> Rect {
        let h = Self::list_box_height(n);
        let y0 = self.rect.y1 + LIST_GAP;
        Rect::new(self.rect.x0, y0, self.rect.x1, y0 + h)
    }

    fn max_scroll(n: usize) -> f32 {
        let content = count_f64(n) * ITEM_HEIGHT;
        px((content - Self::list_box_height(n)).max(0.0))
    }

    /// Filtered-list row index under `pos`, or `None` outside `list`/`n` rows.
    fn row_at(&self, pos: Point, list: Rect, n: usize) -> Option<usize> {
        if !list.contains(pos) {
            return None;
        }
        let rel = pos.y - list.y0 + f64::from(self.scroll);
        if rel < 0.0 {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rel >= 0.0 is checked above; this cast is the intended \
                      floor-to-row-index conversion"
        )]
        let vi = (rel / ITEM_HEIGHT) as usize;
        (vi < n).then_some(vi)
    }

    /// Scroll so the row at filtered-position `vi` is fully visible.
    fn ensure_visible(&mut self, vi: usize, n: usize) {
        let box_h = px(Self::list_box_height(n));
        let top = px(count_f64(vi) * ITEM_HEIGHT);
        let bottom = top + px(ITEM_HEIGHT);
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + box_h {
            self.scroll = bottom - box_h;
        }
        self.scroll = self.scroll.clamp(0.0, Self::max_scroll(n));
    }

    /// Open the list and align highlight + scroll to the current value.
    fn do_open(&mut self) {
        self.open = true;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        let filtered = self.filtered();
        self.highlight = self.selected_pos(&filtered);
        self.scroll = 0.0;
        self.ensure_visible(self.highlight, filtered.len());
    }

    fn close(&mut self) {
        self.open = false;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        self.scroll = 0.0;
    }

    /// Commit `value` into the CRDT `Text` at `value_path` (creating the
    /// object if needed) and mirror it locally.
    fn commit_value(&mut self, value: &str, state: &mut AutoCommit, root: &ObjId) {
        if let Some(action) = &mut self.action {
            let _ = action.emit(value.to_string());
        } else if self.local_state.is_none() {
            let obj = match state.get(root, self.value_path.as_str()) {
                Ok(Some((Value::Object(ObjType::Text), obj))) => Some(obj),
                _ => state
                    .put_object(root, self.value_path.as_str(), ObjType::Text)
                    .ok(),
            };
            if let Some(obj) = obj {
                crate::doc::splice_text_diffed(state, &obj, value);
            } else {
                // Last resort: store as a scalar string (still readable).
                let _ = state.put(root, self.value_path.as_str(), value);
            }
        }
        self.selected = value.to_string();
        if self.action.is_none() {
            self.publish_local_state();
        }
        if self.action.is_none()
            && let Some(callback) = self.on_change.as_mut()
        {
            callback(value);
        }
    }

    /// Select the highlighted option (if any) and close.
    fn select_highlighted(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let filtered = self.filtered();
        if let Some(&opt_idx) = filtered.get(self.highlight)
            && let Some(value) = self.options.get(opt_idx).cloned()
        {
            self.commit_value(&value, state, root);
        }
        self.close();
    }

    /// `WindowEvent::CursorMoved` — hover tracking, plus (while open)
    /// following the highlight to the row under the pointer.
    fn on_cursor_moved(&mut self, pos: Point, handled: &mut bool) {
        self.last_cursor = pos;
        let hover = self.rect.contains(pos);
        if hover != self.is_hovered {
            self.is_hovered = hover;
            *handled = true;
        }
        // While open, hovering a row moves the highlight.
        if self.open {
            let filtered = self.filtered();
            let list = self.list_rect(filtered.len());
            if let Some(vi) = self.row_at(pos, list, filtered.len())
                && vi != self.highlight
            {
                self.highlight = vi;
                *handled = true;
            }
        }
    }

    /// `WindowEvent::MouseInput { state: Pressed, button: Left }` — select a
    /// clicked row, close on a click outside the list, or open when closed.
    fn on_left_press(
        &mut self,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        let pos = self.last_cursor;
        if self.open {
            let filtered = self.filtered();
            let list = self.list_rect(filtered.len());
            if list.contains(pos) {
                if let Some(vi) = self.row_at(pos, list, filtered.len()) {
                    self.highlight = vi;
                    self.select_highlighted(state, root);
                }
                *handled = true;
            } else if self.rect.contains(pos) {
                // Click the control again → close.
                self.close();
                *handled = true;
            } else {
                // Click-outside dismissal.
                self.close();
                *handled = true;
            }
        } else if self.rect.contains(pos) {
            *focus_target = Some(node_id);
            self.do_open();
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel`, only routed here while open.
    fn on_wheel(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => *y * -LINE_PX,
            MouseScrollDelta::PixelDelta(p) => px(-p.y),
        };
        let n = self.filtered().len();
        self.scroll = (self.scroll + dy).clamp(0.0, Self::max_scroll(n));
        *handled = true;
    }

    /// `WindowEvent::KeyboardInput`, guarded by the caller to only fire
    /// while focused and on key-down: open/closed navigation, select,
    /// dismiss, and the filter-query fallback.
    fn on_key(
        &mut self,
        event: &WindowEvent,
        ke: &KeyEvent,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
    ) {
        let logical_key = &ke.logical_key;
        if self.open {
            let mods = modifiers.state();
            let shift = mods.shift_key();
            let ctrl = mods.control_key() || mods.super_key();
            match logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    let n = self.filtered().len();
                    if n > 0 {
                        self.highlight = self.highlight.saturating_add(1).min(n.saturating_sub(1));
                        self.ensure_visible(self.highlight, n);
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let n = self.filtered().len();
                    self.highlight = self.highlight.saturating_sub(1);
                    self.ensure_visible(self.highlight, n);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.select_highlighted(state, root);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    *handled = true;
                }
                _ => {
                    let change = self.filter_composition.handle_event(
                        event,
                        &mut self.filter,
                        &mut self.filter_selection,
                        shift,
                        ctrl,
                    );
                    if change == QueryEventChange::Edited {
                        self.highlight = 0;
                        self.scroll = 0.0;
                    }
                    if change != QueryEventChange::Ignored {
                        *handled = true;
                    }
                }
            }
        } else {
            // Closed: Enter / Space / Down (or Up) open the list.
            if let Key::Named(
                NamedKey::Enter | NamedKey::Space | NamedKey::ArrowDown | NamedKey::ArrowUp,
            ) = logical_key
            {
                self.do_open();
                *handled = true;
            }
        }
    }
}

impl Widget for Dropdown {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(binding) = &mut self.local_state {
            if let Some(selected) = binding.poll() {
                self.selected = selected;
            }
        } else if let Some(value) = Self::read_value(state, root, self.value_path.as_str()) {
            self.selected = value;
        }
        if self.align_highlight_on_sync {
            self.align_highlight_on_sync = false;
            let filtered = self.filtered();
            self.highlight = self.selected_pos(&filtered);
            self.ensure_visible(self.highlight, filtered.len());
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The open list is intentionally NOT a taffy child: it paints in the
        // overlay pass, so it doesn't reserve layout space.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(px(CONTROL_HEIGHT)),
            },
            flex_shrink: 0.0,
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.md));
        // Disabled widgets paint their (forced-idle) look under a reduced
        // group opacity so they read as non-interactive.
        let disabled = !self.effectively_enabled();
        if disabled {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                crate::widget::DISABLED_ALPHA,
                transform,
                &shape,
            );
        }
        let bg = if self.open || self.is_focused {
            theme.bg_active
        } else {
            theme.tokens.colors.surfaces.raised
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        let (bw, bc) = if self.open || self.is_focused {
            (2.0, theme.border_focus)
        } else {
            (1.0, theme.border_standard)
        };
        scene.stroke(&Stroke::new(bw), transform, bc, None, &shape);

        if self.open {
            self.filter_composition.draw(
                scene,
                transform,
                text_renderer,
                &self.filter,
                &self.filter_selection,
                "Type to filter…",
                Rect::new(
                    self.rect.x0 + PAD_X,
                    self.rect.y0,
                    self.rect.x1 - CHEVRON_W,
                    self.rect.y1,
                ),
                &TextStyle::new(FONT),
                theme.text_primary,
                theme.text_dim,
                theme.accent_primary.with_alpha_factor(0.3),
                theme.accent_primary,
                self.is_focused,
            );
        } else {
            let (label, color) = if self.selected.is_empty() {
                (self.placeholder.as_str(), theme.text_dim)
            } else {
                (self.selected.as_str(), theme.text_primary)
            };
            if !label.is_empty() {
                scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
                let (_, th) = text_renderer.measure(label, FONT, None);
                let ty = self.rect.y0 + (self.rect.height() - f64::from(th)) / 2.0;
                text_renderer.draw_text(
                    scene,
                    label,
                    FONT,
                    color,
                    translated(transform, self.rect.x0 + PAD_X, ty),
                    Some(px(self.rect.width() - PAD_X - CHEVRON_W)),
                );
                scene.pop_layer();
            }
        }

        let cx = self.rect.x1 - 18.0;
        let cy = self.rect.center().y;
        let mut chev = BezPath::new();
        if self.open {
            chev.move_to((cx - 5.0, cy + 3.0));
            chev.line_to((cx, cy - 3.0));
            chev.line_to((cx + 5.0, cy + 3.0));
        } else {
            chev.move_to((cx - 5.0, cy - 3.0));
            chev.line_to((cx, cy + 3.0));
            chev.line_to((cx + 5.0, cy - 3.0));
        }
        scene.stroke(&Stroke::new(2.0), transform, theme.text_dim, None, &chev);

        if disabled {
            scene.pop_layer();
        }
    }

    /// Top-layer pass: paint the open list above all later-drawn
    /// siblings. The App composites this after the normal `draw` pass.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        let filtered = self.filtered();
        let list = self.list_rect(filtered.len());
        let shape = RoundedRect::from_rect(list, f64::from(theme.tokens.radii.md));

        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &shape,
        );

        // Clip rows to the rounded list box.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);

        if filtered.is_empty() {
            let ty = list.y0 + (ITEM_HEIGHT - f64::from(FONT)) / 2.0;
            text_renderer.draw_text(
                scene,
                "No matches",
                FONT,
                theme.text_dim,
                translated(transform, list.x0 + PAD_X, ty),
                None,
            );
        } else {
            for (vi, &opt_idx) in filtered.iter().enumerate() {
                let row_top = list.y0 + (count_f64(vi) * ITEM_HEIGHT) - f64::from(self.scroll);
                let row_bottom = row_top + ITEM_HEIGHT;
                // Cull rows scrolled out of the box (hundreds of zones).
                if row_bottom < list.y0 || row_top > list.y1 {
                    continue;
                }
                let Some(opt) = self.options.get(opt_idx) else {
                    continue;
                };
                let row = Rect::new(list.x0, row_top, list.x1, row_bottom);
                let is_selected = *opt == self.selected;
                if vi == self.highlight {
                    scene.fill(Fill::NonZero, transform, theme.accent_primary, None, &row);
                } else if is_selected {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.accent_primary.with_alpha_factor(0.18),
                        None,
                        &row,
                    );
                }
                let color = if vi == self.highlight {
                    theme
                        .tokens
                        .colors
                        .on_accent(crate::tokens::colors::ON_FILL_MIN_CONTRAST)
                } else {
                    theme.text_primary
                };
                let ty = row_top + (ITEM_HEIGHT - f64::from(FONT)) / 2.0;
                text_renderer.draw_text(
                    scene,
                    opt,
                    FONT,
                    color,
                    translated(transform, list.x0 + PAD_X, ty),
                    Some(px(list.width() - PAD_X * 2.0)),
                );
            }
        }
        scene.pop_layer();
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
        _taffy: &TaffyTree<()>,
    ) {
        // Disabled: reject all input — won't open, no selection, no hover.
        if !self.effectively_enabled() {
            return;
        }
        if self.open
            && self.is_focused
            && let WindowEvent::Ime(_) = event
        {
            let change = self.filter_composition.handle_event(
                event,
                &mut self.filter,
                &mut self.filter_selection,
                false,
                false,
            );
            if change == QueryEventChange::Edited {
                self.highlight = 0;
                self.scroll = 0.0;
            }
            *handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.on_cursor_moved(pos, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(state, root, handled, focus_target, node_id);
            }
            WindowEvent::MouseWheel { delta, .. } if self.open => {
                self.on_wheel(delta, handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    ke @ KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                self.on_key(event, ke, *modifiers, state, root, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Dropdown"
    }

    fn take_control_invocation(&mut self) -> Option<qos_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<qos_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
    }

    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("dropdown");
        if let Some(action) = &self.action {
            vec![action.metadata(control_id)]
        } else if !self.value_path.is_empty() {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::CollaborativeDocument(self.value_path.clone()),
            }]
        } else if self.on_change.is_some() {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::LegacyCallback,
            }]
        } else {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::SemanticAction(control_id.to_string()),
            }]
        }
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("dropdown")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        // Losing focus (e.g. Tab away) dismisses the open list.
        if !focused {
            self.close();
        }
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.open
            .then(|| self.filter_composition.ime_cursor_rect())
            .flatten()
    }
    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }
    fn is_enabled(&self) -> bool {
        self.effectively_enabled()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Explicit names are Fluent ids. Keep the legacy value/placeholder
        // fallback for unlabeled standalone dropdowns.
        let name = if let Some(name) = &self.accessible_name {
            Some(crate::i18n!(name))
        } else if !self.selected.is_empty() {
            Some(self.selected.clone())
        } else if !self.placeholder.is_empty() {
            Some(self.placeholder.clone())
        } else {
            None
        };
        let value = if self.open && !self.filter.is_empty() {
            Some(self.filter.clone())
        } else {
            (!self.selected.is_empty()).then(|| self.selected.clone())
        };
        let mut node = AccessNode::new(Role::ComboBox)
            .with_optional_name(name)
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                expanded: Some(self.open),
                ..Default::default()
            });
        // Surface the chosen option's ordinal so the reader can say "2 of 5".
        if let Some(idx) = self.options.iter().position(|o| *o == self.selected) {
            node = node.with_position(idx.saturating_add(1), self.options.len());
        }
        if self.action.is_none() && self.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("dropdown"));
        }
        Some(node)
    }

    /// Accessibility and headless control of the combobox.
    ///
    /// Expand/collapse mirrors pointer opening, Activate toggles the popup,
    /// and `SetValue` selects one exact option through the same model mutation
    /// path as keyboard or pointer selection.
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        match action {
            Action::Expand => {
                self.do_open();
                true
            }
            Action::Collapse => {
                self.close();
                true
            }
            Action::Activate => {
                if self.open {
                    self.close();
                } else {
                    self.do_open();
                }
                true
            }
            Action::SetValue(value) => {
                let Some(value) = self.options.iter().find(|option| *option == value).cloned()
                else {
                    return false;
                };
                self.commit_value(&value, state, root);
                self.close();
                true
            }
            _ => false,
        }
    }

    /// While open, grabs every event — see module docs.
    fn wants_pointer_capture(&self) -> bool {
        self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::Modifiers;
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    fn opts() -> Vec<String> {
        ["UTC", "Europe/Amsterdam", "America/New_York", "Asia/Tokyo"]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// CRDT doc with `tz` seeded as a Text object holding `value`.
    fn doc_with_tz(value: &str) -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let obj = doc.put_object(&root, "tz", ObjType::Text).unwrap();
        let _ = doc.splice_text(&obj, 0, 0, value);
        (doc, root)
    }

    /// Synthesise a `Pressed` `KeyboardInput` (winit keeps `KeyEvent`'s
    /// extra field `pub(crate)`, so a struct literal isn't possible).
    fn key(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(SmolStr::new);
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    /// Drive a key through a focused dropdown with a throwaway taffy node.
    fn feed_key(dd: &mut Dropdown, ev: &WindowEvent, doc: &mut AutoCommit, root: &ObjId) -> bool {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        dd.handle_event(
            ev,
            1.0,
            &Modifiers::default(),
            doc,
            root,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn starts_closed_and_reads_value_on_sync() {
        let mut dd = Dropdown::new(opts(), "tz");
        assert!(!dd.is_open());
        let (mut doc, root) = doc_with_tz("Asia/Tokyo");
        dd.sync(&mut doc, &root);
        assert_eq!(dd.selected(), "Asia/Tokyo");
    }

    #[test]
    fn down_opens_when_closed_and_focused() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        assert!(feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root
        ));
        assert!(dd.is_open());
        // Highlight lands on the currently-selected option.
        assert_eq!(dd.highlight_index(), 0);
    }

    #[test]
    fn arrows_move_highlight_while_open() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root,
        );
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root,
        );
        assert_eq!(dd.highlight_index(), 2);
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowUp), None),
            &mut doc,
            &root,
        );
        assert_eq!(dd.highlight_index(), 1);
    }

    #[test]
    fn enter_selects_writes_value_and_closes() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        // Move to "America/New_York" (index 2) and select it.
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root,
        );
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root,
        );
        assert!(feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::Enter), None),
            &mut doc,
            &root
        ));
        assert!(!dd.is_open(), "selecting closes the list");
        assert_eq!(dd.selected(), "America/New_York");
        // The value landed in the CRDT Text object the host reads.
        assert_eq!(
            Dropdown::read_value(&doc, &root, "tz").as_deref(),
            Some("America/New_York")
        );
    }

    #[test]
    fn escape_closes_without_changing_value() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root,
        );
        assert!(feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::Escape), None),
            &mut doc,
            &root
        ));
        assert!(!dd.is_open());
        assert_eq!(dd.selected(), "UTC", "Esc must not change the value");
        assert_eq!(
            Dropdown::read_value(&doc, &root, "tz").as_deref(),
            Some("UTC")
        );
    }

    #[test]
    fn typing_filters_options_case_insensitively() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        assert_eq!(dd.visible_count(), 4);
        // "ame" matches only "America/New_York".
        feed_key(
            &mut dd,
            &key(Key::Character("a".into()), Some("a")),
            &mut doc,
            &root,
        );
        feed_key(
            &mut dd,
            &key(Key::Character("m".into()), Some("m")),
            &mut doc,
            &root,
        );
        feed_key(
            &mut dd,
            &key(Key::Character("e".into()), Some("e")),
            &mut doc,
            &root,
        );
        assert_eq!(dd.filter_query(), "ame");
        assert_eq!(dd.visible_count(), 1);
        // Enter selects the sole filtered match.
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::Enter), None),
            &mut doc,
            &root,
        );
        assert_eq!(dd.selected(), "America/New_York");
    }

    #[test]
    fn space_is_part_of_an_open_combobox_query() {
        let mut dd = Dropdown::new(vec!["New York".into(), "New Delhi".into()], "city");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        dd.set_focus(true);
        dd.do_open();
        for (key_value, text) in [
            (Key::Character("N".into()), Some("N")),
            (Key::Character("e".into()), Some("e")),
            (Key::Character("w".into()), Some("w")),
            (Key::Named(NamedKey::Space), None),
            (Key::Character("Y".into()), Some("Y")),
        ] {
            feed_key(&mut dd, &key(key_value, text), &mut doc, &root);
        }
        assert_eq!(dd.filter_query(), "New Y");
        assert_eq!(dd.visible_count(), 1);
    }

    #[test]
    fn backspace_edits_the_filter() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        feed_key(
            &mut dd,
            &key(Key::Character("a".into()), Some("a")),
            &mut doc,
            &root,
        );
        feed_key(
            &mut dd,
            &key(Key::Character("z".into()), Some("z")),
            &mut doc,
            &root,
        );
        assert_eq!(dd.visible_count(), 0); // "az" matches nothing
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::Backspace), None),
            &mut doc,
            &root,
        );
        assert_eq!(dd.filter_query(), "a");
        // "a" matches Amsterdam, America, Asia.
        assert_eq!(dd.visible_count(), 3);
    }

    #[test]
    fn backspace_deletes_one_extended_grapheme_from_the_filter() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        let cluster = "👩🏽‍💻";
        feed_key(
            &mut dd,
            &key(Key::Character(cluster.into()), Some(cluster)),
            &mut doc,
            &root,
        );
        assert_eq!(dd.filter_query(), cluster);
        feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::Backspace), None),
            &mut doc,
            &root,
        );
        assert_eq!(dd.filter_query(), "");
        assert_eq!(dd.filter_selection, Selection::collapsed(0));
    }

    #[test]
    fn typing_replaces_the_selected_filter_range() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        dd.filter = "Europe".into();
        dd.filter_selection = Selection {
            anchor: 0,
            cursor: dd.filter.len(),
        };
        feed_key(
            &mut dd,
            &key(Key::Character("亜".into()), Some("亜")),
            &mut doc,
            &root,
        );
        assert_eq!(dd.filter_query(), "亜");
        assert_eq!(dd.filter_selection, Selection::collapsed("亜".len()));
    }

    #[test]
    fn ime_preedit_is_visible_but_filters_only_after_commit() {
        let mut dd = Dropdown::new(vec!["東京".into(), "London".into()], "city");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        dd.apply_layout_external(Rect::new(10.0, 10.0, 260.0, 50.0));
        dd.set_focus(true);
        dd.do_open();

        assert!(feed_key(
            &mut dd,
            &WindowEvent::Ime(winit::event::Ime::Preedit(
                "東京".into(),
                Some(("東".len(), "東".len())),
            )),
            &mut doc,
            &root,
        ));
        assert_eq!(dd.filter_query(), "");
        assert_eq!(dd.visible_count(), 2);
        assert_eq!(
            Widget::accessibility(&dd).unwrap().value,
            None,
            "preedit must stay out of the accessibility value"
        );

        let mut scene = Scene::new();
        let mut renderer = TextRenderer::new();
        Widget::draw(
            &mut dd,
            &mut scene,
            Affine::IDENTITY,
            &mut renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        let candidate = Widget::ime_cursor_rect(&dd).expect("open combobox publishes its caret");
        assert!(dd.get_rect().contains(candidate.center()));

        assert!(feed_key(
            &mut dd,
            &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
            &mut doc,
            &root,
        ));
        assert_eq!(dd.filter_query(), "東京");
        assert_eq!(dd.visible_count(), 1);
        assert_eq!(
            Widget::accessibility(&dd).unwrap().value.as_deref(),
            Some("東京")
        );
    }

    #[test]
    fn losing_focus_closes_the_list() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        assert!(dd.is_open());
        dd.set_focus(false);
        assert!(!dd.is_open(), "Tab-away dismisses the dropdown");
    }

    #[test]
    fn open_dropdown_captures_pointer() {
        let mut dd = Dropdown::new(opts(), "tz");
        assert!(!dd.wants_pointer_capture());
        dd.do_open();
        assert!(dd.wants_pointer_capture());
    }

    #[test]
    fn commit_creates_text_object_when_absent() {
        // No `tz` key seeded: commit must create the Text object.
        let mut dd = Dropdown::new(opts(), "tz");
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        dd.commit_value("Europe/Amsterdam", &mut doc, &root);
        assert_eq!(
            Dropdown::read_value(&doc, &root, "tz").as_deref(),
            Some("Europe/Amsterdam")
        );
    }

    #[test]
    fn repeated_selection_splices_the_diff_not_the_whole_value() {
        // Committing by deleting the old value and reinserting the new one
        // leaves a tombstone per character per selection, so a settings pane
        // toggled back and forth bloats its document without bound.
        let long: Vec<String> = (0..4)
            .map(|i| format!("Europe/Amsterdam/very/long/option/label/number/{i}"))
            .collect();
        let mut dd = Dropdown::new(long.clone(), "tz");
        let (mut doc, root) = doc_with_tz(&long[0]);
        dd.sync(&mut doc, &root);
        let baseline = doc.save().len();

        // Alternate between two options that share a 46-char prefix.
        for i in 0..60 {
            dd.commit_value(&long[i % 2], &mut doc, &root);
        }
        let grown = doc.save().len() - baseline;

        assert_eq!(
            Dropdown::read_value(&doc, &root, "tz").as_deref(),
            Some(long[1].as_str()),
            "the last committed option must be what the document holds"
        );
        assert!(
            grown < 8 * long[0].len(),
            "60 selections grew the document by {grown} bytes for a {}-char label \
             — commit_value is rewriting the whole value",
            long[0].len()
        );
    }

    #[test]
    fn recommitting_the_current_value_is_free() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("Asia/Tokyo");
        dd.sync(&mut doc, &root);
        let before = doc.save().len();

        for _ in 0..25 {
            dd.commit_value("Asia/Tokyo", &mut doc, &root);
        }

        assert_eq!(
            Dropdown::read_value(&doc, &root, "tz").as_deref(),
            Some("Asia/Tokyo")
        );
        assert_eq!(
            doc.save().len(),
            before,
            "re-selecting the already-selected option must not grow the document"
        );
    }

    #[test]
    fn local_state_binding_is_two_way_across_dropdowns() {
        let state = crate::LocalState::new(String::from("UTC"));
        let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&changes);
        let mut first = Dropdown::from_state(opts(), &state)
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        let mut second = Dropdown::from_state(opts(), &state);
        let mut document = AutoCommit::new();

        state.set(String::from("Asia/Tokyo"));
        Widget::sync(&mut first, &mut document, &automerge::ROOT);
        Widget::sync(&mut second, &mut document, &automerge::ROOT);
        assert_eq!(first.selected(), "Asia/Tokyo");
        assert_eq!(second.selected(), "Asia/Tokyo");
        assert!(changes.borrow().is_empty(), "model sync is not user intent");

        assert!(Widget::perform_action(
            &mut first,
            &crate::a11y::Action::SetValue(String::from("Europe/Amsterdam")),
            &mut document,
            &automerge::ROOT,
        ));
        assert_eq!(state.get(), "Europe/Amsterdam");
        assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
        Widget::sync(&mut second, &mut document, &automerge::ROOT);
        assert_eq!(second.selected(), "Europe/Amsterdam");

        second.set_selected("America/New_York");
        assert_eq!(state.get(), "America/New_York");
        Widget::sync(&mut first, &mut document, &automerge::ROOT);
        assert_eq!(first.selected(), "America/New_York");
        assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
    }

    #[test]
    fn semantic_actions_expand_collapse_and_reject_unknown_values() {
        use crate::a11y::Action;

        let mut dropdown = Dropdown::new(opts(), "tz");
        let (mut document, root) = doc_with_tz("UTC");
        Widget::sync(&mut dropdown, &mut document, &root);

        assert!(Widget::perform_action(
            &mut dropdown,
            &Action::Expand,
            &mut document,
            &root,
        ));
        assert!(dropdown.is_open());
        assert!(Widget::perform_action(
            &mut dropdown,
            &Action::Collapse,
            &mut document,
            &root,
        ));
        assert!(!dropdown.is_open());
        assert!(!Widget::perform_action(
            &mut dropdown,
            &Action::SetValue(String::from("not an option")),
            &mut document,
            &root,
        ));
        assert_eq!(dropdown.selected(), "UTC");
        assert_eq!(
            Dropdown::read_value(&document, &root, "tz").as_deref(),
            Some("UTC")
        );
    }

    #[test]
    fn open_builder_aligns_highlight_to_selection_on_sync() {
        let mut dd = Dropdown::new(opts(), "tz").open();
        let (mut doc, root) = doc_with_tz("Asia/Tokyo");
        dd.sync(&mut doc, &root);
        assert!(dd.is_open());
        assert_eq!(
            dd.highlight_index(),
            3,
            "highlight tracks the selected option"
        );
    }

    #[test]
    fn with_enabled_false_is_reported_by_is_enabled() {
        let dd = Dropdown::new(opts(), "tz").with_enabled(false);
        assert!(!Widget::is_enabled(&dd));
        assert!(
            Widget::is_enabled(&Dropdown::new(opts(), "tz")),
            "default is enabled"
        );
    }

    #[test]
    fn disabled_dropdown_does_not_open() {
        let mut dd = Dropdown::new(opts(), "tz").with_enabled(false);
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        let handled = feed_key(
            &mut dd,
            &key(Key::Named(NamedKey::ArrowDown), None),
            &mut doc,
            &root,
        );
        assert!(!dd.is_open(), "disabled dropdown must not open");
        assert!(!handled, "disabled dropdown must not swallow input");
        assert_eq!(
            dd.selected(),
            "UTC",
            "disabled dropdown must not change the value"
        );
    }

    #[test]
    fn accessibility_reports_combobox_role_value_and_collapsed() {
        use crate::a11y::Role;
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("Asia/Tokyo");
        dd.sync(&mut doc, &root);
        let node = Widget::accessibility(&dd).expect("dropdown is not presentational");
        assert_eq!(node.role, Role::ComboBox);
        assert_eq!(node.name.as_deref(), Some("Asia/Tokyo"));
        assert_eq!(node.value.as_deref(), Some("Asia/Tokyo"));
        assert_eq!(
            node.state.expanded,
            Some(false),
            "closed dropdown is collapsed"
        );
        assert!(node.state.focusable);
        assert!(!node.state.disabled);
    }

    #[test]
    fn accessibility_unselected_uses_placeholder_name_and_no_value() {
        use crate::a11y::Role;
        let dd = Dropdown::new(opts(), "tz").with_placeholder("Pick a zone");
        let node = Widget::accessibility(&dd).unwrap();
        assert_eq!(node.role, Role::ComboBox);
        assert_eq!(
            node.name.as_deref(),
            Some("Pick a zone"),
            "placeholder is the name when unset"
        );
        assert_eq!(node.value, None, "no value until something is selected");
    }

    #[test]
    fn accessibility_reflects_open_and_disabled() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        let node = Widget::accessibility(&dd).unwrap();
        assert_eq!(node.state.expanded, Some(true), "open dropdown is expanded");
        assert!(node.state.focused);

        let dd2 = Dropdown::new(opts(), "tz").with_enabled(false);
        let node2 = Widget::accessibility(&dd2).unwrap();
        assert!(node2.state.disabled);
    }

    #[test]
    fn action_binding_owns_accessibility_selection() {
        let mut dropdown = Dropdown::new(opts(), "legacy")
            .on_change(|_| panic!("legacy callback must be suppressed"))
            .on_action(crate::widget::test_string_action_binding());
        let mut doc = AutoCommit::new();
        Widget::refresh_control_action_availability(
            &mut dropdown,
            &std::collections::HashSet::new(),
        );
        assert!(!Widget::is_enabled(&dropdown));
        Widget::refresh_control_action_availability(
            &mut dropdown,
            &std::collections::HashSet::from([qos_actions::ActionAddress::new(
                "test:control",
                "set",
            )]),
        );
        assert!(Widget::is_enabled(&dropdown));
        assert!(Widget::perform_action(
            &mut dropdown,
            &crate::a11y::Action::SetValue("Asia/Tokyo".into()),
            &mut doc,
            &automerge::ROOT
        ));
        assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
        assert_eq!(
            Widget::take_control_invocation(&mut dropdown)
                .unwrap()
                .arguments["value"],
            "Asia/Tokyo"
        );
    }
}
