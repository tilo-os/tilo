//! Incremental reads: after any change, the state a view holds — refreshed
//! from only what the change touched — equals a full read, and the refresh
//! reads only what the change touched.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test assertions and counters"
)]

use std::cell::Cell;
use std::collections::BTreeMap;

use tilo_automerge::ApplyIncoming;

use tilo_resource::__private::{ObjId, Value};
use tilo_resource::history;
use tilo_resource::{
    ActionCx, Collection, Doc, Id, Model, ModelError, Replica, Resource, Slot, resource,
};

thread_local! {
    static LOADS: Cell<usize> = const { Cell::new(0) };
}

/// Text that counts how often it is read from the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counted(pub String);

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
    replica
        .apply(Notebook::RENAME.with("Renamed".into()))
        .unwrap();
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
    left.apply(Notebook::ADD.with("shared".into())).unwrap();
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
    // Summaries come from the action that made each change.
    assert!(page.versions.iter().all(|v| v.summary().is_some()));
    assert_eq!(<Notebook as Resource>::KIND, "test.notebook");
}
