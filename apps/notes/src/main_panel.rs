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
//! |  (save failure, while there is one)|
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
use vello::kurbo::{Affine, Point, Rect};
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
use ui::widget::{Banner, BlockEditor, EditOutcome, EditableLabel, EmptyState, EmptyStateContent};
use ui::widget::{PressOutcome, ScrollMetrics, Scrollbar};

use crate::model::PageId;
use crate::note_header::NoteHeader;
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
    /// The gap between the title band and the header under it.
    header_gap: f64,
    /// The gap between the header and the first block of the body.
    body_gap: f64,
    /// The title band: its line plus room for the focus ring.
    title: f64,
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
            header_gap: f64::from(spacing.rhythm.related),
            body_gap: f64::from(spacing.s4),
            // The title label centres a line box measured at 1x, so at a larger
            // text scale its glyphs hang below the centre by the growth; the
            // band holds the glyphs and that offset.
            title: line(TextRole::LargeTitle) * (2.0 - 1.0 / f64::from(theme.text_scale.max(1.0)))
                + 2.0 * f64::from(spacing.s2),
            scroll_width: f64::from(spacing.s2),
            scroll_inset: f64::from(spacing.s1),
        }
    }
}

/// Place a child of the panel at `top`, as wide as the panel less a gutter on
/// either side and as tall as its own content. The panel positions these by
/// hand: they are absolute, so their height never sizes the panel.
fn pin_under_gutter(taffy: &mut TaffyTree<()>, node: NodeId, gutter: f64, top: f64) {
    let Ok(existing) = taffy.style(node).cloned() else {
        return;
    };
    let _ = taffy.set_style(
        node,
        Style {
            position: taffy::Position::Absolute,
            size: Size {
                width: Dimension::auto(),
                height: Dimension::auto(),
            },
            inset: taffy::Rect {
                left: length(num::f32_of_f64(gutter)),
                right: length(num::f32_of_f64(gutter)),
                top: length(num::f32_of_f64(top)),
                bottom: taffy::LengthPercentageAuto::auto(),
            },
            ..existing
        },
    );
}

/// Presentation data for a durable tag. Keeping its id beside the label means
/// a tag button can invoke the same stable tag operation as the resource
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
    /// Durable tags, the note's place and dates, and the incoming links, as
    /// toolkit widgets under the title. The tag and link buttons only record
    /// intent; the app drains it into the canonical operation.
    header: NoteHeader,
    header_node: Option<NodeId>,
    /// Shown under the title while the note cannot be saved, until it can.
    save_failure: Option<Banner>,
    banner_node: Option<NodeId>,
    /// The height the banner (and the gap under it) takes between the title
    /// and the scrolling document.
    banner_height: f64,
    /// The height the whole header (tags, captions, incoming links and the
    /// gap to the body) takes in the scrolling document. It is recalculated
    /// whenever width, text scale, or membership changes, so large text and
    /// long tag names never collide with the first document block.
    metadata_height: f64,
    /// Set while a new header height is being discovered so the host rebuilds
    /// the absolute editor inset on the immediately following layout pass.
    tag_layout_dirty: bool,
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
    /// `build_layout`: `apply_layout` measures the header with them and is handed
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
            header: NoteHeader::default(),
            header_node: None,
            save_failure: None,
            banner_node: None,
            banner_height: 0.0,
            metadata_height: 0.0,
            tag_layout_dirty: false,
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
        self.refresh_captions();
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
        self.refresh_captions();
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
        self.refresh_captions();
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
        self.header.set_tags(Vec::new());
        self.header.set_backlinks(Vec::new());
        self.header.set_captions(Vec::new());
        self.save_failure = None;
        self.banner_height = 0.0;
        self.metadata_height = 0.0;
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

    /// Replace the visible tags. Sorting/deduplication is done
    /// by the model; this cheap presentation diff avoids needless redraws on
    /// an otherwise idle collaboration sync.
    pub fn set_tags(&mut self, tags: Vec<TagChip>) {
        if self.header.set_tags(tags) {
            self.header_changed();
        }
    }

    /// Show the selected note's parent chain as quiet orientation metadata.
    pub fn set_parent_path(&mut self, path: String) {
        if self.parent_path != path {
            self.parent_path = path;
            self.refresh_captions();
        }
    }

    /// Drain a request to browse a durable tag collection. The panel only
    /// records intent; the app is responsible for dispatching the canonical
    /// resource operation against the current CRDT snapshot.
    pub fn take_tag_open(&mut self) -> Option<crate::model::TagId> {
        self.header.take_tag_open()
    }

    /// Replace the incoming-link projection after the app's bounded canonical
    /// index advances. The source-page id, not the visible title, is retained
    /// for activation so a remote rename/deletion cannot misroute a click.
    pub fn set_backlinks(&mut self, backlinks: Vec<BacklinkChip>) {
        if self.header.set_backlinks(backlinks) {
            self.header_changed();
        }
    }

    /// Drain a request to reveal the source of a canonical incoming link.
    pub fn take_backlink_open(&mut self) -> Option<PageId> {
        self.header.take_backlink_open()
    }

    /// Say whether the note cannot be saved. A failure stays in view, under
    /// the title, until the note saves again.
    pub fn set_save_failed(&mut self, failed: bool) {
        if failed == self.save_failure.is_some() {
            return;
        }
        self.save_failure = failed.then(|| {
            Banner::error(ui::i18n!("notes-save-failed"))
                .with_message(ui::i18n!("notes-save-failed-detail"))
        });
        self.header_changed();
    }

    /// The header's content changed: its widgets are rebuilt, and the body
    /// starts where they now end, on the next layout pass.
    fn header_changed(&mut self) {
        self.tag_layout_dirty = true;
        self.dirty = true;
    }

    /// The lines under the tags, in document order: where the note sits in
    /// the tree and when it was written.
    fn refresh_captions(&mut self) {
        let mut lines = Vec::new();
        if !self.parent_path.is_empty() {
            lines.push(self.parent_path_label());
        }
        lines.extend(self.dates.labels());
        if self.header.set_captions(lines) {
            self.header_changed();
        }
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

    /// How much room the save failure takes under the title, from the layout
    /// the toolkit solved for it, and where it stands.
    fn measure_banner(&mut self, taffy: &TaffyTree<()>) {
        let height = self
            .banner_node
            .filter(|_| self.page_id.is_some())
            .and_then(|node| taffy.layout(node).ok())
            .map_or(0.0, |layout| f64::from(layout.size.height));
        self.banner_height = if height > 0.0 {
            height + f64::from(self.layout_theme.tokens.spacing.rhythm.related)
        } else {
            0.0
        };
        if let (Some(node), Some(banner)) = (self.banner_node, self.save_failure.as_mut()) {
            banner.apply_layout(taffy, node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    /// Where the header stands, from the layout the toolkit solved for it:
    /// `metadata_height` is what the body is inset by.
    fn measure_header(&mut self, taffy: &TaffyTree<()>) {
        let height = self
            .header_node
            .filter(|_| self.page_id.is_some() && !self.header.is_empty())
            .and_then(|node| taffy.layout(node).ok())
            .map_or(0.0, |layout| f64::from(layout.size.height));
        let header = if height > 0.0 {
            self.metrics.header_gap + height
        } else {
            0.0
        };
        self.metadata_height = header + self.metrics.body_gap;
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
        self.metrics.gutter + self.title_height() + self.banner_height
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
    /// Test hook: the test id and rect of each addressed widget in the
    /// header: the tag and link buttons and the caption lines.
    #[doc(hidden)]
    pub fn header_rects_for_test(&self) -> Vec<(String, Rect)> {
        fn collect(widget: &dyn Widget, out: &mut Vec<(String, Rect)>) {
            if let Some(id) = widget.test_id() {
                out.push((id.to_owned(), widget.get_rect()));
            }
            widget.for_each_child(&mut |child| collect(child, out));
        }
        let mut out = Vec::new();
        if let Some(header) = self.header.built() {
            collect(header, &mut out);
        }
        out
    }

    /// Test hook: whether the save failure is showing under the title.
    #[doc(hidden)]
    pub fn save_failure_shown_for_test(&self) -> bool {
        self.save_failure.is_some()
    }

    /// Test hook: where the document starts under the title and header.
    #[doc(hidden)]
    pub fn body_top_for_test(&self) -> f64 {
        self.rect.y0 + self.editor_top_inset() - self.scroll_y
    }

    /// Test hook: whether the title is in its editing state.
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
        if self.page_id.is_none() {
            return Vec::new();
        }
        let mut children: Vec<&dyn Widget> = vec![&self.title_label];
        if let Some(banner) = &self.save_failure {
            children.push(banner);
        }
        if let Some(header) = self.header.built() {
            children.push(header);
        }
        children.push(&self.editor);
        children
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        if self.page_id.is_none() {
            return Vec::new();
        }
        let mut children: Vec<&mut dyn Widget> = vec![&mut self.title_label];
        if let Some(banner) = self.save_failure.as_mut() {
            children.push(banner);
        }
        if let Some(header) = self.header.built_mut() {
            children.push(header);
        }
        children.push(&mut self.editor);
        children
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
            self.refresh_captions();
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
        // changes how the wrapped tags lay out. That one-shot handoff avoids a
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
                        right: length(num::f32_of_f64(self.metrics.gutter)),
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
        // The header is laid out by the toolkit like any column; only its
        // place in the scrolling document is the panel's: under the pinned
        // title, on the content gutter.
        let top_inset = self.scroll_top_inset();
        let gutter = self.metrics.gutter;
        let banner_top = gutter + self.title_height();
        self.banner_node = self
            .save_failure
            .as_mut()
            .map(|banner| banner.build_layout(taffy, theme, node_map));
        if let Some(node) = self.banner_node {
            pin_under_gutter(taffy, node, gutter, banner_top);
        }
        self.header_node = self
            .header
            .column(theme)
            .map(|header| header.build_layout(taffy, theme, node_map));
        if let Some(node) = self.header_node {
            pin_under_gutter(taffy, node, gutter, top_inset + self.metrics.header_gap);
        }
        let children = if self.page_id.is_some() {
            let mut nodes = vec![title_node];
            nodes.extend(self.banner_node);
            nodes.extend(self.header_node);
            nodes.push(editor_node);
            nodes
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
        let prior_banner = self.banner_height;
        self.measure_banner(taffy);
        let prior_metadata = self.metadata_height;
        self.measure_header(taffy);
        if (self.banner_height - prior_banner).abs() > f64::EPSILON {
            self.tag_layout_dirty = true;
            self.dirty = true;
        }
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
            // The header belongs to the same scrolling document as the
            // blocks: it moves with the offset the caret reveal or clamp just
            // settled.
            if let Some(header_node) = self.header_node
                && let Some(header) = self.header.built_mut()
            {
                header.apply_layout(
                    taffy,
                    header_node,
                    Point::new(self.rect.x0, self.rect.y0 - self.scroll_y),
                );
            }
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
            if let Some(banner) = self.save_failure.as_mut() {
                banner.draw(scene, transform, text_renderer, theme, presence);
            }
            // Once the document has moved under the pinned title, one hairline
            // says where the title stops and the document is cut off.
            if self.scroll_y > f64::EPSILON {
                let pinned = Rect::new(
                    self.rect.x0,
                    self.rect.y0,
                    self.rect.x1,
                    self.rect.y0 + self.scroll_top_inset(),
                );
                ui::widget::toolbar::paint_band(scene, transform, theme, pinned);
            }

            // Everything below the sticky title is one scrolling document:
            // metadata first, then authored blocks. One clip keeps scrolled
            // header buttons from painting through the title.
            let body_clip = self.scroll_viewport();
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &body_clip,
            );
            if let Some(header) = self.header.built_mut() {
                header.draw(scene, transform, text_renderer, theme, presence);
            }
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

        // The tags and incoming links are buttons in the scrolling document:
        // they take the pointer only where the document is visible, and every
        // other event (keys, hover) reaches whichever of them has focus.
        let press = matches!(event, WindowEvent::MouseInput { .. });
        if (!press || self.scroll_viewport().contains(self.last_pointer))
            && let Some(node) = self.header_node
            && let Some(header) = self.header.built_mut()
        {
            header.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                return;
            }
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
        } else if self.page_id.is_none() && self.empty_action_rect.contains(point) {
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
                .with_description("Note title, scroll position")
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.title_rect),
            [1] => self.scroll_thumb(),
            _ => None,
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [1] => self.focus_scrollbar(),
            [0] => {
                self.scroll_focused = false;
                self.title_label.begin_edit();
                self.dirty = true;
                true
            }
            _ => false,
        }
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
        false
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
}
