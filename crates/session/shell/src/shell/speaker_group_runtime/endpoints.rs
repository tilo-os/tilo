//! This device's audio endpoints, as its fabric snapshot publishes them.
//!
//! The snapshot builder asks for them on every fabric sample, about once a
//! second while the desktop is in use, and must never wait on the audio
//! graph. Reading the graph runs `pw-dump`, a process that prints all of
//! `PipeWire`'s objects, so it is read only when the device graph changed:
//! a thread of its own follows [`media::audio_watch::watch`] and reads the
//! graph on each change it reports, its first report included. Between
//! changes nothing runs.
//!
//! While `PipeWire` cannot be reached there is no graph to read (`pw-dump`
//! connects the same way the watch does), so the list is empty and the
//! watch is tried again on a later sample, at most every [`RETRY`].
//!
//! A read can fail while the watch stays up (`pw-dump` not finishing in
//! time while the machine is busy logging in). No change may follow to
//! prompt another read, so the follower reads again on its own, [`RETRY`]
//! after the first failure and twice as long after each further one, up to
//! [`RETRY_MAX`]; until a read succeeds the list is the one last read.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use media::audio::AudioGraph;
use media::audio_watch::GraphWatchEvent;
use tilo_lock::MutexRecover as _;

use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_group::AudioEndpoint;

/// How long after a failed or lost watch the next sample may try again,
/// and after a failed read the follower reads again.
const RETRY: Duration = Duration::from_secs(5);
/// The longest the follower waits between reads that keep failing.
const RETRY_MAX: Duration = Duration::from_secs(300);

/// What the watch reports to: called on the watch's own thread.
type Notify = Box<dyn Fn(GraphWatchEvent) + Send>;

/// The device graph as last read, and whether something keeps it current.
#[derive(Debug, Default)]
pub(super) struct GraphCache {
    graph: Option<AudioGraph>,
    /// A thread is following the graph, or starting to.
    following: bool,
    /// When the last watch failed or was lost.
    gave_up: Option<Instant>,
}

impl GraphCache {
    /// Whether a sample at `now` should start following the graph.
    fn should_follow(&self, now: Instant) -> bool {
        !self.following
            && self
                .gave_up
                .is_none_or(|at| now.saturating_duration_since(at) >= RETRY)
    }
}

fn graph_cache() -> &'static Mutex<GraphCache> {
    static CACHE: OnceLock<Mutex<GraphCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(GraphCache::default()))
}

/// The endpoints of the graph last read, for `local`. Starts following the
/// graph when nothing does; never waits for it.
pub(super) fn local_endpoints(local: &DeviceId, now: Instant) -> Vec<AudioEndpoint> {
    let cache = graph_cache();
    let graph = {
        let mut state = cache.lock_recover();
        if state.should_follow(now) {
            state.following = true;
            let spawned = std::thread::Builder::new()
                .name("speaker-endpoints".into())
                .spawn(move || {
                    follow(
                        cache,
                        media::audio_watch::watch,
                        media::audio::try_enumerate,
                        RETRY,
                    );
                });
            if spawned.is_err() {
                state.following = false;
                state.gave_up = Some(now);
            }
        }
        state.graph.clone()
    };
    graph.map_or_else(Vec::new, |graph| super::endpoints_from_graph(local, &graph))
}

/// Follow the device graph into `cache` until the watch ends: read it with
/// `read` on each change `watch` reports, and again `retry` after a read
/// that failed (doubling while they keep failing). A watch that cannot
/// start leaves no graph, since `read` could not reach `PipeWire` either.
pub(super) fn follow<W>(
    cache: &Mutex<GraphCache>,
    watch: impl FnOnce(Notify) -> Result<W, String>,
    mut read: impl FnMut() -> Result<AudioGraph, String>,
    retry: Duration,
) {
    let (events, changes) = mpsc::channel();
    let watching = watch(Box::new(move |event| {
        let _ = events.send(event);
    }));
    if let Ok(watching) = watching {
        // How long until a failed read is tried again; none while the last
        // read succeeded, when only a change prompts the next one.
        let mut again: Option<Duration> = None;
        loop {
            let event = match again {
                None => changes.recv().ok(),
                Some(after) => match changes.recv_timeout(after) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => Some(GraphWatchEvent::Changed),
                    Err(RecvTimeoutError::Disconnected) => None,
                },
            };
            if event != Some(GraphWatchEvent::Changed) {
                break;
            }
            match read() {
                Ok(graph) => {
                    cache.lock_recover().graph = Some(graph);
                    again = None;
                }
                Err(_) => {
                    again = Some(again.map_or(retry, |after| {
                        after.saturating_mul(2).min(RETRY_MAX.max(retry))
                    }));
                }
            }
        }
        drop(watching);
    }
    let mut state = cache.lock_recover();
    state.graph = None;
    state.following = false;
    state.gave_up = Some(Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The graph is read when the watch reports a change and at no other
    /// time, however often the snapshot asks; the list the snapshot
    /// publishes is the one last read.
    #[test]
    fn the_graph_is_read_on_each_change_not_on_each_sample() {
        let cache = Mutex::new(GraphCache::default());
        let (report, reports) = mpsc::channel::<Notify>();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let follower = scope.spawn(|| {
                follow(
                    &cache,
                    |notify| {
                        report.send(notify).map_err(|error| error.to_string())?;
                        Ok(())
                    },
                    || {
                        reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(AudioGraph::default())
                    },
                    RETRY,
                );
            });
            let notify = tilo_test_env::recv("the watch to start", &reports);
            notify(GraphWatchEvent::Changed);
            tilo_test_env::wait_for("the first read", || {
                cache.lock_recover().graph.is_some().then_some(())
            });
            assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
            notify(GraphWatchEvent::Changed);
            notify(GraphWatchEvent::Lost);
            follower.join().expect("the follower ends with the watch");
        });
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "one read per reported change"
        );
        let state = cache
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            state.graph.is_none(),
            "a lost watch leaves no graph to trust"
        );
        assert!(!state.following);
    }

    /// A read that fails while the watch stays up is tried again on its
    /// own: no device change may come to prompt it, and the list must not
    /// stay empty until one does.
    #[test]
    fn a_failed_read_is_tried_again_without_a_change() {
        let cache = Mutex::new(GraphCache::default());
        let (report, reports) = mpsc::channel::<Notify>();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let follower = scope.spawn(|| {
                follow(
                    &cache,
                    |notify| {
                        report.send(notify).map_err(|error| error.to_string())?;
                        Ok(())
                    },
                    || {
                        if reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
                            Err("pw-dump timed out".to_owned())
                        } else {
                            Ok(AudioGraph::default())
                        }
                    },
                    Duration::from_millis(1),
                );
            });
            let notify = tilo_test_env::recv("the watch to start", &reports);
            notify(GraphWatchEvent::Changed);
            tilo_test_env::wait_for("a read that succeeds", || {
                cache.lock_recover().graph.is_some().then_some(())
            });
            assert_eq!(
                reads.load(std::sync::atomic::Ordering::SeqCst),
                3,
                "two failed reads and the one that succeeded, from one change"
            );
            notify(GraphWatchEvent::Lost);
            follower.join().expect("the follower ends with the watch");
        });
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "nothing is read again once a read succeeded"
        );
    }

    /// Without `PipeWire` nothing is read, and the watch is tried again only
    /// after [`RETRY`].
    #[test]
    fn an_unreachable_pipewire_is_not_read_and_retried_later() {
        let cache = Mutex::new(GraphCache {
            following: true,
            ..GraphCache::default()
        });
        let mut reads = 0_u32;
        follow(
            &cache,
            |_notify| Err::<(), _>("PipeWire cannot be reached".to_owned()),
            || {
                reads = reads.saturating_add(1);
                Ok(AudioGraph::default())
            },
            RETRY,
        );
        assert_eq!(reads, 0, "pw-dump cannot reach PipeWire either");
        let state = cache
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let gave_up = state.gave_up.expect("the failure is remembered");
        let later = gave_up.checked_add(RETRY).expect("a retry time in range");
        assert!(!state.should_follow(gave_up));
        assert!(state.should_follow(later));
        assert!(
            !GraphCache {
                following: true,
                ..GraphCache::default()
            }
            .should_follow(later),
            "one follower at a time"
        );
    }
}
