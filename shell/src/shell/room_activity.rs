//! Room activity — everything that has happened to this workspace, by
//! everyone, in one filterable pane.
//!
//! # What a room's history actually is here
//!
//! A workspace is a collaboration room, and the room's own state lives in
//! [`WorkspaceTree::doc`] — an automerge document carrying each workspace's
//! name, its room id, its owner, its capability grants, and its strips,
//! window groups and tabs. Every mirror in `state.rs` named `doc_*` writes
//! into it, peers merge into it, and it is therefore a real change graph with
//! real authors and (since those mirrors commit through
//! [`WorkspaceTree::commit_room_change`]) real timestamps.
//!
//! That graph is what this pane renders. [`ui::HistorySource`] turns it into
//! rows and [`ui::ActivityTimeline`] draws them; this module is the join
//! between the two plus the room-specific vocabulary, the filters the shell
//! owns, and the sheet's geometry.
//!
//! # What it deliberately does not show
//!
//! Not the contents of the documents open inside the room. An app's document
//! lives in that app's own process; the compositor's broker
//! ([`crate::shell::collab_broker`]) routes its CRDT deltas between peers as
//! opaque bytes keyed by `(app_id, cluster_id)` and never materialises them —
//! it could not describe an edit to a note if it wanted to. Naming per-app
//! documents here would mean inventing a data source. A room's own history is
//! what the shell can honestly account for, so a room's own history is what
//! this shows; per-document history is the app's to present, over its own
//! document (see `apps/word-processor/src/history_panel.rs`).
//!
//! # Restore
//!
//! A row's `[Revert]` writes the delta from now back to that version as a
//! **new forward change**, through the same [`ui::state::write_revert_patches`]
//! primitive the per-document scrubber uses. History is appended to, never
//! rewritten: the reverted-past versions stay in the graph and the revert is
//! itself the newest row. Governance rows (a grant, a revoke, an owner change)
//! carry [`OpClass::Govern`], which [`ui::widget::op_class_is_revertible`]
//! declares non-revertible — an owner counters a governance act with another
//! one rather than quietly rewinding it — so those rows never offer the chip.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use automerge::{ChangeHash, Prop, ReadDoc, ScalarValue, Value};
use collab_types::OpClass;
use collab_types::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{ArtifactKind, HistoryEntry, HistoryEntryId, HistoryFilter};
use ui::{ActivityTimeline, AuthorDirectory, HistorySource, Theme, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{Modifiers, WindowEvent};

use crate::shell::state::WorkspaceTree;

/// Widest the sheet ever gets. Past this the rows stop gaining information
/// and start becoming hard to scan across.
const SHEET_MAX_W: f64 = 480.0;
/// Narrowest the sheet gets before it stops being a sheet and takes the whole
/// viewport instead. Below this a side sheet leaves the content behind it too
/// thin to be worth preserving, which is the point at which the HIG's
/// content-panel rule says show one thing at a time.
const SHEET_MIN_W: f64 = 360.0;
/// Viewport width under which the sheet goes full-bleed.
const FULL_BLEED_UNDER: f64 = 720.0;
/// Inset from the viewport edges.
const SHEET_INSET: f64 = 16.0;
/// Height of the pane's own header (title + subtitle).
const HEADER_H: f64 = 52.0;
/// Height of one row of the shell-owned filter chips.
const CHIP_ROW_H: f64 = 30.0;

/// How far back the pane is showing. Maps onto
/// [`HistoryFilter::time_range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    AllTime,
    Today,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 4] = [Period::AllTime, Period::Today, Period::Week, Period::Month];

    pub fn label(self) -> &'static str {
        match self {
            Period::AllTime => "All time",
            Period::Today => "Today",
            Period::Week => "7 days",
            Period::Month => "30 days",
        }
    }

    /// The half-open `[start, now)` window this period selects, or `None` for
    /// "no range" — which is how [`HistoryFilter`] spells "everything".
    ///
    /// `now` is passed in rather than read from the clock so the pane's
    /// behaviour is testable and so every row in one frame is judged against
    /// one instant.
    pub fn range(self, now: SystemTime) -> Option<(SystemTime, SystemTime)> {
        let back = match self {
            Period::AllTime => return None,
            Period::Today => Duration::from_secs(24 * 3600),
            Period::Week => Duration::from_secs(7 * 24 * 3600),
            Period::Month => Duration::from_secs(30 * 24 * 3600),
        };
        // The end is nudged past `now` so a change committed during this very
        // frame is inside the window rather than one tick outside it — the
        // interval is half-open at the top.
        let end = now + Duration::from_secs(1);
        Some((now.checked_sub(back).unwrap_or(SystemTime::UNIX_EPOCH), end))
    }
}

/// What a dispatched event did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomActivityOutcome {
    /// The pane consumed the event.
    pub handled: bool,
    /// A row's `[Revert]` wrote a new forward change into the room document.
    pub reverted: bool,
}

/// The room-activity sheet.
pub struct RoomActivityPane {
    timeline: ActivityTimeline,
    source: HistorySource,
    /// Row the user asked to revert to. Set by the timeline's callback during
    /// event dispatch and drained immediately after — the callback is an
    /// `FnMut` owned by the widget and cannot reach back into this struct.
    revert_intent: Rc<Cell<Option<HistoryEntryId>>>,
    /// Room-document heads at the last rebuild. Re-deriving the timeline every
    /// frame would walk the whole change graph per frame; the graph only moves
    /// when someone commits.
    last_heads: Vec<ChangeHash>,
    period: Period,
    /// Contributor the user picked from the people row, or `None` for
    /// everyone. Drives [`HistoryFilter::authors`].
    author: Option<String>,
    /// Distinct contributors present in the current entry set, in first-seen
    /// order. Derived from the rows, never from a roster — a name only appears
    /// here if that person actually did something in this room.
    contributors: Vec<String>,
    sheet: Rect,
    /// Hit rects cached by `draw` for the two shell-owned chip rows. Cleared
    /// whenever the pane is not painted so a stale click cannot trip them.
    period_rects: Vec<(Period, Rect)>,
    author_rects: Vec<(Option<String>, Rect)>,
    /// Workspace name shown in the header. Refreshed with the entries.
    room_name: String,
    /// Total rows before the shell's own filters, for the header count.
    total_entries: usize,
    /// Pinned "now" for tests, so relative timestamps and the period windows
    /// stay stable across runs.
    now_override: Option<SystemTime>,
}

impl Default for RoomActivityPane {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomActivityPane {
    pub fn new() -> Self {
        let revert_intent = Rc::new(Cell::new(None));
        let ri = Rc::clone(&revert_intent);
        Self {
            timeline: ActivityTimeline::new().on_revert(move |id| ri.set(Some(*id))),
            // A workspace is a cluster: a tiled set of running windows that
            // can be saved and reopened collaboratively. That is what this
            // document describes, so that is what its rows are tagged with.
            source: HistorySource::new().with_artifact_kind(ArtifactKind::Cluster),
            revert_intent,
            last_heads: Vec::new(),
            period: Period::default(),
            author: None,
            contributors: Vec::new(),
            sheet: Rect::ZERO,
            period_rects: Vec::new(),
            author_rects: Vec::new(),
            room_name: String::new(),
            total_entries: 0,
            now_override: None,
        }
    }

    /// Pin "now". Tests use this so a period window and the relative
    /// timestamps do not depend on when the suite runs.
    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
        self.timeline.set_now_for_test(now);
    }

    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    /// Read-only access to the rows the pane derived, unfiltered.
    pub fn entries(&self) -> &[HistoryEntry] {
        self.timeline.entries()
    }

    /// Rows currently on screen, after every filter.
    pub fn visible_len(&self) -> usize {
        self.timeline.visible_len()
    }

    /// Borrow a visible row.
    pub fn visible_entry(&self, i: usize) -> Option<&HistoryEntry> {
        self.timeline.visible_entry(i)
    }

    /// The contributors that actually appear in the room's history.
    pub fn contributors(&self) -> &[String] {
        &self.contributors
    }

    pub fn period(&self) -> Period {
        self.period
    }

    pub fn author_filter(&self) -> Option<&str> {
        self.author.as_deref()
    }

    /// The sheet's last laid-out rect. `Rect::ZERO` before the first layout.
    pub fn rect(&self) -> Rect {
        self.sheet
    }

    /// Borrow the timeline widget — the pane's own filters live here (the
    /// op-class chips and the author typeahead), so a test that wants to drive
    /// them goes through this rather than replicating the geometry.
    pub fn timeline(&self) -> &ActivityTimeline {
        &self.timeline
    }

    pub fn timeline_mut(&mut self) -> &mut ActivityTimeline {
        &mut self.timeline
    }

    /// Select a period. Reinstalls the filter so the list narrows immediately.
    pub fn set_period(&mut self, period: Period) {
        if self.period != period {
            self.period = period;
            self.reinstall_filter();
        }
    }

    /// Restrict to one contributor, or `None` for everyone.
    pub fn set_author_filter(&mut self, author: Option<String>) {
        if self.author != author {
            self.author = author;
            self.reinstall_filter();
        }
    }

    /// Push the shell-owned filters onto the timeline. The op-class chips and
    /// the author typeahead are the widget's own and are left alone.
    fn reinstall_filter(&mut self) {
        let mut filter = HistoryFilter::default();
        filter.time_range = self.period.range(self.now());
        if let Some(a) = &self.author {
            filter.authors.push(a.clone());
        }
        self.timeline.set_filter(filter);
        self.timeline.set_time_range_label(match self.period {
            Period::AllTime => None,
            p => Some(p.label().to_string()),
        });
    }

    /// Re-derive the timeline from the room document if the graph has moved.
    ///
    /// Called every frame while the pane is open; the heads check is what
    /// keeps that cheap. The tree is taken by `&mut` because reading an
    /// automerge change graph closes any open transaction first.
    pub fn refresh(&mut self, tree: &mut WorkspaceTree) {
        // Close anything a mirror left open, so a mutation from this frame is
        // a change with a timestamp rather than an invisible pending
        // transaction.
        tree.commit_room_change();

        let name = tree
            .active_workspace()
            .map(|w| w.name.clone())
            .unwrap_or_default();
        if name != self.room_name {
            self.room_name = name;
        }

        let heads = tree.doc.get_heads();
        if heads == self.last_heads && !self.timeline.entries().is_empty() {
            return;
        }
        self.last_heads = heads;

        // Bind the local automerge actor to the qOS identity on disk so the
        // user's own rows carry their name. Outside a qOS session there is no
        // identity file and the actor falls through to the honest
        // unknown-participant path.
        let actor = tree.doc.get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            self.source.authors_mut().register_local_identity(&actor);
        }

        let mut entries = self.source.entries(&mut tree.doc);
        describe_room_changes(&mut tree.doc, &mut entries);
        self.total_entries = entries.len();

        let mut seen: HashSet<String> = HashSet::new();
        self.contributors = entries
            .iter()
            .filter(|e| seen.insert(e.author.name.clone()))
            .map(|e| e.author.name.clone())
            .collect();
        // A contributor filter naming someone who no longer appears would hide
        // everything with no way back except clearing it, so drop it.
        if let Some(a) = &self.author
            && !self.contributors.iter().any(|c| c == a)
        {
            self.author = None;
        }

        self.timeline.set_entries(entries);
        self.reinstall_filter();
    }

    /// Explicitly seed the author directory — the collaboration layer calls
    /// this when it learns a peer's real display name.
    pub fn authors_mut(&mut self) -> &mut AuthorDirectory {
        self.source.authors_mut()
    }

    // ---------- Geometry ----------

    /// Lay the sheet out inside `viewport`.
    ///
    /// Wide viewport: a right-anchored sheet, so the room stays visible behind
    /// what happened to it. Narrow viewport: full-bleed, because a 360 px
    /// sheet over a 600 px desktop is two unusable halves rather than one
    /// usable pane.
    pub fn layout(&mut self, viewport: Rect) {
        let vw = viewport.width();
        let sheet_w = if vw < FULL_BLEED_UNDER {
            (vw - 2.0 * SHEET_INSET).max(0.0)
        } else {
            (vw * 0.34).clamp(SHEET_MIN_W, SHEET_MAX_W)
        };
        let x1 = viewport.x1 - SHEET_INSET;
        let x0 = (x1 - sheet_w).max(viewport.x0 + SHEET_INSET.min(vw / 2.0));
        let y0 = viewport.y0 + SHEET_INSET;
        let y1 = viewport.y1 - SHEET_INSET;
        self.sheet = Rect::new(x0, y0, x1, y1.max(y0));

        // The timeline occupies whatever is left under the header and the two
        // chip rows. On a very short viewport the chip rows are dropped before
        // the row list is, because a pane with filters and no rows is worse
        // than one with rows and no filters.
        let rows_y0 = self.sheet.y0 + self.chrome_height();
        self.timeline.set_rect(Rect::new(
            self.sheet.x0 + 8.0,
            rows_y0.min(self.sheet.y1),
            self.sheet.x1 - 8.0,
            self.sheet.y1 - 8.0,
        ));
    }

    /// Whether the sheet is tall enough to afford the shell's chip rows.
    fn shows_chip_rows(&self) -> bool {
        self.sheet.height() > HEADER_H + 2.0 * CHIP_ROW_H + 140.0
    }

    /// Vertical space the pane's own chrome takes above the timeline.
    fn chrome_height(&self) -> f64 {
        if self.shows_chip_rows() {
            HEADER_H + 2.0 * CHIP_ROW_H
        } else {
            HEADER_H
        }
    }

    // ---------- Paint ----------

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let card = RoundedRect::from_rect(self.sheet, tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &card);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        // Header. The subtitle is the pane's honesty: it names the room and
        // says what "activity" covers here, so nobody reads an empty list as
        // "nothing happened in my documents".
        let title_size = tokens.typography.scale.lg;
        tr.draw_text(
            scene,
            "Room activity",
            title_size,
            colors.text_primary,
            transform * Affine::translate((self.sheet.x0 + 16.0, self.sheet.y0 + 12.0)),
            None,
        );
        let sub_size = (tokens.typography.scale.sm - 1.0).max(10.0);
        let subtitle = if self.room_name.is_empty() {
            format!("{} changes to this room", self.total_entries)
        } else {
            format!(
                "{} changes to {} \u{00b7} workspace, membership and permissions",
                self.total_entries, self.room_name
            )
        };
        tr.draw_text(
            scene,
            &subtitle,
            sub_size,
            colors.text_dim,
            transform * Affine::translate((self.sheet.x0 + 16.0, self.sheet.y0 + 32.0)),
            Some((self.sheet.width() - 32.0) as f32),
        );

        self.period_rects.clear();
        self.author_rects.clear();
        if self.shows_chip_rows() {
            let y = self.sheet.y0 + HEADER_H;
            let periods: Vec<(Period, String)> = Period::ALL
                .iter()
                .map(|p| (*p, p.label().to_string()))
                .collect();
            let selected = self.period;
            self.period_rects = draw_chip_row(
                scene,
                transform,
                tr,
                theme,
                Rect::new(self.sheet.x0 + 12.0, y, self.sheet.x1 - 12.0, y + CHIP_ROW_H),
                &periods,
                |p| *p == selected,
            );

            let mut people: Vec<(Option<String>, String)> = vec![(None, "Everyone".to_string())];
            for c in &self.contributors {
                people.push((Some(c.clone()), c.clone()));
            }
            let picked = self.author.clone();
            let y2 = y + CHIP_ROW_H;
            self.author_rects = draw_chip_row(
                scene,
                transform,
                tr,
                theme,
                Rect::new(
                    self.sheet.x0 + 12.0,
                    y2,
                    self.sheet.x1 - 12.0,
                    y2 + CHIP_ROW_H,
                ),
                &people,
                |a| *a == picked,
            );
        }

        self.timeline.draw(scene, transform, tr, theme, presence);
    }

    // ---------- Events ----------

    /// Route an event into the pane. Returns what it did.
    ///
    /// `tree` is the live room document: a revert writes into it as a new
    /// forward change and commits, exactly as a `doc_*` mirror would.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        cursor: Option<Point>,
        tree: &mut WorkspaceTree,
    ) -> RoomActivityOutcome {
        let mut out = RoomActivityOutcome::default();

        // The shell's own chip rows first — they sit above the timeline and
        // the timeline would otherwise treat a chip click as a click on its
        // own filter strip.
        if let (Some(p), WindowEvent::MouseInput { state, button, .. }) = (cursor, event)
            && *state == winit::event::ElementState::Pressed
            && *button == winit::event::MouseButton::Left
        {
            for (period, rect) in self.period_rects.clone() {
                if rect.contains(p) {
                    self.set_period(period);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                    };
                }
            }
            for (author, rect) in self.author_rects.clone() {
                if rect.contains(p) {
                    self.set_author_filter(author);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                    };
                }
            }
        }

        let mut handled = false;
        let mut focus_target = None;
        // The timeline hit-tests against its own rect and ignores both of
        // these, but `Widget::handle_event` asks for a layout tree — hand it a
        // throwaway one rather than threading the shell's.
        let mut scratch: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let node = scratch.new_leaf(taffy::Style::default()).unwrap();
        let root = automerge::ROOT;
        self.timeline.handle_event(
            event,
            scale_factor,
            modifiers,
            &mut tree.doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &scratch,
        );
        out.handled = handled;

        if let Some(id) = self.revert_intent.take() {
            out.handled = true;
            if self.revert_to(tree, &id) {
                out.reverted = true;
            }
        }
        out
    }

    /// Rewrite the room document to the state it had at `id`, as a **new
    /// forward change**.
    ///
    /// Same primitive as the per-document scrubber's restore: the delta from
    /// now back to then is derived and written, so the versions rolled past
    /// stay in the graph and the revert becomes the newest row. Returns
    /// `false` when the id names no change here or when the document is
    /// already in that state.
    pub fn revert_to(&mut self, tree: &mut WorkspaceTree, id: &HistoryEntryId) -> bool {
        let Some(target) = ui::heads_for_entry(&mut tree.doc, id) else {
            return false;
        };
        let current = tree.doc.get_heads();
        if current == target {
            return false;
        }
        ui::state::write_revert_patches(&mut tree.doc, &current, &target);
        let before = tree.doc.get_heads();
        tree.commit_room_change();
        if tree.doc.get_heads() == before {
            // The patch set was empty — nothing was actually different.
            return false;
        }
        // The document just changed underneath the cache; bring the tree's
        // read model back in line and re-derive the rows so the revert shows
        // up as the newest entry.
        tree.resync_cache_from_doc();
        self.last_heads.clear();
        self.refresh(tree);
        true
    }

    /// Drop the derived rows so the next [`Self::refresh`] rebuilds them.
    /// Called when the pane is opened, so a room that changed while it was
    /// closed is up to date the moment it appears.
    pub fn invalidate(&mut self) {
        self.last_heads.clear();
    }
}

/// Paint one row of pill-shaped chips, returning the hit rects for the ones
/// that fit.
///
/// Chips that would overflow the row are not painted and not returned, so a
/// narrow sheet loses the tail of a long contributor list rather than
/// overlapping it — and, because the rect never exists, a click cannot select
/// a chip the user cannot see.
fn draw_chip_row<T: Clone>(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    items: &[(T, String)],
    is_active: impl Fn(&T) -> bool,
) -> Vec<(T, Rect)> {
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let size = (tokens.typography.scale.sm - 1.0).max(10.0);
    let chip_h = (row.height() - 8.0).max(18.0);
    let cy = row.center().y;
    let mut out = Vec::with_capacity(items.len());
    let mut x = row.x0;
    let mut dropped = 0usize;
    for (value, label) in items {
        // Reserve room for a "+N" overflow marker so it never itself overflows.
        let (lw, lh) = tr.measure(label, size, None);
        let w = lw as f64 + 16.0;
        if x + w > row.x1 - 28.0 {
            dropped += 1;
            continue;
        }
        let rect = Rect::new(x, cy - chip_h / 2.0, x + w, cy + chip_h / 2.0);
        let active = is_active(value);
        let shape = RoundedRect::from_rect(rect, chip_h / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            if active {
                colors.accent[5]
            } else {
                colors.surface_white_5
            },
            None,
            &shape,
        );
        tr.draw_text(
            scene,
            label,
            size,
            if active {
                colors.text_primary
            } else {
                colors.text_dim
            },
            transform
                * Affine::translate((
                    rect.x0 + (rect.width() - lw as f64) / 2.0,
                    rect.y0 + (rect.height() - lh as f64) / 2.0,
                )),
            None,
        );
        out.push((value.clone(), rect));
        x += w + 6.0;
    }
    if dropped > 0 {
        let more = format!("+{dropped}");
        let (mw, mh) = tr.measure(&more, size, None);
        tr.draw_text(
            scene,
            &more,
            size,
            colors.text_dim,
            transform * Affine::translate((row.x1 - mw as f64, cy - mh as f64 / 2.0)),
            None,
        );
    }
    out
}

// =====================================================================
// Room vocabulary
// =====================================================================

/// Rewrite each row's summary in the room's own words, where the change says
/// something nameable.
///
/// [`HistorySource`] describes a change generically — "1 field", "+2 items" —
/// because it works over any document. The room document has a known shape, so
/// its patches can be read back into what actually happened: a grant, a
/// revoke, a window opened, a workspace renamed. Everything here is derived
/// from the change's own patch set; a change whose shape is not recognised
/// keeps whatever the generic producer said.
fn describe_room_changes(doc: &mut automerge::AutoCommit, entries: &mut [HistoryEntry]) {
    if entries.is_empty() {
        return;
    }
    // One pass over the change list. A change's own `deps` are the document
    // heads immediately before it, so diffing `deps -> [hash]` isolates
    // exactly that change without having to rebuild a prefix per entry.
    let changes: Vec<(HistoryEntryId, Vec<ChangeHash>, ChangeHash)> = doc
        .get_changes(&[])
        .into_iter()
        .map(|c| {
            (
                ui::history_source::entry_id_from_hash(&c.hash().to_string()),
                c.deps().to_vec(),
                c.hash(),
            )
        })
        .collect();

    for (id, deps, hash) in changes {
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
            continue;
        };
        let patches = doc.diff(&deps, &[hash]);
        if let Some((summary, op_class)) = describe_patches(&patches) {
            entry.summary = summary;
            entry.op_class = op_class;
        }
    }
}

/// Map a change's patches onto a room event, or `None` when nothing in it is
/// recognised.
fn describe_patches(patches: &[automerge::Patch]) -> Option<(String, OpClass)> {
    let mut grants: Vec<String> = Vec::new();
    let mut revokes = 0usize;
    let mut renamed: Option<String> = None;
    let mut owner_set = false;
    let mut room_set = false;
    let mut tabs_added = 0usize;
    let mut tabs_removed = 0usize;
    let mut tab_titles: Vec<String> = Vec::new();
    let mut groups_moved = 0usize;
    let mut workspaces_added = 0usize;
    let mut workspaces_removed = 0usize;
    let mut geometry = 0usize;
    let mut active_switched = false;

    for p in patches {
        let container = path_tail(p);
        match (&p.action, container.as_str()) {
            (automerge::PatchAction::PutMap { key, value, .. }, "capabilities") => {
                let _ = key;
                if let Value::Scalar(s) = &value.0
                    && let ScalarValue::Bytes(blob) = s.as_ref()
                    && let Ok(cap) = serde_json::from_slice::<CapOverride>(blob)
                {
                    grants.push(describe_capability(&cap));
                } else {
                    grants.push("a capability".to_string());
                }
            }
            (automerge::PatchAction::DeleteMap { .. }, "capabilities") => revokes += 1,
            (automerge::PatchAction::PutMap { key, value, .. }, _) if key == "name" => {
                renamed = Some(scalar_string(&value.0).unwrap_or_default());
            }
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "owner" => owner_set = true,
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "room" => room_set = true,
            (automerge::PatchAction::PutMap { key, value, .. }, _) if key == "title" => {
                if let Some(t) = scalar_string(&value.0)
                    && !t.is_empty()
                {
                    tab_titles.push(t);
                }
            }
            (automerge::PatchAction::PutMap { key, .. }, _)
                if key == "maximized" || key == "width_pixels" =>
            {
                geometry += 1;
            }
            (automerge::PatchAction::PutMap { key, .. }, _) if key == "active" => {
                active_switched = true;
            }
            (automerge::PatchAction::Insert { values, .. }, "tabs") => tabs_added += values.len(),
            (automerge::PatchAction::DeleteSeq { length, .. }, "tabs") => tabs_removed += length,
            (automerge::PatchAction::Insert { values, .. }, "window_groups") => {
                groups_moved += values.len()
            }
            (automerge::PatchAction::DeleteSeq { length, .. }, "window_groups") => {
                groups_moved += length
            }
            (automerge::PatchAction::Insert { values, .. }, "workspaces") => {
                workspaces_added += values.len()
            }
            (automerge::PatchAction::DeleteSeq { length, .. }, "workspaces") => {
                workspaces_removed += length
            }
            _ => {}
        }
    }

    // Governance first: a change that handed out or took away authority is
    // that, whatever else it also touched. It is also the class the user is
    // most likely to be looking for when they open this pane.
    if !grants.is_empty() {
        let s = if grants.len() == 1 {
            format!("Granted {}", grants[0])
        } else {
            format!("Granted {} capabilities", grants.len())
        };
        return Some((s, OpClass::Govern));
    }
    if revokes > 0 {
        let s = if revokes == 1 {
            "Revoked a capability".to_string()
        } else {
            format!("Revoked {revokes} capabilities")
        };
        return Some((s, OpClass::Govern));
    }
    if owner_set {
        return Some(("Set the room owner".to_string(), OpClass::Govern));
    }
    if room_set {
        return Some(("Assigned the room identity".to_string(), OpClass::Govern));
    }
    if let Some(name) = renamed {
        let s = if name.is_empty() {
            "Renamed the workspace".to_string()
        } else {
            format!("Named the workspace \u{201c}{name}\u{201d}")
        };
        return Some((s, OpClass::EditMeta));
    }
    if workspaces_removed > 0 && workspaces_added == 0 {
        return Some((
            plural(workspaces_removed, "workspace", "Removed"),
            OpClass::Tombstone,
        ));
    }
    if workspaces_added > 0 {
        // A fresh seed writes the whole tree at once; naming the windows it
        // brought with it is more use than counting list elements.
        let s = if tab_titles.is_empty() {
            plural(workspaces_added, "workspace", "Added")
        } else {
            format!(
                "{} \u{00b7} {}",
                plural(workspaces_added, "workspace", "Added"),
                join_titles(&tab_titles)
            )
        };
        return Some((s, OpClass::Splice));
    }
    if tabs_added > 0 {
        let s = if tab_titles.is_empty() {
            plural(tabs_added, "window", "Opened")
        } else {
            format!("Opened {}", join_titles(&tab_titles))
        };
        return Some((s, OpClass::Splice));
    }
    if tabs_removed > 0 {
        return Some((plural(tabs_removed, "window", "Closed"), OpClass::Tombstone));
    }
    if groups_moved > 0 {
        return Some(("Rearranged the strip".to_string(), OpClass::Move));
    }
    if geometry > 0 {
        return Some(("Resized a window group".to_string(), OpClass::EditMeta));
    }
    if active_switched {
        return Some((
            "Switched the active workspace".to_string(),
            OpClass::EditMeta,
        ));
    }
    None
}

/// The map key of the object a patch lands in — `"capabilities"`, `"tabs"`,
/// `"workspaces"` — or an empty string when the patch targets the root or a
/// list element whose parent has no name.
fn path_tail(p: &automerge::Patch) -> String {
    for (_, prop) in p.path.iter().rev() {
        if let Prop::Map(name) = prop {
            return name.clone();
        }
    }
    String::new()
}

fn scalar_string(v: &Value<'_>) -> Option<String> {
    match v {
        Value::Scalar(s) => match s.as_ref() {
            ScalarValue::Str(t) => Some(t.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// A capability in one short phrase: what, over what, to whom.
fn describe_capability(cap: &CapOverride) -> String {
    let verb = match cap.verb {
        Verb::Read => "read",
        Verb::Comment => "comment",
        Verb::Write => "write",
        Verb::Invite => "invite",
        Verb::Admin => "admin",
    };
    let effect = match cap.effect {
        Effect::Grant => "",
        Effect::Restrict => "restricted ",
    };
    format!(
        "{effect}{verb} on {} to {}",
        describe_scope(&cap.scope),
        describe_subject(&cap.subject)
    )
}

fn describe_scope(scope: &Scope) -> String {
    match scope {
        Scope::Workspace => "this workspace".to_string(),
        Scope::App(a) => a.clone(),
        Scope::ResourceType(k) => format!("every {k:?}"),
        Scope::ResourceInstance(_) => "one artifact".to_string(),
        Scope::FsPath(p) => p.clone(),
        Scope::Device(d) => d.clone(),
        Scope::NetHost(h) => format!("network {h}"),
        Scope::Action { app, action } => format!("{app}:{action}"),
        Scope::Room(_) => "this room".to_string(),
    }
}

fn describe_subject(subject: &Subject) -> String {
    match subject {
        Subject::Role(r) => format!("{r:?}s"),
        Subject::User(_) => "a person".to_string(),
        Subject::Principal(p) => describe_principal(p),
    }
}

fn describe_principal(p: &Principal) -> String {
    match p {
        Principal::Person(id) => short(id),
        Principal::App(a) => a.clone(),
        Principal::Agent(a) => format!("agent {a}"),
        Principal::Token(_) => "an invite token".to_string(),
        Principal::Room(_) => "this room".to_string(),
    }
}

/// Keys are 64 hex characters; a row is 40 px tall. Show the prefix a person
/// can actually compare against another one.
fn short(id: &str) -> String {
    if id.len() > 8 {
        id.chars().take(8).collect()
    } else {
        id.to_string()
    }
}

fn plural(n: usize, noun: &str, verb: &str) -> String {
    if n == 1 {
        format!("{verb} 1 {noun}")
    } else {
        format!("{verb} {n} {noun}s")
    }
}

/// Name up to two windows, then count the rest — the row has a line, not a
/// paragraph.
fn join_titles(titles: &[String]) -> String {
    match titles.len() {
        0 => String::new(),
        1 => titles[0].clone(),
        2 => format!("{} and {}", titles[0], titles[1]),
        n => format!("{}, {} and {} more", titles[0], titles[1], n - 2),
    }
}
