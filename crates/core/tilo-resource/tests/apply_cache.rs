//! An action starts from the record the previous action left, refreshed with
//! only the changes since, and writes only what it changed — and still makes
//! the change, byte for byte, that reading and writing the whole record
//! makes, so a replica replaying it with nothing kept reproduces it.

#![allow(
    clippy::unwrap_used,
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
use tilo_resource::AppNamed;
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

#[resource(kind = "test.cached-checklist", catalogue = "tests/locales")]
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

/// The API publishes from the view state the replica keeps current; it
/// follows the document here, as an open window does between actions, so
/// what [`counted`] then sees is the action's own reading and writing.
fn follow(replica: &mut Replica<Checklist, AppNamed>) {
    replica.live().unwrap();
}

/// A checklist of `count` tasks, written straight into a new document.
fn seeded(count: usize) -> Replica<Checklist, AppNamed> {
    let mut replica = Replica::<Checklist>::new().in_app("so.tilo.model", "Model");
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
    replica: &Replica<Checklist, AppNamed>,
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
        0 => {
            let value = value(doc, &id);
            doc.put(&value, "done", ScalarValue::Null).unwrap();
        }
        1 => {
            let value = value(doc, &id);
            doc.delete(&value, "rank").unwrap();
        }
        2 => doc.put(ROOT, "draft", "a plain string draft").unwrap(),
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
    // member whose changes, actions and raw writes alike, both take in.
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
    fn store(&self, doc: &mut Doc, into: Slot<'_>) -> Result<(), ModelError> {
        if let Slot::Key(parent, key) = into
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
        self.0.store(doc, into)
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Texts {
    pub texts: std::collections::BTreeMap<String, MergedText>,
    pub count: i64,
}

#[resource(kind = "test.merged-texts", catalogue = "tests/locales")]
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
    let mut texts = Replica::<Texts>::new().in_app("so.tilo.model", "Model");
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

#[resource(kind = "test.cached-table", catalogue = "tests/locales")]
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
    replica: &Replica<Table, AppNamed>,
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
    let mut live = Replica::<Table>::new().in_app("so.tilo.model", "Model");
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

/// A group of many members runs on one copy of the record: it is read once, each member's
/// verb changes it in turn, and it is written once at the end. The change it makes is still
/// the one a replica with nothing kept makes replaying the group, after a member's
/// changes and raw writes merged in, and a group a member refuses writes nothing at all.
#[test]
fn a_group_reads_and_writes_its_record_once_and_replays_identically() {
    let mut rng = Lcg(37);
    let mut live = seeded(200);
    let mut cold = live.fork();
    let mut remote = live.fork();
    for round in 0..6 {
        if let Some(invoke) = random_invoke(&remote, &mut rng, 5_000 + round) {
            remote.apply(invoke).unwrap();
        }
        raw_write(remote.doc_mut(), &mut rng);
        live.doc_mut().merge_incoming(remote.doc_mut()).unwrap();
        cold.doc_mut().merge_incoming(remote.doc_mut()).unwrap();
        let state = live.state().unwrap();
        let mut members: Vec<tilo_resource::Invoke<Checklist>> = (0..60)
            .map(|n| Checklist::ADD.with(format!("bulk {round}-{n}")))
            .collect();
        for task in state.tasks.ids().take(5) {
            members.push(Checklist::TOGGLE.with(task.clone()));
        }
        members.push(Checklist::TAG.with(format!("round {round}")));
        follow(&mut live);
        let (_, (loads, _)) =
            counted(|| live.apply(tilo_resource::Invoke::group(members)).unwrap());
        assert!(
            loads <= 200 + 60 * (round + 1),
            "round {round}: the group read its record {loads} titles' worth, not once"
        );
        let envelope = live.log().last().unwrap().clone();
        forget_all();
        cold.replay(&envelope).unwrap();
        assert_eq!(
            live.heads(),
            cold.heads(),
            "round {round}: the replayed group differs"
        );
        assert_eq!(live.state().unwrap(), cold.state().unwrap());
    }
    let before = live.heads();
    let refused = tilo_resource::Invoke::group([
        Checklist::ADD.with("kept?".to_owned()),
        Checklist::REFUSE.invoke(),
    ]);
    assert!(live.apply(refused).is_err());
    assert_eq!(live.heads(), before, "a refused group writes nothing");
    assert!(
        !live
            .state()
            .unwrap()
            .tasks
            .iter()
            .any(|(_, task)| task.title.0 == "kept?"),
        "nor does any member of it"
    );
}

/// What a group wrote, once at its end, is where the next action starts: it reads none of
/// the group's own change back, and still writes what a replica with nothing kept writes.
#[test]
fn the_action_after_a_group_starts_from_what_the_group_wrote() {
    let mut live = seeded(50);
    let mut cold = live.fork();
    let members: Vec<tilo_resource::Invoke<Checklist>> = (0..20)
        .map(|n| Checklist::ADD.with(format!("bulk {n}")))
        .collect();
    live.apply(tilo_resource::Invoke::group(members)).unwrap();
    let group = live.log().last().unwrap().clone();
    follow(&mut live);
    let (_, (loads, _)) = counted(|| live.apply(Checklist::DRAFT.with("after")).unwrap());
    assert_eq!(
        loads, 0,
        "the next action reads none of the group's tasks back"
    );
    let next = live.log().last().unwrap().clone();
    forget_all();
    cold.replay(&group).unwrap();
    forget_all();
    cold.replay(&next).unwrap();
    assert_eq!(live.heads(), cold.heads(), "the replayed actions differ");
    assert_eq!(live.state().unwrap(), cold.state().unwrap());
}

/// The view that follows a group takes the items the group stored as they were written: it
/// reads none of them back, holds what reading the whole document holds, and follows the
/// changes after it as it follows any other.
#[test]
fn the_view_after_a_group_takes_the_items_it_stored() {
    let mut live = seeded(50);
    let members: Vec<tilo_resource::Invoke<Checklist>> = (0..20)
        .map(|n| Checklist::ADD.with(format!("bulk {n}")))
        .collect();
    live.apply(tilo_resource::Invoke::group(members)).unwrap();
    let (followed, (loads, _)) = counted(|| live.live().unwrap().clone());
    assert_eq!(loads, 0, "the view reads none of the group's tasks back");
    assert_eq!(followed, live.state().unwrap());
    assert_eq!(followed.tasks.len(), 70);

    let added = followed.tasks.ids().last().unwrap().clone();
    live.apply(Checklist::TOGGLE.with(added.clone())).unwrap();
    live.apply(Checklist::MOVE_TASK.with((added, 0))).unwrap();
    assert_eq!(live.live().unwrap().clone(), live.state().unwrap());
}

thread_local! {
    static DRAFT_LOADS: Cell<usize> = const { Cell::new(0) };
}

/// Text that reads back as something else than it was written, as rich text
/// reads back as the structure the document made of it: written from what it
/// was given, read as what the document holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drafted {
    given: Option<String>,
    read: Option<String>,
}

impl Drafted {
    fn text(&self) -> &str {
        self.given.as_deref().or(self.read.as_deref()).unwrap_or("")
    }
}

impl Model for Drafted {
    tilo_resource::leaf_paths!();
    const OPS: bool = true;
    fn to_json(&self) -> serde_json::Value {
        serde_json::Value::String(self.text().to_owned())
    }
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        String::from_json(value).map(|given| Self {
            given: Some(given),
            read: None,
        })
    }
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        DRAFT_LOADS.with(|loads| loads.set(loads.get() + 1));
        String::load(doc, value, id).map(|read| Self {
            given: None,
            read: Some(read),
        })
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    fn store(&self, doc: &mut Doc, into: Slot<'_>) -> Result<(), ModelError> {
        match &self.given {
            Some(given) => given.store(doc, into),
            None => Ok(()),
        }
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Page {
    pub title: String,
    pub body: Drafted,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notebook {
    pub pages: Collection<Page>,
}

#[resource(kind = "test.cached-notebook", catalogue = "tests/locales")]
impl Notebook {
    /// Write pages.
    #[action]
    pub fn write_pages(&mut self, cx: &mut ActionCx, bodies: Vec<String>) {
        for body in bodies {
            let page = Page {
                title: body.clone(),
                body: Drafted {
                    given: Some(body),
                    read: None,
                },
            };
            self.pages.push(cx.id(), page);
        }
    }

    /// Retitle a page.
    #[action]
    pub fn retitle_page(&mut self, page: &Id, title: String) -> Result<(), &'static str> {
        self.pages.get_mut(page).ok_or("no such page")?.title = title;
        Ok(())
    }
}

fn draft_loads<T>(act: impl FnOnce() -> T) -> (T, usize) {
    let before = DRAFT_LOADS.with(Cell::get);
    let result = act();
    (result, DRAFT_LOADS.with(Cell::get) - before)
}

/// A write that made items holding what reads back as something else than it
/// was written keeps each as it reads back: the view following it and the next
/// action take them rather than reading the pages back, however large the
/// change, and that next action still makes the change a replica with nothing
/// kept makes.
#[test]
fn items_that_read_back_otherwise_are_kept_as_they_read_back() {
    let mut live = Replica::<Notebook>::new().in_app("so.tilo.model", "Model");
    live.apply(Notebook::WRITE_PAGES.with(vec!["first".into()]))
        .unwrap();
    let mut cold = live.fork();
    live.live().unwrap();
    for round in 0..3 {
        let bodies: Vec<String> = (0..300).map(|n| format!("page {round}.{n}")).collect();
        let (_, loads) = draft_loads(|| live.apply(Notebook::WRITE_PAGES.with(bodies)).unwrap());
        assert_eq!(
            loads, 300,
            "round {round}: the write reads back each body it wrote, and no other"
        );
        let (followed, loads) = draft_loads(|| live.live().unwrap().clone());
        assert_eq!(loads, 0, "round {round}: the view reads no page back");
        let whole = live.state().unwrap();
        assert_eq!(
            followed, whole,
            "round {round}: the view holds what a read holds"
        );
        assert!(
            whole
                .pages
                .iter()
                .all(|(_, page)| page.body.given.is_none()),
            "round {round}: a read gives each body as it reads back"
        );
        let envelope = live.log().last().unwrap().clone();
        forget_all();
        cold.replay(&envelope).unwrap();
        assert_eq!(
            live.heads(),
            cold.heads(),
            "round {round}: the replayed change differs"
        );
    }
    let page = live.state().unwrap().pages.ids().last().unwrap().clone();
    let (_, loads) = draft_loads(|| {
        live.apply(Notebook::RETITLE_PAGE.with((page, "renamed".into())))
            .unwrap()
    });
    assert_eq!(
        loads, 0,
        "the next action starts from the pages as they read back"
    );
    let envelope = live.log().last().unwrap().clone();
    forget_all();
    cold.replay(&envelope).unwrap();
    assert_eq!(
        live.heads(),
        cold.heads(),
        "the retitle replays identically"
    );
    assert_eq!(live.state().unwrap(), cold.state().unwrap());
}
