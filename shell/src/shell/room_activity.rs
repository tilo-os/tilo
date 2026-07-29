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
//! # One document, several rooms
//!
//! [`WorkspaceTree::doc`] is **tree-wide**: a single automerge document whose
//! `workspaces` list holds every workspace on this machine. A change in it
//! therefore belongs to a particular workspace (a grant, a rename, a window
//! opened), to several at once (a cache flush rewrites the whole list), or to
//! none (the active-workspace pointer lives at the document root).
//!
//! So a pane titled with one room's name cannot simply render the whole
//! document — that would put another room's governance under this room's
//! heading. Each row is attributed to the workspace whose subtree the change
//! touched, and [`RoomScope`] decides which of them are shown. The default is
//! this room only; widening to the whole tree is explicit, and says so in the
//! header.
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
const SHEET_MAX_W: f64 = 560.0;
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
const HEADER_H: f64 = 58.0;
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

/// Which workspaces' changes the pane is showing.
///
/// The room document is shared by every workspace on the machine, so this is
/// a genuine scope decision rather than a filter: a row outside the scope is
/// not hidden, it is not this room's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RoomScope {
    /// Only changes that touched the active workspace, plus changes that
    /// belong to no single workspace (the active-workspace pointer, a flush
    /// that rewrote the whole list). Those cannot be attributed to another
    /// room, so they cannot carry one into this view.
    #[default]
    ThisRoom,
    /// Every change in the tree document, with each row labelled by the
    /// workspace it belongs to.
    AllWorkspaces,
}

impl RoomScope {
    pub const ALL: [RoomScope; 2] = [RoomScope::ThisRoom, RoomScope::AllWorkspaces];

    pub fn label(self) -> &'static str {
        match self {
            RoomScope::ThisRoom => "This room",
            RoomScope::AllWorkspaces => "All workspaces",
        }
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
    /// Every row the document yielded, with the workspace each belongs to.
    /// `None` means the change belongs to no single workspace. The timeline
    /// is handed the subset [`Self::scope`] selects out of this.
    derived: Vec<(HistoryEntry, Option<u64>)>,
    scope: RoomScope,
    /// Hit rects for the scope segments, cached by `draw`.
    scope_rects: Vec<(RoomScope, Rect)>,
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
    /// Active workspace at the last scope application. Switching rooms moves
    /// no changes, so the heads check would not notice — but it changes which
    /// rows are this room's.
    last_active: Option<u64>,
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
            timeline: {
                let mut t = ActivityTimeline::new().on_revert(move |id| ri.set(Some(*id)));
                // The sheet is the surface; the timeline sits on it.
                t.set_draws_own_surface(false);
                t
            },
            // A workspace is a cluster: a tiled set of running windows that
            // can be saved and reopened collaboratively. That is what this
            // document describes, so that is what its rows are tagged with.
            source: HistorySource::new().with_artifact_kind(ArtifactKind::Cluster),
            revert_intent,
            last_heads: Vec::new(),
            period: Period::default(),
            derived: Vec::new(),
            scope: RoomScope::default(),
            scope_rects: Vec::new(),
            author: None,
            contributors: Vec::new(),
            sheet: Rect::ZERO,
            period_rects: Vec::new(),
            author_rects: Vec::new(),
            room_name: String::new(),
            total_entries: 0,
            last_active: None,
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
        let active = tree.active.raw();
        if heads == self.last_heads && !self.timeline.entries().is_empty() {
            if self.last_active != Some(active) {
                self.apply_scope(tree);
            }
            return;
        }
        self.last_heads = heads;

        // Bind the local automerge actor to whoever is sitting at this machine.
        //
        // The qOS identity on disk is preferred — it carries a display name
        // and an avatar colour the user chose. Failing that (a shell running
        // outside a provisioned session), the actor is still *this* process's,
        // and this process writes on behalf of the local user, so it is named
        // with the same user id the capabilities in these very rows are
        // granted to. Neither is a guess; what is left unresolved is a peer's
        // actor, which honestly reads as an unknown participant until the
        // collaboration layer tells us who they are.
        let actor = tree.doc.get_actor().to_hex_string();
        if !self.source.authors().is_registered(&actor) {
            let registered = self.source.authors_mut().register_local_identity(&actor);
            if !registered {
                let me = crate::shell::state::local_user_id();
                if !me.is_empty() {
                    self.source.authors_mut().register(
                        &actor,
                        ui::widget::HistoryAuthor::new(
                            me,
                            ui::history_source::stable_hue(&actor),
                            ui::history_source::short_id(&actor),
                        ),
                    );
                }
            }
        }

        let mut entries = self.source.entries(&mut tree.doc);
        let tags = describe_room_changes(&mut tree.doc, &mut entries);
        self.derived = entries.into_iter().zip(tags).collect();
        self.apply_scope(tree);
    }

    /// Hand the timeline the rows [`Self::scope`] selects, labelled where a
    /// label is needed, and re-derive everything that depends on which rows
    /// are in play.
    fn apply_scope(&mut self, tree: &WorkspaceTree) {
        let active = tree.active.raw();
        self.last_active = Some(active);
        let names: std::collections::HashMap<u64, String> = tree
            .workspaces
            .iter()
            .map(|w| (w.id.raw(), w.name.clone()))
            .collect();

        let mut rows: Vec<HistoryEntry> = Vec::with_capacity(self.derived.len());
        for (entry, tag) in &self.derived {
            match (self.scope, tag) {
                // This room: its own changes, plus the ones that belong to no
                // room at all. A change owned by another workspace is simply
                // not this room's history.
                (RoomScope::ThisRoom, Some(id)) if *id != active => continue,
                (RoomScope::ThisRoom, _) => rows.push(entry.clone()),
                (RoomScope::AllWorkspaces, tag) => {
                    let mut e = entry.clone();
                    // Across the whole tree a row is meaningless without
                    // saying which room it happened in — three identical
                    // "Set the room owner" lines are noise otherwise.
                    if let Some(id) = tag {
                        let label = names
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| "a removed workspace".to_string());
                        e.summary = format!("{label} \u{00b7} {}", e.summary);
                    }
                    rows.push(e);
                }
            }
        }
        self.total_entries = rows.len();

        let mut seen: HashSet<String> = HashSet::new();
        self.contributors = rows
            .iter()
            .filter(|e| seen.insert(e.author.name.clone()))
            .map(|e| e.author.name.clone())
            .collect();
        // A contributor filter naming someone who no longer appears would hide
        // everything with no way back except clearing it, so drop it.
        let stale_author = self
            .author
            .as_ref()
            .is_some_and(|a| !self.contributors.iter().any(|c| c == a));
        if stale_author {
            self.author = None;
        }

        self.timeline.set_entries(rows);
        self.reinstall_filter();
    }

    /// Widen or narrow which workspaces the pane covers.
    pub fn set_scope(&mut self, scope: RoomScope, tree: &WorkspaceTree) {
        if self.scope != scope {
            self.scope = scope;
            self.apply_scope(tree);
        }
    }

    pub fn scope(&self) -> RoomScope {
        self.scope
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
        let rows_y0 = (self.sheet.y0 + self.chrome_height()).min(self.sheet.y1);
        let rows_y1 = (self.sheet.y1 - 8.0).max(rows_y0);
        self.timeline.set_rect(Rect::new(
            self.sheet.x0 + 8.0,
            rows_y0,
            (self.sheet.x1 - 8.0).max(self.sheet.x0 + 8.0),
            rows_y1,
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
        let title = match self.scope {
            RoomScope::ThisRoom => "Room activity",
            RoomScope::AllWorkspaces => "Workspace activity",
        };
        tr.draw_text(
            scene,
            title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((self.sheet.x0 + 16.0, self.sheet.y0 + 12.0)),
            None,
        );
        let sub_size = (tokens.typography.scale.sm - 1.0).max(10.0);
        // The subtitle is the pane's scope, spelled out. A title naming one
        // room over a list drawn from every room would be a lie the reader
        // has no way to catch.
        let subtitle = match self.scope {
            RoomScope::ThisRoom => {
                let room = if self.room_name.is_empty() {
                    "This room"
                } else {
                    self.room_name.as_str()
                };
                format!(
                    "{room} \u{00b7} {} changes to this workspace, its people and its permissions",
                    self.total_entries
                )
            }
            RoomScope::AllWorkspaces => format!(
                "Every workspace on this machine \u{00b7} {} changes",
                self.total_entries
            ),
        };
        tr.draw_text(
            scene,
            &subtitle,
            sub_size,
            colors.text_dim,
            transform * Affine::translate((self.sheet.x0 + 16.0, self.sheet.y0 + 32.0)),
            Some((self.sheet.width() - 32.0) as f32),
        );

        // Scope switch, right-aligned in the header. Two segments rather than
        // a chip row: this is not another filter, it is which document region
        // the pane is reading.
        self.scope_rects.clear();
        {
            let size = (tokens.typography.scale.sm - 1.0).max(10.0);
            let h = 22.0_f64;
            let cy = self.sheet.y0 + 24.0;
            let mut widths: Vec<f64> = Vec::with_capacity(2);
            for sc in RoomScope::ALL {
                widths.push(tr.measure(sc.label(), size, None).0 as f64 + 16.0);
            }
            let total: f64 = widths.iter().sum::<f64>() + 4.0;
            // Only offered when there is room for it; the sheet stays usable
            // at every width, and the default scope is the safe one.
            if total < self.sheet.width() * 0.62 {
                let mut x = self.sheet.x1 - 16.0 - total;
                for (i, sc) in RoomScope::ALL.iter().enumerate() {
                    let rect = Rect::new(x, cy - h / 2.0, x + widths[i], cy + h / 2.0);
                    self.scope_rects.push((*sc, rect));
                    let active = self.scope == *sc;
                    let shape = RoundedRect::from_rect(rect, h / 2.0);
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
                    let (lw, lh) = tr.measure(sc.label(), size, None);
                    tr.draw_text(
                        scene,
                        sc.label(),
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
                    x += widths[i] + 4.0;
                }
            }
        }

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
            for (scope, rect) in self.scope_rects.clone() {
                if rect.contains(p) {
                    self.set_scope(scope, tree);
                    return RoomActivityOutcome {
                        handled: true,
                        reverted: false,
                    };
                }
            }
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
        // `current` was read before the patches were written, and reading heads
        // is what closes an automerge transaction — asking for them again here
        // would commit the revert *unstamped* and then compare the result
        // against itself. Commit deliberately, then compare against the heads
        // from before the write.
        tree.commit_room_change();
        if tree.doc.get_heads() == current {
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
/// Returns, per entry and in the same order, the workspace the change belongs
/// to — `None` when it belongs to several or to none.
fn describe_room_changes(
    doc: &mut automerge::AutoCommit,
    entries: &mut [HistoryEntry],
) -> Vec<Option<u64>> {
    let mut tags: Vec<Option<u64>> = vec![None; entries.len()];
    if entries.is_empty() {
        return tags;
    }
    // One pass over the change list, indexed so a long-lived room does not
    // pay a linear scan per change. A change's own `deps` are the document
    // heads immediately before it, so diffing `deps -> [hash]` isolates
    // exactly that change without having to rebuild a prefix per entry — and
    // the diff only runs for changes that have a row, which is what keeps the
    // cost proportional to what is on screen rather than to the whole graph.
    let mut by_id: std::collections::HashMap<HistoryEntryId, usize> =
        std::collections::HashMap::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        by_id.insert(e.id, i);
    }
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
        let Some(&i) = by_id.get(&id) else {
            continue;
        };
        let patches = doc.diff(&deps, &[hash]);
        tags[i] = workspace_touched(doc, &patches, hash);
        if let Some((summary, op_class)) = describe_patches(&patches) {
            entries[i].summary = summary;
            entries[i].op_class = op_class;
        }
    }
    tags
}

/// The workspace a change belongs to, or `None` when it belongs to more than
/// one or to none.
///
/// Every path into a workspace's subtree runs `workspaces -> [i] -> …`, so the
/// third path element's object is that workspace's entry map whatever depth
/// the patch lands at, and a patch on the entry itself is that map. The entry
/// is read **at the change's own heads**: a cache flush deletes and re-inserts
/// every entry, so the object a historical change wrote to no longer exists at
/// the document's current state.
fn workspace_touched(
    doc: &automerge::AutoCommit,
    patches: &[automerge::Patch],
    hash: ChangeHash,
) -> Option<u64> {
    let heads = [hash];
    let mut found: Option<u64> = None;
    for p in patches {
        let entry_obj = match p.path.len() {
            0 | 1 => continue, // the document root: not a workspace's change
            2 => p.obj.clone(),
            _ => p.path[2].0.clone(),
        };
        // Guard the shape: the first hop must be into `workspaces`, and the
        // second must be a list index. Anything else is a document layout we
        // do not recognise, and guessing at it would be worse than declining.
        if !matches!(&p.path[0].1, Prop::Map(k) if k == "workspaces") {
            continue;
        }
        if !matches!(p.path[1].1, Prop::Seq(_)) {
            continue;
        }
        let id = match doc.get_at(&entry_obj, "id", &heads) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Uint(n) => *n,
                _ => continue,
            },
            _ => continue,
        };
        match found {
            None => found = Some(id),
            // Two workspaces in one change — a whole-tree flush. It is nobody's
            // room in particular.
            Some(prev) if prev != id => return None,
            Some(_) => {}
        }
    }
    found
}

/// Map a change's patches onto a room event, or `None` when nothing in it is
/// recognised.
fn describe_patches(patches: &[automerge::Patch]) -> Option<(String, OpClass)> {
    let mut grants: Vec<String> = Vec::new();
    let mut revokes = 0usize;
    let mut names: Vec<String> = Vec::new();
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
                if let Some(n) = scalar_string(&value.0) {
                    names.push(n);
                }
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

    // A whole-list rewrite is the shell flushing its cached tree back into the
    // document (`seed_doc_from_cache`). It touches every field of every
    // workspace — including the capability map — so it has to be recognised
    // *before* the individual-field cases, or a flush that granted nothing
    // reads as "granted 4 capabilities".
    if workspaces_removed > 0 && workspaces_added == 0 {
        return Some((
            plural(workspaces_removed, "workspace", "Removed"),
            OpClass::Tombstone,
        ));
    }
    if workspaces_added > 0 {
        let s = match names.len() {
            0 => plural(workspaces_added, "workspace", "Recorded"),
            1 => format!("Recorded workspace \u{201c}{}\u{201d}", names[0]),
            n => format!("Recorded {n} workspaces: {}", join_titles(&names)),
        };
        return Some((s, OpClass::Splice));
    }

    // Governance next: a change that handed out or took away authority is
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
    if !names.is_empty() {
        let name = &names[0];
        let s = if name.is_empty() {
            "Renamed the workspace".to_string()
        } else {
            format!("Named the workspace \u{201c}{name}\u{201d}")
        };
        return Some((s, OpClass::EditMeta));
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

/// A person id is either a readable account name or a 64-character public key.
/// Show the first whole, and enough of the second to compare against another
/// one — chopping every id to eight characters turned `test-owner` into
/// `test-own`.
fn short(id: &str) -> String {
    const READABLE_MAX: usize = 24;
    if id.chars().count() <= READABLE_MAX {
        return id.to_string();
    }
    let mut out: String = id.chars().take(12).collect();
    out.push('\u{2026}');
    out
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

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    //! Every fixture below mutates the room through the *production*
    //! mirrors — `adopt_ownerless_workspaces`, `grant_in_workspace`,
    //! `revoke_in_workspace`, `doc_append_tab_to_first_group_of_active_workspace`
    //! — and then reads the pane back. Nothing writes a synthetic
    //! `HistoryEntry`: if the shell stopped recording a room change, or
    //! recorded it without a timestamp, these fail.

    use super::*;
    use crate::shell::state::{WorkspaceId, WorkspaceTree};
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    use ui::widget::op_class_is_revertible;

    /// A room with an owner, one grant, one revoke, one renamed workspace and
    /// one window opened — a plausible half-hour in a workspace.
    fn busy_room() -> (WorkspaceTree, WorkspaceId, String) {
        let mut tree = WorkspaceTree::bootstrap_default();
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.active;

        let granted = tree
            .grant_in_workspace(
                ws_id,
                &user,
                CapOverride::grant(
                    Subject::app("files"),
                    Scope::FsPath("/home/quinten/notes".to_string()),
                    Verb::Read,
                ),
            )
            .expect("the owner may grant in their own room");
        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::Device("camera".to_string()),
                Verb::Read,
            ),
        )
        .expect("the owner may grant in their own room");
        assert!(tree.revoke_in_workspace(ws_id, &user, &granted));

        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Design review".to_string();
        }
        tree.seed_doc_from_cache();
        (tree, ws_id, user)
    }

    fn pane_over(tree: &mut WorkspaceTree) -> RoomActivityPane {
        let mut pane = RoomActivityPane::new();
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(tree);
        pane
    }

    #[test]
    fn rows_come_from_the_rooms_own_change_graph() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() >= 4,
            "bootstrap + owner + two grants + a revoke is at least four changes, got {}",
            pane.entries().len()
        );
        let summaries: Vec<&str> = pane.entries().iter().map(|e| e.summary.as_str()).collect();
        assert!(
            summaries.iter().any(|s| s.contains("/home/quinten/notes")),
            "the filesystem grant is named by the path it granted: {summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.starts_with("Revoked")),
            "the revoke is its own row: {summaries:?}"
        );
        // The rename reaches the document through `seed_doc_from_cache`, which
        // rewrites the whole workspace list — so the row is the flush, and it
        // has to name what it wrote or it says nothing at all.
        assert!(
            summaries.iter().any(|s| s.contains("Design review")),
            "the flush that carried the rename names it: {summaries:?}"
        );
        assert!(
            !summaries.iter().any(|s| s == &"Granted 4 capabilities"),
            "a flush that granted nothing must not read as a grant: {summaries:?}"
        );
    }

    /// Give the tree a second workspace with governance of its own, so the
    /// two rooms' histories are genuinely interleaved in one document.
    fn two_room_tree() -> (WorkspaceTree, WorkspaceId, WorkspaceId) {
        let (mut tree, first, user) = busy_room();
        let second = tree.append_empty_workspace("Research");
        tree.adopt_ownerless_workspaces(&user);
        tree.grant_in_workspace(
            second,
            &user,
            CapOverride::grant(
                Subject::app("browser"),
                Scope::NetHost("research.example".to_string()),
                Verb::Read,
            ),
        )
        .expect("owner may grant in the second room too");
        // Make the first workspace the active one again — `append_empty_workspace`
        // does not switch, but be explicit rather than relying on that.
        tree.set_active(first);
        (tree, first, second)
    }

    #[test]
    fn this_rooms_scope_excludes_another_workspaces_governance() {
        // The room document is tree-wide. A pane headed with one room's name
        // must not show a different room's grants — that is the whole reason
        // the scope exists.
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        assert_eq!(pane.scope(), RoomScope::ThisRoom, "scoped by default");

        let scoped: Vec<String> = pane
            .entries()
            .iter()
            .map(|e| e.summary.clone())
            .collect();
        assert!(
            !scoped.iter().any(|s| s.contains("research.example")),
            "the other room's network grant leaked into this room: {scoped:?}"
        );
        assert!(
            scoped.iter().any(|s| s.contains("/home/quinten/notes")),
            "this room's own grant is present: {scoped:?}"
        );

        let all_tree = { tree.workspaces.clone() };
        let _ = all_tree;
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let widened: Vec<String> = pane
            .entries()
            .iter()
            .map(|e| e.summary.clone())
            .collect();
        assert!(
            widened.iter().any(|s| s.contains("research.example")),
            "widening the scope must reveal the other room: {widened:?}"
        );
        assert!(
            widened.len() > scoped.len(),
            "the whole tree is strictly more than one room: {} vs {}",
            widened.len(),
            scoped.len()
        );
    }

    #[test]
    fn a_widened_row_names_the_workspace_it_happened_in() {
        // Across the tree, "Set the room owner" three times is noise unless
        // each says which room.
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let owner_rows: Vec<String> = pane
            .entries()
            .iter()
            .filter(|e| e.summary.contains("Set the room owner"))
            .map(|e| e.summary.clone())
            .collect();
        assert!(
            owner_rows.len() > 1,
            "the fixture has several workspaces adopting an owner: {owner_rows:?}"
        );
        assert!(
            owner_rows.iter().any(|s| s.starts_with("Research \u{00b7} ")),
            "a row must name the room it happened in: {owner_rows:?}"
        );
        let distinct: std::collections::HashSet<&String> = owner_rows.iter().collect();
        assert_eq!(
            distinct.len(),
            owner_rows.len(),
            "no two rows may read identically: {owner_rows:?}"
        );
    }

    #[test]
    fn switching_the_active_workspace_re_scopes_without_a_document_change() {
        // Switching rooms moves no changes, so the heads check would not
        // notice — but it changes whose history this is.
        let (mut tree, first, second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        let in_first: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(in_first.iter().any(|s| s.contains("/home/quinten/notes")));

        tree.set_active(second);
        pane.refresh(&mut tree);
        let in_second: Vec<String> = pane.entries().iter().map(|e| e.summary.clone()).collect();
        assert!(
            in_second.iter().any(|s| s.contains("research.example")),
            "the second room's own grant: {in_second:?}"
        );
        assert!(
            !in_second.iter().any(|s| s.contains("/home/quinten/notes")),
            "the first room's grant must not follow us: {in_second:?}"
        );
        let _ = first;
    }

    #[test]
    fn a_whole_tree_flush_belongs_to_no_single_room() {
        // `seed_doc_from_cache` rewrites every workspace at once. Attributing
        // that to whichever entry the patch walk happened to see first would
        // be a guess; it is shown in both scopes because it cannot carry
        // another room's business into this one.
        let (mut tree, _first, _second) = two_room_tree();
        let mut pane = pane_over(&mut tree);
        let scoped_flushes = pane
            .entries()
            .iter()
            .filter(|e| e.summary.starts_with("Recorded "))
            .count();
        assert!(scoped_flushes > 0, "the fixture flushed at least once");
        pane.set_scope(RoomScope::AllWorkspaces, &tree);
        let widened_flushes = pane
            .entries()
            .iter()
            .filter(|e| e.summary.starts_with("Recorded "))
            .count();
        assert_eq!(
            scoped_flushes, widened_flushes,
            "a flush is unattributed, so widening must not add or drop one"
        );
    }

    #[test]
    fn the_local_users_own_rows_are_attributed_to_them() {
        // A room where every change was made locally must not read as a wall
        // of unknown participants. The local actor is this process's, and this
        // process writes for the person at the machine.
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        let me = crate::shell::state::local_user_id();
        assert!(!me.is_empty(), "there is always a local user id");
        assert!(
            pane.entries()
                .iter()
                .all(|e| e.author.name != ui::history_source::UNKNOWN_AUTHOR_NAME),
            "local changes resolved to an unknown participant: {:?}",
            pane.contributors()
        );
        assert_eq!(
            pane.contributors().len(),
            1,
            "one person did all of this: {:?}",
            pane.contributors()
        );
    }

    #[test]
    fn every_row_carries_a_real_timestamp() {
        // The whole date filter rests on this. Automerge leaves a change's
        // advisory time at zero unless the committer supplies one, and a row
        // at the epoch is a row no date range can ever select.
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        assert!(!pane.entries().is_empty());
        for e in pane.entries() {
            assert!(
                e.timestamp > SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000),
                "row {:?} is stamped at {:?} — the room's commits are not carrying a clock",
                e.summary,
                e.timestamp
            );
        }
    }

    #[test]
    fn date_range_filter_partitions_the_real_history() {
        // One change deliberately stamped a week back through the same
        // `commit_stamped_at` the FS layer uses when it replays a durable
        // chain, the rest at now. "Today" must keep the recent ones and drop
        // the old one; "30 days" must keep everything.
        let mut tree = WorkspaceTree::bootstrap_default();
        let user: String = "test-owner".to_string();
        tree.adopt_ownerless_workspaces(&user);
        let ws_id = tree.active;

        let week_ago = SystemTime::now() - Duration::from_secs(7 * 24 * 3600);
        let week_ago_secs = week_ago
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
            9001,
            "Last week's window".to_string(),
            b"{}".to_vec(),
        ));
        qdoc::history::commit_stamped_at(&mut tree.doc, week_ago_secs)
            .expect("the append left a transaction to stamp");

        tree.grant_in_workspace(
            ws_id,
            &user,
            CapOverride::grant(
                Subject::app("files"),
                Scope::FsPath("/home/quinten/today".to_string()),
                Verb::Read,
            ),
        )
        .expect("owner may grant");

        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();
        assert!(all >= 3, "expected the full history, got {all}");

        pane.set_period(Period::Today);
        let today = pane.visible_len();
        assert!(today > 0, "today's changes must survive the day filter");
        assert!(
            today < all,
            "the week-old change must be filtered out: {today} of {all} survived"
        );
        assert!(
            !pane
                .timeline()
                .entries()
                .is_empty(),
            "filtering hides rows, it does not discard them"
        );
        for i in 0..pane.visible_len() {
            let e = pane.visible_entry(i).unwrap();
            assert!(
                !e.summary.contains("Last week"),
                "the week-old row leaked through the day filter: {}",
                e.summary
            );
        }

        pane.set_period(Period::Month);
        assert_eq!(
            pane.visible_len(),
            all,
            "a 30-day window covers a change from seven days ago"
        );
    }

    #[test]
    fn contributor_filter_narrows_to_one_person() {
        // Two actors write into the same room document — the local one and a
        // peer whose changes merged in. The people row must list both, and
        // picking one must hide the other's rows.
        let (mut tree, _, _) = busy_room();

        let mut peer = automerge::AutoCommit::new();
        peer.set_actor(automerge::ActorId::from("bob".as_bytes()));
        peer.apply_changes(
            tree.export_changes_since(&[])
                .iter()
                .map(|b| automerge::Change::try_from(b.as_slice()).unwrap())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        {
            use automerge::transaction::Transactable;
            let (_, ws_entry) = peer
                .get(&peer.get(&automerge::ROOT, "workspaces").unwrap().unwrap().1, 0_usize)
                .unwrap()
                .unwrap();
            peer.put(&ws_entry, "name", "Bob's rename").unwrap();
        }
        qdoc::history::commit_stamped(&mut peer).expect("the peer's edit commits");
        let blobs: Vec<Vec<u8>> = peer
            .get_changes(&tree.doc.get_heads())
            .into_iter()
            .map(|c| c.raw_bytes().to_vec())
            .collect();
        tree.apply_changes(&blobs).unwrap();

        let mut pane = pane_over(&mut tree);
        assert!(
            pane.contributors().len() >= 2,
            "two actors wrote to this room; contributors = {:?}",
            pane.contributors()
        );
        let bob = pane
            .contributors()
            .iter()
            .find(|c| c.as_str() == "bob")
            .cloned()
            .expect("the peer's self-declared handle resolves to a contributor");

        let everyone = pane.visible_len();
        pane.set_author_filter(Some(bob.clone()));
        let just_bob = pane.visible_len();
        assert!(just_bob > 0, "bob authored at least one row");
        assert!(
            just_bob < everyone,
            "filtering to bob must hide the local actor's rows: {just_bob} of {everyone}"
        );
        for i in 0..pane.visible_len() {
            assert_eq!(pane.visible_entry(i).unwrap().author.name, bob);
        }

        pane.set_author_filter(None);
        assert_eq!(pane.visible_len(), everyone, "clearing restores every row");
    }

    #[test]
    fn action_type_chips_narrow_by_op_class() {
        use ui::widget::activity_timeline::ChipKind;
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let all = pane.visible_len();

        pane.timeline_mut().set_active_chip(ChipKind::Governance);
        let gov = pane.visible_len();
        assert!(
            gov > 0,
            "grants and revokes are governance; the room has some"
        );
        assert!(gov < all, "governance is a subset: {gov} of {all}");
        for i in 0..pane.visible_len() {
            assert_eq!(
                pane.visible_entry(i).unwrap().op_class,
                OpClass::Govern,
                "the Governance chip must only show governance rows"
            );
        }

        pane.timeline_mut().set_active_chip(ChipKind::All);
        assert_eq!(pane.visible_len(), all);
    }

    #[test]
    fn reverting_a_row_writes_a_new_forward_change() {
        // The covenant for history: a revert appends. The reverted-past
        // version must still be in the graph afterwards, and the room's
        // readable state must be what it was at the target.
        let (mut tree, ws_id, user) = busy_room();
        let mut pane = pane_over(&mut tree);

        let before_name = tree
            .workspaces
            .iter()
            .find(|w| w.id == ws_id)
            .map(|w| w.name.clone())
            .unwrap();
        assert_eq!(before_name, "Design review");
        let versions_before = pane.entries().len();

        // Rename again, so there is something to revert back across.
        if let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) {
            ws.name = "Renamed by mistake".to_string();
        }
        tree.seed_doc_from_cache();
        pane.refresh(&mut tree);
        assert!(pane.entries().len() > versions_before);

        // Revert to the version the mistake was made on top of.
        let target = pane.entries()[versions_before - 1].id;
        assert!(pane.revert_to(&mut tree, &target), "the revert wrote something");

        assert_eq!(
            tree.workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .map(|w| w.name.as_str()),
            Some("Design review"),
            "the room is back to the state the row named"
        );
        assert!(
            pane.entries().len() > versions_before + 1,
            "the revert is a new row on top, not a rewind: {} rows",
            pane.entries().len()
        );
        assert!(
            pane.entries().iter().any(|e| e.id == target),
            "the version reverted to is still in the graph"
        );
        let _ = user;
    }

    #[test]
    fn reverting_an_unknown_row_is_a_no_op() {
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        let before = tree.doc.get_heads();
        assert!(!pane.revert_to(&mut tree, &HistoryEntryId([0x5a; 16])));
        assert_eq!(tree.doc.get_heads(), before, "nothing was written");
    }

    #[test]
    fn governance_rows_never_offer_revert() {
        // A grant or a revoke is countered, not rewound. The pane must not
        // paint a chip that would silently undo someone's authority.
        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.timeline_mut()
            .set_active_chip(ui::widget::activity_timeline::ChipKind::Governance);
        assert!(pane.visible_len() > 0);
        for i in 0..pane.visible_len() {
            let e = pane.visible_entry(i).unwrap();
            assert!(
                !op_class_is_revertible(e.op_class),
                "{:?} offered a revert chip",
                e.summary
            );
            assert!(
                pane.timeline().revert_chip_rect(i).is_none(),
                "row {i} ({:?}) laid out a revert chip",
                e.summary
            );
        }
    }

    #[test]
    fn a_capability_is_described_in_the_rooms_vocabulary() {
        let (mut tree, _, _) = busy_room();
        let pane = pane_over(&mut tree);
        let grant = pane
            .entries()
            .iter()
            .find(|e| e.summary.contains("/home/quinten/notes"))
            .expect("the filesystem grant has a row");
        assert!(
            grant.summary.starts_with("Granted read on /home/quinten/notes to files"),
            "the grant names verb, scope and subject: {:?}",
            grant.summary
        );
        assert_eq!(grant.op_class, OpClass::Govern);
    }

    #[test]
    fn an_empty_room_and_a_filtered_one_do_not_read_the_same() {
        let mut empty = WorkspaceTree::default();
        let pane = pane_over(&mut empty);
        let (title, detail) = pane.timeline().empty_state_lines();
        assert_eq!(title, "No activity yet");
        assert!(detail.is_none(), "nothing to explain when nothing happened");

        let (mut tree, _, _) = busy_room();
        let mut pane = pane_over(&mut tree);
        pane.set_author_filter(Some("nobody-by-that-name".to_string()));
        assert_eq!(pane.visible_len(), 0);
        let (title, detail) = pane.timeline().empty_state_lines();
        assert_eq!(title, "No activity matches these filters");
        let detail = detail.expect("a filtered-empty list must explain itself");
        assert!(
            detail.contains("nobody-by-that-name"),
            "the explanation names the filter doing the hiding: {detail}"
        );
        assert!(
            detail.contains(&pane.entries().len().to_string()),
            "the explanation says how many rows are hidden: {detail}"
        );
    }

    #[test]
    fn the_sheet_is_a_side_panel_when_wide_and_full_bleed_when_narrow() {
        let mut pane = RoomActivityPane::new();

        pane.layout(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        let wide = pane.rect();
        assert!(
            wide.width() <= SHEET_MAX_W,
            "a 1920 px viewport must not get a 700 px sheet: {}",
            wide.width()
        );
        assert!(
            wide.x0 > 1920.0 * 0.5,
            "the sheet is anchored right, leaving the room visible behind it"
        );

        pane.layout(Rect::new(0.0, 0.0, 640.0, 480.0));
        let narrow = pane.rect();
        assert!(
            narrow.width() > 640.0 * 0.9,
            "under {FULL_BLEED_UNDER} px the sheet takes the viewport: {}",
            narrow.width()
        );
        assert!(narrow.x0 < 32.0, "full bleed starts at the left inset");
    }

    #[test]
    fn a_short_viewport_drops_the_chip_rows_before_the_row_list() {
        let mut pane = RoomActivityPane::new();
        pane.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(pane.shows_chip_rows(), "a normal viewport affords the filters");
        let tall_rows = pane.timeline().rect().height();

        pane.layout(Rect::new(0.0, 0.0, 1280.0, 220.0));
        assert!(
            !pane.shows_chip_rows(),
            "a 220 px viewport must spend its height on rows, not chips"
        );
        assert!(
            pane.timeline().rect().height() > 0.0,
            "the row list survives even when the filters do not"
        );
        assert!(tall_rows > pane.timeline().rect().height());
    }

    #[test]
    fn contributors_are_only_people_who_actually_did_something() {
        // The room has collaborators recorded on the tree who have never
        // touched the document. They must not appear as contributors — the
        // people row answers "who changed this", not "who is in the room".
        let (mut tree, _, _) = busy_room();
        tree.collaborators.push(crate::shell::state::Collaborator {
            id: "ghost".into(),
            name: "Never Edited".into(),
            active_tab: None,
            cursor_color: None,
            avatar: None,
            role_id: None,
        });
        let pane = pane_over(&mut tree);
        assert!(
            !pane.contributors().iter().any(|c| c == "Never Edited"),
            "a peer who never wrote must not be listed: {:?}",
            pane.contributors()
        );
    }

    #[test]
    fn a_long_lived_room_stays_bounded() {
        // Well past the producer's ceiling, all of it real: each iteration is
        // the same mirror the drop handler calls when a file is dropped into
        // a workspace. The pane must show the recent end of the history
        // rather than growing without limit.
        let mut tree = WorkspaceTree::bootstrap_default();
        for i in 0..240u64 {
            assert!(tree.doc_append_tab_to_first_group_of_active_workspace(
                50_000 + i,
                format!("Window {i}"),
                b"{}".to_vec(),
            ));
            tree.commit_room_change();
        }
        let pane = pane_over(&mut tree);
        assert!(
            pane.entries().len() <= ui::history_source::DEFAULT_MAX_ENTRIES,
            "the pane derived {} rows from a 240-change room",
            pane.entries().len()
        );
        assert!(
            pane.entries().len() > 100,
            "and it is showing a real window of them, not a token few"
        );
        // The newest change is the one that must survive the cap.
        assert!(
            pane.entries()
                .iter()
                .any(|e| e.summary.contains("Window 239")),
            "the most recent change is inside the window"
        );
    }

    #[test]
    fn period_windows_are_half_open_and_all_time_is_unbounded() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(Period::AllTime.range(now).is_none());
        let (start, end) = Period::Week.range(now).unwrap();
        assert_eq!(now.duration_since(start).unwrap(), Duration::from_secs(7 * 24 * 3600));
        assert!(end > now, "a change committed this instant is inside the window");
    }
}
