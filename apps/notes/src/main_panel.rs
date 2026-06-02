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

/// Breadcrumb-style title size. v0.1 used a 32-pt "page title" up top
/// which then collided with the body's H1 block (most pages open with
/// a `# Title` heading, so the H1 reads as the page heading and the
/// panel title is redundant). The new title is a small dim breadcrumb
/// — always shown so the user can still tell which page they're on,
/// but visually subordinate to the body so the H1 in the body reads
/// as THE heading.
const TITLE_SIZE: f32 = 13.0;
/// Inner padding around the panel content.
const PADDING_PX: f64 = 32.0;
/// Empty-state hint size.
const HINT_SIZE: f32 = 14.0;
/// Vertical gap between the breadcrumb title and the block editor.
/// Shorter than the previous 24 px since the title is now a small
/// chip rather than a heading row.
const TITLE_TO_EDITOR_GAP: f64 = 12.0;
/// Breadcrumb title row height (~ TITLE_SIZE * 1.5).
const TITLE_HEIGHT: f64 = 20.0;

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
    /// Vertical scroll offset for the panel body (in CSS-like px).
    /// Bumped by mouse-wheel events; clamped each frame to the
    /// content overflow extent in `apply_layout`. We hand-roll this
    /// rather than wrapping in `ui::widget::Scrollable` because the
    /// panel owns its sub-tree (title + editor) as struct fields, not
    /// as boxed Elements — wrapping in Scrollable would require
    /// significant ownership churn for what amounts to "clip + shift".
    scroll_y: f64,
    /// True after a click on the panel that wasn't routed to a block
    /// — used to grant scroll focus so wheel events target the panel
    /// even when the editor itself isn't focused. Stored so a future
    /// trackpad/keyboard-scroll path has somewhere to read it. Always
    /// safe to be true: the panel is the only scrollable region in
    /// the notes app.
    #[allow(dead_code)]
    has_scroll_focus: bool,
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
            scroll_y: 0.0,
            has_scroll_focus: false,
        }
    }

    /// Load a page into the panel. The body is parsed and handed to
    /// a fresh `BlockEditor`. The previous editor's state (focus,
    /// caret) is discarded — switching pages is a hard cut.
    ///
    /// Phase 4: the parse path goes through `notes::markdown` so the
    /// inline-mark grammar is the canonical one (the widget's own
    /// `BlockEditor::from_markdown` doesn't see inline marks).
    pub fn load_page(&mut self, id: PageId, title: String, body: String) {
        self.page_id = Some(id);
        self.title = Some(title);
        let specs = crate::markdown::parse(&body);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = BlockEditor::new();
        self.editor.set_blocks(blocks);
        // Focus the editor's first block so the user can start
        // typing immediately. The App will route keystrokes through
        // its focused-node machinery, but the editor's internal
        // focused_block needs to be primed.
        self.editor.set_focus(true);
        self.body = body;
        // Brand-new page → reset scroll to top (the previous page's
        // scroll position is meaningless for the new content).
        self.scroll_y = 0.0;
        self.dirty = true;
    }

    /// Load a page with caller-pre-resolved link page-ids. Used by
    /// `NotesApp` so a `[[Page Title]]` in the markdown resolves to
    /// the actual `PageId` at load time (otherwise links would be
    /// `Mark::Link(Uuid::nil())` placeholders).
    pub fn load_page_with_link_resolver<F>(
        &mut self,
        id: PageId,
        title: String,
        body: String,
        resolver: F,
    )
    where
        F: FnMut(&str) -> Option<PageId>,
    {
        self.page_id = Some(id);
        self.title = Some(title);
        let mut specs = crate::markdown::parse(&body);
        crate::markdown::resolve_link_ids(&mut specs, resolver);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = BlockEditor::new();
        self.editor.set_blocks(blocks);
        self.editor.set_focus(true);
        self.body = body;
        self.scroll_y = 0.0;
        self.dirty = true;
    }

    /// Clear the panel back to the empty state.
    pub fn clear(&mut self) {
        self.page_id = None;
        self.title = None;
        self.editor = BlockEditor::new();
        self.body = String::new();
        self.scroll_y = 0.0;
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
    ///
    /// Phase 4: serialisation goes through `notes::markdown` so
    /// inline marks survive the round-trip. The editor's own
    /// `to_markdown` only emits plain-text bodies.
    pub fn take_body_snapshot(&mut self) -> String {
        let specs = crate::markdown::blocks_to_specs(self.editor.blocks());
        let md = crate::markdown::serialize(&specs);
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
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The panel is a **leaf** in the parent's flex tree — we do
        // all internal positioning by hand in `apply_layout`. Using a
        // flex container with the editor as a child caused taffy's
        // `content_size` for long pages (many blocks → big intrinsic
        // height) to grow the panel past the parent's allocation,
        // pushing the status bar off-screen. A leaf with parent-driven
        // sizing sidesteps that — the editor is laid out manually
        // inside the panel rect with overflow clipped via push_layer
        // in `draw` and `scroll_y` shifting block rects.
        //
        // We still register the editor's tree so its descendants get
        // taffy-measured (RichTextLine reports intrinsic height per
        // block). The editor's parent in the taffy tree IS the panel
        // node, but we hide that by giving the editor `position:
        // Absolute` — so taffy doesn't push the panel's size from the
        // editor's content extent.
        let editor_node = self.editor.build_layout(taffy, _theme, node_map);
        if let Ok(existing) = taffy.style(editor_node).cloned() {
            let _ = taffy.set_style(
                editor_node,
                Style {
                    position: taffy::Position::Absolute,
                    size: Size {
                        // Width = panel width minus padding. Pulled from
                        // the parent's resolved size at `apply_layout`
                        // time; here we use auto so taffy doesn't
                        // pre-clamp before we know the panel width.
                        width: Dimension::auto(),
                        height: Dimension::auto(),
                    },
                    inset: taffy::Rect {
                        left: length(PADDING_PX as f32),
                        right: length(PADDING_PX as f32),
                        top: length(
                            (PADDING_PX + TITLE_HEIGHT + TITLE_TO_EDITOR_GAP) as f32,
                        ),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    },
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);
        // Title is drawn directly in `draw`; no taffy node needed.
        self.title_node = None;

        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::auto(),
                    },
                    overflow: taffy::Point {
                        x: taffy::Overflow::Hidden,
                        y: taffy::Overflow::Hidden,
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    ..Default::default()
                },
                &[editor_node],
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
        // Lay the editor out at the unshifted origin first so we can
        // compute its natural content extent, then shift by -scroll_y
        // for the second pass. Cheaper than measuring twice through
        // taffy — we only re-walk the apply_layout origin.
        if let Some(n) = self.editor_node {
            let unshifted_origin = Point::new(self.rect.x0, self.rect.y0);
            self.editor.apply_layout(taffy, n, unshifted_origin);
            let editor_rect = self.editor.get_rect();
            let last_block_bottom = self
                .editor
                .blocks()
                .iter()
                .map(|b| b.line.get_rect().y1)
                .fold(editor_rect.y0, f64::max);
            // Content height = where the lowest block actually ends,
            // minus the editor's top. This catches taffy under-reports
            // for flex-column overflow (same kludge ui::Scrollable uses
            // in its apply_layout).
            let content_h = (last_block_bottom - editor_rect.y0).max(0.0);
            let viewport_h = (self.rect.height() - 2.0 * PADDING_PX
                - TITLE_HEIGHT
                - TITLE_TO_EDITOR_GAP)
                .max(0.0);
            let max_scroll = (content_h - viewport_h).max(0.0);
            let clamped = self.scroll_y.clamp(0.0, max_scroll);
            if clamped != self.scroll_y {
                self.scroll_y = clamped;
                self.dirty = true;
            }
            // Re-apply layout with the (possibly clamped) scroll
            // offset so block rects reflect their on-screen position.
            // Without this, click hit-tests would target stale rects
            // for a scrolled view.
            let shifted_origin =
                Point::new(self.rect.x0, self.rect.y0 - self.scroll_y);
            self.editor.apply_layout(taffy, n, shifted_origin);
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
                // Breadcrumb-style title — dim, small, always visible.
                // The body's H1 (if present) is the real heading; this
                // chip is just "you are here". Draw OUTSIDE the scroll
                // clip so the breadcrumb stays put while body scrolls.
                text_renderer.draw_text(
                    scene,
                    t,
                    TITLE_SIZE,
                    tokens.colors.text_dim,
                    transform * Affine::translate((inner_x, inner_y)),
                    Some(inner_w),
                );

                // Clip the editor body to the panel's scrollable
                // viewport so blocks past the bottom don't bleed onto
                // the status bar.
                let body_clip = Rect::new(
                    self.rect.x0,
                    self.rect.y0 + PADDING_PX + TITLE_HEIGHT + TITLE_TO_EDITOR_GAP,
                    self.rect.x1,
                    self.rect.y1 - PADDING_PX,
                );
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &body_clip,
                );
                self.editor
                    .draw(scene, transform, text_renderer, theme, presence);
                scene.pop_layer();
                // Overlay popovers (slash menu) — drawn AFTER the
                // main editor scene so they land on top of every
                // block. The BlockEditor exposes its overlay drawer
                // directly so the host doesn't have to know which
                // overlays the editor owns. Drawn OUTSIDE the scroll
                // clip — popovers must be visible at their anchor
                // even if the editor scrolls past them.
                self.editor.draw_overlay(scene, transform, text_renderer, theme);
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
        // Mouse wheel: scroll the panel's body. Matches ui::Scrollable's
        // wheel-to-px conversion (~120 px / notch). Consumes the event
        // so the underlying TextInput doesn't try to interpret a wheel
        // delta as a cursor move.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -(*y as f64) * 40.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            if dy != 0.0 {
                let prev = self.scroll_y;
                self.scroll_y = (self.scroll_y + dy).max(0.0);
                // Clamp against max_scroll happens in apply_layout
                // since that's where we know the content extent.
                if self.scroll_y != prev {
                    self.dirty = true;
                    *handled = true;
                }
            }
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
