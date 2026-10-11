//! Typed paths, field edits at them, invariants and the storage types whose
//! concurrent edits merge per element.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use tilo_resource::{
    ActionCx, Bytes, Collection, Counter, Edit, Grid, Id, Invoke, Model, Outbox, Path, Refusal,
    Replica, Resource, Segment, Set, Settled, resource,
};

#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Snippet::valid)]
pub struct Snippet {
    /// The snippet's title.
    #[model(label)]
    pub title: String,
    pub body: String,
    pub favorite: bool,
    pub language: Language,
    pub tags: Set<String>,
    #[model(readonly)]
    pub uses: Counter,
}

impl Snippet {
    fn valid(&self) -> Result<(), Refusal> {
        if self.title.chars().count() > 20 {
            return Err(Refusal::new("a title has at most 20 characters"));
        }
        Ok(())
    }
}

#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    #[default]
    Text,
    Rust,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Library {
    pub name: String,
    #[model(unit)]
    pub snippets: Collection<Snippet>,
    pub sheet: Grid<String>,
    pub sent: Outbox<String>,
    pub stars: Counter,
    pub logo: Bytes,
}

#[resource(kind = "test.library", label = "Library")]
impl Library {
    /// Create a snippet: a unit of its own.
    #[action(writes = snippets)]
    pub fn new_snippet(&mut self, cx: &mut ActionCx, title: String) {
        self.snippets.push(
            cx.id(),
            Snippet {
                title,
                ..Snippet::default()
            },
        );
    }

    /// Record a use.
    #[action]
    pub fn record_use(&mut self, snippet: &Id) {
        if let Some(snippet) = self.snippets.get_mut(snippet) {
            snippet.uses.add(1);
        }
    }

    /// Star the library.
    #[action]
    pub fn star(&mut self) {
        self.stars.add(1);
    }

    /// Share the library with someone.
    #[action]
    pub fn share(&mut self, cx: &mut ActionCx, with: String) {
        self.sent.push(cx.id(), with);
    }
}

fn edit(replica: &mut Replica<Library>, edit: &Edit<Library>) -> Result<(), String> {
    replica
        .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn first_snippet(replica: &Replica<Library>) -> Id {
    replica
        .state()
        .unwrap()
        .snippets
        .ids()
        .next()
        .unwrap()
        .clone()
}

#[test]
fn a_path_is_plain_data_naming_fields_and_items() {
    let id = Id::from_raw("s1");
    let favorite: Path<Library, bool> = Library::paths().snippets.item(&id).favorite;
    assert_eq!(
        favorite.segments(),
        [
            Segment::Field("snippets".into()),
            Segment::Item(id.clone()),
            Segment::Field("favorite".into()),
        ]
    );
    let json = serde_json::to_string(&favorite).unwrap();
    let back: Path<Library, bool> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, favorite);
    // A row's own paths, rooted at the item, read the same from the record.
    let row = Snippet::paths().favorite;
    let item = Library::paths().snippets.item(&id);
    assert_eq!(row.under(item.path()), favorite);
    assert!(favorite.starts_with(item.path()));
    assert_ne!(Library::paths().snippets.item(&id).title, favorite.cast());
}

#[test]
fn field_edits_apply_at_their_path_and_merge() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    let snippet = Library::paths().snippets.item(&id);
    edit(&mut library, &snippet.favorite.set(true)).unwrap();
    edit(&mut library, &snippet.language.set(Language::Rust)).unwrap();
    edit(&mut library, &snippet.body.splice(0, 0, "cargo")).unwrap();
    edit(&mut library, &snippet.tags.insert(&"ops".to_owned())).unwrap();

    let mut other = library.fork();
    edit(&mut library, &snippet.body.splice(5, 0, " build")).unwrap();
    edit(&mut other, &snippet.body.splice(0, 0, "$ ")).unwrap();
    edit(&mut other, &snippet.tags.insert(&"ci".to_owned())).unwrap();
    library.sync_with(&mut other).unwrap();

    for replica in [&library, &other] {
        let state = replica.state().unwrap();
        let snippet = state.snippets.get(&id).unwrap();
        assert!(snippet.favorite);
        assert_eq!(snippet.language, Language::Rust);
        assert_eq!(snippet.body, "$ cargo build");
        assert_eq!(
            snippet.tags.iter().cloned().collect::<Vec<_>>(),
            ["ci", "ops"]
        );
    }
    assert_eq!(library.state().unwrap(), other.state().unwrap());
    // The log replays to the same state.
    let mut replay = Replica::<Library>::from_doc(other.doc_mut().fork());
    replay.sync_with(&mut library).unwrap();
    assert_eq!(replay.state().unwrap(), library.state().unwrap());
}

#[test]
fn collection_edits_insert_move_and_remove_items() {
    let mut library = Replica::<Library>::new();
    let snippets = Library::paths().snippets;
    let titled = |title: &str| Snippet {
        title: title.into(),
        ..Snippet::default()
    };
    edit(&mut library, &snippets.insert(0, &titled("b"))).unwrap();
    edit(&mut library, &snippets.insert(0, &titled("a"))).unwrap();
    edit(&mut library, &snippets.insert(9, &titled("c"))).unwrap();
    let order = |replica: &Replica<Library>| {
        replica
            .state()
            .unwrap()
            .snippets
            .values()
            .map(|snippet| snippet.title.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&library), ["a", "b", "c"]);
    let ids: Vec<Id> = library.state().unwrap().snippets.ids().cloned().collect();
    edit(&mut library, &snippets.move_to(&ids[2], 0)).unwrap();
    assert_eq!(order(&library), ["c", "a", "b"]);

    let mut other = library.fork();
    // Removing an item asks first, as a delete verb does.
    assert!(
        edit(&mut library, &snippets.remove(&ids[0]))
            .unwrap_err()
            .contains("confirmation")
    );
    library.apply_confirmed(snippets.remove(&ids[0])).unwrap();
    edit(&mut other, &snippets.insert(1, &titled("d"))).unwrap();
    library.sync_with(&mut other).unwrap();
    assert_eq!(order(&library), ["c", "d", "b"]);
    assert_eq!(order(&other), order(&library));
    assert!(
        library
            .apply_confirmed(snippets.remove(&ids[0]))
            .unwrap_err()
            .to_string()
            .contains("no item")
    );
}

#[test]
fn an_edit_reads_only_its_path() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    library.apply(Library::NEW_SNIPPET.with("Other")).unwrap();
    let before = library.heads();
    edit(
        &mut library,
        &Library::paths().snippets.item(&id).favorite.set(true),
    )
    .unwrap();
    let changes = library.doc_mut().get_changes(&before);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].len(), 1, "one scalar written");
}

#[test]
fn a_check_refuses_every_way_in() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    let long = "a title that is far too long".to_owned();
    let title = Library::paths().snippets.item(&id).title;
    // A field edit.
    let refused = edit(&mut library, &title.set(long.clone())).unwrap_err();
    assert!(refused.contains("at most 20"), "{refused}");
    // A splice that makes it too long.
    assert!(edit(&mut library, &title.splice(6, 0, &long)).is_err());
    // A verb.
    assert!(
        library
            .apply(Library::NEW_SNIPPET.with(long.clone()))
            .is_err()
    );
    // A group, refused whole.
    let group = Invoke::group([
        Library::NEW_SNIPPET.with("fine"),
        Library::NEW_SNIPPET.with(long),
    ]);
    assert!(library.apply(group).is_err());
    let state = library.state().unwrap();
    assert_eq!(state.snippets.len(), 1);
    assert_eq!(state.snippets.get(&id).unwrap().title, "Deploy");
    // Within the invariant it applies.
    edit(&mut library, &title.set("Deploy prod".into())).unwrap();
}

thread_local! {
    /// How often `Shelf::valid` ran on this thread.
    static SHELF_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A record whose invariant reads its name and counts its books.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Shelf::valid, check_reads = "name", check_items = "books")]
pub struct Shelf {
    pub name: String,
    pub note: String,
    pub books: Collection<Book>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Book {
    pub title: String,
}

impl Shelf {
    fn valid(&self) -> Result<(), Refusal> {
        SHELF_CHECKS.with(|checks| checks.set(checks.get().saturating_add(1)));
        if self.name == "bad" {
            return Err(Refusal::new("no shelf is called bad"));
        }
        Ok(())
    }
}

#[resource(kind = "test.shelf", label = "Shelf")]
impl Shelf {
    /// Shelve a book.
    #[action]
    pub fn shelve(&mut self, cx: &mut ActionCx, title: String) {
        self.books.push(cx.id(), Book { title });
    }
}

#[test]
fn an_edit_outside_what_a_check_reads_does_not_read_the_record_to_check_it() {
    let mut shelf = Replica::<Shelf>::new();
    shelf.apply(Shelf::SHELVE.with("Dune")).unwrap();
    let id = shelf.state().unwrap().books.ids().next().unwrap().clone();
    let apply = |shelf: &mut Replica<Shelf>, edit: &Edit<Shelf>| {
        shelf
            .apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
            .map(drop)
            .map_err(|error| error.to_string())
    };
    // What the declaration says an edit at each path can change. (Builds
    // with debug assertions still check the record after the skipped ones,
    // to catch a wrong declaration, so this reads the decision itself.)
    let reads = tilo_resource::table::CheckReads {
        fields: &["name"],
        items: &["books"],
    };
    let books = Shelf::paths().books;
    assert!(!reads.touched_by(Shelf::paths().note.segments()));
    assert!(!reads.touched_by(books.item(&id).title.segments()));
    assert!(reads.touched_by(Shelf::paths().name.segments()));
    assert!(reads.touched_by(books.item(&id).path().segments()));
    assert!(reads.touched_by(books.segments()));
    assert!(reads.touched_by(&[]));
    apply(&mut shelf, &Shelf::paths().note.set("by the door".into())).unwrap();
    apply(
        &mut shelf,
        &books.item(&id).title.set("Dune Messiah".into()),
    )
    .unwrap();
    #[cfg(not(debug_assertions))]
    let before = SHELF_CHECKS.with(std::cell::Cell::get);
    let refused = apply(&mut shelf, &Shelf::paths().name.set("bad".into())).unwrap_err();
    assert!(refused.contains("called bad"), "{refused}");
    #[cfg(not(debug_assertions))]
    assert!(SHELF_CHECKS.with(std::cell::Cell::get) > before);
    let state = shelf.state().unwrap();
    assert_eq!(state.note, "by the door");
    assert_eq!(state.books[&id].title, "Dune Messiah");
    assert_eq!(state.name, "");
}

/// A record whose check reads its note but declares it reads only its
/// name: a mistake the framework catches where debug assertions are on.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Misdeclared::valid, check_reads = "name")]
pub struct Misdeclared {
    pub name: String,
    pub note: String,
}

impl Misdeclared {
    fn valid(&self) -> Result<(), Refusal> {
        if self.note == "bad" {
            return Err(Refusal::new("no note says bad"));
        }
        Ok(())
    }
}

#[resource(kind = "test.misdeclared", label = "Misdeclared")]
impl Misdeclared {
    /// Rename it.
    #[action]
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "say that edit cannot change what it reads")]
fn a_check_that_reads_an_undeclared_field_is_caught() {
    let mut record = Replica::<Misdeclared>::new();
    let edit = Misdeclared::paths().note.set("bad".into());
    let _ = record.apply(Invoke::edit(&edit).unwrap());
}

#[test]
fn a_new_record_leaves_out_scalar_fields_holding_their_default() {
    use automerge::ReadDoc;
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    let stored = |library: &Replica<Library>| {
        let path = Library::paths().snippets.item(&id);
        let snippet = path.path().object(library.doc()).unwrap();
        library
            .doc()
            .keys(&snippet)
            .collect::<std::collections::BTreeSet<_>>()
    };
    // The title holds a value, the counter is always kept (increments add
    // up on one counter) and the body is a text, not a scalar; the unset
    // flag and the default language are what absent fields read as.
    let keys = stored(&library);
    assert!(keys.contains("title") && keys.contains("uses"), "{keys:?}");
    for absent in ["favorite", "language"] {
        assert!(!keys.contains(absent), "{absent} stored: {keys:?}");
    }
    let snippet = &library.state().unwrap().snippets[&id];
    assert!(!snippet.favorite && snippet.body.is_empty());
    // Set back to its default, a field is written: it holds a value now.
    let favorite = Library::paths().snippets.item(&id).favorite;
    edit(&mut library, &favorite.set(true)).unwrap();
    edit(&mut library, &favorite.set(false)).unwrap();
    assert!(stored(&library).contains("favorite"));
    assert!(!library.state().unwrap().snippets[&id].favorite);
}

#[test]
fn readonly_fields_change_only_through_verbs() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    let uses = Library::paths().snippets.item(&id).uses;
    let refused = edit(&mut library, &uses.add(5)).unwrap_err();
    assert!(
        refused.contains("only by the record's actions"),
        "{refused}"
    );
    library.apply(Library::RECORD_USE.with(id.clone())).unwrap();
    assert_eq!(library.state().unwrap().snippets[&id].uses.get(), 1);
    assert!(Library::shape().unwrap().readonly_at(uses.segments()));
}

#[test]
fn counters_add_up_across_replicas() {
    let mut library = Replica::<Library>::new();
    let mut other = library.fork();
    library.apply(Library::STAR.invoke()).unwrap();
    other.apply(Library::STAR.invoke()).unwrap();
    edit(&mut other, &Library::paths().stars.add(3)).unwrap();
    library.sync_with(&mut other).unwrap();
    assert_eq!(library.state().unwrap().stars.get(), 5);
    assert_eq!(other.state().unwrap().stars.get(), 5);
    assert_eq!(library.live().unwrap().stars.get(), 5);
}

#[test]
fn first_writes_into_one_row_both_survive() {
    let mut library = Replica::<Library>::new();
    let sheet = Library::paths().sheet;
    edit(&mut library, &sheet.insert_row(0)).unwrap();
    edit(&mut library, &sheet.insert_column(0)).unwrap();
    edit(&mut library, &sheet.insert_column(1)).unwrap();
    let grid = library.state().unwrap().sheet;
    let row = grid.rows().next().cloned().unwrap();
    let columns: Vec<Id> = grid.columns().cloned().collect();
    let mut other = library.fork();
    edit(
        &mut library,
        &sheet.set_cell(&row, &columns[0], Some(&"a".to_owned())),
    )
    .unwrap();
    edit(
        &mut other,
        &sheet.set_cell(&row, &columns[1], Some(&"b".to_owned())),
    )
    .unwrap();
    library.sync_with(&mut other).unwrap();
    let grid = library.state().unwrap().sheet;
    assert_eq!(grid.cell(&row, &columns[0]).map(String::as_str), Some("a"));
    assert_eq!(grid.cell(&row, &columns[1]).map(String::as_str), Some("b"));
    assert_eq!(grid, other.state().unwrap().sheet);
}

#[test]
fn rows_two_replicas_add_under_one_id_keep_both_cells() {
    let mut library = Replica::<Library>::new();
    let sheet = Library::paths().sheet;
    edit(&mut library, &sheet.insert_column(0)).unwrap();
    let column = library
        .state()
        .unwrap()
        .sheet
        .columns()
        .next()
        .cloned()
        .unwrap();
    let mut other = library.fork();
    let row = |id: &str| {
        sheet.edit(tilo_resource::EditOp::InsertRow {
            id: Some(Id::from_raw(id)),
            at: 0,
        })
    };
    edit(&mut library, &row("r40")).unwrap();
    edit(&mut other, &row("r40")).unwrap();
    edit(&mut other, &row("r39")).unwrap();
    edit(
        &mut library,
        &sheet.set_cell(&Id::from_raw("r40"), &column, Some(&"A40".to_owned())),
    )
    .unwrap();
    edit(
        &mut other,
        &sheet.set_cell(&Id::from_raw("r39"), &column, Some(&"B39".to_owned())),
    )
    .unwrap();
    library.sync_with(&mut other).unwrap();
    let grid = library.state().unwrap().sheet;
    assert_eq!(
        grid.cell(&Id::from_raw("r40"), &column).map(String::as_str),
        Some("A40")
    );
    assert_eq!(
        grid.cell(&Id::from_raw("r39"), &column).map(String::as_str),
        Some("B39")
    );
    assert_eq!(grid, other.state().unwrap().sheet);
}

#[test]
fn grid_cells_keep_identity_when_rows_and_columns_move() {
    let mut library = Replica::<Library>::new();
    let sheet = Library::paths().sheet;
    edit(&mut library, &sheet.insert_row(0)).unwrap();
    edit(&mut library, &sheet.insert_row(1)).unwrap();
    edit(&mut library, &sheet.insert_column(0)).unwrap();
    edit(&mut library, &sheet.insert_column(1)).unwrap();
    let grid = library.state().unwrap().sheet;
    let rows: Vec<Id> = grid.rows().cloned().collect();
    let columns: Vec<Id> = grid.columns().cloned().collect();
    edit(
        &mut library,
        &sheet.set_cell(&rows[1], &columns[1], Some(&"b2".to_owned())),
    )
    .unwrap();
    edit(
        &mut library,
        &sheet.cell(&rows[0], &columns[0]).set("a1".into()),
    )
    .unwrap();

    let mut other = library.fork();
    edit(&mut library, &sheet.insert_row(0)).unwrap();
    edit(
        &mut other,
        &sheet.cell(&rows[1], &columns[1]).splice(2, 0, "!"),
    )
    .unwrap();
    library.sync_with(&mut other).unwrap();
    let grid = library.state().unwrap().sheet;
    assert_eq!(grid.rows().count(), 3);
    assert_eq!(grid.cell(&rows[1], &columns[1]).unwrap(), "b2!");
    assert_eq!(grid.cell(&rows[0], &columns[0]).unwrap(), "a1");

    edit(&mut library, &sheet.remove_column(&columns[0])).unwrap();
    let grid = library.state().unwrap().sheet;
    assert_eq!(grid.columns().count(), 1);
    assert!(grid.cell(&rows[0], &columns[0]).is_none());
    assert_eq!(grid.cells().count(), 1);
    assert_eq!(library.live().unwrap().sheet, grid);
}

#[test]
fn an_outbox_queues_entries_the_owner_settles() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::SHARE.with("ada")).unwrap();
    let state = library.state().unwrap();
    let (id, with) = state.sent.pending().next().unwrap();
    assert_eq!(with, "ada");
    let id = id.clone();
    edit(
        &mut library,
        &Library::paths().sent.settle(&id, &Settled::Done(())),
    )
    .unwrap();
    let state = library.state().unwrap();
    assert_eq!(state.sent.pending().count(), 0);
    assert_eq!(
        state.sent.get(&id).unwrap().settled,
        Some(Settled::Done(()))
    );
}

#[test]
fn bytes_inline_small_values_and_reference_large_ones() {
    let small = Bytes::of(b"logo");
    assert_eq!(small.inline(), Some(&b"logo"[..]));
    let large = Bytes::of(&[7; 4096]);
    assert!(large.needs_store());
    assert_eq!(large.len(), 4096);
    for value in [small, large, Bytes::default()] {
        assert_eq!(Bytes::from_json(&value.to_json()).unwrap(), value);
        let mut library = Replica::<Library>::new();
        edit(&mut library, &Library::paths().logo.set(value.clone())).unwrap();
        assert_eq!(library.state().unwrap().logo, value);
    }
}

#[test]
fn consecutive_edits_of_one_path_are_one_gesture() {
    let id = Id::from_raw("s");
    let snippet = Library::paths().snippets.item(&id);
    let action = |edit: Edit<Library>| Library::edit_action(edit).unwrap();
    assert!(Library::coalesces(
        &action(snippet.title.splice(0, 0, "a")),
        &action(snippet.title.splice(1, 0, "b")),
    ));
    // A space ends the word it follows: it belongs to that step, and the
    // next word is a step of its own.
    assert!(Library::coalesces(
        &action(snippet.title.splice(0, 0, "a")),
        &action(snippet.title.splice(1, 0, " ")),
    ));
    assert!(!Library::coalesces(
        &action(snippet.title.splice(0, 0, "a ")),
        &action(snippet.title.splice(2, 0, "b")),
    ));
    assert!(Library::coalesces(
        &action(snippet.favorite.set(true)),
        &action(snippet.favorite.set(false)),
    ));
    assert!(!Library::coalesces(
        &action(snippet.favorite.set(true)),
        &action(snippet.body.replace("x")),
    ));
}

#[test]
fn the_shape_names_units_titles_and_labels() {
    let shape = Library::shape().unwrap();
    let id = Id::from_raw("s");
    let title = Library::paths().snippets.item(&id).title;
    assert_eq!(
        shape.unit_of(title.segments()),
        [Segment::Field("snippets".into()), Segment::Item(id)]
    );
    assert!(shape.unit_of(Library::paths().name.segments()).is_empty());
    let snippet = Snippet::SHAPE.fields();
    assert_eq!(snippet[0].title, "The snippet's title");
    assert!(snippet[0].label);
    assert_eq!(
        Snippet {
            title: "Deploy\nmore".into(),
            ..Snippet::default()
        }
        .label()
        .as_deref(),
        Some("Deploy")
    );
}

#[test]
fn the_door_stamps_the_time_not_the_caller() {
    let mut library = Replica::<Library>::new();
    library.set_now_ms(42);
    let value = serde_json::json!({ "op": { "action": "star", "args": {} }, "at": 7 });
    let applied = library
        .apply(Invoke::<Library>::apply(&serde_json::from_value(value).unwrap()).unwrap())
        .unwrap();
    let action: <Library as Resource>::Action =
        serde_json::from_slice(&applied.action.payload).unwrap();
    assert_eq!(action.now_ms, 42);
}

#[test]
fn a_path_reads_its_value_and_names_its_tree_node() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    let state = library.state().unwrap();
    let snippet = Library::paths().snippets.item(&id);
    assert_eq!(
        snippet.title.get(&state).map(String::as_str),
        Some("Deploy")
    );
    assert_eq!(snippet.favorite.get(&state), Some(&false));
    assert!(
        Library::paths()
            .snippets
            .item(&Id::from_raw("gone"))
            .title
            .get(&state)
            .is_none()
    );
    let tree = snippet
        .favorite
        .set(true)
        .tree_path("room", "app", "record");
    assert_eq!(
        tree,
        tilo_action_policy::TreePath::room("room")
            .app("app")
            .kind("test.library")
            .resource("record")
            .item(id.as_str())
            .field("favorite")
            .action(tilo_resource::FIELD_EDIT)
    );
    assert_eq!(
        Library::STAR.key("app"),
        tilo_resource::ActionKey::new("app", "test.library", "star")
    );
}

#[test]
fn history_records_what_an_edit_or_verb_did_and_to_which_item() {
    let mut library = Replica::<Library>::new();
    library
        .apply(Library::NEW_SNIPPET.with("Deploy script"))
        .unwrap();
    let id = first_snippet(&library);
    edit(
        &mut library,
        &Library::paths().snippets.item(&id).favorite.set(true),
    )
    .unwrap();
    let described = |library: &mut Replica<Library>| {
        library
            .versions()
            .last()
            .and_then(|version| version.context.clone())
            .and_then(|context| context.described())
            .unwrap()
    };
    let favorite = described(&mut library);
    let tilo_actions::Did::Edit(edit_entry) = &favorite.what else {
        panic!("{favorite:?}");
    };
    assert_eq!(
        (edit_entry.ty.as_str(), edit_entry.field.as_str()),
        ("snippet", "favorite")
    );
    assert_eq!(
        edit_entry.old,
        tilo_actions::Scalar::from_json(&false.into())
    );
    assert_eq!(
        edit_entry.new,
        tilo_actions::Scalar::from_json(&true.into())
    );
    let item = favorite.item.unwrap();
    assert_eq!(
        (item.id.as_str(), item.label.as_str()),
        (id.as_str(), "Deploy script")
    );

    library.apply(Library::RECORD_USE.with(id)).unwrap();
    let used = described(&mut library);
    let tilo_actions::Did::Verb(verb) = &used.what else {
        panic!("{used:?}");
    };
    assert_eq!(verb.action.action, "record-use");
    assert_eq!(used.item.unwrap().label, "Deploy script");
}

#[test]
fn the_invoker_learns_the_ids_its_action_minted() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("One")).unwrap();
    let envelope = library
        .apply(Library::NEW_SNIPPET.with("Two"))
        .unwrap()
        .clone();
    let created = tilo_resource::minted_ids(&envelope).next().unwrap();
    assert_eq!(library.state().unwrap().snippets[&created].title, "Two");
}

#[test]
fn search_finds_an_item_by_its_codes_and_variants() {
    let mut library = Replica::<Library>::new();
    library.apply(Library::NEW_SNIPPET.with("Deploy")).unwrap();
    let id = first_snippet(&library);
    edit(
        &mut library,
        &Library::paths()
            .snippets
            .item(&id)
            .language
            .set(Language::Rust),
    )
    .unwrap();
    let entries = Library::search(&library.state().unwrap());
    assert_eq!(entries.len(), 1);
    assert!(entries[0].text.contains("rust"), "{:?}", entries[0].text);
}

/// A path names the object its value is stored in, through the items of a
/// collection: what an editor working on document objects binds to.
#[test]
fn a_path_finds_the_object_its_value_is_stored_in() {
    use automerge::ReadDoc as _;

    let mut library = Replica::<Library>::new();
    library
        .apply(Library::NEW_SNIPPET.with(library::NewSnippet {
            title: "Deploy".into(),
        }))
        .unwrap();
    let id = library
        .state()
        .unwrap()
        .snippets
        .ids()
        .next()
        .unwrap()
        .clone();
    let paths = Library::paths();
    let body = paths.snippets.item(&id).body;
    let text = body.object(library.doc()).expect("the body's text");
    assert_eq!(library.doc().text(&text).unwrap(), "");
    library
        .apply(Invoke::edit(&body.splice(0, 0, "kubectl")).unwrap())
        .unwrap();
    assert_eq!(library.doc().text(&text).unwrap(), "kubectl");
    assert!(
        paths
            .snippets
            .item(&id)
            .favorite
            .object(library.doc())
            .is_none(),
        "a scalar is stored in no object of its own"
    );
    assert!(
        paths
            .snippets
            .item(&Id::from_raw("gone"))
            .body
            .object(library.doc())
            .is_none()
    );
}
