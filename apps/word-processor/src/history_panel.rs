//! The document's version history, as a strip under the editing canvas.
//!
//! The editor already round-trips through `qdoc`, so its document *is* the
//! change graph — this pane is the view onto it. [`ui::HistorySource`] turns
//! that graph into rows, the [`HistoryScrubber`] renders them as a draggable
//! track, and dragging the handle scrubs through the document's past.
//!
//! Two affordances, both the scrubber's own:
//!
//! * **Preview** materialises the document as of the selected tick and shows
//!   its text above the strip. Nothing is written; the live document is
//!   untouched and the user can keep scrubbing.
//! * **Restore to this version** rewrites the live document to that state — as
//!   a *forward* change. See [`restore_to`].
//!
//! The strip lives inside the app's own content area, under the canvas and
//! above the layout bar, alongside the outline pane and the styles panel. It is
//! not window chrome: the compositor owns titlebars and window decoration, and
//! an app must not draw those. What an app owns is its document, and its
//! document's history is document content.

use std::cell::Cell;
use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{HistoryEntryId, HistoryScrubber};
use ui::{AuthorDirectory, HistorySource, Theme};
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

/// Height of the history strip when open. Covers the scrubber's popover, track
/// and action-button row (56 + 6 + 24 + 28 plus breathing room).
pub const HISTORY_BAND_H: f64 = 140.0;

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
        let pi = Rc::clone(&preview_intent);
        let ri = Rc::clone(&restore_intent);
        Self {
            open: false,
            scrubber: HistoryScrubber::new(Vec::new())
                .on_preview(move |id| pi.set(Some(*id)))
                .on_restore(move |id| ri.set(Some(*id))),
            source: HistorySource::new().with_artifact_kind(collab_kind()),
            preview_intent,
            restore_intent,
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
    pub fn band_height(&self) -> f64 {
        if self.open { HISTORY_BAND_H } else { 0.0 }
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
        self.scrubber.set_entries(entries);
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

        let mut out = HistoryOutcome { restored: false, handled };
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
        out
    }

    /// Drop the preview and re-pin the handle to the present. Called after a
    /// restore has been committed, so the freshly-appended entry is selected.
    pub fn follow_document(&mut self) {
        self.preview = None;
        self.last_heads.clear();
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
