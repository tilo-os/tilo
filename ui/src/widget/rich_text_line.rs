//! `RichTextLine` — a single editable line of text wrapping a
//! local-mode [`TextInput`].
//!
//! Designed as the leaf cell of a block editor. The inner `TextInput`
//! handles cursor placement, selection, IME, copy/paste — everything
//! that's "edit a string". `RichTextLine` adds the block-editor-shaped
//! affordances around it:
//!
//! * Intercepts **Enter**, **Backspace at byte 0**, **Up arrow at the
//!   start**, and **Down arrow at the end** before they reach the
//!   underlying input. These keystrokes don't insert / delete / move
//!   the cursor — they're queued as [`LineSignal`]s the parent
//!   ([`crate::widget::block_editor::BlockEditor`]) consumes.
//! * Exposes the cursor position so the parent can split the line at
//!   the right byte offset.
//! * Carries a per-line font size so a block editor can render
//!   headings at H1/H2/H3 sizes without forking the widget.
//!
//! Phase 2 doesn't render inline marks (bold, italic) — Phase 4 will.
//! For now the rendered output is identical to a plain `TextInput`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

/// Signals emitted by a `RichTextLine` when a key would carry the
/// edit out of the line itself. The parent (a `BlockEditor`) drains
/// these via [`RichTextLine::take_signal`] after every handled event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSignal {
    /// User pressed Enter while focused. `split_at` is the byte index
    /// of the caret *inside the line's current value at the moment
    /// Enter was pressed* — the parent splits the line there, drops
    /// the suffix into a new block, and re-focuses.
    Enter { split_at: usize },
    /// User pressed Backspace while the caret was at byte 0 with no
    /// active selection. Parent decides whether to merge with the
    /// previous block, delete an empty block, or no-op.
    BackspaceAtStart,
    /// User pressed Up while the caret was on the first visual line
    /// (single-line for now, so always). Parent moves focus up.
    ArrowUp,
    /// User pressed Down while the caret was on the last visual line.
    /// Parent moves focus down.
    ArrowDown,
}

/// Default font size when [`RichTextLine::with_size`] hasn't been
/// called. Matches the body baseline the rest of the UI uses for
/// `TextInput`.
const DEFAULT_FONT_SIZE: f32 = 16.0;

pub struct RichTextLine {
    inner: TextInput,
    /// Pending signal from the most-recent `handle_event`. At most one
    /// signal is in flight per frame; new signals overwrite the slot.
    /// Drained by [`take_signal`].
    signal: Option<LineSignal>,
    /// Per-line font size in CSS-like pixels. Drives both the rendered
    /// glyph height and the leaf's intrinsic taffy height.
    font_size: f32,
}

impl RichTextLine {
    /// Empty, unfocused line ready to be filled by [`with_text`] or
    /// [`set_text`].
    pub fn new() -> Self {
        Self {
            inner: TextInput::local(),
            signal: None,
            font_size: DEFAULT_FONT_SIZE,
        }
    }

    /// Builder-style: seed the line's text. Cursor parked at the end.
    /// The dirty flag stays false — this is a programmatic seed.
    pub fn with_text(mut self, s: &str) -> Self {
        self.inner = self.inner.with_initial_value(s);
        self
    }

    /// Builder-style: set the per-line font size (in CSS-like pixels).
    /// Used by `BlockEditor` to render heading blocks (H1=28, H2=22,
    /// H3=18) larger than paragraph blocks.
    pub fn with_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    /// Current text content.
    pub fn value(&self) -> &str {
        self.inner.value()
    }

    /// Replace the text programmatically. Does NOT mark the line as
    /// dirty (programmatic writes are not user edits). Cursor lands
    /// at the end after clamping to a char boundary.
    pub fn set_text(&mut self, s: &str) {
        self.inner.set_text(s);
        // After a programmatic set_text the cursor needs to be a
        // valid offset; TextInput::set_text just runs clamp, so
        // park the caret at the end of the new value (matches what
        // `with_initial_value` does at construction time).
        self.inner.set_cursor_pos(s.len());
    }

    /// One-shot read of user edits. Returns `Some(text)` if the user
    /// has typed/deleted since the last call. Clears the dirty flag.
    pub fn take_dirty(&mut self) -> Option<String> {
        self.inner.take_dirty()
    }

    /// True if the inner input currently holds focus.
    pub fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }

    /// Set focus on the underlying input. When losing focus we don't
    /// touch the cursor — it stays wherever the user last left it.
    pub fn set_focus(&mut self, focused: bool) {
        self.inner.set_focus(focused);
    }

    /// True iff the cursor is at byte 0 (the start of the line).
    pub fn cursor_at_start(&self) -> bool {
        self.inner.cursor_pos() == 0
    }

    /// True iff the cursor is at `value().len()` (the end of the line).
    pub fn cursor_at_end(&self) -> bool {
        self.inner.cursor_pos() == self.inner.value().len()
    }

    /// Current cursor position as a byte index into [`value`]. Always
    /// on a UTF-8 char boundary.
    pub fn cursor_pos(&self) -> usize {
        self.inner.cursor_pos()
    }

    /// Move the caret to the given byte index. Clamped + char-boundary
    /// safe. Used by `BlockEditor` after a split to land the caret at
    /// position 0 of the new block, or after a merge to land it at
    /// the join point.
    pub fn set_cursor_pos(&mut self, pos: usize) {
        self.inner.set_cursor_pos(pos);
    }

    /// Drain the pending signal (if any). At most one signal is in
    /// flight per frame.
    pub fn take_signal(&mut self) -> Option<LineSignal> {
        self.signal.take()
    }

    /// Apply layout directly. Used by `BlockEditor` which owns its
    /// children's rects and lays them out manually rather than via
    /// taffy nesting (one taffy leaf per block keeps the focus
    /// machinery simple).
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.inner.apply_layout_external(rect);
    }

    /// Font size in CSS-like pixels. Read by `BlockEditor` to lay out
    /// each block at the right height before delegating to
    /// [`apply_layout_external`].
    pub fn font_size(&self) -> f32 {
        self.font_size
    }
}

impl Default for RichTextLine {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for RichTextLine {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Local-mode input; nothing to sync from the CRDT.
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Single leaf sized by `font_size * 1.5` (line height plus a
        // small breathing room) so headings end up taller than
        // paragraph rows without needing a measure pass.
        let height = (self.font_size * 1.5).max(20.0);
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(height),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        self.inner.apply_layout_external(rect);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // For Phase 2 we delegate verbatim to the inner TextInput;
        // marks rendering comes in Phase 4. The inner widget already
        // honours its rect via `apply_layout_external`.
        self.inner
            .draw(scene, transform, text_renderer, theme, presence);
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
        // Intercept the four "carry the edit out of the line" keys
        // BEFORE the inner TextInput sees them — otherwise Enter would
        // be filtered as a control char (no-op), and Backspace at
        // byte 0 would silently do nothing (TextInput already guards
        // `if cursor_pos > 0` so we can't piggy-back on the same
        // event). Up / Down arrows would just walk into the next /
        // previous visual line within the input (currently a no-op
        // since it's single-line, but we still want to claim them).
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if self.inner.is_focused() {
                match logical_key {
                    Key::Named(NamedKey::Enter) => {
                        self.signal = Some(LineSignal::Enter {
                            split_at: self.inner.cursor_pos(),
                        });
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        // Only intercept when the caret is at byte 0
                        // *and* there's nothing selected — otherwise
                        // the inner input should handle the delete.
                        if self.inner.cursor_pos() == 0 {
                            self.signal = Some(LineSignal::BackspaceAtStart);
                            *handled = true;
                            return;
                        }
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.signal = Some(LineSignal::ArrowUp);
                        *handled = true;
                        return;
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        self.signal = Some(LineSignal::ArrowDown);
                        *handled = true;
                        return;
                    }
                    _ => {}
                }
            }
        }

        self.inner.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }

    fn name(&self) -> &str {
        "RichTextLine"
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.inner.set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.inner.is_focused()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::KeyEvent;
    use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey, SmolStr};

    /// `KeyEventExtra` is `pub(crate)` in winit 0.30, so we can't
    /// construct a `KeyEvent` via struct-literal — same workaround as
    /// `command_palette::tests::key_press`. The all-zero bit pattern
    /// is safe for every pub field this widget reads.
    fn key_press(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn char_press(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(c));
        ev.physical_key = PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(c));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(line: &mut RichTextLine, event: &WindowEvent) {
        let mut state = automerge::AutoCommit::new();
        let root = automerge::ROOT.clone();
        let modifiers = Modifiers::default();
        let mut handled = false;
        let mut focus_target = None;
        let taffy: TaffyTree<()> = TaffyTree::new();
        // We never reach the taffy tree from `handle_event` here
        // (no mouse events) so a fake NodeId is safe.
        let fake_node = NodeId::new(0);
        line.handle_event(
            event,
            1.0,
            &modifiers,
            &mut state,
            &root,
            &mut handled,
            &mut focus_target,
            fake_node,
            &taffy,
        );
    }

    #[test]
    fn new_starts_empty_unfocused() {
        let l = RichTextLine::new();
        assert_eq!(l.value(), "");
        assert!(!l.is_focused());
        assert!(l.cursor_at_start());
        assert!(l.cursor_at_end());
    }

    #[test]
    fn with_text_seeds_value_and_parks_cursor_at_end() {
        let l = RichTextLine::new().with_text("hello");
        assert_eq!(l.value(), "hello");
        assert!(l.cursor_at_end());
        assert!(!l.cursor_at_start());
    }

    #[test]
    fn set_text_does_not_mark_dirty() {
        let mut l = RichTextLine::new();
        l.set_text("seeded");
        assert!(l.take_dirty().is_none(), "programmatic set_text is not dirty");
        assert_eq!(l.value(), "seeded");
        // Cursor parked at the end of the new content.
        assert!(l.cursor_at_end());
    }

    #[test]
    fn cursor_at_start_after_set_cursor_pos_zero() {
        let mut l = RichTextLine::new().with_text("abc");
        l.set_cursor_pos(0);
        assert!(l.cursor_at_start());
        assert!(!l.cursor_at_end());
        l.set_cursor_pos(3);
        assert!(l.cursor_at_end());
    }

    #[test]
    fn enter_key_yields_signal_with_cursor_byte() {
        let mut l = RichTextLine::new().with_text("abcdef");
        l.set_focus(true);
        l.set_cursor_pos(3);
        dispatch(&mut l, &key_press(NamedKey::Enter));
        match l.take_signal() {
            Some(LineSignal::Enter { split_at: 3 }) => {}
            other => panic!("expected Enter@3, got {:?}", other),
        }
        // The inner input did NOT receive the Enter key — value is
        // unchanged.
        assert_eq!(l.value(), "abcdef");
    }

    #[test]
    fn enter_at_end_yields_signal_with_full_length() {
        let mut l = RichTextLine::new().with_text("hi");
        l.set_focus(true);
        dispatch(&mut l, &key_press(NamedKey::Enter));
        match l.take_signal() {
            Some(LineSignal::Enter { split_at }) => assert_eq!(split_at, 2),
            other => panic!("expected Enter@2, got {:?}", other),
        }
    }

    #[test]
    fn backspace_at_byte_zero_yields_signal() {
        let mut l = RichTextLine::new().with_text("abc");
        l.set_focus(true);
        l.set_cursor_pos(0);
        dispatch(&mut l, &key_press(NamedKey::Backspace));
        assert_eq!(l.take_signal(), Some(LineSignal::BackspaceAtStart));
        assert_eq!(l.value(), "abc");
    }

    #[test]
    fn backspace_after_byte_zero_falls_through_to_inner() {
        let mut l = RichTextLine::new().with_text("abc");
        l.set_focus(true);
        l.set_cursor_pos(3);
        dispatch(&mut l, &key_press(NamedKey::Backspace));
        assert!(l.take_signal().is_none());
        // Inner TextInput deleted the last char.
        assert_eq!(l.value(), "ab");
        // And that counts as a user edit.
        assert!(l.take_dirty().is_some());
    }

    #[test]
    fn arrow_up_down_emit_signals() {
        let mut l = RichTextLine::new().with_text("x");
        l.set_focus(true);
        dispatch(&mut l, &key_press(NamedKey::ArrowUp));
        assert_eq!(l.take_signal(), Some(LineSignal::ArrowUp));
        dispatch(&mut l, &key_press(NamedKey::ArrowDown));
        assert_eq!(l.take_signal(), Some(LineSignal::ArrowDown));
    }

    #[test]
    fn typing_marks_dirty_and_appends() {
        let mut l = RichTextLine::new();
        l.set_focus(true);
        dispatch(&mut l, &char_press("h"));
        dispatch(&mut l, &char_press("i"));
        assert_eq!(l.value(), "hi");
        let dirty = l.take_dirty();
        assert_eq!(dirty.as_deref(), Some("hi"));
        // Second drain is empty.
        assert!(l.take_dirty().is_none());
    }

    #[test]
    fn unfocused_line_ignores_keys() {
        let mut l = RichTextLine::new().with_text("abc");
        // No set_focus(true) on purpose.
        dispatch(&mut l, &key_press(NamedKey::Enter));
        assert!(l.take_signal().is_none(), "unfocused line must not emit");
        dispatch(&mut l, &char_press("z"));
        assert_eq!(l.value(), "abc");
    }
}
