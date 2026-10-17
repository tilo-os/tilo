//! `MainPanel` — the right-hand pane that renders the currently
//! selected page.
//!
//! The panel owns:
//!
//! - the currently displayed page's id (or `None` if nothing's
//!   selected),
//! - the page's editor: the SDK rich-text editor bound to the page's body,
//!   every edit a [`NotesAction::EditBody`](crate::actions::Action),
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
#[cfg(test)]
use ui::widget::Edits;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::{RichTextField, RichTextSource, RichTextSurface};
use ui::Widget;
use ui::framework::Dispatcher;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{BlockEditor, EditOutcome, EditableLabel, EmptyState, EmptyStateContent};
use ui::widget::{PressOutcome, ScrollMetrics, Scrollbar};

use crate::model::PageId;
use crate::note_header::NoteHeader;
use crate::num;

fn empty_content() -> EmptyStateContent<'static> {
    EmptyStateContent {
        icon: Some(Icon::FileText),
        action_label: Some("New note"),
        ..EmptyStateContent::new("Start your first note")
    }
}

/// The note header's geometry in the theme the tree was built with. The note
/// title is the page's `LargeTitle`: an editable heading pinned above the
/// body, so there is a single clear heading per page. Every measure is a
/// theme token or a measured line, never a pixel size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    /// The page's content gutter, between the panel's edges and its content.
    gutter: f64,
    /// The text column's leading edge: the gutter plus the editor's handle
    /// gutter, so title, metadata and body text share one edge and the drag
    /// handles hang outside it, a full gutter clear of the sidebar.
    text_edge: f64,
    /// Where the body editor starts inside the panel: the text edge less the
    /// editor's own text inset, so a paragraph's first letter stands on the
    /// text edge with the title above it.
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
            text_edge: f64::from(spacing.rhythm.group),
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

/// A line of body text people read comfortably: about seventy characters,
/// measured from a pangram so it follows the font and the text scale.
fn reading_width(theme: &Theme) -> f64 {
    const SAMPLE: &str = "the quick brown fox jumps over the lazy dog";
    const CHARACTERS: u32 = 70;
    let style = theme.text(TextRole::Body);
    let sample = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
        f64::from(text.measure_style(SAMPLE, None, &style).0)
    });
    // `SAMPLE` is ASCII, so its byte length is its character count.
    let per_character = sample / num::f64_of_usize(SAMPLE.len());
    (per_character * f64::from(CHARACTERS)).max(f64::from(theme.tokens.metrics.chooser_width))
}

/// Place a child of the panel at `top`, from the text edge to the trailing
/// gutter and as tall as its own content. The panel positions these by
/// hand: they are absolute, so their height never sizes the panel.
fn pin_under_gutter(
    taffy: &mut TaffyTree<()>,
    node: NodeId,
    metrics: &Metrics,
    side: f64,
    top: f64,
) {
    let Ok(existing) = taffy.style(node).cloned() else {
        return;
    };
    let _ = taffy.set_style(
        node,
        Style {
            position: taffy::Position::Absolute,
            size: Size::auto(),
            inset: taffy::Rect {
                left: length(num::f32_of_f64(metrics.text_edge + side)),
                right: length(num::f32_of_f64(metrics.gutter + side)),
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

/// A page's body in the SDK rich-text editor, edited as the notebook's
/// actions.
pub type NotesEditor = RichTextSurface<PageBody>;

/// A page's body: the notebook's rich-text child resource, edited through
/// its dispatcher once the app is hosted by the framework.
pub struct PageBody {
    field: RichTextField,
    page: Option<PageId>,
}

impl PageBody {
    fn attach(&mut self, notebook: &Dispatcher<crate::resource::Notebook>) {
        if let Some(page) = self.page {
            self.field.attach(body_dispatcher(notebook, page));
        }
    }
}

/// The dispatcher `page`'s body is edited through: each rich-text edit is a
/// field edit of the notebook at the body's path.
fn body_dispatcher(
    notebook: &Dispatcher<crate::resource::Notebook>,
    page: PageId,
) -> Dispatcher<tilo_document::RichText> {
    notebook.child(&crate::actions::page_resource_id(page), move |op| {
        crate::actions::edit_body_action(page, &op)
    })
}

impl RichTextSource for PageBody {
    fn read_only(&self) -> bool {
        self.field.read_only()
    }

    fn sync(&mut self, doc: &AutoCommit) {
        self.field.sync(doc);
    }

    fn revision(&self) -> Option<u64> {
        self.field.revision()
    }

    fn read_from(&mut self, doc: &AutoCommit) -> bool {
        self.field.read_from(doc)
    }

    fn with_state<T>(&self, read: impl FnOnce(&tilo_document::RichTextState) -> T) -> Option<T> {
        self.field.with_state(read)
    }

    fn apply(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        edit: &tilo_document::RichTextAction,
    ) -> Result<(), ui::tilo_actions::ActionError> {
        self.field.apply(edits, edit)
    }

    fn presence_keys(&self) -> Option<ui::widget::PresenceKeyFn> {
        self.field.presence_keys()
    }
}

/// Notes is a prose editor, so ordinary blocks must reflow to the available
/// column width. Keeping construction behind one helper prevents a page bind,
/// remote rebind, or empty-state reset from silently returning to the shared
/// editor's single-line (code/editor) default. `page` is the page whose body
/// it edits, through the notebook's dispatcher; without a page or a
/// dispatcher it shows what it is given and writes nothing.
fn notes_editor(
    page: Option<(PageId, ObjId)>,
    notebook: Option<&Dispatcher<crate::resource::Notebook>>,
) -> NotesEditor {
    let body_object = page.as_ref().map(|(_, body)| body.clone());
    let mut body = PageBody {
        field: RichTextField::view_only(move |doc| {
            body_object
                .clone()
                .filter(|body| tilo_doc::model::open(doc, body).is_ok())
        }),
        page: page.map(|(page, _)| page),
    };
    if let Some(notebook) = notebook {
        body.attach(notebook);
    }
    RichTextSurface::over(body).with_editor(
        BlockEditor::new()
            .with_wrap_lines(true)
            .with_compact_paragraph_spacing(true),
    )
}

/// Where a frame shows the note's body: its scroll offset, the clip it
/// paints into, the thumb's track with its reach, the pinned title's rule
/// with its reach when it is shown, the transform, and what the blocks
/// paint for where the pointer is.
#[derive(Clone, Debug, PartialEq)]
struct BodyShown {
    scroll_y: f64,
    clip: Rect,
    track: Rect,
    rule: Option<Rect>,
    transform: Affine,
    /// What the blocks paint for the pointer rather than the text (the
    /// drag handle of the block under it, a code run's lit copy chrome, a
    /// table's add controls, an image's button): it stays with the pointer
    /// while the text moves.
    pointer_chrome: Vec<Rect>,
}

/// A scroll of the note's body that a host keeping its last frame may copy
/// (`ui::scroll_copy`): where the body paints and how far it moved, what
/// paints over it without moving (the thumb's track), and the damage beside
/// the copy (the track, and the pinned title's rule where it came or went).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScrollCopy {
    clip: Rect,
    delta: Vec2,
    track: Rect,
    transform: Affine,
    pub(crate) damage: Vec<Rect>,
}

impl ScrollCopy {
    /// Offer the copy to the host collecting damage, as `panel`. `false`
    /// when the host does not copy: the body is then damage.
    pub(crate) fn offer(&self, panel: &MainPanel) -> bool {
        ui::scroll_copy::offer(
            panel,
            self.clip,
            self.delta,
            vec![self.track],
            self.transform,
        )
    }

    /// Where the body paints: what to damage when the copy is not made.
    pub(crate) fn body(&self) -> Rect {
        self.clip
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the local focus, hover, empty-state and layout flags independently track retained widget lifecycles; grouping them would only hide rather than simplify that state"
)]
pub struct MainPanel {
    header: crate::note_header::NoteHeader,
    pending_backlink_open: Option<PageId>,
    header_node: Option<NodeId>,
    metadata_height: f64,
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
    /// The open title edit names a note just made: leaving it from the
    /// keyboard puts the caret at the start of the body.
    naming_new: bool,
    /// Durable tags assigned to the open page, rendered as quiet chips. The
    /// selection/filter around them belongs to the app and is intentionally
    /// local; this view only receives the document-derived labels.
    /// Exact hit geometry for the wrapped tag chips. This is computed during
    /// layout, shared by pointer + semantic activation, and deliberately
    /// avoids making the title's entire metadata band accidentally editable.
    /// The height reserved for wrapped tag rows. It is recalculated whenever
    /// width, text scale, or membership changes, so large text and long tag
    /// names never collide with the first document block.
    /// A tag chip is a local navigation control. The app opens the tag it
    /// names, checking the tag still exists, before changing the local browse
    /// view.
    pending_tag_open: Option<crate::model::TagId>,
    /// Set while a new chip height is being discovered so the host rebuilds
    /// the absolute editor inset on the immediately following layout pass.
    tag_layout_dirty: bool,
    /// How far the text column stands in from each gutter, as the last
    /// layout found it: a wide window shows one centred column of the
    /// reading measure rather than lines as wide as the screen.
    column_side: f64,
    /// The widest the text column grows: about seventy characters of body
    /// text, measured in the theme the tree was built with.
    reading_width: f64,
    /// Live editor. Always present (defaults to an empty document)
    /// so layout/draw don't need a `None` branch.
    editor: NotesEditor,
    /// The notebook's dispatcher, once the app is hosted: page bodies are
    /// edited through it.
    notebook: Option<Dispatcher<crate::resource::Notebook>>,
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
    paint_damage: ui::widget::paint_damage::PaintDamage,
    /// What this draw's title painted past its band (a correction chip),
    /// and what the last drawn frame's did.
    title_overhang: Option<Rect>,
    drawn_title_overhang: Option<Rect>,
    /// Vertical scroll offset for the panel body (in CSS-like px).
    /// Bumped by mouse-wheel events; clamped each frame to the
    /// content overflow extent in `apply_layout`. We hand-roll this
    /// rather than wrapping in `ui::widget::Scrollable` because the
    /// panel owns its sub-tree (title + editor) as struct fields, not
    /// as boxed Elements — wrapping in Scrollable would require
    /// significant ownership churn for what amounts to "clip + shift".
    scroll_y: f64,
    /// When the block drag last scrolled the page, for its speed.
    drag_scroll_tick: Option<ui::time::Instant>,
    scroll_content_height: f64,
    bottom_inset: f64,
    scroll_track: Rect,
    scrollbar: Scrollbar,
    scroll_focused: bool,
    scroll_layout_dirty: bool,
    /// A scroll moved the body since the last frame drawn. Unlike `dirty`,
    /// it may leave every pixel but the body's to a copy the host makes of
    /// what stays on screen (`ui::scroll_copy`).
    scroll_moved: bool,
    /// Where the frame being drawn shows the body, and where the last frame
    /// the host presented did.
    body_drawing: Option<BodyShown>,
    body_presented: Option<BodyShown>,
    /// Follow an explicit reveal or keyboard edit once layout has fresh caret geometry.
    follow_caret: bool,
    /// The block the caret goes to (at its start) when the open page binds
    /// again, instead of where it was: the line an edit applied as an action
    /// (a pasted picture) left for typing to go on.
    caret_on_rebind: Option<String>,
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
                .with_placeholder(ui::i18n!("notes-title-placeholder"))
                .with_id("note_title")
                .with_accessible_name("Page title"),
            title_rect: Rect::ZERO,
            last_pointer: Point::ZERO,
            pending_rename: None,
            naming_new: false,
            pending_tag_open: None,
            header: NoteHeader::default(),
            header_node: None,
            metadata_height: 0.0,
            tag_layout_dirty: false,
            column_side: 0.0,
            reading_width: f64::INFINITY,
            pending_backlink_open: None,
            editor: notes_editor(None, None),
            notebook: None,
            image_store: None,
            body: String::new(),
            rect: Rect::ZERO,
            editor_node: None,
            title_node: None,
            dirty: true,
            paint_damage: ui::widget::paint_damage::PaintDamage::default(),
            title_overhang: None,
            drawn_title_overhang: None,
            scroll_y: 0.0,
            drag_scroll_tick: None,
            scroll_content_height: 0.0,
            bottom_inset: 0.0,
            scroll_track: Rect::ZERO,
            scrollbar: Scrollbar::vertical(),
            scroll_focused: false,
            scroll_layout_dirty: false,
            scroll_moved: false,
            body_drawing: None,
            body_presented: None,
            follow_caret: false,
            caret_on_rebind: None,
            local_author: None,
            empty_action_rect: Rect::ZERO,
            empty_action_requested: false,
            empty_action_focused: false,
            metrics: Metrics::default(),
            layout_theme: Theme::dark(),
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
        self.naming_new = false;
        self.dirty = true;
    }

    /// End naming a new note: Enter or Escape goes on to the start of its
    /// body; a click away leaves the caret where the click puts it.
    fn finish_naming(&mut self, event: &WindowEvent) {
        if std::mem::take(&mut self.naming_new)
            && matches!(event, WindowEvent::KeyboardInput { .. })
        {
            self.editor
                .editor_mut()
                .focus_block(0, ui::widget::CursorPlacement::Start);
        }
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
        let body = crate::notebook::body_object(doc, page).ok_or("the page has no body")?;
        // Binding the page already open again (its record re-read, a reveal)
        // keeps the caret where the person left it: a rebind that put it back
        // at the start would make the next keystroke land before the last.
        let same_page = self.page_id == Some(page);
        let moved = self.caret_on_rebind.take().filter(|_| same_page);
        let caret = same_page
            .then(|| {
                if let Some(id) = moved {
                    return Some((id, 0));
                }
                let editor = self.editor.editor();
                let block = editor.blocks().get(editor.focused_index()?)?;
                Some((block.id.clone(), block.line.cursor_pos()))
            })
            .flatten();
        let mut editor = self.fresh_editor(Some((page, body)));
        editor
            .editor_mut()
            .set_local_author(self.local_author.clone());
        editor.present_from(doc);
        editor.set_focus(true);
        let (index, placement) = caret
            .and_then(|(id, byte)| {
                let index = editor
                    .editor()
                    .blocks()
                    .iter()
                    .position(|block| block.id == id)?;
                Some((index, ui::widget::CursorPlacement::Byte(byte)))
            })
            .unwrap_or((0, ui::widget::CursorPlacement::Start));
        editor.editor_mut().focus_block(index, placement);
        self.editor = editor;
        self.page_id = Some(page);
        self.dates = self.date_index.get(page);
        self.refresh_captions();
        self.empty_action_focused = false;
        self.set_title_value(title);
        self.body.clear();
        // Another page starts at its top; the same page, bound again after an
        // edit (a paste, an import), stays where the person is reading, and
        // layout clamps the offset to the new extent.
        if same_page {
            self.scroll_layout_dirty = true;
        } else {
            self.scroll_y = 0.0;
        }
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn project_canonical(
        &self,
        doc: &AutoCommit,
        workspace: &crate::Workspace,
    ) -> Result<Self, String> {
        let mut panel = Self::new();
        panel.notebook.clone_from(&self.notebook);
        // The notes' dates come from the document's whole history: the new
        // panel goes on from where this one read it rather than reading
        // every change again.
        panel.date_index.clone_from(&self.date_index);
        if let Some(actions) = self.header.actions() {
            panel.header.attach(actions);
        }
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
    /// The title's layout node, the keyboard's target while it is edited.
    pub(crate) fn title_node(&self) -> Option<NodeId> {
        self.title_node
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

    /// Give an explicitly opened note its body focus without discarding a
    /// pending title edit or moving the existing caret.
    pub(crate) fn focus_note_body(&mut self) {
        if self.title_label.is_editing() {
            self.title_label.commit();
            if let EditOutcome::Committed(title) = self.title_label.take_outcome() {
                self.pending_rename = Some(title);
            }
        }
        self.editor.set_focus(true);
        self.scroll_focused = false;
        self.dirty = true;
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
        self.metadata_height = 0.0;
        self.editor = self.fresh_editor(None);
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

    /// Replace the incoming-link projection after the app's bounded canonical
    /// index advances. The source-page id, not the visible title, is retained
    /// for activation so a remote rename/deletion cannot misroute a click.
    pub fn set_backlinks(&mut self, backlinks: Vec<BacklinkChip>) {
        if self.header.set_backlinks(backlinks) {
            self.header_changed();
        }
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
        let lines = self.caption_lines();
        if self.header.set_captions(lines) {
            self.header_changed();
        }
    }

    fn caption_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.parent_path.is_empty() {
            lines.push(self.parent_path_label());
        }
        lines.extend(self.dates.labels());
        lines
    }

    pub fn set_transclusion_previews(
        &mut self,
        previews: HashMap<PageId, ui::widget::TransclusionPreview>,
    ) {
        self.editor.editor_mut().set_transclusion_previews(previews);
    }

    /// The store the editor draws images from.
    pub(crate) fn image_store(&self) -> Option<&ui::ImageStore> {
        self.image_store.as_ref()
    }

    pub fn set_image_store(&mut self, store: Option<ui::ImageStore>) {
        self.image_store.clone_from(&store);
        self.editor.editor_mut().set_image_store(store);
    }

    /// Edit page bodies through `notebook` from now on, the one shown
    /// included.
    /// What the header's controls invoke, once attached.
    #[doc(hidden)]
    pub fn actions(&self) -> Option<ui::framework::Actions<crate::resource::Notebook>> {
        self.header.actions()
    }

    /// Bind the header's controls to the notebook window's effects, for a
    /// panel shown on its own.
    #[doc(hidden)]
    pub fn attach_actions_for_test(
        &mut self,
        actions: ui::framework::Actions<crate::resource::Notebook>,
    ) {
        self.header.attach(actions);
    }

    pub(crate) fn attach_notebook(
        &mut self,
        notebook: Dispatcher<crate::resource::Notebook>,
        actions: ui::framework::Actions<crate::resource::Notebook>,
    ) {
        self.editor.source_mut().attach(&notebook);
        self.notebook = Some(notebook);
        self.header.attach(actions);
    }

    fn fresh_editor(&self, page: Option<(PageId, ObjId)>) -> NotesEditor {
        let mut editor = notes_editor(page, self.notebook.as_ref());
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

    /// Images pasted into the open note since the last call.
    pub fn take_pasted_images(&mut self) -> Vec<ui::widget::block_editor::ImagePasteRequest> {
        self.editor.editor_mut().take_image_paste_requests()
    }

    pub fn set_bottom_inset(&mut self, inset: f64) {
        let inset = inset.max(0.0);
        if (self.bottom_inset - inset).abs() > f64::EPSILON {
            self.bottom_inset = inset;
            self.dirty = true;
        }
    }

    fn body_bottom_inset(&self) -> f64 {
        self.metrics.gutter.max(self.bottom_inset)
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
            self.scroll_moved = true;
        }
    }

    /// The copy a host may make of this frame (after its draw): when only a
    /// scroll moved the body since the last presented frame, with the same
    /// clip, track and transform, and nothing in the body, its header or
    /// over it changed besides.
    pub(crate) fn scroll_copy(&self) -> Option<ScrollCopy> {
        let (drawing, presented) = (self.body_drawing.as_ref()?, self.body_presented.as_ref()?);
        // What a block paints for the pointer rather than the text is
        // repainted where it is and was (`pointer_chrome`); anything else
        // that changed in the body is the editor's own redraw.
        let quiet = self.scroll_moved
            && !self.dirty
            && !self.editor.needs_redraw()
            && !self.has_editor_popup()
            && self
                .header
                .built()
                .is_none_or(|header| !header.needs_redraw());
        (quiet
            && drawing.clip == presented.clip
            && drawing.track == presented.track
            && drawing.transform == presented.transform
            && (drawing.scroll_y - presented.scroll_y).abs() > f64::EPSILON)
            .then(|| ScrollCopy {
                clip: drawing.clip,
                delta: Vec2::new(0.0, presented.scroll_y - drawing.scroll_y),
                track: drawing.track,
                transform: drawing.transform,
                // The thumb moved within its track, the pinned title's rule
                // came or went, and what the blocks paint for the pointer is
                // where it is and no longer where it was (the host carries
                // that).
                damage: std::iter::once(drawing.track)
                    .chain(
                        (drawing.rule != presented.rule)
                            .then(|| drawing.rule.into_iter().chain(presented.rule))
                            .into_iter()
                            .flatten(),
                    )
                    .chain(drawing.pointer_chrome.iter().copied())
                    .chain(presented.pointer_chrome.iter().copied())
                    .collect(),
            })
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
            self.naming_new = false;
            self.dirty = true;
        }
    }

    /// Name a note just made: its title opens empty, the placeholder showing,
    /// so what is typed names it, and Enter goes on to the start of its body.
    pub fn begin_new_title_edit(&mut self) {
        if self.page_id.is_some() {
            self.editor.set_focus(false);
            self.title_label.begin_blank_edit();
            self.naming_new = true;
            self.dirty = true;
        }
    }

    /// How far the text column stands in from each gutter at panel `width`:
    /// none until the text would run past the reading measure, then whatever
    /// keeps the column at that measure, centred.
    fn column_side_for(&self, width: f64) -> f64 {
        let text = (width - self.metrics.text_edge - self.metrics.gutter).max(0.0);
        ((text - self.reading_width) / 2.0).max(0.0).floor()
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

    /// The document starts under the pinned title.
    fn scroll_top_inset(&self) -> f64 {
        self.metrics.gutter + self.title_height()
    }

    /// Whether a block drag is in flight and needs a frame to keep scrolling.
    pub fn block_drag_scrolling(&self) -> bool {
        self.editor.editor().block_drag_active()
    }

    /// Scroll the page while a dragged block's pointer rests near the edge
    /// of what is visible, so a target below the fold can be reached.
    /// Returns whether the page moved.
    pub fn autoscroll_block_drag(&mut self) -> bool {
        let now = ui::time::now();
        if !self.editor.editor().block_drag_active() {
            self.drag_scroll_tick = None;
            return false;
        }
        let elapsed = self
            .drag_scroll_tick
            .replace(now)
            .map_or(std::time::Duration::ZERO, |last| {
                now.saturating_duration_since(last)
            });
        let viewport = self.scroll_viewport();
        let delta = self.editor.editor_mut().tick_block_drag(viewport, elapsed);
        if delta == 0.0 {
            return false;
        }
        let before = self.scroll_y;
        self.set_scroll_offset(before + delta);
        (self.scroll_y - before).abs() > f64::EPSILON
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

    pub(crate) fn editor_route_available(&self) -> bool {
        self.editor_node.is_some()
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

    /// Put the caret at the start of block `id` when the open page binds
    /// again after an edit applied as an action, and keep it in view.
    pub fn put_caret_on_rebind(&mut self, id: String) {
        self.caret_on_rebind = Some(id);
        self.follow_caret = true;
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

    /// Test hook: why a wheel over the body might not scroll it: the
    /// offset and how far it can go, where the panel last saw the pointer
    /// against the body's viewport, and what holds the input instead.
    #[doc(hidden)]
    pub fn scroll_state_for_test(&self) -> String {
        let blocks = self.editor.editor().blocks();
        format!(
            "blocks={} measured={} content_bottom={} editor={:?} content_h={} \
             scroll_y={} max={} pointer={:?} viewport={:?} popup={} title_editing={} rect={:?}",
            blocks.len(),
            blocks
                .iter()
                .filter(|block| block.line.get_rect().height() > 0.0)
                .count(),
            self.editor.editor().content_bottom(),
            self.editor.get_rect(),
            self.scroll_content_height,
            self.scroll_y,
            self.scroll_metrics().max_scroll(),
            self.last_pointer,
            self.scroll_viewport(),
            self.has_editor_popup(),
            self.title_label.is_editing(),
            self.rect,
        )
    }

    /// Test hook: how far the body can scroll, as the last layout measured
    /// it.
    #[doc(hidden)]
    pub fn max_scroll_for_test(&self) -> f64 {
        self.scroll_metrics().max_scroll()
    }

    /// Test hook: the panel's current vertical scroll offset in px.
    /// Lets integration tests assert wheel scrolling + the
    /// `apply_layout` overflow clamp through the real event path.
    #[doc(hidden)]
    pub fn scroll_y_for_test(&self) -> f64 {
        self.scroll_y
    }

    /// Test hook: the dates the header shows for the open note.
    #[doc(hidden)]
    pub fn dates_for_test(&self) -> crate::metadata::NoteDates {
        self.dates
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

    /// Test hook: what the title's field holds while it is edited.
    #[doc(hidden)]
    pub fn title_draft_for_test(&self) -> String {
        self.title_label.current_text().to_owned()
    }

    /// Test hook: centre of the first-note call to action after a layout pass.
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
    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        action.apply_to(self, Self::primary_vertical_scroll, |panel, _, next| {
            panel.set_scroll_offset(next);
        })
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.page_id.is_none() {
            return Vec::new();
        }
        let mut children: Vec<&dyn Widget> = vec![&self.title_label];
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
        if let Some(header) = self.header.built_mut() {
            children.push(header);
        }
        children.push(&mut self.editor);
        children
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        ui::tilo_resource::profiling::measure("note_dates", || self.date_index.sync(state));
        let dates = self
            .page_id
            .map(|page| self.date_index.get(page))
            .unwrap_or_default();
        if self.dates != dates {
            self.dates = dates;
            // The edit time turning over a minute changes a line's words, not
            // the header's widgets: rebuilding them would repaint the window.
            let lines = self.caption_lines();
            if !self.header.retext_captions(&lines) {
                self.refresh_captions();
            }
            self.dirty = true;
        }
        self.header.sync_built(state, path);
        self.editor.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Metrics::of(theme);
        // A note is a block document: its drag handles hang in the editor's
        // handle gutter, outside the text column and inside the page gutter.
        // Every block starts its content at the editor's text inset, so the
        // body starts that much short of the text edge and text, tables,
        // code and embeds stand on it with the title.
        let editor = self.editor.editor();
        self.metrics.text_edge = self.metrics.gutter + editor.handle_gutter(theme);
        self.metrics.body_inset = (self.metrics.text_edge - editor.text_inset(theme)).max(0.0);
        self.layout_theme = theme.clone();
        self.reading_width = reading_width(theme);
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
                        left: length(num::f32_of_f64(self.metrics.body_inset + self.column_side)),
                        right: length(num::f32_of_f64(self.metrics.gutter + self.column_side)),
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
        // title, on the text edge.
        let top_inset = self.scroll_top_inset();
        self.header_node = self
            .header
            .column(theme)
            .map(|header| header.build_layout(taffy, theme, node_map));
        if let Some(node) = self.header_node {
            pin_under_gutter(
                taffy,
                node,
                &self.metrics,
                self.column_side,
                top_inset + self.metrics.header_gap,
            );
        }
        let children = if self.page_id.is_some() {
            let mut nodes = vec![title_node];
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
        let style = Style {
            size: Size::auto(),
            overflow: taffy::Point {
                x: taffy::Overflow::Hidden,
                y: taffy::Overflow::Hidden,
            },
            flex_grow: 1.0,
            flex_basis: Dimension::length(0.0),
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
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
        let new_rect = ui::widget::placed(layout, origin);
        if (new_rect.width() - self.rect.width()).abs() > f64::EPSILON
            || (new_rect.height() - self.rect.height()).abs() > f64::EPSILON
        {
            self.dirty = true;
        }
        self.rect = new_rect;
        // Input can arrive before the first paint, including after a resize.
        self.empty_action_rect = if self.page_id.is_none() {
            ui::text_render::with_layout_renderer(self.layout_theme.text_scale, |text| {
                EmptyState::layout(text, &self.layout_theme, self.rect, &empty_content())
                    .action
                    .unwrap_or(Rect::ZERO)
            })
        } else {
            Rect::ZERO
        };
        // A title that does not fit its line steps down one role instead of
        // eliding, so a phone at 200% text still reads the whole name. This
        // is responsive type, not cancellation of the text scale.
        // The editor and header were built at the last width's column; a
        // new width that moves the column builds them again at once.
        let side = self.column_side_for(self.rect.width());
        if (side - self.column_side).abs() > 0.5 {
            self.column_side = side;
            self.tag_layout_dirty = true;
            self.dirty = true;
        }
        let gutter = self.metrics.gutter + side;
        let text_edge = self.metrics.text_edge + side;
        let line_width = (self.rect.width() - text_edge - gutter).max(0.0);
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
            self.rect.x0 + text_edge,
            self.rect.y0 + self.metrics.gutter,
            (self.rect.x1 - gutter).max(self.rect.x0 + text_edge),
            self.rect.y0 + self.metrics.gutter + self.title_height(),
        );
        self.title_label.apply_layout_external(self.title_rect);
        let prior_metadata = self.metadata_height;
        self.measure_header(taffy);
        if (self.metadata_height - prior_metadata).abs() > f64::EPSILON {
            self.tag_layout_dirty = true;
            self.dirty = true;
        }
        // An image preview takes at most a share of what the page shows at
        // once; a new window height measures the images again.
        let visible = self.scroll_viewport().height();
        if self.editor.editor_mut().set_viewport_height(Some(visible)) {
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
                self.metadata_height + editor_content_h + self.body_bottom_inset();
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
                    && let Some(caret) = self.editor.editor().document_caret_rect()
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
                    // A caret revealed downward keeps the page's own bottom
                    // padding under it, the room the last line has at the
                    // end of the note: a new line never lands flush against
                    // the window's edge or under what floats over it. A
                    // viewport too short for that keeps the caret itself.
                    let margin = self
                        .body_bottom_inset()
                        .min((viewport_h - caret.height()).max(0.0));
                    if at_document_start {
                        self.scroll_y = 0.0;
                    } else if caret.y0 - self.scroll_y < viewport_top {
                        self.scroll_y = caret.y0 - viewport_top;
                    } else if caret.y1 + margin - self.scroll_y > viewport_bottom {
                        self.scroll_y = caret.y1 + margin - viewport_bottom;
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
            // A drag at the edge scrolled the blocks under the pointer.
            self.editor.editor_mut().retarget_block_drag();
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
        let incoming = std::mem::take(scene);
        // No ground: the window paints `surfaces.content` under the panel.
        if let Some(page_id) = self.page_id {
            // Prominent, editable page title, drawn OUTSIDE the scroll clip so
            // it stays pinned while the body scrolls under it.
            // Its own part: naming the note repaints the title, not the page.
            self.title_label.apply_layout_external(self.title_rect);
            let mut title_scene = Scene::new();
            self.title_label
                .draw(&mut title_scene, transform, text_renderer, theme, presence);
            // Once the document has moved under the pinned title, one hairline
            // says where the title stops and the document is cut off.
            let pinned = Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x1,
                self.rect.y0 + self.scroll_top_inset(),
            );
            if self.scroll_y > f64::EPSILON {
                ui::widget::toolbar::paint_band(scene, transform, theme, pinned);
            }

            // Everything below the sticky title is one scrolling document:
            // metadata first, then authored blocks. One clip keeps scrolled
            // header buttons from painting through the title.
            let body_clip = self.scroll_viewport();
            // The thumb and its focus ring stay within a spacing step of the
            // track.
            let track_reach = f64::from(theme.tokens.spacing.s2);
            self.body_drawing = Some(BodyShown {
                scroll_y: self.scroll_y,
                clip: body_clip,
                track: self.scroll_track.inflate(track_reach, track_reach),
                rule: (self.scroll_y > f64::EPSILON)
                    .then(|| Rect::new(pinned.x0, pinned.y1 - track_reach, pinned.x1, pinned.y1)),
                transform: ui::scroll_copy::on_screen(transform),
                pointer_chrome: Vec::new(),
            });
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &body_clip,
            );
            // Metadata can change while typing crosses a clock boundary.
            // Keep its actual draw separate so that repainting the date does
            // not invalidate every paragraph in the scrolling document.
            let mut parts = Vec::new();
            let mut outside = scene.clone();
            if let Some(header) = self.header.built_mut() {
                let mut header_scene = Scene::new();
                header.draw(&mut header_scene, transform, text_renderer, theme, presence);
                scene.append(&header_scene, None);
                let rect = header.get_rect();
                parts.push(ui::widget::paint_damage::PaintPart::new(
                    "note-header".into(),
                    rect,
                    header_scene,
                    vec![rect.inflate(
                        f64::from(theme.tokens.spacing.s2),
                        f64::from(theme.tokens.spacing.s2),
                    )],
                ));
            }
            let mut editor_scene = Scene::new();
            self.editor
                .draw(&mut editor_scene, transform, text_renderer, theme, presence);
            let pointer_chrome = self.editor.editor().pointer_chrome();
            if let Some(body) = &mut self.body_drawing {
                body.pointer_chrome = pointer_chrome;
            }
            scene.append(&editor_scene, None);
            scene.pop_layer();
            outside.pop_layer();
            let mut complete = std::mem::take(scene);
            // Its own part: a paragraph that came or went moves the thumb of
            // a long note, which repaints the track, not the page.
            let mut scrollbar_scene = Scene::new();
            if self.scroll_thumb().is_some() {
                self.scrollbar.draw(
                    &mut scrollbar_scene,
                    transform,
                    self.scroll_track,
                    self.scroll_metrics(),
                    theme,
                    self.scroll_focused,
                );
            }
            complete.append(&scrollbar_scene, None);
            // Overlay popovers (slash menu / link popover) — drawn AFTER the
            // main editor scene so they land on top of every block, and
            // OUTSIDE the scroll clip so they stay visible at their anchor.
            self.editor
                .editor_mut()
                .draw_overlay(scene, transform, text_renderer, theme);
            let suffix = std::mem::take(scene);
            complete.append(&suffix, None);
            outside.append(&suffix, None);
            scene.append(&title_scene, None);
            scene.append(&complete, None);
            let mut editor_damage = Vec::new();
            self.editor.collect_damage(&mut editor_damage);
            // The editor's top edge and width, not its height: a paragraph
            // that came or went grows or shrinks it, and its damage says
            // where its blocks were and are.
            let editor_rect = self.editor.get_rect();
            parts.push(ui::widget::paint_damage::PaintPart::new(
                page_id.to_string(),
                Rect::new(
                    editor_rect.x0,
                    editor_rect.y0,
                    editor_rect.x1,
                    editor_rect.y0 + 1.0,
                ),
                editor_scene,
                editor_damage,
            ));
            parts.push(ui::widget::paint_damage::PaintPart::new(
                "note-scrollbar".into(),
                self.scroll_track,
                scrollbar_scene,
                vec![self.scroll_track.inflate(track_reach, track_reach)],
            ));
            // The field's caret, selection and focus ring stay within a
            // spacing step of its band.
            let title_reach = f64::from(theme.tokens.spacing.s2);
            // A correction chip the field floats past its band, where it is
            // and where it was.
            self.title_overhang = self.title_label.overhang();
            let mut title_damage = vec![self.title_rect.inflate(title_reach, title_reach)];
            title_damage.extend(self.title_overhang);
            title_damage.extend(self.drawn_title_overhang);
            parts.push(ui::widget::paint_damage::PaintPart::new(
                "note-title".into(),
                self.title_rect,
                title_scene,
                title_damage,
            ));
            self.paint_damage.record(
                self.paint_extent(),
                Some(ui::widget::paint_damage::PaintFrame::new(
                    vec![self.rect, self.title_rect, body_clip, self.scroll_track],
                    transform,
                    theme,
                    outside,
                    parts,
                )),
            );
        } else {
            self.title_overhang = None;
            self.body_drawing = None;
            self.paint_damage.record(self.rect, None);
            // No note open: a composed zero-state rather than a bare line —
            // this is what a fresh, unseeded workspace shows, so it should
            // invite the first note rather than look broken.
            EmptyState::paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                self.rect,
                &empty_content(),
                self.last_pointer,
            );
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
        let own = std::mem::replace(scene, incoming);
        scene.append(&own, None);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event boundary keeps pointer, keyboard, empty-state and editor routing ordered around one handled flag"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Track the pointer so a press (which carries no position) can be
        // classified as landing in the title band.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_pointer = ctx.logical(position);
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
                    self.finish_naming(event);
                    self.dirty = true;
                }
                EditOutcome::Cancelled => {
                    self.editor.set_focus(true);
                    self.finish_naming(event);
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
                    *ctx.handled = true;
                    // This event belongs to the viewport. Lending it to the
                    // rich-text editor snapshots and compares every block as
                    // though it might be an edit, even though it only scrolled.
                    return;
                }
            }
        }
        if let Some(n) = self.editor_node {
            let _profile = ui::tilo_resource::profiling::Phase::start("rich_text_input");
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
        // Header controls keep their toolkit hit geometry and hints. The
        // panel supplies the exact persisted times behind its compact caption.
        fn header_hint(
            widget: &dyn Widget,
            point: Point,
            date_id: &str,
            dates: crate::metadata::NoteDates,
        ) -> Option<(Rect, String)> {
            if widget.test_id() == Some(date_id) && widget.get_rect().contains(point) {
                return Some((widget.get_rect(), dates.display_labels(false).join("\n")));
            }
            widget.tooltip_target_at(point).or_else(|| {
                widget
                    .children()
                    .into_iter()
                    .find_map(|child| header_hint(child, point, date_id, dates))
            })
        }
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
        let date_id = format!(
            "notes.caption:{}",
            usize::from(!self.parent_path.is_empty())
        );
        self.header
            .built()
            .and_then(|header| header_hint(header, point, &date_id, self.dates))
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
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.empty_action_focused),
                );
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
        _key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
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
        state: &mut ui::widget::Edits<'_>,
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
            return self.perform_action(action, &[], state, path);
        }
        if target == "notes.page-title" {
            return Widget::perform_action(&mut self.title_label, action, &[], state, path);
        }
        false
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.scroll_moved || self.editor.needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        // A scroll that moved the body and changed nothing else: a host that
        // keeps the last frame copies what stays on screen.
        if let Some(copy) = self.scroll_copy()
            && copy.offer(self)
        {
            regions.extend(copy.damage);
            return;
        }
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.paint_extent(), regions);
        }
    }

    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for child in self.children_mut() {
            child.release_scene_cache();
        }
    }

    fn mark_drawn(&mut self) {
        // Not drawn this frame, what the screen shows of the body is not
        // known: the next scroll repaints it before copies resume.
        self.body_presented = self.body_drawing.take();
        self.scroll_moved = false;
        if self.paint_damage.is_recorded() {
            self.drawn_title_overhang = self.title_overhang;
        }
        self.paint_damage.mark_drawn();
        self.dirty = false;
        self.editor.mark_drawn();
        // The header's widgets learn their paint landed too, so they say
        // when they change rather than on every frame.
        if let Some(header) = self.header.built_mut() {
            header.mark_drawn();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }

    /// The header rebuilt its widgets: the host rebuilds its tree, but the
    /// panel's damage describes it (the header is a part of its own, and a
    /// header that grew or shrank moves the parts below it, which repaints
    /// the panel). The editor's flag (a split or merge changed the block
    /// count) rebuilds.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        use ui::widget::StructureChange;
        let header = if std::mem::take(&mut self.tag_layout_dirty) {
            StructureChange::Described
        } else {
            StructureChange::None
        };
        header.join(self.editor.take_structure_change())
    }

    fn is_layout_dirty(&self) -> bool {
        // The editor's leaf nodes have intrinsic sizes (font_size *
        // 1.5) so size-affecting edits (heading promotion) need a
        // re-layout. Be conservative: always claim dirty when the
        // editor reports a redraw need.
        self.scroll_layout_dirty || self.tag_layout_dirty || self.editor.needs_redraw()
    }
}

impl MainPanel {
    /// Everything the last draw painted: the panel, and a correction chip
    /// the title's field floats above it.
    fn paint_extent(&self) -> Rect {
        self.title_overhang
            .map_or(self.rect, |overhang| self.rect.union(overhang))
    }

    /// Whether `event` is a wheel notch that only moves the body: the panel
    /// takes every vertical wheel over the body before the editor sees it
    /// (`handle_event`), so what the note holds (pictures, lists, code,
    /// tables) cannot change what a notch does. Asking the block kinds here
    /// sent every note with anything but prose down the host's full path,
    /// which syncs the whole tree and re-projects every block's resources on
    /// each notch.
    pub(crate) fn view_only_scroll(&self, event: &WindowEvent) -> bool {
        let delta = match event {
            WindowEvent::MouseWheel { delta, .. } => match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 40.0,
                winit::event::MouseScrollDelta::PixelDelta(position) => -position.y,
            },
            _ => return false,
        };
        let target = (self.scroll_y + delta).clamp(0.0, self.scroll_metrics().max_scroll());
        (target - self.scroll_y).abs() > f64::EPSILON
            && self.page_id.is_some()
            && self.scroll_viewport().contains(self.last_pointer)
            && !self.title_label.is_editing()
            && !self.has_editor_popup()
            && self.pending_rename.is_none()
            && self.pending_tag_open.is_none()
            && self.pending_backlink_open.is_none()
    }

    pub(crate) fn has_editor_popup(&self) -> bool {
        self.editor.editor().has_open_popup()
    }

    pub fn take_tag_open(&mut self) -> Option<crate::model::TagId> {
        self.pending_tag_open.take()
    }
    pub fn take_backlink_open(&mut self) -> Option<PageId> {
        self.pending_backlink_open.take()
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
                &mut Edits::lend(&mut doc),
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
            assert!(p.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut Edits::lend(&mut doc),
                &ObjId::Root
            ));
            assert!(p.take_empty_action());
        }

        eprintln!("phase surface_hints_follow_hit_targets_and_reveal_exact_compact_dates");
        {
            use ui::headless::Framed;
            use ui::tilo_actions::Action;
            crate::metadata::register_messages();
            let page = fresh_page_id();
            let tag = fresh_page_id();
            let backlink = fresh_page_id();
            let mut doc = AutoCommit::new();
            crate::actions::create_page_action(page, None, "Launch")
                .apply(&mut doc)
                .unwrap();
            doc.commit_with(
                automerge::transaction::CommitOptions::default().with_time(1_700_000_000),
            );
            crate::actions::edit_body_action(
                page,
                tilo_document::RichTextAction::AppendParagraph {
                    text: "Body".into(),
                },
            )
            .apply(&mut doc)
            .unwrap();
            doc.commit_with(
                automerge::transaction::CommitOptions::default().with_time(1_700_003_600),
            );
            let mut panel = MainPanel::new();
            panel.load_page(page, "Launch", "Body");
            panel.set_tags(vec![TagChip {
                id: tag,
                name: "Research".into(),
            }]);
            panel.set_backlinks(vec![BacklinkChip {
                page: backlink,
                title: "A long source note title".into(),
                references: 1,
            }]);
            let mut panel = Framed::new(ui::State::from_doc(doc), panel, (300, 600), Theme::dark());
            let targets = Framed::harness(&panel).targets();
            let tag_rect = targets[&format!("notes.tag-chip:{tag}")];
            let backlink_rect = targets[&format!("notes.backlink-chip:{backlink}")];
            let date_rect = targets["notes.caption:0"];
            for rect in [panel.title_rect, tag_rect, backlink_rect, date_rect] {
                assert!(
                    !rect.is_zero_area(),
                    "the retained target is laid out: {rect:?}"
                );
                assert!(
                    panel.rect.contains(rect.origin())
                        && panel
                            .rect
                            .contains(Point::new(rect.x1 - 0.1, rect.y1 - 0.1)),
                    "header target fits the panel: {rect:?}"
                );
            }
            assert_eq!(
                panel.cursor_shape(panel.title_rect.center()),
                Some(winit::window::CursorIcon::Text)
            );
            assert!(panel.tooltip_target_at(panel.title_rect.center()).is_some());
            assert!(
                panel
                    .tooltip_target_at(tag_rect.center())
                    .unwrap()
                    .1
                    .contains("Research")
            );
            assert!(
                panel
                    .tooltip_target_at(backlink_rect.center())
                    .unwrap()
                    .1
                    .contains("A long source note title")
            );
            let (target, label) = panel.tooltip_target_at(date_rect.center()).unwrap();
            assert_eq!(target, date_rect);
            assert_eq!(label, panel.dates.display_labels(false).join("\n"));
            assert_eq!(
                label.lines().count(),
                2,
                "both persisted dates are revealed"
            );
            assert!(
                label.lines().all(|line| line.contains(':')),
                "compact metadata exposes each exact time"
            );
            assert!(panel.tooltip_target_at(Point::new(400.0, 400.0)).is_none());
            assert!(panel.tooltip_target_at(Point::new(100.0, 500.0)).is_none());
            panel.suppress_tooltips(true);
            for rect in [panel.title_rect, tag_rect, backlink_rect, date_rect] {
                assert!(panel.tooltip_target_at(rect.center()).is_none());
            }
            panel.suppress_tooltips(false);
            for rect in [panel.title_rect, tag_rect, backlink_rect, date_rect] {
                assert!(panel.tooltip_target_at(rect.center()).is_some());
            }
            panel.begin_title_edit();
            assert!(
                panel.tooltip_target_at(panel.title_rect.center()).is_none(),
                "rename hint must not cover an active title edit"
            );
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

    #[test]
    fn metadata_bands_tags_and_backlinks_clear_the_editor_and_keep_identity() {
        use ui::headless::Framed;
        fn mounted(
            width: u32,
            theme: Theme,
        ) -> (Framed<MainPanel>, Vec<crate::model::TagId>, PageId) {
            let mut panel = MainPanel::new();
            panel.load_page(fresh_page_id(), "Launch", "First body paragraph");
            let tags = ["Release planning", "Customer research", "Accessibility"]
                .into_iter()
                .map(|name| TagChip {
                    id: fresh_page_id(),
                    name: name.into(),
                })
                .collect::<Vec<_>>();
            let ids = tags.iter().map(|t| t.id).collect::<Vec<_>>();
            panel.set_tags(tags);
            panel.set_parent_path("Projects / Launch".into());
            let source = fresh_page_id();
            panel.set_backlinks(vec![BacklinkChip {
                page: source,
                title: "Long-running launch notes".into(),
                references: 2,
            }]);
            (
                Framed::new(ui::State::new(), panel, (width, 700), theme),
                ids,
                source,
            )
        }
        for (width, scale) in [(800, 1.0), (300, 2.0)] {
            let theme = Theme::dark().with_text_scale(scale);
            let (panel, tags, source) = mounted(width, theme.clone());
            let targets = Framed::harness(&panel).targets();
            let chips = tags
                .iter()
                .map(|tag| targets[&format!("notes.tag-chip:{tag}")])
                .collect::<Vec<_>>();
            assert_eq!(chips.len(), 3);
            assert!(
                chips
                    .iter()
                    .all(|r| r.y0 >= panel.title_rect.y1 + f64::from(theme.tokens.spacing.s1)),
                "title and tags have separate bands"
            );
            assert!(
                chips.iter().all(|r| r.y0 >= panel.scroll_viewport().y0),
                "metadata starts inside the scrolling viewport"
            );
            assert!(
                chips
                    .iter()
                    .all(|r| r.x0 >= 0.0 && r.x1 <= f64::from(width)),
                "tags remain inside the panel"
            );
            if width == 300 {
                assert!(chips[1].y0 > chips[0].y0, "long tags wrap");
            }
            let backlink = targets[&format!("notes.backlink-chip:{source}")];
            let editor_top = panel.rect.y0 + panel.editor_top_inset();
            assert!(chips.iter().all(|r| r.y1 <= editor_top));
            assert!(
                backlink.y1 <= editor_top,
                "incoming-link metadata clears the first block"
            );
        }
    }
}
