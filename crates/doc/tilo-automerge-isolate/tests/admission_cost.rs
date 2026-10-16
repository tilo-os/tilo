//! What admitting untrusted Automerge bytes costs, split into the worker's
//! part and the caller's own decode after it.
//!
//! The caller decodes only once a worker has accepted the same bytes, so an
//! admission costs the worker's round trip plus that second decode. This
//! measures both, for a one-keystroke change into a document with a long
//! history and for loading a whole document, and prints one line per case.
//! A plain test run keeps the cases small; `TILO_ADMISSION_COST=full`
//! measures keystrokes into 1,000, 5,000 and 20,000 changes of history and
//! loads of about 1 MB and 10 MB:
//!
//! ```text
//! TILO_ADMISSION_COST=full cargo test --release --locked -p tilo-automerge-isolate \
//!     --test admission_cost -- --nocapture
//! ```
//!
//! Without `--release` the numbers are not the shipped ones: the test
//! profile keeps Automerge's debug assertions, one of which walks every op
//! of the document after each apply.
//!
//! Workers exist only on Linux; elsewhere admission is a decode in the page
//! and there is nothing to split.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::arithmetic_side_effects,
    reason = "a measurement harness: these are the assertion and its report"
)]

use std::time::{Duration, Instant};

use automerge::transaction::Transactable as _;
use automerge::{
    ActorId, AutoCommit, LoadOptions, ObjId, ObjType, OnPartialLoad, ROOT, ReadDoc as _,
    VerificationMode,
};
use tilo_automerge::ApplyIncoming as _;
use tilo_automerge_isolate::{Applied, Dependencies, Isolate, Limits};

tilo_automerge_isolate::test_worker!();

/// How much to measure.
struct Scale {
    /// Changes of history each keystroke document has.
    histories: &'static [usize],
    /// Keystrokes timed per document.
    keystrokes: usize,
    /// Documents to load: a label, their changes and letters per change.
    documents: &'static [(&'static str, usize, usize)],
    /// Loads timed per document.
    loads: usize,
}

const SMALL: Scale = Scale {
    histories: &[1_000],
    keystrokes: 40,
    documents: &[("small", 800, 120)],
    loads: 3,
};

const FULL: Scale = Scale {
    histories: &[1_000, 5_000, 20_000],
    keystrokes: 200,
    documents: &[("1MB", 4_000, 300), ("10MB", 20_000, 600)],
    loads: 5,
};

/// A deterministic stream of lowercase letters, so every run builds the same
/// documents.
struct Letters(u64);

impl Letters {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn word(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| char::from(b'a' + u8::try_from(self.step() % 26).unwrap()))
            .collect()
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.step()).unwrap() % bound.max(1)
    }
}

/// A text written by two collaborators in `changes` changes of `per_change`
/// letters each, at scattered positions.
fn history(changes: usize, per_change: usize) -> (AutoCommit, ObjId) {
    let mut letters = Letters(0x9e37_79b9_7f4a_7c15);
    let mut ada = AutoCommit::new().with_actor(ActorId::from(vec![0xa1; 16]));
    let text = ada.put_object(ROOT, "body", ObjType::Text).unwrap();
    ada.commit();
    let mut bo = ada.fork().with_actor(ActorId::from(vec![0xb2; 16]));
    for i in 0..changes {
        let writer = if i % 2 == 0 { &mut ada } else { &mut bo };
        let at = letters.below(writer.length(&text) + 1);
        writer
            .splice_text(&text, at, 0, &letters.word(per_change))
            .unwrap();
        writer.commit();
        if i % 64 == 63 {
            ada.merge_incoming(&mut bo).unwrap();
            bo.merge_incoming(&mut ada).unwrap();
        }
    }
    ada.merge_incoming(&mut bo).unwrap();
    (ada, text)
}

fn strict() -> LoadOptions<'static> {
    LoadOptions::new()
        .on_partial_load(OnPartialLoad::Error)
        .verification_mode(VerificationMode::Check)
}

fn micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e6
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

/// Nearest-rank percentile.
fn percentile(samples: &mut [Duration], p: usize) -> Duration {
    samples.sort_unstable();
    let rank = (samples.len() * p).div_ceil(100).max(1);
    samples[rank - 1]
}

fn isolate() -> Isolate {
    let isolate = Isolate::new(Limits {
        workers: 1,
        ..Limits::default()
    });
    // Start the worker, so no case pays its startup.
    let mut warm = AutoCommit::new();
    warm.put(ROOT, "warm", 1).unwrap();
    isolate.load(&warm.save()).unwrap();
    isolate
}

fn keystroke_cost(isolate: &Isolate, changes: usize, keystrokes: usize) {
    let (mut base, text) = history(changes, 4);
    // The caller's document, admitted as a peer's would be.
    let mut doc = isolate.load(&base.save()).unwrap();
    // The same document, taking the same keystrokes without a worker.
    let mut mirror = doc.fork().with_actor(doc.get_actor().clone());
    let mut remote = base.fork().with_actor(ActorId::from(vec![0xc3; 16]));
    let mut letters = Letters(u64::try_from(changes).unwrap() | 1);
    let mut total = Vec::with_capacity(keystrokes);
    let mut caller = Vec::with_capacity(keystrokes);
    for _ in 0..keystrokes {
        let before = remote.get_heads();
        let at = letters.below(remote.length(&text) + 1);
        remote.splice_text(&text, at, 0, &letters.word(1)).unwrap();
        remote.commit();
        let change = remote.save_after(&before);

        let started = Instant::now();
        let applied = isolate.apply(&mut doc, &change, Dependencies::RequireComplete);
        total.push(started.elapsed());
        assert_eq!(applied, Ok(Applied::Changed(true)));

        let started = Instant::now();
        mirror.load_incoming(&change).unwrap();
        caller.push(started.elapsed());
    }
    assert_eq!(doc.get_heads(), remote.get_heads());
    assert_eq!(mirror.get_heads(), remote.get_heads());
    let total_p50 = percentile(&mut total, 50);
    let caller_p50 = percentile(&mut caller, 50);
    eprintln!(
        "admission_cost keystroke changes={changes} total_p50_us={:.1} total_p95_us={:.1} \
         caller_p50_us={:.1} caller_p95_us={:.1} worker_p50_us={:.1} caller_share={:.3}",
        micros(total_p50),
        micros(percentile(&mut total, 95)),
        micros(caller_p50),
        micros(percentile(&mut caller, 95)),
        micros(total_p50.saturating_sub(caller_p50)),
        caller_p50.as_secs_f64() / total_p50.as_secs_f64(),
    );
}

fn load_cost(isolate: &Isolate, (label, changes, per_change): (&str, usize, usize), loads: usize) {
    let (mut doc, _) = history(changes, per_change);
    let saved = doc.save();
    let hashes = AutoCommit::load(&saved)
        .unwrap()
        .document()
        .change_hashes()
        .to_vec();
    let mut total = Vec::with_capacity(loads);
    let mut strict_load = Vec::with_capacity(loads);
    let mut known_load = Vec::with_capacity(loads);
    for _ in 0..loads {
        let started = Instant::now();
        let mut admitted = isolate.load(&saved).unwrap();
        total.push(started.elapsed());
        assert_eq!(admitted.get_heads(), doc.get_heads());

        let started = Instant::now();
        let mut loaded = AutoCommit::load_with_options(&saved, strict()).unwrap();
        strict_load.push(started.elapsed());
        assert_eq!(loaded.get_heads(), doc.get_heads());

        let started = Instant::now();
        let mut loaded =
            AutoCommit::load_with_options(&saved, strict().known_change_hashes(&hashes)).unwrap();
        known_load.push(started.elapsed());
        assert_eq!(loaded.get_heads(), doc.get_heads());
    }
    let total_p50 = percentile(&mut total, 50);
    let strict_p50 = percentile(&mut strict_load, 50);
    let known_p50 = percentile(&mut known_load, 50);
    eprintln!(
        "admission_cost load size={label} bytes={} changes={changes} total_p50_ms={:.2} \
         full_load_p50_ms={:.2} known_hashes_load_p50_ms={:.2} min_total_ms={:.2}",
        saved.len(),
        millis(total_p50),
        millis(strict_p50),
        millis(known_p50),
        millis(total[0]),
    );
}

#[test]
fn admission_cost() {
    let scale = if std::env::var_os("TILO_ADMISSION_COST").is_some_and(|value| value == "full") {
        &FULL
    } else {
        &SMALL
    };
    let isolate = isolate();
    for &changes in scale.histories {
        keystroke_cost(&isolate, changes, scale.keystrokes);
    }
    for &document in scale.documents {
        load_cost(&isolate, document, scale.loads);
    }
}
