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

    const WORKERS: usize = 24;
    const OPENS: usize = 40;
    // A healthy open lands its picture in well under a second even under
    // this load; a stalled one never does.
    const PER_OPEN: Duration = Duration::from_secs(15);

    // Encode the fixture before the load starts.
    let _ = fixture_path();

    let stop = Arc::new(AtomicBool::new(false));
    let hogs: Vec<_> = (0..std::thread::available_parallelism().map_or(8, std::num::NonZero::get))
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
                        stalls.push(format!(
                            "worker {w} open {open} seek {target}ms: {error}; {}",
                            e.hw_report().describe()
                        ));
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
