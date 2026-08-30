//! Records provider — the gesture that brings your own things into a room.
//! A room starts empty; moving a record in costs one step: type its
//! name, the row says which room it lives in, Enter **brings it here**.
//!
//! Bringing is referencing, not copying: activating a row adds this
//! room's reference to the record
//! ([`crate::shell::room_records::RoomRecords::reference`]) — no bytes
//! move, no id is minted, the record stays one document. Copying is a
//! different, deliberate verb ([`ui::record::room::duplicate`]). A
//! record whose identity is a path is also opened, since bringing and
//! opening are meant to be the same gesture.
//!
//! [`RoomRecords`] answers both halves — which records exist, and which
//! rooms hold which — already in memory, so `query` only reads maps and
//! matches strings; it never touches the filesystem or a CRDT.

use std::cell::RefCell;
use std::rc::Rc;

use crate::shell::chrome_state::Chrome;
use crate::shell::room_records::{KnownRecord, RoomRecords};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
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
        Self { index }
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
        // Dropped before any result is built: a row's callback borrows
        // the chrome mutably.
        let (here, rooms) = {
            let tree = ctx.chrome.tree();
            (tree.active_workspace().map(|w| w.room), room_names(&tree))
        };

        let index = self.index.borrow();
        let mut ranked: Vec<(i32, KnownRecord, String)> = Vec::new();
        for record in index.known().iter() {
            let title = record.title();
            let Some(score) = match_score(&needle, &title, record.id.as_str()) else {
                continue;
            };
            ranked.push((score, record.clone(), title));
        }
        drop(index);
        // Best first; ties broken by title so the order is stable
        // rather than following map iteration.
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

/// Enter on a record row. Two effects, in this order: the reference is
/// recorded first (it's what "bring it here" *means*, whether or not
/// there's anything to open), so a launch that fails still leaves the
/// record in this room.
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

    // A path-identified record is also opened; anything else is brought
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

/// The line under the title: where this record lives. Not decoration —
/// without it, two rooms' `notes.md` are identical rows and activating
/// one is a guess.
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
            // No workspace for this room (deleted, or minted on a peer):
            // say so rather than print a uuid.
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
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// How well `needle` names this record. Title first; the identity is
/// matched last and lowest — a file record's id contains its whole path,
/// so a directory-name match is a weaker signal than the title matching.
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
    haystack.match_indices(needle).any(|(at, _)| {
        at == 0
            || haystack.get(..at).is_some_and(|prefix| {
                !prefix
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::room_records::ObservedRecord;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
    use ui::record::{RecordCheckout, RecordId, RecordKind, RecordSet};

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

    #[test]
    fn a_record_from_another_room_is_findable_and_says_where_it_lives() {
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
            package_id: "dev.qos.ebook".into(),
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

    #[test]
    #[allow(
        clippy::similar_names,
        reason = "here/there are the domain vocabulary for the two rooms under test, not a naming accident"
    )]
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

    #[test]
    #[allow(
        clippy::similar_names,
        reason = "here/there are the domain vocabulary for the two rooms under test, not a naming accident"
    )]
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
        // Leave and come back, so the park/restore path is exercised.
        set.checkout(
            &mut state,
            None,
            &RecordCheckout::new(RecordId::minted(), kind),
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
