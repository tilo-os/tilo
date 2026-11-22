//! The chain's storage of an app's items at 1,000, 10,000 and 25,000 items:
//! a change to one item writes about the same bytes, finding an item by its
//! id takes about as long, and a page of the listing reads about as fast,
//! however many items the chain keeps. The app here declares only its
//! items and verbs over them; where each item is kept is the chain's.

use std::rc::Rc;
use std::time::{Duration, Instant};

use tilo_app::framework::{Chain, Chained, PART_ITEMS, PartLimits};
use tilo_app::prelude::*;
use tilo_app::resource::{DocumentId, ResourceOwners, ResourceStore};
use tilo_app::tilo_resource::{Collection, NamesItem};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Log {
    pub title: String,
}

#[resource(kind = "test.scale-log", catalogue = "tests/locales")]
impl Log {
    /// Rename the log.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Entry {
    pub text: String,
    pub amount: i64,
}

/// An entry a verb adds: new, so it names no item yet.
#[derive(
    Default,
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_app::tilo_resource::JsonSchema,
)]
pub struct NewEntry {
    pub text: String,
    pub amount: i64,
}

impl NamesItem for NewEntry {
    fn named_item(&self) -> Option<Id> {
        None
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Entries {
    #[model(readonly)]
    pub entries: Collection<Entry>,
}

#[resource(kind = "test.scale-log-entries", catalogue = "tests/locales")]
impl Entries {
    /// Add entries.
    #[action(items = entries)]
    pub fn add(&mut self, cx: &mut ActionCx, entries: Vec<NewEntry>) {
        for NewEntry { text, amount } in entries {
            self.entries.push(cx.id(), Entry { text, amount });
        }
    }

    /// Change what an entry says.
    #[action]
    pub fn say(&mut self, entry: &Id, text: String) {
        if let Some(entry) = self.entries.get_mut(entry) {
            entry.text = text;
        }
    }
}

impl Chained for Entries {
    type Base = Log;

    fn chain() -> DocumentId {
        DocumentId::derived("entries", &DocumentId::workspace_default())
    }

    const HELD_WHOLE: bool = true;
}

fn chain() -> (ResourceStore, Chain<Entries>) {
    let store = ResourceOwners::with_store(
        Rc::new(durable_store::MemoryStore::new()) as Rc<dyn durable_store::DurableStore>
    )
    .open("so.tilo.test-scale-log", "workspace")
    .unwrap();
    let me = MemberId::person("ada");
    let log = Rc::new(Log::default());
    let chain = Chain::<Entries>::detached_in(store.clone(), "so.tilo.test-scale-log", &me, {
        move || Rc::clone(&log)
    })
    .unwrap();
    (store, chain)
}

/// Add `count` entries, a batch of them per call.
fn add(chain: &Chain<Entries>, from: usize, count: usize) {
    const BATCH: usize = 500;
    let end = from.saturating_add(count);
    for at in (from..end).step_by(BATCH) {
        let last = at.saturating_add(BATCH).min(end);
        let entries = (at..last)
            .map(|n| NewEntry {
                text: format!("entry {n}: a line of what was bought, and for whom"),
                amount: i64::try_from(n).unwrap(),
            })
            .collect();
        chain
            .invoke(&Entries::ADD.with(entries::Add { entries }))
            .unwrap();
    }
}

/// Every entry's id, oldest first.
fn ids(chain: &Chain<Entries>) -> Vec<Id> {
    chain
        .parts()
        .iter()
        .flat_map(|(_, part)| part.entries.ids().cloned().collect::<Vec<_>>())
        .collect()
}

/// What one change writes, in bytes, into every part together, and how many
/// parts it wrote into.
fn change_bytes(
    store: &ResourceStore,
    chain: &Chain<Entries>,
    change: impl FnOnce(),
) -> (usize, usize) {
    let parts: Vec<DocumentId> = chain
        .parts()
        .iter()
        .map(|(index, _)| Entries::chain().part(*index))
        .collect();
    let mut frontiers: Vec<_> = parts
        .iter()
        .map(|id| {
            let mut frontier = tilo_app::DocumentFrontier::default();
            let (_, heads) = store.replication_delta(id, &mut frontier).unwrap();
            frontier.advance(heads);
            frontier
        })
        .collect();
    change();
    let mut bytes: usize = 0;
    let mut written: usize = 0;
    for (id, frontier) in parts.iter().zip(&mut frontiers) {
        let (changes, _) = store.replication_delta(id, frontier).unwrap();
        if !changes.is_empty() {
            written = written.saturating_add(1);
            bytes = bytes.saturating_add(changes.len());
        }
    }
    (bytes, written)
}

struct Measured {
    items: usize,
    parts: usize,
    edit_bytes: usize,
    lookup: Duration,
    page: Duration,
}

fn per(total: Duration, count: u32) -> Duration {
    total.checked_div(count).unwrap_or_default()
}

fn measure(store: &ResourceStore, chain: &Chain<Entries>, items: usize) -> Measured {
    let ids = ids(chain);
    assert_eq!(ids.len(), items);
    assert_eq!(chain.len(), items, "the index holds every item");
    let parts = chain.parts();
    for (index, part) in &parts {
        assert!(
            part.entries.len() <= PART_ITEMS,
            "part {index} holds {} items",
            part.entries.len()
        );
    }

    // A change to an item of the oldest part, and one of the newest.
    let mut edit_bytes = 0;
    let oldest = ids.get(1).expect("the oldest part holds a second item");
    let newest = ids
        .get(items.saturating_sub(2))
        .expect("the newest part holds a second item from its end");
    for id in [oldest, newest] {
        let (bytes, written) = change_bytes(store, chain, || {
            chain
                .invoke(&Entries::SAY.with(entries::Say {
                    entry: id.clone(),
                    text: format!("changed at {items}"),
                }))
                .unwrap();
        });
        assert_eq!(written, 1, "a change to one item writes only its part");
        edit_bytes = edit_bytes.max(bytes);
        let holding = chain.find(id).expect("the item is found by its id");
        assert_eq!(
            holding.entries.get(id).unwrap().text,
            format!("changed at {items}")
        );
    }

    // Finding items by id, spread over the whole chain.
    let probes: Vec<&Id> = ids
        .iter()
        .step_by(items.checked_div(200).unwrap_or(0).max(1))
        .collect();
    let first_probe = probes.first().expect("a chain of items has a probe");
    let _ = chain.find(first_probe);
    let started = Instant::now();
    for id in &probes {
        assert!(chain.find(id).is_some());
    }
    let lookup = per(started.elapsed(), u32::try_from(probes.len()).unwrap());

    // A page of the listing, then the page after it.
    let rounds = 20u32;
    let started = Instant::now();
    for _ in 0..rounds {
        let first = chain.read(&serde_json::json!({ "pageSize": 50 })).unwrap();
        let cursor = first
            .get("nextCursor")
            .and_then(serde_json::Value::as_str)
            .expect("a page names the cursor of the one after it")
            .to_owned();
        let next = chain
            .read(&serde_json::json!({ "pageSize": 50, "cursor": cursor }))
            .unwrap();
        assert!(next.get("nextCursor").is_some(), "{next}");
    }
    let page = per(started.elapsed(), rounds.saturating_mul(2));

    Measured {
        items,
        parts: parts.len(),
        edit_bytes,
        lookup,
        page,
    }
}

#[test]
fn bytes_lookup_and_paging_stay_flat_from_one_thousand_to_twenty_five_thousand_items() {
    let (store, chain) = chain();
    let mut measured = Vec::new();
    let mut held = 0;
    for items in [1_000, 10_000, 25_000] {
        add(&chain, held, items - held);
        held = items;
        let m = measure(&store, &chain, items);
        eprintln!(
            "{} items in {} parts: a change writes {} bytes, a lookup takes {:?}, a page {:?}",
            m.items, m.parts, m.edit_bytes, m.lookup, m.page
        );
        measured.push(m);
    }
    let small = &measured[0];
    let large = &measured[2];
    assert_eq!(
        large.parts, 25,
        "twenty-five thousand items in parts of a thousand"
    );
    assert!(
        large.edit_bytes <= small.edit_bytes * 2 + 256,
        "a change to one item costs the item: {} bytes at 1k, {} at 25k",
        small.edit_bytes,
        large.edit_bytes
    );
    // Timings on a shared machine move; what grows with the items held grows
    // by twenty-five times here, far past this.
    let flat = |small: Duration, large: Duration| large <= small * 8 + Duration::from_micros(50);
    assert!(
        flat(small.lookup, large.lookup),
        "a lookup: {:?} at 1k, {:?} at 25k",
        small.lookup,
        large.lookup
    );
    assert!(
        flat(small.page, large.page),
        "a page: {:?} at 1k, {:?} at 25k",
        small.page,
        large.page
    );
}

/// A chain's typed read answers each item as an inline collection of the
/// same items holds it (`{id, value}`, as the model writes it), a page at a
/// time, so one type reads a collection however it is kept.
#[test]
fn a_chained_read_answers_its_items_as_the_inline_collection_of_them_reads() {
    let _limits = PartLimits::scoped(10, usize::MAX);
    let (_store, chain) = chain();
    add(&chain, 0, 35);
    assert_eq!(chain.parts().len(), 4, "the items are kept in four parts");

    // Every page, newest first.
    let mut read = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = chain
            .read(&serde_json::json!({ "pageSize": 7, "cursor": cursor }))
            .unwrap();
        read.extend(page["entries"].as_array().cloned().unwrap_or_default());
        match page.get("nextCursor").and_then(serde_json::Value::as_str) {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    read.reverse();

    // The same items in one inline collection, oldest first.
    let mut inline = Entries::default();
    for (_, part) in chain.parts() {
        for (id, entry) in part.entries.iter() {
            inline.entries.push(id.clone(), entry.clone());
        }
    }
    let written = Model::to_json(&inline);
    assert_eq!(read.len(), 35);
    assert_eq!(serde_json::Value::Array(read), written["entries"]);

    // The fields asked for keep the value to them; the id stays.
    let narrow = chain
        .read(&serde_json::json!({ "pageSize": 1, "fields": ["amount"] }))
        .unwrap();
    let newest = &written["entries"][34];
    assert_eq!(
        narrow["entries"],
        serde_json::json!([{ "id": newest["id"], "value": { "amount": 34 } }])
    );

    // A row's id reads its item whole.
    let id = newest["id"].as_str().unwrap();
    let item = chain.read(&serde_json::json!({ "item": id })).unwrap();
    assert_eq!(item["amount"], 34, "{item}");
}
