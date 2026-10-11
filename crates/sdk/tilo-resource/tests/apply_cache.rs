//! An action starts from the record the previous action left, refreshed with
//! only the changes since, and writes only what it changed — and still makes
//! the change, byte for byte, that reading and writing the whole record
//! makes, so a replica replaying it with nothing kept reproduces it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "test assertions, counters and fixtures of a known shape"
)]

use std::cell::Cell;

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ROOT, ReadDoc as _, ScalarValue};
use tilo_automerge::ApplyIncoming as _;

use tilo_resource::__private::{Value, forget_all};
use tilo_resource::{
    ActionCx, Collection, Doc, Fields, Grid, Id, Model, ModelError, Replica, Slot, resource,
};

thread_local! {
    static LOADS: Cell<usize> = const { Cell::new(0) };
    static STORES: Cell<usize> = const { Cell::new(0) };
}

/// Text that counts how often it is read from, and written to, the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe(pub String);

impl Model for Probe {
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
        STORES.with(|stores| stores.set(stores.get() + 1));
        self.0.store(doc, slot)
    }
    fn stores_as(&self, before: &Self) -> bool {
        self == before
    }
}

fn counts() -> (usize, usize) {
    (LOADS.with(Cell::get), STORES.with(Cell::get))
}

/// Loads and stores of [`Probe`] while `run` runs.
fn counted<T>(run: impl FnOnce() -> T) -> (T, (usize, usize)) {
    let (loads, stores) = counts();
    let result = run();
    let (after_loads, after_stores) = counts();
    (result, (after_loads - loads, after_stores - stores))
}

#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    #[default]
    List,
    Board,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Task {
    pub title: Probe,
    pub done: bool,
    pub note: Option<String>,
    pub rank: f64,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
    pub mode: Mode,
    pub tags: Vec<String>,
}

#[resource(kind = "test.cached-checklist", label = "Checklist")]
impl Checklist {
    /// Type into the draft.
    #[action]
    pub fn draft(&mut self, text: String) {
        self.draft = text;
    }

    /// Add a task.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.tasks.push(
            cx.id(),
            Task {
                title: Probe(title),
                ..Task::default()
            },
        );
    }

    /// Tick or untick a task.
    #[action]
    pub fn toggle(&mut self, task: &Id) -> Result<(), &'static str> {
        let task = self.tasks.get_mut(task).ok_or("no such task")?;
        task.done = !task.done;
        Ok(())
    }

    /// Retitle a task, and note why.
    #[action]
    pub fn retitle(&mut self, task: &Id, title: String) -> Result<(), &'static str> {
        let task = self.tasks.get_mut(task).ok_or("no such task")?;
        task.note = (!title.is_empty()).then(|| format!("was {}", task.title.0));
        task.title = Probe(title);
        task.rank += 0.5;
        Ok(())
    }

    /// Move a task.
    #[action]
    pub fn move_task(&mut self, task: &Id, index: usize) {
        self.tasks.move_to(task, index);
    }

    /// Delete a task.
    #[action]
    pub fn remove(&mut self, task: &Id) {
        self.tasks.remove(task);
    }

    /// Switch between list and board.
    #[action]
    pub fn flip(&mut self) {
        self.mode = match self.mode {
            Mode::List => Mode::Board,
            Mode::Board => Mode::List,
        };
    }

    /// Tag the list.
    #[action]
    pub fn tag(&mut self, tag: String) {
        self.tags.push(tag);
    }

    /// Refuse, having changed the record first.
    #[action]
    pub fn refuse(&mut self) -> Result<(), &'static str> {
        self.draft.push_str("never written");
        Err("refused")
    }
}

/// The door publishes from the view state the replica keeps current; it
/// follows the document here, as an open window does between actions, so
/// what [`counted`] then sees is the action's own reading and writing.
fn follow(replica: &mut Replica<Checklist>) {
    replica.live().unwrap();
}

/// A checklist of `count` tasks, written straight into a new document.
fn seeded(count: usize) -> Replica<Checklist> {
    let mut replica = Replica::<Checklist>::new();
    let mut record = Checklist::default();
    for n in 0..count {
        record.tasks.push(
            Id::from_raw(format!("task-{n:04}")),
            Task {
                title: Probe(format!("task {n}")),
                ..Task::default()
            },
        );
    }
    record.write_fields(replica.doc_mut(), &ROOT).unwrap();
    replica.doc_mut().commit();
    follow(&mut replica);
    replica
}

#[test]
fn apply_reads_only_touched_items() {
    let mut replica = seeded(1_000);
    let target = Id::from_raw("task-0500");

    // Nothing kept yet: the first action reads every task.
    follow(&mut replica);
    let (_, (loads, _)) = counted(|| replica.apply(Checklist::DRAFT.with("h")).unwrap());
    assert_eq!(loads, 1_000, "the first action reads the record whole");

    follow(&mut replica);
    let (_, (loads, stores)) = counted(|| replica.apply(Checklist::DRAFT.with("he")).unwrap());
    assert_eq!(
        (loads, stores),
        (0, 0),
        "typing into the draft reads and writes no task"
    );

    follow(&mut replica);
    let (_, (loads, stores)) = counted(|| {
        replica
            .apply(Checklist::TOGGLE.with(target.clone()))
            .unwrap()
    });
    assert_eq!((loads, stores), (0, 0), "a tick touches no title");

    follow(&mut replica);
    let (_, (loads, stores)) = counted(|| {
        replica
            .apply(Checklist::RETITLE.with((target.clone(), "renamed".into())))
            .unwrap()
    });
    assert_eq!(
        (loads, stores),
        (0, 1),
        "retitling one task writes one title"
    );

    follow(&mut replica);
    let (_, (loads, stores)) = counted(|| replica.apply(Checklist::DRAFT.with("hel")).unwrap());
    assert_eq!(
        (loads, stores),
        (1, 0),
        "the next action re-reads only the title the last one wrote"
    );

    let state = replica.state().unwrap();
    assert_eq!(state.draft, "hel");
    let task = &state.tasks[&target];
    assert_eq!(task.title.0, "renamed");
    assert!(task.done);
    assert_eq!(task.note.as_deref(), Some("was task 500"));
    assert_eq!(state.tasks.len(), 1_000);
}

#[test]
fn a_refused_action_writes_nothing_and_the_next_reads_whole() {
    let mut replica = seeded(10);
    replica.apply(Checklist::DRAFT.with("kept")).unwrap();
    let heads = replica.heads();
    assert!(replica.apply(Checklist::REFUSE.invoke()).is_err());
    assert_eq!(replica.heads(), heads, "a refusal changes nothing");
    follow(&mut replica);
    let (_, (loads, _)) = counted(|| replica.apply(Checklist::DRAFT.with("kept!")).unwrap());
    assert_eq!(loads, 10, "a refusal keeps no record");
    assert_eq!(replica.state().unwrap().draft, "kept!");
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

fn random_invoke(
    replica: &Replica<Checklist>,
    rng: &mut Lcg,
    step: usize,
) -> Option<tilo_resource::Invoke<Checklist>> {
    let ids: Vec<Id> = replica.state().unwrap().tasks.ids().cloned().collect();
    let pick = |rng: &mut Lcg| ids.get(rng.next(ids.len())).cloned();
    Some(match rng.next(10) {
        0 | 1 => Checklist::DRAFT.with(format!("draft {step} {}", "x".repeat(step % 7))),
        2 | 3 => Checklist::ADD.with(format!("task {step}")),
        4 => Checklist::TOGGLE.with(pick(rng)?),
        5 => Checklist::RETITLE.with((pick(rng)?, format!("title {step}"))),
        6 => Checklist::MOVE_TASK.with((pick(rng)?, rng.next(ids.len() + 1))),
        7 => Checklist::REMOVE.with(pick(rng)?),
        8 => Checklist::FLIP.invoke(),
        _ => Checklist::TAG.with(format!("t{}", step % 3)),
    })
}

fn tasks_map(doc: &Doc) -> ObjId {
    match doc.get(ROOT, "tasks").unwrap() {
        Some((Value::Object(ObjType::Map), id)) => id,
        other => panic!("tasks is {other:?}"),
    }
}

/// Write something into `doc` no action of this schema would: the slots an
/// older or newer replica might leave, which a whole write rewrites.
fn raw_write(doc: &mut Doc, rng: &mut Lcg) {
    let tasks = tasks_map(doc);
    let ids: Vec<String> = doc.keys(&tasks).collect();
    let entry = |doc: &Doc, id: &str| match doc.get(&tasks, id).unwrap() {
        Some((Value::Object(ObjType::Map), entry)) => entry,
        other => panic!("entry is {other:?}"),
    };
    let value = |doc: &Doc, id: &str| match doc.get(entry(doc, id), "v").unwrap() {
        Some((Value::Object(ObjType::Map), value)) => value,
        other => panic!("value is {other:?}"),
    };
    let Some(id) = ids.get(rng.next(ids.len())).cloned() else {
        doc.put(ROOT, "draft", "a plain string draft").unwrap();
        doc.commit();
        return;
    };
    match rng.next(5) {
        // A null where a boolean belongs.
        0 => {
            let value = value(doc, &id);
            doc.put(&value, "done", ScalarValue::Null).unwrap();
        }
        // A field missing.
        1 => {
            let value = value(doc, &id);
            doc.delete(&value, "rank").unwrap();
        }
        // A string scalar where text belongs.
        2 => doc.put(ROOT, "draft", "a plain string draft").unwrap(),
        // An integer where a float belongs.
        3 => {
            let value = value(doc, &id);
            doc.put(&value, "rank", ScalarValue::Int(3)).unwrap();
        }
        // Two items under one order key.
        _ => {
            let other = ids[rng.next(ids.len())].clone();
            let key = match doc.get(entry(doc, &other), "k").unwrap() {
                Some((Value::Scalar(key), _)) => key.into_owned(),
                other => panic!("order key is {other:?}"),
            };
            let entry = entry(doc, &id);
            doc.put(&entry, "k", key).unwrap();
        }
    }
    doc.commit();
}

#[test]
fn cached_apply_is_replay_identical() {
    let mut rng = Lcg(29);
    // `live` acts with the record kept from action to action; `cold`
    // replays each of its actions with nothing kept; `remote` is a
    // collaborator whose changes, actions and raw writes alike, both take in.
    let mut live = seeded(12);
    let mut cold = live.fork();
    let mut remote = live.fork();
    let mut live_loads = 0;
    let mut cold_loads = 0;
    let mut replayed = 0;
    for round in 0..40 {
        if let Some(invoke) = random_invoke(&remote, &mut rng, 1_000 + round) {
            remote.apply(invoke).unwrap();
        }
        if round % 3 == 0 {
            raw_write(remote.doc_mut(), &mut rng);
        }
        if round % 2 == 0 {
            live.doc_mut().merge_incoming(remote.doc_mut()).unwrap();
            cold.doc_mut().merge_incoming(remote.doc_mut()).unwrap();
        }
        for step in 0..4 {
            let Some(invoke) = random_invoke(&live, &mut rng, round * 10 + step) else {
                continue;
            };
            follow(&mut live);
            let (_, (loads, _)) = counted(|| live.apply(invoke).unwrap());
            live_loads += loads;
            let envelope = live.log().last().unwrap().clone();
            forget_all();
            let ((), (loads, _)) = counted(|| cold.replay(&envelope).unwrap());
            cold_loads += loads;
            replayed += 1;
            assert_eq!(
                live.heads(),
                cold.heads(),
                "round {round} step {step}: the replayed change differs"
            );
            assert_eq!(live.state().unwrap(), cold.state().unwrap());
        }
    }
    assert!(replayed > 100, "only {replayed} actions replayed");
    assert!(
        live_loads * 4 < cold_loads,
        "the live replica read {live_loads} titles, the cold one {cold_loads}"
    );
}

/// Text made the first time as a change of its own beside the action that
/// stores it (as a deterministic slot is), merged in while the action runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MergedText(pub String);

impl Model for MergedText {
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
        if let Slot::Key(parent, key) = slot
            && doc.pending_ops() == 0
            && doc.get(parent, key)?.is_none()
        {
            let heads = doc.get_heads();
            let mut made = doc
                .fork_at(&heads)?
                .with_actor(automerge::ActorId::from(b"merged-slot".as_slice()));
            let text = made.put_object(parent, key, ObjType::Text)?;
            made.splice_text(&text, 0, 0, &self.0)?;
            made.commit();
            doc.merge_incoming(&mut made)?;
            return Ok(());
        }
        self.0.store(doc, slot)
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Texts {
    pub texts: std::collections::BTreeMap<String, MergedText>,
    pub count: i64,
}

#[resource(kind = "test.merged-texts", label = "Texts")]
impl Texts {
    /// Keep a text under a key, the first time.
    #[action]
    pub fn keep_text(&mut self, key: String, text: String) {
        self.texts.entry(key).or_insert(MergedText(text));
    }

    /// Count once more.
    #[action]
    pub fn bump(&mut self) {
        self.count += 1;
    }
}

/// An action whose write merged a change of its own (a text made beside
/// it) moved its base while it ran: it keeps no record for the next action,
/// which must still see the text rather than write over it.
#[test]
fn a_record_kept_across_a_merged_slot_does_not_lose_it() {
    let mut texts = Replica::<Texts>::new();
    // A record kept from this action is what the next one would start from.
    texts.apply(Texts::BUMP.invoke()).unwrap();
    let keep = |key: &str, text: &str| Texts::KEEP_TEXT.with((key.to_owned(), text.to_owned()));
    texts.apply(keep("readme", "hello")).unwrap();
    // The next action writes the map the text is in.
    texts.apply(keep("notes", "later")).unwrap();
    let state = texts.state().unwrap();
    assert_eq!(
        state.texts.get("readme").map(|text| text.0.as_str()),
        Some("hello"),
        "the next action keeps the text the last one merged"
    );
    assert_eq!(
        state.texts.get("notes").map(|text| text.0.as_str()),
        Some("later")
    );
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Table {
    pub cells: Grid<Probe>,
    pub title: String,
}

#[resource(kind = "test.cached-table", label = "Table")]
impl Table {
    /// Add a row.
    #[action]
    pub fn add_row(&mut self, cx: &mut ActionCx) {
        let at = self.cells.rows().len();
        self.cells.insert_row(at, cx.id());
    }

    /// Add a column.
    #[action]
    pub fn add_column(&mut self, cx: &mut ActionCx) {
        let at = self.cells.columns().len();
        self.cells.insert_column(at, cx.id());
    }

    /// Write a cell, or clear it with no text.
    #[action]
    pub fn write(&mut self, cell: (String, String), text: String) {
        let value = (!text.is_empty()).then_some(Probe(text));
        self.cells
            .set_cell(&Id::from_raw(cell.0), &Id::from_raw(cell.1), value);
    }

    /// Remove a row.
    #[action]
    pub fn drop_row(&mut self, line: String) {
        self.cells.remove_row(&Id::from_raw(line));
    }

    /// Rename the table.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

fn random_table_invoke(
    replica: &Replica<Table>,
    rng: &mut Lcg,
    step: usize,
) -> tilo_resource::Invoke<Table> {
    let state = replica.state().unwrap();
    let rows: Vec<Id> = state.cells.rows().cloned().collect();
    let columns: Vec<Id> = state.cells.columns().cloned().collect();
    let cell = |rng: &mut Lcg| {
        let row = rows.get(rng.next(rows.len()))?;
        let column = columns.get(rng.next(columns.len()))?;
        Some((row.as_str().to_owned(), column.as_str().to_owned()))
    };
    match rng.next(9) {
        0 => Table::ADD_ROW.with(()),
        1 => Table::ADD_COLUMN.with(()),
        2 => match rows.get(rng.next(rows.len())) {
            Some(row) => Table::DROP_ROW.with(row.as_str().to_owned()),
            None => Table::ADD_ROW.with(()),
        },
        3 => Table::RENAME.with(format!("table {step}")),
        4 => match cell(rng) {
            Some(cell) => Table::WRITE.with((cell, String::new())),
            None => Table::ADD_COLUMN.with(()),
        },
        _ => match cell(rng) {
            Some(cell) => Table::WRITE.with((cell, format!("v{step}"))),
            None => Table::ADD_ROW.with(()),
        },
    }
}

#[test]
fn cached_grid_apply_is_replay_identical() {
    let mut rng = Lcg(31);
    let mut live = Replica::<Table>::new();
    let mut cold = live.fork();
    let mut remote = live.fork();
    let mut replayed = 0;
    for round in 0..40 {
        let invoke = random_table_invoke(&remote, &mut rng, 1_000 + round);
        remote.apply(invoke).unwrap();
        if round % 2 == 0 {
            live.doc_mut().merge_incoming(remote.doc_mut()).unwrap();
            cold.doc_mut().merge_incoming(remote.doc_mut()).unwrap();
        }
        for step in 0..5 {
            let invoke = random_table_invoke(&live, &mut rng, round * 10 + step);
            live.live().unwrap();
            live.apply(invoke).unwrap();
            let envelope = live.log().last().unwrap().clone();
            forget_all();
            cold.replay(&envelope).unwrap();
            replayed += 1;
            assert_eq!(
                live.heads(),
                cold.heads(),
                "round {round} step {step}: the replayed change differs"
            );
            assert_eq!(live.state().unwrap(), cold.state().unwrap());
        }
    }
    assert!(replayed > 100, "only {replayed} actions replayed");
}

#[test]
fn writing_one_cell_writes_only_that_cell() {
    let mut replica = Replica::<Table>::new();
    replica.apply(Table::ADD_COLUMN.with(())).unwrap();
    for _ in 0..100 {
        replica.apply(Table::ADD_ROW.with(())).unwrap();
    }
    let state = replica.state().unwrap();
    let rows: Vec<String> = state
        .cells
        .rows()
        .map(|row| row.as_str().to_owned())
        .collect();
    let column = state.cells.columns().next().unwrap().as_str().to_owned();
    for (n, row) in rows.iter().enumerate() {
        replica
            .apply(Table::WRITE.with(((row.clone(), column.clone()), format!("cell {n}"))))
            .unwrap();
    }
    replica.live().unwrap();
    let (_, (_, stores)) = counted(|| {
        replica
            .apply(Table::WRITE.with(((rows[50].clone(), column.clone()), "changed".into())))
            .unwrap()
    });
    assert_eq!(stores, 1, "one cell written");
}
