//! Incremental reads: after any change, the state a view holds — refreshed
//! from only what the change touched — equals a full read, and the refresh
//! reads only what the change touched.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test assertions and counters"
)]

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_automerge::ApplyIncoming;

use tilo_resource::__private::{ObjId, Value};
use tilo_resource::history;
use tilo_resource::{
    ActionCx, Collection, Doc, Grid, Id, Identified, Keyed, Map, Model, ModelError, Ordered,
    Reader, Replica, Resource, Set, Slot, resource,
};

thread_local! {
    static LOADS: Cell<usize> = const { Cell::new(0) };
    static CLONES: Cell<usize> = const { Cell::new(0) };
    static IDS: Cell<usize> = const { Cell::new(0) };
}

/// Text that counts how often it is read from the document and copied.
#[derive(Debug, Default, PartialEq)]
pub struct Counted(pub String);

impl Clone for Counted {
    fn clone(&self) -> Self {
        CLONES.with(|clones| clones.set(clones.get() + 1));
        Self(self.0.clone())
    }
}

impl Model for Counted {
    tilo_resource::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        self.0.to_json()
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        String::from_json(value).map(Self)
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        LOADS.with(|loads| loads.set(loads.get() + 1));
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.0.store(doc, slot)
    }
}

fn loads() -> usize {
    LOADS.with(Cell::get)
}

fn clones() -> usize {
    CLONES.with(Cell::get)
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notebook {
    pub title: String,
    pub settings: Settings,
    pub notes: Collection<Note>,
    pub tags: BTreeMap<String, u32>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: String,
    pub wrap: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub body: Counted,
    pub pinned: bool,
    pub detail: Option<Detail>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Detail {
    pub color: String,
}

#[resource(kind = "test.notebook", label = "Notebook")]
impl Notebook {
    /// Rename.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }

    /// Set the theme.
    #[action]
    pub fn theme(&mut self, theme: String) {
        self.settings.theme = theme;
    }

    /// Add a note.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, body: String) {
        self.notes.push(
            cx.id(),
            Note {
                body: Counted(body),
                ..Note::default()
            },
        );
    }

    /// Edit a note's text.
    #[action]
    pub fn edit(&mut self, note: &Id, body: String) -> Result<(), &'static str> {
        self.notes.get_mut(note).ok_or("no such note")?.body = Counted(body);
        Ok(())
    }

    /// Pin and colour a note, or unpin it with no colour.
    #[action]
    pub fn decorate(&mut self, note: &Id, color: String) -> Result<(), &'static str> {
        let note = self.notes.get_mut(note).ok_or("no such note")?;
        note.pinned = !color.is_empty();
        note.detail = (!color.is_empty()).then_some(Detail { color });
        Ok(())
    }

    /// Move a note.
    #[action]
    pub fn move_note(&mut self, note: &Id, index: usize) {
        self.notes.move_to(note, index);
    }

    /// Delete a note.
    #[action]
    pub fn delete(&mut self, note: &Id) {
        self.notes.remove(note);
    }

    /// Count a tag.
    #[action]
    pub fn tag(&mut self, tag: String) {
        *self.tags.entry(tag).or_insert(0) += 1;
    }
}

/// A small deterministic generator, so a failing sequence replays.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap() % bound.max(1)
    }
}

fn random_action(replica: &mut Replica<Notebook>, rng: &mut Lcg, step: usize) {
    let ids: Vec<Id> = replica.state().unwrap().notes.ids().cloned().collect();
    let pick = |rng: &mut Lcg| ids.get(rng.next(ids.len())).cloned();
    let result = match rng.next(9) {
        0 => replica.apply(Notebook::RENAME.with(format!("title {step}"))),
        1 => replica.apply(Notebook::THEME.with(format!("theme {}", step % 3))),
        2 | 3 => replica.apply(Notebook::ADD.with(format!("note {step}"))),
        4 => match pick(rng) {
            Some(id) => replica.apply(Notebook::EDIT.with((id, format!("edited {step}")))),
            None => return,
        },
        5 => match pick(rng) {
            Some(id) => {
                let color = if step.is_multiple_of(2) {
                    format!("c{step}")
                } else {
                    String::new()
                };
                replica.apply(Notebook::DECORATE.with((id, color)))
            }
            None => return,
        },
        6 => match pick(rng) {
            Some(id) => replica.apply(Notebook::MOVE_NOTE.with((id, rng.next(ids.len() + 1)))),
            None => return,
        },
        7 => match pick(rng) {
            Some(id) => replica.apply(Notebook::DELETE.with(id)),
            None => return,
        },
        _ => replica.apply(Notebook::TAG.with(format!("t{}", step % 4))),
    };
    result.unwrap();
}

#[test]
fn the_refreshed_state_equals_a_full_read_after_actions_and_merges() {
    // phase: the_refreshed_state_equals_a_full_read_after_every_action
    {
        let mut rng = Lcg(7);
        let mut replica = Replica::<Notebook>::new();
        replica.live().unwrap();
        for step in 0..400 {
            random_action(&mut replica, &mut rng, step);
            let full = replica.state().unwrap();
            assert_eq!(replica.live().unwrap(), &full, "after step {step}");
        }
    }
    // phase: the_refreshed_state_follows_merged_collaborators_and_raw_changes
    {
        let mut rng = Lcg(11);
        let mut left = Replica::<Notebook>::new();
        for step in 0..20 {
            random_action(&mut left, &mut rng, step);
        }
        let mut right = left.fork();
        left.live().unwrap();
        for round in 0..30 {
            for step in 0..3 {
                random_action(&mut left, &mut rng, round * 10 + step);
                random_action(&mut right, &mut rng, round * 10 + step + 5);
            }
            left.sync_with(&mut right).unwrap();
            let full = left.state().unwrap();
            assert_eq!(left.live().unwrap(), &full, "after round {round}");
        }
        // A change outside the resource's actions (a restore to an old version
        // writes the old values back) is followed too.
        let old = left.versions()[5].hash;
        let past = left.state_at(&[old]).unwrap();
        let mut doc = left.doc().clone();
        tilo_resource::Fields::write_fields(&past, &mut doc, &tilo_resource::__private::ROOT)
            .unwrap();
        doc.commit();
        left.doc_mut().merge_incoming(&mut doc).unwrap();
        let full = left.state().unwrap();
        assert_eq!(left.live().unwrap(), &full);
    }
}

#[test]
fn a_reader_whose_state_a_view_holds_copies_only_the_touched_item() {
    let mut replica = Replica::<Notebook>::new();
    for n in 0..1000 {
        replica
            .apply(Notebook::ADD.with(format!("note {n}")))
            .unwrap();
    }
    let mut reader = Reader::<Notebook>::open(replica.doc_mut()).unwrap();
    let target = reader.state().notes.ids().nth(500).cloned().unwrap();
    let was = reader.state().notes[&target].body.0.clone();
    // A view keeps the state it was shown, as a surface's `present` may.
    let shown = Rc::clone(reader.state());

    replica
        .apply(Notebook::EDIT.with((target.clone(), "changed".into())))
        .unwrap();
    let before = clones();
    assert!(reader.sync(replica.doc()).unwrap());
    assert_eq!(clones() - before, 1, "only the edited note is copied");

    let now = reader.state();
    assert!(
        !Rc::ptr_eq(now, &shown),
        "the shown state was not changed in place"
    );
    assert_eq!(
        shown.notes[&target].body.0, was,
        "the view still sees what it was shown"
    );
    assert_eq!(now.notes[&target].body.0, "changed");
    assert_eq!(now.as_ref(), &replica.state().unwrap());
    for (id, note) in now.notes.iter() {
        let held = &shown.notes[id];
        assert_eq!(std::ptr::eq(note, held), *id != target, "note {id} shared");
    }
}

#[test]
fn a_held_state_stays_as_it_was_shown_while_the_reader_follows() {
    let mut rng = Lcg(23);
    let mut replica = Replica::<Notebook>::new();
    for step in 0..20 {
        random_action(&mut replica, &mut rng, step);
    }
    let mut reader = Reader::<Notebook>::open(replica.doc_mut()).unwrap();
    let mut held: Vec<(Rc<Notebook>, Notebook)> = Vec::new();
    for step in 20..220 {
        held.push((Rc::clone(reader.state()), replica.state().unwrap()));
        random_action(&mut replica, &mut rng, step);
        reader.sync(replica.doc()).unwrap();
        assert_eq!(
            reader.state().as_ref(),
            &replica.state().unwrap(),
            "after step {step}"
        );
    }
    for (at, (shown, read)) in held.iter().enumerate() {
        assert_eq!(shown.as_ref(), read, "state shown before step {}", at + 20);
    }
}

#[test]
fn a_collection_keeps_its_membership_exactly_while_its_ids_hold() {
    let ids = |notebook: &Notebook| notebook.notes.ids().cloned().collect::<Vec<_>>();
    let mut rng = Lcg(23);
    let mut replica = Replica::<Notebook>::new();
    let live = replica.live().unwrap();
    let mut last = (live.notes.membership(), ids(live));
    let mut kept = 0_usize;
    for step in 0..400 {
        random_action(&mut replica, &mut rng, step);
        let live = replica.live().unwrap();
        let now = (live.notes.membership(), ids(live));
        if now.0 == last.0 {
            assert_eq!(
                now.1, last.1,
                "the same membership, other ids at step {step}"
            );
            kept = kept.saturating_add(1);
        }
        last = now;
    }
    assert!(kept > 0, "some steps edit without moving an item");

    // Editing an item keeps the membership; adding, moving and removing
    // one change it.
    let notes = replica.state().unwrap().notes;
    assert!(notes.len() > 1, "a move needs somewhere to go");
    let last_place = notes.len();
    let target = notes.ids().next().cloned().unwrap();
    let before = replica.live().unwrap().notes.membership();
    replica
        .apply(Notebook::EDIT.with((target.clone(), "edited".into())))
        .unwrap();
    replica
        .apply(Notebook::DECORATE.with((target.clone(), "blue".into())))
        .unwrap();
    replica
        .apply(Notebook::RENAME.with("Kept".to_owned()))
        .unwrap();
    assert_eq!(replica.live().unwrap().notes.membership(), before);
    for change in [
        Notebook::ADD.with("another".to_owned()),
        Notebook::MOVE_NOTE.with((target.clone(), last_place)),
        Notebook::DELETE.with(target),
    ] {
        let before = replica.live().unwrap().notes.membership();
        replica.apply(change).unwrap();
        assert_ne!(replica.live().unwrap().notes.membership(), before);
    }
}

#[test]
fn editing_one_note_of_many_reads_only_that_note() {
    let mut replica = Replica::<Notebook>::new();
    for n in 0..500 {
        replica
            .apply(Notebook::ADD.with(format!("note {n}")))
            .unwrap();
    }
    let target = replica
        .state()
        .unwrap()
        .notes
        .ids()
        .nth(250)
        .cloned()
        .unwrap();
    replica.live().unwrap();

    // (Applying an action reads its record; what is measured here is what
    // a view pays to follow the change.)
    replica
        .apply(Notebook::EDIT.with((target.clone(), "changed".into())))
        .unwrap();
    let before = loads();
    let live = replica.live().unwrap();
    assert_eq!(loads() - before, 1, "one note's text read again");
    assert_eq!(live.notes[&target].body.0, "changed");

    replica
        .apply(Notebook::DECORATE.with((target.clone(), "red".into())))
        .unwrap();
    replica.apply(Notebook::RENAME.with("Renamed")).unwrap();
    let before = loads();
    replica.live().unwrap();
    assert_eq!(loads() - before, 0, "no note text read for other fields");

    replica
        .apply(Notebook::MOVE_NOTE.with((target.clone(), 0)))
        .unwrap();
    let before = loads();
    let live = replica.live().unwrap();
    assert_eq!(loads() - before, 0, "a move reads an order key, not a note");
    assert_eq!(live.notes.ids().next(), Some(&target));
}

#[test]
fn history_pages_newest_first_and_lists_every_change_once() {
    let mut left = Replica::<Notebook>::new();
    left.apply(Notebook::ADD.with("shared")).unwrap();
    let mut right = left.fork();
    let mut rng = Lcg(3);
    for round in 0..15 {
        random_action(&mut left, &mut rng, round);
        random_action(&mut right, &mut rng, round + 100);
        if round % 4 == 0 {
            left.sync_with(&mut right).unwrap();
        }
    }
    left.sync_with(&mut right).unwrap();
    let doc = &mut left.doc().clone();
    let all = history::versions(doc);

    let mut paged = Vec::new();
    let mut cursor = None;
    loop {
        let page = history::page(doc, cursor.as_ref(), 4);
        assert!(page.versions.len() <= 4);
        paged.extend(page.versions);
        match page.older {
            Some(older) => cursor = Some(older),
            None => break,
        }
    }
    assert_eq!(paged.len(), all.len(), "every change listed exactly once");
    let mut sorted: Vec<_> = paged.iter().map(|v| v.hash).collect();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len());
    // Newest first: nothing is listed before a change that depends on it.
    let position: std::collections::HashMap<_, _> = paged
        .iter()
        .enumerate()
        .map(|(index, version)| (version.hash, index))
        .collect();
    for version in &paged {
        let change = doc.get_change_by_hash(&version.hash).unwrap();
        for dep in change.deps() {
            assert!(
                position[dep] > position[&version.hash],
                "{dep} listed too early"
            );
        }
    }
    let heads = doc.get_heads();
    if let [head] = heads.as_slice() {
        assert_eq!(paged.first().map(|v| v.hash), Some(*head));
    }
}

#[test]
fn the_first_page_of_a_long_history_is_its_newest_end() {
    let mut replica = Replica::<Notebook>::new();
    for n in 0..2_000 {
        replica
            .apply(Notebook::TAG.with(format!("t{}", n % 7)))
            .unwrap();
    }
    let doc = &mut replica.doc().clone();
    // How much of the history a page reads is counted in `history`'s own
    // tests; here the page is the newest end of the whole history.
    let page = history::page(doc, None, 50);
    let all = history::versions(doc);
    assert_eq!(page.versions.len(), 50);
    assert_eq!(
        page.versions.first().map(|v| v.hash),
        all.last().map(|v| v.hash)
    );
    assert!(page.older.is_some());
    // Every change records what the action that made it did.
    assert!(
        page.versions
            .iter()
            .all(|v| v.context.as_ref().is_some_and(|c| c.what.is_some()))
    );
    assert_eq!(<Notebook as Resource>::KIND, "test.notebook");
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Sheet {
    pub grid: Grid<Counted>,
}

#[resource(kind = "test.sheet", label = "Sheet")]
impl Sheet {
    /// Add a row.
    #[action]
    pub fn add_row(&mut self, cx: &mut ActionCx) {
        let at = self.grid.rows().len();
        self.grid.insert_row(at, cx.id());
    }

    /// Add a column.
    #[action]
    pub fn add_column(&mut self, cx: &mut ActionCx) {
        let at = self.grid.columns().len();
        self.grid.insert_column(at, cx.id());
    }

    /// Write a cell, or clear it with no text.
    #[action]
    pub fn write(&mut self, cell: (String, String), text: String) {
        let value = (!text.is_empty()).then_some(Counted(text));
        self.grid
            .set_cell(&Id::from_raw(cell.0), &Id::from_raw(cell.1), value);
    }

    /// Remove a row.
    #[action]
    pub fn drop_row(&mut self, line: String) {
        self.grid.remove_row(&Id::from_raw(line));
    }

    /// Remove a column.
    #[action]
    pub fn drop_column(&mut self, line: String) {
        self.grid.remove_column(&Id::from_raw(line));
    }
}

fn cell(row: &Id, column: &Id) -> (String, String) {
    (row.as_str().to_owned(), column.as_str().to_owned())
}

fn random_sheet_action(replica: &mut Replica<Sheet>, rng: &mut Lcg, step: usize) {
    let state = replica.state().unwrap();
    let rows: Vec<Id> = state.grid.rows().cloned().collect();
    let columns: Vec<Id> = state.grid.columns().cloned().collect();
    let row = rows.get(rng.next(rows.len())).cloned();
    let column = columns.get(rng.next(columns.len())).cloned();
    let result = match (rng.next(10), row, column) {
        (0, ..) => replica.apply(Sheet::ADD_ROW.with(())),
        (1, ..) => replica.apply(Sheet::ADD_COLUMN.with(())),
        (2, Some(row), _) => replica.apply(Sheet::DROP_ROW.with(row.as_str().to_owned())),
        (3, _, Some(column)) => replica.apply(Sheet::DROP_COLUMN.with(column.as_str().to_owned())),
        (4, Some(row), Some(column)) => {
            replica.apply(Sheet::WRITE.with((cell(&row, &column), String::new())))
        }
        (_, Some(row), Some(column)) => {
            replica.apply(Sheet::WRITE.with((cell(&row, &column), format!("v{step}"))))
        }
        _ => replica.apply(Sheet::ADD_ROW.with(())),
    };
    result.unwrap();
}

#[test]
fn a_grid_refreshes_only_the_cells_a_change_touched() {
    // phase: the_refreshed_grid_equals_a_full_read_after_every_action
    {
        let mut rng = Lcg(5);
        let mut replica = Replica::<Sheet>::new();
        replica.live().unwrap();
        for step in 0..300 {
            random_sheet_action(&mut replica, &mut rng, step);
            let full = replica.state().unwrap();
            assert_eq!(replica.live().unwrap(), &full, "after step {step}");
        }
    }
    // phase: writing_one_cell_of_many_reads_only_that_cell
    {
        let mut replica = Replica::<Sheet>::new();
        replica.apply(Sheet::ADD_COLUMN.with(())).unwrap();
        for _ in 0..200 {
            replica.apply(Sheet::ADD_ROW.with(())).unwrap();
        }
        let state = replica.state().unwrap();
        let rows: Vec<Id> = state.grid.rows().cloned().collect();
        let column = state.grid.columns().next().cloned().unwrap();
        for (n, row) in rows.iter().enumerate() {
            replica
                .apply(Sheet::WRITE.with((cell(row, &column), format!("cell {n}"))))
                .unwrap();
        }
        replica.live().unwrap();
        replica
            .apply(Sheet::WRITE.with((cell(&rows[100], &column), "changed".into())))
            .unwrap();
        let before = loads();
        let live = replica.live().unwrap();
        assert_eq!(loads() - before, 1, "one cell read again");
        assert_eq!(live.grid.cell(&rows[100], &column).unwrap().0, "changed");
    }
}

#[test]
fn a_reader_whose_state_a_view_holds_copies_one_chunk_of_a_grid() {
    let mut replica = Replica::<Sheet>::new();
    replica.apply(Sheet::ADD_COLUMN.with(())).unwrap();
    for _ in 0..1000 {
        replica.apply(Sheet::ADD_ROW.with(())).unwrap();
    }
    let state = replica.state().unwrap();
    let rows: Vec<Id> = state.grid.rows().cloned().collect();
    let column = state.grid.columns().next().cloned().unwrap();
    for (n, row) in rows.iter().enumerate() {
        replica
            .apply(Sheet::WRITE.with((cell(row, &column), format!("cell {n}"))))
            .unwrap();
    }
    let mut reader = Reader::<Sheet>::open(replica.doc_mut()).unwrap();
    // A view keeps the state it was shown, as a surface's `present` may.
    let shown = Rc::clone(reader.state());

    replica
        .apply(Sheet::WRITE.with((cell(&rows[500], &column), "changed".into())))
        .unwrap();
    let before = clones();
    assert!(reader.sync(replica.doc()).unwrap());
    let copied = clones() - before;
    assert!(
        copied <= 64,
        "{copied} cells copied: one chunk of rows at most, not the sheet"
    );

    let now = reader.state();
    assert!(!Rc::ptr_eq(now, &shown), "the shown state was not changed");
    assert_eq!(shown.grid.cell(&rows[500], &column).unwrap().0, "cell 500");
    assert_eq!(now.grid.cell(&rows[500], &column).unwrap().0, "changed");
    assert_eq!(now.as_ref(), &replica.state().unwrap());
    let shared = rows
        .iter()
        .filter(|row| {
            std::ptr::eq(
                now.grid.cell(row, &column).unwrap(),
                shown.grid.cell(row, &column).unwrap(),
            )
        })
        .count();
    assert!(shared >= 1000 - 64, "{shared} of 1000 cells still shared");
}

#[test]
fn a_held_grid_stays_as_it_was_shown_while_the_reader_follows() {
    let mut rng = Lcg(29);
    let mut replica = Replica::<Sheet>::new();
    for step in 0..60 {
        random_sheet_action(&mut replica, &mut rng, step);
    }
    let mut reader = Reader::<Sheet>::open(replica.doc_mut()).unwrap();
    let mut held: Vec<(Rc<Sheet>, Sheet)> = Vec::new();
    for step in 60..360 {
        held.push((Rc::clone(reader.state()), replica.state().unwrap()));
        random_sheet_action(&mut replica, &mut rng, step);
        reader.sync(replica.doc()).unwrap();
        assert_eq!(
            reader.state().as_ref(),
            &replica.state().unwrap(),
            "after step {step}"
        );
    }
    for (at, (shown, read)) in held.iter().enumerate() {
        assert_eq!(shown.as_ref(), read, "state shown before step {}", at + 60);
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    pub id: String,
    pub body: Counted,
}

impl Identified for Card {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub cards: Keyed<Card>,
    pub archive: Keyed<Card>,
    pub stack: Ordered<Card>,
    pub seen: Set<Counted>,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Add a card, to the archive when asked.
    #[action]
    pub fn add_card(&mut self, cx: &mut ActionCx, body: String, archived: bool) {
        let card = Card {
            id: cx.id().as_str().to_owned(),
            body: Counted(body),
        };
        if archived {
            self.archive.push(card);
        } else {
            self.cards.push(card);
        }
    }

    /// Edit a card's text, wherever it is.
    #[action(item = cards)]
    pub fn edit_card(&mut self, card: &Id, body: String) -> Result<(), &'static str> {
        let found = match self.cards.find_mut(card) {
            Some(found) => found,
            None => self.archive.find_mut(card).ok_or("no such card")?,
        };
        found.body = Counted(body);
        Ok(())
    }

    /// Archive a card.
    #[action(item = cards)]
    pub fn archive_card(&mut self, card: &Id) -> Result<(), &'static str> {
        let card = self.cards.take(card).ok_or("no such card")?;
        self.archive.push(card);
        self.archive.settle();
        Ok(())
    }

    /// Stack a card at a place, the top when past it.
    #[action]
    pub fn stack_card(&mut self, cx: &mut ActionCx, body: String, under: usize) {
        let card = Card {
            id: cx.id().as_str().to_owned(),
            body: Counted(body),
        };
        let at = under.min(self.stack.len());
        self.stack.insert(at, card);
    }

    /// Drop the card at the bottom of the stack.
    #[action]
    pub fn unstack(&mut self) {
        if !self.stack.is_empty() {
            self.stack.remove(0);
        }
    }

    /// Mark a word seen, or unseen.
    #[action]
    pub fn see(&mut self, word: String, seen: bool) {
        if seen {
            self.seen.insert(Counted(word));
        } else {
            self.seen.remove(&Counted(word));
        }
    }
}

fn random_board_action(replica: &mut Replica<Board>, rng: &mut Lcg, step: usize) {
    let state = replica.state().unwrap();
    let ids: Vec<Id> = state
        .cards
        .iter()
        .chain(state.archive.iter())
        .map(Identified::id)
        .collect();
    let live: Vec<Id> = state.cards.iter().map(Identified::id).collect();
    let result = match rng.next(9) {
        0 | 1 => replica.apply(Board::ADD_CARD.with((format!("card {step}"), rng.next(3) == 0))),
        2 | 3 => match ids.get(rng.next(ids.len())) {
            Some(id) => replica.apply(Board::EDIT_CARD.with((id.clone(), format!("edit {step}")))),
            None => return,
        },
        4 => match live.get(rng.next(live.len())) {
            Some(id) => replica.apply(Board::ARCHIVE_CARD.with(id.clone())),
            None => return,
        },
        5 => replica.apply(Board::STACK_CARD.with((format!("stacked {step}"), rng.next(8)))),
        6 => replica.apply(Board::UNSTACK.with(())),
        _ => replica.apply(Board::SEE.with((format!("w{}", rng.next(90)), rng.next(3) != 0))),
    };
    result.unwrap();
}

#[test]
fn a_reader_whose_state_a_view_holds_copies_only_the_touched_items() {
    let mut replica = Replica::<Board>::new();
    for n in 0..300 {
        replica
            .apply(Board::ADD_CARD.with((format!("card {n}"), n % 2 == 1)))
            .unwrap();
        replica
            .apply(Board::SEE.with((format!("word {n:03}"), true)))
            .unwrap();
    }
    let mut reader = Reader::<Board>::open(replica.doc_mut()).unwrap();
    let target = reader.state().cards[75].id();
    let was = reader.state().cards[75].body.0.clone();
    // A view keeps the state it was shown, as a surface's `present` may.
    let shown = Rc::clone(reader.state());

    // Editing one card copies the cards once, and no other field.
    replica
        .apply(Board::EDIT_CARD.with((target.clone(), "changed".into())))
        .unwrap();
    let before = clones();
    assert!(reader.sync(replica.doc()).unwrap());
    assert_eq!(clones() - before, 150, "the 150 cards are copied once");
    let now = Rc::clone(reader.state());
    assert_eq!(shown.cards.find(&target).unwrap().body.0, was);
    assert_eq!(now.cards.find(&target).unwrap().body.0, "changed");
    assert!(
        std::ptr::eq(now.archive.as_ptr(), shown.archive.as_ptr()),
        "the archive the edit did not touch is shared, not copied"
    );
    assert_eq!(now.as_ref(), &replica.state().unwrap());

    // Marking a word seen copies one chunk of the set, and no card.
    replica
        .apply(Board::SEE.with(("word 150a".into(), true)))
        .unwrap();
    let before = clones();
    assert!(reader.sync(replica.doc()).unwrap());
    let copied = clones() - before;
    assert!(
        copied <= 64,
        "{copied} words copied: one chunk, not the set"
    );
    assert_eq!(shown.seen.len(), 300);
    assert_eq!(reader.state().seen.len(), 301);
    assert!(std::ptr::eq(
        reader.state().cards.as_ptr(),
        now.cards.as_ptr()
    ));
    assert_eq!(reader.state().as_ref(), &replica.state().unwrap());
}

#[test]
fn held_items_and_sets_stay_as_they_were_shown_while_the_reader_follows() {
    let mut rng = Lcg(31);
    let mut replica = Replica::<Board>::new();
    for step in 0..40 {
        random_board_action(&mut replica, &mut rng, step);
    }
    let mut reader = Reader::<Board>::open(replica.doc_mut()).unwrap();
    let mut held: Vec<(Rc<Board>, Board)> = Vec::new();
    for step in 40..340 {
        held.push((Rc::clone(reader.state()), replica.state().unwrap()));
        random_board_action(&mut replica, &mut rng, step);
        reader.sync(replica.doc()).unwrap();
        assert_eq!(
            reader.state().as_ref(),
            &replica.state().unwrap(),
            "after step {step}"
        );
    }
    for (at, (shown, read)) in held.iter().enumerate() {
        assert_eq!(shown.as_ref(), read, "state shown before step {}", at + 40);
    }
}

/// A cue at a place on a track: keyed by id, held in memory by place.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Cue {
    pub id: String,
    pub at: u32,
    pub body: String,
}

impl Identified for Cue {
    /// Counts how often an item is asked: an id may be costly to make.
    fn id(&self) -> Id {
        IDS.with(|ids| ids.set(ids.get() + 1));
        Id::from_raw(self.id.clone())
    }
    fn order(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at)
    }
}

fn ids_asked() -> usize {
    IDS.with(Cell::get)
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Track {
    pub cues: Keyed<Cue>,
}

#[resource(kind = "test.track", label = "Track")]
impl Track {
    /// Add a cue at a place.
    #[action]
    pub fn add_cue(&mut self, cx: &mut ActionCx, at: u32) {
        self.cues.push(Cue {
            id: cx.id().as_str().to_owned(),
            at,
            body: String::new(),
        });
    }
}

fn cue_ids(track: &Track) -> Vec<Id> {
    track
        .cues
        .iter()
        .map(|cue| Id::from_raw(cue.id.clone()))
        .collect()
}

/// Add, move, rewrite or remove a cue; places collide often, so cues at one
/// place sort by id.
fn random_cue_action(replica: &mut Replica<Track>, rng: &mut Lcg) {
    let ids = cue_ids(&replica.state().unwrap());
    let cues = Track::paths().cues;
    let place = u32::try_from(rng.next(12)).unwrap();
    let Some(id) = ids.get(rng.next(ids.len())).cloned() else {
        replica.apply(Track::ADD_CUE.with(place)).unwrap();
        return;
    };
    let edit = match rng.next(7) {
        0 | 1 => {
            replica.apply(Track::ADD_CUE.with(place)).unwrap();
            return;
        }
        2 | 3 => cues.item(&id).at.set(place),
        4 | 5 => cues.item(&id).body.replace(format!("cue {place}")),
        _ => cues.remove(&id),
    };
    replica
        .apply_confirmed(tilo_resource::Invoke::edit(&edit).unwrap())
        .unwrap();
}

#[test]
fn keyed_items_a_reader_follows_sort_as_a_full_read_does() {
    let mut rng = Lcg(37);
    let mut replica = Replica::<Track>::new();
    for _ in 0..40 {
        random_cue_action(&mut replica, &mut rng);
    }
    let mut reader = Reader::<Track>::open(replica.doc_mut()).unwrap();
    let mut held: Vec<(Rc<Track>, Track)> = Vec::new();
    for step in 40..400 {
        held.push((Rc::clone(reader.state()), replica.state().unwrap()));
        random_cue_action(&mut replica, &mut rng);
        reader.sync(replica.doc()).unwrap();
        assert_eq!(
            reader.state().as_ref(),
            &replica.state().unwrap(),
            "after step {step}"
        );
    }
    for (at, (shown, read)) in held.iter().enumerate() {
        assert_eq!(shown.as_ref(), read, "state shown before step {}", at + 40);
    }
}

#[test]
fn a_one_item_change_asks_only_the_items_it_touched_for_their_ids() {
    let mut replica = Replica::<Track>::new();
    for n in 0..600 {
        replica.apply(Track::ADD_CUE.with(n * 10)).unwrap();
    }
    let mut reader = Reader::<Track>::open(replica.doc_mut()).unwrap();
    let ids = cue_ids(reader.state());
    let cues = Track::paths().cues;
    // The first change after opening learns every id.
    replica
        .apply(tilo_resource::Invoke::edit(&cues.item(&ids[0]).body.replace("first")).unwrap())
        .unwrap();
    reader.sync(replica.doc()).unwrap();

    let changes = [
        (
            "rewrite one in place",
            cues.item(&ids[300]).body.replace("changed"),
        ),
        (
            "move one past 200 others",
            cues.item(&ids[100]).at.set(3_005),
        ),
        ("move one to the front", cues.item(&ids[599]).at.set(0)),
    ];
    for (change, edit) in changes {
        replica
            .apply(tilo_resource::Invoke::edit(&edit).unwrap())
            .unwrap();
        let shown = Rc::clone(reader.state());
        let before = ids_asked();
        assert!(reader.sync(replica.doc()).unwrap());
        let asked = ids_asked() - before;
        assert!(
            asked <= 1,
            "{change}: {asked} items of 600 asked for their ids, not the one it touched"
        );
        assert_ne!(shown.as_ref(), reader.state().as_ref());
        assert_eq!(
            reader.state().as_ref(),
            &replica.state().unwrap(),
            "{change}"
        );
    }

    replica.apply(Track::ADD_CUE.with(1_234)).unwrap();
    let before = ids_asked();
    reader.sync(replica.doc()).unwrap();
    let asked = ids_asked() - before;
    assert!(asked <= 1, "adding one: {asked} items asked for their ids");
    assert_eq!(reader.state().as_ref(), &replica.state().unwrap());

    replica
        .apply_confirmed(tilo_resource::Invoke::edit(&cues.remove(&ids[42])).unwrap())
        .unwrap();
    let before = ids_asked();
    reader.sync(replica.doc()).unwrap();
    let asked = ids_asked() - before;
    assert_eq!(asked, 0, "removing one: {asked} items asked for their ids");
    assert_eq!(reader.state().as_ref(), &replica.state().unwrap());
}

#[test]
fn the_edits_between_two_keyed_values_ask_each_item_once() {
    let mut replica = Replica::<Track>::new();
    for n in 0..300 {
        replica.apply(Track::ADD_CUE.with(n)).unwrap();
    }
    let before = replica.state().unwrap();
    let mut after = before.clone();
    let id = Id::from_raw(after.cues[150].id.clone());
    after.cues.find_mut(&id).unwrap().body = "changed".into();
    let asked = ids_asked();
    let edits = Track::paths().path().edits(&before, &after);
    let asked = ids_asked() - asked;
    assert_eq!(edits.len(), 1, "one field of one cue changed: {edits:?}");
    assert!(
        asked <= 2 * 300,
        "{asked} ids asked to compare 300 cues with 300: each at most once per side"
    );
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub title: String,
    pub progress: Map<Counted>,
}

#[resource(kind = "test.shelf", label = "Shelf")]
impl Shelf {
    /// Rename.
    #[action]
    pub fn retitle(&mut self, title: String) {
        self.title = title;
    }

    /// Record where a book is read to.
    #[action]
    pub fn mark(&mut self, book: String, at: String) {
        match self.progress.get_mut(&book) {
            Some(held) => held.0 = at,
            None => {
                self.progress.insert(book, Counted(at));
            }
        }
    }

    /// Forget a book.
    #[action]
    pub fn forget(&mut self, book: String) {
        self.progress.remove(&book);
    }
}

fn book(n: usize) -> String {
    format!("book {n:04}")
}

fn random_shelf_action(replica: &mut Replica<Shelf>, rng: &mut Lcg, step: usize) {
    let result = match rng.next(8) {
        0 => replica.apply(Shelf::RETITLE.with(format!("shelf {step}"))),
        1 | 2 => replica.apply(Shelf::FORGET.with(book(rng.next(150)))),
        _ => replica.apply(Shelf::MARK.with((book(rng.next(150)), format!("at {step}")))),
    };
    result.unwrap();
}

#[test]
fn a_reader_whose_state_a_view_holds_copies_one_chunk_of_a_map() {
    let mut replica = Replica::<Shelf>::new();
    for n in 0..1000 {
        replica
            .apply(Shelf::MARK.with((book(n), format!("page {n}"))))
            .unwrap();
    }
    let mut reader = Reader::<Shelf>::open(replica.doc_mut()).unwrap();
    // A view keeps the state it was shown, as a surface's `present` may.
    let shown = Rc::clone(reader.state());

    // A change to another field copies no entry of the map.
    replica
        .apply(Shelf::RETITLE.with("renamed".to_owned()))
        .unwrap();
    let before = clones();
    assert!(reader.sync(replica.doc()).unwrap());
    assert_eq!(clones() - before, 0, "no entry copied for a rename");
    let renamed = Rc::clone(reader.state());
    assert_eq!(shown.title, "");
    assert_eq!(renamed.title, "renamed");
    for n in 0..1000 {
        assert!(
            std::ptr::eq(
                renamed.progress.get(&book(n)).unwrap(),
                shown.progress.get(&book(n)).unwrap()
            ),
            "entry {n} shared"
        );
    }

    // A change to one entry copies its chunk, not the map.
    replica
        .apply(Shelf::MARK.with((book(500), "changed".into())))
        .unwrap();
    let before = clones();
    assert!(reader.sync(replica.doc()).unwrap());
    let copied = clones() - before;
    assert!(
        copied <= 64,
        "{copied} entries copied: one chunk at most, not the map"
    );
    let now = reader.state();
    assert!(!Rc::ptr_eq(now, &renamed), "the held state was not changed");
    assert_eq!(renamed.progress.get(&book(500)).unwrap().0, "page 500");
    assert_eq!(now.progress.get(&book(500)).unwrap().0, "changed");
    assert_eq!(now.as_ref(), &replica.state().unwrap());
    let shared = (0..1000)
        .filter(|n| {
            std::ptr::eq(
                now.progress.get(&book(*n)).unwrap(),
                renamed.progress.get(&book(*n)).unwrap(),
            )
        })
        .count();
    assert!(shared >= 1000 - 64, "{shared} of 1000 entries still shared");
}

#[test]
fn a_held_map_stays_as_it_was_shown_while_the_reader_follows() {
    let mut rng = Lcg(37);
    let mut replica = Replica::<Shelf>::new();
    for step in 0..120 {
        random_shelf_action(&mut replica, &mut rng, step);
    }
    let mut reader = Reader::<Shelf>::open(replica.doc_mut()).unwrap();
    let mut held: Vec<(Rc<Shelf>, Shelf)> = Vec::new();
    for step in 120..520 {
        held.push((Rc::clone(reader.state()), replica.state().unwrap()));
        random_shelf_action(&mut replica, &mut rng, step);
        reader.sync(replica.doc()).unwrap();
        assert_eq!(
            reader.state().as_ref(),
            &replica.state().unwrap(),
            "after step {step}"
        );
    }
    for (at, (shown, read)) in held.iter().enumerate() {
        assert_eq!(shown.as_ref(), read, "state shown before step {}", at + 120);
    }
}

#[test]
fn a_map_is_stored_as_a_btree_map_is() {
    use tilo_resource::__private::ROOT;
    let entries: BTreeMap<String, String> =
        (0..100).map(|n| (book(n), format!("page {n}"))).collect();
    let map = Map::from(entries.clone());

    let mut doc = Doc::new();
    tilo_resource::write_key(&mut doc, &ROOT, "progress", &map).unwrap();
    let read: BTreeMap<String, String> = tilo_resource::read_key(&doc, &ROOT, "progress").unwrap();
    assert_eq!(read, entries);

    let mut doc = Doc::new();
    tilo_resource::write_key(&mut doc, &ROOT, "progress", &entries).unwrap();
    let read: Map<String> = tilo_resource::read_key(&doc, &ROOT, "progress").unwrap();
    assert_eq!(read, map);
    assert_eq!(map.to_json(), entries.to_json());
    assert_eq!(
        <Map<String> as Model>::SHAPE,
        <BTreeMap<String, String> as Model>::SHAPE
    );
}

thread_local! {
    static COMPARED: Cell<usize> = const { Cell::new(0) };
    static SERIALISED: Cell<usize> = const { Cell::new(0) };
}

/// Text that counts how often it is compared and turned into JSON.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compared(pub String);

impl tilo_resource::Settable for Compared {}

impl Model for Compared {
    tilo_resource::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        SERIALISED.with(|serialised| serialised.set(serialised.get() + 1));
        self.0.to_json()
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        String::from_json(value).map(Self)
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        self.0.store(doc, slot)
    }
    fn stores_as(&self, before: &Self) -> bool {
        COMPARED.with(|compared| compared.set(compared.get() + 1));
        self == before
    }
}

/// What `run` returned, and how often a [`Compared`] was compared and
/// turned into JSON while it ran.
fn counted<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    let compared = || COMPARED.with(Cell::get);
    let serialised = || SERIALISED.with(Cell::get);
    let (was_compared, was_serialised) = (compared(), serialised());
    let out = run();
    (
        out,
        compared() - was_compared,
        serialised() - was_serialised,
    )
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shot {
    pub id: String,
    pub opacity: f64,
    pub label: Compared,
}

impl Identified for Shot {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Cut {
    pub title: String,
    pub shots: Keyed<Shot>,
    pub stack: Ordered<Shot>,
}

fn cut(shots: usize) -> Cut {
    let shots: Vec<Shot> = (0..shots)
        .map(|at| Shot {
            id: format!("s{at:04}"),
            opacity: 1.0,
            label: Compared(format!("shot {at}")),
        })
        .collect();
    Cut {
        title: "cut".into(),
        shots: shots.clone().into(),
        stack: shots.into(),
    }
}

fn edits_of(before: &Cut, after: &Cut) -> Vec<tilo_resource::Edit<Cut>> {
    tilo_resource::Path::<Cut, Cut>::root().edits(before, after)
}

/// The same items, held apart from every other copy: compared item by item.
fn apart(cut: &Cut) -> Cut {
    Cut {
        title: cut.title.clone(),
        shots: cut.shots.to_vec().into(),
        stack: cut.stack.to_vec().into(),
    }
}

#[test]
fn a_one_item_edit_of_a_local_copy_compares_only_that_item() {
    let before = cut(300);
    let mut after = before.clone();
    let id = Id::from_raw("s0150");
    after.shots.find_mut(&id).unwrap().opacity = 0.5;
    after.stack.find_mut(&id).unwrap().label = Compared("changed".into());
    let (edits, compared, serialised) = counted(|| edits_of(&before, &after));
    assert!(
        compared <= 4,
        "{compared} labels of 600 compared for a change to one item of each list"
    );
    assert!(
        serialised <= 2,
        "{serialised} labels turned into JSON: only the changed one may be"
    );
    let paths = Cut::paths();
    assert_eq!(
        edits,
        [
            paths.shots.item(&id).opacity.set(0.5),
            paths.stack.item(&id).label.set(Compared("changed".into())),
        ],
    );
    assert_eq!(edits, edits_of(&apart(&before), &apart(&after)));

    // A copy nothing changed shares its items: nothing is compared.
    let (edits, compared, serialised) = counted(|| edits_of(&before, &before.clone()));
    assert!(edits.is_empty());
    assert_eq!((compared, serialised), (0, 0));
}

#[test]
fn items_changed_in_place_are_compared_without_serialising_the_others() {
    let before = cut(300);
    let mut after = before.clone();
    for shot in &mut after.shots {
        if shot.id == "s0150" {
            shot.opacity = 0.5;
        }
    }
    let (edits, compared, serialised) = counted(|| edits_of(&before, &after));
    assert_eq!(
        edits,
        [Cut::paths()
            .shots
            .item(&Id::from_raw("s0150"))
            .opacity
            .set(0.5)]
    );
    assert!(compared <= 300, "{compared} labels compared for 300 shots");
    assert_eq!(serialised, 0, "unchanged labels are compared in memory");
}

#[test]
fn the_edits_of_a_local_copy_match_comparing_every_item() {
    for seed in 0..60 {
        let mut rng = Lcg(seed);
        // Made here, not cloned: once a copy of it changes, `before` is the
        // only holder of its items, and changing it changes them in place.
        let mut before = cut(40);
        if seed % 2 == 0 {
            // Sorted, so its ids are known and kept beside the items.
            before.shots.settle();
        }
        let mut after = before.clone();
        for step in 0..rng.next(8) {
            let id = Id::from_raw(format!("s{:04}", rng.next(40)));
            match rng.next(7) {
                0 | 1 => {
                    if let Some(shot) = after.shots.find_mut(&id) {
                        shot.opacity = f64::from(u32::try_from(step).unwrap());
                    }
                }
                2 => {
                    if let Some(shot) = after.shots.find_mut(&id) {
                        shot.label = Compared(format!("step {step}"));
                    }
                }
                3 => {
                    // An id changed in place: the item left, another came.
                    if let Some(shot) = after.shots.find_mut(&id) {
                        shot.id = format!("s{:04}", rng.next(60));
                    }
                }
                4 => {
                    if let Some(shot) = before.shots.find_mut(&id) {
                        shot.label = Compared(format!("before {step}"));
                    }
                }
                5 => {
                    if let Some(shot) = after.stack.find_mut(&id) {
                        shot.opacity = 0.25;
                    }
                }
                _ => {
                    // A copy held across a change: the change copies again.
                    let held = after.clone();
                    if let Some(shot) = after.shots.find_mut(&id) {
                        shot.opacity = 0.75;
                    }
                    drop(held);
                }
            }
        }
        assert_eq!(
            edits_of(&before, &after),
            edits_of(&apart(&before), &apart(&after)),
            "seed {seed}"
        );
        for shot in &after.shots {
            let first = after.shots.iter().position(|other| other.id == shot.id);
            assert_eq!(after.shots.position_of(&shot.id()), first, "seed {seed}");
        }
    }
}

thread_local! {
    static PAGES_COMPARED: Cell<usize> = const { Cell::new(0) };
    static PAGES_STORED: Cell<usize> = const { Cell::new(0) };
}

/// Text that counts how often it is compared and written.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page(pub String);

impl tilo_resource::Settable for Page {}

impl Model for Page {
    tilo_resource::leaf_paths!();
    fn to_json(&self) -> serde_json::Value {
        self.0.to_json()
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        String::from_json(value).map(Self)
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        String::load(doc, value, id).map(Self)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        PAGES_STORED.with(|stored| stored.set(stored.get() + 1));
        self.0.store(doc, slot)
    }
    fn stores_as(&self, before: &Self) -> bool {
        PAGES_COMPARED.with(|compared| compared.set(compared.get() + 1));
        self == before
    }
}

/// What `run` returned, and how many [`Page`]s it compared and wrote.
fn pages_counted<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    let compared = || PAGES_COMPARED.with(Cell::get);
    let stored = || PAGES_STORED.with(Cell::get);
    let (was_compared, was_stored) = (compared(), stored());
    let out = run();
    (out, compared() - was_compared, stored() - was_stored)
}

fn actor(name: &str) -> automerge::ActorId {
    automerge::ActorId::from(name.as_bytes())
}

/// Close the open change at a fixed time, so two documents that made the
/// same operations end at the same heads.
fn commit_at_zero(doc: &mut Doc) {
    doc.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
}

#[test]
fn saving_a_map_changed_in_one_entry_visits_one_chunk() {
    use tilo_resource::__private::ROOT;
    let held: Map<Page> = (0..1000)
        .map(|n| (book(n), Page(format!("page {n}"))))
        .collect();
    let mut doc = Doc::new().with_actor(actor("base"));
    tilo_resource::write_key(&mut doc, &ROOT, "progress", &held).unwrap();
    commit_at_zero(&mut doc);

    let mut now = held.clone();
    now.insert(book(500), Page("changed".into()));
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut changed,
            &ROOT,
            "progress",
            &now,
            &held,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!(stored, 1, "only the changed entry is written");
    assert!(
        compared <= 2 * 64,
        "{compared} entries of 1000 compared: the changed chunk's, at most twice"
    );

    let mut whole = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key(&mut whole, &ROOT, "progress", &now).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");
    let read: Map<Page> = tilo_resource::read_key(&changed, &ROOT, "progress").unwrap();
    assert_eq!(read, now);

    // An unchanged copy writes nothing and compares nothing.
    let mut same = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut same,
            &ROOT,
            "progress",
            &held.clone(),
            &held,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!((compared, stored), (0, 0));
    assert_eq!(automerge::transaction::Transactable::pending_ops(&same), 0);

    // The edits between the two compare that chunk too, and are the edits
    // between the same entries in a BTreeMap.
    let root = tilo_resource::Path::<Map<Page>, Map<Page>>::root();
    let (edits, compared, _) = pages_counted(|| root.edits(&held, &now));
    assert!(compared <= 64, "{compared} entries compared for the edits");
    assert_eq!(edit_parts(&edits), btree_edits(&held, &now));
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Folio {
    pub id: String,
    pub page: Page,
}

impl Identified for Folio {
    fn id(&self) -> Id {
        Id::from_raw(self.id.clone())
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Volume {
    pub title: String,
    pub folios: Keyed<Folio>,
    pub stack: Ordered<Folio>,
}

#[test]
fn saving_a_record_whose_items_no_change_touched_visits_none() {
    use tilo_resource::__private::ROOT;
    use tilo_resource::Fields;
    let folios: Vec<Folio> = (0..300)
        .map(|n| Folio {
            id: format!("f{n:04}"),
            page: Page(format!("page {n}")),
        })
        .collect();
    let before = Volume {
        title: "volume".into(),
        folios: folios.clone().into(),
        stack: folios.into(),
    };
    let mut doc = Doc::new().with_actor(actor("base"));
    before.write_fields(&mut doc, &ROOT).unwrap();
    commit_at_zero(&mut doc);

    let mut after = before.clone();
    after.title = "renamed".into();
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        after
            .write_changed_fields(
                &before,
                &mut changed,
                &ROOT,
                &tilo_resource::Delta::nothing(),
            )
            .unwrap();
    });
    assert_eq!((compared, stored), (0, 0), "no item visited for a rename");

    // One item changed: the lists are written as a whole write writes them.
    after.folios.find_mut(&Id::from_raw("f0150")).unwrap().page = Page("changed".into());
    let mut changed = doc.fork().with_actor(actor("me"));
    let mut whole = doc.fork().with_actor(actor("me"));
    after
        .write_changed_fields(
            &before,
            &mut changed,
            &ROOT,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    after.write_fields(&mut whole, &ROOT).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Mark {
    pub at: String,
    pub page: i64,
}

fn mark_key(n: usize) -> String {
    format!("k{n:04}")
}

type EditParts = Vec<(Vec<tilo_resource::Segment>, tilo_resource::EditOp)>;

fn edit_parts<R>(edits: &[tilo_resource::Edit<R>]) -> EditParts {
    edits
        .iter()
        .map(|edit| (edit.path().to_vec(), edit.op().clone()))
        .collect()
}

/// The edits between two maps compared entry by entry, as a `BTreeMap`
/// of the same entries is.
fn btree_edits<T: Model + Clone>(before: &Map<T>, after: &Map<T>) -> EditParts {
    let before: BTreeMap<String, T> = before.into();
    let after: BTreeMap<String, T> = after.into();
    edit_parts(
        &tilo_resource::Path::<BTreeMap<String, T>, BTreeMap<String, T>>::root()
            .edits(&before, &after),
    )
}

/// Change `map` at random: entries changed, added (some in runs that split
/// a chunk), removed (some in runs that empty one), now and then all of it
/// cleared or replaced by an equal map that shares nothing.
fn change_marks(map: &mut Map<Mark>, rng: &mut Lcg, step: usize) {
    let page = i64::try_from(step).unwrap();
    for _ in 0..rng.next(6) {
        let n = rng.next(400);
        match rng.next(12) {
            0..=3 => {
                if let Some(mark) = map.get_mut(&mark_key(n)) {
                    mark.page = page;
                }
            }
            4 | 5 => {
                map.insert(
                    mark_key(n),
                    Mark {
                        at: format!("at {step}"),
                        page,
                    },
                );
            }
            6 => {
                // A run of new keys inside one chunk: it splits.
                for k in 0..rng.next(80) {
                    map.insert(format!("{}-{k:02}", mark_key(n)), Mark::default());
                }
            }
            7 | 8 => {
                map.remove(&mark_key(n));
            }
            9 => {
                // A run of keys gone: a chunk may empty.
                for k in n..n + rng.next(70) {
                    map.remove(&mark_key(k));
                }
            }
            10 => {
                if rng.next(4) == 0 {
                    map.clear();
                }
            }
            _ => {
                let apart: BTreeMap<String, Mark> = (&*map).into();
                *map = Map::from(apart);
            }
        }
    }
}

/// What a collaborator writes into the map meanwhile: entries put,
/// removed, or changed inside.
fn peer_marks(doc: &mut Doc, rng: &mut Lcg, step: usize) {
    use tilo_resource::__private::{ObjType, ROOT, ReadDoc};
    let mut peer = doc.fork().with_actor(actor("peer"));
    let progress = match peer.get(ROOT, "progress").unwrap() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
    .unwrap();
    let keys: Vec<String> = peer.keys(&progress).collect();
    for _ in 0..=rng.next(4) {
        let key = keys
            .get(rng.next(keys.len() + 1))
            .cloned()
            .unwrap_or_else(|| mark_key(rng.next(400)));
        match rng.next(3) {
            0 => {
                let mark = Mark {
                    at: format!("peer {step}"),
                    page: -1,
                };
                tilo_resource::write_key(&mut peer, &progress, &key, &mark).unwrap();
            }
            1 => {
                if peer.get(&progress, key.as_str()).unwrap().is_some() {
                    automerge::transaction::Transactable::delete(
                        &mut peer,
                        &progress,
                        key.as_str(),
                    )
                    .unwrap();
                }
            }
            _ => {
                if let Some((Value::Object(ObjType::Map), entry)) =
                    peer.get(&progress, key.as_str()).unwrap()
                {
                    automerge::transaction::Transactable::put(&mut peer, &entry, "page", 99_i64)
                        .unwrap();
                }
            }
        }
    }
    peer.commit();
    doc.merge_incoming(&mut peer).unwrap();
}

#[test]
fn a_map_saved_over_its_copy_writes_what_a_whole_write_does() {
    use tilo_resource::__private::{ObjType, ROOT, ReadDoc};
    let mut saves = 0;
    for seed in 0..40 {
        let mut rng = Lcg(seed);
        let mut held: Map<Mark> = (0..rng.next(400))
            .map(|n| {
                (
                    mark_key(n),
                    Mark {
                        at: format!("at {n}"),
                        page: 0,
                    },
                )
            })
            .collect();
        let mut doc = Doc::new().with_actor(actor("base"));
        tilo_resource::write_key(&mut doc, &ROOT, "progress", &held).unwrap();
        commit_at_zero(&mut doc);
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(&mut doc);
        for step in 0..8 {
            if rng.next(3) == 0 {
                peer_marks(&mut doc, &mut rng, step);
            }
            let dirty = tracker.delta(&mut doc);
            // What the slot holds now, brought up to date as a reader does:
            // the entries no one touched still shared with `held`.
            let mut was = held.clone();
            let (value, id) = match doc.get(ROOT, "progress").unwrap() {
                Some((value @ Value::Object(ObjType::Map), id)) => Some((value, id)),
                _ => None,
            }
            .unwrap();
            was.refresh(&doc, &value, &id, &dirty).unwrap();
            let read: Map<Mark> = tilo_resource::read_key(&doc, &ROOT, "progress").unwrap();
            assert_eq!(was, read, "seed {seed} step {step}: refreshed as read");

            let mut now = was.clone();
            change_marks(&mut now, &mut rng, step);
            assert_eq!(
                edit_parts(&tilo_resource::Path::<Map<Mark>, Map<Mark>>::root().edits(&was, &now)),
                btree_edits(&was, &now),
                "seed {seed} step {step}: the edits"
            );

            let mut changed = doc.fork().with_actor(actor("me"));
            let mut whole = doc.fork().with_actor(actor("me"));
            tilo_resource::write_key_changed(&mut changed, &ROOT, "progress", &now, &was, &dirty)
                .unwrap();
            tilo_resource::write_key(&mut whole, &ROOT, "progress", &now).unwrap();
            commit_at_zero(&mut changed);
            commit_at_zero(&mut whole);
            assert_eq!(
                changed.get_heads(),
                whole.get_heads(),
                "seed {seed} step {step}: the saved change"
            );
            saves += 1;
            doc = changed;
            tracker.start(&mut doc);
            held = now;
        }
    }
    assert_eq!(saves, 320);
}

// --- Items added to a local copy; lists, sets and grids saved over theirs ----

fn shot(id: &str, label: &str) -> Shot {
    Shot {
        id: id.to_owned(),
        opacity: 1.0,
        label: Compared(label.to_owned()),
    }
}

#[test]
fn an_item_pushed_onto_a_local_copy_is_compared_with_nothing() {
    let before = cut(300);
    let mut after = before.clone();
    let id = Id::from_raw("s0150");
    after.shots.find_mut(&id).unwrap().opacity = 0.5;
    after.shots.push(shot("s9000", "new"));
    after.stack.push(shot("s9001", "newer"));
    // Changed after it came: still one new item.
    after
        .stack
        .find_mut(&Id::from_raw("s9001"))
        .unwrap()
        .opacity = 0.25;
    let (edits, compared, _) = counted(|| edits_of(&before, &after));
    assert!(
        compared <= 2,
        "{compared} labels of 600 compared for one changed and two new items"
    );
    let paths = Cut::paths();
    assert_eq!(
        edits,
        [
            paths.shots.item(&id).opacity.set(0.5),
            paths.shots.insert(&shot("s9000", "new")),
            paths.stack.insert(
                300,
                &Shot {
                    opacity: 0.25,
                    ..shot("s9001", "newer")
                }
            ),
        ],
    );
    assert_eq!(edits, edits_of(&apart(&before), &apart(&after)));
}

/// Change `cut`'s lists at random, items added among the changes: new ids,
/// ids already held, items changed after they came, and changes a copy
/// cannot follow item by item.
fn add_and_change(cut: &mut Cut, rng: &mut Lcg, step: usize) {
    let id = Id::from_raw(format!("s{:04}", rng.next(48)));
    let label = format!("step {step}");
    match rng.next(10) {
        // An id that may be held already: then items left and came.
        0 | 1 => cut.shots.push(shot(id.as_str(), &label)),
        2 => cut.stack.push(shot(id.as_str(), &label)),
        3 => cut
            .shots
            .push(shot(&format!("n{step:04}-{}", rng.next(1000)), &label)),
        4 => {
            if let Some(shot) = cut.shots.find_mut(&id) {
                shot.opacity = f64::from(u32::try_from(step).unwrap());
            }
        }
        5 => {
            if let Some(shot) = cut.stack.find_mut(&id) {
                shot.label = Compared(label);
            }
        }
        6 => {
            // An id changed in place, perhaps of an item just added.
            if let Some(shot) = cut.shots.find_mut(&id) {
                shot.id = format!("s{:04}", rng.next(60));
            }
        }
        7 => cut.shots.retain(|shot| shot.id != id.as_str()),
        8 => {
            cut.stack.take(&id);
        }
        _ => {
            // A copy held across an addition: the addition copies again.
            let held = cut.clone();
            cut.shots
                .push(shot(&format!("h{step:04}-{}", rng.next(1000)), &label));
            drop(held);
        }
    }
}

#[test]
fn the_edits_of_a_local_copy_items_were_added_to_match_comparing_every_item() {
    for seed in 0..60 {
        let mut rng = Lcg(seed);
        let mut before = cut(40);
        if seed % 2 == 0 {
            before.shots.settle();
        }
        let mut after = before.clone();
        for step in 0..rng.next(10) {
            add_and_change(&mut after, &mut rng, step);
        }
        assert_eq!(
            edits_of(&before, &after),
            edits_of(&apart(&before), &apart(&after)),
            "seed {seed}"
        );
        for shot in &after.shots {
            let first = after.shots.iter().position(|other| other.id == shot.id);
            assert_eq!(after.shots.position_of(&shot.id()), first, "seed {seed}");
        }
    }
}

#[test]
fn items_removed_from_a_local_copy_are_compared_with_nothing() {
    let before = cut(300);
    let mut after = before.clone();
    let changed = Id::from_raw("s0150");
    after.shots.find_mut(&changed).unwrap().opacity = 0.5;
    after
        .shots
        .retain(|shot| shot.id != "s0100" && shot.id != "s0200");
    assert_eq!(
        after.shots.take(&Id::from_raw("s0250")).unwrap().id,
        "s0250"
    );
    assert_eq!(after.stack.remove(10).id, "s0010");
    after.stack.push(shot("s9001", "newer"));
    let (edits, compared, _) = counted(|| edits_of(&before, &after));
    assert!(
        compared <= 2,
        "{compared} labels of 600 compared for four removed, one changed and one new item"
    );
    let paths = Cut::paths();
    assert_eq!(
        edits,
        [
            paths.shots.remove(&Id::from_raw("s0100")),
            paths.shots.item(&changed).opacity.set(0.5),
            paths.shots.remove(&Id::from_raw("s0200")),
            paths.shots.remove(&Id::from_raw("s0250")),
            paths.stack.remove(&Id::from_raw("s0010")),
            paths.stack.insert(299, &shot("s9001", "newer")),
        ],
    );
    assert_eq!(edits, edits_of(&apart(&before), &apart(&after)));

    // Keeping every item changes nothing and copies nothing.
    let mut kept = before.clone();
    kept.shots.retain(|_| true);
    assert!(kept.shots.stores_as(&before.shots));
}

/// Change `cut`'s lists at random, items removed among the changes: one at
/// a time by id or position, several at once, items added under the id of
/// one removed, items changed after others left.
fn remove_and_change(cut: &mut Cut, rng: &mut Lcg, step: usize) {
    let id = Id::from_raw(format!("s{:04}", rng.next(48)));
    match rng.next(10) {
        0 => {
            cut.shots.take(&id);
        }
        1 => {
            if !cut.stack.is_empty() {
                let at = rng.next(cut.stack.len());
                cut.stack.remove(at);
            }
        }
        2 => {
            if !cut.shots.is_empty() {
                let at = rng.next(cut.shots.len());
                cut.shots.remove(at);
            }
        }
        3 => {
            let (every, offset) = (2 + rng.next(5), rng.next(5));
            cut.shots.retain(|shot| {
                let n: usize = shot.id.get(1..).and_then(|n| n.parse().ok()).unwrap_or(0);
                n % every != offset
            });
        }
        4 => {
            let (every, offset) = (2 + rng.next(5), rng.next(5));
            cut.stack.retain(|shot| {
                let n: usize = shot.id.get(1..).and_then(|n| n.parse().ok()).unwrap_or(1);
                n % every != offset
            });
        }
        5 => {
            // Out, then in again under the same id.
            let label = format!("again {step}");
            if cut.stack.take(&id).is_some() {
                cut.stack.push(shot(id.as_str(), &label));
            }
        }
        6 => {
            // Changed by position, perhaps after others left.
            if !cut.stack.is_empty() {
                let at = rng.next(cut.stack.len());
                cut.stack.at_mut(at).unwrap().label = Compared(format!("at {step}"));
            }
        }
        _ => add_and_change(cut, rng, step),
    }
}

#[test]
fn the_edits_of_a_local_copy_items_were_removed_from_match_comparing_every_item() {
    for seed in 0..120 {
        let mut rng = Lcg(seed);
        let mut before = cut(40);
        if seed % 2 == 0 {
            before.shots.settle();
        }
        let mut after = before.clone();
        for step in 0..rng.next(12) {
            remove_and_change(&mut after, &mut rng, step);
        }
        assert_eq!(
            edits_of(&before, &after),
            edits_of(&apart(&before), &apart(&after)),
            "seed {seed}"
        );
        for shot in &after.shots {
            let first = after.shots.iter().position(|other| other.id == shot.id);
            assert_eq!(after.shots.position_of(&shot.id()), first, "seed {seed}");
        }
    }
}

#[test]
fn saving_an_ordered_list_changed_removed_from_and_added_to_visits_only_those_items() {
    use tilo_resource::__private::ROOT;
    let held: Ordered<Folio> = (0..300)
        .map(|n| Folio {
            id: format!("f{n:04}"),
            page: Page(format!("page {n}")),
        })
        .collect();
    let mut doc = Doc::new().with_actor(actor("base"));
    tilo_resource::write_key(&mut doc, &ROOT, "stack", &held).unwrap();
    commit_at_zero(&mut doc);
    // As read: its order keys are known to rise.
    let before: Ordered<Folio> = tilo_resource::read_key(&doc, &ROOT, "stack").unwrap();

    let mut after = before.clone();
    after.find_mut(&Id::from_raw("f0150")).unwrap().page = Page("changed".into());
    after.take(&Id::from_raw("f0100")).unwrap();
    after.retain(|folio| folio.id != "f0299");
    after.push(Folio {
        id: "f9000".into(),
        page: Page("new".into()),
    });
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut changed,
            &ROOT,
            "stack",
            &after,
            &before,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!(stored, 2, "the changed item and the new one are written");
    assert!(compared <= 4, "{compared} items of 300 compared");
    let mut whole = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key(&mut whole, &ROOT, "stack", &after).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");
    let read: Ordered<Folio> = tilo_resource::read_key(&changed, &ROOT, "stack").unwrap();
    assert_eq!(read, after, "read back in the order saved");
}

#[test]
fn saving_a_keyed_list_removed_from_visits_only_those_items() {
    use tilo_resource::__private::ROOT;
    let held: Keyed<Folio> = (0..300)
        .map(|n| Folio {
            id: format!("f{n:04}"),
            page: Page(format!("page {n}")),
        })
        .collect();
    let mut doc = Doc::new().with_actor(actor("base"));
    tilo_resource::write_key(&mut doc, &ROOT, "folios", &held).unwrap();
    commit_at_zero(&mut doc);
    let before: Keyed<Folio> = tilo_resource::read_key(&doc, &ROOT, "folios").unwrap();

    let mut after = before.clone();
    after.retain(|folio| folio.id != "f0100" && folio.id != "f0200");
    after.find_mut(&Id::from_raw("f0150")).unwrap().page = Page("changed".into());
    after.remove(0);
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut changed,
            &ROOT,
            "folios",
            &after,
            &before,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!(stored, 1, "only the changed item is written");
    assert!(compared <= 2, "{compared} items of 300 compared");
    let mut whole = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key(&mut whole, &ROOT, "folios", &after).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");
    let read: Keyed<Folio> = tilo_resource::read_key(&changed, &ROOT, "folios").unwrap();
    assert_eq!(read.len(), 297, "three items left");
}

/// Write the entry `{ k: key, v: item }` under `id` into the Ordered map
/// `map`, as a collaborator's store would.
fn put_ordered_entry(doc: &mut Doc, map: &ObjId, id: &str, key: &str, item: &Shot) {
    use tilo_resource::__private::ObjType;
    let entry =
        automerge::transaction::Transactable::put_object(doc, map, id, ObjType::Map).unwrap();
    automerge::transaction::Transactable::put(doc, &entry, "k", key).unwrap();
    tilo_resource::write_key(doc, &entry, "v", item).unwrap();
}

/// The order key of the Ordered entry `id` of `map`.
fn order_key(doc: &Doc, map: &ObjId, id: &str) -> Option<String> {
    use tilo_resource::__private::{ObjType, ReadDoc};
    match doc.get(map, id).unwrap() {
        Some((Value::Object(ObjType::Map), entry)) => {
            tilo_resource::read_key(doc, &entry, "k").ok()
        }
        _ => None,
    }
}

/// What a collaborator writes into the Ordered list `stack` meanwhile: an
/// item inserted under an order key another item holds (as two people
/// inserting at one place at once do) or a new one, one removed, one moved
/// onto another's key, or a field of one changed.
fn peer_stack(doc: &mut Doc, rng: &mut Lcg, step: usize) {
    use tilo_resource::__private::{ObjType, ReadDoc};
    let mut peer = doc.fork().with_actor(actor("peer"));
    let stack = root_map(&peer, "stack");
    let ids: Vec<String> = peer.keys(&stack).collect();
    for _ in 0..=rng.next(3) {
        let other = ids.get(rng.next(ids.len() + 1)).cloned();
        let other_key = other.as_deref().and_then(|id| order_key(&peer, &stack, id));
        match rng.next(5) {
            0 => {
                let id = format!("p{step:04}-{}", rng.next(1000));
                let key = other_key.unwrap_or_else(|| "zz".to_owned());
                put_ordered_entry(&mut peer, &stack, &id, &key, &shot(&id, "peer"));
            }
            1 => {
                if let Some(other) = &other
                    && peer.get(&stack, other.as_str()).unwrap().is_some()
                {
                    automerge::transaction::Transactable::delete(&mut peer, &stack, other.as_str())
                        .unwrap();
                }
            }
            2 => {
                let mover = ids.get(rng.next(ids.len() + 1));
                if let (Some(mover), Some(key)) = (mover, other_key)
                    && let Some((Value::Object(ObjType::Map), entry)) =
                        peer.get(&stack, mover.as_str()).unwrap()
                {
                    automerge::transaction::Transactable::put(&mut peer, &entry, "k", key).unwrap();
                }
            }
            _ => {
                if let Some(other) = &other
                    && let Some((Value::Object(ObjType::Map), entry)) =
                        peer.get(&stack, other.as_str()).unwrap()
                    && let Some((Value::Object(ObjType::Map), value)) =
                        peer.get(&entry, "v").unwrap()
                {
                    automerge::transaction::Transactable::put(&mut peer, &value, "opacity", -1.0)
                        .unwrap();
                }
            }
        }
    }
    peer.commit();
    doc.merge_incoming(&mut peer).unwrap();
}

/// Change the Ordered list `stack` of `cut` at random: items changed,
/// removed, added and moved (which a copy cannot follow item by item).
fn change_stack(cut: &mut Cut, rng: &mut Lcg, step: usize) {
    match rng.next(8) {
        0 => {
            let len = cut.stack.len();
            if len > 1 {
                cut.stack.swap(rng.next(len), rng.next(len));
            }
        }
        1 => cut
            .stack
            .push(shot(&format!("n{step:04}-{}", rng.next(1000)), "new")),
        _ => remove_and_change(cut, rng, step),
    }
}

#[test]
fn an_ordered_list_saved_over_its_copy_writes_what_a_whole_write_does() {
    use tilo_resource::__private::ROOT;
    let root = tilo_resource::Path::<Ordered<Shot>, Ordered<Shot>>::root();
    let apart = |items: &Ordered<Shot>| -> Ordered<Shot> { items.to_vec().into() };
    let mut saves = 0;
    for seed in 0..40 {
        let mut rng = Lcg(seed);
        let mut held: Ordered<Shot> = (0..rng.next(80))
            .map(|n| shot(&format!("s{n:04}"), &format!("shot {n}")))
            .collect();
        let mut doc = Doc::new().with_actor(actor("base"));
        tilo_resource::write_key(&mut doc, &ROOT, "stack", &held).unwrap();
        commit_at_zero(&mut doc);
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(&mut doc);
        for step in 0..8 {
            if rng.next(3) == 0 {
                peer_stack(&mut doc, &mut rng, step);
            }
            let dirty = tracker.delta(&mut doc);
            // What the slot holds now, brought up to date as a reader does.
            let mut was = held.clone();
            let id = root_map(&doc, "stack");
            let value = Value::Object(tilo_resource::__private::ObjType::Map);
            was.refresh(&doc, &value, &id, &dirty).unwrap();
            let read: Ordered<Shot> = tilo_resource::read_key(&doc, &ROOT, "stack").unwrap();
            assert_eq!(was, read, "seed {seed} step {step}: refreshed as read");

            let mut copy = Cut {
                title: String::new(),
                shots: Keyed::new(),
                stack: was.clone(),
            };
            for _ in 0..rng.next(6) {
                change_stack(&mut copy, &mut rng, step);
            }
            let now = copy.stack;
            assert_eq!(
                edit_parts(&root.edits(&was, &now)),
                edit_parts(&root.edits(&apart(&was), &apart(&now))),
                "seed {seed} step {step}: the edits"
            );

            let mut changed = doc.fork().with_actor(actor("me"));
            let mut whole = doc.fork().with_actor(actor("me"));
            tilo_resource::write_key_changed(&mut changed, &ROOT, "stack", &now, &was, &dirty)
                .unwrap();
            tilo_resource::write_key(&mut whole, &ROOT, "stack", &now).unwrap();
            commit_at_zero(&mut changed);
            commit_at_zero(&mut whole);
            assert_eq!(
                changed.get_heads(),
                whole.get_heads(),
                "seed {seed} step {step}: the saved change"
            );
            // Saving a copy no one changed writes what a whole write does.
            let mut same = doc.fork().with_actor(actor("me"));
            let mut same_whole = doc.fork().with_actor(actor("me"));
            tilo_resource::write_key_changed(&mut same, &ROOT, "stack", &was.clone(), &was, &dirty)
                .unwrap();
            tilo_resource::write_key(&mut same_whole, &ROOT, "stack", &was).unwrap();
            commit_at_zero(&mut same);
            commit_at_zero(&mut same_whole);
            assert_eq!(
                same.get_heads(),
                same_whole.get_heads(),
                "seed {seed} step {step}: the unchanged copy saved"
            );
            saves += 1;
            doc = changed;
            tracker.start(&mut doc);
            // Two items under one id are one item in the document.
            let mut ids: Vec<Id> = now.iter().map(Identified::id).collect();
            ids.sort();
            ids.dedup();
            held = if ids.len() == now.len() {
                now
            } else {
                tilo_resource::read_key(&doc, &ROOT, "stack").unwrap()
            };
        }
    }
    assert_eq!(saves, 320);
}

#[test]
fn saving_a_keyed_list_changed_and_added_to_visits_only_those_items() {
    use tilo_resource::__private::ROOT;
    use tilo_resource::Fields;
    let folios: Vec<Folio> = (0..300)
        .map(|n| Folio {
            id: format!("f{n:04}"),
            page: Page(format!("page {n}")),
        })
        .collect();
    let before = Volume {
        title: "volume".into(),
        folios: folios.clone().into(),
        stack: folios.into(),
    };
    let mut doc = Doc::new().with_actor(actor("base"));
    before.write_fields(&mut doc, &ROOT).unwrap();
    commit_at_zero(&mut doc);

    let mut after = before.clone();
    after.folios.find_mut(&Id::from_raw("f0150")).unwrap().page = Page("changed".into());
    after.folios.push(Folio {
        id: "f9000".into(),
        page: Page("new".into()),
    });
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        after
            .write_changed_fields(
                &before,
                &mut changed,
                &ROOT,
                &tilo_resource::Delta::nothing(),
            )
            .unwrap();
    });
    assert_eq!(stored, 2, "the changed item and the new one are written");
    assert!(compared <= 4, "{compared} items of 300 compared");
    let mut whole = doc.fork().with_actor(actor("me"));
    after.write_fields(&mut whole, &ROOT).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");
    let folios: Keyed<Folio> = tilo_resource::read_key(&changed, &ROOT, "folios").unwrap();
    assert_eq!(folios.len(), 301, "the new item is there");
}

#[test]
fn saving_a_collection_changed_in_one_item_compares_no_item_it_shares() {
    use tilo_resource::__private::ROOT;
    let mut held = Collection::new();
    for n in 0..300 {
        held.push(
            Id::from_raw(format!("f{n:04}")),
            Folio {
                id: format!("f{n:04}"),
                page: Page(format!("page {n}")),
            },
        );
    }
    let mut doc = Doc::new().with_actor(actor("base"));
    tilo_resource::write_key(&mut doc, &ROOT, "pages", &held).unwrap();
    commit_at_zero(&mut doc);
    // As read, so each item knows its stored order key.
    let before: Collection<Folio> = tilo_resource::read_key(&doc, &ROOT, "pages").unwrap();

    let mut after = before.clone();
    after.get_mut(&Id::from_raw("f0150")).unwrap().page = Page("changed".into());
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut changed,
            &ROOT,
            "pages",
            &after,
            &before,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!(stored, 1, "only the changed item is written");
    assert!(compared <= 4, "{compared} items of 300 compared");
    let mut whole = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key(&mut whole, &ROOT, "pages", &after).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");

    // Its edits compare the changed item alone.
    let root = tilo_resource::Path::<Collection<Folio>, Collection<Folio>>::root();
    let (edits, compared, _) = pages_counted(|| root.edits(&before, &after));
    assert!(compared <= 2, "{compared} items compared for the edits");
    assert_eq!(edits.len(), 1);
    // A copy nothing changed stores as it is.
    let ((), compared, _) = pages_counted(|| assert!(before.clone().stores_as(&before)));
    assert_eq!(compared, 0);
}

/// The map `key` at the document root.
fn root_map(doc: &Doc, key: &str) -> ObjId {
    use tilo_resource::__private::{ObjType, ROOT, ReadDoc};
    match doc.get(ROOT, key).unwrap() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    }
    .unwrap()
}

/// What a collaborator writes into the list `shots` meanwhile: an item put
/// whole, one removed, or a field of one changed.
fn peer_shots(doc: &mut Doc, rng: &mut Lcg, step: usize) {
    use tilo_resource::__private::{ObjType, ReadDoc};
    let mut peer = doc.fork().with_actor(actor("peer"));
    let shots = root_map(&peer, "shots");
    let keys: Vec<String> = peer.keys(&shots).collect();
    for _ in 0..=rng.next(3) {
        let key = keys
            .get(rng.next(keys.len() + 1))
            .cloned()
            .unwrap_or_else(|| format!("p{step:04}"));
        match rng.next(3) {
            0 => tilo_resource::write_key(
                &mut peer,
                &shots,
                &key,
                &shot(&key, &format!("peer {step}")),
            )
            .unwrap(),
            1 => {
                if peer.get(&shots, key.as_str()).unwrap().is_some() {
                    automerge::transaction::Transactable::delete(&mut peer, &shots, key.as_str())
                        .unwrap();
                }
            }
            _ => {
                if let Some((Value::Object(ObjType::Map), entry)) =
                    peer.get(&shots, key.as_str()).unwrap()
                {
                    automerge::transaction::Transactable::put(&mut peer, &entry, "opacity", -1.0)
                        .unwrap();
                }
            }
        }
    }
    peer.commit();
    doc.merge_incoming(&mut peer).unwrap();
}

/// Whether no two of `items` carry the same id.
fn ids_unique(items: &Keyed<Shot>) -> bool {
    let mut ids: Vec<Id> = items.iter().map(Identified::id).collect();
    ids.sort();
    ids.dedup();
    ids.len() == items.len()
}

#[test]
fn a_keyed_list_saved_over_its_copy_writes_what_a_whole_write_does() {
    keyed_saves_write_what_a_whole_write_does(add_and_change);
}

#[test]
fn a_keyed_list_removed_from_saved_over_its_copy_writes_what_a_whole_write_does() {
    keyed_saves_write_what_a_whole_write_does(remove_and_change);
}

/// 40 random sequences of 8 saves of a Keyed list, each copy changed by
/// `change`, with a collaborator's concurrent writes: each save writes what
/// a whole write does, and the edits are those of comparing every item.
fn keyed_saves_write_what_a_whole_write_does(change: fn(&mut Cut, &mut Lcg, usize)) {
    use tilo_resource::__private::ROOT;
    let root = tilo_resource::Path::<Keyed<Shot>, Keyed<Shot>>::root();
    let apart = |items: &Keyed<Shot>| -> Keyed<Shot> { items.to_vec().into() };
    let mut saves = 0;
    for seed in 0..40 {
        let mut rng = Lcg(seed);
        let mut held: Keyed<Shot> = (0..rng.next(120))
            .map(|n| shot(&format!("s{n:04}"), &format!("shot {n}")))
            .collect();
        held.settle();
        let mut doc = Doc::new().with_actor(actor("base"));
        tilo_resource::write_key(&mut doc, &ROOT, "shots", &held).unwrap();
        commit_at_zero(&mut doc);
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(&mut doc);
        for step in 0..8 {
            if rng.next(3) == 0 {
                peer_shots(&mut doc, &mut rng, step);
            }
            let dirty = tracker.delta(&mut doc);
            let mut was = held.clone();
            let id = root_map(&doc, "shots");
            let value = Value::Object(tilo_resource::__private::ObjType::Map);
            was.refresh(&doc, &value, &id, &dirty).unwrap();
            was.settle();
            let read: Keyed<Shot> = tilo_resource::read_key(&doc, &ROOT, "shots").unwrap();
            assert_eq!(was, read, "seed {seed} step {step}: refreshed as read");

            let mut copy = Cut {
                title: String::new(),
                shots: was.clone(),
                stack: Ordered::new(),
            };
            for _ in 0..rng.next(6) {
                change(&mut copy, &mut rng, step);
            }
            let now = copy.shots;
            assert_eq!(
                edit_parts(&root.edits(&was, &now)),
                edit_parts(&root.edits(&apart(&was), &apart(&now))),
                "seed {seed} step {step}: the edits"
            );

            let mut changed = doc.fork().with_actor(actor("me"));
            let mut whole = doc.fork().with_actor(actor("me"));
            tilo_resource::write_key_changed(&mut changed, &ROOT, "shots", &now, &was, &dirty)
                .unwrap();
            tilo_resource::write_key(&mut whole, &ROOT, "shots", &now).unwrap();
            commit_at_zero(&mut changed);
            commit_at_zero(&mut whole);
            assert_eq!(
                changed.get_heads(),
                whole.get_heads(),
                "seed {seed} step {step}: the saved change"
            );
            saves += 1;
            doc = changed;
            tracker.start(&mut doc);
            held = if ids_unique(&now) {
                now
            } else {
                tilo_resource::read_key(&doc, &ROOT, "shots").unwrap()
            };
        }
    }
    assert_eq!(saves, 320);
}

fn element(n: usize) -> String {
    format!("e{n:04}")
}

/// Change `set` at random: elements added (some in runs that split a
/// chunk) and removed (some in runs that empty one), now and then all of it
/// cleared or replaced by an equal set that shares nothing.
fn change_elements(set: &mut Set<String>, rng: &mut Lcg) {
    for _ in 0..rng.next(6) {
        let n = rng.next(400);
        match rng.next(10) {
            0..=2 => {
                set.insert(element(n));
            }
            3 => {
                for k in 0..rng.next(80) {
                    set.insert(format!("{}-{k:02}", element(n)));
                }
            }
            4..=6 => {
                set.remove(&element(n));
            }
            7 => {
                for k in n..n + rng.next(70) {
                    set.remove(&element(k));
                }
            }
            8 => {
                if rng.next(4) == 0 {
                    *set = Set::new();
                }
            }
            _ => *set = set.iter().cloned().collect(),
        }
    }
}

/// What a collaborator writes into the set `tags` meanwhile: elements
/// added and removed.
fn peer_elements(doc: &mut Doc, rng: &mut Lcg) {
    use tilo_resource::__private::ReadDoc;
    let mut peer = doc.fork().with_actor(actor("peer"));
    let tags = root_map(&peer, "tags");
    for _ in 0..=rng.next(4) {
        let key = element(rng.next(400));
        if rng.next(2) == 0 {
            automerge::transaction::Transactable::put(&mut peer, &tags, key.as_str(), true)
                .unwrap();
        } else if peer.get(&tags, key.as_str()).unwrap().is_some() {
            automerge::transaction::Transactable::delete(&mut peer, &tags, key.as_str()).unwrap();
        }
    }
    peer.commit();
    doc.merge_incoming(&mut peer).unwrap();
}

#[test]
fn a_set_saved_over_its_copy_writes_what_a_whole_write_does() {
    use tilo_resource::__private::ROOT;
    let root = tilo_resource::Path::<Set<String>, Set<String>>::root();
    let apart = |set: &Set<String>| -> Set<String> { set.iter().cloned().collect() };
    let mut saves = 0;
    for seed in 0..40 {
        let mut rng = Lcg(seed);
        let mut held: Set<String> = (0..rng.next(400)).map(element).collect();
        let mut doc = Doc::new().with_actor(actor("base"));
        tilo_resource::write_key(&mut doc, &ROOT, "tags", &held).unwrap();
        commit_at_zero(&mut doc);
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(&mut doc);
        for step in 0..8 {
            if rng.next(3) == 0 {
                peer_elements(&mut doc, &mut rng);
            }
            let dirty = tracker.delta(&mut doc);
            let mut was = held.clone();
            let id = root_map(&doc, "tags");
            let value = Value::Object(tilo_resource::__private::ObjType::Map);
            was.refresh(&doc, &value, &id, &dirty).unwrap();
            let read: Set<String> = tilo_resource::read_key(&doc, &ROOT, "tags").unwrap();
            assert_eq!(was, read, "seed {seed} step {step}: refreshed as read");

            let mut now = was.clone();
            change_elements(&mut now, &mut rng);
            assert_eq!(
                edit_parts(&root.edits(&was, &now)),
                edit_parts(&root.edits(&apart(&was), &apart(&now))),
                "seed {seed} step {step}: the edits"
            );

            let mut changed = doc.fork().with_actor(actor("me"));
            let mut whole = doc.fork().with_actor(actor("me"));
            tilo_resource::write_key_changed(&mut changed, &ROOT, "tags", &now, &was, &dirty)
                .unwrap();
            tilo_resource::write_key(&mut whole, &ROOT, "tags", &now).unwrap();
            if now.stores_as(&was) {
                assert_eq!(
                    automerge::transaction::Transactable::pending_ops(&whole),
                    0,
                    "seed {seed} step {step}: what stores as before writes nothing"
                );
            }
            commit_at_zero(&mut changed);
            commit_at_zero(&mut whole);
            assert_eq!(
                changed.get_heads(),
                whole.get_heads(),
                "seed {seed} step {step}: the saved change"
            );
            saves += 1;
            doc = changed;
            tracker.start(&mut doc);
            held = now;
        }
    }
    assert_eq!(saves, 320);
}

#[test]
fn a_set_changed_in_one_element_writes_one() {
    use tilo_resource::__private::ROOT;
    let held: Set<String> = (0..1000).map(element).collect();
    let mut doc = Doc::new().with_actor(actor("base"));
    tilo_resource::write_key(&mut doc, &ROOT, "tags", &held).unwrap();
    commit_at_zero(&mut doc);

    assert!(held.clone().stores_as(&held), "a copy stores as it is");
    let mut now = held.clone();
    now.insert("e0500-new".to_owned());
    assert!(!now.stores_as(&held));
    let mut changed = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key_changed(
        &mut changed,
        &ROOT,
        "tags",
        &now,
        &held,
        &tilo_resource::Delta::nothing(),
    )
    .unwrap();
    assert_eq!(
        automerge::transaction::Transactable::pending_ops(&changed),
        1
    );
    let mut whole = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key(&mut whole, &ROOT, "tags", &now).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");
    assert_eq!(
        edit_parts(&tilo_resource::Path::<Set<String>, Set<String>>::root().edits(&held, &now)),
        [(
            Vec::new(),
            tilo_resource::EditOp::Insert {
                value: serde_json::json!("e0500-new")
            }
        )]
    );
}

fn row(n: usize) -> Id {
    Id::from_raw(format!("r{n:04}"))
}

fn column(n: usize) -> Id {
    Id::from_raw(format!("c{n:02}"))
}

/// Change `grid` at random: cells written, cleared and changed in place,
/// rows and columns added and removed, now and then all of it replaced by
/// an equal grid that shares nothing.
fn change_grid(grid: &mut Grid<i64>, rng: &mut Lcg, step: usize) {
    let rows: Vec<Id> = grid.rows().cloned().collect();
    let columns: Vec<Id> = grid.columns().cloned().collect();
    let pick = |ids: &[Id], rng: &mut Lcg| ids.get(rng.next(ids.len())).cloned();
    let value = i64::try_from(step).unwrap();
    for made in 0..rng.next(6) {
        let (at_row, at_column) = (pick(&rows, rng), pick(&columns, rng));
        match rng.next(11) {
            0..=3 => {
                if let (Some(r), Some(c)) = (at_row, at_column) {
                    grid.set_cell(&r, &c, Some(value));
                }
            }
            4 => {
                if let (Some(r), Some(c)) = (at_row, at_column) {
                    grid.set_cell(&r, &c, None);
                }
            }
            5 => {
                if let (Some(r), Some(c)) = (at_row, at_column)
                    && let Some(cell) = grid.cell_mut(&r, &c)
                {
                    *cell += 100;
                }
            }
            6 => {
                let at = rng.next(rows.len() + 1);
                grid.insert_row(at, Id::from_raw(format!("r{step}-{made}")));
            }
            7 => {
                if let Some(r) = at_row {
                    grid.remove_row(&r);
                }
            }
            8 => {
                let at = rng.next(columns.len() + 1);
                grid.insert_column(at, Id::from_raw(format!("c{step}-{made}")));
            }
            9 => {
                if let Some(c) = at_column {
                    grid.remove_column(&c);
                }
            }
            _ => *grid = Grid::from_json(&grid.to_json()).unwrap(),
        }
    }
}

/// What a collaborator writes into the grid `grid` meanwhile: cells
/// written, changed and cleared.
fn peer_cells(doc: &mut Doc, rng: &mut Lcg) {
    use tilo_resource::__private::{ObjType, ReadDoc};
    let mut peer = doc.fork().with_actor(actor("peer"));
    let grid = root_map(&peer, "grid");
    let map = |key: &str| match peer.get(&grid, key).unwrap() {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    };
    let (rows, columns, cells) = (map("rows"), map("columns"), map("cells"));
    let (Some(rows), Some(columns), Some(cells)) = (rows, columns, cells) else {
        return;
    };
    let rows: Vec<String> = peer.keys(&rows).collect();
    let columns: Vec<String> = peer.keys(&columns).collect();
    let held: Vec<String> = peer.keys(&cells).collect();
    for _ in 0..=rng.next(4) {
        match rng.next(3) {
            0 => {
                if let (Some(r), Some(c)) = (
                    rows.get(rng.next(rows.len())),
                    columns.get(rng.next(columns.len())),
                ) {
                    let key = format!("{r}/{c}");
                    automerge::transaction::Transactable::put(&mut peer, &cells, key, -1_i64)
                        .unwrap();
                }
            }
            1 => {
                if let Some(key) = held.get(rng.next(held.len())) {
                    automerge::transaction::Transactable::put(
                        &mut peer,
                        &cells,
                        key.as_str(),
                        -2_i64,
                    )
                    .unwrap();
                }
            }
            _ => {
                if let Some(key) = held.get(rng.next(held.len()))
                    && peer.get(&cells, key.as_str()).unwrap().is_some()
                {
                    automerge::transaction::Transactable::delete(&mut peer, &cells, key.as_str())
                        .unwrap();
                }
            }
        }
    }
    peer.commit();
    doc.merge_incoming(&mut peer).unwrap();
}

#[test]
fn a_grid_saved_over_its_copy_writes_what_a_whole_write_does() {
    use tilo_resource::__private::ROOT;
    let root = tilo_resource::Path::<Grid<i64>, Grid<i64>>::root();
    let apart = |grid: &Grid<i64>| -> Grid<i64> { Grid::from_json(&grid.to_json()).unwrap() };
    let mut saves = 0;
    for seed in 0..40 {
        let mut rng = Lcg(seed);
        let mut held: Grid<i64> = Grid::new();
        for n in 0..rng.next(200) {
            held.insert_row(n, row(n));
        }
        for n in 0..=rng.next(8) {
            held.insert_column(n, column(n));
        }
        for n in 0..rng.next(600) {
            let (r, c) = (row(rng.next(200)), column(rng.next(8)));
            held.set_cell(&r, &c, Some(i64::try_from(n).unwrap()));
        }
        let mut doc = Doc::new().with_actor(actor("base"));
        tilo_resource::write_key(&mut doc, &ROOT, "grid", &held).unwrap();
        commit_at_zero(&mut doc);
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(&mut doc);
        for step in 0..8 {
            if rng.next(3) == 0 {
                peer_cells(&mut doc, &mut rng);
            }
            let dirty = tracker.delta(&mut doc);
            let mut was = held.clone();
            let id = root_map(&doc, "grid");
            let value = Value::Object(tilo_resource::__private::ObjType::Map);
            was.refresh(&doc, &value, &id, &dirty).unwrap();
            let read: Grid<i64> = tilo_resource::read_key(&doc, &ROOT, "grid").unwrap();
            assert_eq!(was, read, "seed {seed} step {step}: refreshed as read");

            let mut now = was.clone();
            change_grid(&mut now, &mut rng, step);
            assert_eq!(
                edit_parts(&root.edits(&was, &now)),
                edit_parts(&root.edits(&apart(&was), &apart(&now))),
                "seed {seed} step {step}: the edits"
            );

            let mut changed = doc.fork().with_actor(actor("me"));
            let mut whole = doc.fork().with_actor(actor("me"));
            tilo_resource::write_key_changed(&mut changed, &ROOT, "grid", &now, &was, &dirty)
                .unwrap();
            tilo_resource::write_key(&mut whole, &ROOT, "grid", &now).unwrap();
            if now.stores_as(&was) {
                assert_eq!(
                    automerge::transaction::Transactable::pending_ops(&whole),
                    0,
                    "seed {seed} step {step}: what stores as before writes nothing"
                );
            }
            commit_at_zero(&mut changed);
            commit_at_zero(&mut whole);
            assert_eq!(
                changed.get_heads(),
                whole.get_heads(),
                "seed {seed} step {step}: the saved change"
            );
            saves += 1;
            doc = changed;
            tracker.start(&mut doc);
            held = now;
        }
    }
    assert_eq!(saves, 320);
}

#[test]
fn saving_a_grid_changed_in_one_cell_visits_one_chunk_of_rows() {
    use tilo_resource::__private::ROOT;
    let mut held: Grid<Page> = Grid::new();
    for n in 0..1000 {
        held.insert_row(n, row(n));
    }
    for n in 0..4 {
        held.insert_column(n, column(n));
    }
    for r in 0..1000 {
        for c in 0..4 {
            held.set_cell(&row(r), &column(c), Some(Page(format!("{r}:{c}"))));
        }
    }
    let mut doc = Doc::new().with_actor(actor("base"));
    tilo_resource::write_key(&mut doc, &ROOT, "grid", &held).unwrap();
    commit_at_zero(&mut doc);

    let mut now = held.clone();
    *now.cell_mut(&row(500), &column(2)).unwrap() = Page("changed".into());
    let mut changed = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut changed,
            &ROOT,
            "grid",
            &now,
            &held,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!(stored, 1, "only the changed cell is written");
    assert!(
        compared <= 2 * 64 * 4,
        "{compared} cells of 4000 compared: the changed chunk's rows, at most twice"
    );
    let mut whole = doc.fork().with_actor(actor("me"));
    tilo_resource::write_key(&mut whole, &ROOT, "grid", &now).unwrap();
    commit_at_zero(&mut changed);
    commit_at_zero(&mut whole);
    assert_eq!(changed.get_heads(), whole.get_heads(), "the same change");

    // An unchanged copy compares and writes nothing.
    let mut same = doc.fork().with_actor(actor("me"));
    let ((), compared, stored) = pages_counted(|| {
        tilo_resource::write_key_changed(
            &mut same,
            &ROOT,
            "grid",
            &held.clone(),
            &held,
            &tilo_resource::Delta::nothing(),
        )
        .unwrap();
    });
    assert_eq!((compared, stored), (0, 0));
    assert_eq!(automerge::transaction::Transactable::pending_ops(&same), 0);

    // The edits between the two compare that chunk's cells.
    let root = tilo_resource::Path::<Grid<Page>, Grid<Page>>::root();
    let (edits, compared, _) = pages_counted(|| root.edits(&held, &now));
    assert!(
        compared <= 64 * 4,
        "{compared} cells compared for the edits"
    );
    assert_eq!(edits.len(), 1);
}
