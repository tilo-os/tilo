//! The document's version history, as a strip under the editing canvas.
//!
//! The document already *is* the change graph (via `qdoc`); this pane is the
//! view onto it. [`ui::HistorySource`] turns it into rows, [`HistoryScrubber`]
//! renders a draggable track, and dragging scrubs through the document's past.
//! Four scrubber-owned affordances: **Preview** (materialise as-of a tick,
//! nothing written), **Restore to this version** ([`restore_to`], a forward
//! change), **Name/Rename** (retroactive, on the run the handle sits in), and
//! **Start/Finish version** (prospective — opens an unbounded range that
//! absorbs changes until closed).
//!
//! The track shows **top-level runs**, not one tick per change ([`ui::group_entries`]
//! groups the graph — hundreds of raw ticks would be an unaimable grey smear) and
//! not nested sub-ranges (one dimension, unlike the two-dimensional activity pane).
//!
//! Lives inside the app's own content area (under the canvas, above the layout
//! bar) — not window chrome, since an app owns its document and the document's
//! history is document content.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::num;
use automerge::{AutoCommit, ChangeHash};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::history_scrubber::{ScrubberGroup, VersionAction};
use ui::widget::{HistoryEntryId, HistoryScrubber};
use ui::{AuthorDirectory, GroupNameRequest, GroupingConfig, HistorySource, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{Modifiers, WindowEvent};

/// Height of the history strip when open (with the version band showing). The
/// strip asks the widget rather than hard-coding this, since a host that
/// reserved the old (bandless) height would clip the button row.
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

    /// Re-derive the timeline if the document moved since the last call. Also
    /// binds the local automerge actor to the Tilo identity on disk, so the
    /// user's rows carry their name rather than the raw actor hex (outside a
    /// Tilo session, falls back to `QUICK_APP_ID`, the same key remote carets
    /// are coloured by).
    pub fn refresh(&mut self, doc: &mut AutoCommit) {
        let heads = ui::history_source::committed_heads(doc);
        if heads == self.last_heads && !self.scrubber.entries().is_empty() {
            return;
        }
        self.last_heads = heads;

        let actor = doc.get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            self.source.authors_mut().register_local_identity(&actor);
        }

        let at_end =
            self.scrubber.current_tick().saturating_add(1) >= self.scrubber.tick_count().max(1);
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
        let Ok(node) = scratch.new_leaf(taffy::Style::default()) else {
            // A fresh tree cannot be out of capacity; if it ever were, the
            // panel simply reports "nothing happened" for this event.
            return HistoryOutcome::default();
        };
        self.scrubber.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers,
                state: doc,
                path: root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &scratch,
            },
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

/// Write (or remove) a name over a run of changes. An empty label clears it
/// (clearing the field + Enter). Either way the transaction is left open for
/// the host to commit, so the name lands stamped/attributed like any edit.
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

/// Rewrite `doc` to the state at version `id`, as a **forward change**: the
/// delta from now to then is derived and *written*, exactly as undo does.
/// History is appended to, never rewritten — rolled-past versions stay in the
/// graph, the restore becomes the newest entry, and a remote edit merged in
/// while scrubbing survives (only the properties that differ are touched).
/// Transaction is left open for the host to commit.
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
    let snap = ui::history_source::materialize_entry(doc, id).ok()??;
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
        theme.tokens.colors.lift(0.72),
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 51),
        None,
        &shape,
    );
    let head = format!(
        "Version {} \u{00b7} {} \u{00b7} {}",
        p.version.saturating_add(1),
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
        Some(num::f32_of_f64(card.width() - 24.0)),
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
    use automerge::transaction::{CommitOptions, Transactable};
    use tilo_automerge::ApplyIncoming;
    use ui::State;

    /// Build a document with three versions of a paragraph, committed the way
    /// the editor commits (through `State::commit_action`, which stamps and
    /// attributes).
    fn three_version_doc() -> State {
        let mut s = State::new();
        s.doc
            .set_actor(automerge::ActorId::from("alice".as_bytes()));
        let c = tilo_doc::model::init(&mut s.doc, &s.root);
        let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
        s.commit_action(None, None);
        let p = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &c,
                ids: &mut ids,
            };
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "first");
            id
        };
        s.commit_action(None, None);
        {
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.splice_text(&p, 5, 0, " second");
        }
        s.commit_action(None, None);
        s
    }

    /// The entry whose edit produced `summary`, addressed by meaning rather
    /// than by index — the track's contents shift when the schema changes.
    fn entry_with(panel: &HistoryPanel, summary: &str) -> ui::widget::history::HistoryEntryId {
        panel
            .entries()
            .iter()
            .find(|e| e.summary == summary)
            .unwrap_or_else(|| {
                panic!(
                    "no entry summarised {summary:?}; track is {:?}",
                    panel
                        .entries()
                        .iter()
                        .map(|e| e.summary.as_str())
                        .collect::<Vec<_>>()
                )
            })
            .id
    }

    fn body(s: &State) -> String {
        let c = tilo_doc::model::open(&s.doc, &s.root).unwrap();
        tilo_doc::model::materialize(&s.doc, &c)
            .blocks
            .first()
            .map(|b| b.text.clone())
            .unwrap_or_default()
    }

    fn append_pending_text(s: &mut State) {
        let c = tilo_doc::model::open(&s.doc, &s.root).unwrap();
        let blocks = tilo_doc::model::materialize(&s.doc, &c).blocks;
        let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
        let mut dm = tilo_doc::DocMut {
            doc: &mut s.doc,
            c: &c,
            ids: &mut ids,
        };
        dm.splice_text(&blocks[0].id, 12, 0, " typing");
        assert!(s.doc.pending_ops() > 0);
    }

    fn commit_pending_text(s: &mut State, changes_before: usize) {
        s.doc.commit_with(
            CommitOptions::default()
                .with_message("Finish typing")
                .with_time(1_780_000_000),
        );
        let changes = s.doc.get_changes(&[]);
        assert_eq!(changes.len(), changes_before + 1);
        let last = changes.last().unwrap();
        assert_eq!(last.message(), Some("Finish typing"));
        assert_eq!(last.timestamp(), 1_780_000_000);
        assert_eq!(body(s), "first second typing");
    }

    /// Refresh and preview preserve pending edits and their action boundary; refresh builds the track from
    /// the document and keeps the handle where the user parked it; the strip grows to make room for the
    /// version band.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn the_history_panel_keeps_pending_edits_and_the_users_handle() {
        // Phase: refresh_preserves_pending_edits_and_their_action_boundary.
        {
            let mut s = three_version_doc();
            let changes_before = s.doc.get_changes(&[]).len();
            append_pending_text(&mut s);
            let pending = s.doc.pending_ops();
            let mut panel = HistoryPanel::new();

            // Exercise both the initial population and the unchanged-head fast path.
            panel.refresh(&mut s.doc);
            assert_eq!(s.doc.pending_ops(), pending);
            assert_eq!(panel.entries().len(), 2);
            panel.refresh(&mut s.doc);
            assert_eq!(s.doc.pending_ops(), pending);
            assert_eq!(body(&s), "first second typing");

            commit_pending_text(&mut s, changes_before);
            panel.refresh(&mut s.doc);
            assert_eq!(panel.entries().len(), 3);
        }

        // Phase: preview_preserves_pending_edits_and_their_action_boundary.
        {
            let mut s = three_version_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let target = entry_with(&panel, "+5 -0 chars");
            let changes_before = s.doc.get_changes(&[]).len();
            append_pending_text(&mut s);
            let pending = s.doc.pending_ops();

            panel.preview = build_preview(&mut s.doc, &target, panel.scrubber.entries());
            assert_eq!(panel.preview_text(), Some("first"));
            assert_eq!(s.doc.pending_ops(), pending);
            assert_eq!(body(&s), "first second typing");
            commit_pending_text(&mut s, changes_before);
        }

        // Phase: refresh_builds_the_track_from_the_document.
        // The pane derives its track from the live document — no seeded demo data.
        {
            let mut s = three_version_doc();
            let mut panel = HistoryPanel::new();
            // The production refresh binds an unregistered *local* actor to the
            // machine identity. Register this fixture actor first so an ambient
            // developer identity cannot relabel Alice's deterministic test edits.
            let alice_actor = automerge::ActorId::from("alice".as_bytes()).to_hex_string();
            panel.authors_mut().register(
                &alice_actor,
                ui::HistoryAuthor::new("alice", 210.0, "alice"),
            );
            panel.refresh(&mut s.doc);
            assert_eq!(
                panel.version_count(),
                2,
                "the two edits; the schema genesis is written by a fixed actor and \
                 is not an authored revision"
            );
            let summaries: Vec<&str> = panel.entries().iter().map(|e| e.summary.as_str()).collect();
            assert!(
                summaries.contains(&"+5 -0 chars") && summaries.contains(&"+7 -0 chars"),
                "summaries come from the real edits: {summaries:?}"
            );
            assert!(
                panel.entries().iter().all(|e| e.author.name == "alice"),
                "the actor the editor set resolves to a name: {:?}",
                panel
                    .entries()
                    .iter()
                    .map(|e| &e.author.name)
                    .collect::<Vec<_>>()
            );
        }

        // Phase: refresh_keeps_the_handle_where_the_user_parked_it.
        // Scrubbing back then re-syncing must not yank the handle to the present
        // under the user's finger.
        {
            let mut s = three_version_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            panel.set_current_tick(0);

            // A concurrent edit lands (a peer, or an autosave).
            let c = tilo_doc::model::open(&s.doc, &s.root).unwrap();
            let blocks = tilo_doc::model::materialize(&s.doc, &c).blocks;
            let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
            {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut s.doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.splice_text(&blocks[0].id, 0, 0, "X");
            }
            s.commit_action(None, None);

            panel.refresh(&mut s.doc);
            assert_eq!(
                panel.current_tick(),
                0,
                "the handle stays where it was dragged"
            );
        }

        // Phase: the_strip_grows_to_make_room_for_the_version_band.
        {
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.toggle();
            panel.refresh(&mut s.doc);
            assert!(
                (panel.band_height() - HISTORY_BAND_H).abs() < f64::EPSILON,
                "the strip asks for the taller band once it has versions to show"
            );
            let mut empty = HistoryPanel::new();
            empty.toggle();
            let mut blank = State::new();
            empty.refresh(&mut blank.doc);
            assert!(
                empty.band_height() > 0.0,
                "an open strip over an empty document still reserves space to say so"
            );
        }
    }

    /// Restore brings back old text as a new version; preview shows old text and leaves the document alone;
    /// restoring an unknown or the current version is a no-op; restoring to a version boundary lands on that
    /// version's text.
    #[test]
    fn history_restore_and_preview_act_only_on_real_versions() {
        // Phase: restore_brings_back_old_text_as_a_new_version.
        // Restoring rewrites the document forward: the old text comes back AND the
        // change graph grows.
        {
            let mut s = three_version_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            assert_eq!(body(&s), "first second");

            let versions_before = panel.version_count();
            // The state after "first" was typed, before " second".
            let target = entry_with(&panel, "+5 -0 chars");
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

        // Phase: preview_shows_old_text_and_leaves_the_document_alone.
        // A preview reads the past without touching the present.
        {
            let mut s = three_version_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let target = entry_with(&panel, "+5 -0 chars");

            panel.preview = build_preview(&mut s.doc, &target, panel.scrubber.entries());
            assert_eq!(panel.preview_text(), Some("first"));
            assert_eq!(panel.preview_summary(), Some("+5 -0 chars"));
            assert_eq!(body(&s), "first second", "previewing writes nothing");
        }

        // Phase: restoring_an_unknown_version_is_a_no_op.
        // A row id from another document restores nothing.
        {
            let mut s = three_version_doc();
            assert!(!restore_to(&mut s.doc, &HistoryEntryId([0x5a; 16])));
            assert_eq!(body(&s), "first second");
        }

        // Phase: restoring_the_current_version_is_a_no_op.
        // Restoring to the version already showing writes nothing.
        {
            let mut s = three_version_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let newest = panel.entries().last().unwrap().id;
            assert!(!restore_to(&mut s.doc, &newest));
        }

        // Phase: restoring_to_a_version_boundary_lands_on_that_versions_text.
        {
            // The strip's [Restore] passes the tick the handle is on, and with
            // snapping the handle can only be on a version boundary. So this is what
            // "restore to the end of that version" actually does to the document.
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let live = body(&s);
            assert!(live.contains("three"), "the fixture ends in session three");

            // The end of the *second* session: everything up to and including Bob's
            // work, and nothing after it.
            let v = panel.versions()[1].clone();
            let boundary = panel.entries()[v.end].id;
            // Read the expected text through the same id the restore uses. `v.end`
            // indexes `entries()`, which omits the schema genesis, while
            // `materialize_at` counts raw changes — passing one to the other
            // compares two different states and the assertion drifts by whatever
            // the entry list filtered out.
            let expected = build_preview(&mut s.doc, &boundary, panel.scrubber.entries())
                .map(|p| p.text)
                .unwrap_or_default();
            assert_ne!(expected, live, "the boundary is a different state");

            assert!(
                restore_to(&mut s.doc, &boundary),
                "the restore wrote something"
            );
            s.commit_action(None, None);
            assert_eq!(
                body(&s),
                expected,
                "the document is the state that version boundary produced"
            );
            // Forward, not a rewind.
            panel.follow_document();
            panel.refresh(&mut s.doc);
            assert!(
                panel.version_count() > v.end + 1,
                "the changes rolled past are still in the graph"
            );
        }
    }

    // -----------------------------------------------------------------
    // Versions on the track
    // -----------------------------------------------------------------

    /// A document with three working sessions: two by the local author either
    /// side of a long break, one by a peer in between — the shape that makes
    /// grouping mean something. Committed with explicit wall clocks (the gaps
    /// are the point), the way the FS layer commits when replaying a durable chain.
    fn three_session_doc() -> (State, Vec<String>) {
        let mut s = State::new();
        s.doc
            .set_actor(automerge::ActorId::from("alice".as_bytes()));
        let c = tilo_doc::model::init(&mut s.doc, &s.root);
        let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
        ui::tilo_resource::history::commit_stamped_at(&mut s.doc, 1_000_000);
        let p = {
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &c,
                ids: &mut ids,
            };
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "Start.");
            id
        };
        ui::tilo_resource::history::commit_stamped_at(&mut s.doc, 1_000_010);

        // Session one: five more edits, seconds apart.
        for stamp in (1_000_020..).step_by(10).take(5) {
            let at = {
                let cc = tilo_doc::model::open(&s.doc, &s.root).unwrap();
                tilo_doc::model::materialize(&s.doc, &cc).blocks[0]
                    .text
                    .chars()
                    .count()
            };
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &c,
                ids: &mut ids,
            };
            dm.splice_text(&p, at, 0, " one");
            ui::tilo_resource::history::commit_stamped_at(&mut s.doc, stamp);
        }

        // Session two, an hour later, by a peer — forked, edited under its own
        // actor, merged back, exactly as a sync round arrives.
        let mut peer = s.doc.fork();
        peer.set_actor(automerge::ActorId::from("bob".as_bytes()));
        for stamp in (1_004_000..).step_by(10).take(3) {
            let at = {
                let cc = tilo_doc::model::open(&peer, &s.root).unwrap();
                tilo_doc::model::materialize(&peer, &cc).blocks[0]
                    .text
                    .chars()
                    .count()
            };
            let cc = tilo_doc::model::open(&peer, &s.root).unwrap();
            let mut pids = tilo_doc::IdGen::new(peer.get_actor().to_hex_string());
            let mut dm = tilo_doc::DocMut {
                doc: &mut peer,
                c: &cc,
                ids: &mut pids,
            };
            dm.splice_text(&p, at, 0, " two");
            ui::tilo_resource::history::commit_stamped_at(&mut peer, stamp);
        }
        s.doc.merge_incoming(&mut peer).expect("peers merge");

        // Session three, two hours after that, back to the local author.
        for stamp in (1_012_000..).step_by(10).take(4) {
            let at = {
                let cc = tilo_doc::model::open(&s.doc, &s.root).unwrap();
                tilo_doc::model::materialize(&s.doc, &cc).blocks[0]
                    .text
                    .chars()
                    .count()
            };
            let cc = tilo_doc::model::open(&s.doc, &s.root).unwrap();
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &cc,
                ids: &mut ids,
            };
            dm.splice_text(&p, at, 0, " three");
            ui::tilo_resource::history::commit_stamped_at(&mut s.doc, stamp);
        }
        (s, Vec::new())
    }

    /// The track shows versions, not one tick per change; naming the current version round-trips and adds no
    /// version of its own; an open version absorbs later changes; snapping can reach every version.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn versions_group_changes_and_names_round_trip() {
        // Phase: the_track_shows_versions_not_one_tick_per_change.
        {
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let changes = panel.version_count();
            assert!(changes >= 13, "the fixture has three sessions: {changes}");
            let versions = panel.versions().len();
            assert!(
                versions >= 3 && versions < changes,
                "{changes} changes must read as a handful of versions, got {versions}"
            );
            // The sessions are the boundaries: a long gap and a change of author.
            assert!(
                panel.versions().iter().any(|v| v.derived.contains("edits")),
                "an unnamed version describes itself: {:?}",
                panel
                    .versions()
                    .iter()
                    .map(|v| v.derived.clone())
                    .collect::<Vec<_>>()
            );
            // And the segments tile the whole track without gaps or overlaps.
            let mut cursor = 0usize;
            for v in panel.versions() {
                assert_eq!(
                    v.start,
                    cursor,
                    "versions must tile: {:?}",
                    panel.versions()
                );
                cursor = v.end + 1;
            }
            assert_eq!(cursor, changes, "and cover every change");
        }

        // Phase: naming_the_current_version_round_trips_through_the_document.
        {
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let v = panel.versions()[1].clone();
            let entries = panel.entries().to_vec();
            let req = GroupNameRequest {
                record_id: None,
                start: entries[v.start].id,
                end: entries[v.end].id,
                label: "Bob's pass".to_string(),
            };
            assert!(apply_name(&mut s.doc, &req));
            s.commit_action(None, None);

            // In the document, so a collaborator gets it with the edits.
            let ranges = ui::history_groups::read_named_ranges(&s.doc, &AuthorDirectory::new());
            assert_eq!(ranges.len(), 1);
            assert_eq!(ranges[0].label, "Bob's pass");

            panel.follow_document();
            panel.refresh(&mut s.doc);
            assert!(
                panel
                    .versions()
                    .iter()
                    .any(|g| g.label.as_deref() == Some("Bob's pass")),
                "the band is on the track: {:?}",
                panel
                    .versions()
                    .iter()
                    .map(|g| g.title().to_string())
                    .collect::<Vec<_>>()
            );
        }

        // Phase: naming_a_version_adds_no_version_of_its_own.
        {
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let before = panel.version_count();
            let entries = panel.entries().to_vec();
            apply_name(
                &mut s.doc,
                &GroupNameRequest {
                    record_id: None,
                    start: entries[0].id,
                    end: entries[2].id,
                    label: "Opening".to_string(),
                },
            );
            s.commit_action(None, None);
            panel.follow_document();
            panel.refresh(&mut s.doc);
            assert_eq!(
                panel.version_count(),
                before,
                "naming a version must not appear as a change on the track it named"
            );
        }

        // Phase: an_open_version_absorbs_the_changes_that_come_after_it.
        {
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            let entries = panel.entries().to_vec();
            let last = entries.last().unwrap().id;

            assert!(apply_version(
                &mut s.doc,
                &VersionAction::Open {
                    at: last,
                    label: "Rework the intro".to_string()
                }
            ));
            s.commit_action(None, None);
            panel.follow_document();
            panel.refresh(&mut s.doc);
            let open = panel
                .versions()
                .iter()
                .find(|v| v.open)
                .cloned()
                .expect("the version is open");
            assert_eq!(open.label.as_deref(), Some("Rework the intro"));
            let width_before = open.end - open.start;

            // Keep working. The open version has to grow.
            let c = tilo_doc::model::open(&s.doc, &s.root).unwrap();
            let blocks = tilo_doc::model::materialize(&s.doc, &c).blocks;
            let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
            for _ in 0..3 {
                let at = {
                    let cc = tilo_doc::model::open(&s.doc, &s.root).unwrap();
                    tilo_doc::model::materialize(&s.doc, &cc).blocks[0]
                        .text
                        .chars()
                        .count()
                };
                let mut dm = tilo_doc::DocMut {
                    doc: &mut s.doc,
                    c: &c,
                    ids: &mut ids,
                };
                dm.splice_text(&blocks[0].id, at, 0, " more");
                s.commit_action(None, None);
            }
            panel.follow_document();
            panel.refresh(&mut s.doc);
            let open = panel
                .versions()
                .iter()
                .find(|v| v.open)
                .cloned()
                .expect("still open");
            assert!(
                open.end - open.start > width_before,
                "an open version must absorb what comes next: {} then {}",
                width_before,
                open.end - open.start
            );
            assert_eq!(
                open.end,
                panel.version_count() - 1,
                "and it must run right up to the present"
            );

            // Closing it pins the end where it is now.
            let entries = panel.entries().to_vec();
            let end_id = entries.last().unwrap().id;
            let record = ui::history_groups::read_named_ranges(&s.doc, &AuthorDirectory::new())[0]
                .id
                .clone();
            assert!(apply_version(
                &mut s.doc,
                &VersionAction::Close {
                    record_id: record,
                    at: end_id
                }
            ));
            s.commit_action(None, None);
            panel.follow_document();
            panel.refresh(&mut s.doc);
            assert!(
                panel.versions().iter().all(|v| !v.open),
                "nothing is open any more"
            );
            assert!(
                panel
                    .versions()
                    .iter()
                    .any(|v| v.label.as_deref() == Some("Rework the intro")),
                "and the version is still named"
            );
        }

        // Phase: a_version_that_snapping_can_reach_is_every_version.
        {
            // Every version boundary must be reachable, or a version the user can see
            // is one they cannot scrub to.
            let (mut s, _) = three_session_doc();
            let mut panel = HistoryPanel::new();
            panel.refresh(&mut s.doc);
            for v in panel.versions().to_vec() {
                panel.set_current_tick(v.end);
                assert_eq!(
                    panel.current_tick(),
                    v.end,
                    "version ending at {} unreachable",
                    v.end
                );
            }
        }
    }
}
