//! The runtime's budgets (`tilolang_tilo::bench`), held by the
//! bench's own scenarios. A tilolang app that misses one is a runtime bug.
//!
//! The gate holds the work each scenario does, in counts that do not depend on the machine or
//! its load ([`Work`]), as docs/measurements/performance.md has pull requests gate: what a
//! window shows, and what a change re-renders, patches and reads, is the same in a workspace of 10k
//! items as in one of 1k. Opening over the index a workspace kept reads no more of it at 10k than at
//! 1k: the pages of the rows it shows, and nothing of the workspace's record.
//!
//! The budgets in time and memory depend on the machine and what else runs on it, so no test
//! asserts them: the bench checks and records them on the machine they are for,
//! `cargo run --release -p tilolang-tilo --example bench -- apps/tl/<app> --sizes 1000,10000
//! --budgets`.
//!
//! Its own test target: the allocation counts are those of this work alone (they are counted per
//! thread, so the apps' tests may run side by side). The runs do the work the gate counts and
//! nothing that is only timed ([`BenchConfig::timings`]).

#![allow(clippy::panic, reason = "test assertions")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tilolang::Compiled;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::{InvokeOutcome, Principal, Value};
use tilolang_tilo::bench::{
    BenchConfig, CountingAlloc, EngineWorkloads, Env, Record, Work, compile, run, seeded_record,
};
use tilolang_tilo::host::Host;

/// The notes app, whose notebook's export the gate holds.
const NOTES: &str = "so.tilo.lang.notes";

/// Counted, so the records hold the allocations each scenario made.
#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// The sizes the gate compares: a scenario does at 10k what it does at 1k.
const SMALL: usize = 1000;
const LARGE: usize = 10_000;

/// The scenarios of a workspace that change it after it opened.
const CHANGES: &[&str] = &[
    "keystroke",
    "press",
    "peer_edit",
    "peer_post",
    "scroll",
    "thread_keystroke",
    "thread_post",
    "thread_send",
    "thread_scroll",
];

fn apps() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

fn bench(app: &str) -> Vec<Record> {
    bench_at(app, vec![SMALL, LARGE], 3)
}

/// The bench of `app` over workspaces of `sizes` items, `samples` changes each.
fn bench_at(app: &str, sizes: Vec<usize>, samples: usize) -> Vec<Record> {
    let config = BenchConfig {
        package: apps().join(app),
        sizes,
        // The first change apart (see `Work::first_of_record`), the rest steady ones.
        samples,
        idle: Duration::ZERO,
        // The engine the suite runs on (`TILOLANG_ENGINE`).
        engine: tilolang_runtime::configured(),
        workloads: EngineWorkloads::default(),
        profile: false,
        memory: None,
        open_only: false,
        budgets: false,
        timings: false,
        env: Env {
            commit: "test".into(),
            virtualization: "none".into(),
            runner: "test".into(),
        },
    };
    run(&config, &mut |_| {}).unwrap_or_else(|e| panic!("{app}: {e}"))
}

/// The record of `scenario` at the size `size` names (`10k`).
fn find<'a>(records: &'a [Record], scenario: &str, size: &str) -> Option<&'a Record> {
    let prefix = format!("tilolang.{scenario}.");
    let suffix = format!(".{size}");
    records
        .iter()
        .find(|r| r.kpi.starts_with(&prefix) && r.kpi.ends_with(&suffix))
}

/// The counts of what a scenario shows and changes: the boundaries it renders, the item
/// references it builds, the patches it sends, what it reads of the record one by one, the
/// index's directories, pages and fields it reads, and the records publication looked at.
fn shown(work: Work) -> [(&'static str, u64); 10] {
    [
        ("renders", work.renders),
        ("items", work.items),
        ("patches", work.patches),
        ("located", work.located),
        ("fields", work.fields),
        ("listings", work.listings),
        ("dirs", work.dirs),
        ("pages", work.pages),
        ("index_fields", work.index_fields),
        ("published", work.published),
    ]
}

/// What would read a collection's items as a whole: its entries walked in the record, and the
/// items a filter no column answers is evaluated on.
fn loaded(work: Work) -> [(&'static str, u64); 2] {
    [("entries", work.entries), ("evaluated", work.evaluated)]
}

/// The work `of` reads off the record of `scenario`, at 1k and at 10k.
fn both(
    records: &[Record],
    scenario: &str,
    of: fn(&Record) -> Option<Work>,
) -> Option<(Work, Work)> {
    let work = |size: &str| {
        let record = find(records, scenario, size)?;
        Some(of(record).unwrap_or_else(|| panic!("{} counts no work", record.kpi)))
    };
    Some((work("1k")?, work("10k")?))
}

/// How many more, or fewer, of `name` a frame may read at one size than at the other: a page
/// holds hundreds of entries where a frame shows tens, so whether the first row it shows starts a
/// page decides whether its window spans one page or two, and two sizes put the same window at
/// different offsets in a page. Only pages read so: what a frame reads of the workspace grows with
/// its items, and every entry of it is still read once.
fn slack(name: &str) -> u64 {
    u64::from(name == "pages")
}

/// What `scenario` shows and changes is the same at 10k as at 1k, count for count.
fn same_shown(app: &str, label: &str, (small, large): (Work, Work), misses: &mut Vec<String>) {
    for ((name, at_large), (_, at_small)) in shown(large).into_iter().zip(shown(small)) {
        if at_large.abs_diff(at_small) > slack(name) {
            misses.push(format!(
                "{app} {label}: {name} {at_large} at 10k, {at_small} at 1k"
            ));
        }
    }
}

fn gate(app: &str, records: &[Record], misses: &mut Vec<String>) {
    // Opening over the workspace's index shows the same at 10k as at 1k and reads the same: of the
    // index, the directories and pages of what it shows; of the record, nothing that grows.
    for scenario in ["open", "thread_open"] {
        let Some((small, large)) = both(records, scenario, Work::of_record) else {
            continue;
        };
        same_shown(app, scenario, (small, large), misses);
        for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
            if at_large != at_small {
                misses.push(format!(
                    "{app} {scenario}: {name} {at_large} at 10k, {at_small} at 1k: opening reads the workspace"
                ));
            }
        }
    }
    // Choosing a filter: its query's first page reads the same at 10k as at 1k, from the
    // index's columns; no item is checked one by one.
    if let Some((small, large)) = both(records, "filter", Work::of_record) {
        same_shown(app, "filter", (small, large), misses);
        for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
            if at_large != at_small {
                misses.push(format!(
                    "{app} filter: {name} {at_large} at 10k, {at_small} at 1k: a filter reads the workspace"
                ));
            }
        }
    } else if matches!(app, "todo") {
        // A window whose sidebar is a list of places is asked for one. The notebook's sidebar is
        // its notes as a tree: what a person filters by, the pinned ones, stands over the tree
        // as a filtered tree, and its query is read as the window opens.
        misses.push(format!("{app}: no filter was chosen"));
    }
    let nodes = |size: &str| find(records, "open", size).and_then(|r| r.meta.get("nodes"));
    if nodes("1k") != nodes("10k") {
        misses.push(format!(
            "{app} open: {:?} nodes at 10k, {:?} at 1k",
            nodes("10k"),
            nodes("1k")
        ));
    }

    for scenario in CHANGES {
        // A steady change: the same on screen and in the store, it loads nothing, and the
        // allocations it makes do not grow with the workspace.
        if let Some((small, large)) = both(records, scenario, Work::of_record) {
            same_shown(app, scenario, (small, large), misses);
            for ((name, at_large), (_, at_small)) in loaded(large).into_iter().zip(loaded(small)) {
                if at_large != at_small {
                    misses.push(format!(
                        "{app} {scenario}: {name} {at_large} at 10k, {at_small} at 1k: a change loads the workspace again"
                    ));
                }
            }
            // Automerge's columns are a few levels deeper in a larger record: a few percent
            // more, never in proportion to the items.
            let limit = small
                .allocations
                .saturating_add(small.allocations / 10)
                .saturating_add(64);
            if large.allocations > limit {
                misses.push(format!(
                    "{app} {scenario}: allocations {} at 10k, {} at 1k",
                    large.allocations, small.allocations
                ));
            }
        }
        // The first change, which brings a writer new to the record: the same on screen and in
        // what the runtime reads. What Automerge does to take a new actor in grows with the
        // record (its allocations are not held here).
        if matches!(*scenario, "scroll" | "thread_scroll") {
            // Scrolling writes nothing: it has no first change.
            continue;
        }
        let first = |size: &str| find(records, scenario, size).map(Work::first_of_record);
        match (first("1k"), first("10k")) {
            (Some(Some(small)), Some(Some(large))) => {
                same_shown(app, &format!("first {scenario}"), (small, large), misses);
            }
            (None, None) => {}
            _ => misses.push(format!("{app} {scenario}: the first change counts no work")),
        }
    }
}

/// The runtime does the work of what `app` shows, whatever the size of its workspace.
fn the_runtime_does_the_work_of_what_it_shows(app: &str) {
    let mut misses = Vec::new();
    gate(app, &bench(app), &mut misses);
    assert!(
        misses.is_empty(),
        "more work in a larger workspace:\n{}",
        misses.join("\n")
    );
}

#[test]
fn notes_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("notes");
}

/// Typing in a notebook runs the views that read what was typed, and no others: a key costs
/// the same from the first note to ten thousand.
#[test]
fn a_key_reruns_the_same_views_whatever_the_notebook_holds() {
    let records = bench_at("notes", vec![1, 200, LARGE], 2);
    let work = |size: &str| {
        let record = find(&records, "keystroke", size)
            .unwrap_or_else(|| panic!("the keystroke at {size} counts work"));
        Work::of_record(record).unwrap_or_else(|| panic!("the keystroke at {size} counts work"))
    };
    let (one, some, many) = (work("1"), work("200"), work("10k"));
    let mut misses = Vec::new();
    let at = |work: Work| shown(work);
    for ((name, many), (_, some), (_, one)) in at(many)
        .into_iter()
        .zip(at(some))
        .zip(at(one))
        .map(|((a, b), c)| (a, b, c))
    {
        // The views' work, and what they read. Index reads are the store deriving a change
        // into its columns, held below from 200 up: an only note is a shape of its own.
        if name == "index_fields" {
            if some != many {
                misses.push(format!("{name}: {some} at 200 notes, {many} at 10k"));
            }
            continue;
        }
        // A list's row reads the item above it to know what it continues, and the only note
        // has no row above it: one item fewer at 1 note, and the same from 200 up.
        if name == "items" {
            if some != many || one > some {
                misses.push(format!(
                    "{name}: {one} at 1 note, {some} at 200, {many} at 10k"
                ));
            }
            continue;
        }
        if one != some || one != many {
            misses.push(format!(
                "{name}: {one} at 1 note, {some} at 200, {many} at 10k"
            ));
        }
    }
    assert!(
        misses.is_empty(),
        "a key does more work in a larger notebook:\n{}",
        misses.join("\n")
    );
}

#[test]
fn tasks_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("tasks");
}

#[test]
fn today_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("today");
}

#[test]
fn chat_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("chat");
}

// Mail's root holds the workspace's accounts, and what people read is the conversations in
// one: the harness reaches them ([`Shape`]'s holders), so the gate holds them too.
#[test]
fn mail_does_the_work_of_what_it_shows() {
    the_runtime_does_the_work_of_what_it_shows("mail");
}

/// A keystroke is a change of the record the moment it is typed — that is what keeps every letter
/// and sends it to the other devices at once — and the burst of them is one step of history and
/// undo. One letter is one change, and it reads nothing of the workspace: no collection's entries,
/// no listing of it, no filter evaluated on an item.
///
/// What this scenario types into is the note's body, the document its window edits, as a person
/// writing a note does; what else a letter costs is the views that show it, which are the same at
/// 200 notes and at 2k ([`a_key_reruns_the_same_views_whatever_the_notebook_holds`]).
#[test]
fn a_keystroke_is_one_cheap_change() {
    let records = bench_at("notes", vec![SMALL / 5, LARGE / 5], 3);
    let work = |size: &str| {
        let record = find(&records, "keystroke", size)
            .unwrap_or_else(|| panic!("the keystroke at {size} counts work"));
        Work::of_record(record).unwrap_or_else(|| panic!("the keystroke at {size} counts work"))
    };
    let first = |size: &str| {
        let record = find(&records, "keystroke", size)
            .unwrap_or_else(|| panic!("the keystroke at {size} counts work"));
        Work::first_of_record(record)
            .unwrap_or_else(|| panic!("the keystroke at {size} counts work"))
    };
    let (small, large) = (work("200"), work("2k"));
    let mut misses = Vec::new();
    for (size, at, first) in [("200", small, first("200")), ("2k", large, first("2k"))] {
        // One letter is one change of the record, never one per thing it touched, and never
        // none: a letter the record does not hold is a letter the person lost.
        for (which, at) in [("a steady keystroke", at), ("the first", first)] {
            if at.commits != 1 {
                misses.push(format!(
                    "{size}: {which} committed {} changes, where a letter is one",
                    at.commits
                ));
            }
        }
        // Nothing of the workspace is read for the letter: no collection's entries listed, no
        // filter evaluated on an item, and no resource found by walking the record beyond the
        // one the change derives.
        for (name, count, most) in [
            ("located", at.located, 2),
            ("fields", at.fields, 2),
            ("listings", at.listings, 0),
            ("entries", at.entries, 0),
            ("evaluated", at.evaluated, 0),
        ] {
            if count > most {
                misses.push(format!(
                    "{size}: a keystroke read the workspace: {name} {count}"
                ));
            }
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// The files an export writes, kept as they were handed in: the address of the content is the
/// content's own, as a device's content store keeps one file once.
#[derive(Default)]
struct Kept {
    content: RefCell<HashMap<String, Vec<u8>>>,
}

impl BlobHost for Kept {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.content.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
        Err("this test chooses no files".to_owned())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
        self.content
            .borrow_mut()
            .insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

/// What one export read of the workspace, in the runtime's own counts.
#[derive(Clone, Copy, Debug, Default)]
struct Read {
    /// Resources found by walking the record, and the field values it read.
    located: u64,
    fields: u64,
    /// The items a filter was evaluated on one by one, which an index's column answers.
    evaluated: u64,
    /// References to collection items built.
    items: u64,
}

/// What the notebook's export of `n` notes reads of the workspace, from the moment it opens.
fn export_work(packages: &[Compiled], n: usize) -> Read {
    let doc = seeded_record(packages, n).unwrap_or_else(|e| panic!("{n} notes: {e}"));
    let host = Host::new(&doc, Principal::new("ada"), || None)
        .unwrap_or_else(|e| panic!("a host over {n} notes: {e}"));
    for compiled in packages {
        host.load_wasm(&compiled.wasm)
            .unwrap_or_else(|e| panic!("{} loads: {e}", compiled.meta.app));
    }
    host.runtime().set_blobs(Kept::default());
    let nb = host
        .root_of(NOTES)
        .unwrap_or_else(|e| panic!("the notebook of {n} notes: {e}"));
    let package = host
        .package(NOTES)
        .unwrap_or_else(|| panic!("the notes package"));
    let export = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == "export_notebook")
        .map_or_else(
            || panic!("the notebook's export"),
            |action| action.id.clone(),
        );
    let who = Principal::new("ada");
    let export_once = || match host
        .runtime()
        .invoke_action(&who, (export.as_str(), vec![Value::resource(&nb)]))
    {
        Ok(InvokeOutcome::Done(Value::File(file))) if file.name == "Notes.zip" => (),
        other => panic!("an export of {n} notes answered {other:?}"),
    };
    export_once();
    let after = host.store().read_stats();
    Read {
        located: after.doc.located,
        fields: after.doc.fields,
        evaluated: after.evaluated,
        items: host.runtime().items_built(),
    }
}

/// The counts of one export, in the order the gate reads them.
fn counts(read: Read) -> [(&'static str, u64); 4] {
    [
        ("located", read.located),
        ("fields", read.fields),
        ("evaluated", read.evaluated),
        ("items", read.items),
    ]
}

/// Exporting a notebook reads each of its notes once: what the export costs per note is the same
/// whether the notebook holds a thousand or four thousand, in the runtime's counts and in what
/// the index answers. Walking the whole notebook for every note instead grows with its square,
/// which is minutes for a notebook a person has been writing in for a year.
#[test]
fn exporting_a_notebook_costs_its_notes() {
    const MANY: usize = 4_000;
    let packages = compile(&apps().join("notes")).unwrap_or_else(|e| panic!("notes: {e}"));
    let small = export_work(&packages, SMALL);
    let large = export_work(&packages, MANY);
    // Each count crossed with the other notebook's size: what four thousand notes cost is four
    // times what a thousand do, and no more.
    let (at_small, at_large) = (
        u128::try_from(SMALL).unwrap_or(u128::MAX),
        u128::try_from(MANY).unwrap_or(u128::MAX),
    );
    let mut misses = Vec::new();
    for ((name, many), (_, some)) in counts(large).into_iter().zip(counts(small)) {
        if u128::from(many) * at_small > u128::from(some) * at_large {
            misses.push(format!(
                "{name}: {many} at {MANY} notes, {some} at {SMALL}: exporting reads the notebook \
                 again for each note"
            ));
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// One action that adds every item of a workspace at once, as an import of ten thousand tasks
/// does: past a few hundred items it is more than a change records whole, and keeps its large
/// parts as content. Opening such a workspace builds its index in what its items cost, and an
/// item's history says the import created it, when the import says it did.
#[test]
fn an_import_of_ten_thousand_items_reads_as_what_it_made() {
    use tilolang_runtime::store::{Store as _, VersionKind};
    const TASKS: &str = "so.tilo.lang.tasks";
    let packages = compile(&apps().join("tasks")).unwrap_or_else(|e| panic!("tasks: {e}"));
    let open = |n: usize, doc: tilo_resource::Doc| {
        let start = std::time::Instant::now();
        let host = Host::new(&doc, Principal::new("ada"), || None)
            .unwrap_or_else(|e| panic!("a host over {n} tasks: {e}"));
        for compiled in &packages {
            host.load_wasm(&compiled.wasm)
                .unwrap_or_else(|e| panic!("{} loads: {e}", compiled.meta.app));
        }
        host.wait_index();
        assert!(host.index_ready(), "the index of {n} tasks is built");
        let built = start.elapsed();
        let root = host
            .root_of(TASKS)
            .unwrap_or_else(|e| panic!("the root: {e}"));
        let items = match host.runtime().read(&root, "items") {
            Ok(Value::List(items)) => items,
            other => panic!("the tasks are a list: {other:?}"),
        };
        assert_eq!(items.len(), n, "every task is there");
        let item = match items.get(n / 2) {
            Some(Value::Ref(item)) => item.target.clone(),
            other => panic!("a task is a reference: {other:?}"),
        };
        let versions = host
            .store()
            .versions(&item)
            .unwrap_or_else(|e| panic!("the versions of a task: {e}"));
        (built, versions)
    };
    let imported = |n: usize| {
        let doc = tilolang_tilo::bench::seeded_record_in_one_action(&packages, n)
            .unwrap_or_else(|e| panic!("{n} tasks: {e}"));
        open(n, doc)
    };
    let (small, at_small) = imported(SMALL);
    let (large, at_large) = imported(LARGE);
    for (n, versions) in [(SMALL, &at_small), (LARGE, &at_large)] {
        let first = versions
            .first()
            .unwrap_or_else(|| panic!("at {n}, the task has a version: the import made it"));
        assert_eq!(
            (first.kind, first.action.as_str(), first.at.as_str()),
            (VersionKind::Created, "add", "2026-06-29T09:00:00Z"),
            "at {n}, the task was created by the import, when it says: {versions:?}"
        );
        assert_eq!(
            versions.len(),
            1,
            "at {n}, the import is all it holds: {versions:?}"
        );
    }
    // Ten times the items cost about ten times as much, never their square.
    assert!(
        large < small.saturating_mul(20),
        "the index of {LARGE} imported tasks took {large:?}, of {SMALL} {small:?}"
    );
}
