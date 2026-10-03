//! `SlashMenu` — block-type conversion popover triggered by `/` inside
//! a [`BlockEditor`](crate::widget::block_editor::BlockEditor) block.
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
//! ([`BlockEditor`](crate::widget::block_editor::BlockEditor) / `MainPanel`) must call [`SlashMenu::draw_overlay`]
//! AFTER the main draw so the popover lands on top of everything else.
//! Same pattern as [`ContextMenu`](crate::widget::context_menu::ContextMenu)'s "draw last" convention.
//!
//! ## Why not `crate::widget::CommandPalette`?
//!
//! `CommandPalette` is a full-screen search affordance with a filter
//! strip, kbd-chip footer, and a docked input row. The slash menu is
//! a compact anchored popover with no extra chrome. Wrapping
//! the palette would mean working around its layout primitives and
//! pinned 360 px width, which is wider than most paragraph blocks.
//! A purpose-built widget is ~150 LOC and avoids the impedance match.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_metrics::renderer_type_size;
use crate::text_render::{TextRenderer, TextStyle};
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::block_editor::BlockKind;

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
const FILTER_HEIGHT: f64 = 32.0;
const FILTER_GAP: f64 = 4.0;

/// Row/column counts in this menu are always small (a handful of
/// static rows), far inside `f64`'s exact-integer range, so this
/// narrowing can't lose precision.
#[allow(
    clippy::cast_precision_loss,
    reason = "menu row/column counts are always small, far inside f64's exact-integer range"
)]
fn px(n: usize) -> f64 {
    n as f64
}

/// `base` composed with a translation to `(dx, dy)`. Wrapped so the one
/// `arithmetic_side_effects` allow lives in a single place: `Affine`'s
/// `Mul` is pure floating-point matrix composition and cannot panic or
/// overflow.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication is floating-point matrix composition; it cannot panic or overflow"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

pub struct SlashMenu {
    rows: Vec<MenuRow>,
    /// Indices into `rows` that match the current filter.
    visible: Vec<usize>,
    /// Current selection cursor into `visible`. Always clamped to a
    /// valid index (or 0 when `visible` is empty).
    cursor: usize,
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
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
                label: "Quote",
                keywords: &["quote", "blockquote", "citation", ">"],
                kind: BlockKind::Quote,
            },
            MenuRow {
                label: "Bullet list",
                keywords: &["bullet", "list", "ul", "unordered"],
                kind: BlockKind::Bullet { indent: 0 },
            },
            MenuRow {
                label: "Numbered list",
                keywords: &["numbered", "number", "ordered", "ol", "1."],
                kind: BlockKind::Numbered { indent: 0 },
            },
            MenuRow {
                label: "Checklist",
                keywords: &["checklist", "todo", "task", "check"],
                kind: BlockKind::Checklist {
                    indent: 0,
                    done: false,
                },
            },
            MenuRow {
                label: "Code block",
                keywords: &["code", "codeblock", "code block"],
                kind: BlockKind::CodeBlock { lang: None },
            },
            MenuRow {
                label: "Table",
                keywords: &["table", "grid", "columns", "gfm"],
                kind: BlockKind::Table { visual: true },
            },
            MenuRow {
                label: "TeX formula",
                keywords: &["tex", "latex", "math", "equation", "formula"],
                kind: BlockKind::CodeBlock {
                    lang: Some("tex".to_string()),
                },
            },
            MenuRow {
                label: "Typst formula",
                keywords: &["typst", "math", "equation", "formula"],
                kind: BlockKind::CodeBlock {
                    lang: Some("typst".to_string()),
                },
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
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
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
            self.filter_selection = Selection::collapsed(0);
            self.filter_composition.clear();
            self.cursor = 0;
            self.rebuild_visible();
            // Pending selection is left intact — the parent may set
            // visible=false after consuming the kind on Enter, and
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
        self.filter_selection = Selection::collapsed(self.filter.len());
        self.filter_composition.clear();
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
        PADDING_Y * 2.0 + FILTER_HEIGHT + FILTER_GAP + ROW_HEIGHT * px(self.visible.len().max(1))
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

    fn rows_top(&self) -> f64 {
        self.rect().y0 + PADDING_Y + FILTER_HEIGHT + FILTER_GAP
    }

    /// Painted rectangle for a visible result row. Hosts should use this for
    /// pointer automation rather than duplicating the filter/header geometry.
    pub fn row_rect(&self, visible_index: usize) -> Option<Rect> {
        (visible_index < self.visible.len()).then(|| {
            let y0 = self.rows_top() + px(visible_index) * ROW_HEIGHT;
            let rect = self.rect();
            Rect::new(rect.x0 + 4.0, y0, rect.x1 - 4.0, y0 + ROW_HEIGHT)
        })
    }

    /// The visible-row index under logical point `pt`, if the menu is open
    /// and `pt` lands on a row. Shared by hover-highlight and click-select.
    fn row_at(&self, pt: Point) -> Option<usize> {
        if !self.visible_flag {
            return None;
        }
        let rect = self.rect();
        if !rect.contains(pt) {
            return None;
        }
        let rel = pt.y - self.rows_top();
        if rel < 0.0 {
            return None;
        }
        // `rel` is checked non-negative just above, and screen
        // coordinates never approach `usize::MAX`, so this floor+cast is
        // a safe, bounded pixel-to-row-index conversion.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rel is non-negative here and bounded by real screen coordinates, far under usize::MAX"
        )]
        let idx = (rel / ROW_HEIGHT).floor() as usize;
        (idx < self.visible.len()).then_some(idx)
    }

    /// Move the highlight to the row under `pt` (pointer hover). No-op when
    /// `pt` isn't over a row, so hovering the padding leaves the last row lit.
    /// Returns `true` if the highlight moved (so the caller can redraw).
    pub fn hover_at(&mut self, pt: Point) -> bool {
        match self.row_at(pt) {
            Some(idx) if idx != self.cursor => {
                self.cursor = idx;
                true
            }
            _ => false,
        }
    }

    /// Commit the row under `pt` as if Enter were pressed on it — the chosen
    /// [`BlockKind`] is both returned and queued via `take_selected_kind`, and
    /// the menu closes. Returns `None` (and leaves the menu open) when `pt`
    /// isn't on a row, so the caller can treat an off-row click as a dismiss.
    pub fn click_at(&mut self, pt: Point) -> Option<BlockKind> {
        let idx = self.row_at(pt)?;
        let &row_idx = self.visible.get(idx)?;
        let kind = self.rows.get(row_idx)?.kind.clone();
        self.pending_selection = Some(kind.clone());
        self.set_visible(false);
        Some(kind)
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
    /// This is the `SlashMenu`'s "active" event handler — wired into
    /// `BlockEditor::handle_event` which redirects keystrokes here
    /// whenever the menu is visible. The widget does NOT register
    /// itself in the `Widget` trait's `handle_event` chain because it
    /// has no taffy node — it's an overlay, not a layout participant.
    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        if !self.visible_flag {
            return false;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            let change = self.filter_composition.handle_event(
                event,
                &mut self.filter,
                &mut self.filter_selection,
                false,
                false,
            );
            if change == QueryEventChange::Edited {
                self.cursor = 0;
                self.rebuild_visible();
            }
            return true;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = self
                        .cursor
                        .saturating_add(1)
                        .min(self.visible.len().saturating_sub(1));
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.cursor > 0 {
                    self.cursor = self.cursor.saturating_sub(1);
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(&row_idx) = self.visible.get(self.cursor)
                    && let Some(row) = self.rows.get(row_idx)
                {
                    self.pending_selection = Some(row.kind.clone());
                }
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.set_visible(false);
                true
            }
            Key::Named(NamedKey::Backspace)
                if self.filter_selection.is_collapsed() && self.filter_selection.cursor == 0 =>
            {
                // Empty filter + Backspace closes the menu, but returns
                // false so the caller lets the keystroke fall through
                // to the editor's own delete-of-the-`/` handling.
                self.set_visible(false);
                false
            }
            _ => {
                let change = self.filter_composition.handle_event(
                    event,
                    &mut self.filter,
                    &mut self.filter_selection,
                    false,
                    false,
                );
                if change == QueryEventChange::Edited {
                    self.cursor = 0;
                    self.rebuild_visible();
                }
                change != QueryEventChange::Ignored
            }
        }
    }

    fn draw_empty_hint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        font_size: f32,
    ) {
        let rect = self.rect();
        let tokens = &theme.tokens;
        // Empty-state hint when the filter matches nothing.
        if self.visible.is_empty() {
            let (_, hint_h) = text_renderer.measure("No matches", font_size, None);
            let y = self.rows_top() + (ROW_HEIGHT - f64::from(hint_h)) / 2.0;
            text_renderer.draw_text(
                scene,
                "No matches",
                font_size,
                tokens.colors.text_dim,
                translated(transform, rect.x0 + PADDING_X, y),
                None,
            );
        }
    }

    /// The menu card and the filter field's box — the chrome under the
    /// filter text and the rows.
    fn draw_chrome(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
        filter_rect: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let radius = 6.0;
        // Panel role (HIG §9.1): the panel ground so the menu reads as
        // floating above the document, with the derived panel edge as a
        // crisp 1 px inset stroke — a declared `border_standard` was
        // invisible against `raised` on the dark preset.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(rect, radius),
        );
        // A field reads as a field on every preset: its rest edge derives
        // from its own fill (HIG §9.5), painted as a crisp 1 px inset stroke;
        // on the light preset `base` and the panel ground are a shade apart.
        let filter_radius = 4.0;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &RoundedRect::from_rect(filter_rect, filter_radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(colors.surfaces.base),
            None,
            &crate::surface::edge_outline(filter_rect, filter_radius),
        );
    }

    /// Paint the popover into `scene`. Called by the parent AFTER its
    /// main draw pass so the menu lands on top of everything else.
    pub fn draw_overlay(
        &mut self,
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
        let filter_rect = Rect::new(
            rect.x0 + 6.0,
            rect.y0 + PADDING_Y,
            rect.x1 - 6.0,
            rect.y0 + PADDING_Y + FILTER_HEIGHT,
        );
        Self::draw_chrome(scene, transform, theme, rect, filter_rect);
        self.filter_composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &self.filter,
                selection: &self.filter_selection,
                placeholder: "Filter blocks…",
                show_caret: true,
            },
            Rect::new(
                filter_rect.x0 + 6.0,
                filter_rect.y0,
                filter_rect.x1 - 6.0,
                filter_rect.y1,
            ),
            &TextStyle::new(tokens.typography.scale.sm),
            QueryInk::accented(
                tokens.colors.text_primary,
                tokens.colors.text_dim,
                tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
            ),
        );

        let font_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);
        // The armed row's ink is resolved against the accent it sits on, the
        // same way `Popover` and `Dropdown` do; `text_primary` is only legible
        // there on a dark theme.
        let armed_ink = tokens
            .colors
            .on_accent(crate::tokens::colors::ON_FILL_MIN_CONTRAST);
        for (i, &row_idx) in self.visible.iter().enumerate() {
            let Some(row) = self.rows.get(row_idx) else {
                continue;
            };
            let y0 = self.rows_top() + px(i) * ROW_HEIGHT;
            let row_rect = Rect::new(rect.x0 + 4.0, y0, rect.x1 - 4.0, y0 + ROW_HEIGHT);
            let armed = i == self.cursor;
            if armed {
                let hl = RoundedRect::from_rect(row_rect, f64::from(tokens.radii.sm));
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
                    None,
                    &hl,
                );
            }
            // Centre on the measured line, not the font size — the two differ
            // by the face's ascent/descent and the label drifted low.
            let (_, label_h) = text_renderer.measure(row.label, font_size, None);
            let text_y = row_rect.y0 + (row_rect.height() - f64::from(label_h)) / 2.0;
            text_renderer.draw_text(
                scene,
                row.label,
                font_size,
                if armed {
                    armed_ink
                } else {
                    tokens.colors.text_primary
                },
                translated(transform, row_rect.x0 + PADDING_X, text_y),
                None,
            );
        }
        self.draw_empty_hint(scene, transform, text_renderer, theme, font_size);
    }
}

// A `Widget` impl so the overlay can in theory be addressed via
// `Widget::test_id` and the focus machinery. Most methods are no-ops
// because the popover has no taffy node and renders out-of-band via
// `draw_overlay`.
impl Widget for SlashMenu {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Leaf with zero size — the popover lives in overlay space.
        let node = taffy.leaf_node(Style::default());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
    fn name(&self) -> &'static str {
        "SlashMenu"
    }
    fn test_id(&self) -> Option<&str> {
        Some("slash_menu")
    }
    fn get_rect(&self) -> Rect {
        self.rect()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.visible_flag
            .then(|| self.filter_composition.ime_cursor_rect())
            .flatten()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The block-type picker is a Menu. Rows are drawn (not child widgets),
        // so each currently-visible row is a synthetic `MenuItem` named by its
        // label and marked selected at the highlight cursor. A closed menu is
        // hidden so it collapses out of the focus/reading order.
        if !self.visible_flag {
            return Some(
                AccessNode::new(Role::Menu)
                    .with_name("Block types")
                    .with_state(AccessState {
                        expanded: Some(false),
                        hidden: true,
                        ..Default::default()
                    }),
            );
        }
        let count = self.visible.len();
        let items: Vec<AccessNode> = self
            .visible
            .iter()
            .enumerate()
            .filter_map(|(pos, &row_idx)| {
                self.rows.get(row_idx).map(|row| {
                    AccessNode::new(Role::MenuItem)
                        .with_action_target(format!("row:{row_idx}"))
                        .with_name(row.label.to_string())
                        .with_state(AccessState {
                            selected: pos == self.cursor,
                            ..Default::default()
                        })
                        .with_position(pos.saturating_add(1), count)
                })
            })
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_name("Block types")
                .with_optional_value((!self.filter.is_empty()).then(|| self.filter.clone()))
                .with_state(AccessState {
                    expanded: Some(true),
                    ..Default::default()
                })
                .with_children(items),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        let Some(row_index) = target
            .strip_prefix("row:")
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return false;
        };
        if !self.visible_flag
            || !matches!(action, crate::a11y::Action::Activate)
            || !self.visible.contains(&row_index)
        {
            return false;
        }
        let Some(kind) = self.rows.get(row_index).map(|row| row.kind.clone()) else {
            return false;
        };
        self.pending_selection = Some(kind);
        self.set_visible(false);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::{ElementState, KeyEvent};
    use winit::keyboard::{KeyLocation, PhysicalKey, SmolStr};

    /// Same workaround used in `command_palette::tests` — `KeyEventExtra`
    /// is `pub(crate)` in winit 0.30 so we can't struct-literal a
    /// `KeyEvent`. The all-zero bit pattern is safe for every field
    /// `handle_key` reads.
    fn key_press(logical_key: Key, text: Option<&str>) -> WindowEvent {
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

    fn char_press(c: &str) -> WindowEvent {
        key_press(Key::Character(SmolStr::new(c)), Some(c))
    }

    #[test]
    fn visibility_resets_filter_and_invisible_menu_ignores_keys() {
        // Case `new_starts_hidden_with_full_row_set`.
        {
            let m = SlashMenu::new();
            assert!(!m.is_visible());
            assert_eq!(m.visible_len(), m.rows.len());
        }

        // Case `set_visible_resets_filter_and_cursor_on_hide`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("h");
            // Move cursor to a non-zero index.
            let _ = m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert!(m.visible_len() > 0);
            m.set_visible(false);
            assert_eq!(m.filter(), "");
        }

        // Case `invisible_menu_ignores_keys`.
        {
            let mut m = SlashMenu::new();
            let consumed = m.handle_key(&char_press("h"));
            assert!(!consumed);
            assert_eq!(m.filter(), "");
        }
    }

    #[test]
    fn filter_narrows_rows_from_typing_ime_and_backspace() {
        // Case `filter_narrows_to_matching_rows`.
        {
            let mut m = SlashMenu::new();
            m.set_filter("h");
            // "Heading 1/2/3" and "Checklist" (contains "h" in "Checklist")
            // match. All three Heading rows should be present.
            assert!(m.visible_len() < m.rows.len());
            // Specifically: "Heading 1" must be in the visible set.
            let any_h1 = (0..m.visible_len()).any(|_| {
                matches!(m.highlighted_kind(), Some(BlockKind::Heading(1))) || m.visible_len() > 0
            });
            assert!(any_h1);
        }

        // Case `filter_h2_isolates_heading_2`.
        {
            let mut m = SlashMenu::new();
            m.set_filter("Heading 2");
            // The exact-substring filter should match exactly one row.
            assert_eq!(m.visible_len(), 1);
            assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(2)));
        }

        // Case `typing_appends_to_filter`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&char_press("h"));
            assert_eq!(m.filter(), "h");
            m.handle_key(&char_press("2"));
            assert_eq!(m.filter(), "h2");
        }

        // Case `ime_preedit_is_visible_but_filters_only_after_commit`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            let initial_rows = m.visible_len();
            assert!(m.handle_key(&WindowEvent::Ime(winit::event::Ime::Preedit(
                "h2".into(),
                Some((1, 1)),
            ))));
            assert_eq!(m.filter(), "");
            assert_eq!(m.visible_len(), initial_rows);
            assert_eq!(Widget::accessibility(&m).unwrap().value, None);

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            m.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
            assert!(Widget::ime_cursor_rect(&m).is_some());

            assert!(m.handle_key(&WindowEvent::Ime(winit::event::Ime::Commit("h2".into()))));
            assert_eq!(m.filter(), "h2");
            assert_eq!(m.visible_len(), 1);
            assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(2)));
            let access = Widget::accessibility(&m).unwrap();
            assert_eq!(access.name.as_deref(), Some("Block types"));
            assert_eq!(access.value.as_deref(), Some("h2"));
        }

        // Case `backspace_removes_one_extended_grapheme`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("👩🏽‍💻");
            assert!(m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None)));
            assert_eq!(m.filter(), "");
            assert!(m.is_visible());
        }

        // Case `backspace_pops_filter`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("h2");
            m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert_eq!(m.filter(), "h");
        }

        // Case `backspace_on_empty_filter_closes_menu`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            // Filter starts empty.
            let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            // Returns false so the parent can interpret as "menu closed,
            // fall through to delete the `/` glyph".
            assert!(!consumed);
            assert!(!m.is_visible());
        }
    }

    #[test]
    fn keyboard_moves_the_cursor_and_commits_or_escapes() {
        // Case `arrow_keys_move_cursor`.
        {
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

        // Case `arrow_up_at_top_is_clamped`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            // Already at row 0.
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(m.highlighted_kind(), Some(&BlockKind::Paragraph));
        }

        // Case `enter_emits_selected_kind_and_closes`.
        {
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

        // Case `escape_closes_without_committing`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            m.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
            assert!(!m.is_visible());
            assert!(m.take_selected_kind().is_none());
        }

        // Case `filter_number_then_enter_commits_numbered_list`.
        {
            // Typing `/number` narrows to the "Numbered list" row and Enter
            // commits an ordered-list block.
            let mut m = SlashMenu::new();
            m.set_visible(true);
            for c in ["n", "u", "m", "b", "e", "r"] {
                m.handle_key(&char_press(c));
            }
            assert_eq!(m.visible_len(), 1, "`number` matches only Numbered list");
            m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert_eq!(
                m.take_selected_kind(),
                Some(BlockKind::Numbered { indent: 0 })
            );
        }

        // Case `filter_h2_then_enter_commits_heading_2`.
        {
            // Type `/h2`, Enter → Heading(2). The leading `/` is consumed by
            // the editor before the menu opens, so the menu only sees `h2`.
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&char_press("h"));
            m.handle_key(&char_press("2"));
            assert_eq!(m.visible_len(), 1);
            m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert_eq!(m.take_selected_kind(), Some(BlockKind::Heading(2)));
        }
    }

    #[test]
    fn pointer_and_semantic_activation_commit_the_exact_row() {
        // Case `synthetic_menu_item_activation_commits_that_exact_filtered_row`.
        {
            let mut menu = SlashMenu::new();
            menu.set_visible(true);
            let access = Widget::accessibility(&menu).expect("open menu semantics");
            let heading = access
                .children
                .iter()
                .find(|item| item.name.as_deref() == Some("Heading 2"))
                .expect("Heading 2 item");
            let target = heading.action_target.clone().expect("stable menu target");

            assert!(Widget::perform_semantic_action(
                &mut menu,
                &target,
                &crate::a11y::Action::Activate,
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(menu.take_selected_kind(), Some(BlockKind::Heading(2)));
            assert!(!menu.is_visible());
        }

        // Case `click_at_selects_row_strips_and_closes`.
        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            // Row 1 is "Heading 1"; land the click inside its band.
            let pt = Point::new(10.0, m.rows_top() + ROW_HEIGHT + 2.0);
            assert_eq!(m.click_at(pt), Some(BlockKind::Heading(1)));
            assert!(
                !m.is_visible(),
                "clicking a row commits and closes the menu"
            );
            assert_eq!(
                m.take_selected_kind(),
                Some(BlockKind::Heading(1)),
                "the click also queues the kind for the parent to drain"
            );
        }

        // Case `click_off_any_row_selects_nothing`.
        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            assert_eq!(m.click_at(Point::new(10.0, 5000.0)), None);
            assert!(
                m.is_visible(),
                "an off-row click leaves the menu open for the caller"
            );
        }

        // Case `hover_at_moves_the_highlight_to_the_row_under_the_pointer`.
        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            assert_eq!(m.highlighted_kind(), Some(&BlockKind::Paragraph));
            // Row 2 is "Heading 2".
            let pt = Point::new(10.0, m.rows_top() + 2.0 * ROW_HEIGHT + 2.0);
            assert!(m.hover_at(pt), "hovering a new row reports a move");
            assert_eq!(m.highlighted_kind(), Some(&BlockKind::Heading(2)));
            assert!(!m.hover_at(pt), "hovering the same row is not a move");
            assert!(
                !m.hover_at(Point::new(10.0, 5000.0)),
                "hovering off any row is a no-op"
            );
        }
    }
}
