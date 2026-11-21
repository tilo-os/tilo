//! A caller with a small request is not kept waiting behind another caller's
//! large ones: while one caller keeps every worker it can busy with whole
//! documents to rebuild, as a workspace restoring its parts does, a small
//! load from another caller waits about as long as it takes alone.
//!
//! Workers exist only on Linux.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::arithmetic_side_effects,
    reason = "a timing test: these are the assertion and its report"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc as _};
use tilo_automerge_isolate::{Isolate, Limits};

/// Requests the flooding caller keeps in flight at once.
const FLOOD: usize = 4;
const SAMPLES: usize = 9;

fn large() -> Vec<u8> {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![0xa1; 16]));
    let text = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
    doc.commit();
    for _ in 0..300 {
        let words: String = (0..1_000)
            .map(|_| char::from(b'a' + u8::try_from(next() % 26).unwrap()))
            .collect();
        let end = doc.length(&text);
        doc.splice_text(&text, end, 0, &words).unwrap();
        doc.commit();
    }
    doc.save()
}

fn small() -> Vec<u8> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "a few bytes").unwrap();
    doc.commit();
    doc.save()
}

fn median(isolate: &Isolate, bytes: &[u8]) -> Duration {
    let mut samples: Vec<Duration> = (0..SAMPLES)
        .map(|_| {
            let started = Instant::now();
            isolate.load(bytes).unwrap();
            started.elapsed()
        })
        .collect();
    samples.sort_unstable();
    samples[SAMPLES / 2]
}

#[test]
fn a_small_load_is_not_kept_behind_another_callers_large_ones() {
    let isolate = Arc::new(Isolate::new(Limits {
        workers: 2,
        timeout: tilo_test_env::hang_bound(),
        ..Limits::default()
    }));
    let (large, small) = (Arc::new(large()), small());

    // Start both workers, so no measurement pays a worker's start.
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| isolate.load(&small).unwrap());
        }
    });
    let alone = median(&isolate, &small);
    let started = Instant::now();
    isolate.load(&large).unwrap();
    let large_alone = started.elapsed();
    assert!(
        large_alone > alone * 20 && large_alone > Duration::from_millis(100),
        "the large document must cost far more than the small one: {large_alone:?} against {alone:?}"
    );

    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let flood: Vec<_> = (0..FLOOD)
        .map(|_| {
            let (isolate, large, stop, done) =
                (isolate.clone(), large.clone(), stop.clone(), done.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    isolate.load(&large).unwrap();
                    done.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    // Every flooding request is queued or running once some have finished.
    tilo_test_env::wait_until("the flood to be under way", || {
        done.load(Ordering::Relaxed) >= FLOOD
    });
    let flooded = median(&isolate, &small);
    stop.store(true, Ordering::Relaxed);
    for thread in flood {
        thread.join().unwrap();
    }

    eprintln!(
        "small load alone {alone:?}, beside the flood {flooded:?}; large load alone {large_alone:?}"
    );
    assert!(
        flooded <= alone * 5 + large_alone / 10,
        "a small load waited {flooded:?} behind another caller's large ones, against {alone:?} alone \
         (a large load takes {large_alone:?})"
    );
}
