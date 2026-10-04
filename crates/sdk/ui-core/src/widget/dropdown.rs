//! [`Dropdown`] — a reusable, CRDT-backed single-select control.
//!
//! Closed it reads like a button: the current selection plus a chevron. Open
//! it reveals a scrollable list that composites *on top of* following content
//! via the App's top-layer [`Widget::draw_overlay`] pass — a real overlay,
//! not an inline expander that shoves siblings down.
//!
//! Like [`crate::widget::TextInput`], the chosen value can live in either an
//! explicit durable or process-local model. [`Dropdown::new`] binds a
//! document key; [`Dropdown::from_state`] binds a typed
//! [`crate::LocalState<String>`]. In both forms `sync` mirrors model changes
//! back without app-owned fan-out.
//!
//! Keyboard (while focused): Enter/Space/Down open; Up/Down move the
//! highlight; Enter selects + closes; Esc dismisses; printable characters
//! drive an incremental, case-insensitive substring filter (Backspace edits
//! it) so hundred-entry lists stay usable without endless scrolling.
//!
//! While open the widget reports [`Widget::wants_pointer_capture`] so its
//! container routes all events to it exclusively — how the out-of-layout
//! list becomes hit-testable and click-outside dismissal works.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use crate::widget::TaffyTreeExt;
use crate::widget::binding::{BoxedBinding, Pulled};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::{ActionEmitter, ControlBindingKind, ControlBindingMetadata};

/// Height of the closed control before the first layout: a Regular control.
const CONTROL_HEIGHT: f64 = crate::tokens::Metrics::BASE.min_pointer_target as f64;
/// One row of the open list: a menu row.
const ITEM_HEIGHT: f64 = crate::tokens::Metrics::BASE.menu_row_height as f64;
/// Tallest the rows of the open list get before it scrolls.
const MAX_LIST_HEIGHT: f64 = 240.0;
const LIST_GAP: f64 = 4.0;
/// The open list's inset round its rows: the menu inset every menu shares
/// (`floating_surface::menu_inset`), so the armed row runs concentric with
/// the panel.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const LIST_PAD: f64 = crate::tokens::Spacing::BASE.s2 as f64;
const PAD_X: f64 = 10.0;
const CHEVRON_W: f64 = 30.0;
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
    /// Stable machine values parallel to the human-readable option labels.
    /// The legacy constructor uses equal value/label pairs.
    values: Vec<String>,
    options: Vec<String>,
    /// Where the selected value lives: a document key or process-local state.
    binding: Option<BoxedBinding<String>>,
    on_change: Option<SelectionChangeCallback>,
    action: Option<ActionEmitter<String>>,
    placeholder: String,

    /// Mirror of the bound value. Empty when unset.
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

    /// The field: the closed control, below its caption when it has one.
    rect: Rect,
    /// The whole widget: the caption (if any) above the field.
    outer: Rect,
    /// A short label stacked above the field ("Level", "Source").
    caption: Option<String>,
    /// Height the caption takes above the field in the current theme.
    caption_height: f64,
    /// The field's height and label role.
    control_size: crate::widget::ControlSize,
    is_focused: bool,
    is_hovered: bool,
    last_cursor: Point,
    /// The host viewport from the latest paint, so the open list can flip
    /// above the control instead of running off the bottom of the window.
    /// `None` until a paint has reported one (bare harnesses never do).
    viewport: Option<Rect>,

    /// When `false`: skipped by focus, rejects input, renders dimmed.
    /// Default `true`.
    enabled: bool,
}

impl Dropdown {
    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    /// Construct a dropdown over `options`, keeping the chosen value as text
    /// under the document key `value_path`.
    pub fn new(options: Vec<String>, value_path: &str) -> Self {
        Self {
            id: String::new(),
            accessible_name: None,
            values: options.clone(),
            options,
            binding: Some(crate::doc_binding::text_key(value_path)),
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
            outer: Rect::ZERO,
            caption: None,
            caption_height: 0.0,
            control_size: crate::widget::ControlSize::Regular,
            is_focused: false,
            is_hovered: false,
            last_cursor: Point::ZERO,
            viewport: None,
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
        let (selected, binding) = super::binding::local(state);
        Self {
            selected,
            binding: Some(binding),
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
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
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

    /// The bounds the open list must stay inside — normally the window. A
    /// paint that sees the presence viewport keeps this in sync on its own;
    /// a host whose tree does not carry it down to the control (`Row` and
    /// `Column` clear it for their children, and the App pops it before the
    /// overlay pass) hands it over here so a control near the bottom edge
    /// still opens upward.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = Some(viewport);
    }

    /// Builder form of [`Self::set_viewport`].
    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.set_viewport(viewport);
        self
    }

    /// Replace the available options while preserving the current value when
    /// possible. Useful for embedded hosts whose choices are model-derived.
    pub fn set_options(&mut self, options: Vec<String>) {
        self.values.clone_from(&options);
        self.options = options;
        self.highlight = self.highlight.min(self.options.len().saturating_sub(1));
    }

    /// Set stable values with separate presentation labels. This keeps a
    /// duplicate label selectable and searchable without exposing ids in UI.
    pub fn set_value_options(&mut self, options: Vec<(String, String)>) {
        let (values, labels): (Vec<_>, Vec<_>) = options.into_iter().unzip();
        self.values = values;
        self.options = labels;
        self.highlight = self.highlight.min(self.options.len().saturating_sub(1));
    }

    /// Open the shared searchable, scrollable menu and align its highlight to
    /// the current selected value on the next sync.
    pub fn open_menu(&mut self) {
        self.open = true;
        self.align_highlight_on_sync = true;
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
        if let Some(binding) = &mut self.binding {
            binding.publish(&self.selected);
        }
    }

    /// Place the dropdown at an absolute rect without going through taffy —
    /// the counterpart to `TextInput::apply_layout_external`. The open
    /// list still paints in the overlay pass, so only the closed control's
    /// box is caller-driven; focus / a11y / CRDT-bound selection are unaffected.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.place(rect);
    }

    /// Stack a short caption above the field ("Level", "Source") in the
    /// `Caption` role and secondary ink. It also names the control to
    /// assistive technology unless an accessible name was given.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// The field's height and label role.
    #[must_use]
    pub fn control_size(mut self, size: crate::widget::ControlSize) -> Self {
        self.control_size = size;
        self
    }

    /// Lay the widget out in `outer`: the caption on top, the field below.
    fn place(&mut self, outer: Rect) {
        self.outer = outer;
        let field_y0 = (outer.y0 + self.caption_height).min(outer.y1);
        self.rect = Rect::new(outer.x0, field_y0, outer.x1, outer.y1);
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

    /// Height occupied by the closed control and its open options overlay.
    ///
    /// External-layout hosts can use this to make a surrounding popover hug
    /// the visible rows without duplicating the dropdown's sizing constants.
    pub fn open_overlay_height(&self) -> f64 {
        // The laid-out control height (a `ui_scale`d `metrics.bar_height`)
        // when there is one; the reference height before the first layout.
        let control = if self.rect.height() > 0.0 {
            self.rect.height()
        } else {
            CONTROL_HEIGHT
        };
        control + LIST_GAP + Self::list_box_height(self.visible_count())
    }

    /// Position of the selected option within `filtered`, or 0.
    fn selected_pos(&self, filtered: &[usize]) -> usize {
        filtered
            .iter()
            .position(|&i| {
                self.values
                    .get(i)
                    .is_some_and(|value| *value == self.selected)
            })
            .unwrap_or(0)
    }

    /// Presentation label for the selected stable value.
    fn selected_label(&self) -> &str {
        self.values
            .iter()
            .position(|value| *value == self.selected)
            .and_then(|index| self.options.get(index))
            .map_or(self.selected.as_str(), String::as_str)
    }

    /// Height of the open list box for `n` filtered rows (>=1 so the
    /// "no matches" row has a box to live in).
    fn list_box_height(n: usize) -> f64 {
        Self::rows_height(n) + 2.0 * LIST_PAD
    }

    /// Height of the visible rows of the open list, inside its inset.
    fn rows_height(n: usize) -> f64 {
        (count_f64(n.max(1)) * ITEM_HEIGHT).min(MAX_LIST_HEIGHT)
    }

    /// The rows' box inside the open `list`.
    fn rows_rect(list: Rect) -> Rect {
        Rect::new(
            list.x0 + LIST_PAD,
            list.y0 + LIST_PAD,
            list.x1 - LIST_PAD,
            list.y1 - LIST_PAD,
        )
    }

    /// Absolute rect of the open list: below the control, or above it when
    /// opening below would leave the host viewport and there is more room
    /// above, placed by `floating_surface::place_anchored` like every
    /// anchored Panel.
    /// Hit-testing and paint share this, so a flipped list is clickable
    /// where it is drawn.
    fn list_rect(&self, n: usize) -> Rect {
        let h = Self::list_box_height(n);
        let viewport = self
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        crate::widget::floating_surface::place_anchored(
            self.rect,
            (self.rect.width(), h),
            crate::widget::PopoverSide::Below,
            LIST_GAP,
            viewport,
        )
        .0
    }

    fn max_scroll(n: usize) -> f32 {
        let content = count_f64(n) * ITEM_HEIGHT;
        px((content - Self::rows_height(n)).max(0.0))
    }

    /// Filtered-list row index under `pos`, or `None` outside `list`/`n` rows.
    fn row_at(&self, pos: Point, list: Rect, n: usize) -> Option<usize> {
        let rows = Self::rows_rect(list);
        if !rows.contains(pos) {
            return None;
        }
        let rel = pos.y - rows.y0 + f64::from(self.scroll);
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
        let box_h = px(Self::rows_height(n));
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

    /// Close the options overlay without changing its selected value.
    pub fn close(&mut self) {
        self.open = false;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        self.scroll = 0.0;
    }

    /// Store `value` through the binding and mirror it locally.
    fn commit_value(&mut self, value: &str, state: &mut AppModel, root: &ModelPath) {
        if let Some(action) = &mut self.action {
            let _ = action.emit(value.to_string());
        } else if let Some(binding) = &mut self.binding {
            binding.push(state, root, &value.to_string());
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
    fn select_highlighted(&mut self, state: &mut AppModel, root: &ModelPath) {
        let filtered = self.filtered();
        if let Some(&opt_idx) = filtered.get(self.highlight)
            && let Some(value) = self.values.get(opt_idx).cloned()
        {
            self.commit_value(&value, state, root);
        }
        self.close();
    }

    /// `WindowEvent::CursorMoved` — hover tracking, plus (while open)
    /// following the highlight to the row under the pointer.
    fn on_cursor_moved(&mut self, pos: Point, handled: &mut bool) {
        self.last_cursor = pos;
        // A hover change is a repaint, not consumed input (see `Button`): a
        // parent that stops at the first handled child would otherwise never
        // deliver this move to the sibling the pointer went to, and the
        // sibling's hover — or this one's — would stick.
        self.is_hovered = self.rect.contains(pos);
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
        state: &mut AppModel,
        root: &ModelPath,
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
        state: &mut AppModel,
        root: &ModelPath,
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
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding
            && let Pulled::Value(selected) = binding.pull(state, root)
        {
            self.selected = selected;
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
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The open list is intentionally NOT a taffy child: it paints in the
        // overlay pass, so it doesn't reserve layout space.
        //
        // A fluid control: it asks for the row's width and is the item that
        // gives when the row is short of room — like `TextInput`, and unlike
        // fixed-size leaves such as `Toggle`, which refuse to shrink. With
        // `flex_shrink: 0` a dropdown claimed the whole row and pushed every
        // later sibling past the edge to zero width. The floor keeps the
        // chevron slot and padding so a squeezed control is still a control.
        self.caption_height = if self.caption.is_some() {
            f64::from(theme.text(TextRole::Caption).line_height_px())
                + f64::from(theme.tokens.spacing.s1)
        } else {
            0.0
        };
        let field = self.control_size.height(theme);
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(px(field + self.caption_height)),
            },
            min_size: Size {
                width: Dimension::length(px(PAD_X + CHEVRON_W)),
                height: Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.place(Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        ));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
        }
        if let Some(caption) = &self.caption {
            let style = theme.text(TextRole::Caption);
            let shown = text_renderer.elide_to_width_style(
                caption,
                px(self.outer.width().max(1.0)),
                &style,
            );
            text_renderer.draw_style(
                scene,
                &shown,
                theme.tokens.colors.text_dim,
                translated(transform, self.outer.x0, self.outer.y0),
                None,
                &style,
            );
        }
        let shape = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.control));
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
        // The field is a Secondary control: the raised face, its lift under
        // the pointer and while open (HIG §9.2), the derived rest edge, and
        // your colour as the focus ring.
        let raised = theme.tokens.colors.surfaces.raised;
        let bg = if self.open || self.is_focused {
            theme.tokens.colors.lift_on(raised, 1.0)
        } else if self.is_hovered && !disabled {
            theme.tokens.colors.lift_on(raised, 0.5)
        } else {
            raised
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);
        let (bw, bc) = if self.open || self.is_focused {
            (2.0, theme.tokens.colors.intent.you)
        } else {
            (
                1.0,
                crate::surface::edge_on(theme.tokens.colors.surfaces.raised),
            )
        };
        // Rest edge and focus ring share one half-pixel-inset outline.
        scene.stroke(
            &Stroke::new(bw),
            transform,
            bc,
            None,
            &crate::surface::edge_outline(self.rect, f64::from(theme.tokens.radii.control)),
        );

        if self.open {
            self.filter_composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &self.filter,
                    selection: &self.filter_selection,
                    placeholder: "Type to filter…",
                    show_caret: self.is_focused,
                },
                Rect::new(
                    self.rect.x0 + PAD_X,
                    self.rect.y0,
                    self.rect.x1 - CHEVRON_W,
                    self.rect.y1,
                ),
                &self.control_size.label_style(theme),
                QueryInk::of(&theme.tokens.colors),
            );
        } else {
            let (label, color) = if self.selected.is_empty() {
                (self.placeholder.as_str(), theme.text_dim)
            } else {
                (self.selected_label(), theme.text_primary)
            };
            if !label.is_empty() {
                scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
                // One line, elided to the room left of the chevron: a max
                // width handed to `draw_text` wraps instead, and a wrapped
                // second line lands under the control's bottom edge.
                let style = self.control_size.label_style(theme);
                let room = px((self.rect.width() - PAD_X - CHEVRON_W).max(0.0));
                let shown = text_renderer.elide_to_width_style(label, room, &style);
                let (_, th) = text_renderer.measure_style(&shown, None, &style);
                let ty = self.rect.y0 + (self.rect.height() - f64::from(th)) / 2.0;
                text_renderer.draw_style(
                    scene,
                    &shown,
                    color,
                    translated(transform, self.rect.x0 + PAD_X, ty),
                    None,
                    &style,
                );
                scene.pop_layer();
            }
        }

        // The trailing chevron, centred in its slot, from the one icon set.
        let edge = f64::from(theme.tokens.metrics.icon_sm);
        let slot_center = Point::new(self.rect.x1 - CHEVRON_W / 2.0, self.rect.center().y);
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(slot_center, (edge, edge)),
            if self.open {
                crate::icons::Icon::ChevronUp
            } else {
                crate::icons::Icon::ChevronDown
            },
            theme.text_dim,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );

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
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
        }
        let filtered = self.filtered();
        let list = self.list_rect(filtered.len());
        // The open list is a floating Panel (HIG §9.1 / §9.5): the panel
        // radius, the floating shadow, and the floating material with its
        // derived edge, so it separates from the field it drops from.
        let radius = f64::from(theme.tokens.radii.panel);
        crate::surface::shadow(
            scene,
            transform,
            list,
            radius,
            &theme.tokens.elevation.floating,
        );
        crate::surface::panel(scene, transform, list, radius, theme);

        // Rows sit inside the menu inset and scroll inside it.
        let rows = Self::rows_rect(list);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &rows);

        if filtered.is_empty() {
            // Centre on the measured line, not the font size (the face's
            // ascent/descent otherwise sits the label low in the row).
            let (_, hint_h) =
                text_renderer.measure_style("No matches", None, &theme.text(TextRole::Body));
            let ty = rows.y0 + (ITEM_HEIGHT - f64::from(hint_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                "No matches",
                theme.text_dim,
                translated(transform, rows.x0 + LIST_PAD, ty),
                None,
                &theme.text(TextRole::Body),
            );
        } else {
            for (vi, &opt_idx) in filtered.iter().enumerate() {
                let row_top = rows.y0 + (count_f64(vi) * ITEM_HEIGHT) - f64::from(self.scroll);
                let row_bottom = row_top + ITEM_HEIGHT;
                // Cull rows scrolled out of the box (hundreds of zones).
                if row_bottom < rows.y0 || row_top > rows.y1 {
                    continue;
                }
                let Some(opt) = self.options.get(opt_idx) else {
                    continue;
                };
                let row = Rect::new(rows.x0, row_top, rows.x1, row_bottom);
                let is_selected = self
                    .values
                    .get(opt_idx)
                    .is_some_and(|value| *value == self.selected);
                // The armed row is the panel's strong lift, as in every
                // menu (HIG §9.3);
                // the applied value carries a trailing check.
                let color = if vi == self.highlight {
                    crate::widget::floating_surface::paint_armed_row(
                        scene, transform, row, LIST_PAD, theme,
                    )
                } else {
                    theme.text_primary
                };
                let check = f64::from(theme.tokens.metrics.icon_sm);
                if is_selected {
                    crate::icons::draw_icon(
                        scene,
                        transform,
                        Rect::from_center_size(
                            Point::new(row.x1 - LIST_PAD - check / 2.0, row.center().y),
                            (check, check),
                        ),
                        crate::icons::Icon::Check,
                        color,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                // Rows are one `ITEM_HEIGHT` tall, so a long option elides
                // rather than wrapping into the row beneath it.
                let shown = text_renderer.elide_to_width_style(
                    opt,
                    px((row.width() - LIST_PAD * 3.0 - check).max(0.0)),
                    &theme.text(TextRole::Body),
                );
                let (_, opt_h) =
                    text_renderer.measure_style(&shown, None, &theme.text(TextRole::Body));
                let ty = row_top + (ITEM_HEIGHT - f64::from(opt_h)) / 2.0;
                text_renderer.draw_style(
                    scene,
                    &shown,
                    color,
                    translated(transform, row.x0 + LIST_PAD, ty),
                    None,
                    &theme.text(TextRole::Body),
                );
            }
        }
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
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
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.on_cursor_moved(pos, ctx.handled);
            }
            // The pointer left the window: no `CursorMoved` follows, so the
            // resting hover veneer would otherwise stay painted.
            WindowEvent::CursorLeft { .. } => {
                self.is_hovered = false;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.on_left_press(
                    ctx.state,
                    ctx.path,
                    ctx.handled,
                    ctx.focus_target,
                    ctx.node_id,
                );
            }
            WindowEvent::MouseWheel { delta, .. } if self.open => {
                self.on_wheel(delta, ctx.handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    ke @ KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                self.on_key(event, ke, *ctx.modifiers, ctx.state, ctx.path, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Dropdown"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("dropdown")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.outer
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
        } else if let Some(caption) = &self.caption {
            Some(caption.clone())
        } else if !self.selected.is_empty() {
            Some(self.selected_label().to_string())
        } else if !self.placeholder.is_empty() {
            Some(self.placeholder.clone())
        } else {
            None
        };
        let value = if self.open && !self.filter.is_empty() {
            Some(self.filter.clone())
        } else {
            (!self.selected.is_empty()).then(|| self.selected_label().to_string())
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
        if let Some(idx) = self.values.iter().position(|value| *value == self.selected) {
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
        state: &mut AppModel,
        root: &ModelPath,
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
                let Some(index) = self.options.iter().position(|option| option == value) else {
                    return false;
                };
                let Some(value) = self.values.get(index).cloned() else {
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

impl crate::widget::controls::ActionControls for Dropdown {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action.as_mut().and_then(ActionEmitter::take)
    }
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("dropdown");
        if let Some(action) = &self.action {
            vec![action.metadata(control_id)]
        } else if let Some(key) = self.binding.as_ref().and_then(|b| b.document_key()) {
            vec![ControlBindingMetadata {
                control_id: control_id.to_string(),
                kind: ControlBindingKind::CollaborativeDocument(key.to_owned()),
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
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, transaction::Transactable};
    use winit::event::Modifiers;
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    fn opts() -> Vec<String> {
        ["UTC", "Europe/Amsterdam", "America/New_York", "Asia/Tokyo"]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The open list's rows sit inside the menu inset every menu shares,
    /// so its armed row runs concentric with the panel: the inset is not a
    /// row, and the first row starts after it.
    #[test]
    fn the_open_list_insets_its_rows_like_every_menu() {
        let mut dd = Dropdown::new(opts(), "tz");
        dd.rect = Rect::new(0.0, 0.0, 240.0, 32.0);
        let n = dd.options.len();
        let list = dd.list_rect(n);
        let theme = Theme::dark();
        assert!(
            (LIST_PAD - crate::widget::floating_surface::menu_inset(&theme)).abs() < 1e-9,
            "the dropdown's inset is the shared menu inset"
        );
        let x = list.center().x;
        assert_eq!(dd.row_at(Point::new(x, list.y0 + 2.0), list, n), None);
        assert_eq!(dd.row_at(Point::new(4.0, list.y0 + 20.0), list, n), None);
        assert_eq!(
            dd.row_at(Point::new(x, list.y0 + LIST_PAD + 1.0), list, n),
            Some(0)
        );
        assert!(
            (list.height() - (count_f64(n) * ITEM_HEIGHT + 2.0 * LIST_PAD)).abs() < 1e-9,
            "the list hugs its rows plus the inset: {list:?}"
        );
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
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: doc,
                path: root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn dropdown_keyboard_opens_navigates_selects_and_escapes() {
        fn starts_closed_and_reads_value_on_sync() {
            let mut dd = Dropdown::new(opts(), "tz");
            assert!(!dd.is_open());
            let (mut doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&mut doc, &root);
            assert_eq!(dd.selected(), "Asia/Tokyo");
        }

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
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("America/New_York")
            );
        }

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
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("UTC")
            );
        }

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

        starts_closed_and_reads_value_on_sync();
        down_opens_when_closed_and_focused();
        arrows_move_highlight_while_open();
        enter_selects_writes_value_and_closes();
        escape_closes_without_changing_value();
        open_builder_aligns_highlight_to_selection_on_sync();
    }

    #[test]
    fn combobox_filter_editing_handles_case_spaces_graphemes_and_ime() {
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
            let candidate =
                Widget::ime_cursor_rect(&dd).expect("open combobox publishes its caret");
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

        typing_filters_options_case_insensitively();
        space_is_part_of_an_open_combobox_query();
        backspace_edits_the_filter();
        backspace_deletes_one_extended_grapheme_from_the_filter();
        typing_replaces_the_selected_filter_range();
        ime_preedit_is_visible_but_filters_only_after_commit();
    }

    #[test]
    fn open_list_pointer_capture_focus_and_placement() {
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

        fn open_dropdown_captures_pointer() {
            let mut dd = Dropdown::new(opts(), "tz");
            assert!(!dd.wants_pointer_capture());
            dd.do_open();
            assert!(dd.wants_pointer_capture());
        }

        /// Hover is a repaint, not consumed input (the rule `Button` documents):
        /// the move that changes it is left for siblings, and leaving the window
        /// clears it because no `CursorMoved` will follow.
        fn hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover() {
            let mut dd = Dropdown::new(opts(), "tz");
            dd.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
            let (mut doc, root) = doc_with_tz("UTC");
            let enter = WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(20.0, 20.0),
            };
            assert!(
                !feed_key(&mut dd, &enter, &mut doc, &root),
                "a hover change must not mark the move handled"
            );
            assert!(dd.is_hovered, "the pointer is over the control");
            let leave = WindowEvent::CursorLeft {
                device_id: unsafe { std::mem::zeroed() },
            };
            feed_key(&mut dd, &leave, &mut doc, &root);
            assert!(!dd.is_hovered, "CursorLeft clears the resting hover");
        }

        /// The open list flips above the control when opening below would leave
        /// the host viewport and there is room above; hit-testing shares the
        /// rect, so the flipped list is clickable where it is painted. The
        /// viewport arrives either through the presence context at paint or
        /// from the host via `set_viewport`; both routes are exercised.
        fn open_list_flips_above_when_it_would_leave_the_viewport() {
            let mut dd = Dropdown::new(opts(), "tz");
            dd.rect = Rect::new(0.0, 700.0, 200.0, 740.0);
            let n = opts().len();
            let below = dd.list_rect(n);
            assert!(
                below.y0 >= dd.rect.y1,
                "without a viewport the list opens below"
            );
            // Route 1: the paint pass sees the App's pushed window viewport.
            let presence = PresenceCtx::default();
            presence
                .viewport
                .set(Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
            let mut scene = Scene::new();
            dd.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &presence,
            );
            assert_eq!(dd.viewport, Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
            // Route 2: a host inside a Row/Column hands it over explicitly.
            let mut hosted =
                Dropdown::new(opts(), "tz").with_viewport(Rect::new(0.0, 0.0, 800.0, 760.0));
            hosted.rect = dd.rect;
            assert_eq!(hosted.list_rect(n), dd.list_rect(n));
            let flipped = dd.list_rect(n);
            assert!(
                flipped.y1 <= dd.rect.y0,
                "the list opens above the control: {flipped:?} vs {:?}",
                dd.rect
            );
            assert!(flipped.y0 >= 0.0, "and stays inside the viewport");
            assert!((flipped.height() - below.height()).abs() < 1e-6);
            assert_eq!(
                dd.row_at(Point::new(10.0, flipped.y0 + ITEM_HEIGHT / 2.0), flipped, n),
                Some(0),
                "the first row is hit where it is painted"
            );
            // No room above either: stay below rather than run off the top.
            dd.set_viewport(Rect::new(0.0, 690.0, 800.0, 760.0));
            assert!((dd.list_rect(n).y0 - below.y0).abs() < 1e-6);
        }

        /// The dropdown is the fluid item in a row (F2-16): laid beside fixed
        /// controls in a row too narrow for its `percent(100%)`, it gives the
        /// room back, so its siblings keep their size and the trailing one is
        /// still on screen instead of being pushed to zero width past the edge.
        fn dropdown_in_a_row_yields_width_to_fixed_siblings() {
            use crate::widget::{Button, Row};
            let mut row = Row::new()
                .no_wrap()
                .push(Box::new(Button::new("left", "Left", None)))
                .push(Box::new(Dropdown::new(opts(), "tz")))
                .push(Box::new(Button::new("right", "Right", None)));
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = row.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(600.0),
                        height: taffy::AvailableSpace::Definite(80.0),
                    },
                )
                .unwrap();
            row.apply_layout(&taffy, node, Point::ZERO);
            let rects: Vec<Rect> = row.children().iter().map(|c| c.get_rect()).collect();
            assert_eq!(rects.len(), 3);
            let (left, dd, right) = (rects[0], rects[1], rects[2]);
            assert!(
                (left.width() - right.width()).abs() < 0.5 && left.width() > 0.0,
                "the fixed buttons keep their size: {left:?} / {right:?}"
            );
            assert!(
                dd.width() >= PAD_X + CHEVRON_W - 0.5,
                "the dropdown keeps at least its chevron slot: {dd:?}"
            );
            assert!(
                right.x1 <= 600.0 + 0.5,
                "the trailing button stays inside the 600 px row: {right:?}"
            );
            assert!(
                left.x1 <= dd.x0 + 0.5 && dd.x1 <= right.x0 + 0.5,
                "row order without overlap: {left:?} {dd:?} {right:?}"
            );
            assert!(
                (dd.height() - crate::widget::ControlSize::Regular.height(&theme)).abs() < 0.5,
                "the control is a Regular control: {dd:?}"
            );
        }

        fn a_caption_stacks_above_the_field_and_names_it() {
            let theme = Theme::dark();
            let mut dd = Dropdown::new(opts(), "level").caption("Level");
            let mut taffy = TaffyTree::new();
            let mut node_map = HashMap::new();
            let node = dd.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(300.0),
                        height: taffy::AvailableSpace::Definite(200.0),
                    },
                )
                .unwrap();
            dd.apply_layout(&taffy, node, Point::ZERO);
            let field = crate::widget::ControlSize::Regular.height(&theme);
            assert!((dd.rect.height() - field).abs() < 0.5, "{:?}", dd.rect);
            assert!(dd.rect.y0 > dd.outer.y0, "the caption sits above the field");
            assert_eq!(Widget::get_rect(&dd), dd.outer);
            let node = Widget::accessibility(&dd).expect("dropdown semantics");
            assert_eq!(node.name.as_deref(), Some("Level"));
        }

        fn external_overlay_height_hugs_short_lists_and_caps_long_ones() {
            let short = Dropdown::new(opts(), "tz");
            assert_eq!(
                short.open_overlay_height(),
                CONTROL_HEIGHT + LIST_GAP + 4.0 * ITEM_HEIGHT + 2.0 * LIST_PAD
            );

            let long = Dropdown::new((0..100).map(|index| index.to_string()).collect(), "value");
            assert_eq!(
                long.open_overlay_height(),
                CONTROL_HEIGHT + LIST_GAP + MAX_LIST_HEIGHT + 2.0 * LIST_PAD
            );
        }

        losing_focus_closes_the_list();
        open_dropdown_captures_pointer();
        hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover();
        open_list_flips_above_when_it_would_leave_the_viewport();
        dropdown_in_a_row_yields_width_to_fixed_siblings();
        a_caption_stacks_above_the_field_and_names_it();
        external_overlay_height_hugs_short_lists_and_caps_long_ones();
    }

    #[test]
    fn commit_writes_minimal_crdt_diffs_and_binds_state() {
        fn commit_creates_text_object_when_absent() {
            // No `tz` key seeded: commit must create the Text object.
            let mut dd = Dropdown::new(opts(), "tz");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            dd.commit_value("Europe/Amsterdam", &mut doc, &root);
            assert_eq!(
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("Europe/Amsterdam")
            );
        }

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
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
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

        fn recommitting_the_current_value_is_free() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&mut doc, &root);
            let before = doc.save().len();

            for _ in 0..25 {
                dd.commit_value("Asia/Tokyo", &mut doc, &root);
            }

            assert_eq!(
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("Asia/Tokyo")
            );
            assert_eq!(
                doc.save().len(),
                before,
                "re-selecting the already-selected option must not grow the document"
            );
        }

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

        commit_creates_text_object_when_absent();
        repeated_selection_splices_the_diff_not_the_whole_value();
        recommitting_the_current_value_is_free();
        local_state_binding_is_two_way_across_dropdowns();
    }

    #[test]
    fn dropdown_accessibility_semantics_and_disabled_state() {
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
                crate::doc::read_str_or_text(&document, &root, "tz").as_deref(),
                Some("UTC")
            );
        }

        fn with_enabled_false_is_reported_by_is_enabled() {
            let dd = Dropdown::new(opts(), "tz").with_enabled(false);
            assert!(!Widget::is_enabled(&dd));
            assert!(
                Widget::is_enabled(&Dropdown::new(opts(), "tz")),
                "default is enabled"
            );
        }

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

        fn action_binding_owns_accessibility_selection() {
            let mut dropdown = Dropdown::new(opts(), "legacy")
                .on_change(|_| panic!("legacy callback must be suppressed"))
                .on_action(crate::widget::test_string_action_binding());
            let mut doc = AutoCommit::new();
            crate::widget::controls::refresh_control_action_availability(
                &mut dropdown,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&dropdown));
            crate::widget::controls::refresh_control_action_availability(
                &mut dropdown,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
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
                crate::widget::controls::take_control_invocation(&mut dropdown)
                    .unwrap()
                    .arguments["value"],
                "Asia/Tokyo"
            );
        }

        semantic_actions_expand_collapse_and_reject_unknown_values();
        with_enabled_false_is_reported_by_is_enabled();
        disabled_dropdown_does_not_open();
        accessibility_reports_combobox_role_value_and_collapsed();
        accessibility_unselected_uses_placeholder_name_and_no_value();
        accessibility_reflects_open_and_disabled();
        action_binding_owns_accessibility_selection();
    }
}
