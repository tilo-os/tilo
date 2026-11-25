//! The workspace's one history: every document's steps (undo, redo, restore and checkpoints),
//! the revert they are taken back by, and who wrote them, for every app's records and whichever
//! language declared them; and each resource's versions across the documents it is in
//! ([`Version`]).
//!
//! The toolkit's `ui::history_source` builds the History views on this and keeps its paths; a
//! tilolang host, the shell and a process without the toolkit read and move the same history
//! through the same code.
//!
//! Lazy, paged access to a channel's op history ([`HistoryPager`]).
//!
//! A member who joins a channel receives the current state (the latest
//! snapshot) and the key material for everything older, not the history
//! itself: history can be gigabytes and most of it is never looked at.
//! Older ops stay with the hosts that retain them (brokers, relays) and are
//! fetched on demand, a page at a time, newest first — the same
//! `before_seq` scrollback request brokers already serve.
//!
//! [`HistoryPager`] is the client side of that exchange for one artifact:
//! it remembers how far back history is loaded, issues at most one request
//! at a time, prefetches when a view comes within a margin of the loaded
//! edge, and keeps paging when the caller needs a specific older point
//! (opening an old version, searching backwards). It is transport- and
//! crypto-agnostic; callers decrypt pages with keys they already hold.

pub mod authors;
mod built_on;
pub mod feed;
mod patches;
pub mod revert;
mod sequence_replay;
pub mod steps;
pub mod typing;
mod undo_log;
pub mod verbs;
mod versions;
pub mod words;

pub use patches::{
    write_patches, write_revert_patches, write_revert_patches_where, write_selected_patches,
};
#[doc(hidden)]
pub use undo_log::{HistoryMoment, OwnedActionAttribution, OwnedActionReceipt, same_frontier};
pub use undo_log::{InstanceId, UndoEntry, UndoLog};
pub use versions::{Nested, Version, VersionKind, touched_by};
pub use versions::{Touches, stamped};

use automerge::{PatchAction, Prop};

/// The hash is the document's own content address, so two peers
/// independently building a timeline over the same history agree on the
/// ids — what lets one peer's `[Revert]` mean anything to another.
pub fn entry_id_from_hash(change_hash_hex: &str) -> HistoryEntryId {
    let mut bytes = [0u8; 16];
    let src = change_hash_hex.as_bytes();
    for (i, slot) in bytes.iter_mut().enumerate() {
        // `i` walks a fixed 16-byte array, so `idx` never approaches
        // `usize::MAX`; `saturating_mul`/`saturating_add` are a no-op here
        // and only exist to satisfy the arithmetic-side-effects lint.
        let idx = i.saturating_mul(2);
        let (hi, lo) = (src.get(idx), src.get(idx.saturating_add(1)));
        let hi = hi.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        let lo = lo.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        // `to_digit(16)` only ever returns `0..=15`, so `combined` is always
        // `0..=255` and the `try_from` below can never actually fail.
        let combined = hi.saturating_mul(16).saturating_add(lo);
        *slot = u8::try_from(combined).unwrap_or(u8::MAX);
    }
    HistoryEntryId(bytes)
}

// History's own metadata about other changes, kept in the document beside
// them: the toolkit's `groups`, `runs` and `descriptions` read and write it.

/// Namespaced so it cannot collide with an app's own schema: `tilo-doc` uses
/// `content` / `comments` / `props`, and the shell's workspace document uses
/// `workspaces` / `active`.
pub const GROUPS_KEY: &str = "tilo:group:";

/// The longest name of a run of changes: a label, not a paragraph.
pub const MAX_RUN_NAME: usize = 120;

/// The id history names a change by in a run's name: the first half of its hash, in hex.
#[must_use]
pub fn run_id(change: &automerge::ChangeHash) -> String {
    use std::fmt::Write as _;
    change.0[..16].iter().fold(String::new(), |mut id, byte| {
        let _ = write!(id, "{byte:02x}");
        id
    })
}

/// Name the run of changes from `start` to `end` (open while `end` is `None`), each as
/// [`run_id`] gives it, `label`, as `by` wrote it, replacing the name `replaces` holds; an empty
/// label only removes that name. Two people naming one run write one key, the run itself, which
/// settles the same way on every peer. Leaves the transaction open. The key of the name it
/// wrote, if it wrote one.
pub fn name_run(
    doc: &mut automerge::AutoCommit,
    (start, end): (&str, Option<&str>),
    label: &str,
    by: &str,
    replaces: Option<&str>,
) -> Option<String> {
    use automerge::transaction::Transactable as _;
    let label: String = label.trim().chars().take(MAX_RUN_NAME).collect();
    let key = (!label.is_empty()).then(|| format!("{GROUPS_KEY}{start}"));
    if let Some(replaced) = replaces
        .filter(|replaced| replaced.starts_with(GROUPS_KEY) && Some(*replaced) != key.as_deref())
    {
        let _ = doc.delete(automerge::ROOT, replaced);
    }
    let key = key?;
    // The whole record is written again, so no peer reads a new label against an old run.
    let record = doc
        .put_object(automerge::ROOT, &key, automerge::ObjType::Map)
        .ok()?;
    let _ = doc.put(&record, "label", label);
    if let Some(end) = end {
        let _ = doc.put(&record, "end", end);
    }
    let _ = doc.put(&record, "by", by);
    Some(key)
}

/// True when every patch in `patches` lands inside the named-range map — used
/// by the producer to keep naming out of the row list. An empty patch set is
/// **not** a marker: a change that did nothing visible is still a change, and
/// hiding it would be incorrect behaviour.
pub fn is_group_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(GROUPS_KEY),
            Some(_) => false,
            // A patch at the root: the creation (or removal) of a record itself.
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key, .. } => {
                    key.starts_with(GROUPS_KEY)
                }
                _ => false,
            },
        })
}

/// Prefix of the root keys holding this document's cross-application
/// invocations. Namespaced like [`GROUPS_KEY`] so it cannot collide with an
/// app's own schema.
pub const INVOCATIONS_KEY: &str = "tilo:invocation:";

/// True when every patch lands inside an invocation's metadata — the change did nothing
/// but record bookkeeping, already on screen as a band, so a "1 field" row
/// under it would be noise. An empty patch set is not a marker.
pub fn is_invocation_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(INVOCATIONS_KEY),
            Some(_) => false,
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(INVOCATIONS_KEY)
                }
                _ => false,
            },
        })
}

/// Namespaced beside the names of groups and invocations, apart from any
/// app's own schema.
pub const DESCRIPTIONS_KEY: &str = "tilo:describe:";

/// Whether a change only wrote descriptions.
pub fn is_description_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| match patch.path.first() {
            Some((_, Prop::Map(key))) => key.starts_with(DESCRIPTIONS_KEY),
            Some(_) => false,
            None => match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(DESCRIPTIONS_KEY)
                }
                _ => false,
            },
        })
}

/// Whether a change only wrote history's own metadata about other changes:
/// names of groups ([`GROUPS_KEY`]), cross-app invocations ([`INVOCATIONS_KEY`]) and
/// descriptions ([`DESCRIPTIONS_KEY`]). Such a change is neither a row nor an undo step.
pub fn is_history_metadata(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| {
            let one = std::slice::from_ref(patch);
            is_group_marker(one) || is_invocation_marker(one) || is_description_marker(one)
        })
}

/// Words for the tests here: an actor by the handle it declares, a step by
/// what its entry says it did.
#[cfg(test)]
pub(crate) struct TestWords;

#[cfg(test)]
impl authors::ResolveAuthor for TestWords {
    fn resolve(&self, actor_hex: &str) -> authors::HistoryAuthor {
        let name = authors::actor_handle(actor_hex)
            .unwrap_or_else(|| authors::UNKNOWN_AUTHOR_NAME.to_owned());
        authors::author_coloured(
            name,
            authors::short_id(actor_hex),
            crate::presence::user_color(actor_hex),
        )
    }
}

#[cfg(test)]
impl words::HistoryWords for TestWords {
    fn sentence(&self, entry: &tilo_actions::ActionContext) -> String {
        format!("{:?}", entry.what)
    }
}

use crate::artifact::ArtifactId;
use crate::op::SignedOp;

pub use tilo_actions::HistoryEntryId;

/// A delegated agent on a workspace's roster, as the session verified it: its
/// own device key, the person whose root signed its delegation for exactly
/// that device, and how it is shown. Only the session produces these, after
/// checking that delegation; history uses them to name who wrote and never
/// to decide what anyone may do.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RosterAgent {
    /// The agent's own device key: the actor its signed changes carry.
    pub device: [u8; 32],
    /// The root key of the person it acts for.
    pub person: [u8; 32],
    /// That person's name as the workspace's governance records it.
    pub person_name: String,
    /// The delegation's stable agent id.
    pub agent_id: String,
    /// The agent's display name.
    pub label: String,
    /// Its cursor colour, sRGB.
    pub color: [u8; 3],
}

/// What an application installed on this device is called, in each language
/// its package names it in: what a window reads a change of another app as
/// ("Heute"), never as the words its id makes. The session sends these with
/// the names of the workspace's people, and each window reads the name its
/// own language gives.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NamedApp {
    /// The app's package id (`so.tilo.lang.today`).
    pub id: String,
    /// The name its package gives itself.
    pub name: String,
    /// The name in other languages, by BCP 47 tag.
    pub names: std::collections::BTreeMap<String, String>,
}

impl NamedApp {
    /// What the app is called in `locale`: [`localized_name`], else the name its package gives
    /// itself.
    #[must_use]
    pub fn name_in(&self, locale: &str) -> &str {
        localized_name(&self.names, locale).unwrap_or(self.name.as_str())
    }
}

/// The name `names` (a package's `[package.names]`, by language tag) gives in `locale`: the
/// name of its exact tag, else of its language, else of another region of its language — the
/// one rule for which tag a reader gets ([`tilo_paths::matching`]), so a package named `de-DE`
/// reads as `Aufgaben` on a German machine whatever its locale is spelled like.
#[must_use]
pub fn localized_name<'a>(
    names: &'a std::collections::BTreeMap<String, String>,
    locale: &str,
) -> Option<&'a str> {
    let position = tilo_paths::matching(locale, names.keys().map(String::as_str))?;
    names.values().nth(position).map(String::as_str)
}

/// Collect up to `max` ops of `artifact` with `seq < before_seq`: the newest
/// such window, in ascending `seq` order, plus whether nothing older remains.
/// The page semantics every history host shares.
pub fn page_before<'a>(
    ops: impl IntoIterator<Item = &'a SignedOp>,
    artifact: ArtifactId,
    before_seq: u64,
    max: u16,
) -> (Vec<SignedOp>, bool) {
    let mut matching: Vec<&SignedOp> = ops
        .into_iter()
        .filter(|op| op.artifact == artifact && op.seq < before_seq)
        .collect();
    matching.sort_by_key(|op| op.seq);
    let total = matching.len();
    if max == 0 {
        return (Vec::new(), total == 0);
    }
    let start = total.saturating_sub(usize::from(max));
    let page: Vec<SignedOp> = matching
        .get(start..)
        .unwrap_or_default()
        .iter()
        .map(|op| (*op).clone())
        .collect();
    let exhausted = page.len() >= total;
    (page, exhausted)
}

/// One page request: ops strictly older than `before_seq`, at most `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    /// The artifact whose history is paged.
    pub artifact: ArtifactId,
    /// Exclusive upper bound on `seq`.
    pub before_seq: u64,
    /// Page size.
    pub max: u16,
}

/// Client-side paging state for one artifact's history.
#[derive(Clone, Debug)]
pub struct HistoryPager {
    artifact: ArtifactId,
    page_size: u16,
    prefetch_margin: usize,
    /// Everything with `seq >= oldest_loaded` is loaded; `None` before the
    /// first page or anchor.
    oldest_loaded: Option<u64>,
    /// An outstanding request's `before_seq`.
    in_flight: Option<u64>,
    /// A point the caller asked to reach (older version, search).
    target: Option<u64>,
    exhausted: bool,
}

impl HistoryPager {
    /// A pager fetching `page_size` ops per request and prefetching when a
    /// view is within `prefetch_margin` loaded ops of the oldest one.
    pub fn new(artifact: ArtifactId, page_size: u16, prefetch_margin: usize) -> Self {
        Self {
            artifact,
            page_size: page_size.max(1),
            prefetch_margin,
            oldest_loaded: None,
            in_flight: None,
            target: None,
            exhausted: false,
        }
    }

    /// The artifact this pager fetches.
    pub fn artifact(&self) -> ArtifactId {
        self.artifact
    }

    /// Anchor at the current state: everything from `seq` on arrived live
    /// or in the joining snapshot; history before it is fetched lazily.
    pub fn anchor(&mut self, seq: u64) {
        self.oldest_loaded = Some(self.oldest_loaded.map_or(seq, |oldest| oldest.min(seq)));
    }

    pub fn oldest_loaded(&self) -> Option<u64> {
        self.oldest_loaded
    }

    /// Whether the host reported that nothing older exists.
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Whether a request is outstanding.
    pub fn is_loading(&self) -> bool {
        self.in_flight.is_some()
    }

    fn next_request(&mut self) -> Option<PageRequest> {
        if self.exhausted || self.in_flight.is_some() {
            return None;
        }
        let before_seq = self.oldest_loaded.unwrap_or(u64::MAX);
        if before_seq == 0 {
            self.exhausted = true;
            return None;
        }
        self.in_flight = Some(before_seq);
        Some(PageRequest {
            artifact: self.artifact,
            before_seq,
            max: self.page_size,
        })
    }

    /// A history view reports how many loaded ops lie above (older than)
    /// its viewport. Returns a request when the view is close enough to
    /// the loaded edge to prefetch the next page.
    pub fn on_viewport(&mut self, loaded_ops_above_viewport: usize) -> Option<PageRequest> {
        if loaded_ops_above_viewport > self.prefetch_margin {
            return None;
        }
        self.next_request()
    }

    /// The caller needs history back to `seq` (opening an older version,
    /// searching backwards). Returns the next request, and keeps requesting
    /// after each page until `seq` is loaded or history is exhausted.
    pub fn ensure_loaded_back_to(&mut self, seq: u64) -> Option<PageRequest> {
        if self.oldest_loaded.is_some_and(|oldest| oldest <= seq) {
            return None;
        }
        self.target = Some(self.target.map_or(seq, |target| target.min(seq)));
        self.next_request()
    }

    /// Record a page of ops answering the outstanding request; see
    /// [`Self::on_page`].
    pub fn on_page_ops(&mut self, page: &[SignedOp], exhausted: bool) -> Option<PageRequest> {
        let oldest = page
            .iter()
            .filter(|op| op.artifact == self.artifact)
            .map(|op| op.seq)
            .min();
        self.on_page(oldest, exhausted)
    }

    /// Record the answer to the outstanding request: the oldest `seq` the
    /// host returned (before the caller dropped anything it could not use
    /// yet, such as an incomplete segment) and whether nothing older exists.
    /// Returns the next request when an [`Self::ensure_loaded_back_to`]
    /// target is still not reached. An answer to no outstanding request is
    /// ignored.
    pub fn on_page(&mut self, oldest_seq: Option<u64>, exhausted: bool) -> Option<PageRequest> {
        let before_seq = self.in_flight.take()?;
        let oldest_in_page = oldest_seq.filter(|seq| *seq < before_seq);
        if let Some(seq) = oldest_in_page {
            self.oldest_loaded = Some(seq);
        }
        if exhausted {
            if oldest_in_page.is_none() && before_seq != u64::MAX {
                self.oldest_loaded = Some(before_seq);
            }
            self.exhausted = true;
            self.target = None;
            return None;
        }
        // Nothing yet, but more exists: the host is fetching it from its
        // peers. Keep the position; the next trigger asks again.
        oldest_in_page?;
        match self.target {
            Some(target) if self.oldest_loaded.is_some_and(|oldest| oldest > target) => {
                self.next_request()
            }
            _ => {
                self.target = None;
                None
            }
        }
    }

    /// Forget an outstanding request that will never be answered (transport
    /// reset), so the next trigger re-issues it.
    pub fn reset_in_flight(&mut self) {
        self.in_flight = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Ed25519PublicKey;
    use crate::identity::AttestationId;
    use crate::role::OpKind;

    fn op(artifact: ArtifactId, seq: u64) -> SignedOp {
        SignedOp::new_unsigned(
            seq.to_le_bytes().to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes([1; 32]),
            AttestationId::new(),
            OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    tilo_test_env::scenario! {
        /// A history id reads as the lower-case hex a document key and the wire spell it, and
        /// only exactly that many hex digits, in either case, read back as an id.
        fn a_history_id_round_trips_through_its_hex;

        phase it_spells_lower_case_and_reads_either_case;
        {
            let mut bytes = [0u8; 16];
            bytes[0] = 0xab;
            bytes[15] = 0xcd;
            let id = HistoryEntryId(bytes);
            assert_eq!(id.hex(), format!("ab{}cd", "0".repeat(28)));
            assert_eq!(HistoryEntryId::from_hex(&id.hex()), Some(id));
            assert_eq!(HistoryEntryId::from_hex(&id.hex().to_ascii_uppercase()), Some(id));
        }

        phase junk_reads_as_no_id;
        {
            assert_eq!(HistoryEntryId::from_hex("nope"), None);
            assert_eq!(HistoryEntryId::from_hex(&"z".repeat(32)), None);
            assert_eq!(HistoryEntryId::from_hex(&"a".repeat(31)), None);
            assert_eq!(HistoryEntryId::from_hex(&"a".repeat(34)), None);
        }
    }

    tilo_test_env::scenario! {
        /// Scrolling back prefetches near the edge and stops when exhausted; opening an old version pages
        /// until it is loaded; unsolicited or foreign pages change nothing; an empty non-final page is
        /// asked again.
        fn history_paging_fetches_only_what_the_view_needs;

        phase scrolling_back_prefetches_near_the_edge_and_stops_when_exhausted;
        {
            let artifact = ArtifactId::new();
            let log: Vec<SignedOp> = (1..=250).map(|seq| op(artifact, seq)).collect();
            let mut pager = HistoryPager::new(artifact, 100, 20);
            pager.anchor(240);
            assert_eq!(pager.on_viewport(50), None, "far from the edge");
            let request = pager.on_viewport(10).unwrap();
            assert_eq!(request.before_seq, 240);
            assert_eq!(pager.on_viewport(0), None, "one request at a time");
            let (page, exhausted) = page_before(&log, artifact, request.before_seq, request.max);
            assert_eq!(page.len(), 100);
            assert_eq!(page.first().unwrap().seq, 140);
            assert_eq!(pager.on_page_ops(&page, exhausted), None);
            assert_eq!(pager.oldest_loaded(), Some(140));

            let request = pager.on_viewport(5).unwrap();
            let (page, exhausted) = page_before(&log, artifact, request.before_seq, request.max);
            pager.on_page_ops(&page, exhausted);
            let request = pager.on_viewport(5).unwrap();
            let (page, exhausted) = page_before(&log, artifact, request.before_seq, request.max);
            assert!(exhausted);
            pager.on_page_ops(&page, exhausted);
            assert_eq!(pager.oldest_loaded(), Some(1));
            assert!(pager.is_exhausted());
            assert_eq!(pager.on_viewport(0), None);
        }

        phase opening_an_old_version_pages_until_it_is_loaded;
        {
            let artifact = ArtifactId::new();
            let log: Vec<SignedOp> = (1..=1000).map(|seq| op(artifact, seq)).collect();
            let mut pager = HistoryPager::new(artifact, 64, 8);
            pager.anchor(1001);
            let mut request = pager.ensure_loaded_back_to(700);
            let mut fetched = 0;
            while let Some(next) = request {
                fetched += 1;
                let (page, exhausted) = page_before(&log, artifact, next.before_seq, next.max);
                request = pager.on_page_ops(&page, exhausted);
            }
            assert_eq!(fetched, 5, "only the pages covering the requested range");
            assert!(pager.oldest_loaded().unwrap() <= 700);
            assert!(!pager.is_exhausted());
            assert_eq!(pager.ensure_loaded_back_to(800), None, "already loaded");
        }

        phase unsolicited_or_foreign_pages_change_nothing;
        {
            let artifact = ArtifactId::new();
            let mut pager = HistoryPager::new(artifact, 10, 0);
            pager.anchor(50);
            assert_eq!(pager.on_page_ops(&[op(artifact, 3)], false), None);
            assert_eq!(pager.oldest_loaded(), Some(50));
            let request = pager.on_viewport(0).unwrap();
            assert_eq!(request.before_seq, 50);
            assert_eq!(pager.on_page_ops(&[op(ArtifactId::new(), 3)], false), None);
            assert!(!pager.is_exhausted());
            assert_eq!(pager.oldest_loaded(), Some(50));
        }

        phase an_empty_page_that_is_not_final_is_asked_again;
        {
            let artifact = ArtifactId::new();
            let mut pager = HistoryPager::new(artifact, 10, 0);
            pager.anchor(50);
            let first = pager.ensure_loaded_back_to(10).unwrap();
            assert_eq!(pager.on_page(None, false), None);
            assert!(!pager.is_exhausted());
            let again = pager.ensure_loaded_back_to(10).unwrap();
            assert_eq!(again.before_seq, first.before_seq);
            assert_eq!(pager.on_page(Some(40), false).unwrap().before_seq, 40);
            assert_eq!(pager.on_page(None, true), None);
            assert!(pager.is_exhausted());
            assert_eq!(pager.oldest_loaded(), Some(40));
        }
    }
}
