//! [`Dropdown`] — a reusable, CRDT-backed single-select control.
//!
//! Closed it reads like a button: the current selection plus a chevron.
//! Open it reveals a scrollable list of options that composites *on top
//! of* following content via the App's top-layer [`Widget::draw_overlay`]
//! pass — it is a real overlay, not an inline expander that shoves
//! siblings down.
//!
//! ## Selection model (toolkit convention)
//!
//! Like [`crate::widget::TextInput`], the chosen value lives in the CRDT,
//! not on the widget. The constructor takes a `value_path`; on selection
//! the widget splices the chosen string into the Automerge `Text` object
//! at that path (creating it if absent), so a host that reads the path —
//! e.g. the installer reading `installer.timezone` — sees the change with
//! no extra wiring. `sync` pulls the current value back out so the closed
//! control always mirrors the document. A scalar string at the path is
//! also accepted on read for flexibility.
//!
//! ## Input
//!
//! * **Mouse** — click the control to open; click an item to select +
//!   close; click outside to dismiss; wheel scrolls the open list.
//! * **Keyboard** (while focused) — Enter / Space / Down open when
//!   closed; Up / Down move the highlight; Enter / Space select + close;
//!   Esc dismisses without changing; printable characters drive an
//!   incremental, case-insensitive substring **filter** (Backspace edits
//!   it). The filter makes hundreds-of-entries lists (time zones,
//!   keyboard layouts) usable without endless scrolling.
//!
//! While open the widget reports [`Widget::wants_pointer_capture`] so its
//! container routes *all* events to it exclusively — that is how the
//! out-of-layout list becomes hit-testable and how click-outside
//! dismissal works.

use std::collections::HashMap;

use automerge::{
    transaction::Transactable, AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value,
};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use vello::Scene;
use winit::event::{
    ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Height of the closed control (matches `TextInput` / `SegChoice`).
const CONTROL_HEIGHT: f64 = 40.0;
/// Per-item row height in the open list.
const ITEM_HEIGHT: f64 = 32.0;
/// Maximum height of the open list before it scrolls.
const MAX_LIST_HEIGHT: f64 = 240.0;
/// Vertical gap between the control and the open list.
const LIST_GAP: f64 = 4.0;
/// Horizontal text padding inside the control / rows.
const PAD_X: f64 = 10.0;
/// Reserved width on the right of the control for the chevron.
const CHEVRON_W: f64 = 30.0;
/// Label font size.
const FONT: f32 = 15.0;
/// Pixels scrolled per wheel notch in the open list.
const LINE_PX: f32 = 60.0;

pub struct Dropdown {
    id: String,
    options: Vec<String>,
    value_path: String,
    placeholder: String,

    /// Mirror of the CRDT value at `value_path`. Empty when unset.
    selected: String,
    open: bool,
    /// Position (0-based) into the *filtered* list of the highlighted row.
    highlight: usize,
    /// Incremental substring filter; empty shows every option.
    filter: String,
    /// Vertical scroll offset (px) of the open list.
    scroll: f32,

    /// One-shot: align `highlight` to the selected option on the next
    /// `sync`. Set by the `open()` builder (used for headless shots),
    /// where `selected` isn't known until `sync` reads the document.
    align_highlight_on_sync: bool,

    rect: Rect,
    is_focused: bool,
    is_hovered: bool,
    last_cursor: Point,
}

impl Dropdown {
    /// Construct a dropdown over `options`, writing the chosen value into
    /// the CRDT `Text` object at `value_path`.
    pub fn new(options: Vec<String>, value_path: &str) -> Self {
        Self {
            id: String::new(),
            options,
            value_path: value_path.to_string(),
            placeholder: String::new(),
            selected: String::new(),
            open: false,
            highlight: 0,
            filter: String::new(),
            scroll: 0.0,
            align_highlight_on_sync: false,
            rect: Rect::ZERO,
            is_focused: false,
            is_hovered: false,
            last_cursor: Point::ZERO,
        }
    }

    /// Stable identifier for the e2e harness (`Widget::test_id`).
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    /// Text shown (dimmed) when no value is selected yet.
    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_string();
        self
    }

    /// Start opened. Used for headless `--shots` of the open state; the
    /// highlight is aligned to the selected option on the first `sync`.
    pub fn open(mut self) -> Self {
        self.open = true;
        self.align_highlight_on_sync = true;
        self
    }

    /// Conditionally start opened — convenience for callers that decide
    /// at build time (e.g. a shot seed).
    pub fn open_if(self, cond: bool) -> Self {
        if cond {
            self.open()
        } else {
            self
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn selected(&self) -> &str {
        &self.selected
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
            .position(|&i| self.options[i] == self.selected)
            .unwrap_or(0)
    }

    /// Height of the open list box for `n` filtered rows (>=1 so the
    /// "no matches" row has a box to live in).
    fn list_box_height(n: usize) -> f64 {
        (n.max(1) as f64 * ITEM_HEIGHT).min(MAX_LIST_HEIGHT)
    }

    /// Absolute rect of the open list (drawn below the control).
    fn list_rect(&self, n: usize) -> Rect {
        let h = Self::list_box_height(n);
        let y0 = self.rect.y1 + LIST_GAP;
        Rect::new(self.rect.x0, y0, self.rect.x1, y0 + h)
    }

    fn max_scroll(n: usize) -> f32 {
        let content = n as f64 * ITEM_HEIGHT;
        (content - Self::list_box_height(n)).max(0.0) as f32
    }

    /// Scroll so the row at filtered-position `vi` is fully visible.
    fn ensure_visible(&mut self, vi: usize, n: usize) {
        let box_h = Self::list_box_height(n) as f32;
        let top = (vi as f64 * ITEM_HEIGHT) as f32;
        let bottom = top + ITEM_HEIGHT as f32;
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
        let filtered = self.filtered();
        self.highlight = self.selected_pos(&filtered);
        self.scroll = 0.0;
        self.ensure_visible(self.highlight, filtered.len());
    }

    fn close(&mut self) {
        self.open = false;
        self.filter.clear();
        self.scroll = 0.0;
    }

    /// Commit `value` into the CRDT `Text` at `value_path` (creating the
    /// object if needed) and mirror it locally.
    fn commit_value(&mut self, value: &str, state: &mut AutoCommit, root: &ObjId) {
        let obj = match state.get(root, self.value_path.as_str()) {
            Ok(Some((Value::Object(ObjType::Text), obj))) => Some(obj),
            _ => state.put_object(root, self.value_path.as_str(), ObjType::Text).ok(),
        };
        if let Some(obj) = obj {
            let cur = state.text(&obj).unwrap_or_default();
            let _ = state.splice_text(&obj, 0, cur.chars().count() as isize, value);
        } else {
            // Last resort: store as a scalar string (still readable).
            let _ = state.put(root, self.value_path.as_str(), value);
        }
        self.selected = value.to_string();
    }

    /// Select the highlighted option (if any) and close.
    fn select_highlighted(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let filtered = self.filtered();
        if let Some(&opt_idx) = filtered.get(self.highlight) {
            let value = self.options[opt_idx].clone();
            self.commit_value(&value, state, root);
        }
        self.close();
    }
}

impl Widget for Dropdown {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(v) = Self::read_value(state, root, self.value_path.as_str()) {
            self.selected = v;
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
        // The open list is intentionally NOT a taffy child: it paints in
        // the overlay pass so it doesn't reserve layout space (and the
        // widget-tree-mirrors-taffy invariant holds whether open or not).
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(CONTROL_HEIGHT as f32),
                },
                flex_shrink: 0.0,
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // --- Closed control (always painted, in the normal pass) ---
        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
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

        // Label: the selected value, or a dimmed placeholder.
        let (label, color) = if self.selected.is_empty() {
            (self.placeholder.as_str(), theme.text_dim)
        } else {
            (self.selected.as_str(), theme.text_primary)
        };
        if !label.is_empty() {
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &shape);
            let (_, th) = text_renderer.measure(label, FONT, None);
            let ty = self.rect.y0 + (self.rect.height() - th as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                FONT,
                color,
                transform * Affine::translate((self.rect.x0 + PAD_X, ty)),
                Some((self.rect.width() - PAD_X - CHEVRON_W) as f32),
            );
            scene.pop_layer();
        }

        // Chevron (down when closed, up when open).
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
        let shape = RoundedRect::from_rect(list, theme.tokens.radii.md as f64);

        // Surface + border.
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
            let ty = list.y0 + (ITEM_HEIGHT - FONT as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                "No matches",
                FONT,
                theme.text_dim,
                transform * Affine::translate((list.x0 + PAD_X, ty)),
                None,
            );
        } else {
            for (vi, &opt_idx) in filtered.iter().enumerate() {
                let row_top = list.y0 + (vi as f64 * ITEM_HEIGHT) - self.scroll as f64;
                let row_bottom = row_top + ITEM_HEIGHT;
                // Cull rows scrolled out of the box (hundreds of zones).
                if row_bottom < list.y0 || row_top > list.y1 {
                    continue;
                }
                let row = Rect::new(list.x0, row_top, list.x1, row_bottom);
                let is_selected = self.options[opt_idx] == self.selected;
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
                    vello::peniko::Color::WHITE
                } else {
                    theme.text_primary
                };
                let ty = row_top + (ITEM_HEIGHT - FONT as f64) / 2.0;
                text_renderer.draw_text(
                    scene,
                    &self.options[opt_idx],
                    FONT,
                    color,
                    transform * Affine::translate((list.x0 + PAD_X, ty)),
                    Some((list.width() - PAD_X * 2.0) as f32),
                );
            }
        }
        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    if list.contains(pos) {
                        let vi = ((pos.y - list.y0 + self.scroll as f64) / ITEM_HEIGHT)
                            .floor() as isize;
                        if vi >= 0 && (vi as usize) < filtered.len() {
                            let vi = vi as usize;
                            if vi != self.highlight {
                                self.highlight = vi;
                                *handled = true;
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pos = self.last_cursor;
                if self.open {
                    let filtered = self.filtered();
                    let list = self.list_rect(filtered.len());
                    if list.contains(pos) {
                        let vi = ((pos.y - list.y0 + self.scroll as f64) / ITEM_HEIGHT)
                            .floor() as isize;
                        if vi >= 0 && (vi as usize) < filtered.len() {
                            self.highlight = vi as usize;
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
            WindowEvent::MouseWheel { delta, .. } if self.open => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y * -LINE_PX,
                    MouseScrollDelta::PixelDelta(p) => -p.y as f32,
                };
                let n = self.filtered().len();
                self.scroll = (self.scroll + dy).clamp(0.0, Self::max_scroll(n));
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        text,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                if self.open {
                    match logical_key {
                        Key::Named(NamedKey::ArrowDown) => {
                            let n = self.filtered().len();
                            if n > 0 {
                                self.highlight = (self.highlight + 1).min(n - 1);
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
                        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                            self.select_highlighted(state, root);
                            *handled = true;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.close();
                            *handled = true;
                        }
                        Key::Named(NamedKey::Backspace) => {
                            self.filter.pop();
                            self.highlight = 0;
                            self.scroll = 0.0;
                            *handled = true;
                        }
                        _ => {
                            if let Some(t) = text {
                                if !t.as_str().chars().all(|c| c.is_control()) {
                                    self.filter.push_str(t.as_str());
                                    self.highlight = 0;
                                    self.scroll = 0.0;
                                    *handled = true;
                                }
                            }
                        }
                    }
                } else {
                    // Closed: Enter / Space / Down (or Up) open the list.
                    match logical_key {
                        Key::Named(NamedKey::Enter)
                        | Key::Named(NamedKey::Space)
                        | Key::Named(NamedKey::ArrowDown)
                        | Key::Named(NamedKey::ArrowUp) => {
                            self.do_open();
                            *handled = true;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
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

    /// While open the dropdown grabs every event so its out-of-layout
    /// list is hit-testable and click-outside dismissal works.
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
            .map(|s| s.to_string())
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

    /// Synthesise a `Pressed` `KeyboardInput` (mirrors the zero-init
    /// workaround used across the ui/shell test suites — winit keeps
    /// `KeyEvent`'s extra field `pub(crate)`).
    fn key(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
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
        let node = taffy.new_leaf(Style::default()).unwrap();
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
        assert!(feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowDown), None), &mut doc, &root));
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
        feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowDown), None), &mut doc, &root);
        feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowDown), None), &mut doc, &root);
        assert_eq!(dd.highlight_index(), 2);
        feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowUp), None), &mut doc, &root);
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
        feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowDown), None), &mut doc, &root);
        feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowDown), None), &mut doc, &root);
        assert!(feed_key(&mut dd, &key(Key::Named(NamedKey::Enter), None), &mut doc, &root));
        assert!(!dd.is_open(), "selecting closes the list");
        assert_eq!(dd.selected(), "America/New_York");
        // The value landed in the CRDT Text object the host reads.
        assert_eq!(Dropdown::read_value(&doc, &root, "tz").as_deref(), Some("America/New_York"));
    }

    #[test]
    fn escape_closes_without_changing_value() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        feed_key(&mut dd, &key(Key::Named(NamedKey::ArrowDown), None), &mut doc, &root);
        assert!(feed_key(&mut dd, &key(Key::Named(NamedKey::Escape), None), &mut doc, &root));
        assert!(!dd.is_open());
        assert_eq!(dd.selected(), "UTC", "Esc must not change the value");
        assert_eq!(Dropdown::read_value(&doc, &root, "tz").as_deref(), Some("UTC"));
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
        feed_key(&mut dd, &key(Key::Character("a".into()), Some("a")), &mut doc, &root);
        feed_key(&mut dd, &key(Key::Character("m".into()), Some("m")), &mut doc, &root);
        feed_key(&mut dd, &key(Key::Character("e".into()), Some("e")), &mut doc, &root);
        assert_eq!(dd.filter_query(), "ame");
        assert_eq!(dd.visible_count(), 1);
        // Enter selects the sole filtered match.
        feed_key(&mut dd, &key(Key::Named(NamedKey::Enter), None), &mut doc, &root);
        assert_eq!(dd.selected(), "America/New_York");
    }

    #[test]
    fn backspace_edits_the_filter() {
        let mut dd = Dropdown::new(opts(), "tz");
        let (mut doc, root) = doc_with_tz("UTC");
        dd.sync(&mut doc, &root);
        dd.set_focus(true);
        dd.do_open();
        feed_key(&mut dd, &key(Key::Character("a".into()), Some("a")), &mut doc, &root);
        feed_key(&mut dd, &key(Key::Character("z".into()), Some("z")), &mut doc, &root);
        assert_eq!(dd.visible_count(), 0); // "az" matches nothing
        feed_key(&mut dd, &key(Key::Named(NamedKey::Backspace), None), &mut doc, &root);
        assert_eq!(dd.filter_query(), "a");
        // "a" matches Amsterdam, America, Asia.
        assert_eq!(dd.visible_count(), 3);
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
        // No `tz` key seeded: commit must create the Text object so the
        // host's Text-reading path keeps working.
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
    fn open_builder_aligns_highlight_to_selection_on_sync() {
        let mut dd = Dropdown::new(opts(), "tz").open();
        let (mut doc, root) = doc_with_tz("Asia/Tokyo");
        dd.sync(&mut doc, &root);
        assert!(dd.is_open());
        assert_eq!(dd.highlight_index(), 3, "highlight tracks the selected option");
    }
}
