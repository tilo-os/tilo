//! An inline-editable label: reads as static text until the caller puts it
//! into edit mode, then becomes a real [`TextInput`] — so the edit gets every
//! affordance the toolkit's text field already has (Ctrl+A / C / X / V,
//! drag-select, double-click word, Shift+Arrow selection, a right-click menu)
//! for free, instead of a hand-rolled buffer.
//!
//! ## Commit / cancel policy
//!
//! An edit ends in one of three ways, matching how every inline rename in a
//! desktop UI behaves:
//!
//! * **Enter** — commit the typed value.
//! * **Focus loss / a click outside the field** — commit (blur-commit). This is
//!   the affordance users expect and the reason this lives in the toolkit: a
//!   rename shouldn't be lost just because you clicked away instead of pressing
//!   Enter.
//! * **Esc** — cancel, keeping the previous value.
//!
//! A committed value that is blank after trimming keeps the previous value
//! (an empty rename is treated as "no change"), so `value()` is never emptied
//! by an accidental clear.
//!
//! The outcome of the most recent edit is polled once via [`take_outcome`]
//! (rising-edge, like [`TextInput::take_dirty`]): the host reads it after each
//! event to learn whether — and how — the edit ended, then persists the value.
//!
//! ## Where it renders
//!
//! Both the idle label and the in-edit field shape their text in the widget's
//! [`FontChoice`] family, so an inline rename of (say) a font entry previews
//! the name in that font's own face while you type it.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::{FontChoice, TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

/// Line-height ratio the label uses to vertically centre its idle text — the
/// same ratio [`TextInput`] centres its own glyph band against, so the text
/// doesn't jump vertically when the field enters / leaves edit mode.
const LINE_HEIGHT_RATIO: f32 = 1.25;

/// How the most recent inline edit ended. Returned once by [`EditableLabel::take_outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// No edit has ended since the last poll.
    None,
    /// The edit committed with this (final) value. Also returned when a blank
    /// entry kept the previous value — the payload is always the live value.
    Committed(String),
    /// The edit was cancelled (Esc); the value is unchanged.
    Cancelled,
}

pub struct EditableLabel {
    _id: String,
    /// The committed label text — what `draw` shows when not editing and what
    /// [`value`](Self::value) returns.
    value: String,
    /// The embedded field, live only while editing. Owns the caret / selection
    /// and every editing affordance.
    input: TextInput,
    editing: bool,
    /// Family both the idle label and the in-edit field render in.
    family: FontChoice,
    size: f32,
    /// Optional idle-text colour override; `None` uses `theme.text_primary`.
    idle_color: Option<vello::peniko::Color>,
    rect: Rect,
    /// Latest pointer position (logical), tracked so a press can be classified
    /// as inside (caret) vs. outside (blur-commit) without the press event
    /// carrying a position.
    last_pointer: Point,
    /// Rising-edge outcome of the most recent edit, cleared by `take_outcome`.
    outcome: EditOutcome,
}

impl EditableLabel {
    /// Construct an editable label showing `value`. Starts in the idle
    /// (non-editing) state.
    pub fn new(value: &str) -> Self {
        Self {
            _id: String::new(),
            value: value.to_string(),
            input: TextInput::local(),
            editing: false,
            family: FontChoice::SansSerif,
            size: 16.0,
            idle_color: None,
            rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            outcome: EditOutcome::None,
        }
    }

    /// Builder-style: render (idle + editing) in an explicit font family.
    pub fn with_font_family(mut self, family: FontChoice) -> Self {
        self.input.set_font_family(Some(family.clone()));
        self.family = family;
        self
    }

    /// Builder-style: set the text size in logical pixels for both states.
    pub fn with_font_size(mut self, size: f32) -> Self {
        self.size = size;
        self.input.set_font_size(size);
        self
    }

    /// Builder-style: override the idle text colour (pass a theme token so it
    /// still tracks the active theme).
    pub fn with_color(mut self, color: vello::peniko::Color) -> Self {
        self.idle_color = Some(color);
        self
    }

    /// Builder-style stable id (used by [`Widget::test_id`]).
    pub fn with_id(mut self, id: &str) -> Self {
        self._id = id.to_string();
        self
    }

    /// The committed label value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The text currently shown: the live field contents while editing, else
    /// the committed value.
    pub fn current_text(&self) -> &str {
        if self.editing {
            self.input.value()
        } else {
            &self.value
        }
    }

    /// Programmatically set the committed value (e.g. the model changed under
    /// the widget). Ignored while editing so it doesn't stomp the user's typing.
    pub fn set_value(&mut self, value: &str) {
        if !self.editing {
            self.value = value.to_string();
        }
    }

    /// Whether the field is currently in edit mode.
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Enter edit mode: seed the field with the current value, focus it, and
    /// select the whole value so the first keystroke replaces it (native
    /// rename behaviour). Clears any stale outcome.
    pub fn begin_edit(&mut self) {
        self.input.set_text(&self.value);
        self.input.set_focus(true);
        self.input.select_all();
        self.editing = true;
        self.outcome = EditOutcome::None;
    }

    /// Commit the in-flight edit: fold the trimmed field value into the label
    /// (a blank entry keeps the previous value) and leave edit mode. Records a
    /// [`EditOutcome::Committed`] for the next [`take_outcome`](Self::take_outcome).
    /// No-op when not editing.
    pub fn commit(&mut self) {
        if !self.editing {
            return;
        }
        let trimmed = self.input.value().trim().to_string();
        if !trimmed.is_empty() {
            self.value = trimmed;
        }
        self.editing = false;
        self.input.set_focus(false);
        self.outcome = EditOutcome::Committed(self.value.clone());
    }

    /// Cancel the in-flight edit, keeping the previous value. Records a
    /// [`EditOutcome::Cancelled`]. No-op when not editing.
    pub fn cancel(&mut self) {
        if !self.editing {
            return;
        }
        self.editing = false;
        self.input.set_focus(false);
        self.outcome = EditOutcome::Cancelled;
    }

    /// Rising-edge read of how the most recent edit ended. Returns the outcome
    /// once, then resets to [`EditOutcome::None`].
    pub fn take_outcome(&mut self) -> EditOutcome {
        std::mem::replace(&mut self.outcome, EditOutcome::None)
    }

    /// Place the widget (and its embedded field) at `rect` without a taffy
    /// pass — for callers embedding the label in their own immediate-mode
    /// chrome. Mirrors [`TextInput::apply_layout_external`].
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input.apply_layout_external(rect);
    }

    /// The vertical band `(top_y, line_height)` the idle text is centred in —
    /// matches [`TextInput`]'s own centring so text doesn't shift on edit.
    fn text_band(&self) -> (f64, f64) {
        let line_height = (self.size * LINE_HEIGHT_RATIO) as f64;
        let top = self.rect.y0 + (self.rect.height() - line_height) / 2.0;
        (top, line_height)
    }
}

impl Widget for EditableLabel {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length((self.size * LINE_HEIGHT_RATIO).max(28.0)),
                },
                flex_shrink: 1.0,
                flex_grow: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let (w, _) = text_renderer.measure_font(
            self.current_text(),
            self.size,
            None,
            TextWeight::Regular,
            false,
            &self.family,
        );
        taffy::Size {
            width: w + 4.0,
            height: (self.size * LINE_HEIGHT_RATIO).max(28.0),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
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
        if self.editing {
            // The real field owns the caret, selection, and chrome.
            self.input.apply_layout_external(self.rect);
            self.input.draw(scene, transform, text_renderer, theme, presence);
            return;
        }
        if self.value.is_empty() {
            return;
        }
        let color = self.idle_color.unwrap_or(theme.text_primary);
        let (top, _lh) = self.text_band();
        text_renderer.draw_text_font(
            scene,
            &self.value,
            self.size,
            color,
            transform * Affine::translate((self.rect.x0, top)),
            Some(self.rect.width() as f32 + 1.0),
            TextWeight::Regular,
            false,
            &self.family,
        );
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
        // Track the pointer for inside/outside classification of a press.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if !self.editing {
            return;
        }
        match event {
            // Enter commits, Esc cancels — the field itself consumes neither, so
            // the wrapper owns them. Intercept before forwarding.
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } if matches!(logical_key, Key::Named(NamedKey::Enter)) => {
                self.commit();
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } if matches!(logical_key, Key::Named(NamedKey::Escape)) => {
                self.cancel();
                *handled = true;
            }
            // A left press outside the field blur-commits. It is deliberately
            // NOT consumed, so the click still reaches whatever is under it
            // (another rename target, a button) — clicking away both saves and
            // acts, exactly like a native inline rename.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !self.rect.contains(self.last_pointer) => {
                self.commit();
                // handled stays false: fall through to the host.
            }
            // A move while the field is drag-selecting: forward and let the
            // field claim it. A plain hover move is forwarded but not claimed,
            // so the host keeps its own pointer bookkeeping.
            WindowEvent::CursorMoved { .. } => {
                let was = *handled;
                self.input.handle_event(
                    event, scale_factor, modifiers, state, root, handled, focus_target, node_id,
                    taffy,
                );
                if !self.input.is_dragging() {
                    *handled = was;
                }
            }
            // Everything else (typing, in-field clicks, selection drag start,
            // clipboard shortcuts, IME) goes straight to the real field.
            _ => {
                self.input.handle_event(
                    event, scale_factor, modifiers, state, root, handled, focus_target, node_id,
                    taffy,
                );
            }
        }
    }

    fn name(&self) -> &str {
        "EditableLabel"
    }

    fn test_id(&self) -> Option<&str> {
        if self._id.is_empty() {
            Some("editable_label")
        } else {
            Some(self._id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.editing && self.rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    /// Focus follows edit mode. Losing focus while editing blur-commits — the
    /// keyboard-navigation counterpart to a click-outside.
    fn set_focus(&mut self, focused: bool) {
        if focused {
            self.input.set_focus(true);
        } else if self.editing {
            self.commit();
        }
    }

    fn is_focused(&self) -> bool {
        self.editing
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editing && self.input.is_dragging()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if self.editing {
            return self.input.accessibility();
        }
        if self.value.is_empty() {
            return None;
        }
        Some(
            AccessNode::new(Role::TextInput)
                .with_optional_value(Some(self.value.clone()))
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(el: &mut EditableLabel, doc: &mut AutoCommit, ev: &WindowEvent, m: &Modifiers) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        el.handle_event(
            ev, 1.0, m, doc, &automerge::ROOT, &mut handled, &mut focus_target, node, &taffy,
        );
        handled
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

    fn named_key(k: NamedKey) -> WindowEvent {
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

    fn no_mods() -> Modifiers {
        Modifiers::default()
    }

    /// The headline behaviour: focus → type → click elsewhere → the new value
    /// is committed. Every step is driven through the real `handle_event` path,
    /// so the blur-commit is proven on the production event flow, not a helper.
    #[test]
    fn blur_commit_saves_typed_value_on_click_outside() {
        let mut el = EditableLabel::new("Old Name");
        el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
        let mut doc = AutoCommit::new();

        el.begin_edit();
        assert!(el.is_editing());

        // begin_edit selects-all, so the first keystroke replaces the value.
        drive(&mut el, &mut doc, &char_key("N"), &no_mods());
        drive(&mut el, &mut doc, &char_key("e"), &no_mods());
        drive(&mut el, &mut doc, &char_key("w"), &no_mods());
        assert_eq!(el.current_text(), "New", "typing replaced the selected value");
        assert!(el.is_editing(), "still editing until a commit trigger");

        // Move the pointer well outside the field, then press — a click away.
        drive(&mut el, &mut doc, &mouse_move(500.0, 500.0), &no_mods());
        let consumed = drive(&mut el, &mut doc, &left_press(), &no_mods());

        assert!(!el.is_editing(), "click outside ends the edit");
        assert_eq!(el.value(), "New", "blur committed the typed value");
        assert!(
            !consumed,
            "the outside click is not swallowed — it still reaches the host",
        );
        assert_eq!(el.take_outcome(), EditOutcome::Committed("New".into()));
        assert_eq!(el.take_outcome(), EditOutcome::None, "outcome is rising-edge");
    }

    /// Enter commits the current value; the event is consumed so it can't leak
    /// to a host shortcut.
    #[test]
    fn enter_commits_and_consumes() {
        let mut el = EditableLabel::new("first");
        el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
        let mut doc = AutoCommit::new();
        el.begin_edit();
        drive(&mut el, &mut doc, &char_key("z"), &no_mods()); // replaces selection → "z"
        let consumed = drive(&mut el, &mut doc, &named_key(NamedKey::Enter), &no_mods());
        assert!(consumed, "Enter is consumed");
        assert!(!el.is_editing());
        assert_eq!(el.value(), "z");
        assert_eq!(el.take_outcome(), EditOutcome::Committed("z".into()));
    }

    /// Esc cancels, restoring the previous value and reporting Cancelled.
    #[test]
    fn escape_cancels_and_keeps_previous_value() {
        let mut el = EditableLabel::new("keep me");
        el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
        let mut doc = AutoCommit::new();
        el.begin_edit();
        drive(&mut el, &mut doc, &char_key("X"), &no_mods()); // would-be new value
        let consumed = drive(&mut el, &mut doc, &named_key(NamedKey::Escape), &no_mods());
        assert!(consumed, "Esc is consumed");
        assert!(!el.is_editing());
        assert_eq!(el.value(), "keep me", "cancel restores the previous value");
        assert_eq!(el.take_outcome(), EditOutcome::Cancelled);
    }

    /// A blank entry keeps the previous value on commit (an empty rename is a
    /// no-op), so the label is never accidentally emptied.
    #[test]
    fn blank_commit_keeps_previous_value() {
        let mut el = EditableLabel::new("Original");
        el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
        let mut doc = AutoCommit::new();
        el.begin_edit(); // selects all
        // Ctrl+A then Delete would clear; simplest: select-all is already
        // active, type a space then backspace to empty it.
        drive(&mut el, &mut doc, &char_key(" "), &no_mods()); // → " "
        drive(&mut el, &mut doc, &named_key(NamedKey::Backspace), &no_mods()); // → ""
        assert_eq!(el.current_text(), "", "field is empty before commit");
        el.commit();
        assert_eq!(el.value(), "Original", "blank commit keeps the old value");
    }

    /// Losing focus while editing (keyboard nav away) also blur-commits — the
    /// counterpart of a click-outside for callers driven by a focus manager.
    #[test]
    fn focus_loss_blur_commits() {
        let mut el = EditableLabel::new("before");
        el.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 30.0));
        let mut doc = AutoCommit::new();
        el.begin_edit();
        drive(&mut el, &mut doc, &char_key("A"), &no_mods()); // → "A"
        el.set_focus(false);
        assert!(!el.is_editing());
        assert_eq!(el.value(), "A");
        assert_eq!(el.take_outcome(), EditOutcome::Committed("A".into()));
    }

    /// Selection works on the real field inside the label: begin_edit's
    /// select-all covers the whole value, and Ctrl+A re-selects it, proving the
    /// embedded `TextInput` (not a hand-rolled buffer) is doing the work.
    #[test]
    fn ctrl_a_selects_all_through_embedded_field() {
        let mut el = EditableLabel::new("hello world");
        el.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 30.0));
        let mut doc = AutoCommit::new();
        el.begin_edit();
        // Collapse the initial select-all by pressing Right (caret to end).
        drive(&mut el, &mut doc, &named_key(NamedKey::ArrowRight), &no_mods());
        // Now Ctrl+A must reselect everything, then typing replaces it all.
        let mut ctrl = winit::keyboard::ModifiersState::empty();
        ctrl |= winit::keyboard::ModifiersState::CONTROL;
        drive(&mut el, &mut doc, &char_key("a"), &Modifiers::from(ctrl));
        drive(&mut el, &mut doc, &char_key("Z"), &no_mods());
        assert_eq!(
            el.current_text(),
            "Z",
            "Ctrl+A selected the whole value so the next keystroke replaced it",
        );
    }
}
