//! `MainPanel` — the right-hand pane that renders the currently
//! selected page.
//!
//! The panel owns:
//!
//! - the currently displayed page's id (or `None` if nothing's
//!   selected),
//! - the page's editor: the SDK rich-text editor bound to the page's body,
//!   every edit a [`NotesAction::EditBody`](crate::actions::NotesAction),
//! - the last-loaded body string, kept around so a Markdown-backed page can
//!   compare against the editor's current content and only request a save
//!   when the user actually changed something.
//!
//! The title heading stays — it's just a sibling of the editor now,
//! pinned to the top of the panel. Title editing is still the
//! sidebar's job (inline rename).
//!
//! Layout sketch, every gap a theme token:
//!
//! ```text
//! +-- panel rect ----------------------+
//! |  Title (LargeTitle, pinned)        |
//! |  [tag] [tag]                       |
//! |  In Projects                       |
//! |  Created 2 Mar 2026, edited today  |
//! |  Linked from                       |
//! |  [note] [note]                     |
//! |                                    |
//! |  BlockEditor                       |
//! |    Paragraph                       |
//! |    ...                             |
//! +------------------------------------+
//! ```

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::AutoCommit;
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::{RichTextField, RichTextSurface};
use ui::Widget;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    BlockEditor, ChipFace, ChipLead, EditOutcome, EditableLabel, EmptyState, EmptyStateContent,
};
use ui::widget::{PressOutcome, ScrollMetrics, Scrollbar};

use crate::model::PageId;
use crate::num;

/// The note header's geometry in the theme the tree was built with. The note
/// title is the page's `LargeTitle`: an editable heading pinned above the
/// body, so there is a single clear heading per page. Every measure is a
/// theme token or a measured line, never a pixel size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    /// The content gutter: the one left edge of title, metadata and body.
    gutter: f64,
    /// Where the body editor starts inside the panel: the gutter less the
    /// padding its text lines add inside their own rect, so a paragraph's
    /// first letter stands on the gutter with the title above it.
    body_inset: f64,
    /// Chips beside each other, and a line under the line it belongs to.
    related: f64,
    /// The gap between two groups of the header.
    sibling: f64,
    /// The gap between the header and the first block of the body.
    body_gap: f64,
    /// The title band: its line plus room for the focus ring.
    title: f64,
    /// One `Caption` line.
    caption: f64,
    /// One `Subheading` line.
    group_title: f64,
    /// One chip.
    chip: f64,
    /// The scroll bar's track width and its inset from the panel edge.
    scroll_width: f64,
    scroll_inset: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let line = |role: TextRole| f64::from(theme.text(role).line_height_px());
        Self {
            gutter: f64::from(spacing.rhythm.group),
            body_inset: (f64::from(spacing.rhythm.group) - f64::from(theme.padding_standard))
                .max(0.0),
            related: f64::from(spacing.rhythm.related),
            sibling: f64::from(spacing.rhythm.sibling),
            body_gap: f64::from(spacing.s4),
            title: line(TextRole::LargeTitle) + 2.0 * f64::from(spacing.s2),
            caption: line(TextRole::Caption),
            group_title: line(TextRole::Subheading),
            chip: ChipFace::height(theme),
            scroll_width: f64::from(spacing.s2),
            scroll_inset: f64::from(spacing.s1),
        }
    }
}

/// Presentation data for a durable tag. Keeping its id beside the label means
/// a painted chip can invoke the same stable tag operation as the resource
/// palette; it never relies on a display name that can change remotely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagChip {
    pub id: crate::model::TagId,
    pub name: String,
}

/// A real incoming page-link, projected from canonical native link marks.
/// It has no document-owned presentation state: opening it only changes this
/// person's selected note through the existing `OpenPage` operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BacklinkChip {
    pub page: PageId,
    pub title: String,
    pub references: u32,
}

/// What an incoming-link chip says: the source note, and how many times it
/// links here when more than once.
fn backlink_label(backlink: &BacklinkChip) -> String {
    if backlink.references > 1 {
        format!("{} ({})", backlink.title, backlink.references)
    } else {
        backlink.title.clone()
    }
}

/// A page's body in the SDK rich-text editor, edited as Notes actions.
pub type NotesEditor = RichTextSurface<RichTextField<crate::actions::NotesAction>>;

/// Notes is a prose editor, so ordinary blocks must reflow to the available
/// column width. Keeping construction behind one helper prevents a page bind,
/// remote rebind, or empty-state reset from silently returning to the shared
/// editor's single-line (code/editor) default. `page` is the page whose body
/// it edits; without one it shows what it is given and writes nothing.
fn notes_editor(page: Option<PageId>) -> NotesEditor {
    let context = page.map(|page| {
        crate::actions::history_context(&crate::actions::edit_body_action(
            page,
            tilo_document::RichTextAction::AppendParagraph {
                text: String::new(),
            },
        ))
    });
    let mut field = RichTextField::new(
        crate::actions::NOTES_ACTION_TYPE,
        move |doc| page.and_then(|page| crate::doc::NotesDoc::page_body(doc, page)),
        move |edit| crate::actions::edit_body_action(page.unwrap_or_default(), edit),
    );
    if let Some(context) = context {
        field = field.with_context(context);
    }
    RichTextSurface::field(field).with_editor(
        BlockEditor::new()
            .with_wrap_lines(true)
            .with_compact_paragraph_spacing(true),
    )
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the local focus, hover, empty-state and layout flags independently track retained widget lifecycles; grouping them would only hide rather than simplify that state"
)]
pub struct MainPanel {
    /// Currently displayed page (None until a selection lands). Also the
    /// "is a page open" signal that gates the title vs. the empty state.
    page_id: Option<PageId>,
    date_index: crate::metadata::NoteDateIndex,
    dates: crate::metadata::NoteDates,
    /// Parent hierarchy for the open note. The editable leaf title is not
    /// repeated here.
    parent_path: String,
    tooltips_suppressed: bool,
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
    /// Durable tags assigned to the open page, rendered as quiet chips. The
    /// selection/filter around them belongs to the app and is intentionally
    /// local; this view only receives the document-derived labels.
    tags: Vec<TagChip>,
    /// Exact hit geometry for the wrapped tag chips. This is computed during
    /// layout, shared by pointer + semantic activation, and deliberately
    /// avoids making the title's entire metadata band accidentally editable.
    tag_rects: Vec<Rect>,
    /// The note's parent path and dates as `Caption` lines, in document
    /// order, with the rect each is painted and read at.
    caption_rows: Vec<(Rect, String)>,
    /// The "Linked from" group title.
    backlink_label: Rect,
    /// The height the whole header (tags, captions, incoming links and the
    /// gap to the body) takes in the scrolling document. It is recalculated
    /// whenever width, text scale, or membership changes, so large text and
    /// long tag names never collide with the first document block.
    metadata_height: f64,
    /// A tag chip is a local navigation control. The app drains this retained
    /// id into `NotesOperation::OpenTag`, which validates the real canonical
    /// tag resource before changing the local browse view.
    pending_tag_open: Option<crate::model::TagId>,
    focused_tag: Option<crate::model::TagId>,
    hovered_tag: Option<crate::model::TagId>,
    /// Set while a new chip height is being discovered so the host rebuilds
    /// the absolute editor inset on the immediately following layout pass.
    tag_layout_dirty: bool,
    /// Incoming links are an indexed projection of durable page-link marks.
    /// Keeping them beside tags makes relationships discoverable without
    /// pretending that backlinks are separately authored metadata.
    backlinks: Vec<BacklinkChip>,
    backlink_rects: Vec<Rect>,
    pending_backlink_open: Option<PageId>,
    focused_backlink: Option<PageId>,
    hovered_backlink: Option<PageId>,
    /// Live editor. Always present (defaults to an empty document)
    /// so layout/draw don't need a `None` branch.
    editor: NotesEditor,
    image_store: Option<ui::ImageStore>,
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
    scroll_content_height: f64,
    scroll_track: Rect,
    scrollbar: Scrollbar,
    scroll_focused: bool,
    scroll_layout_dirty: bool,
    /// Follow an explicit reveal or keyboard edit once layout has fresh caret geometry.
    follow_caret: bool,
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
    /// The header geometry and the theme it was resolved in, from the last
    /// `build_layout`: `apply_layout` measures chips with them and is handed
    /// no theme of its own.
    metrics: Metrics,
    layout_theme: Theme,
    /// How far the editor's paragraphs inset their text inside their own
    /// rect, measured from the first one laid out. The body starts that much
    /// short of the gutter, so the first letter stands on it.
    line_padding: Option<f64>,
}

impl MainPanel {
    pub fn new() -> Self {
        crate::metadata::register_messages();
        Self {
            page_id: None,
            date_index: crate::metadata::NoteDateIndex::default(),
            dates: crate::metadata::NoteDates::default(),
            parent_path: String::new(),
            tooltips_suppressed: false,
            title_label: EditableLabel::new("")
                .with_text_role(TextRole::LargeTitle)
                .with_id("note_title")
                .with_accessible_name("Page title"),
            title_rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            pending_rename: None,
            tags: Vec::new(),
            tag_rects: Vec::new(),
            caption_rows: Vec::new(),
            backlink_label: Rect::ZERO,
            metadata_height: 0.0,
            pending_tag_open: None,
            focused_tag: None,
            hovered_tag: None,
            tag_layout_dirty: false,
            backlinks: Vec::new(),
            backlink_rects: Vec::new(),
            pending_backlink_open: None,
            focused_backlink: None,
            hovered_backlink: None,
            editor: notes_editor(None),
            image_store: None,
            body: String::new(),
            rect: Rect::ZERO,
            editor_node: None,
            title_node: None,
            dirty: true,
            scroll_y: 0.0,
            scroll_content_height: 0.0,
            scroll_track: Rect::ZERO,
            scrollbar: Scrollbar::vertical(),
            scroll_focused: false,
            scroll_layout_dirty: false,
            follow_caret: false,
            local_author: None,
            empty_action_rect: Rect::ZERO,
            empty_action_requested: false,
            empty_action_focused: false,
            metrics: Metrics::default(),
            layout_theme: Theme::dark(),
            line_padding: None,
        }
    }

    /// Set the committed title value shown by the idle label, cancelling any
    /// in-flight rename first (a page swap should never carry an open editor
    /// or a stale pending rename into the next page).
    fn set_title_value(&mut self, title: &str) {
        self.scrollbar.release();
        self.scrollbar.set_hover(false);
        self.scroll_focused = false;
        self.title_label.cancel();
        let _ = self.title_label.take_outcome();
        self.title_label.set_value(title);
        self.title_label
            .set_semantic_target(self.page_id.map(|page| format!("notes:page-title:{page}")));
        self.pending_rename = None;
        self.dirty = true;
    }

    /// Rising-edge: a committed title edit that renames the current page. The
    /// host drains this after each event and applies it as a rename. Cleared
    /// on read.
    pub fn take_title_rename(&mut self) -> Option<String> {
        if let EditOutcome::Committed(value) = self.title_label.take_outcome() {
            self.pending_rename = Some(value);
        }
        self.pending_rename.take()
    }

    /// Load a page into the panel. The body is parsed and handed to
    /// a fresh `BlockEditor`. The previous editor's state (focus,
    /// caret) is discarded — switching pages is a hard cut.
    /// Parsing goes through `notes::markdown`, which owns the inline-mark
    /// grammar that `BlockEditor::from_markdown` does not handle.
    pub fn load_page(&mut self, id: PageId, title: &str, body: impl AsRef<str>) {
        let body = body.as_ref();
        self.page_id = Some(id);
        self.dates = self.date_index.get(id);
        self.empty_action_focused = false;
        self.set_title_value(title);
        let specs = crate::markdown::parse(body);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = self.fresh_editor(None);
        self.editor.editor_mut().set_blocks(blocks);
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
            .editor_mut()
            .focus_block(0, ui::widget::CursorPlacement::Start);
        self.take_body_snapshot();
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
        body: impl AsRef<str>,
        resolver: F,
    ) where
        F: FnMut(&str) -> Option<PageId>,
    {
        let body = body.as_ref();
        self.page_id = Some(id);
        self.dates = self.date_index.get(id);
        self.set_title_value(title);
        let mut specs = crate::markdown::parse(body);
        crate::markdown::resolve_link_ids(&mut specs, resolver);
        let blocks = crate::markdown::specs_to_blocks(&specs);
        self.editor = self.fresh_editor(None);
        self.editor.editor_mut().set_blocks(blocks);
        self.editor.set_focus(true);
        self.editor
            .editor_mut()
            .focus_block(0, ui::widget::CursorPlacement::Start);
        self.take_body_snapshot();
        self.scroll_y = 0.0;
        self.dirty = true;
    }

    /// Bind the panel's editor to a page's body in the shared document,
    /// showing it as it is now. The editor edits it through its action host
    /// from then on; binding itself writes nothing.
    ///
    /// # Errors
    /// Rejects a page without a body.
    pub fn bind_canonical_page(
        &mut self,
        doc: &AutoCommit,
        page: PageId,
        title: &str,
    ) -> Result<(), String> {
        if crate::doc::NotesDoc::page_body(doc, page).is_none() {
            return Err("the page has no body".into());
        }
        let mut editor = self.fresh_editor(Some(page));
        editor
            .editor_mut()
            .set_local_author(self.local_author.clone());
        editor.present_from(doc);
        editor.set_focus(true);
        editor
            .editor_mut()
            .focus_block(0, ui::widget::CursorPlacement::Start);
        self.editor = editor;
        self.page_id = Some(page);
        self.dates = self.date_index.get(page);
        self.empty_action_focused = false;
        self.set_title_value(title);
        self.body.clear();
        self.scroll_y = 0.0;
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn project_canonical(
        &self,
        doc: &AutoCommit,
        workspace: &crate::Workspace,
    ) -> Result<Self, String> {
        let mut panel = Self::new();
        panel.set_image_store(self.image_store.clone());
        panel.set_local_author(self.local_author.clone());
        if let Some(page) = workspace.selected() {
            let title = &workspace.get(page).ok_or("selected page missing")?.title;
            panel.bind_canonical_page(doc, page, title)?;
        }
        Ok(panel)
    }

    pub(crate) fn local_scroll(&self) -> f64 {
        self.scroll_y
    }
    pub(crate) fn restore_scroll(&mut self, scroll: f64) {
        self.scroll_y = scroll;
    }
    pub(crate) fn title_focused(&self) -> bool {
        self.title_label.is_editing()
    }
    pub(crate) fn finish_local_focus(&mut self, focus: crate::local_view::NotesFocus) {
        self.title_label.cancel();
        let _ = self.title_label.take_outcome();
        self.pending_rename = None;
        self.empty_action_focused = false;
        self.editor.set_focus(false);
        if self.page_id.is_some() {
            match focus {
                crate::local_view::NotesFocus::Title => self.title_label.begin_edit(),
                crate::local_view::NotesFocus::Editor => self.editor.set_focus(true),
                _ => {}
            }
        }
    }

    /// Set the local user's id for authorship attribution. Re-applied to
    /// the editor on every `bind_page`.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.local_author.clone_from(&author);
        self.editor.editor_mut().set_local_author(author);
    }

    /// Route the window-level Find command into the shared structured editor,
    /// even when the note title or another sibling currently owns focus.
    pub fn open_find(&mut self, focus_target: &mut Option<NodeId>) -> bool {
        if self.page_id.is_none() {
            return false;
        }
        if self.title_label.is_editing() {
            self.title_label.commit();
            if let EditOutcome::Committed(title) = self.title_label.take_outcome() {
                self.pending_rename = Some(title);
            }
        }
        self.editor.editor_mut().open_find();
        *focus_target = self.editor_node;
        self.dirty = true;
        true
    }

    /// Clear the panel back to the empty state.
    pub fn clear(&mut self) {
        self.page_id = None;
        self.parent_path.clear();
        self.set_title_value("");
        self.tags.clear();
        self.tag_rects.clear();
        self.caption_rows.clear();
        self.backlinks.clear();
        self.backlink_rects.clear();
        self.backlink_label = Rect::ZERO;
        self.metadata_height = 0.0;
        self.pending_tag_open = None;
        self.focused_tag = None;
        self.hovered_tag = None;
        self.pending_backlink_open = None;
        self.focused_backlink = None;
        self.hovered_backlink = None;
        self.editor = notes_editor(None);
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

    /// Replace the visible metadata-chip labels. Sorting/deduplication is done
    /// by the model; this cheap presentation diff avoids needless redraws on
    /// an otherwise idle collaboration sync.
    pub fn set_tags(&mut self, tags: Vec<TagChip>) {
        if self.tags != tags {
            self.tags = tags;
            self.tag_rects.clear();
            self.focused_tag = None;
            self.hovered_tag = None;
            self.dirty = true;
        }
    }

    /// Show the selected note's parent chain as quiet orientation metadata.
    pub fn set_parent_path(&mut self, path: String) {
        if self.parent_path != path {
            self.parent_path = path;
            self.dirty = true;
        }
    }

    /// Drain a request to browse a durable tag collection. The panel only
    /// records intent; the app is responsible for dispatching the canonical
    /// resource operation against the current CRDT snapshot.
    pub fn take_tag_open(&mut self) -> Option<crate::model::TagId> {
        self.pending_tag_open.take()
    }

    /// Replace the incoming-link projection after the app's bounded canonical
    /// index advances. The source-page id, not the visible title, is retained
    /// for activation so a remote rename/deletion cannot misroute a click.
    pub fn set_backlinks(&mut self, backlinks: Vec<BacklinkChip>) {
        if self.backlinks != backlinks {
            self.backlinks = backlinks;
            self.backlink_rects.clear();
            self.focused_backlink = None;
            self.hovered_backlink = None;
            self.dirty = true;
        }
    }

    /// Drain a request to reveal the source of a canonical incoming link.
    pub fn take_backlink_open(&mut self) -> Option<PageId> {
        self.pending_backlink_open.take()
    }

    pub fn set_transclusion_previews(
        &mut self,
        previews: HashMap<PageId, ui::widget::TransclusionPreview>,
    ) {
        self.editor.editor_mut().set_transclusion_previews(previews);
    }

    pub fn set_image_store(&mut self, store: Option<ui::ImageStore>) {
        self.image_store.clone_from(&store);
        self.editor.editor_mut().set_image_store(store);
    }

    fn fresh_editor(&self, page: Option<PageId>) -> NotesEditor {
        let mut editor = notes_editor(page);
        editor
            .editor_mut()
            .set_image_store(self.image_store.clone());
        editor
    }

    pub fn take_transclusion_open(&mut self) -> Option<PageId> {
        self.editor.editor_mut().take_transclusion_open()
    }

    pub fn take_external_link_open(&mut self) -> Option<String> {
        self.editor.editor_mut().take_external_link_open()
    }

    fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.scroll_content_height,
            self.scroll_track.height().max(0.0),
            self.scroll_y,
        )
    }

    fn scroll_thumb(&self) -> Option<Rect> {
        if !self.scroll_metrics().overflows() {
            return None;
        }
        self.page_id.and_then(|_| {
            self.scrollbar
                .thumb_rect(self.scroll_track, self.scroll_metrics())
        })
    }

    fn set_scroll_offset(&mut self, offset: f64) {
        self.follow_caret = false;
        let offset = offset.clamp(0.0, self.scroll_metrics().max_scroll());
        if (offset - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = offset;
            self.scroll_layout_dirty = true;
            self.dirty = true;
        }
    }

    fn focus_scrollbar(&mut self) -> bool {
        if self.scroll_thumb().is_none() {
            return false;
        }
        self.scroll_focused = true;
        self.focused_tag = None;
        self.focused_backlink = None;
        self.editor.set_focus(false);
        self.dirty = true;
        true
    }

    fn handle_scrollbar_event(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        if self.scroll_thumb().is_none() {
            self.scrollbar.release();
            self.scrollbar.set_hover(false);
            self.scroll_focused = false;
            return false;
        }
        let metrics = self.scroll_metrics();
        match event {
            WindowEvent::Focused(false) => {
                self.scrollbar.release();
                self.scrollbar.set_hover(false);
                self.scroll_focused = false;
                self.dirty = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.dirty |= self.scrollbar.set_hover(false);
            }
            WindowEvent::CursorMoved { .. } => {
                if let Some(offset) =
                    self.scrollbar
                        .drag_to(self.scroll_track, metrics, self.last_pointer)
                {
                    self.set_scroll_offset(offset);
                    *handled = true;
                    return true;
                }
                self.dirty |=
                    self.scrollbar
                        .update_hover(self.scroll_track, metrics, self.last_pointer);
            }
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: ElementState::Released,
                ..
            } => {
                if self.scrollbar.release() {
                    self.dirty = true;
                    *handled = true;
                    return true;
                }
            }
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: ElementState::Pressed,
                ..
            } if !*handled => {
                match self
                    .scrollbar
                    .press(self.scroll_track, metrics, self.last_pointer)
                {
                    PressOutcome::Thumb => {
                        self.follow_caret = false;
                        self.dirty = true;
                    }
                    PressOutcome::Page(offset) => self.set_scroll_offset(offset),
                    PressOutcome::Miss => {
                        self.scroll_focused = false;
                        return false;
                    }
                }
                *handled = true;
                return true;
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.scroll_focused && !*handled && event.state == ElementState::Pressed =>
            {
                let offset = match event.logical_key {
                    Key::Named(NamedKey::ArrowDown) => self.scroll_y + 40.0,
                    Key::Named(NamedKey::ArrowUp) => self.scroll_y - 40.0,
                    Key::Named(NamedKey::PageDown) => {
                        self.scroll_y + metrics.viewport * ui::widget::scrollbar::PAGE_FRACTION
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.scroll_y - metrics.viewport * ui::widget::scrollbar::PAGE_FRACTION
                    }
                    Key::Named(NamedKey::Home) => 0.0,
                    Key::Named(NamedKey::End) => metrics.max_scroll(),
                    _ => return false,
                };
                self.set_scroll_offset(offset);
                *handled = true;
                return true;
            }
            _ => {}
        }
        false
    }

    /// The title band, pinned above the scrolling document.
    fn title_height(&self) -> f64 {
        self.metrics.title
    }

    /// The lines under the tags, in document order: where the note sits in
    /// the tree and when it was written.
    fn caption_texts(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.parent_path.is_empty() {
            lines.push(self.parent_path_label());
        }
        lines.extend(self.dates.labels());
        lines
    }

    fn parent_path_label(&self) -> String {
        ui::i18n::I18nManager::format(
            "notes-parent-path",
            &[("path", self.parent_path.clone().into())],
        )
    }

    /// Enter the shared title editor from a note resource's Rename command.
    pub fn begin_title_edit(&mut self) {
        if self.page_id.is_some() {
            self.editor.set_focus(false);
            self.title_label.begin_edit();
            self.dirty = true;
        }
    }

    /// Lay `labels` out as wrapped chips from `top`, inside `[x0, x1]`.
    /// Every chip is kept: a note can have many tags, and clipping metadata
    /// after the first row would hide it.
    fn flow_chips<'a>(
        &self,
        labels: impl Iterator<Item = &'a str>,
        x0: f64,
        x1: f64,
        top: f64,
    ) -> Vec<Rect> {
        let theme = &self.layout_theme;
        let m = self.metrics;
        ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            let mut rects = Vec::new();
            let (mut x, mut y) = (x0, top);
            for label in labels {
                let width = ChipFace::intrinsic_width(label, ChipLead::None, false, text, theme)
                    .min((x1 - x0).max(0.0));
                if x > x0 && x + width > x1 {
                    x = x0;
                    y += m.chip + m.related;
                }
                rects.push(Rect::new(x, y, x + width, y + m.chip));
                x += width + m.related;
            }
            rects
        })
    }

    /// Place the note header in the scrolling document: tags, the caption
    /// lines, then the incoming links under their group title. The rects are
    /// what is painted and what pointer and assistive technology hit, and
    /// `metadata_height` is what the body is inset by.
    fn layout_metadata(&mut self) {
        self.tag_rects.clear();
        self.backlink_rects.clear();
        self.caption_rows.clear();
        self.backlink_label = Rect::ZERO;
        if self.page_id.is_none() || self.title_rect.width() <= 0.0 {
            self.metadata_height = 0.0;
            return;
        }
        let m = self.metrics;
        let origin = self.rect.y0 + self.scroll_top_inset() - self.scroll_y;
        let (x0, x1) = (self.title_rect.x0, self.title_rect.x1);
        let mut y = origin;
        let mut sections = 0_usize;

        if !self.tags.is_empty() {
            let rects = self.flow_chips(self.tags.iter().map(|tag| tag.name.as_str()), x0, x1, y);
            y = rects.last().map_or(y, |rect| rect.y1);
            self.tag_rects = rects;
            sections += 1;
        }

        let captions = self.caption_texts();
        if !captions.is_empty() {
            if sections > 0 {
                y += m.related;
            }
            for line in captions {
                self.caption_rows
                    .push((Rect::new(x0, y, x1, y + m.caption), line));
                y += m.caption;
            }
            sections += 1;
        }

        if !self.backlinks.is_empty() {
            if sections > 0 {
                y += m.sibling;
            }
            self.backlink_label = Rect::new(x0, y, x1, y + m.group_title);
            y += m.group_title + m.related;
            let labels: Vec<String> = self.backlinks.iter().map(backlink_label).collect();
            let rects = self.flow_chips(labels.iter().map(String::as_str), x0, x1, y);
            y = rects.last().map_or(y, |rect| rect.y1);
            self.backlink_rects = rects;
        }

        self.metadata_height = (y - origin).max(0.0) + m.body_gap;
    }

    /// Paint the note header from the rects `layout_metadata` placed, so
    /// pointer and assistive-technology hit testing share exactly what is
    /// drawn here.
    fn paint_metadata(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let ink = theme.tokens.colors.text_dim;
        let chip = |scene: &mut Scene,
                    text: &mut TextRenderer,
                    rect: Rect,
                    label: &str,
                    hovered: bool,
                    focused: bool| {
            ChipFace::new(rect, label)
                .hovered(hovered)
                .paint(scene, transform, text, theme);
            if focused {
                ui::widget::paint_focus_ring(scene, transform, rect, rect.height() / 2.0, theme);
            }
        };
        for (tag, rect) in self.tags.iter().zip(&self.tag_rects) {
            let hovered = self.hovered_tag == Some(tag.id);
            let focused = self.focused_tag == Some(tag.id);
            chip(scene, text, *rect, &tag.name, hovered, focused);
        }
        let caption = theme.text(TextRole::Caption);
        for (rect, line) in &self.caption_rows {
            text.draw_style(
                scene,
                line,
                ink,
                transform.pre_translate(Vec2::new(rect.x0, rect.y0)),
                Some(num::f32_of_f64(rect.width())),
                &caption,
            );
        }
        if !self.backlinks.is_empty() {
            let label = &self.backlink_label;
            text.draw_style(
                scene,
                &ui::i18n!("notes-linked-from"),
                ink,
                transform.pre_translate(Vec2::new(label.x0, label.y0)),
                Some(num::f32_of_f64(label.width())),
                &theme.text(TextRole::Subheading),
            );
        }
        for (backlink, rect) in self.backlinks.iter().zip(&self.backlink_rects) {
            let hovered = self.hovered_backlink == Some(backlink.page);
            let focused = self.focused_backlink == Some(backlink.page);
            chip(
                scene,
                text,
                *rect,
                &backlink_label(backlink),
                hovered,
                focused,
            );
        }
    }

    /// Measure how far a paragraph's first letter stands inside its rect, from
    /// the first paragraph laid out, and ask for one more layout pass when it
    /// is not what the body was inset by.
    fn measure_line_padding(&mut self) {
        let measured = self
            .editor
            .editor()
            .blocks()
            .iter()
            .find(|block| matches!(block.kind, ui::widget::BlockKind::Paragraph))
            .and_then(|block| {
                let rect = block.line.get_rect();
                (rect.width() > 0.0)
                    .then(|| block.line.caret_rect(&self.layout_theme, 0).x0 - rect.x0)
            })
            .filter(|padding| padding.is_finite() && *padding >= 0.0);
        if let Some(padding) = measured
            && self
                .line_padding
                .is_none_or(|known| (known - padding).abs() > 0.01)
        {
            self.line_padding = Some(padding);
            self.tag_layout_dirty = true;
            self.dirty = true;
        }
    }

    /// The document starts under the pinned title.
    fn scroll_top_inset(&self) -> f64 {
        self.metrics.gutter + self.title_height()
    }

    fn scroll_viewport(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.scroll_top_inset(),
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn editor_top_inset(&self) -> f64 {
        self.scroll_top_inset() + self.metadata_height
    }

    /// Currently displayed page id, if any.
    pub fn page_id(&self) -> Option<PageId> {
        self.page_id
    }

    /// True if the editor reported edits since the last call. Drains
    /// the editor's dirty flag.
    pub fn editor_is_dirty(&mut self) -> bool {
        self.editor.editor_mut().is_dirty()
    }

    /// A page's first content from its Markdown: the blocks an empty body is
    /// written with. `resolver` gives `[[Page]]` links the ids of real pages,
    /// so they point at stable resources before the content is dispatched.
    pub fn seed_blocks<F>(body: &str, resolver: F) -> Vec<ui::widget::BlockContent>
    where
        F: FnMut(&str) -> Option<PageId>,
    {
        let mut specs = crate::markdown::parse(body);
        crate::markdown::resolve_link_ids(&mut specs, resolver);
        crate::markdown::specs_to_blocks(&specs)
            .iter()
            .map(ui::widget::Block::content)
            .collect()
    }

    /// The Notes formatting and page links of every block the editor shows.
    pub fn inline_marks(&self) -> Vec<(String, Vec<crate::actions::InlineMarkSpan>)> {
        self.editor
            .editor()
            .blocks()
            .iter()
            .map(|block| {
                (
                    block.id.clone(),
                    block
                        .line
                        .marks()
                        .iter()
                        .map(crate::actions::InlineMarkSpan::from_ui)
                        .collect(),
                )
            })
            .collect()
    }

    /// Re-serialise the editor's content into the cached `body` and
    /// return a clone for persistence. Called by the app's tick when
    /// `editor_is_dirty()` came back true.
    /// Serialization goes through `notes::markdown` so inline marks survive;
    /// the editor's own `to_markdown` only emits plain text.
    pub fn take_body_snapshot(&mut self) -> String {
        let specs = crate::markdown::blocks_to_specs(self.editor.editor().blocks());
        let md = crate::markdown::serialize(&specs);
        self.body.clone_from(&md);
        md
    }

    /// Compare content rather than paint dirtiness, which also changes on load
    /// and focus. Used only when recovering a failed legacy disk read.
    pub(crate) fn has_unsaved_body(&self) -> bool {
        let specs = crate::markdown::blocks_to_specs(self.editor.editor().blocks());
        crate::markdown::serialize(&specs) != self.body
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
        self.editor.editor()
    }

    /// Test/internal mutable accessor for the editor — used by the
    /// integration tests to drive split/merge/promote and observe
    /// the resulting markdown.
    pub fn editor_mut(&mut self) -> &mut BlockEditor {
        self.editor.editor_mut()
    }

    /// The editor with the rich text it is bound to: its undo steps and
    /// edits are the page's.
    pub fn body_editor_mut(&mut self) -> &mut NotesEditor {
        &mut self.editor
    }

    /// Whether the editor's block `id` is one the page stores.
    #[must_use]
    pub fn stores_block(&self, id: &str) -> bool {
        self.editor.shows_stored(id)
    }

    /// Whether the undo step the body's last edit opened stays open for the
    /// next letters of the word.
    pub fn keeps_undo_step_open(&self) -> bool {
        ui::undo::keeps_undo_step_open(&self.editor)
    }

    /// Modal overlays own the visible caret while retaining the underlying edit.
    pub fn suppress_caret_paint(&mut self, suppressed: bool) {
        self.title_label.suppress_caret_paint(suppressed);
        self.editor.editor_mut().suppress_caret_paint(suppressed);
    }

    pub fn suppress_tooltips(&mut self, suppressed: bool) {
        self.tooltips_suppressed = suppressed;
        let viewport = if suppressed {
            Rect::ZERO
        } else {
            Rect::new(
                self.rect.x0,
                self.rect.y0 + self.editor_top_inset(),
                self.rect.x1,
                self.rect.y1,
            )
        };
        self.editor
            .editor_mut()
            .set_tooltip_viewport(Some(viewport));
    }

    /// Reveal a block without mutating the collaborative document.
    pub fn reveal_block(&mut self, index: usize) -> bool {
        if index >= self.editor.editor().blocks().len() {
            return false;
        }
        self.editor.set_focus(true);
        self.editor
            .editor_mut()
            .focus_block(index, ui::widget::CursorPlacement::Start);
        self.follow_caret = true;
        self.dirty = true;
        true
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
    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        Some(ui::widget::PrimaryScrollRegion {
            key: "primary",
            owner_generation: None,
            metrics: self.scroll_metrics(),
        })
    }
    #[expect(
        clippy::float_cmp,
        reason = "the clamped target is compared exactly: identical means the scroll did not move"
    )]
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        use ui::widget::PrimaryScrollResult;
        let Some(region) = self.primary_vertical_scroll() else {
            return PrimaryScrollResult::Unsupported;
        };
        let Some(next) = action.offset(region.metrics) else {
            return PrimaryScrollResult::Unsupported;
        };
        if next == region.metrics.offset {
            return PrimaryScrollResult::Unchanged(region);
        }
        self.set_scroll_offset(next);
        self.primary_vertical_scroll().map_or(
            PrimaryScrollResult::Unsupported,
            PrimaryScrollResult::Changed,
        )
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.page_id.is_some() {
            vec![&self.title_label, &self.editor]
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        if self.page_id.is_some() {
            vec![&mut self.title_label, &mut self.editor]
        } else {
            Vec::new()
        }
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document(state, path);
        self.date_index.sync(state);
        let dates = self
            .page_id
            .map(|page| self.date_index.get(page))
            .unwrap_or_default();
        if self.dates != dates {
            self.dates = dates;
            self.dirty = true;
        }
        self.editor.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Metrics::of(theme);
        if let Some(padding) = self.line_padding {
            self.metrics.body_inset = (self.metrics.gutter - padding).max(0.0);
        }
        self.layout_theme = theme.clone();
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
        // `apply_layout` raises this again only if the actual available width
        // changes the wrapped chip rows. That one-shot handoff avoids a
        // perpetual re-layout loop while preserving a correct editor inset.
        self.tag_layout_dirty = false;
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
                        left: length(num::f32_of_f64(self.metrics.body_inset)),
                        right: length(num::f32_of_f64(self.metrics.body_inset)),
                        top: length(num::f32_of_f64(self.editor_top_inset())),
                        bottom: taffy::LengthPercentageAuto::auto(),
                    },
                    ..existing
                },
            );
        }
        self.editor_node = Some(editor_node);
        // Retained child order must agree with the layout tree for bound
        // accessibility dispatch. Geometry and painting remain panel-owned.
        let title_node = self.title_label.build_layout(taffy, theme, node_map);
        if let Ok(existing) = taffy.style(title_node).cloned() {
            let _ = taffy.set_style(
                title_node,
                Style {
                    position: taffy::Position::Absolute,
                    ..existing
                },
            );
        }
        self.title_node = Some(title_node);
        let children = if self.page_id.is_some() {
            vec![title_node, editor_node]
        } else {
            Vec::new()
        };

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
                &children,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    // `layout` only errors for a `NodeId` from a different tree, which never
    // happens here since `node` is always the id this same widget received
    // from its own `build_layout`.
    #[allow(
        clippy::too_many_lines,
        reason = "applies and propagates the complete panel geometry in one layout pass"
    )]
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
        // A title that does not fit its line steps down one role instead of
        // eliding, so a phone at 200% text still reads the whole name. This
        // is responsive type, not cancellation of the text scale.
        let gutter = self.metrics.gutter;
        let line_width = (self.rect.width() - 2.0 * gutter).max(0.0);
        let fits = ui::text_render::with_layout_renderer(self.layout_theme.text_scale, |text| {
            let style = self.layout_theme.text(TextRole::LargeTitle);
            f64::from(text.measure_style(self.title_label.value(), None, &style).0) <= line_width
        });
        self.title_label.set_text_role(if fits {
            TextRole::LargeTitle
        } else {
            TextRole::Title
        });
        // Title band pinned to the top of the panel, above the scrolled body.
        self.title_rect = Rect::new(
            self.rect.x0 + gutter,
            self.rect.y0 + gutter,
            (self.rect.x1 - gutter).max(self.rect.x0 + gutter),
            self.rect.y0 + gutter + self.title_height(),
        );
        self.title_label.apply_layout_external(self.title_rect);
        let prior_metadata = self.metadata_height;
        self.layout_metadata();
        if (self.metadata_height - prior_metadata).abs() > f64::EPSILON {
            self.tag_layout_dirty = true;
            self.dirty = true;
        }
        // Lay the editor out at the unshifted origin first so we can
        // compute its natural content extent, then shift by -scroll_y
        // for the second pass. Cheaper than measuring twice through
        // taffy — we only re-walk the apply_layout origin.
        if let Some(n) = self.editor_node {
            let unshifted_origin = Point::new(self.rect.x0, self.rect.y0);
            self.editor.apply_layout(taffy, n, unshifted_origin);
            self.measure_line_padding();
            let editor_rect = self.editor.get_rect();
            let last_block_bottom = self.editor.editor().content_bottom();
            // Content height = where the lowest block actually ends,
            // minus the editor's top. This catches taffy under-reports
            // for flex-column overflow (same kludge ui::Scrollable uses
            // in its apply_layout).
            let editor_content_h = (last_block_bottom - editor_rect.y0).max(0.0);
            let viewport_h = (self.rect.height() - self.scroll_top_inset()).max(0.0);
            // The last block ends one gutter above the window's bottom edge.
            self.scroll_content_height =
                self.metadata_height + editor_content_h + self.metrics.gutter;
            let track_top = self.rect.y0 + self.scroll_top_inset();
            self.scroll_track = Rect::new(
                self.rect.x1 - self.metrics.scroll_inset - self.metrics.scroll_width,
                track_top,
                self.rect.x1 - self.metrics.scroll_inset,
                track_top + viewport_h,
            );
            if !self.scroll_metrics().overflows() {
                self.scrollbar.release();
                self.scroll_focused = false;
            }
            let max_scroll = (self.scroll_content_height - viewport_h).max(0.0);
            // A layout pass can land before the editor's text has been shaped.
            // Every block rect is then degenerate, `content_h` collapses, and
            // clamping against it throws the reader back to the top of the
            // document with nothing to explain it — the position is gone, not
            // restored on the next frame. Ask whether anything was measured at
            // all rather than whether the total came out at exactly zero: a
            // partial pass yields a small non-zero height that clamps just as
            // destructively. An empty document has no blocks, and 0 is right
            // for it.
            let measured = self.editor.editor().blocks().is_empty()
                || self
                    .editor
                    .editor()
                    .blocks()
                    .iter()
                    .any(|b| b.line.get_rect().height() > 0.0);
            if measured {
                if self.follow_caret
                    && let Some(caret) = self.editor.ime_cursor_rect()
                {
                    let viewport_top = self.rect.y0 + self.scroll_top_inset();
                    let viewport_bottom = viewport_top + viewport_h;
                    // The first caret starts below the editor's own top padding.
                    // Revealing the document start must retain that padding,
                    // rather than scrolling it away to pin the glyph to the clip.
                    let at_document_start = self.editor.editor().focused_index() == Some(0)
                        && self
                            .editor
                            .editor()
                            .blocks()
                            .first()
                            .is_some_and(|block| block.line.cursor_pos() == 0);
                    if at_document_start {
                        self.scroll_y = 0.0;
                    } else if caret.y0 - self.scroll_y < viewport_top {
                        self.scroll_y = caret.y0 - viewport_top;
                    } else if caret.y1 - self.scroll_y > viewport_bottom {
                        self.scroll_y = caret.y1 - viewport_bottom;
                    }
                    self.follow_caret = false;
                }
                let clamped = self.scroll_y.clamp(0.0, max_scroll);
                if (clamped - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = clamped;
                    self.dirty = true;
                }
            }
            // Metadata belongs to the same scrolling document as the blocks.
            // Refresh its hit/paint geometry after a caret reveal or clamp
            // changes the shared offset during this layout pass.
            self.layout_metadata();
            // Re-apply layout with the (possibly clamped) scroll
            // offset so block rects reflect their on-screen position.
            // Without this, click hit-tests would target stale rects
            // for a scrolled view.
            let shifted_origin = Point::new(self.rect.x0, self.rect.y0 - self.scroll_y);
            self.editor.apply_layout(taffy, n, shifted_origin);
            let viewport = self.scroll_viewport();
            self.editor
                .editor_mut()
                .set_tooltip_viewport(Some(viewport));
            self.scroll_layout_dirty = false;
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the canvas draw order keeps the sticky title, clipped scrolling document and editor overlay in one auditable stacking context"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // No ground: the window paints `surfaces.content` under the panel.
        if self.page_id.is_some() {
            // Prominent, editable page title, drawn OUTSIDE the scroll clip so
            // it stays pinned while the body scrolls under it.
            self.title_label.apply_layout_external(self.title_rect);
            self.title_label
                .draw(scene, transform, text_renderer, theme, presence);

            // Everything below the sticky title is one scrolling document:
            // metadata first, then authored blocks. One clip keeps scrolled
            // chips from painting through the title.
            let body_clip = self.scroll_viewport();
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &body_clip,
            );
            self.paint_metadata(scene, transform, text_renderer, theme);
            self.editor
                .draw(scene, transform, text_renderer, theme, presence);
            scene.pop_layer();
            if self.scroll_thumb().is_some() {
                self.scrollbar.draw(
                    scene,
                    transform,
                    self.scroll_track,
                    self.scroll_metrics(),
                    theme,
                    self.scroll_focused,
                );
            }
            // Overlay popovers (slash menu / link popover) — drawn AFTER the
            // main editor scene so they land on top of every block, and
            // OUTSIDE the scroll clip so they stay visible at their anchor.
            self.editor
                .editor_mut()
                .draw_overlay(scene, transform, text_renderer, theme);
        } else {
            // No note open: a composed zero-state rather than a bare line —
            // this is what a fresh, unseeded workspace shows, so it should
            // invite the first note rather than look broken.
            self.empty_action_rect = EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                self.rect,
                &EmptyStateContent {
                    icon: Some(Icon::FileText),
                    action_label: Some("New note"),
                    ..EmptyStateContent::new("Start your first note")
                },
                self.last_pointer,
            )
            .unwrap_or(Rect::ZERO);
            if self.empty_action_focused && self.empty_action_rect.width() > 0.0 {
                ui::widget::paint_focus_ring(
                    scene,
                    transform,
                    self.empty_action_rect,
                    f64::from(theme.tokens.radii.control),
                    theme,
                );
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event boundary keeps pointer, keyboard, empty-state and editor routing ordered around one handled flag"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Track the pointer so a press (which carries no position) can be
        // classified as landing in the title band.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            let inside_document = self.scroll_viewport().contains(self.last_pointer);
            let hovered = inside_document
                .then(|| {
                    self.tags
                        .iter()
                        .zip(&self.tag_rects)
                        .find_map(|(tag, rect)| rect.contains(self.last_pointer).then_some(tag.id))
                })
                .flatten();
            if self.hovered_tag != hovered {
                self.hovered_tag = hovered;
                self.dirty = true;
            }
            let backlink = inside_document
                .then(|| {
                    self.backlinks
                        .iter()
                        .zip(&self.backlink_rects)
                        .find_map(|(link, rect)| {
                            rect.contains(self.last_pointer).then_some(link.page)
                        })
                })
                .flatten();
            if self.hovered_backlink != backlink {
                self.hovered_backlink = backlink;
                self.dirty = true;
            }
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
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            } else if keyboard_activation {
                self.empty_action_requested = true;
                self.dirty = true;
                *ctx.handled = true;
            }
            return;
        }

        if self.handle_scrollbar_event(event, ctx.handled) {
            return;
        }

        // Metadata chips are real navigation controls, not painted labels.
        // Their id is retained and later admitted through the same canonical
        // tag operation used by Cmd-K, so a rename/deletion race cannot route
        // a click based on stale display text.
        let press_on_tag = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        if press_on_tag
            && self.scroll_viewport().contains(self.last_pointer)
            && let Some(tag) = self
                .tags
                .iter()
                .zip(&self.tag_rects)
                .find_map(|(tag, rect)| rect.contains(self.last_pointer).then_some(tag.id))
        {
            self.pending_tag_open = Some(tag);
            self.focused_tag = Some(tag);
            self.dirty = true;
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
            return;
        }
        if press_on_tag
            && self.scroll_viewport().contains(self.last_pointer)
            && let Some(page) = self
                .backlinks
                .iter()
                .zip(&self.backlink_rects)
                .find_map(|(link, rect)| rect.contains(self.last_pointer).then_some(link.page))
        {
            self.pending_backlink_open = Some(page);
            self.focused_backlink = Some(page);
            self.dirty = true;
            *ctx.focus_target = Some(ctx.node_id);
            *ctx.handled = true;
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
            self.begin_title_edit();
            *ctx.focus_target = self.title_node;
            *ctx.handled = true;
            return;
        }
        if self.title_label.is_editing() {
            self.title_label.apply_layout_external(self.title_rect);
            self.title_label
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id));
            match self.title_label.take_outcome() {
                EditOutcome::Committed(new) => {
                    self.pending_rename = Some(new);
                    self.editor.set_focus(true);
                    self.dirty = true;
                }
                EditOutcome::Cancelled => {
                    self.editor.set_focus(true);
                    self.dirty = true;
                }
                EditOutcome::None => {}
            }
            // Typing / Enter / Esc are consumed by the field; stop so they
            // never reach the body. A blur-commit press outside the title is
            // deliberately NOT consumed, so it falls through to place the
            // caret in the body below.
            if *ctx.handled {
                return;
            }
        }

        // Mouse wheel: scroll the panel's body. Matches ui::Scrollable's
        // wheel-to-px conversion (~120 px / notch). Consumes the event
        // so the underlying TextInput doesn't try to interpret a wheel
        // delta as a cursor move.
        if let WindowEvent::MouseWheel { delta, .. } = event {
            self.follow_caret = false;
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 40.0,
                winit::event::MouseScrollDelta::PixelDelta(p) => -p.y,
            };
            if dy.abs() > f64::EPSILON {
                let prev = self.scroll_y;
                self.set_scroll_offset(self.scroll_y + dy);
                // Clamp against max_scroll happens in apply_layout
                // since that's where we know the content extent.
                if (self.scroll_y - prev).abs() > f64::EPSILON {
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
        }
        if let Some(n) = self.editor_node {
            self.editor
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if self.editor.is_focused()
                && *ctx.handled
                && matches!(
                    event,
                    WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
                )
            {
                self.follow_caret = true;
                self.dirty = true;
            }
        }
    }

    fn name(&self) -> &'static str {
        "MainPanel"
    }

    fn test_id(&self) -> Option<&str> {
        self.page_id.is_none().then_some("notes.first_note")
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.page_id.is_some() && self.title_rect.contains(point) {
            Some(winit::window::CursorIcon::Text)
        } else if self.tag_rects.iter().any(|rect| rect.contains(point))
            || self.backlink_rects.iter().any(|rect| rect.contains(point))
            || (self.page_id.is_none() && self.empty_action_rect.contains(point))
        {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        use ui::i18n::I18nManager;
        self.page_id?;
        if self.tooltips_suppressed || !self.rect.contains(point) {
            return None;
        }
        if self.title_rect.contains(point) && !self.title_label.is_editing() {
            return Some((
                self.title_rect,
                I18nManager::lookup("notes-surface-rename", None),
            ));
        }
        for (tag, rect) in self.tags.iter().zip(&self.tag_rects) {
            if rect.contains(point) {
                return Some((
                    *rect,
                    I18nManager::format("notes-surface-tag", &[("tag", tag.name.clone().into())]),
                ));
            }
        }
        for (backlink, rect) in self.backlinks.iter().zip(&self.backlink_rects) {
            if rect.contains(point) {
                return Some((
                    *rect,
                    I18nManager::format(
                        "notes-surface-backlink",
                        &[("title", backlink.title.clone().into())],
                    ),
                ));
            }
        }
        None
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        self.page_id.is_none()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.page_id.is_some() {
            if !focused {
                self.scroll_focused = false;
                self.dirty = true;
            }
            return;
        }
        let next = focused && self.page_id.is_none();
        if self.empty_action_focused != next {
            self.empty_action_focused = next;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        (self.page_id.is_none() && self.empty_action_focused) || self.scroll_focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if self.page_id.is_none() {
            let button = AccessNode::new(Role::Button)
                .with_name("New note")
                .with_description("Create the first note in this workspace")
                .with_action_target("notes.first_note")
                .with_state(AccessState {
                    focusable: true,
                    focused: self.empty_action_focused,
                    ..Default::default()
                });
            // Only the painted pill takes the click; without its own bounds
            // the button would claim the whole panel, whose centre is empty.
            return Some(if self.empty_action_rect.width() > 0.0 {
                button.with_bounds(self.empty_action_rect)
            } else {
                button
            });
        }
        let mut children = vec![
            AccessNode::new(Role::TextInput)
                .with_name("Note title")
                .with_value(self.title_label.current_text())
                .with_action_target("notes.page-title")
                .with_bounds(self.title_rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.title_label.is_editing()),
                ),
        ];
        children.push(AccessNode::new(Role::Label).with_name(self.caption_texts().join(". ")));
        for (tag, rect) in self.tags.iter().zip(&self.tag_rects) {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Tag: {}", tag.name))
                    .with_description("Open notes with this tag")
                    .with_action_target(format!("notes.tag-chip:{}", tag.id))
                    .with_hashed_key(tag.id)
                    .with_bounds(*rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused_tag == Some(tag.id)),
                    ),
            );
        }
        for (backlink, rect) in self.backlinks.iter().zip(&self.backlink_rects) {
            let reference_word = if backlink.references == 1 {
                "reference"
            } else {
                "references"
            };
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Linked from: {}", backlink.title))
                    .with_description(format!(
                        "Open the source note; {} {}",
                        backlink.references, reference_word
                    ))
                    .with_action_target(format!("notes.backlink-chip:{}", backlink.page))
                    .with_hashed_key(backlink.page)
                    .with_bounds(*rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.focused_backlink == Some(backlink.page)),
                    ),
            );
        }
        if let Some(thumb) = self.scroll_thumb() {
            children.push(
                AccessNode::new(Role::Slider)
                    .with_name(ui::i18n!("notes-scroll-position"))
                    .with_description(ui::i18n!("notes-scroll-help"))
                    .with_value(format!(
                        "{:.0}",
                        self.scroll_y / self.scroll_metrics().max_scroll() * 100.0
                    ))
                    .with_bounds(thumb)
                    .with_action_target("notes.scroll-position")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.scroll_focused),
                    ),
            );
        }
        Some(
            AccessNode::new(Role::Group)
                .with_name("Note header")
                .with_description("Note title, dates, tags, and incoming links")
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        if *index == 2 + self.tags.len() + self.backlinks.len() {
            return self.scroll_thumb();
        }
        if *index == 0 {
            return Some(self.title_rect);
        }
        if *index == 1 {
            return None;
        }
        let index = index.saturating_sub(2);
        self.tag_rects.get(index).copied().or_else(|| {
            self.backlink_rects
                .get(index.saturating_sub(self.tag_rects.len()))
                .copied()
        })
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index == 2 + self.tags.len() + self.backlinks.len() {
            return self.focus_scrollbar();
        }
        self.scroll_focused = false;
        if *index == 0 {
            self.focused_tag = None;
            self.focused_backlink = None;
            self.title_label.begin_edit();
        } else if *index == 1 {
            return false;
        } else if let Some(tag) = self.tags.get(index.saturating_sub(2)) {
            self.focused_tag = Some(tag.id);
            self.focused_backlink = None;
        } else if let Some(backlink) = self
            .backlinks
            .get(index.saturating_sub(2 + self.tags.len()))
        {
            self.focused_tag = None;
            self.focused_backlink = Some(backlink.page);
        } else {
            return false;
        }
        self.dirty = true;
        true
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
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

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        if target == "notes.scroll-position" {
            if self.scroll_thumb().is_none() {
                return false;
            }
            let metrics = self.scroll_metrics();
            let offset = match action {
                ui::a11y::Action::Focus => return self.focus_scrollbar(),
                ui::a11y::Action::Increment => {
                    self.scroll_y + metrics.viewport * ui::widget::scrollbar::PAGE_FRACTION
                }
                ui::a11y::Action::Decrement => {
                    self.scroll_y - metrics.viewport * ui::widget::scrollbar::PAGE_FRACTION
                }
                ui::a11y::Action::SetValue(value) => {
                    let Ok(percent) = value.parse::<f64>() else {
                        return false;
                    };
                    if !percent.is_finite() {
                        return false;
                    }
                    percent.clamp(0.0, 100.0) / 100.0 * metrics.max_scroll()
                }
                _ => return false,
            };
            self.set_scroll_offset(offset);
            return true;
        }
        if target == "notes.first_note" && self.page_id.is_none() {
            return self.perform_action(action, state, path);
        }
        if target == "notes.page-title" {
            return Widget::perform_action(&mut self.title_label, action, state, path);
        }
        if let Some(page) = target
            .strip_prefix("notes.backlink-chip:")
            .and_then(|id| id.parse().ok())
        {
            if !self
                .backlinks
                .iter()
                .any(|candidate| candidate.page == page)
                || !matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus)
            {
                return false;
            }
            self.focused_backlink = Some(page);
            if matches!(action, ui::a11y::Action::Activate) {
                self.pending_backlink_open = Some(page);
            }
            self.dirty = true;
            return true;
        }
        let Some(tag) = target
            .strip_prefix("notes.tag-chip:")
            .and_then(|id| id.parse().ok())
        else {
            return false;
        };
        if !self.tags.iter().any(|candidate| candidate.id == tag)
            || !matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus)
        {
            return false;
        }
        self.focused_tag = Some(tag);
        if matches!(action, ui::a11y::Action::Activate) {
            self.pending_tag_open = Some(tag);
        }
        self.dirty = true;
        true
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
        std::mem::take(&mut self.tag_layout_dirty) || self.editor.take_structure_dirty()
    }

    fn is_layout_dirty(&self) -> bool {
        // The editor's leaf nodes have intrinsic sizes (font_size *
        // 1.5) so size-affecting edits (heading promotion) need a
        // re-layout. Be conservative: always claim dirty when the
        // editor reports a redraw need.
        self.scroll_layout_dirty || self.tag_layout_dirty || self.editor.needs_redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ObjId;
    use ui::widget::BlockKind;
    use uuid::Uuid;

    fn fresh_page_id() -> PageId {
        Uuid::new_v4()
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn page_titles_and_empty_state_are_accessible_rename_boundaries() {
        eprintln!("phase set_title_marks_dirty_when_changed");
        {
            let mut p = MainPanel::new();
            p.mark_drawn();
            assert!(!p.needs_redraw());
            p.set_title(Some("Hello".into()));
            assert!(p.needs_redraw());
            p.mark_drawn();
            p.set_title(Some("Hello".into()));
            assert!(!p.needs_redraw());
        }

        eprintln!("phase accessible_page_title_queues_the_same_rename_boundary");
        {
            let mut panel = MainPanel::new();
            let id = fresh_page_id();
            panel.load_page(id, "Launch", "");
            let node = panel
                .accessibility()
                .expect("open pages expose their title");
            assert_eq!(node.role, ui::a11y::Role::Group);
            let title = node
                .children
                .first()
                .expect("header exposes the page title");
            assert_eq!(title.role, ui::a11y::Role::TextInput);
            assert_eq!(title.action_target.as_deref(), Some("notes.page-title"));
            let target = title.action_target.as_deref().unwrap();
            let mut doc = AutoCommit::new();
            assert!(panel.perform_semantic_action(
                target,
                &ui::a11y::Action::SetValue("Launch plan".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(
                panel.take_title_rename().is_none(),
                "typing remains an uncommitted inline rename"
            );
            panel.title_label.commit();
            assert_eq!(panel.take_title_rename().as_deref(), Some("Launch plan"));
        }

        eprintln!("phase empty_state_primary_action_is_a_focused_accessible_button");
        {
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
    }

    #[test]
    fn main_panel_loads_snapshots_and_clears_page_bodies() {
        eprintln!("phase load_page_seeds_editor_from_markdown");
        {
            let mut p = MainPanel::new();
            let id = fresh_page_id();
            p.load_page(id, "Hello", "# H\n\nbody\n");
            assert_eq!(p.page_id(), Some(id));
            assert_eq!(p.editor().blocks().len(), 2);
            assert!(matches!(p.editor().blocks()[0].kind, BlockKind::Heading(1)));
        }

        eprintln!("phase load_page_preserves_external_link_presentation");
        {
            let mut panel = MainPanel::new();
            panel.load_page(
                fresh_page_id(),
                "Links",
                "Read [the guide](https://example.test/guide).\n",
            );
            assert!(matches!(
                &panel.editor().blocks()[0].line.marks()[0].mark,
                ui::widget::Mark::ExternalLink(url) if url == "https://example.test/guide"
            ));
        }

        eprintln!("phase take_body_snapshot_round_trips");
        {
            let mut p = MainPanel::new();
            p.load_page(fresh_page_id(), "T", "# H\n\nbody\n");
            let snap = p.take_body_snapshot();
            assert_eq!(snap, "# H\n\nbody\n");
        }

        eprintln!("phase clear_resets_state");
        {
            let mut p = MainPanel::new();
            p.load_page(fresh_page_id(), "T", "# H\n");
            p.clear();
            assert!(p.page_id().is_none());
            assert!(p.title().is_none());
        }
    }

    /// A panel laid out at `width` in `theme`, with the title band placed
    /// where `apply_layout` puts it.
    fn panel_at(theme: &Theme, width: f64) -> MainPanel {
        let mut panel = MainPanel::new();
        panel.metrics = Metrics::of(theme);
        panel.layout_theme = theme.clone();
        panel.page_id = Some(fresh_page_id());
        panel.rect = Rect::new(0.0, 0.0, width, 800.0);
        let gutter = panel.metrics.gutter;
        panel.title_rect = Rect::new(
            gutter,
            gutter,
            width - gutter,
            gutter + panel.title_height(),
        );
        panel
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn metadata_bands_tags_and_backlinks_clear_the_editor_and_keep_identity() {
        eprintln!("phase high_text_title_tags_and_first_block_have_separate_bands");
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut panel = panel_at(&theme, 900.0);
            panel.set_tags(vec![
                TagChip {
                    id: fresh_page_id(),
                    name: "Release".into(),
                },
                TagChip {
                    id: fresh_page_id(),
                    name: "Writing".into(),
                },
            ]);
            panel.layout_metadata();
            let first = panel.tag_rects[0];
            let last = panel.tag_rects[1];
            assert!(
                panel.title_rect.y1 <= first.y0,
                "title must not overlap tag chips"
            );
            assert!(
                last.y1 <= panel.editor_top_inset(),
                "tag chips ({}) must clear the first block ({})",
                last.y1,
                panel.editor_top_inset()
            );
            assert!(
                (last.x0 - first.x1 - panel.metrics.related).abs() < 1e-9,
                "neighbouring chips keep the related step apart"
            );
            assert!(
                (first.height() - ChipFace::height(&theme)).abs() < 1e-9,
                "a chip is as high as the toolkit's chip"
            );
        }

        eprintln!("phase the_header_is_one_group_on_the_content_gutter");
        {
            let theme = Theme::dark();
            let mut panel = panel_at(&theme, 800.0);
            panel.set_parent_path("Projects / Launch".into());
            panel.dates = crate::metadata::NoteDates {
                created: Some(1_700_000_000),
                edited: Some(1_700_003_600),
            };
            panel.set_tags(vec![TagChip {
                id: fresh_page_id(),
                name: "Research".into(),
            }]);
            panel.set_backlinks(vec![BacklinkChip {
                page: fresh_page_id(),
                title: "Roadmap".into(),
                references: 1,
            }]);
            panel.layout_metadata();
            let gutter = panel.metrics.gutter;
            let edge = panel.rect.x0 + gutter;
            assert!((panel.title_rect.x0 - edge).abs() < 1e-9);
            assert!((panel.tag_rects[0].x0 - edge).abs() < 1e-9);
            assert!((panel.backlink_label.x0 - edge).abs() < 1e-9);
            assert!((panel.backlink_rects[0].x0 - edge).abs() < 1e-9);
            assert_eq!(panel.caption_rows.len(), 2, "the path and one dates line");
            for (rect, _) in &panel.caption_rows {
                assert!((rect.x0 - edge).abs() < 1e-9);
                assert!(rect.y0 >= panel.scroll_viewport().y0);
            }
            let last = panel.backlink_rects[0];
            assert!(
                (panel.editor_top_inset() - last.y1 - panel.metrics.body_gap).abs() < 1e-9,
                "the body starts one body gap under the header"
            );
        }

        eprintln!("phase a_note_without_metadata_still_keeps_its_body_gap_under_the_title");
        {
            let theme = Theme::dark();
            let mut panel = panel_at(&theme, 800.0);
            panel.layout_metadata();
            assert!(panel.caption_rows.is_empty());
            assert!(panel.tag_rects.is_empty());
            assert!(
                (panel.editor_top_inset() - panel.title_rect.y1 - panel.metrics.body_gap).abs()
                    < 1e-9
            );
        }

        eprintln!("phase wrapped_tags_reserve_space_before_the_editor");
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut panel = panel_at(&theme, 420.0);
            panel.set_tags(
                ["Release planning", "Customer research", "Accessibility"]
                    .into_iter()
                    .map(|name| TagChip {
                        id: fresh_page_id(),
                        name: name.into(),
                    })
                    .collect(),
            );
            panel.layout_metadata();
            assert_eq!(panel.tag_rects.len(), 3);
            assert!(
                panel.tag_rects[1].y0 > panel.tag_rects[0].y0,
                "long tags should wrap instead of disappearing beyond the panel edge"
            );
            for rect in &panel.tag_rects {
                assert!(
                    rect.x1 <= panel.title_rect.x1 + 1e-9,
                    "no chip leaves the gutter"
                );
            }
            assert!(
                panel.editor_top_inset() >= panel.tag_rects.last().expect("tag rect").y1,
                "the editor must start below every wrapped tag row"
            );
        }

        eprintln!("phase surface_hints_follow_hit_targets");
        {
            let theme = Theme::dark();
            let mut panel = panel_at(&theme, 600.0);
            panel.set_tags(vec![TagChip {
                id: fresh_page_id(),
                name: "Research".into(),
            }]);
            panel.set_backlinks(vec![BacklinkChip {
                page: fresh_page_id(),
                title: "A long source note title".into(),
                references: 1,
            }]);
            panel.layout_metadata();
            assert_eq!(
                panel.cursor_shape(panel.title_rect.center()),
                Some(winit::window::CursorIcon::Text)
            );
            assert_eq!(
                panel.cursor_shape(panel.tag_rects[0].center()),
                Some(winit::window::CursorIcon::Pointer)
            );
            assert!(panel.tooltip_target_at(panel.title_rect.center()).is_some());
            assert!(
                panel
                    .tooltip_target_at(panel.tag_rects[0].center())
                    .unwrap()
                    .1
                    .contains("Research")
            );
            assert!(
                panel
                    .tooltip_target_at(panel.backlink_rects[0].center())
                    .unwrap()
                    .1
                    .contains("A long source note title")
            );
            assert!(panel.tooltip_target_at(Point::new(590.0, 790.0)).is_none());
            panel.suppress_tooltips(true);
            assert!(panel.tooltip_target_at(panel.title_rect.center()).is_none());
            panel.suppress_tooltips(false);
            assert!(panel.tooltip_target_at(panel.title_rect.center()).is_some());
            panel.begin_title_edit();
            assert!(
                panel.tooltip_target_at(panel.title_rect.center()).is_none(),
                "rename hint must not cover an active title edit"
            );
        }

        eprintln!("phase semantic_tag_activation_retains_the_stable_tag_id");
        {
            let mut panel = MainPanel::new();
            let tag = fresh_page_id();
            panel.page_id = Some(fresh_page_id());
            panel.set_tags(vec![TagChip {
                id: tag,
                name: "Release".into(),
            }]);
            panel.tag_rects = vec![Rect::new(32.0, 90.0, 120.0, 112.0)];
            let mut doc = AutoCommit::new();

            assert!(Widget::perform_semantic_action(
                &mut panel,
                &format!("notes.tag-chip:{tag}"),
                &ui::a11y::Action::Activate,
                &mut doc,
                &ObjId::Root,
            ));
            assert_eq!(panel.take_tag_open(), Some(tag));
        }

        eprintln!("phase backlinks_activate_by_page_identity");
        {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut panel = panel_at(&theme, 420.0);
            let source = fresh_page_id();
            panel.set_backlinks(vec![BacklinkChip {
                page: source,
                title: "Long-running launch notes".into(),
                references: 2,
            }]);
            panel.layout_metadata();
            assert_eq!(panel.backlink_rects.len(), 1);
            assert!(
                panel.editor_top_inset() >= panel.backlink_rects[0].y1,
                "incoming-link metadata must clear the first block"
            );
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_semantic_action(
                &mut panel,
                &format!("notes.backlink-chip:{source}"),
                &ui::a11y::Action::Activate,
                &mut doc,
                &ObjId::Root,
            ));
            assert_eq!(panel.take_backlink_open(), Some(source));
        }
    }
}
