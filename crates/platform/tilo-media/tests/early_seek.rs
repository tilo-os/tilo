//! A seek issued straight after `open`, before the first preroll completes,
//! must still produce a picture.
//!
//! `GStreamer` sometimes accepts a flushing seek while decodebin is still
//! autoplugging, and that seek leaves the decoder's caps unnegotiated: no
//! error, and no picture ever. The window is narrow, so this opens many
//! engines in parallel under CPU contention. Before the fix it stalled a few
//! opens in every thousand; it is a bounded stress test, so a run that misses
//! the window passes without proving anything.

mod common;

#[cfg(feature = "decode")]
#[test]
fn an_early_seek_under_load_always_produces_a_picture() {
    use common::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tilo_media::{EngineConfig, MediaEngine};

    const CONTENDED_CPUS: usize = 4;
    const WORKERS: usize = 24;
    const OPENS: usize = 40;
    // A healthy open lands its picture in well under a second even under
    // this load; a stalled one never does.
    const PER_OPEN: Duration = Duration::from_secs(15);
    // How much longer a miss is watched before it counts as a stall. On a
    // runner whose cores other jobs also hold, a starved open can outlast
    // PER_OPEN; the stall this test exists for never produces its picture,
    // so only an open that stays silent for the shared hang bound is one.
    let stall = tilo_test_env::hang_bound();

    // The contention is made on a few CPUs, not the whole machine: every
    // thread of the test (the fixture's encoder, the hogs, the workers and the
    // pipelines they start) runs on them, a hog for each, so the opens contend
    // as on a busy machine while a shared one keeps its other cores.
    let cpus = confine_to_cpus(CONTENDED_CPUS);

    // Encode the fixture before the load starts.
    let _ = fixture_path();

    let stop = Arc::new(AtomicBool::new(false));
    let hogs: Vec<_> = (0..cpus)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    x = std::hint::black_box(
                        x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                    );
                }
            })
        })
        .collect();

    let workers: Vec<_> = (0..WORKERS)
        .map(|w| {
            std::thread::spawn(move || {
                let mut stalls = Vec::new();
                for open in 0..OPENS {
                    let mut e = MediaEngine::with_config(EngineConfig::default().frames_only());
                    e.open(fixture_source());
                    let target = if (w + open) % 2 == 0 { 5_000 } else { 1_000 };
                    if let Err(error) = e.seek_and_wait(target, PER_OPEN) {
                        // Keep pumping the engine without asking again: a
                        // second seek could unstick the very stall sought.
                        let deadline = std::time::Instant::now() + stall;
                        let mut late = false;
                        while std::time::Instant::now() < deadline {
                            let _ = e.poll();
                            if e.frame_is_current() {
                                late = true;
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        if late {
                            eprintln!(
                                "worker {w} open {open} seek {target}ms: picture arrived after {PER_OPEN:?} ({error}): slow, not stalled"
                            );
                        } else {
                            stalls.push(format!(
                                "worker {w} open {open} seek {target}ms: {error}, and nothing for {stall:?} more; {}",
                                e.hw_report().describe()
                            ));
                        }
                    }
                }
                stalls
            })
        })
        .collect();
    let mut stalls = Vec::new();
    for worker in workers {
        stalls.extend(
            worker
                .join()
                .unwrap_or_else(|_| vec!["worker panicked".into()]),
        );
    }

    stop.store(true, Ordering::Relaxed);
    for hog in hogs {
        let _ = hog.join();
    }
    assert!(
        stalls.is_empty(),
        "{} of {} early seeks produced no picture: {stalls:#?}",
        stalls.len(),
        WORKERS * OPENS
    );
}

/// Confines the calling thread, and every thread it starts from now on, to at
/// most `count` of the CPUs it may run on; returns how many it got.
#[cfg(all(feature = "decode", target_os = "linux"))]
fn confine_to_cpus(count: usize) -> usize {
    let size = std::mem::size_of::<libc::cpu_set_t>();
    // SAFETY: an all-zero `cpu_set_t` is the empty set, and both calls read or
    // write only the set they are handed, of the size they are told.
    unsafe {
        let mut allowed: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, size, &raw mut allowed),
            0,
            "read this thread's CPUs"
        );
        let mut chosen: libc::cpu_set_t = std::mem::zeroed();
        let cpus = (0..size.saturating_mul(8))
            .filter(|&cpu| libc::CPU_ISSET(cpu, &allowed))
            .take(count);
        let got = cpus.inspect(|&cpu| libc::CPU_SET(cpu, &mut chosen)).count();
        assert_eq!(
            libc::sched_setaffinity(0, size, &raw const chosen),
            0,
            "confine this thread's CPUs"
        );
        got
    }
}

/// Where threads can't be confined, the load is still bounded to `count`.
#[cfg(all(feature = "decode", not(target_os = "linux")))]
fn confine_to_cpus(count: usize) -> usize {
    std::thread::available_parallelism().map_or(count, |cpus| cpus.get().min(count))
}
