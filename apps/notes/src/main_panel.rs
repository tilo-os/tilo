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
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer};
use ui::theme::Theme;
use ui::widget::{BlockEditor, EditOutcome, EditableLabel, EmptyState};

use crate::model::PageId;
use crate::num;

/// Page-title size. The title is the note's most prominent element — a
/// large, editable heading pinned above the body, the way Notion/Bear put
/// the page name at the top of the page rather than burying it in the
/// first line of the body. Editing it renames the page (and re-syncs the
/// sidebar). The seeded page bodies no longer repeat the title as a body
/// H1, so there's a single clear heading per page.
const TITLE_SIZE: f32 = 30.0;
/// Inner padding around the panel content.
const PADDING_PX: f64 = 32.0;
/// Vertical gap between the title and the block editor.
const TITLE_TO_EDITOR_GAP: f64 = 14.0;
/// Title row height (~ `TITLE_SIZE` * 1.4) — enough for the editable field's
/// caret band without clipping descenders.
const TITLE_HEIGHT: f64 = 44.0;

pub struct MainPanel {
    /// Currently displayed page (None until a selection lands). Also the
    /// "is a page open" signal that gates the title vs. the empty state.
    page_id: Option<PageId>,
    /// The note's prominent, inline-editable title. A shared [`EditableLabel`]
    /// so the rename gets real selection / clipboard / caret for free; a
    /// committed edit is queued in `pending_rename` for the host to apply as a
    /// page rename (which re-syncs the sidebar).
    title_label: EditableLabel,
    /// Rect the title occupies (pinned above the scrolled body), used to
    /// hit-test a click into the title band since press events carry no
    /// position.
    title_rect: Rect,
    /// Latest pointer position (logical), tracked so a press can be classified
    /// as "in the title" without the event carrying coordinates.
    last_pointer: Point,
    /// Rising-edge: a committed title edit that should rename the page. The
    /// host drains it via [`take_title_rename`](Self::take_title_rename).
    pending_rename: Option<String>,
    /// Live editor. Always present (defaults to an empty document)
    /// so layout/draw don't need a `None` branch.
    editor: BlockEditor,
    /// Last markdown body the panel was loaded with. Used to detect
    /// whether the editor's current state still matches the loaded
    /// document — if so we skip the `to_markdown` serialisation.
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
    /// Local user's id for authorship attribution, re-applied to the
    /// editor whenever a page is (re)bound.
    local_author: Option<String>,
    /// The immediately-drawn first-note action. Keeping its rect here makes
    /// the otherwise empty document state a real, discoverable path into the
    /// editor rather than an instruction that asks people to find the sidebar.
    empty_action_rect: Rect,
    /// Rising edge set when the first-note action is pressed; the app drains
    /// it onto the same create action path as every other new-page affordance.
    empty_action_requested: bool,
    /// Focus belongs to the actionable empty state while there is no page to
    /// edit. It makes the CTA available to Tab, keyboard-only use and AT.
    empty_action_focused: bool,
}

impl MainPanel {
    pub fn new() -> Self {
        Self {
            page_id: None,
            title_label: EditableLabel::new("")
                .with_font_family(FontChoice::SansSerif)
                .with_font_size(TITLE_SIZE)
                .with_id("note_title"),
            title_rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            pending_rename: None,
            editor: BlockEditor::new(),
            body: String::new(),
            rect: Rect::ZERO,
            editor_node: None,
            title_node: None,
            dirty: true,
            scroll_y: 0.0,
            local_author: None,
            empty_action_rect: Rect::ZERO,
            empty_action_requested: false,
            empty_action_focused: false,
        }
    }

    /// Set the committed title value shown by the idle label, cancelling any
    /// in-flight rename first (a page swap should never carry an open editor
    /// or a stale pending rename into the next page).
    fn set_title_value(&mut self, title: &str) {
        self.title_label.cancel();
        let _ = self.title_label.take_outcome();
        self.title_label.set_value(title);
        self.pending_rename = None;
        self.dirty = true;
    }

    /// Rising-edge: a committed title edit that renames the current page. The
    /// host drains this after each event and applies it as a rename. Cleared
    /// on read.
    pub fn take_title_rename(&mut self) -> Option<String> {
        self.pending_rename.take()
    }

    /// Load a page into the panel. The body is parsed and handed to
    /// a fresh `BlockEditor`. The previous editor's state (focus,
    /// caret) is discarded — switching pages is a hard cut.
    ///
    /// Phase 4: the parse path goes through `notes::markdown` so the
    /// inline-mark grammar is the canonical one (the widget's own
    /// `BlockEditor::from_markdown` doesn't see inline marks).
    pub fn load_page(&mut self, id: PageId, title: &str, body: String) {
        self.page_id = Some(id);
        self.empty_action_focused = false;
        self.set_title_value(title);
        let specs = crate::markdown::parse(&body);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = BlockEditor::new();
        self.editor.set_blocks(blocks);
        // Focus the editor's first block so the user can start
        // typing immediately. The App will route keystrokes through
        // its focused-node machinery, but the editor's internal
        // focused_block needs to be primed.
        self.editor.set_focus(true);
        // Open a note at its beginning. A focused single-line block keeps its
        // caret in view; leaving it at the parser's end would horizontally
        // scroll the first long paragraph on a narrow window and hide its
        // opening words before the person has interacted with it.
        self.editor
            .focus_block(0, ui::widget::CursorPlacement::Start);
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
        title: &str,
        body: String,
        resolver: F,
    ) where
        F: FnMut(&str) -> Option<PageId>,
    {
        self.page_id = Some(id);
        self.set_title_value(title);
        let mut specs = crate::markdown::parse(&body);
        crate::markdown::resolve_link_ids(&mut specs, resolver);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = BlockEditor::new();
        self.editor.set_blocks(blocks);
        self.editor.set_focus(true);
        self.editor
            .focus_block(0, ui::widget::CursorPlacement::Start);
        self.body = body;
        self.scroll_y = 0.0;
        self.dirty = true;
    }

    /// Bind the panel's editor to a page's `blocks` List in the shared
    /// Automerge document. `body` is the page's markdown, used only to
    /// **seed** the doc when its block list is still empty (a brand-new
    /// page); if the doc already has blocks (an existing or remote page)
    /// the editor adopts them and `body` is ignored. After this, every
    /// edit syncs through the CRDT and remote edits appear via `sync`.
    pub fn bind_page(
        &mut self,
        state: &mut AutoCommit,
        id: PageId,
        title: &str,
        blocks_list: ObjId,
        body: &str,
    ) {
        self.page_id = Some(id);
        self.set_title_value(title);
        let specs = crate::markdown::parse(body);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = BlockEditor::new();
        self.editor.set_blocks(blocks);
        self.editor.set_local_author(self.local_author.clone());
        // Seeds the doc if empty, otherwise adopts the doc's blocks.
        self.editor.bind_blocks(state, blocks_list);
        self.editor.set_focus(true);
        self.editor
            .focus_block(0, ui::widget::CursorPlacement::Start);
        self.body = body.to_string();
        self.scroll_y = 0.0;
        self.dirty = true;
    }

    /// Set the local user's id for authorship attribution. Re-applied to
    /// the editor on every `bind_page`.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.local_author.clone_from(&author);
        self.editor.set_local_author(author);
    }

    /// Clear the panel back to the empty state.
    pub fn clear(&mut self) {
        self.page_id = None;
        self.set_title_value("");
        self.editor = BlockEditor::new();
        self.body = String::new();
        self.scroll_y = 0.0;
        self.empty_action_rect = Rect::ZERO;
        self.empty_action_requested = false;
        self.empty_action_focused = false;
        self.dirty = true;
    }

    /// Drain a press of the first-note action in the empty state.
    pub fn take_empty_action(&mut self) -> bool {
        std::mem::take(&mut self.empty_action_requested)
    }

    /// Update only the title (used by the sidebar's inline rename). Ignored
    /// while the title is itself being edited, so a model echo doesn't stomp
    /// the user's in-flight typing.
    pub fn set_title(&mut self, title: Option<String>) {
        if let Some(t) = title
            && !self.title_label.is_editing()
            && self.title_label.value() != t
        {
            self.title_label.set_value(&t);
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
        self.body.clone_from(&md);
        md
    }

    /// Currently displayed title, if any. `None` when no page is open.
    pub fn title(&self) -> Option<&str> {
        if self.page_id.is_some() {
            Some(self.title_label.value())
        } else {
            None
        }
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

    /// Test hook: the panel's current vertical scroll offset in px.
    /// Lets integration tests assert wheel scrolling + the
    /// `apply_layout` overflow clamp through the real event path.
    #[doc(hidden)]
    pub fn scroll_y_for_test(&self) -> f64 {
        self.scroll_y
    }

    /// Test hook: the laid-out title band, for click targeting through the
    /// real event path.
    #[doc(hidden)]
    pub fn title_rect_for_test(&self) -> Rect {
        self.title_rect
    }

    /// Test hook: whether the in-page title is currently in edit mode.
    #[doc(hidden)]
    pub fn title_is_editing_for_test(&self) -> bool {
        self.title_label.is_editing()
    }

    /// Test hook: centre of the first-note call to action after a draw pass.
    #[doc(hidden)]
    pub fn empty_action_center_for_test(&self) -> Option<Point> {
        (self.empty_action_rect.width() > 0.0).then(|| self.empty_action_rect.center())
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
        let editor_node = self.editor.build_layout(taffy, theme, node_map);
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
                        left: length(num::f32_of_f64(PADDING_PX)),
                        right: length(num::f32_of_f64(PADDING_PX)),
                        top: length(num::f32_of_f64(
                            PADDING_PX + TITLE_HEIGHT + TITLE_TO_EDITOR_GAP,
                        )),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    },
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);
        // Title is drawn directly in `draw`; no taffy node needed.
        self.title_node = None;

        // `new_with_children` only errors for a corrupted tree; `Widget::
        // build_layout` returns a bare `NodeId` across every implementor, so
        // surfacing a theoretical error here would mean threading a `Result`
        // through the whole toolkit for a branch that can't be taken (see
        // `ui::widget::TaffyTreeExt`).
        #[allow(
            clippy::unwrap_used,
            reason = "new_with_children on a freshly-built child cannot fail"
        )]
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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    // `layout` only errors for a `NodeId` from a different tree, which never
    // happens here since `node` is always the id this same widget received
    // from its own `build_layout`.
    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let new_rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if (new_rect.width() - self.rect.width()).abs() > f64::EPSILON
            || (new_rect.height() - self.rect.height()).abs() > f64::EPSILON
        {
            self.dirty = true;
        }
        self.rect = new_rect;
        // Title band pinned to the top of the panel, above the scrolled body.
        self.title_rect = Rect::new(
            self.rect.x0 + PADDING_PX,
            self.rect.y0 + PADDING_PX,
            (self.rect.x1 - PADDING_PX).max(self.rect.x0 + PADDING_PX),
            self.rect.y0 + PADDING_PX + TITLE_HEIGHT,
        );
        self.title_label.apply_layout_external(self.title_rect);
        // Lay the editor out at the unshifted origin first so we can
        // compute its natural content extent, then shift by -scroll_y
        // for the second pass. Cheaper than measuring twice through
        // taffy — we only re-walk the apply_layout origin.
        if let Some(n) = self.editor_node {
            let unshifted_origin = Point::new(self.rect.x0, self.rect.y0);
            self.editor.apply_layout(taffy, n, unshifted_origin);
            let editor_rect = self.editor.get_rect();
            let last_block_bottom = self.editor.content_bottom();
            // Content height = where the lowest block actually ends,
            // minus the editor's top. This catches taffy under-reports
            // for flex-column overflow (same kludge ui::Scrollable uses
            // in its apply_layout).
            let content_h = (last_block_bottom - editor_rect.y0).max(0.0);
            let viewport_h =
                (self.rect.height() - 2.0 * PADDING_PX - TITLE_HEIGHT - TITLE_TO_EDITOR_GAP)
                    .max(0.0);
            let max_scroll = (content_h - viewport_h).max(0.0);
            // A layout pass can land before the editor's text has been shaped.
            // Every block rect is then degenerate, `content_h` collapses, and
            // clamping against it throws the reader back to the top of the
            // document with nothing to explain it — the position is gone, not
            // restored on the next frame. Ask whether anything was measured at
            // all rather than whether the total came out at exactly zero: a
            // partial pass yields a small non-zero height that clamps just as
            // destructively. An empty document has no blocks, and 0 is right
            // for it.
            let measured = self.editor.blocks().is_empty()
                || self
                    .editor
                    .blocks()
                    .iter()
                    .any(|b| b.line.get_rect().height() > 0.0);
            if measured {
                let clamped = self.scroll_y.clamp(0.0, max_scroll);
                if (clamped - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = clamped;
                    self.dirty = true;
                }
            }
            // Re-apply layout with the (possibly clamped) scroll
            // offset so block rects reflect their on-screen position.
            // Without this, click hit-tests would target stale rects
            // for a scrolled view.
            let shifted_origin = Point::new(self.rect.x0, self.rect.y0 - self.scroll_y);
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

        if self.page_id.is_some() {
            // Prominent, editable page title, drawn OUTSIDE the scroll clip so
            // it stays pinned while the body scrolls under it.
            self.title_label.apply_layout_external(self.title_rect);
            self.title_label
                .draw(scene, transform, text_renderer, theme, presence);

            // Clip the editor body to the panel's scrollable viewport so
            // blocks past the bottom don't bleed onto the status bar.
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
            // Overlay popovers (slash menu / link popover) — drawn AFTER the
            // main editor scene so they land on top of every block, and
            // OUTSIDE the scroll clip so they stay visible at their anchor.
            self.editor
                .draw_overlay(scene, transform, text_renderer, theme);
        } else {
            // No note open: a composed zero-state rather than a bare line —
            // this is what a fresh, unseeded workspace shows, so it should
            // invite the first note rather than look broken.
            self.empty_action_rect = EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.rect,
                Some(Icon::FileText),
                "Start your first note",
                Some("Capture an idea, a plan, or a decision. You can organise it later."),
                Some("New note"),
                self.last_pointer,
            )
            .unwrap_or(Rect::ZERO);
            if self.empty_action_focused && self.empty_action_rect.width() > 0.0 {
                let focus = RoundedRect::from_rect(
                    self.empty_action_rect.inflate(3.0, 3.0),
                    f64::from(tokens.radii.md) + 2.0,
                );
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus,
                    None,
                    &focus,
                );
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event boundary keeps pointer, keyboard, empty-state and editor routing ordered around one handled flag"
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Track the pointer so a press (which carries no position) can be
        // classified as landing in the title band.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if self.page_id.is_none() {
            let pressed = matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            );
            let keyboard_activation = self.empty_action_focused
                && (matches!(
                    event,
                    WindowEvent::KeyboardInput {
                        event: winit::event::KeyEvent {
                            logical_key: Key::Named(NamedKey::Enter),
                            state: ElementState::Pressed,
                            ..
                        },
                        ..
                    }
                ) || matches!(
                    event,
                    WindowEvent::KeyboardInput {
                        event:
                            winit::event::KeyEvent {
                                logical_key: Key::Character(c),
                                state: ElementState::Pressed,
                                ..
                            },
                        ..
                    } if c.as_str() == " "
                ));
            if pressed && self.empty_action_rect.contains(self.last_pointer) {
                self.empty_action_requested = true;
                self.empty_action_focused = true;
                self.dirty = true;
                *focus_target = Some(node_id);
                *handled = true;
            } else if keyboard_activation {
                self.empty_action_requested = true;
                self.dirty = true;
                *handled = true;
            }
            return;
        }

        // Title editing. A left press in the title band opens an inline rename
        // (whole title selected, ready to replace or reposition); while
        // editing, the title owns typing / Enter / Esc, and a click elsewhere
        // blur-commits. A committed edit is queued for the host to apply as a
        // page rename.
        let press_in_title = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && self.title_rect.contains(self.last_pointer);
        if !self.title_label.is_editing() && press_in_title {
            self.title_label.begin_edit();
            self.dirty = true;
            *handled = true;
            return;
        }
        if self.title_label.is_editing() {
            self.title_label.apply_layout_external(self.title_rect);
            self.title_label.handle_event(
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
            match self.title_label.take_outcome() {
                EditOutcome::Committed(new) => {
                    self.pending_rename = Some(new);
                    self.dirty = true;
                }
                EditOutcome::Cancelled => self.dirty = true,
                EditOutcome::None => {}
            }
            // Typing / Enter / Esc are consumed by the field; stop so they
            // never reach the body. A blur-commit press outside the title is
            // deliberately NOT consumed, so it falls through to place the
            // caret in the body below.
            if *handled {
                return;
            }
        }

        // Mouse wheel: scroll the panel's body. Matches ui::Scrollable's
        // wheel-to-px conversion (~120 px / notch). Consumes the event
        // so the underlying TextInput doesn't try to interpret a wheel
        // delta as a cursor move.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 40.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            if dy.abs() > f64::EPSILON {
                let prev = self.scroll_y;
                self.scroll_y = (self.scroll_y + dy).max(0.0);
                // Clamp against max_scroll happens in apply_layout
                // since that's where we know the content extent.
                if (self.scroll_y - prev).abs() > f64::EPSILON {
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

    fn name(&self) -> &'static str {
        "MainPanel"
    }

    fn test_id(&self) -> Option<&str> {
        self.page_id.is_none().then_some("notes.first_note")
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.page_id.is_none() && self.empty_action_rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        self.page_id.is_none()
    }

    fn set_focus(&mut self, focused: bool) {
        let next = focused && self.page_id.is_none();
        if self.empty_action_focused != next {
            self.empty_action_focused = next;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.page_id.is_none() && self.empty_action_focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.page_id.is_none().then(|| {
            AccessNode::new(Role::Button)
                .with_name("New note")
                .with_description("Create the first note in this workspace")
                .with_state(AccessState {
                    focusable: true,
                    focused: self.empty_action_focused,
                    ..Default::default()
                })
        })
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if self.page_id.is_none() && matches!(action, ui::a11y::Action::Activate) {
            self.empty_action_requested = true;
            self.empty_action_focused = true;
            self.dirty = true;
            true
        } else {
            false
        }
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
    fn empty_state_primary_action_is_a_focused_accessible_button() {
        let mut p = MainPanel::new();
        p.set_focus(true);
        let node = p.accessibility().expect("empty state is exposed");
        assert_eq!(node.role, ui::a11y::Role::Button);
        assert_eq!(node.name.as_deref(), Some("New note"));
        assert!(node.state.focusable);
        assert!(node.state.focused);

        let mut doc = AutoCommit::new();
        assert!(p.perform_action(&ui::a11y::Action::Activate, &mut doc, &ObjId::Root));
        assert!(p.take_empty_action());
    }

    #[test]
    fn load_page_seeds_editor_from_markdown() {
        let mut p = MainPanel::new();
        let id = fresh_page_id();
        p.load_page(id, "Hello", "# H\n\nbody\n".into());
        assert_eq!(p.page_id(), Some(id));
        assert_eq!(p.editor().blocks().len(), 2);
        assert!(matches!(p.editor().blocks()[0].kind, BlockKind::Heading(1)));
    }

    #[test]
    fn take_body_snapshot_round_trips() {
        let mut p = MainPanel::new();
        p.load_page(fresh_page_id(), "T", "# H\n\nbody\n".into());
        let snap = p.take_body_snapshot();
        assert_eq!(snap, "# H\n\nbody\n");
    }

    #[test]
    fn clear_resets_state() {
        let mut p = MainPanel::new();
        p.load_page(fresh_page_id(), "T", "# H\n".into());
        p.clear();
        assert!(p.page_id().is_none());
        assert!(p.title().is_none());
    }
}
