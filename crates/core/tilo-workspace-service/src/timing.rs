//! Phase timings for capacity runs.
//!
//! A workspace service under load is a queue of queues: the workspace's lock, its
//! persistence owner, the replay slots, the blocking workers. Which one a
//! crowd waits in is invisible from outside, so the phases a join, a commit
//! and an owner turn pass through report how long they took, on the
//! `tilo_server::timing` target at debug level:
//!
//! - [`event`]: one `TIMING <step> <microseconds> <size>` line per
//!   occurrence, for phases that happen about once per newcomer or commit,
//!   so a cost that grows with the workspace shows in arrival order;
//! - [`window`]: phases that happen per frame or per lock acquisition are
//!   summed instead, one `TIMING_WINDOW <unix second> <step> <count>
//!   <total µs> <max µs>` line per step and second.
//!
//! Both are off unless the target is enabled (a capacity run does, such as the
//! server's own timing): a
//! production host pays one disabled-callsite check per phase and reads no
//! clock.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;

/// Whether timings are being recorded.
pub(crate) fn enabled() -> bool {
    tracing::enabled!(target: "tilo_server::timing", tracing::Level::DEBUG)
}

/// Now, when timings are recorded: the start [`event`] and [`window`] take.
pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

fn micros(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// Record one occurrence of `step` begun at `started` (from [`start`]),
/// with a size of whatever it worked on (frames, entries; 0 when none).
pub(crate) fn event(step: &'static str, started: Option<Instant>, size: u64) {
    if let Some(started) = started {
        tracing::debug!(
            target: "tilo_server::timing",
            "TIMING {step} {} {size}",
            micros(started.elapsed())
        );
    }
}

#[derive(Default)]
struct Window {
    count: u64,
    total_us: u64,
    max_us: u64,
}

fn windows() -> &'static Mutex<BTreeMap<&'static str, Window>> {
    static WINDOWS: OnceLock<Mutex<BTreeMap<&'static str, Window>>> = OnceLock::new();
    WINDOWS.get_or_init(|| {
        // One reporter for the process, started by the first window.
        let _ = std::thread::Builder::new()
            .name("server-timings".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let taken = std::mem::take(&mut *windows().lock_recover());
                    let second = tilo_clock::wall()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |since| since.as_secs());
                    for (step, window) in taken {
                        tracing::debug!(
                            target: "tilo_server::timing",
                            "TIMING_WINDOW {second} {step} {} {} {}",
                            window.count,
                            window.total_us,
                            window.max_us
                        );
                    }
                }
            });
        Mutex::default()
    })
}

/// Add one occurrence of `step` begun at `started` (from [`start`]) to this
/// second's sum.
pub fn window(step: &'static str, started: Option<Instant>) {
    let Some(started) = started else {
        return;
    };
    let us = micros(started.elapsed());
    let mut windows = windows().lock_recover();
    let window = windows.entry(step).or_default();
    window.count = window.count.saturating_add(1);
    window.total_us = window.total_us.saturating_add(us);
    window.max_us = window.max_us.max(us);
    drop(windows);
}

/// How long a phase of a write may wait before it is said while it still waits.
const WAITED_LONG: Duration = Duration::from_secs(10);

/// Run `work`, a phase of a write, saying at warn level once it has waited [`WAITED_LONG`] for
/// `phase` and still waits: a write that never ends names the phase it waits in before whoever
/// gives up on it does, on any host, timings enabled or not.
pub(crate) async fn said_if_long<T>(
    phase: &'static str,
    work: impl std::future::Future<Output = T>,
) -> T {
    let mut work = std::pin::pin!(work);
    let waited = std::pin::pin!(tilo_rt::sleep(WAITED_LONG));
    match futures::future::select(work.as_mut(), waited).await {
        futures::future::Either::Left((done, _)) => done,
        futures::future::Either::Right(((), _)) => {
            tracing::warn!(
                phase,
                waited_s = WAITED_LONG.as_secs(),
                "a write still waits"
            );
            work.await
        }
    }
}
