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
}

/// Installs a hook recording `message @ file:line` of the last panic on this
/// thread (with AM012_LOUD, plus the vendored backtrace frames). Returns the previous hook.
fn record_panics() -> Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static> {
    let previous = std::panic::take_hook();
    let loud = std::env::var_os("AM012_LOUD").is_some();
    std::panic::set_hook(Box::new(move |info| {
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
    previous
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
        let result = catch_unwind(AssertUnwindSafe(|| match path {
            Path::Checked => drop(checked(input)),
            Path::Load => drop(AutoCommit::load(input)),
            Path::Incremental => drop(seeded_doc().load_incremental(input)),
        }));
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

static PROGRESS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CURRENT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A mutant that makes a load path spin is a CPU DoS: report it and stop.
fn watchdog() {
    use std::sync::atomic::Ordering;
    std::thread::spawn(|| {
        let mut last = usize::MAX;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            let now = PROGRESS.load(Ordering::Relaxed);
            if now == last {
                eprintln!("HANG (>30s on one mutant): {}", CURRENT.lock().unwrap());
                std::process::exit(2);
            }
            last = now;
        }
    });
}

#[test]
fn mutation_sweep_never_panics_on_any_load_path() {
    let started = std::time::Instant::now();
    watchdog();
    let previous = record_panics();
    let mut failures: Vec<(String, Path, String, String)> = Vec::new();
    let mut total = 0usize;
    for (kind, raw) in seeds() {
        for (what, m) in mutants(body(&raw)) {
            total += 1;
            let input = chunk(kind, &m);
            *CURRENT.lock().unwrap() = format!("kind {kind} {what}: {}", hex::encode(&input));
            PROGRESS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let t = std::time::Instant::now();
            for (path, msg) in panics(&input) {
                failures.push((format!("kind {kind} {what}"), path, msg, hex::encode(&input)));
            }
            if t.elapsed() > std::time::Duration::from_secs(2) {
                println!("SLOW {:?}: kind {kind} {what}: {}", t.elapsed(), hex::encode(&input));
            }
        }
    }
    std::panic::set_hook(previous);
    println!("mutation sweep: {total} mutants x 3 paths in {:?}, {} panics", started.elapsed(), failures.len());
    let mut groups: Vec<(String, usize, String)> = Vec::new();
    for (site, path, msg, input) in &failures {
        // Digits masked in the message (ids, lengths), kept in the location.
        let (text, at) = msg.rsplit_once(" @ ").unwrap_or((msg, ""));
        let key = format!("{path:?}: {} @ {at}", text.replace(|c: char| c.is_ascii_digit(), "#"));
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1 += 1,
            None => groups.push((key, 1, format!("{site}: (\"{site}\", \"{input}\"),"))),
        }
    }
    let report: Vec<String> = groups.iter().map(|(k, n, e)| format!("{n:6} x {k}\n         e.g. {e}")).collect();
    assert!(failures.is_empty(), "{} panics:\n{}", failures.len(), report.join("\n"));
}

mod regressions {
    use super::*;

    /// `(what, hex input)`: crashing inputs found by the sweeps (after L5's
    /// `sweep_regressions`), each must now load or be refused, never panic.
    const CASES: &[(&str, &str)] = &[];

    #[test]
    fn crashing_inputs_found_by_the_sweeps_no_longer_panic() {
        let previous = record_panics();
        let mut failures = Vec::new();
        for (what, hex_input) in CASES {
            let input = hex::decode(hex_input).unwrap();
            for (path, msg) in panics(&input) {
                failures.push(format!("{what}: {path:?}: {msg}"));
            }
        }
        std::panic::set_hook(previous);
        assert!(failures.is_empty(), "{} of {}:\n{}", failures.len(), CASES.len(), failures.join("\n"));
    }
}

/// Debug helper: `AM012_ONE=<hex> cargo test ... one_input -- --nocapture`
/// runs a single input through every load path. No-op without the variable.
#[test]
fn one_input() {
    let Some(hex_input) = std::env::var_os("AM012_ONE") else { return };
    let input = hex::decode(hex_input.to_str().unwrap()).unwrap();
    let previous = record_panics();
    let found = panics(&input);
    std::panic::set_hook(previous);
    println!("{found:?}");
}
