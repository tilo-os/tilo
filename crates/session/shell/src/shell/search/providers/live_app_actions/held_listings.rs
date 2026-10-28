//! What a listing of the live app actions reads: the revision its cursors
//! carry, and the snapshot its first page took, held for its continuations
//! so a listing reads one consistent snapshot from its first page to its
//! last. Changes show on the next listing.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    AuxiliaryResume, AuxiliarySnapshot, ID_SEPARATOR, LiveAppActionsProvider, PendingPoll,
    SurfaceSnapshot,
};
use crate::shell::search::SearchContext;

/// How many queries' first pages are held for their continuations at once
/// ([`HeldListing`]): the listings in flight, the oldest let go first.
pub(super) const HELD_LISTINGS: usize = 16;

/// What a query's first page read, held for its continuations: the
/// revision its cursors carry, the local index it pages through and the
/// remote projections it has built. A listing reads this one snapshot from
/// its first page to its last.
pub(super) struct HeldListing {
    pub(super) revision: String,
    /// Who read it, and how ([`LiveAppActionsProvider::listing_reader`]):
    /// a snapshot holds the data, not its reader, so a continuation read
    /// by another Person, with programmatic rows otherwise, or in another
    /// language is stale.
    pub(super) reader: String,
    pub(super) surface: Option<Rc<SurfaceSnapshot>>,
    pub(super) projections: Vec<Rc<AuxiliarySnapshot>>,
}

impl LiveAppActionsProvider {
    /// Move the directory epoch, a part of every cursor's revision, saying
    /// why: an open palette and a listing read everything again on each
    /// move, so a move with nothing changed is churn worth finding.
    pub(super) fn bump_directory_epoch(&self, cause: &'static str) {
        let epoch = self.directory_epoch.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            target: "tilo::publications",
            epoch = epoch.wrapping_add(1),
            cause,
            "app actions directory epoch moved"
        );
    }

    /// The scope a remote projection for `ctx` is built for.
    pub(super) fn auxiliary_scope(&self, ctx: &SearchContext<'_>) -> String {
        if self.enumerating.get() && ctx.chrome.search_resource_scope.is_none() {
            format!("{}{ID_SEPARATOR}enumerating", Self::surface_scope(ctx))
        } else {
            Self::surface_scope(ctx)
        }
    }

    /// Take in the continuation pages that have arrived, and nothing else:
    /// what a listing's continuation may read beyond its first page. Such
    /// a page appends behind the cursor waiting on it, moving no revision,
    /// so the cursor resumes at it. First pages, failures and publication
    /// changes wait for the next query's first page. Projections built while
    /// one of those pages was owed are let go, so they are built again with
    /// it.
    pub(super) fn poll_appended_pages(&self) {
        let mut landed = Vec::new();
        self.directory_pending.borrow_mut().retain(|key, pending| {
            if pending.initial_page {
                return true;
            }
            match crate::shell::search::invocation_wake::poll(
                &mut pending.receiver,
                &crate::shell::search::invocation_wake::waker(),
            ) {
                PendingPoll::Ready(Ok(page)) if !page.stale_cursor => {
                    landed.push((key.clone(), page, false));
                    false
                }
                // A failed or stale reply is the next first page's to judge.
                PendingPoll::Ready(answer) => {
                    pending.receiver = tilo_actions::remote::Pending::ready(answer);
                    true
                }
                PendingPoll::Waiting | PendingPoll::Abandoned => true,
            }
        });
        if landed.is_empty() {
            return;
        }
        self.land_directory_pages(landed);
        for listing in self.held.borrow_mut().iter_mut() {
            listing
                .projections
                .retain(|projection| projection.pending_directory.is_empty());
        }
    }

    /// Hold what a query's first page read under `revision`, the revision
    /// its cursors carry. A first page starts afresh: projections an earlier
    /// listing of the same revision built may predate a page that has
    /// landed since, so they are not kept.
    pub(super) fn hold_listing(
        &self,
        revision: &str,
        reader: String,
        surface: Option<Rc<SurfaceSnapshot>>,
    ) {
        let mut held = self.held.borrow_mut();
        held.retain(|listing| listing.revision != revision);
        if held.len() >= HELD_LISTINGS {
            held.pop_front();
        }
        held.push_back(HeldListing {
            revision: revision.to_owned(),
            reader,
            surface,
            projections: Vec::new(),
        });
    }

    /// The remote projection for `needle` from `resume` the listing
    /// `revision` pages through: one it already built, else one built now
    /// from the pages cached and kept for the rest of the listing.
    pub(super) fn listing_projection(
        &self,
        ctx: &SearchContext<'_>,
        needle: &str,
        revision: &str,
        root_limit: usize,
        resume: &AuxiliaryResume,
    ) -> Option<Rc<AuxiliarySnapshot>> {
        let scope = self.auxiliary_scope(ctx);
        let held = self
            .held
            .borrow()
            .iter()
            .find(|listing| listing.revision == revision)
            .and_then(|listing| {
                listing
                    .projections
                    .iter()
                    .find(|projection| {
                        projection.query == needle
                            && projection.scope == scope
                            && projection.resume == *resume
                    })
                    .cloned()
            });
        if held.is_some() {
            return held;
        }
        self.prepare_auxiliary_snapshot(ctx, needle, revision, root_limit, resume);
        let projection = self.auxiliary_snapshot.borrow().clone()?;
        let mut held = self.held.borrow_mut();
        let listing = held.iter_mut().find(|listing| listing.revision == revision);
        if let Some(listing) = listing {
            listing.projections.push(Rc::clone(&projection));
        }
        drop(held);
        Some(projection)
    }

    /// What a cursor of this provider stays valid for: the local index,
    /// the directory pages a walk starts from, publishers, actor and
    /// locale. Pages appended behind a continuation leave it unchanged.
    pub(super) fn cursor_revision(&self, ctx: &SearchContext<'_>) -> String {
        let scope = Self::surface_scope(ctx);
        let source_generation = self.broker.surfaces_generation();
        let ready = self
            .surface_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.source_generation == source_generation && snapshot.scope == scope
            });
        format!(
            "{}:{source_generation}:{}:{}:{}:{}",
            if ready { "ready" } else { "indexing" },
            self.directory_epoch.load(Ordering::Relaxed),
            // Listing publishers is a memory read; it never asks an app.
            self.directory_revision(ctx.chrome),
            self.broker.statuses_generation(),
            Self::listing_reader(ctx),
        )
    }

    /// Who reads a listing, and how: the Person, whether programmatic rows
    /// are listed, and the desktop's language the rows are written in.
    pub(super) fn listing_reader(ctx: &SearchContext<'_>) -> String {
        format!(
            "{}:{}:{}",
            ctx.chrome.active_local_user(),
            ctx.include_programmatic,
            crate::shell::i18n::current_locale(),
        )
    }

    /// What [`Self::directory_roots_in`] answers changes with, read without
    /// opening a certified session: the broker's published surfaces and
    /// directory roots, and per workspace its local Person and that Person's
    /// identity files. Stamps and counters only, so the palette can ask on
    /// every tick.
    pub(super) fn directory_revision(
        &self,
        chrome: &crate::shell::chrome_state::ChromeState,
    ) -> String {
        use std::hash::{Hash as _, Hasher as _};
        /// Moves on every call while some identity can't be trusted to its
        /// stamp, so such a revision never matches the previous one.
        static UNSETTLED: AtomicU64 = AtomicU64::new(0);
        let rooms = chrome
            .tree()
            .desktop
            .workspaces
            .iter()
            .map(crate::shell::state::Workspace::room)
            .collect::<Vec<_>>();
        let mut identities = std::collections::hash_map::DefaultHasher::new();
        for room in rooms {
            let name = room.to_string();
            let Some(identity) = self.action_sessions.identity_revision(&name) else {
                let unsettled = UNSETTLED.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    target: "tilo::publications",
                    room = %name,
                    unsettled,
                    "a room's identity is unsettled; the app actions revision moves on every read"
                );
                return format!("unsettled-{unsettled}");
            };
            (name, chrome.local_user_for_room(&room), identity).hash(&mut identities);
        }
        format!(
            "{}.{}.{:016x}",
            self.broker.surfaces_generation(),
            self.broker.action_directory_root_page(None, 0).generation,
            identities.finish()
        )
    }
}
