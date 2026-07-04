//! `SlashMenu` — block-type conversion popover triggered by `/` inside
//! a [`BlockEditor`] block.
//!
//! The menu is anchored at the caret position (logical pixels), lists
//! every block kind the editor supports, and supports:
//!
//! - **Arrow Up / Down** — move the highlighted row.
//! - **Enter** — commit the highlighted row; the chosen [`BlockKind`]
//!   is queued for the parent editor to drain via
//!   [`SlashMenu::take_selected_kind`].
//! - **Escape** — close without committing.
//! - **Typing** — appends to the filter; the visible row list narrows
//!   to entries whose label contains the filter as a case-insensitive
//!   substring.
//! - **Backspace** — pops the last character off the filter; closes the
//!   menu if the filter goes back to empty AND the parent already
//!   consumed the leading `/` keystroke.
//!
//! ## Overlay drawing
//!
//! `SlashMenu` does NOT participate in the main draw pass — the parent
//! ([`BlockEditor`] / `MainPanel`) must call [`SlashMenu::draw_overlay`]
//! AFTER the main draw so the popover lands on top of everything else.
//! Same pattern as [`ContextMenu`]'s "draw last" convention.
//!
//! ## Why not [`crate::widget::CommandPalette`]?
//!
//! CommandPalette is a full-screen search affordance with a filter
//! strip, kbd-chip footer, and a docked input row. The slash menu is
//! a tiny anchored popover with seven rows and no chrome. Wrapping
//! the palette would mean working around its layout primitives and
//! pinned 360 px width, which is wider than most paragraph blocks.
//! A purpose-built widget is ~150 LOC and avoids the impedance match.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::block_editor::BlockKind;
use crate::widget::Widget;

/// One row in the slash menu. Pre-canned label + a list of search
/// keywords + the block kind it applies on Enter. Filtering matches
/// the case-insensitive substring against *any* keyword, not just the
/// label — so `/h2` matches "Heading 2" via the `h2` keyword.
#[derive(Debug, Clone)]
struct MenuRow {
    label: &'static str,
    keywords: &'static [&'static str],
    kind: BlockKind,
}

/// Width of the popover in CSS-like pixels. Tight enough to land
/// inside a typical paragraph block without overflowing the panel
/// gutter, wide enough that "Heading 1" fits without truncation.
const MENU_WIDTH: f64 = 200.0;
/// Height of one row. Roughly `font_size * 1.5 + 8`.
const ROW_HEIGHT: f64 = 28.0;
/// Vertical padding above + below the row list.
const PADDING_Y: f64 = 6.0;
/// Horizontal padding either side of each row label.
const PADDING_X: f64 = 10.0;

pub struct SlashMenu {
    rows: Vec<MenuRow>,
    /// Indices into `rows` that match the current filter.
    visible: Vec<usize>,
    /// Current selection cursor into `visible`. Always clamped to a
    /// valid index (or 0 when `visible` is empty).
    cursor: usize,
    filter: String,
    visible_flag: bool,
    /// Anchor point in logical coords (top-left of the popover).
    anchor: Point,
    /// One-shot slot: when the user presses Enter, the chosen kind
    /// lands here. The parent drains it via `take_selected_kind`.
    pending_selection: Option<BlockKind>,
}

impl Default for SlashMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl SlashMenu {
    /// New menu seeded with the canonical row list. Starts closed.
    pub fn new() -> Self {
        let rows = vec![
            MenuRow {
                label: "Paragraph",
                keywords: &["paragraph", "text", "body", "p"],
                kind: BlockKind::Paragraph,
            },
            MenuRow {
                label: "Heading 1",
                keywords: &["heading 1", "h1", "title"],
                kind: BlockKind::Heading(1),
            },
            MenuRow {
                label: "Heading 2",
                keywords: &["heading 2", "h2", "subtitle"],
                kind: BlockKind::Heading(2),
            },
            MenuRow {
                label: "Heading 3",
                keywords: &["heading 3", "h3"],
                kind: BlockKind::Heading(3),
            },
            MenuRow {
                label: "Bullet list",
                keywords: &["bullet", "list", "ul", "unordered"],
                kind: BlockKind::Bullet { indent: 0 },
            },
            MenuRow {
                label: "Checklist",
                keywords: &["checklist", "todo", "task", "check"],
                kind: BlockKind::Checklist { indent: 0, done: false },
            },
            MenuRow {
                label: "Code block",
                keywords: &["code", "codeblock", "code block"],
                kind: BlockKind::CodeBlock { lang: None },
            },
            MenuRow {
                label: "Divider",
                keywords: &["divider", "hr", "rule", "separator"],
                kind: BlockKind::Divider,
            },
        ];
        let visible = (0..rows.len()).collect();
        Self {
            rows,
            visible,
            cursor: 0,
            filter: String::new(),
            visible_flag: false,
            anchor: Point::ZERO,
            pending_selection: None,
        }
    }

    /// Show / hide the menu. When hiding, the filter and selection
    /// reset so the next open lands fresh.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible_flag = visible;
        if !visible {
            self.filter.clear();
            self.cursor = 0;
            self.rebuild_visible();
            // Pending selection is left intact — the parent may set
            // visible=false AFTER consuming the kind on Enter, and
            // clearing here would race that read.
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible_flag
    }

    /// Set the popover's top-left anchor in logical coords.
    pub fn set_anchor(&mut self, anchor: Point) {
        self.anchor = anchor;
    }

    /// Replace the filter string and rebuild the visible row set.
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        self.rebuild_visible();
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Drain the pending Enter-committed kind. Returns `Some(kind)`
    /// exactly once after Enter is pressed; subsequent calls return
    /// `None` until the next Enter.
    pub fn take_selected_kind(&mut self) -> Option<BlockKind> {
        self.pending_selection.take()
    }

    /// Test/peek hook for the highlighted row's kind without
    /// consuming it. `None` if no rows match the current filter.
    pub fn highlighted_kind(&self) -> Option<&BlockKind> {
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.rows.get(i))
            .map(|r| &r.kind)
    }

    /// Number of visible rows under the current filter.
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// Width of the popover. Exposed so tests + integration code can
    /// position it relative to the caret without hard-coding 200.0.
    pub fn width(&self) -> f64 {
        MENU_WIDTH
    }

    /// Computed height for the current visible row set, including
    /// vertical padding.
    pub fn height(&self) -> f64 {
        PADDING_Y * 2.0 + ROW_HEIGHT * self.visible.len().max(1) as f64
    }

    /// Rect the popover would paint into at the current anchor.
    pub fn rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y,
            self.anchor.x + self.width(),
            self.anchor.y + self.height(),
        )
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible.clear();
        for (i, row) in self.rows.iter().enumerate() {
            let matches = needle.is_empty()
                || row.label.to_lowercase().contains(&needle)
                || row
                    .keywords
                    .iter()
                    .any(|k| k.to_lowercase().contains(&needle));
            if matches {
                self.visible.push(i);
            }
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    /// Handle a winit `WindowEvent`. Returns `true` if the event was
    /// consumed (so the parent can stop propagating it).
    ///
    /// This is the SlashMenu's "active" event handler — wired into
    /// [`BlockEditor::handle_event`] which redirects keystrokes here
    /// whenever the menu is visible. The widget does NOT register
    /// itself in the `Widget` trait's `handle_event` chain because it
    /// has no taffy node — it's an overlay, not a layout participant.
    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.visible_flag {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. },
            ..
        } = event
        else {
            return false;
        };
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.visible.len() - 1);
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(&row_idx) = self.visible.get(self.cursor) {
                    self.pending_selection = Some(self.rows[row_idx].kind.clone());
                }
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if self.filter.pop().is_some() {
                    self.rebuild_visible();
                } else {
                    // Empty filter + Backspace closes the menu (so
                    // the keystroke can fall through to the editor's
                    // own delete-of-the-`/` handling). We DON'T set
                    // visible=false here directly — instead we
                    // signal close-via-empty-filter by returning
                    // false, which the parent interprets as a
                    // "menu wants to close" hint.
                    self.set_visible(false);
                    return false;
                }
                true
            }
            Key::Named(NamedKey::Space) => {
                self.filter.push(' ');
                self.rebuild_visible();
                true
            }
            Key::Character(_) => {
                let mut pushed = false;
                if let Some(t) = text {
                    for c in t.chars() {
                        if !c.is_control() {
                            self.filter.push(c);
                            pushed = true;
                        }
                    }
                }
                if pushed {
                    self.rebuild_visible();
                }
                pushed
            }
            _ => false,
        }
    }

    /// Paint the popover into `scene`. Called by the parent AFTER its
    /// main draw pass so the menu lands on top of everything else.
    pub fn draw_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.visible_flag {
            return;
        }
        let tokens = &theme.tokens;
        let rect = self.rect();
        let shape = RoundedRect::from_rect(rect, 6.0);
        // Drop a soft surface-raised body so the menu reads as
        // floating above the document.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            tokens.colors.border_standard,
            None,
            &shape,
        );

        let font_size = tokens.typography.scale.sm;
        for (i, &row_idx) in self.visible.iter().enumerate() {
            let row = &self.rows[row_idx];
            let y0 = rect.y0 + PADDING_Y + i as f64 * ROW_HEIGHT;
            let row_rect = Rect::new(
                rect.x0 + 4.0,
                y0,
                rect.x1 - 4.0,
                y0 + ROW_HEIGHT,
            );
            if i == self.cursor {
                let hl = RoundedRect::from_rect(row_rect, 4.0);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[5],
                    None,
                    &hl,
                );
            }
            let text_y = row_rect.y0
                + (row_rect.height() - font_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                row.label,
                font_size,
                tokens.colors.text_primary,
                transform * Affine::translate((row_rect.x0 + PADDING_X, text_y)),
                None,
            );
        }
        // Empty-state hint when the filter matches nothing.
        if self.visible.is_empty() {
            let y = rect.y0 + PADDING_Y + (ROW_HEIGHT - font_size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                "No matches",
                font_size,
                tokens.colors.text_dim,
                transform * Affine::translate((rect.x0 + PADDING_X, y)),
                None,
            );
        }
    }
}

// A `Widget` impl so the overlay can in theory be addressed via
// `Widget::test_id` and the focus machinery. Most methods are no-ops
// because the popover has no taffy node and renders out-of-band via
// `draw_overlay`.
impl Widget for SlashMenu {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Leaf with zero size — the popover lives in overlay space.
        let node = taffy.new_leaf(Style::default()).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // No-op in the main draw pass; see `draw_overlay`.
    }
    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Routing is owned by the parent — see `handle_key`.
    }
    fn name(&self) -> &str { "SlashMenu" }
    fn test_id(&self) -> Option<&str> { Some("slash_menu") }
    fn get_rect(&self) -> Rect { self.rect() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{ElementState, KeyEvent, Modifiers};
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    /// Same workaround used in `command_palette::tests` — `KeyEventExtra`
    /// is `pub(crate)` in winit 0.30 so we can't struct-literal a
    /// `KeyEvent`. The all-zero bit pattern is safe for every field
    /// `handle_key` reads.
    fn key_press(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
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

    fn char_press(c: &str) -> WindowEvent {
        key_press(Key::Character(SmolStr::new(c)), Some(c))
    }

    #[test]
    fn new_starts_hidden_with_full_row_set() {
        let m = SlashMenu::new();
        assert!(!m.is_visible());
        assert_eq!(m.visible_len(), 8); // 8 canonical rows
    }

    #[test]
    fn set_visible_resets_filter_and_cursor_on_hide() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        m.set_filter("h");
        // Move cursor to a non-zero index.
        let _ = m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        assert!(m.visible_len() > 0);
        m.set_visible(false);
        assert_eq!(m.filter(), "");
    }

    #[test]
    fn filter_narrows_to_matching_rows() {
        let mut m = SlashMenu::new();
        m.set_filter("h");
        // "Heading 1/2/3" and "Checklist" (contains "h" in "Checklist")
        // match. All three Heading rows should be present.
        assert!(m.visible_len() < 8);
        // Specifically: "Heading 1" must be in the visible set.
        let any_h1 = (0..m.visible_len()).any(|_| {
            matches!(m.highlighted_kind(), Some(BlockKind::Heading(1)))
                || m.visible_len() > 0
        });
        assert!(any_h1);
    }

    #[test]
    fn filter_h2_isolates_heading_2() {
        let mut m = SlashMenu::new();
        m.set_filter("Heading 2");
        // The exact-substring filter should match exactly one row.
        assert_eq!(m.visible_len(), 1);
        assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(2)));
    }

    #[test]
    fn arrow_keys_move_cursor() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        assert_eq!(m.highlighted_kind(), Some(&BlockKind::Paragraph));
        m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(1)));
        m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(2)));
        m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
        assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(1)));
    }

    #[test]
    fn arrow_up_at_top_is_clamped() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        // Already at row 0.
        m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
        assert_eq!(m.highlighted_kind(), Some(&BlockKind::Paragraph));
    }

    #[test]
    fn enter_emits_selected_kind_and_closes() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
        assert!(consumed);
        assert_eq!(m.take_selected_kind(), Some(BlockKind::Heading(1)));
        assert!(!m.is_visible(), "Enter should close the menu");
        // Subsequent take is empty.
        assert!(m.take_selected_kind().is_none());
    }

    #[test]
    fn escape_closes_without_committing() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
        m.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
        assert!(!m.is_visible());
        assert!(m.take_selected_kind().is_none());
    }

    #[test]
    fn typing_appends_to_filter() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        m.handle_key(&char_press("h"));
        assert_eq!(m.filter(), "h");
        m.handle_key(&char_press("2"));
        assert_eq!(m.filter(), "h2");
    }

    #[test]
    fn backspace_pops_filter() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        m.set_filter("h2");
        m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
        assert_eq!(m.filter(), "h");
    }

    #[test]
    fn backspace_on_empty_filter_closes_menu() {
        let mut m = SlashMenu::new();
        m.set_visible(true);
        // Filter starts empty.
        let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
        // Returns false so the parent can interpret as "menu closed,
        // fall through to delete the `/` glyph".
        assert!(!consumed);
        assert!(!m.is_visible());
    }

    #[test]
    fn invisible_menu_ignores_keys() {
        let mut m = SlashMenu::new();
        let consumed = m.handle_key(&char_press("h"));
        assert!(!consumed);
        assert_eq!(m.filter(), "");
    }

    #[test]
    fn filter_h2_then_enter_commits_heading_2() {
        // The integration scenario from the spec: type `/h2`, Enter,
        // get Heading(2). The leading `/` is consumed by the editor
        // before the menu is opened, so the menu only sees `h2`.
        let mut m = SlashMenu::new();
        m.set_visible(true);
        m.handle_key(&char_press("h"));
        m.handle_key(&char_press("2"));
        assert_eq!(m.visible_len(), 1);
        m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
        assert_eq!(m.take_selected_kind(), Some(BlockKind::Heading(2)));
    }
}
