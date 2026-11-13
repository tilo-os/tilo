//! What one storage unit costs (open, files on disk, bytes on disk, memory),
//! and what one Automerge document costs as it grows (save bytes, load time,
//! one change appended, history bytes, memory).
//!
//! A storage unit today is one record: one file of Automerge changes under
//! `<root>/<package>/rooms/<workspace>/records/<record>/state.automerge`, one
//! route in the workspace, one index row. The fixed cost of a unit is what a
//! workspace pays per record before it holds anything; the document curve is
//! what a unit costs once it does, and is the input to choosing how many
//! records' worth of items one chunk of a workspace holds.
//!
//! Every documented item is one map holding two scalars, and grows by one
//! change per item, which is the append path an app's local action takes
//! ([`DocStore::append`], append-then-compact).
//!
//! Run with:
//!
//!     cargo bench -p tilo-app-state --bench record_cost
//!
//! `--units 1,10,100,1000 --items 1000,10000,100000 --samples N` change what
//! it measures. One CSV line per scenario and size, on stdout. Set `TMPDIR` to
//! keep the scratch records off a small tmpfs.

#![expect(
    clippy::expect_used,
    reason = "a benchmark fixture must fail rather than measure a half-built record"
)]
// A measurement is bookkeeping over the numbers it just took, not app logic:
// the indices are into the sample vectors the fixture just filled, and the
// arithmetic is over counts, sizes and times.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "see the note above"
)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use app_state::{DocStore, FsStore};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc};
use durable_store::StorageKey;
use tilo_automerge::heap::{self, ThreadCounting};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

const PACKAGE: &str = "so.tilo.bench";
const WORKSPACE: &str = "bench-workspace";

/// One documented item: its title and whether it is done.
fn add_item(items: &automerge::ObjId, doc: &mut AutoCommit, n: usize) {
    let id = format!("item-{n:06}");
    let item = doc
        .put_object(items, id, ObjType::Map)
        .expect("an item object");
    doc.put(&item, "title", format!("item {n}"))
        .expect("a title");
    doc.put(&item, "done", false).expect("a done flag");
}

/// A document holding `n` items under one collection, one change per item.
fn document(n: usize) -> AutoCommit {
    let mut doc = AutoCommit::new();
    let items = doc
        .put_object(ROOT, "items", ObjType::Map)
        .expect("an items map");
    for index in 0..n {
        add_item(&items, &mut doc, index);
        doc.commit();
    }
    doc
}

/// Every byte of the document's history: each change's raw encoding.
fn history_bytes(doc: &mut AutoCommit) -> usize {
    doc.get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().len())
        .fold(0, usize::saturating_add)
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1_000.0
}

fn summarise(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("a time"));
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let median = if n % 2 == 1 {
        samples[n / 2]
    } else {
        samples[n / 2 - 1].midpoint(samples[n / 2])
    };
    (median, mean)
}

/// Print one row of the table: a scenario at one size, with what it moved.
fn row(scenario: &str, count: usize, samples: Vec<f64>, files: usize, bytes: usize, heap: isize) {
    let (median, mean) = summarise(samples);
    println!(
        "{scenario},{count},{median:.2},{mean:.2},{:.1},{files},{bytes},{heap}",
        median * 1_000.0 / count as f64
    );
}

/// Files and bytes under `dir`.
fn disk(dir: &Path) -> (usize, usize) {
    let mut files = 0;
    let mut bytes: usize = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                files += 1;
                bytes = bytes.saturating_add(usize::try_from(meta.len()).unwrap_or(0));
            }
        }
    }
    (files, bytes)
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("record-cost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn store(root: &Path) -> Rc<dyn durable_store::DurableStore> {
    Rc::new(FsStore::new(root))
}

fn key(index: usize) -> StorageKey {
    StorageKey::document(PACKAGE, WORKSPACE, format!("record-{index:05}"))
}

/// Open `n` records that do not exist yet, each storing one empty document:
/// what a workspace pays to gain a unit.
fn create(root: &Path, n: usize) {
    let store = store(root);
    for index in 0..n {
        let mut unit = DocStore::shared(Rc::clone(&store), key(index)).expect("a record");
        let mut doc = unit.load_or_new();
        unit.save(&mut doc).expect("one save");
    }
}

/// Open and load the `n` records `create` made.
fn reopen(root: &Path, n: usize) {
    let store = store(root);
    for index in 0..n {
        let mut unit = DocStore::shared(Rc::clone(&store), key(index)).expect("a record");
        let mut doc = unit.load().expect("a load").expect("a stored document");
        assert_eq!(doc.get_heads().len(), 0);
    }
}

struct Sizes {
    units: Vec<usize>,
    items: Vec<usize>,
    samples: usize,
}

fn parse() -> Sizes {
    let mut sizes = Sizes {
        units: vec![1, 10, 100, 1_000],
        items: vec![1_000, 10_000, 100_000],
        samples: 3,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().expect("a value follows the flag");
        let sizes_of = |value: &str| -> Vec<usize> {
            value
                .split(',')
                .map(|s| s.trim().parse().expect("a size"))
                .collect()
        };
        match arg.as_str() {
            "--units" => sizes.units = sizes_of(&value()),
            "--items" => sizes.items = sizes_of(&value()),
            "--samples" => sizes.samples = value().parse().expect("a sample count"),
            "--bench" => {}
            other => eprintln!("record_cost: ignoring {other}"),
        }
    }
    sizes
}

fn units(sizes: &Sizes) {
    for &n in &sizes.units {
        let mut samples = Vec::new();
        let mut files = 0;
        let mut bytes = 0;
        let mut heap = 0;
        for _ in 0..sizes.samples {
            let root = scratch();
            let before = heap::held();
            let started = Instant::now();
            create(&root, n);
            samples.push(ms(started.elapsed()));
            (files, bytes) = disk(&root);
            // Every unit's store alive at once: what holding `n` records open
            // costs, which is what a workspace of `n` records holds today.
            let held = {
                let store = store(&root);
                let units: Vec<(DocStore, AutoCommit)> = (0..n)
                    .map(|index| {
                        let mut unit =
                            DocStore::shared(Rc::clone(&store), key(index)).expect("a record");
                        let mut doc = unit.load_or_new();
                        // One change per unit, so an empty document's actor and
                        // base are what is measured.
                        doc.put(ROOT, "open", true).expect("a mark");
                        doc.commit();
                        (unit, doc)
                    })
                    .collect();
                let held = heap::held().wrapping_sub(before);
                drop(units);
                held
            };
            heap = held;
            let _ = std::fs::remove_dir_all(&root);
        }
        row("unit_create", n, samples, files, bytes, heap);

        let mut samples = Vec::new();
        for _ in 0..sizes.samples {
            let root = scratch();
            create(&root, n);
            let started = Instant::now();
            reopen(&root, n);
            samples.push(ms(started.elapsed()));
            let _ = std::fs::remove_dir_all(&root);
        }
        row("unit_open", n, samples, files, bytes, 0);
    }
}

fn documents(sizes: &Sizes) {
    for &n in &sizes.items {
        let mut doc = document(n);
        let history = history_bytes(&mut doc);

        // The snapshot a full save writes, and what loading it costs.
        let mut samples = Vec::new();
        let mut snapshot = 0;
        for _ in 0..sizes.samples {
            let started = Instant::now();
            snapshot = doc.save().len();
            samples.push(ms(started.elapsed()));
        }
        row("doc_save", n, samples, 1, snapshot, 0);

        let bytes = doc.save();
        let mut samples = Vec::new();
        for _ in 0..sizes.samples {
            let started = Instant::now();
            let loaded = AutoCommit::load(&bytes).expect("a load");
            samples.push(ms(started.elapsed()));
            drop(loaded);
        }
        let load_heap = heap::held_by(|| AutoCommit::load(&bytes).expect("a load"));
        row("doc_load", n, samples, 1, bytes.len(), load_heap);

        // One more change on the loaded document, and the bytes that append.
        let mut samples = Vec::new();
        let mut delta = 0;
        for _ in 0..sizes.samples {
            let mut loaded = AutoCommit::load(&bytes).expect("a load");
            let heads = loaded.get_heads();
            let started = Instant::now();
            let items = loaded
                .get(ROOT, "items")
                .expect("an items map")
                .expect("an items map")
                .1;
            let item = loaded
                .get(&items, "item-000000")
                .expect("an item")
                .expect("an item")
                .1;
            loaded.put(&item, "done", true).expect("a change");
            loaded.commit();
            delta = loaded.save_after(&heads).len();
            samples.push(ms(started.elapsed()));
        }
        row("doc_change", n, samples, 1, delta, 0);

        // What the record's file holds once every item is stored, and what one
        // more change costs on top of it: the write a local action pays.
        let root = scratch();
        let store = store(&root);
        let mut unit = DocStore::shared(Rc::clone(&store), key(0)).expect("a record");
        let mut live = AutoCommit::load(&bytes).expect("a load");
        let started = Instant::now();
        unit.save(&mut live).expect("a save");
        let save_ms = ms(started.elapsed());
        let (files, stored) = disk(&root);
        row("store_save", n, vec![save_ms], files, stored, 0);

        // One real change: the item's title is rewritten, which is a change at
        // every sample where toggling a flag twice is not.
        let mut samples = Vec::new();
        let mut appended = 0;
        for sample in 0..sizes.samples {
            let items = live
                .get(ROOT, "items")
                .expect("an items map")
                .expect("an items map")
                .1;
            let item = live
                .get(&items, "item-000000")
                .expect("an item")
                .expect("an item")
                .1;
            live.put(&item, "title", format!("item 0 edit {sample}"))
                .expect("a change");
            live.commit();
            let started = Instant::now();
            appended = unit.append(&mut live).expect("an append");
            samples.push(ms(started.elapsed()));
        }
        row("store_append", n, samples, 1, appended, 0);

        // The record after the appends: what opening it costs, and what it
        // holds on disk.
        let mut samples = Vec::new();
        for _ in 0..sizes.samples {
            let mut unit = DocStore::shared(Rc::clone(&store), key(0)).expect("a record");
            let started = Instant::now();
            let loaded = unit.load().expect("a load").expect("a stored document");
            samples.push(ms(started.elapsed()));
            drop(loaded);
        }
        let (files, stored) = disk(&root);
        row("store_load", n, samples, files, stored, 0);
        let _ = std::fs::remove_dir_all(&root);

        // Every change the document's history holds, before any compaction.
        row("doc_history", n, vec![0.0], 1, history, 0);
    }
}

fn main() {
    // A test runner executes every target it lists, and this one writes
    // hundreds of megabytes of scratch records. Only `cargo bench` measures.
    if !std::env::args().skip(1).any(|arg| arg == "--bench") {
        return;
    }
    let sizes = parse();
    println!("scenario,count,median_ms,mean_ms,per_count_us,files,bytes,heap_bytes");
    units(&sizes);
    documents(&sizes);
}
