//! Records provider — the gesture that brings your own things into a room.
//! A room starts empty; moving a record in costs one step: type its
//! name, the row says which room it lives in, Enter **brings it here**.
//!
//! Bringing is referencing, not copying: activating a row adds this
//! room's reference to the record
//! ([`crate::shell::room_records::RoomRecords::reference`]) — no bytes
//! move, no id is minted, and hydration remains in this room's document
//! scope. Copying is a different, deliberate verb
//! ([`ui::record::room::duplicate`]). A
//! record whose identity is a path is also opened, since bringing and
//! opening are meant to be the same gesture.
//!
//! [`RoomRecords`] answers both halves — which records exist, and which
//! rooms hold which — already in memory. The provider indexes the known
//! records once per corpus revision, so a query only searches.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_search::{Document, FieldWeight, Matching, Page, SearchIndex};

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::{t, t_args};
use crate::shell::room_records::{KnownRecord, RoomRecords};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use crate::shell::state::{RoomId, WorkspaceTree};

/// Top of this provider's scale: the record is named exactly. Declared
/// honestly because [`crate::shell::search::SearchRegistry::query`]
/// divides by it to compare providers — scoring above the ceiling claims
/// a better-than-perfect match and takes over the palette.
pub const MAX_SCORE: i32 = 100;

/// Rows one query contributes; a long tail here only costs memory, not
/// the palette's shared row budget.
const MAX_RESULTS: usize = 20;

pub struct RecordsProvider {
    index: Rc<RefCell<RoomRecords>>,
    /// The known records at one corpus revision, indexed by position.
    snapshot: RefCell<Option<RecordsSnapshot>>,
}

struct RecordsSnapshot {
    revision: String,
    records: Rc<Vec<KnownRecord>>,
    index: SearchIndex,
}

impl RecordsSnapshot {
    /// Title first; the identity is matched lower — a file record's id
    /// contains its whole path, so a directory-name match is a weaker
    /// signal than the title.
    fn new(revision: String, records: Rc<Vec<KnownRecord>>) -> Self {
        let mut index = SearchIndex::new(Matching::LOOSE);
        index.replace_all(records.iter().enumerate().map(|(at, record)| {
            Document::new(at as u64)
                .field(record.title(), FieldWeight::TITLE)
                .field(record.id.as_str(), FieldWeight::IDENTITY)
        }));
        if let Err(error) = index.commit() {
            tracing::error!(%error, "could not index records");
        }
        Self {
            revision,
            records,
            index,
        }
    }
}

impl RecordsProvider {
    /// A provider over a fresh index; a test or screenshot run building
    /// a palette here reads nobody's state.
    pub fn new() -> Self {
        Self::with_index(Rc::new(RefCell::new(RoomRecords::in_memory())))
    }

    /// A provider over a caller-supplied index — the shell shares one
    /// across every surface that references records.
    pub fn with_index(index: Rc<RefCell<RoomRecords>>) -> Self {
        Self {
            index,
            snapshot: RefCell::new(None),
        }
    }

    pub fn index(&self) -> Rc<RefCell<RoomRecords>> {
        Rc::clone(&self.index)
    }
}

impl Default for RecordsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for RecordsProvider {
    fn id(&self) -> &'static str {
        "records"
    }
    fn name(&self) -> &'static str {
        "Records"
    }

    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-records")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Record]
    }
    fn max_score(&self) -> i32 {
        MAX_SCORE
    }

    fn proximity(
        &self,
        result: &SearchResult,
        chrome: &ChromeState,
    ) -> crate::shell::search::SearchProximity {
        let Some(id) = ui::record::RecordId::parse(&result.id) else {
            return crate::shell::search::SearchProximity::Global;
        };
        let here = chrome
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room);
        let is_here = here.is_some_and(|room| {
            self.index
                .borrow()
                .known()
                .iter()
                .find(|record| record.id == id)
                .is_some_and(|record| record.is_in(room))
        });
        if is_here {
            crate::shell::search::SearchProximity::CurrentWorkspace
        } else {
            crate::shell::search::SearchProximity::Global
        }
    }

    /// A record's address is its id: it resolves from the retained record
    /// list directly, with no search beforehand.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Record) {
            return None;
        }
        let wanted = ui::record::RecordId::parse(id)?;
        let (here, rooms) = {
            let tree = ctx.chrome.tree();
            (
                tree.desktop.active_workspace().map(|w| w.room),
                room_names(&tree),
            )
        };
        let records = self.index.borrow().known();
        let record = records.iter().find(|record| record.id == wanted)?;
        let subtitle = provenance(record, here, &rooms);
        Some(self.row(record, record.title(), subtitle, MAX_SCORE))
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        // Legacy direct callers get one retained, bounded page too. They no
        // longer accidentally turn an integration test or an old surface into
        // an unbounded corpus traversal.
        let mut results = self
            .query_page(
                ctx,
                None,
                SearchBudget {
                    max_results: MAX_RESULTS,
                    max_bytes: SearchBudget::PALETTE.max_bytes,
                    max_work: SearchBudget::PALETTE.max_work,
                },
            )
            .results;
        results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
        results
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        // `known()` is a retained, generation-keyed snapshot.  Its Rc is
        // replaced exactly when room references or the background record
        // scan changes, which makes it a cheap opaque cursor revision.
        self.index.borrow().search_generation()
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if !ctx.wanted.contains(&ResourceKind::Record)
            || ctx.needle.trim().is_empty()
            || budget.max_results == 0
            || budget.max_work == 0
        {
            return SearchPage::exhausted("empty", Vec::new());
        }
        let (here, rooms) = {
            let tree = ctx.chrome.tree();
            (
                tree.desktop.active_workspace().map(|w| w.room),
                room_names(&tree),
            )
        };
        let needle = ctx.needle.trim().to_lowercase();
        let revision = self.index.borrow().search_generation();
        let offset = match cursor {
            Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
            Some(cursor) => match cursor
                .token
                .split_once('\u{1f}')
                .filter(|(cursor_needle, _)| *cursor_needle == needle)
                .and_then(|(_, offset)| offset.parse::<usize>().ok())
            {
                Some(offset) => offset,
                None => return SearchPage::stale(revision),
            },
            None => 0,
        };
        let mut slot = self.snapshot.borrow_mut();
        if slot
            .as_ref()
            .is_none_or(|snapshot| snapshot.revision != revision)
        {
            let records = self.index.borrow().known();
            *slot = Some(RecordsSnapshot::new(revision.clone(), records));
        }
        let Some(snapshot) = slot.as_ref() else {
            return SearchPage::exhausted(revision, Vec::new());
        };
        let found = snapshot.index.search(
            &needle,
            Page {
                offset,
                limit: budget.max_work.min(budget.max_results),
            },
        );
        let mut used = 0usize;
        let mut results = Vec::new();
        let mut shown = 0usize;
        for hit in &found.hits {
            let Some(record) = usize::try_from(hit.key)
                .ok()
                .and_then(|at| snapshot.records.get(at))
            else {
                shown = shown.saturating_add(1);
                continue;
            };
            let title = record.title();
            let subtitle = provenance(record, here, &rooms);
            let bytes = record
                .id
                .as_str()
                .len()
                .saturating_add(title.len())
                .saturating_add(subtitle.len())
                .saturating_add(96);
            if used.saturating_add(bytes) > budget.max_bytes {
                break;
            }
            used = used.saturating_add(bytes);
            shown = shown.saturating_add(1);
            results.push(self.row(
                record,
                title,
                subtitle,
                crate::shell::search::provider_score(hit.rank.unwrap_or(0)),
            ));
        }
        let exhausted = !found.more && shown == found.hits.len();
        let next = offset.saturating_add(shown);
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision,
                token: format!("{needle}\u{1f}{next}"),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: !exhausted,
        }
    }
}

impl RecordsProvider {
    fn row(
        &self,
        record: &KnownRecord,
        title: String,
        subtitle: String,
        score: i32,
    ) -> SearchResult {
        let index = Rc::clone(&self.index);
        let id = record.id.clone();
        let kind = record.kind.clone();
        SearchResult::view_with_output(
            ResourceKind::Record,
            "records",
            record.id.as_str().to_string(),
            title,
            Some(subtitle),
            score,
            move |chrome: &Chrome| bring_into_this_room(&index, &id, &kind, chrome),
        )
    }
}

/// Enter on a record row. Two effects, in this order: the reference is
/// recorded first (it's what "bring it here" *means*, whether or not
/// there's anything to open), so a launch that fails still leaves the
/// record in this room.
fn bring_into_this_room(
    index: &Rc<RefCell<RoomRecords>>,
    id: &ui::record::RecordId,
    kind: &ui::record::RecordKind,
    chrome: &Chrome,
) -> Result<serde_json::Value, crate::shell::search::action_api::ActionError> {
    use crate::shell::search::action_api::ActionError;
    let here = {
        let chrome_state = chrome.borrow();
        let tree = chrome_state.tree();
        tree.desktop.active_workspace().map(|w| w.room)
    };
    if let Some(here) = here {
        index.borrow_mut().reference(here, id, kind);
    }

    // A path-identified record is also opened; anything else is brought
    // here and waits for its app.
    let Some(path) = id.file_path() else {
        return Ok(serde_json::json!({ "brought": id.as_str() }));
    };
    if !path.exists() {
        let message = t("search-record-file-missing");
        crate::shell::doc_open::report_open_failure(chrome, &path, &message, Some("Records"));
        return Err(ActionError::Refused(message));
    }
    crate::shell::doc_open::open_in_workspace(&path, chrome)
        .map(|opened| opened.to_value(&path))
        .map_err(|e| {
            let message = e.user_message();
            crate::shell::doc_open::report_open_failure(chrome, &path, &message, Some("Records"));
            ActionError::Refused(message)
        })
}

/// Room uuid → the name the user gave that workspace.
fn room_names(tree: &WorkspaceTree) -> Vec<(RoomId, String)> {
    tree.desktop
        .workspaces
        .iter()
        .map(|w| (w.room, w.name.clone()))
        .collect()
}

/// The line under the title: where this record lives. Not decoration —
/// without it, two rooms' `notes.md` are identical rows and activating
/// one is a guess.
fn provenance(record: &KnownRecord, here: Option<RoomId>, rooms: &[(RoomId, String)]) -> String {
    let where_it_lives = if here.is_some_and(|h| record.is_in(h)) {
        t("search-record-in-this-workspace")
    } else {
        let named = record
            .rooms()
            .into_iter()
            .filter(|r| Some(*r) != here)
            .find_map(|r| {
                rooms
                    .iter()
                    .find(|(id, _)| *id == r)
                    .map(|(_, name)| name.clone())
            });
        match named {
            Some(name) => t_args("search-record-in-workspace", &[("workspace", name.into())]),
            // No workspace for this room (deleted, or minted on a peer):
            // say so rather than print a uuid.
            None => t("search-record-in-another-workspace"),
        }
    };
    match record.id.file_path() {
        Some(path) => format!("{where_it_lives} · {}", display_path(&path)),
        None if !record.kind.as_str().is_empty() => {
            format!("{where_it_lives} · {}", record.kind)
        }
        None => where_it_lives,
    }
}

/// `~`-shortened path for the subtitle.
fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = tilo_paths::home()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Prefix the provider-owned source continuation with an exact byte length of
/// the normalized query. A delimiter alone is ambiguous because a record
/// search string is ordinary user text; malformed continuations are rejected
/// as stale instead of seeking an unintended part of the corpus.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::room_records::ObservedRecord;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use ui::record::{RecordCheckout, RecordId, RecordKind, RecordSet};

    fn chrome_with_rooms(names: &[&str]) -> (Chrome, Vec<RoomId>) {
        let mut tree = WorkspaceTree::default();
        let mut rooms = Vec::new();
        for name in names {
            let mut ws = Workspace::new(WorkspaceId::new(), *name);
            ws.room = RoomId::new();
            rooms.push(ws.room);
            tree.desktop.workspaces.push(ws);
        }
        tree.desktop.active = tree.desktop.workspaces[0].id;
        (ChromeState::new(tree), rooms)
    }

    fn activate(chrome: &Chrome, id: WorkspaceId) {
        chrome.borrow().tree_mut().desktop.active = id;
    }

    fn query(provider: &RecordsProvider, needle: &str, chrome: &Chrome) -> Vec<SearchResult> {
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Record],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    fn page(
        provider: &RecordsProvider,
        needle: &str,
        chrome: &Chrome,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        provider.query_page(
            SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Record],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            },
            cursor,
            budget,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn record_pages_seek_without_duplicates_and_stale_on_mutation() {
        // Phase `a_large_corpus_is_capped_on_the_first_page_without_a_title_rebuild`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            for number in 0..128 {
                index.borrow_mut().reference(
                    rooms[0],
                    &RecordId::for_file(format!("/work/entry-{number:03}.note")),
                    &RecordKind::new("note"),
                );
            }
            let provider = RecordsProvider::with_index(index);
            let first = page(
                &provider,
                "entry",
                &chrome,
                None,
                SearchBudget {
                    max_results: 16,
                    max_bytes: usize::MAX,
                    max_work: 3,
                },
            );
            assert_eq!(first.results.len(), 3, "work is a hard source cap");
            assert!(first.budget_exhausted);
            assert!(!first.exhausted);
            assert!(first.next_cursor.is_some());
        }

        // Phase `record_pages_seek_from_the_last_key_without_duplicates`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            for number in 0..5 {
                index.borrow_mut().reference(
                    rooms[0],
                    &RecordId::for_file(format!("/work/page-{number}.note")),
                    &RecordKind::new("note"),
                );
            }
            let provider = RecordsProvider::with_index(index);
            let budget = SearchBudget {
                max_results: 2,
                max_bytes: usize::MAX,
                max_work: 2,
            };
            let first = page(&provider, "page", &chrome, None, budget);
            let cursor = first.next_cursor.clone().expect("more source rows");
            let second = page(&provider, "page", &chrome, Some(&cursor), budget);
            let cursor = second.next_cursor.clone().expect("one final source row");
            let third = page(&provider, "page", &chrome, Some(&cursor), budget);
            let ids: Vec<_> = first
                .results
                .iter()
                .chain(&second.results)
                .chain(&third.results)
                .map(|row| row.id.clone())
                .collect();
            assert_eq!(ids.len(), 5);
            let unique: std::collections::BTreeSet<_> = ids.iter().collect();
            assert_eq!(unique.len(), ids.len(), "a key cursor must not replay rows");
            assert!(third.exhausted);
        }

        // Phase `resource_limits_do_not_consume_an_unemitted_record`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            index.borrow_mut().reference(
                rooms[0],
                &RecordId::for_file("/work/too-large-to-fit.note"),
                &RecordKind::new("note"),
            );
            let provider = RecordsProvider::with_index(index);
            let limited = page(
                &provider,
                "large",
                &chrome,
                None,
                SearchBudget {
                    max_results: 1,
                    max_bytes: 1,
                    max_work: 1,
                },
            );
            assert!(limited.results.is_empty());
            assert!(limited.budget_exhausted);
            let cursor = limited.next_cursor.expect("the unreturned row is retained");
            let retried = page(
                &provider,
                "large",
                &chrome,
                Some(&cursor),
                SearchBudget {
                    max_results: 1,
                    max_bytes: usize::MAX,
                    max_work: 1,
                },
            );
            assert_eq!(retried.results.len(), 1, "retry resumes at the row");
        }

        // Phase `a_room_reference_mutation_stales_a_retained_cursor`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            for name in ["first", "second"] {
                index.borrow_mut().reference(
                    rooms[0],
                    &RecordId::for_file(format!("/work/{name}.note")),
                    &RecordKind::new("note"),
                );
            }
            let provider = RecordsProvider::with_index(Rc::clone(&index));
            let first = page(
                &provider,
                "note",
                &chrome,
                None,
                SearchBudget {
                    max_results: 1,
                    max_bytes: usize::MAX,
                    max_work: 1,
                },
            );
            let cursor = first.next_cursor.expect("one source row remains");
            index.borrow_mut().reference(
                rooms[0],
                &RecordId::for_file("/work/third.note"),
                &RecordKind::new("note"),
            );
            assert!(
                page(
                    &provider,
                    "note",
                    &chrome,
                    Some(&cursor),
                    SearchBudget::PALETTE,
                )
                .stale_cursor,
                "local mutations must never advance an old continuation"
            );
        }

        // Phase `same_count_scan_replacement_stales_a_record_cursor`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            let observed = |name: &str| ObservedRecord {
                id: RecordId::for_file(format!("/scan/{name}.note")),
                kind: RecordKind::new("note"),
                home: Some(rooms[0]),
                package_id: "so.tilo.notes".into(),
            };
            index
                .borrow_mut()
                .replace_scanned_records_for_test(vec![observed("a"), observed("b")]);
            let provider = RecordsProvider::with_index(Rc::clone(&index));
            let chrome_state = chrome.borrow();
            let doc = automerge::AutoCommit::new();
            let context = || SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "note",
                wanted: &[ResourceKind::Record],
                chrome: &chrome_state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            let first = provider.query_page(
                context(),
                None,
                SearchBudget {
                    max_results: 1,
                    max_bytes: usize::MAX,
                    max_work: 16,
                },
            );
            let cursor = first
                .next_cursor
                .expect("a bounded first page has a cursor");

            index
                .borrow_mut()
                .replace_scanned_records_for_test(vec![observed("c"), observed("d")]);
            let replacement_page =
                provider.query_page(context(), Some(&cursor), SearchBudget::PALETTE);
            assert!(
                replacement_page.stale_cursor,
                "same-count replacement must invalidate the cursor"
            );
        }
    }

    #[test]
    #[allow(
        clippy::similar_names,
        reason = "here/there are the domain vocabulary for the two rooms under test, not a naming accident"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn records_from_other_rooms_are_findable_referenced_and_isolated() {
        // Phase `a_record_from_another_room_is_findable_and_says_where_it_lives`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            let book = RecordId::minted();
            index
                .borrow_mut()
                .reference(rooms[1], &book, &RecordKind::new("book"));
            index.borrow_mut().seed_observed([ObservedRecord {
                id: book,
                kind: RecordKind::new("book"),
                home: Some(rooms[1]),
                package_id: "so.tilo.ebook".into(),
            }]);
            let provider = RecordsProvider::with_index(index);

            let out = query(&provider, "book", &chrome);
            assert_eq!(
                out.len(),
                1,
                "the record in the other room must be findable: {out:?}"
            );
            assert_eq!(out[0].kind, ResourceKind::Record);
            assert_eq!(
                out[0].subtitle.as_deref(),
                Some("In Reading · book"),
                "the row must say which room the record lives in"
            );
        }

        // Phase `activating_a_row_references_the_same_record_into_this_room`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
            let (here, there) = (rooms[0], rooms[1]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            let book = RecordId::minted();
            let kind = RecordKind::new("book");
            index.borrow_mut().reference(there, &book, &kind);
            let provider = RecordsProvider::with_index(Rc::clone(&index));

            assert!(
                !index.borrow().holds(here, &book),
                "the active room starts without it — a room starts empty"
            );

            let out = query(&provider, "book", &chrome);
            let row = out.first().expect("the record is findable");
            let mut doc = automerge::AutoCommit::new();
            (row.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &crate::shell::search::action_api::ActionArgs::none(),
            )
            .expect("bringing a record here takes no arguments");

            let index = index.borrow();
            assert!(index.holds(here, &book), "activating brings it here");
            assert!(
                index.holds(there, &book),
                "and leaves it in the room it came from — this is a reference, \
                 not a move"
            );
            let ids_here: Vec<RecordId> =
                index.records_in(here).into_iter().map(|c| c.id).collect();
            let ids_there: Vec<RecordId> =
                index.records_in(there).into_iter().map(|c| c.id).collect();
            assert_eq!(
                ids_here, ids_there,
                "both rooms name the same record id — a copy would have minted \
                 a new one"
            );
        }

        // Phase `an_edit_through_one_rooms_reference_does_not_leak_through_the_other`.
        {
            use automerge::{ROOT, ReadDoc, transaction::Transactable};

            let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
            let (here, there) = (rooms[0], rooms[1]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            let book = RecordId::minted();
            let kind = RecordKind::new("book");
            index.borrow_mut().reference(there, &book, &kind);
            let provider = RecordsProvider::with_index(Rc::clone(&index));

            let out = query(&provider, "book", &chrome);
            let mut doc = automerge::AutoCommit::new();
            (out[0].on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT,
                &chrome,
                &crate::shell::search::action_api::ActionArgs::none(),
            )
            .expect("bringing a record here takes no arguments");

            let mut there_set = RecordSet::in_memory().with_home(there.to_string());
            let mut there_state = ui::State::new();
            let from_there: RecordCheckout = index
                .borrow()
                .records_in(there)
                .into_iter()
                .find(|c| c.id == book)
                .expect("the room it came from still holds it");
            there_set.checkout(&mut there_state, None, &from_there);
            let _ = there_state.doc.put(&ROOT, "title", "Chapter 3");

            let from_here: RecordCheckout = index
                .borrow()
                .records_in(here)
                .into_iter()
                .find(|c| c.id == book)
                .expect("this room holds it now");
            let mut here_set = RecordSet::in_memory().with_home(here.to_string());
            let mut here_state = ui::State::new();
            here_set.checkout(&mut here_state, None, &from_here);
            assert!(
                matches!(here_state.doc.get(&ROOT, "title"), Ok(None)),
                "the same id in another room must hydrate a separate document"
            );
        }
    }

    #[test]
    fn record_provenance_labels_follow_the_active_room() {
        // Phase `a_record_in_this_room_is_labelled_as_being_here`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            let id = RecordId::for_file("/work/report.odt");
            index
                .borrow_mut()
                .reference(rooms[0], &id, &RecordKind::new("document"));
            let provider = RecordsProvider::with_index(index);

            let out = query(&provider, "report", &chrome);
            assert_eq!(out.len(), 1);
            assert!(
                out[0]
                    .subtitle
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with("In this workspace"),
                "got {:?}",
                out[0].subtitle
            );
        }

        // Phase `a_record_from_an_unknown_room_says_so`.
        {
            let (chrome, _rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            index.borrow_mut().seed_observed([ObservedRecord {
                id: RecordId::for_file("/shared/minutes.odt"),
                kind: RecordKind::new("document"),
                home: Some(RoomId::new()),
                package_id: "so.tilo.word-processor".into(),
            }]);
            let provider = RecordsProvider::with_index(index);

            let out = query(&provider, "minutes", &chrome);
            assert_eq!(out.len(), 1);
            assert_eq!(
                out[0].subtitle.as_deref(),
                Some("In another workspace · /shared/minutes.odt")
            );
        }

        // Phase `provenance_follows_the_active_room`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
            let reading_ws = chrome.borrow().tree().desktop.workspaces[1].id;
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            let id = RecordId::minted();
            index
                .borrow_mut()
                .reference(rooms[1], &id, &RecordKind::new("book"));
            let provider = RecordsProvider::with_index(index);

            assert_eq!(
                query(&provider, "book", &chrome)[0].subtitle.as_deref(),
                Some("In Reading · book")
            );
            activate(&chrome, reading_ws);
            assert_eq!(
                query(&provider, "book", &chrome)[0].subtitle.as_deref(),
                Some("In this workspace · book")
            );
        }
    }

    #[test]
    fn record_queries_stay_in_their_ceiling_and_answer_only_when_wanted() {
        // Phase `scores_stay_inside_the_declared_ceiling`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            for path in [
                "/work/dune.epub",
                "/work/notes-on-dune.md",
                "/work/redundant.txt",
                "/dune/other.txt",
            ] {
                index.borrow_mut().reference(
                    rooms[0],
                    &RecordId::for_file(path),
                    &RecordKind::new("document"),
                );
            }
            let provider = RecordsProvider::with_index(index);
            let out = query(&provider, "dune", &chrome);
            assert!(!out.is_empty());
            for r in &out {
                assert!(
                    r.score <= provider.max_score(),
                    "{} scored {} over the declared ceiling {}",
                    r.title,
                    r.score,
                    provider.max_score()
                );
            }
            // Ranked, not merely returned: the exact name leads, and a
            // directory-only hit is last.
            assert_eq!(out[0].title, "dune.epub");
            assert_eq!(out.last().unwrap().title, "other.txt");
            assert!(
                !out.iter().any(|r| r.title == "redundant.txt"),
                "`dune` must not match inside another word: {out:?}"
            );
        }

        // Phase `an_empty_query_returns_nothing`.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            index.borrow_mut().reference(
                rooms[0],
                &RecordId::for_file("/work/a.odt"),
                &RecordKind::new("document"),
            );
            let provider = RecordsProvider::with_index(index);
            assert!(query(&provider, "   ", &chrome).is_empty());
        }

        // Phase `a_query_that_does_not_want_records_gets_none`.
        // The kind filter is honoured: with Records unticked the provider
        // contributes nothing, rather than the registry filtering afterwards.
        {
            let (chrome, rooms) = chrome_with_rooms(&["Work"]);
            let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
            index.borrow_mut().reference(
                rooms[0],
                &RecordId::for_file("/work/a.odt"),
                &RecordKind::new("document"),
            );
            let provider = RecordsProvider::with_index(index);
            let state = chrome.borrow();
            let doc = automerge::AutoCommit::new();
            let out = provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "a.odt",
                wanted: &[ResourceKind::File],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert!(out.is_empty());
        }
    }
}
