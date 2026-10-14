//! Incremental reads: after any change, the state a view holds — refreshed
//! from only what the change touched — equals a full read, and the refresh
//! reads only what the change touched.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test assertions and counters"
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tilo_automerge::ApplyIncoming;

use tilo_resource::__private::{ObjId, Value};
use tilo_resource::history;
use tilo_resource::{
    ActionCx, Collection, Doc, Grid, Id, Identified, Keyed, Model, ModelError, Ordered, Reader,
    Replica, Resource, Set, Slot, resource,
};

thread_local! {
    static LOADS: Cell<usize> = const { Cell::new(0) };
    static CLONES: Cell<usize> = const { Cell::new(0) };
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
fn the_first_page_of_a_long_history_reads_only_that_page() {
    let mut replica = Replica::<Notebook>::new();
    for n in 0..2_000 {
        replica
            .apply(Notebook::TAG.with(format!("t{}", n % 7)))
            .unwrap();
    }
    let doc = &mut replica.doc().clone();
    let started = std::time::Instant::now();
    let page = history::page(doc, None, 50);
    let paged = started.elapsed();
    let started = std::time::Instant::now();
    let all = history::versions(doc);
    let listed = started.elapsed();
    assert_eq!(page.versions.len(), 50);
    assert_eq!(
        page.versions.first().map(|v| v.hash),
        all.last().map(|v| v.hash)
    );
    assert!(page.older.is_some());
    assert!(
        paged < listed,
        "a page ({paged:?}) should cost less than the whole history ({listed:?})"
    );
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
