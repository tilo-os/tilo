//! Load-path panic hardening for the vendored automerge 0.12 (lane L6).
//!
//! Release builds use `panic = "abort"`, so a panic on hostile storage is a
//! remote crash of the host, not a recoverable error. These tests feed
//! mutated storage (checksum recomputed, so every mutation reaches the
//! decoders) to plain `AutoCommit::load`, to `load_incremental` into a
//! resident document, and to the checked pipeline
//! (`normalize_input -> reconstruct`), and require that none of them panic.
//!
//! - `regressions`: every distinct crashing input the sweeps found, fixed.
//! - `sweep`: single-byte, two-byte, insert and delete mutations of a few
//!   valid documents, change chunks and bundles. Full value sets and all
//!   byte pairs behind `AM012_SLOW=1`; `AM012_LOUD=1` prints each panic.
#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "adversarial test harness in the vendored crate's style"
)]

use automerge::marks::{ExpandMark, Mark};
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance, NativeLimits};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, Change, ObjType, ScalarValue, ROOT};
use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];

fn leb(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 128);
    }
}

fn read_uint(bytes: &mut &[u8]) -> u64 {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let b = bytes[0];
        *bytes = &bytes[1..];
        result |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return result;
        }
        shift += 7;
    }
}

/// Frame `body` as a chunk of `kind` with a genuine checksum.
fn chunk(kind: u8, body: &[u8]) -> Vec<u8> {
    use sha2::Digest;
    let mut len = Vec::new();
    leb(body.len() as u64, &mut len);
    let mut hash = sha2::Sha256::new();
    hash.update([kind]);
    hash.update(&len);
    hash.update(body);
    let digest = hash.finalize();
    let mut out = MAGIC.to_vec();
    out.extend(&digest[..4]);
    out.push(kind);
    out.extend(len);
    out.extend(body);
    out
}

/// Body of a single chunk (after magic, checksum, kind and length).
fn body(raw: &[u8]) -> &[u8] {
    let mut b = &raw[9..];
    let _ = read_uint(&mut b);
    b
}

thread_local! {
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Set while a load path runs under `catch_unwind` in `panics`: only
    /// those panics are recorded silently; the rest (the tests' own
    /// assertions) go to the default hook.
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs (once per process; tests run in parallel) a hook recording
/// `message @ file:line` of the last panic on the current thread, with
/// AM012_LOUD also the vendored backtrace frames. The hook stays installed:
/// restoring the previous one would race with the other tests' panics.
fn record_panics() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let loud = std::env::var_os("AM012_LOUD").is_some();
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !PROBING.with(|p| p.get()) {
                return default(info);
            }
            let msg = info
                .payload()
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| info.payload().downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
            let mut full = format!("{} @ {at}", msg.chars().take(120).collect::<String>());
            if loud {
                // Keep only the vendored frames: enough to find the caller.
                let trace = std::backtrace::Backtrace::force_capture().to_string();
                for frame in trace.lines().filter(|l| l.contains("automerge::") || l.contains("hexane::")).take(12) {
                    full.push_str("\n    ");
                    full.push_str(frame.trim());
                }
            }
            LAST_PANIC.with(|p| *p.borrow_mut() = Some(full));
        }));
    });
}

fn last_panic() -> String {
    LAST_PANIC.with(|p| p.borrow_mut().take()).unwrap_or_default()
}

fn seeded_doc() -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0x51; 16]));
    doc.put(ROOT, "seed", "resident").unwrap();
    let list = doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.insert(&list, 0, 1_i64).unwrap();
    doc.commit();
    doc
}

/// The whole checked pipeline on a fresh owner.
fn checked(input: &[u8]) -> bool {
    let credit = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = NativeAllocationSet::new(credit.clone()).unwrap();
    let turn = registry.work_turn(credit).unwrap();
    turn.normalize_input(input, &NativeLimits::default())
        .and_then(|n| n.reconstruct(&mut registry, &turn))
        .is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path {
    Load,
    Incremental,
    Checked,
}

/// Runs `input` through every load path; returns the panics as
/// `(path, "message @ location")`.
fn panics(input: &[u8]) -> Vec<(Path, String)> {
    let mut out = Vec::new();
    for path in [Path::Checked, Path::Load, Path::Incremental] {
        if std::env::var_os("AM012_TRACE").is_some() {
            eprintln!("path {path:?}");
        }
        PROBING.with(|p| p.set(true));
        let result = catch_unwind(AssertUnwindSafe(|| match path {
            Path::Checked => drop(checked(input)),
            Path::Load => drop(AutoCommit::load(input)),
            Path::Incremental => drop(seeded_doc().load_incremental(input)),
        }));
        PROBING.with(|p| p.set(false));
        if result.is_err() {
            out.push((path, last_panic()));
        }
    }
    out
}

/// `(kind, raw chunk)` seeds: documents, change chunks and bundles.
fn seeds() -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();

    // L5's sweep seed: text with a mark, a counter in a list, a map key.
    let mut src = AutoCommit::new().with_actor(ActorId::from(vec![0xd1; 16]));
    let text = src.put_object(ROOT, "t", ObjType::Text).unwrap();
    src.splice_text(&text, 0, 0, "hello").unwrap();
    src.mark(&text, Mark::new("b".into(), true, 0, 3), ExpandMark::After).unwrap();
    let list = src.put_object(ROOT, "l", ObjType::List).unwrap();
    src.insert(&list, 0, ScalarValue::counter(1)).unwrap();
    src.commit();
    src.put(ROOT, "k", "v").unwrap();
    src.increment(&list, 0, 2).unwrap();
    src.delete(&text, 1).unwrap();
    src.commit();
    let changes = src.get_changes(&[]);
    let hashes: Vec<_> = changes.iter().map(Change::hash).collect();
    out.push((0, src.save_nocompress()));
    out.push((1, changes[1].raw_bytes().to_vec()));
    out.push((3, src.bundle(hashes).unwrap().bytes().to_vec()));

    // Two actors, concurrent edits merged: map/table/list nesting, a
    // conflict, deletes and a map counter, so the doc has several actors,
    // deps with more than one parent and succ/pred across actors.
    let mut a = AutoCommit::new().with_actor(ActorId::from(vec![0xa1; 16]));
    let map = a.put_object(ROOT, "m", ObjType::Map).unwrap();
    a.put(&map, "x", 1_i64).unwrap();
    a.put(ROOT, "c", ScalarValue::counter(5)).unwrap();
    let tbl = a.put_object(ROOT, "tbl", ObjType::Table).unwrap();
    a.put(&tbl, "row", "r").unwrap();
    let items = a.put_object(ROOT, "items", ObjType::List).unwrap();
    a.insert(&items, 0, "p").unwrap();
    a.insert(&items, 1, "q").unwrap();
    a.commit();
    let mut b = a.fork().with_actor(ActorId::from(vec![0xb2; 16]));
    a.put(&map, "x", 2_i64).unwrap();
    a.delete(&items, 0).unwrap();
    a.commit();
    b.put(&map, "x", 3_i64).unwrap();
    b.increment(ROOT, "c", 4).unwrap();
    let nested = b.insert_object(&items, 1, ObjType::Map).unwrap();
    b.put(&nested, "deep", true).unwrap();
    b.commit();
    a.merge(&mut b).unwrap();
    a.put(ROOT, "after", 1.5_f64).unwrap();
    a.commit();
    let changes = a.get_changes(&[]);
    let hashes: Vec<_> = changes.iter().map(Change::hash).collect();
    out.push((0, a.save_nocompress()));
    out.push((1, changes.last().unwrap().raw_bytes().to_vec()));
    out.push((3, a.bundle(hashes).unwrap().bytes().to_vec()));

    // A document large enough that `save()` DEFLATEs its columns, so the
    // inflate paths (plain load and normalize_input) see mutated streams.
    let mut c = AutoCommit::new().with_actor(ActorId::from(vec![0xc4; 16]));
    let text = c.put_object(ROOT, "long", ObjType::Text).unwrap();
    for i in 0..40 {
        c.splice_text(&text, 0, 0, &format!("line {i} of a text long enough to compress; ")).unwrap();
        c.commit();
    }
    let saved = c.save();
    assert!(saved.len() < c.save_nocompress().len(), "seed is not compressed");
    out.push((0, saved));
    out
}

fn slow() -> bool {
    std::env::var_os("AM012_SLOW").is_some()
}

/// A mutant: description plus the new chunk body.
type Mutant = (String, Vec<u8>);

fn mutants(body: &[u8]) -> Vec<Mutant> {
    let mut out = Vec::new();
    let values: &[(&str, fn(u8) -> u8)] = if slow() {
        &[
            ("^1", |b| b ^ 1),
            ("^80", |b| b ^ 0x80),
            ("=0", |_| 0),
            ("=ff", |_| 0xff),
            ("+1", |b| b.wrapping_add(1)),
            ("-1", |b| b.wrapping_sub(1)),
            ("^40", |b| b ^ 0x40),
            ("=7f", |_| 0x7f),
            ("^2", |b| b ^ 2),
        ]
    } else {
        &[("^1", |b| b ^ 1), ("^80", |b| b ^ 0x80), ("+1", |b| b.wrapping_add(1)), ("-1", |b| b.wrapping_sub(1))]
    };
    for i in 0..body.len() {
        for (name, f) in values {
            let mut m = body.to_vec();
            m[i] = f(m[i]);
            if m[i] != body[i] {
                out.push((format!("byte {i} {name}"), m));
            }
        }
        // Delete a byte / insert one (shifts every later column).
        let mut m = body.to_vec();
        m.remove(i);
        out.push((format!("delete {i}"), m));
        for v in if slow() { &[0u8, 1, 0x7f, 0x80, 0xff][..] } else { &[0u8, 0x80][..] } {
            let mut m = body.to_vec();
            m.insert(i, *v);
            out.push((format!("insert {i} {v:#x}"), m));
        }
        // Two-byte mutations: the adjacent pair always, every pair when slow
        // (each byte +1: keeps LEB/RLE framing plausible while moving two
        // fields at once).
        let partners: Vec<usize> =
            if slow() { (i + 1..body.len()).collect() } else { (i + 1..body.len().min(i + 2)).collect() };
        for j in partners {
            let mut m = body.to_vec();
            m[i] = m[i].wrapping_add(1);
            m[j] = m[j].wrapping_add(1);
            out.push((format!("pair {i},{j} +1"), m));
        }
    }
    out
}

/// Per-case deadline: a load that spins is a CPU DoS, and would hang the
/// test forever. Cases register here; a watchdog thread reports any case
/// running longer than [`DEADLINE`] and exits the test process.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
static RUNNING: std::sync::Mutex<Vec<(std::thread::ThreadId, String, std::time::Instant)>> =
    std::sync::Mutex::new(Vec::new());

fn watched<T>(label: impl FnOnce() -> String, f: impl FnOnce() -> T) -> T {
    static WATCHDOG: std::sync::Once = std::sync::Once::new();
    WATCHDOG.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let running = RUNNING.lock().unwrap();
            if let Some((_, what, _)) = running.iter().find(|(_, _, since)| since.elapsed() > DEADLINE) {
                eprintln!("HANG (a load path ran > {DEADLINE:?}): {what}");
                std::process::exit(2);
            }
        });
    });
    let me = std::thread::current().id();
    RUNNING.lock().unwrap().push((me, label(), std::time::Instant::now()));
    let out = f();
    RUNNING.lock().unwrap().retain(|(id, _, _)| *id != me);
    out
}

#[test]
fn mutation_sweep_never_panics_on_any_load_path() {
    let started = std::time::Instant::now();
    record_panics();
    let mut failures: Vec<(String, Path, String, String)> = Vec::new();
    let mut total = 0usize;
    for (kind, raw) in seeds() {
        for (what, m) in mutants(body(&raw)) {
            total += 1;
            let input = chunk(kind, &m);
            let found = watched(|| format!("kind {kind} {what}: {}", hex::encode(&input)), || panics(&input));
            for (path, msg) in found {
                failures.push((format!("kind {kind} {what}"), path, msg, hex::encode(&input)));
            }
        }
    }
    println!("mutation sweep: {total} mutants x 3 paths in {:?}, {} panics", started.elapsed(), failures.len());
    let mut groups: Vec<(String, usize, String)> = Vec::new();
    for (site, path, msg, input) in &failures {
        // Digits masked in the message (ids, lengths), kept in the location.
        let (text, at) = msg.rsplit_once(" @ ").unwrap_or((msg, ""));
        let key = format!("{path:?}: {} @ {at}", text.replace(|c: char| c.is_ascii_digit(), "#"));
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1 += 1,
            None => groups.push((key, 1, format!("(\"{site}\", \"{input}\"),"))),
        }
    }
    let report: Vec<String> = groups.iter().map(|(k, n, e)| format!("{n:6} x {k}\n         e.g. {e}")).collect();
    assert!(failures.is_empty(), "{} panics:\n{}", failures.len(), report.join("\n"));
}

mod regressions {
    use super::*;

    /// `(what: root cause -> fix, hex input)`: one crashing (or hanging)
    /// input per distinct root cause the sweeps found beyond L5's
    /// `sweep_regressions`. Each must now load or be refused, never panic,
    /// on every load path.
    const CASES: &[(&str, &str)] = &[
        (
            "kind 3 byte 144 ^80: truncated boolean run count: hexane BoolDecoder::next spun forever (plain load hang) -> decoder ends the column; bundle framing validator refuses it",
            "856f4a834eed0e0803df01000110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020002017e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105817c0102040702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
        ),
        (
            "kind 0 insert 87 0x80: document value lengths exceed the value column: hexane RawColumnIter::take past end (op_iter.rs) -> Columns::load value-length check",
            "856f4a83ff6b7be500f1010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f805708800109810102830103940103a50107020002017e0a0302007e00017f000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
        ),
        (
            "kind 0 byte 104 ^80: truncated varint in a change-graph actor column: hexane RleValue::unpack unwrap (lib.rs) -> decoder ends the column",
            "856f4a833d52effc00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107820002017e0a0302007e00017f000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
        ),
        (
            "kind 0 pair 115,116 +1: truncated null count: hexane RleDecoder::advance_run unwrap (rle/decoder.rs) -> decoder ends the column",
            "856f4a83a004450d00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e000280000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
        ),
        (
            "kind 0 byte 123 ^1: op actor index past the actor table: ActorMapper::process_actor OOB (op_set2/change.rs) -> collector InvalidOp + Columns::load actor bounds",
            "856f4a833d51cb7a00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000207000309010003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
        ),
        (
            "kind 0 pair 181,182 +1: non-numeric increment in a document: Verified change iter_ops unwrap (storage/change.rs) -> collector InvalidOp",
            "856f4a835de5e29a00f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000207000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0103050703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
        ),
        (
            "kind 3 pair 147,148 +1: non-numeric increment in a bundle: Verified change iter_ops unwrap (storage/change.rs) -> collector InvalidOp",
            "856f4a830ec77b6303df01000110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020002017e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105017c0103050702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
        ),
        (
            "kind 0 pair 132,133 +1: null run in a non-nullable change-graph column: hexane get_null panic -> decoder ends the column",
            "856f4a83a9cba9c000ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f010401800002017f02020002010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103",
        ),
        (
            "kind 0 pair 139,140 +1: dep index past the change list: change_graph.rs ChangeGraphCols::load max_ops[dep] OOB -> guard",
            "856f4a8398a104ba00ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f0104007f0002017f02030102010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103",
        ),
        (
            "kind 0 byte 140 ^80: dep index 256 past the change list: same, larger index",
            "856f4a83e18fbf9900ba020210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2014707c716356b85249314a37e0cdff282bd05e090ed2b18595c18a1969a410cbf0701050305130623024006430456020e0106020a110613081527210e230f3403420d560f571080010d81010583010502007e010002017e7f027f0802027f0104007f0002017f02028002010407000607007f01000603017f0403067f0b000b02000001000a7d00070000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700037f046465657002007f0105007f0102007d010001720d76077c7b037e07007c02047d040a030102017c0502000605017f0002017d850118140300031402167d001602000000000000f83f05040102037270717e000104007f0203007f0103007c010001007c0a7f000103",
        ),
        (
            "kind 0 byte 119 ^40: extra-bytes lengths exceed the extra column: change_graph.rs change-meta slice OOB -> ChangeGraphCols::load check",
            "856f4a83b42dd7f000f0010110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1011f0242f07c77fbf3d3aac425927bb392f9d579080452645fdb50bf7e4097564b07010203021303230240034302560210010402061108130d15092102230e3403420d560f5708800109810102830103940103a50107020002017e0a0302007e00017f000247000309000003070102090005050000017f00000302007f0202017c00017b0a7d016b016c017400090c007b0b7e78067b02017b047d0104020308017c0102040703017f0703017f057f1602007f0203167f0002167e18147668656c6c6f010205007f0104007e010002007e0d7f07010400037f0162000801",
        ),
    ];

    /// Batch-apply seq-contiguity panics on the PLAIN load paths (the checked
    /// path refuses them in L2's validator). The guard is L2's (batch.rs);
    /// un-ignore once it is merged.
    const PENDING_BATCH_SEQ_GUARD: &[(&str, &str)] = &[
        (
            "kind 3 pair 40,41 +1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert) -> L2 seq pre-check",
            "856f4a83e50603e203df01000110d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020003027e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105017c0102040702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
        ),
        (
            "kind 3 byte 64 ^1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert) -> L2 seq pre-check",
            "856f4a83960a9a3f03aa02000210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b20901050305130523063302450250065304600202007e010002017e7f037c010800047f0802027f01040000047f0002017f020200020104000e0106030d110613091527210e3405420e56105710700e71027305b30111000608007f0100067f0102007e030203007f05000b03000001000a7e0007020000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700047f046465657002007f0105007f0103007d0100010a0101020102017c0502000605017e030002017d8501181403000314021602007e1602000000000000f83f050401020372707102007f010400020102007f01030004007c037f00057d04027b0204767a07037a0475087a0a02",
        ),
        (
            "kind 3 pair 3,41 +1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert) -> L2 seq pre-check",
            "856f4a83578227cc03df01000110d2d1d1d1d1d1d1d1d1d1d1d1d1d1d1d109010203021303230333024502500353026002020002027e010a7e0a03020000027e00017f00020010010403091108130d150921023405420f56115708700871027303940103a50107b3010f00030a0000037f0107007e08000005060000017f0000030200780201000100017b0a7d016b016c0174000a0d0003030105017c0102040702017d03010703017f057f1602007f0202167d00160002167e18147668656c6c6f010206007f0105007f0102007e030708010400037f0162000902027f01020278017905790a750c7a",
        ),
        (
            "kind 3 pair 3,64 +1: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert) -> L2 seq pre-check",
            "856f4a83343c8b3403aa02000210a2a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b20901050305130523063302450250065304600202007e010002017e7f037c010800047f0802027f01040000047f0002017f020200020104000e0106030d110613091527210e3405420e56105710700e71027305b30111000608007f0100067f0102007e030203007f05000b03000001000a7e0007020000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700047f046465657002007f0105007f0103007d0100010a0101020102017c0502000605017e030002017d8501181403000314021602007e1602000000000000f83f050401020372707102007f010400020102007f01030004007c037f00057d04027b0204767a07037a0475087a0a02",
        ),
        (
            "kind 3 byte 64 ^40: seq gap / unknown actor with seq != 1 in batch apply (batch.rs lookup_actor unwrap, change_graph.rs:571 assert) -> L2 seq pre-check",
            "856f4a83aa519aed03aa02000210a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a110b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b20901050305130523063302450250065304600202007e010002017e7f427c010800047f0802027f01040000047f0002017f020200020104000e0106030d110613091527210e3405420e56105710700e71027305b30111000608007f0100067f0102007e030203007f05000b03000001000a7e0007020000017f0561667465720201637d056974656d73016d0374626c0301787f03726f7700047f046465657002007f0105007f0103007d0100010a0101020102017c0502000605017e030002017d8501181403000314021602007e1602000000000000f83f050401020372707102007f010400020102007f01030004007c037f00057d04027b0204767a07037a0475087a0a02",
        ),
    ];

    fn run(cases: &[(&str, &str)]) {
        record_panics();
        let mut failures = Vec::new();
        for (what, hex_input) in cases {
            let input = hex::decode(hex_input).unwrap();
            for (path, msg) in watched(|| what.to_string(), || panics(&input)) {
                failures.push(format!("{what}: {path:?}: {msg}"));
            }
        }
            assert!(failures.is_empty(), "{} of {}:\n{}", failures.len(), cases.len(), failures.join("\n"));
    }

    #[test]
    fn crashing_inputs_found_by_the_sweeps_no_longer_panic() {
        run(CASES);
    }

    #[test]
    #[ignore = "needs L2's batch seq-contiguity guard (codex/am012-l2)"]
    fn batch_seq_gaps_no_longer_panic_on_plain_load() {
        run(PENDING_BATCH_SEQ_GUARD);
    }
}

/// Debug helper: `AM012_ONE=<hex> cargo test ... one_input -- --nocapture`
/// runs a single input through every load path. No-op without the variable.
#[test]
fn one_input() {
    let Some(hex_input) = std::env::var_os("AM012_ONE") else { return };
    let input = hex::decode(hex_input.to_str().unwrap()).unwrap();
    record_panics();
    let found = panics(&input);
    println!("{found:?}");
}
