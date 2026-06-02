//! `MainPanel` — the right-hand pane that renders the currently
//! selected page.
//!
//! Phase 1 showed only the page's title. Phase 2 replaces that with
//! a [`BlockEditor`] over the page body. The panel owns:
//!
//! - the currently displayed page's id (or `None` if nothing's
//!   selected),
//! - the live `BlockEditor` (re-built whenever the page changes),
//! - the last-loaded body string, kept around so the app's tick can
//!   compare against the editor's current `to_markdown()` and only
//!   request a save when the user actually changed something.
//!
//! The title heading stays — it's just a sibling of the editor now,
//! pinned to the top of the panel. Title editing is still the
//! sidebar's job (inline rename).
//!
//! Layout sketch:
//!
//! ```text
//! +-- panel rect ----------------------+
//! | [PADDING_PX gutter]               |
//! |   Title (32 pt)                   |
//! |   --------- gap -----------       |
//! |   BlockEditor                     |
//! |     Paragraph                     |
//! |     Heading 1                     |
//! |     ...                           |
//! +-----------------------------------+
//! ```

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::BlockEditor;

use crate::model::PageId;

/// Heading point size for the title row. Reads as a "page title"
/// rather than body copy.
const HEADING_SIZE: f32 = 32.0;
/// Inner padding around the panel content.
const PADDING_PX: f64 = 32.0;
/// Empty-state hint size.
const HINT_SIZE: f32 = 14.0;
/// Vertical gap between the title row and the block editor.
const TITLE_TO_EDITOR_GAP: f64 = 24.0;
/// Title row height in CSS-like px (~ HEADING_SIZE * 1.25 + a bit).
const TITLE_HEIGHT: f64 = 44.0;

pub struct MainPanel {
    /// Currently displayed page (None until a selection lands).
    page_id: Option<PageId>,
    /// Title rendered above the editor. Drives the empty-state hint
    /// too — `None` means "no page selected".
    title: Option<String>,
    /// Live editor. Always present (defaults to an empty document)
    /// so layout/draw don't need a `None` branch.
    editor: BlockEditor,
    /// Last markdown body the panel was loaded with. Used to detect
    /// whether the editor's current state still matches the loaded
    /// document — if so we skip the to_markdown serialisation.
    body: String,
    /// Panel rect, populated by `apply_layout`.
    rect: Rect,
    /// Cached taffy node id for the editor child, used by
    /// `apply_layout` + `handle_event`.
    editor_node: Option<NodeId>,
    title_node: Option<NodeId>,
    /// True until the next `mark_drawn`. Flipped on page swap, on
    /// title change, on rect resize, on editor mutations.
    dirty: bool,
}

impl MainPanel {
    pub fn new() -> Self {
        Self {
            page_id: None,
            title: None,
            editor: BlockEditor::new(),
            body: String::new(),
            rect: Rect::ZERO,
            editor_node: None,
            title_node: None,
            dirty: true,
        }
    }

    /// Load a page into the panel. The body is parsed and handed to
    /// a fresh `BlockEditor`. The previous editor's state (focus,
    /// caret) is discarded — switching pages is a hard cut.
    pub fn load_page(&mut self, id: PageId, title: String, body: String) {
        self.page_id = Some(id);
        self.title = Some(title);
        self.editor = BlockEditor::from_markdown(&body);
        // Focus the editor's first block so the user can start
        // typing immediately. The App will route keystrokes through
        // its focused-node machinery, but the editor's internal
        // focused_block needs to be primed.
        self.editor.set_focus(true);
        self.body = body;
        self.dirty = true;
    }

    /// Clear the panel back to the empty state.
    pub fn clear(&mut self) {
        self.page_id = None;
        self.title = None;
        self.editor = BlockEditor::new();
        self.body = String::new();
        self.dirty = true;
    }

    /// Update only the title (used by the sidebar's inline rename).
    pub fn set_title(&mut self, title: Option<String>) {
        if self.title != title {
            self.title = title;
            self.dirty = true;
        }
    }

    /// Currently displayed page id, if any.
    pub fn page_id(&self) -> Option<PageId> {
        self.page_id
    }

    /// True if the editor reported edits since the last call. Drains
    /// the editor's dirty flag.
    pub fn editor_is_dirty(&mut self) -> bool {
        self.editor.is_dirty()
    }

    /// Re-serialise the editor's content into the cached `body` and
    /// return a clone for persistence. Called by the app's tick when
    /// `editor_is_dirty()` came back true.
    pub fn take_body_snapshot(&mut self) -> String {
        let md = self.editor.to_markdown();
        self.body = md.clone();
        md
    }

    /// Currently displayed title, if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Read-only access to the live editor. Used by integration
    /// tests and any future read-only UI (status bar block count,
    /// etc.).
    pub fn editor(&self) -> &BlockEditor {
        &self.editor
    }

    /// Test/internal mutable accessor for the editor — used by the
    /// integration tests to drive split/merge/promote and observe
    /// the resulting markdown.
    pub fn editor_mut(&mut self) -> &mut BlockEditor {
        &mut self.editor
    }
}

impl Default for MainPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for MainPanel {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.editor.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The title is drawn directly in `draw`, but we still want a
        // taffy leaf reserving that space so the editor lands below it.
        let title_node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(TITLE_HEIGHT as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        self.title_node = Some(title_node);

        let editor_node = self.editor.build_layout(taffy, theme, node_map);
        // Preserve the editor's own internal flex-column layout
        // while overriding the parent-driven sizing fields. See the
        // matching pattern in `NotesApp::build_layout` for why
        // `..Default::default()` would be wrong here.
        if let Ok(existing) = taffy.style(editor_node).cloned() {
            let _ = taffy.set_style(
                editor_node,
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);

        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    align_items: Some(taffy::AlignItems::Stretch),
                    padding: taffy::Rect {
                        left: length(PADDING_PX as f32),
                        right: length(PADDING_PX as f32),
                        top: length(PADDING_PX as f32),
                        bottom: length(PADDING_PX as f32),
                    },
                    gap: taffy::Size {
                        width: length(0.0),
                        height: length(TITLE_TO_EDITOR_GAP as f32),
                    },
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    ..Default::default()
                },
                &[title_node, editor_node],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if new_rect.width() != self.rect.width() || new_rect.height() != self.rect.height() {
            self.dirty = true;
        }
        self.rect = new_rect;
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(n) = self.editor_node {
            self.editor.apply_layout(taffy, n, new_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;

        // Background.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        let inner_x = self.rect.x0 + PADDING_PX;
        let inner_y = self.rect.y0 + PADDING_PX;
        let inner_w = (self.rect.width() - 2.0 * PADDING_PX).max(0.0) as f32;

        match &self.title {
            Some(t) => {
                text_renderer.draw_text(
                    scene,
                    t,
                    HEADING_SIZE,
                    tokens.colors.text_primary,
                    transform * Affine::translate((inner_x, inner_y)),
                    Some(inner_w),
                );
                self.editor
                    .draw(scene, transform, text_renderer, theme, presence);
            }
            None => {
                text_renderer.draw_text(
                    scene,
                    "No page selected",
                    HINT_SIZE,
                    tokens.colors.text_dim,
                    transform * Affine::translate((inner_x, inner_y)),
                    Some(inner_w),
                );
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if self.page_id.is_none() {
            return;
        }
        if let Some(n) = self.editor_node {
            self.editor.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
        }
    }

    fn name(&self) -> &str {
        "MainPanel"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.editor.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.editor.mark_drawn();
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Forward the editor's structure-dirty flag (split/merge
        // shifted the block count) so the host App rebuilds the
        // taffy tree.
        self.editor.take_structure_dirty()
    }

    fn is_layout_dirty(&self) -> bool {
        // The editor's leaf nodes have intrinsic sizes (font_size *
        // 1.5) so size-affecting edits (heading promotion) need a
        // re-layout. Be conservative: always claim dirty when the
        // editor reports a redraw need.
        self.editor.needs_redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::BlockKind;
    use uuid::Uuid;

    fn fresh_page_id() -> PageId {
        Uuid::new_v4()
    }

    #[test]
    fn set_title_marks_dirty_when_changed() {
        let mut p = MainPanel::new();
        p.mark_drawn();
        assert!(!p.needs_redraw());
        p.set_title(Some("Hello".into()));
        assert!(p.needs_redraw());
        p.mark_drawn();
        p.set_title(Some("Hello".into()));
        assert!(!p.needs_redraw());
    }

    #[test]
    fn load_page_seeds_editor_from_markdown() {
        let mut p = MainPanel::new();
        let id = fresh_page_id();
        p.load_page(id, "Hello".into(), "# H\n\nbody\n".into());
        assert_eq!(p.page_id(), Some(id));
        assert_eq!(p.editor().blocks().len(), 2);
        assert!(matches!(p.editor().blocks()[0].kind, BlockKind::Heading(1)));
    }

    #[test]
    fn take_body_snapshot_round_trips() {
        let mut p = MainPanel::new();
        p.load_page(fresh_page_id(), "T".into(), "# H\n\nbody\n".into());
        let snap = p.take_body_snapshot();
        assert_eq!(snap, "# H\n\nbody\n");
    }

    #[test]
    fn clear_resets_state() {
        let mut p = MainPanel::new();
        p.load_page(fresh_page_id(), "T".into(), "# H\n".into());
        p.clear();
        assert!(p.page_id().is_none());
        assert!(p.title().is_none());
    }
}
