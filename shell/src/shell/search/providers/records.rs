//! Records provider — the one gesture that brings your own things into a
//! room.
//!
//! A room starts empty (`plan/rooms.md`), and that only holds up if moving
//! a record into it costs one step. This is that step: type the record's
//! name, the row says which room it lives in, and Enter **brings it here**.
//!
//! # Bringing is referencing, not copying
//!
//! Activating a row adds this room's reference to the record
//! ([`crate::shell::room_records::RoomRecords::reference`]). It moves no
//! bytes and mints no id: the record is one document, and an edit made
//! through either room is an edit to that document. That is the property
//! the whole model rests on — two *copies* of a book diverge and no merge
//! makes them one book again — and it is why copying is a different verb
//! with a different name ([`ui::record::room::duplicate`]) rather than
//! something this row does quietly.
//!
//! A record whose identity is a path is also opened, because the gesture
//! for *opening* something and the gesture for *bringing* it are meant to
//! be the same gesture. A record with no file to open is still brought
//! here; the reference is the part that always happens.
//!
//! # Where the corpus comes from
//!
//! [`RoomRecords`] answers both halves — which records exist (from the
//! per-record documents apps write) and which rooms hold which (from the
//! rooms' own indexes). Both are in memory by the time a key is pressed:
//! this `query` reads maps and matches strings, and never reaches the
//! filesystem or loads a CRDT.

use std::cell::RefCell;
use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::room_records::{KnownRecord, RoomRecords};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::state::{RoomId, WorkspaceTree};

/// Top of this provider's scale: the record is named exactly.
///
/// Declared honestly because [`crate::shell::search::SearchRegistry::query`]
/// divides by it to compare providers — a provider that scores above its
/// own ceiling claims a better-than-perfect match and takes over the
/// palette.
pub const MAX_SCORE: i32 = 100;

/// Rows one query contributes. The palette shares a small row budget
/// between kinds, so a long tail here only costs memory; the cap is
/// against a pathological corpus, not against the budget.
const MAX_RESULTS: usize = 20;

pub struct RecordsProvider {
    index: Rc<RefCell<RoomRecords>>,
}

impl RecordsProvider {
    /// A provider over a fresh index. The compositor makes that index
    /// durable — and starts the scan for records apps wrote — by calling
    /// [`RoomRecords::attach_store`] once at session start, so building a
    /// palette in a test or a screenshot run reads nobody's state.
    pub fn new() -> Self {
        Self::with_index(Rc::new(RefCell::new(RoomRecords::in_memory())))
    }

    /// A provider over a caller-supplied index — the shell shares one with
    /// every other surface that references records, and tests supply an
    /// in-memory one.
    pub fn with_index(index: Rc<RefCell<RoomRecords>>) -> Self {
        Self { index }
    }

    /// The index this provider reads and writes.
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
    fn id(&self) -> &str {
        "records"
    }
    fn name(&self) -> &str {
        "Records"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Record]
    }
    fn max_score(&self) -> i32 {
        MAX_SCORE
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Record) {
            return Vec::new();
        }
        let needle = ctx.needle.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        // One read of the tree, for the active room and for the room
        // names. Dropped before any result is built: a row's callback
        // borrows the chrome mutably.
        let (here, rooms) = {
            let tree = ctx.chrome.tree();
            (
                tree.active_workspace().map(|w| w.room),
                room_names(&tree),
            )
        };

        let index = self.index.borrow();
        let mut ranked: Vec<(i32, KnownRecord, String)> = Vec::new();
        for record in index.known().iter() {
            let title = record.title();
            let Some(score) = match_score(&needle, &title, record.id.as_str()) else {
                continue;
            };
            // Cloned only for the handful that matched — the corpus itself
            // is shared, not copied, on every keystroke.
            ranked.push((score, record.clone(), title));
        }
        drop(index);
        // Best first; ties broken by title so the order is the same on
        // every keystroke rather than following map iteration.
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
        ranked.truncate(MAX_RESULTS);

        ranked
            .into_iter()
            .map(|(score, record, title)| {
                let subtitle = provenance(&record, here, &rooms);
                let index = Rc::clone(&self.index);
                let id = record.id.clone();
                let kind = record.kind.clone();
                SearchResult::nullary(
                    ResourceKind::Record,
                    "records",
                    record.id.as_str().to_string(),
                    title,
                    Some(subtitle),
                    score,
                    move |_doc, _root, chrome: &Chrome| {
                        bring_into_this_room(&index, &id, &kind, chrome);
                    },
                )
            })
            .collect()
    }
}

/// Enter on a record row.
///
/// Two effects, in this order and for a reason: the reference is what
/// "bring it here" *means*, so it is recorded whether or not there is
/// anything to open, and it is recorded before the open so a launch that
/// fails still leaves the record in this room.
fn bring_into_this_room(
    index: &Rc<RefCell<RoomRecords>>,
    id: &ui::record::RecordId,
    kind: &ui::record::RecordKind,
    chrome: &Chrome,
) {
    let here = {
        let state = chrome.borrow();
        let tree = state.tree();
        tree.active_workspace().map(|w| w.room)
    };
    if let Some(here) = here {
        index.borrow_mut().reference(here, id, kind);
    }

    // A record whose identity is a path can also be opened, which is the
    // point of the gesture being one gesture. Anything else is brought
    // here and waits for its app.
    let Some(path) = id.file_path() else {
        return;
    };
    if !path.exists() {
        crate::shell::doc_open::report_open_failure(
            chrome,
            &path,
            "the file this record names is no longer there",
            Some("Records"),
        );
        return;
    }
    if let Err(e) = crate::shell::doc_open::open_in_workspace(&path, chrome) {
        crate::shell::doc_open::report_open_failure(
            chrome,
            &path,
            &e.user_message(),
            Some("Records"),
        );
    }
}

/// Room uuid → the name the user gave that workspace.
fn room_names(tree: &WorkspaceTree) -> Vec<(RoomId, String)> {
    tree.workspaces
        .iter()
        .map(|w| (w.room, w.name.clone()))
        .collect()
}

/// The line under the title: **where this record lives**.
///
/// Provenance is not decoration here. A search that offers you records
/// from every room you have is only usable if each row says which room it
/// came from — otherwise two rooms' `notes.md` are two identical rows, and
/// activating one is a guess.
fn provenance(record: &KnownRecord, here: Option<RoomId>, rooms: &[(RoomId, String)]) -> String {
    let where_it_lives = if here.is_some_and(|h| record.is_in(h)) {
        "In this room".to_string()
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
            Some(name) => format!("In {name}"),
            // A room this compositor has no workspace for: a record left
            // behind by a workspace that was deleted, or one minted on a
            // peer. Saying so is better than printing a uuid, and much
            // better than implying it is here.
            None => "In another room".to_string(),
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
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// How well `needle` names this record.
///
/// Title first — a record's title is what the user typed the name of. The
/// identity is matched last and lowest: a file record's id contains its
/// whole path, so a directory name matching is a much weaker signal than
/// the record's own name matching, and scoring them alike is how a palette
/// fills up with everything under one folder.
fn match_score(needle: &str, title: &str, id: &str) -> Option<i32> {
    let title = title.to_lowercase();
    if title == needle {
        return Some(MAX_SCORE);
    }
    if title.starts_with(needle) {
        return Some(88);
    }
    if starts_a_word(&title, needle) {
        return Some(72);
    }
    if title.contains(needle) {
        return Some(60);
    }
    if id.to_lowercase().contains(needle) {
        return Some(30);
    }
    None
}

/// Does `needle` start a word inside `haystack`? `dune` matches
/// `the-dune-notes.md` here but not `redundant.md`.
fn starts_a_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    haystack
        .match_indices(needle)
        .any(|(at, _)| at == 0 || !haystack[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::room_records::ObservedRecord;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use ui::record::{RecordCheckout, RecordId, RecordKind, RecordSet};

    /// Two workspaces — two rooms — with the first active.
    fn chrome_with_rooms(names: &[&str]) -> (Chrome, Vec<RoomId>) {
        let mut tree = WorkspaceTree::default();
        let mut rooms = Vec::new();
        for name in names {
            let mut ws = Workspace::new(WorkspaceId::new(), *name);
            ws.room = RoomId::new();
            rooms.push(ws.room);
            tree.workspaces.push(ws);
        }
        tree.active = tree.workspaces[0].id;
        (ChromeState::new(tree), rooms)
    }

    fn activate(chrome: &Chrome, id: WorkspaceId) {
        chrome.borrow().tree_mut().active = id;
    }

    fn query(provider: &RecordsProvider, needle: &str, chrome: &Chrome) -> Vec<SearchResult> {
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        provider.query(SearchContext {
            needle,
            wanted: &[ResourceKind::Record],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    /// The gesture `plan/rooms.md` rests on: a record that lives in another
    /// room is *findable from here*, and the row says where it lives.
    #[test]
    fn a_record_from_another_room_is_findable_and_says_where_it_lives() {
        let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
        let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
        let book = RecordId::minted();
        index.borrow_mut().reference(rooms[1], &book, &RecordKind::new("book"));
        index.borrow_mut().seed_observed([ObservedRecord {
            id: book.clone(),
            kind: RecordKind::new("book"),
            home: Some(rooms[1]),
            package_id: "dev.qos.ebook".into(),
        }]);
        let provider = RecordsProvider::with_index(index);

        // "book" is this record's type, which is what a record with no
        // natural name is called.
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

    /// Activating brings it here **as a reference**: same record id, in
    /// both rooms, one document.
    #[test]
    fn activating_a_row_references_the_same_record_into_this_room() {
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
            &mut doc,
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
        let ids_here: Vec<RecordId> = index.records_in(here).into_iter().map(|c| c.id).collect();
        let ids_there: Vec<RecordId> = index.records_in(there).into_iter().map(|c| c.id).collect();
        assert_eq!(
            ids_here, ids_there,
            "both rooms name the same record id — a copy would have minted \
             a new one"
        );
    }

    /// The consequence that distinguishes a reference from a copy: an edit
    /// made through one room's reference is visible through the other's.
    #[test]
    fn an_edit_through_one_rooms_reference_is_visible_through_the_others() {
        use automerge::{ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};

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
            &mut doc,
            &ROOT,
            &chrome,
            &crate::shell::search::action_api::ActionArgs::none(),
        )
        .expect("bringing a record here takes no arguments");

        // Open the record the way an app does, from each room's own list
        // of what it holds. One `RecordSet` stands for the one machine
        // both rooms are being read on.
        let mut set = RecordSet::in_memory();
        let mut state = ui::State::new();
        let from_there: RecordCheckout = index
            .borrow()
            .records_in(there)
            .into_iter()
            .find(|c| c.id == book)
            .expect("the room it came from still holds it");
        set.checkout(&mut state, None, &from_there);
        let _ = state.doc.put(&ROOT, "title", "Chapter 3");

        let from_here: RecordCheckout = index
            .borrow()
            .records_in(here)
            .into_iter()
            .find(|c| c.id == book)
            .expect("this room holds it now");
        // Leave and come back, so the park/restore path is the one under
        // test rather than a document that never moved.
        set.checkout(
            &mut state,
            None,
            &RecordCheckout::new(RecordId::minted(), kind.clone()),
        );
        set.checkout(&mut state, None, &from_here);

        let title = match state.doc.get(&ROOT, "title") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(
            title.as_deref(),
            Some("Chapter 3"),
            "the record reached from this room is the record edited through \
             the other — if activating had copied it, this would be empty"
        );
    }

    /// A record already in this room says so rather than naming a room the
    /// user is already in.
    #[test]
    fn a_record_in_this_room_is_labelled_as_being_here() {
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
                .starts_with("In this room"),
            "got {:?}",
            out[0].subtitle
        );
    }

    /// A record whose home room is not a workspace on this machine — a
    /// deleted room, or a peer's — is named honestly rather than with a
    /// uuid nobody can read.
    #[test]
    fn a_record_from_an_unknown_room_says_so() {
        let (chrome, _rooms) = chrome_with_rooms(&["Work"]);
        let index = Rc::new(RefCell::new(RoomRecords::in_memory()));
        index.borrow_mut().seed_observed([ObservedRecord {
            id: RecordId::for_file("/shared/minutes.odt"),
            kind: RecordKind::new("document"),
            home: Some(RoomId::new()),
            package_id: "dev.qos.word-processor".into(),
        }]);
        let provider = RecordsProvider::with_index(index);

        let out = query(&provider, "minutes", &chrome);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].subtitle.as_deref(),
            Some("In another room · /shared/minutes.odt")
        );
    }

    /// Which room is active decides what "here" means: the same record is
    /// "in this room" from one workspace and "In Reading" from the other.
    #[test]
    fn provenance_follows_the_active_room() {
        let (chrome, rooms) = chrome_with_rooms(&["Work", "Reading"]);
        let reading_ws = chrome.borrow().tree().workspaces[1].id;
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
            Some("In this room · book")
        );
    }

    #[test]
    fn scores_stay_inside_the_declared_ceiling() {
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

    #[test]
    fn an_empty_query_returns_nothing() {
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

    /// The kind filter is honoured: with Records unticked the provider
    /// contributes nothing, rather than the registry filtering afterwards.
    #[test]
    fn a_query_that_does_not_want_records_gets_none() {
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
            needle: "a.odt",
            wanted: &[ResourceKind::File],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.is_empty());
    }
}
