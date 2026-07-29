//! The document's version history, as a strip under the editing canvas.
//!
//! The editor already round-trips through `qdoc`, so its document *is* the
//! change graph — this pane is the view onto it. [`ui::HistorySource`] turns
//! that graph into rows, the [`HistoryScrubber`] renders them as a draggable
//! track, and dragging the handle scrubs through the document's past.
//!
//! Four affordances, all the scrubber's own:
//!
//! * **Preview** materialises the document as of the selected tick and shows
//!   its text above the strip. Nothing is written; the live document is
//!   untouched and the user can keep scrubbing.
//! * **Restore to this version** rewrites the live document to that state — as
//!   a *forward* change. See [`restore_to`].
//! * **Name / Rename this version** writes a name over the run of changes the
//!   handle is sitting in. Retroactive: the grouping has already drawn the run,
//!   so naming it costs one click and no forethought.
//! * **Start a version / Finish version** is the prospective case — say what you
//!   are about to do, work, close it. It opens a range with no end, which absorbs
//!   every change until it is closed.
//!
//! # Versions on the track
//!
//! The strip does not show one tick per change. A debounced editor produces
//! hundreds of them, and a track of hundreds of ticks is a grey smear you cannot
//! aim at. So [`ui::group_entries`] cuts the change graph into runs, the strip
//! renders those as a labelled band above the track, and the handle snaps to
//! their boundaries.
//!
//! The band shows **top-level** runs only. A name nested inside another name is
//! a legible thing in the activity pane, which has two dimensions to draw it in;
//! the strip has one, and stacking a sub-range on top of its parent would mean
//! two labels over the same pixels. The pane is where nesting is read.
//!
//! The strip lives inside the app's own content area, under the canvas and
//! above the layout bar, alongside the outline pane and the styles panel. It is
//! not window chrome: the compositor owns titlebars and window decoration, and
//! an app must not draw those. What an app owns is its document, and its
//! document's history is document content.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::history_scrubber::{ScrubberGroup, VersionAction};
use ui::widget::{HistoryEntryId, HistoryScrubber};
use ui::{AuthorDirectory, GroupNameRequest, GroupingConfig, HistorySource, Theme};
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

/// Height of the history strip when open, with the version band showing.
///
/// The strip asks the widget rather than hard-coding: it is taller once there is
/// a version band above the track, and a host that reserved the old height would
/// clip the button row.
pub const HISTORY_BAND_H: f64 = 168.0;

/// Height of the preview card drawn above the strip while a past version is
/// selected.
const PREVIEW_CARD_H: f64 = 108.0;

/// Longest preview excerpt rendered. A historical document can be arbitrarily
/// long; the card is a glance, not a second editor.
const PREVIEW_CHARS: usize = 600;

/// What a dispatched event did to the document.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HistoryOutcome {
    /// The document was rewritten to an earlier version. The host must commit
    /// (via `needs_commit()`) so the restore lands on the undo stack.
    pub restored: bool,
    /// A version was named, renamed, un-named, opened or closed. Also needs a
    /// commit: a name is a document write like any other, and collaborators only
    /// see it once it is committed.
    pub named: bool,
    /// The pane consumed the event.
    pub handled: bool,
}

/// The history strip and everything behind it.
pub struct HistoryPanel {
    open: bool,
    scrubber: HistoryScrubber,
    source: HistorySource,
    /// Set by the scrubber's callbacks during event dispatch, drained straight
    /// after. The callbacks are `FnMut` closures owned by the widget, so they
    /// cannot reach back into this struct; a shared cell is the handshake.
    preview_intent: Rc<Cell<Option<HistoryEntryId>>>,
    restore_intent: Rc<Cell<Option<HistoryEntryId>>>,
    /// Same handshake for the two writes that are not a restore. Neither payload
    /// is `Copy`, so these are `RefCell` rather than `Cell`.
    name_intent: Rc<RefCell<Option<GroupNameRequest>>>,
    version_intent: Rc<RefCell<Option<VersionAction>>>,
    /// Document heads at the last rebuild. Re-deriving the whole timeline on
    /// every sync would materialise a snapshot per version per frame; the graph
    /// only moves when someone commits.
    last_heads: Vec<ChangeHash>,
    /// Text of the version being previewed, or `None` when showing live state.
    preview: Option<PreviewCard>,
    band: Rect,
}

struct PreviewCard {
    version: usize,
    author: String,
    summary: String,
    text: String,
}

impl Default for HistoryPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryPanel {
    pub fn new() -> Self {
        let preview_intent = Rc::new(Cell::new(None));
        let restore_intent = Rc::new(Cell::new(None));
        let name_intent = Rc::new(RefCell::new(None));
        let version_intent = Rc::new(RefCell::new(None));
        let pi = Rc::clone(&preview_intent);
        let ri = Rc::clone(&restore_intent);
        let ni = Rc::clone(&name_intent);
        let vi = Rc::clone(&version_intent);
        Self {
            open: false,
            scrubber: HistoryScrubber::new(Vec::new())
                .on_preview(move |id| pi.set(Some(*id)))
                .on_restore(move |id| ri.set(Some(*id)))
                .on_name(move |req| *ni.borrow_mut() = Some(req.clone()))
                .on_version(move |act| *vi.borrow_mut() = Some(act.clone())),
            source: HistorySource::new().with_artifact_kind(collab_kind()),
            preview_intent,
            restore_intent,
            name_intent,
            version_intent,
            last_heads: Vec::new(),
            preview: None,
            band: Rect::ZERO,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Toggle the strip. Closing drops any preview so the next open starts on
    /// the live document.
    pub fn toggle(&mut self) {
        self.open = !self.open;
        if !self.open {
            self.preview = None;
        }
    }

    /// Vertical space the strip needs out of the host's layout.
    ///
    /// Asked of the widget rather than fixed: the strip is taller once it has a
    /// version band to draw above the track.
    pub fn band_height(&self) -> f64 {
        if self.open {
            self.scrubber.natural_height()
        } else {
            0.0
        }
    }

    /// The versions currently on the track.
    pub fn versions(&self) -> &[ScrubberGroup] {
        self.scrubber.groups()
    }

    /// True while the strip's name field has the keyboard. The host must route
    /// keys here regardless of where the pointer is, or a name could only be
    /// typed with the mouse held still over the strip.
    pub fn wants_keyboard(&self) -> bool {
        self.scrubber.wants_keyboard()
    }

    /// Rect of the strip's `[Name this version]` button.
    pub fn name_button_rect(&self) -> Rect {
        self.scrubber.name_button_rect()
    }

    /// Rect of the strip's `[Start a version]` / `[Finish version]` button.
    pub fn version_button_rect(&self) -> Rect {
        self.scrubber.version_button_rect()
    }

    /// Whether the handle moves version by version (the default) or change by
    /// change.
    pub fn set_snap_to_versions(&mut self, snap: bool) {
        self.scrubber.set_snap_to_groups(snap);
    }

    /// Number of versions currently on the track. Lets the host (and tests)
    /// see the timeline without reaching into the widget.
    pub fn version_count(&self) -> usize {
        self.scrubber.tick_count()
    }

    /// The rows currently rendered, oldest first.
    pub fn entries(&self) -> &[ui::HistoryEntry] {
        self.scrubber.entries()
    }

    /// The tick the handle sits on.
    pub fn current_tick(&self) -> usize {
        self.scrubber.current_tick()
    }

    /// Move the handle. Exposed so a host — or a headless test — can drive the
    /// track without synthesising a drag.
    pub fn set_current_tick(&mut self, tick: usize) {
        self.scrubber.set_current_tick(tick);
    }

    /// The band the strip was last drawn into — the host hit-tests against it.
    pub fn band(&self) -> Rect {
        self.band
    }

    /// True while the user is dragging the handle. The host must keep routing
    /// events here even once the pointer leaves the band.
    pub fn is_dragging(&self) -> bool {
        self.scrubber.is_dragging()
    }

    /// Rect of the strip's `[Preview]` button. Exposed so a host — a tooltip
    /// layer, or an end-to-end scenario driving the strip — can address it
    /// without reaching into the widget.
    pub fn preview_button_rect(&self) -> Rect {
        self.scrubber.preview_button_rect()
    }

    /// Rect of the strip's `[Restore to this version]` button.
    pub fn restore_button_rect(&self) -> Rect {
        self.scrubber.restore_button_rect()
    }

    /// Screen x of tick `i` on the track, for the same reason.
    pub fn tick_x(&self, i: usize) -> f64 {
        self.scrubber.tick_x(i)
    }

    /// Centre of the track, for the same reason.
    pub fn track_center_y(&self) -> f64 {
        self.scrubber.track_rect().center().y
    }

    /// Summary of the version being previewed, if any.
    pub fn preview_summary(&self) -> Option<&str> {
        self.preview.as_ref().map(|p| p.summary.as_str())
    }

    /// Text of the version being previewed, if any.
    pub fn preview_text(&self) -> Option<&str> {
        self.preview.as_ref().map(|p| p.text.as_str())
    }

    /// Re-derive the timeline if the document moved since the last call.
    ///
    /// Also binds the local automerge actor to the qOS identity on disk, so the
    /// user's own rows carry their name rather than the raw actor hex. Outside
    /// a qOS session there is no identity file and the actor falls through to
    /// the handle the process set for itself (`QUICK_APP_ID`), which is the
    /// same key the remote carets are already coloured by.
    pub fn refresh(&mut self, doc: &mut AutoCommit) {
        let heads = doc.get_heads();
        if heads == self.last_heads && !self.scrubber.entries().is_empty() {
            return;
        }
        self.last_heads = heads;

        let actor = doc.get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            self.source.authors_mut().register_local_identity(&actor);
        }

        let at_end = self.scrubber.current_tick() + 1 >= self.scrubber.tick_count().max(1);
        let entries = self.source.entries(doc);
        let tick = self.scrubber.current_tick();
        // The names people have given runs of this document live in the document,
        // so a collaborator's name arrives with their edits and needs no separate
        // channel. Grouping is derived from the graph and the names together.
        let named = ui::history_groups::read_named_ranges(doc, self.source.authors());
        let groups = ui::group_entries(&entries, &named, &GroupingConfig::default());
        let segments: Vec<ScrubberGroup> = groups
            .iter()
            .map(|g| ScrubberGroup {
                start: g.start,
                // The model's ranges are half-open; the track's are inclusive.
                end: g.end.saturating_sub(1),
                label: g.name.clone(),
                derived: g.title(&entries),
                record_id: g.record_id.clone(),
                open: g.open,
            })
            .collect();
        self.scrubber.set_entries(entries);
        self.scrubber.set_groups(segments);
        // `set_entries` pins the handle to the newest tick. Keep the user's
        // position while they are scrubbing through the past; only follow the
        // document forward if they were already parked at the present.
        if !at_end {
            self.scrubber.set_current_tick(tick);
        }
    }

    /// Explicitly seed the author directory — the collaboration layer calls
    /// this when it learns a peer's real display name.
    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        self.source.authors_mut()
    }

    /// Draw the strip into `band`, plus the preview card above it when one is
    /// showing.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        band: Rect,
    ) {
        use ui::Widget;
        self.band = band;
        self.scrubber.set_rect(band);
        self.scrubber.draw(scene, transform, tr, theme, presence);
        if let Some(p) = &self.preview {
            draw_preview_card(scene, transform, tr, theme, band, p);
        }
    }

    /// Route an event to the strip and act on whatever the user asked for.
    ///
    /// `doc` is the live document: a restore writes into its open transaction
    /// and leaves the commit to the host, so it lands on the undo stack with
    /// the same author and timestamp as any other edit.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        root: &automerge::ObjId,
    ) -> HistoryOutcome {
        use ui::Widget;
        if !self.open {
            return HistoryOutcome::default();
        }
        let mut handled = false;
        let mut focus_target = None;
        // The scrubber ignores both of these — it hit-tests against its own
        // rect — but `Widget::handle_event` asks for a layout tree, so hand it
        // a throwaway one rather than threading the host's.
        let mut scratch: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let node = scratch.new_leaf(taffy::Style::default()).unwrap();
        self.scrubber.handle_event(
            event,
            scale_factor,
            modifiers,
            doc,
            root,
            &mut handled,
            &mut focus_target,
            node,
            &scratch,
        );

        let mut out = HistoryOutcome {
            restored: false,
            named: false,
            handled,
        };
        if let Some(id) = self.preview_intent.take() {
            self.preview = build_preview(doc, &id, self.scrubber.entries());
            out.handled = true;
        }
        if let Some(id) = self.restore_intent.take() {
            if restore_to(doc, &id) {
                out.restored = true;
                // The document is now the version that was showing, so the
                // preview card has nothing left to say.
                self.preview = None;
            }
            out.handled = true;
        }
        if let Some(req) = self.name_intent.borrow_mut().take() {
            out.named = apply_name(doc, &req);
            out.handled = true;
        }
        if let Some(act) = self.version_intent.borrow_mut().take() {
            out.named = apply_version(doc, &act);
            out.handled = true;
        }
        if out.named {
            // The graph will have a new change in it once the host commits; drop
            // the cached heads so the next sync re-derives the band.
            self.last_heads.clear();
        }
        out
    }

    /// Drop the preview and re-pin the handle to the present. Called after a
    /// restore has been committed, so the freshly-appended entry is selected.
    pub fn follow_document(&mut self) {
        self.preview = None;
        self.last_heads.clear();
    }
}

/// Write (or remove) a name over a run of changes.
///
/// An empty label is the request to take a name off — that is what clearing the
/// field and pressing Enter means. Both paths leave the transaction open for the
/// host to commit, so a name lands stamped and attributed like any other edit and
/// reaches collaborators through the same sync.
pub fn apply_name(doc: &mut AutoCommit, req: &GroupNameRequest) -> bool {
    if req.label.is_empty() {
        return match &req.record_id {
            Some(id) => ui::history_groups::clear_name(doc, id),
            None => false,
        };
    }
    ui::history_groups::name_range(doc, &req.start, Some(&req.end), &req.label);
    true
}

/// Open or close a prospective version.
pub fn apply_version(doc: &mut AutoCommit, action: &VersionAction) -> bool {
    match action {
        VersionAction::Open { at, label } => {
            // No end anchor: the range stays open and absorbs whatever comes
            // next, which is the whole point of saying so up front.
            ui::history_groups::name_range(doc, at, None, label);
            true
        }
        VersionAction::Close { record_id, at } => {
            ui::history_groups::close_range(doc, record_id, at)
        }
    }
}

/// Rewrite `doc` to the state it had at the version `id` names, as a **forward
/// change**.
///
/// The delta from now to then is derived and *written*, exactly as undo does.
/// History is appended to, never rewritten: the versions rolled past are still
/// in the change graph, the restore itself becomes the newest entry on the
/// timeline, and any remote edit merged in while the user was scrubbing
/// survives because only the properties that actually differ are touched.
///
/// The transaction is left open for the host to commit.
pub fn restore_to(doc: &mut AutoCommit, id: &HistoryEntryId) -> bool {
    let Some(target) = ui::heads_for_entry(doc, id) else {
        return false;
    };
    let current = doc.get_heads();
    if current == target {
        return false;
    }
    ui::state::write_revert_patches(doc, &current, &target);
    true
}

fn build_preview(
    doc: &mut AutoCommit,
    id: &HistoryEntryId,
    entries: &[ui::HistoryEntry],
) -> Option<PreviewCard> {
    let version = ui::version_index_for_entry(doc, id)?;
    let snap = qdoc::history::materialize_at(doc, version).ok()?;
    let mut text = String::new();
    for b in &snap.blocks {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&b.text);
        if text.chars().count() > PREVIEW_CHARS {
            break;
        }
    }
    let text: String = text.chars().take(PREVIEW_CHARS).collect();
    let row = entries.iter().find(|e| e.id == *id);
    Some(PreviewCard {
        version,
        author: row.map(|e| e.author.name.clone()).unwrap_or_default(),
        summary: row.map(|e| e.summary.clone()).unwrap_or_default(),
        text,
    })
}

fn draw_preview_card(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    band: Rect,
    p: &PreviewCard,
) {
    let card = Rect::new(
        band.x0 + 12.0,
        (band.y0 - PREVIEW_CARD_H - 8.0).max(0.0),
        band.x1 - 12.0,
        (band.y0 - 8.0).max(PREVIEW_CARD_H),
    );
    if card.height() < 24.0 {
        return;
    }
    let shape = RoundedRect::from_rect(card, 8.0);
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.surface_white_10,
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_white_20,
        None,
        &shape,
    );
    let head = format!(
        "Version {} \u{00b7} {} \u{00b7} {}",
        p.version + 1,
        p.author,
        p.summary
    );
    tr.draw_text(
        scene,
        &head,
        11.0,
        theme.tokens.colors.text_dim,
        transform * Affine::translate((card.x0 + 12.0, card.y0 + 8.0)),
        None,
    );
    let body = if p.text.trim().is_empty() {
        "(the document was empty at this version)"
    } else {
        p.text.as_str()
    };
    tr.draw_text(
        scene,
        body,
        13.0,
        theme.tokens.colors.text_primary,
        transform * Affine::translate((card.x0 + 12.0, card.y0 + 28.0)),
        Some((card.width() - 24.0) as f32),
    );
    let _ = Point::ZERO;
}

/// A word-processor document is a file artifact.
fn collab_kind() -> ui::widget::ArtifactKind {
    ui::widget::ArtifactKind::File
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::State;

    /// Build a document with three versions of a paragraph, committed the way
    /// the editor commits (through `State::commit_action`, which stamps and
    /// attributes).
    fn three_version_doc() -> State {
        let mut s = State::new();
        s.doc
            .set_actor(automerge::ActorId::from("alice".as_bytes()));
        let c = qdoc::model::init(&mut s.doc, &s.root);
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        s.commit_action(None, None);
        let p = {
            let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &c, ids: &mut ids };
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "first");
            id
        };
        s.commit_action(None, None);
        {
            let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &c, ids: &mut ids };
            dm.splice_text(&p, 5, 0, " second");
        }
        s.commit_action(None, None);
        s
    }

    fn body(s: &State) -> String {
        let c = qdoc::model::open(&s.doc, &s.root).unwrap();
        qdoc::model::materialize(&s.doc, &c)
            .blocks
            .first()
            .map(|b| b.text.clone())
            .unwrap_or_default()
    }

    /// The pane derives its track from the live document — no seeded demo data.
    #[test]
    fn refresh_builds_the_track_from_the_document() {
        let mut s = three_version_doc();
        let mut panel = HistoryPanel::new();
        panel.refresh(&mut s.doc);
        assert_eq!(panel.version_count(), 3, "schema init plus two edits");
        let summaries: Vec<&str> = panel.entries().iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.contains(&"+5 -0 chars") && summaries.contains(&"+7 -0 chars"),
            "summaries come from the real edits: {summaries:?}"
        );
        assert!(
            panel.entries().iter().all(|e| e.author.name == "alice"),
            "the actor the editor set resolves to a name: {:?}",
            panel.entries().iter().map(|e| &e.author.name).collect::<Vec<_>>()
        );
    }

    /// Restoring rewrites the document forward: the old text comes back AND the
    /// change graph grows.
    #[test]
    fn restore_brings_back_old_text_as_a_new_version() {
        let mut s = three_version_doc();
        let mut panel = HistoryPanel::new();
        panel.refresh(&mut s.doc);
        assert_eq!(body(&s), "first second");

        let versions_before = panel.version_count();
        let target = panel.entries()[1].id;
        assert!(restore_to(&mut s.doc, &target), "restore writes something");
        // The host commits — this is what `needs_commit()` triggers in the app.
        s.commit_action(None, None);
        assert_eq!(body(&s), "first", "content is the version restored to");

        panel.follow_document();
        panel.refresh(&mut s.doc);
        assert_eq!(
            panel.version_count(),
            versions_before + 1,
            "the restore is a new entry, not a rewind"
        );
        assert_eq!(
            panel.entries().last().unwrap().summary,
            "+0 -7 chars",
            "the newest entry describes the restore itself"
        );
    }

    /// A preview reads the past without touching the present.
    #[test]
    fn preview_shows_old_text_and_leaves_the_document_alone() {
        let mut s = three_version_doc();
        let mut panel = HistoryPanel::new();
        panel.refresh(&mut s.doc);
        let target = panel.entries()[1].id;

        panel.preview = build_preview(&mut s.doc, &target, panel.scrubber.entries());
        assert_eq!(panel.preview_text(), Some("first"));
        assert_eq!(panel.preview_summary(), Some("+5 -0 chars"));
        assert_eq!(body(&s), "first second", "previewing writes nothing");
    }

    /// Scrubbing back then re-syncing must not yank the handle to the present
    /// under the user's finger.
    #[test]
    fn refresh_keeps_the_handle_where_the_user_parked_it() {
        let mut s = three_version_doc();
        let mut panel = HistoryPanel::new();
        panel.refresh(&mut s.doc);
        panel.set_current_tick(0);

        // A concurrent edit lands (a peer, or an autosave).
        let c = qdoc::model::open(&s.doc, &s.root).unwrap();
        let blocks = qdoc::model::materialize(&s.doc, &c).blocks;
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &c, ids: &mut ids };
            dm.splice_text(&blocks[0].id, 0, 0, "X");
        }
        s.commit_action(None, None);

        panel.refresh(&mut s.doc);
        assert_eq!(panel.current_tick(), 0, "the handle stays where it was dragged");
    }

    /// A row id from another document restores nothing.
    #[test]
    fn restoring_an_unknown_version_is_a_no_op() {
        let mut s = three_version_doc();
        assert!(!restore_to(&mut s.doc, &HistoryEntryId([0x5a; 16])));
        assert_eq!(body(&s), "first second");
    }

    /// Restoring to the version already showing writes nothing.
    #[test]
    fn restoring_the_current_version_is_a_no_op() {
        let mut s = three_version_doc();
        let mut panel = HistoryPanel::new();
        panel.refresh(&mut s.doc);
        let newest = panel.entries().last().unwrap().id;
        assert!(!restore_to(&mut s.doc, &newest));
    }
}
