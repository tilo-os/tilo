//! Driving a newly launched player to the state a cross-device handoff
//! carried: its source, its playhead, playing or paused.
//!
//! Completion is reported only after the player itself reflects the
//! requested source, playhead and transport status. A resume the compositor
//! revokes issues no player call after the revocation returns, and one
//! already issued is waited out first.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{MPRIS_PATH, Mpris, PLAYER_CALL_TIMEOUT, PLAYER_IFACE, ROOT_IFACE};
use crate::{ResumeRequest, handoff};

const RESUME_DEADLINE: Duration = Duration::from_secs(10);
const RESUME_POLL: Duration = Duration::from_millis(100);
const RESUME_TOLERANCE_US: i64 = 500_000;
const MAX_PENDING_RESUMES: usize = 16;

/// Whether one resume may still act on a player, and the lock its player
/// mutations hold so that revocation is ordered after any in flight.
struct Gate {
    active: AtomicBool,
    mutation: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub(super) struct Authority(Arc<Gate>);

impl Authority {
    pub(super) fn is_active(&self) -> bool {
        self.0.active.load(Ordering::Acquire)
    }

    /// Run `operation` (one player mutation) only while still authorized.
    async fn mutate<F: Future>(&self, operation: F) -> Option<F::Output> {
        let _mutation = self.0.mutation.lock().await;
        if !self.is_active() {
            return None;
        }
        Some(operation.await)
    }

    /// Stop every later mutation, after waiting out one already issued, so
    /// that once this returns no mutation for the resume is in flight.
    /// Called from synchronous code only, never on an async worker.
    fn revoke(&self) {
        self.0.active.store(false, Ordering::Release);
        drop(self.0.mutation.blocking_lock());
    }
}

/// The resumes running now, by request.
#[derive(Default)]
pub(super) struct Resumes(Mutex<BTreeMap<[u8; 16], Authority>>);

impl Resumes {
    /// Authority for a new resume; `None` for one already running or past
    /// the bound.
    pub(super) fn begin(&self, request_id: [u8; 16]) -> Option<Authority> {
        let mut pending = self.0.lock().ok()?;
        if pending.len() >= MAX_PENDING_RESUMES || pending.contains_key(&request_id) {
            return None;
        }
        let authority = Authority(Arc::new(Gate {
            active: AtomicBool::new(true),
            mutation: tokio::sync::Mutex::new(()),
        }));
        pending.insert(request_id, authority.clone());
        drop(pending);
        Some(authority)
    }

    /// The resume is over, however it ended.
    pub(super) fn finish(&self, authority: &Authority) {
        if let Ok(mut pending) = self.0.lock() {
            pending.retain(|_, running| !Arc::ptr_eq(&running.0, &authority.0));
        }
    }

    /// Revoke one resume. Requests arrive in order, so a cancel never
    /// precedes the resume it revokes.
    pub(super) fn cancel(&self, request_id: [u8; 16]) {
        let authority = self
            .0
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&request_id));
        if let Some(authority) = authority {
            authority.revoke();
        }
    }
}

/// Drive a player of `request`'s package to its state within
/// [`RESUME_DEADLINE`]: the player and the source it now plays, or `None`.
pub(super) async fn run(
    mpris: &Mpris,
    request: &ResumeRequest,
    authority: &Authority,
) -> Option<(String, String)> {
    let expected_uri = handoff::resume_uri(&request.source)?;
    let connection = mpris.bus().await?;
    let started = Instant::now();
    let mut opened = BTreeSet::new();
    while started.elapsed() < RESUME_DEADLINE {
        if !authority.is_active() {
            return None;
        }
        let candidates = mpris
            .with_table(|table| table.players_of(&request.package_id))
            .await
            .unwrap_or_default();
        for (bus_name, reported_uri) in candidates {
            if !authority.is_active() {
                return None;
            }
            if reported_uri.as_deref() == Some(expected_uri.as_str()) {
                if drive_and_verify(mpris, &bus_name, request, authority).await {
                    return (authority.is_active() && started.elapsed() <= RESUME_DEADLINE)
                        .then(|| (bus_name.clone(), expected_uri.clone()));
                }
            } else if opened.insert(bus_name.clone()) {
                open_uri_if_supported(mpris, &connection, &bus_name, &expected_uri, authority)
                    .await;
            }
        }
        if authority.is_active() {
            // The player has no signal for "now at this source and
            // position"; it is asked again until the deadline.
            tokio::time::sleep(RESUME_POLL).await;
        }
    }
    None
}

/// Call `method` on `bus_name`'s `interface` only while `authority` is
/// still active.
async fn call_authorized<B>(
    connection: &zbus::Connection,
    authority: &Authority,
    bus_name: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> bool
where
    B: serde::Serialize + zbus::zvariant::DynamicType + Sync,
{
    let call = connection.call_method(Some(bus_name), MPRIS_PATH, Some(interface), method, body);
    authority
        .mutate(tokio::time::timeout(PLAYER_CALL_TIMEOUT, call))
        .await
        .is_some_and(|result| result.is_ok_and(|reply| reply.is_ok()))
}

async fn drive_and_verify(
    mpris: &Mpris,
    bus_name: &str,
    request: &ResumeRequest,
    authority: &Authority,
) -> bool {
    let Some(connection) = mpris.bus().await else {
        return false;
    };
    if !authority.is_active() {
        return false;
    }
    let target_us = i64::try_from(request.position_ms.saturating_mul(1_000)).unwrap_or(i64::MAX);
    let can_seek = mpris
        .property::<bool>(bus_name, PLAYER_IFACE, "CanSeek")
        .await
        .unwrap_or(false);
    let current_us = mpris
        .property::<i64>(bus_name, PLAYER_IFACE, "Position")
        .await
        .unwrap_or(-1);
    match required_seek(current_us, target_us, can_seek) {
        SeekDecision::Satisfied => {}
        SeekDecision::Offset(offset)
            if call_authorized(
                &connection,
                authority,
                bus_name,
                PLAYER_IFACE,
                "Seek",
                &(offset,),
            )
            .await => {}
        SeekDecision::Offset(_) | SeekDecision::Impossible => return false,
    }
    let (method, status) = if request.playing {
        ("Play", "Playing")
    } else {
        ("Pause", "Paused")
    };
    if !call_authorized(&connection, authority, bus_name, PLAYER_IFACE, method, &()).await {
        return false;
    }
    for _ in 0..5 {
        if !authority.is_active() {
            return false;
        }
        let status_matches = mpris
            .property::<String>(bus_name, PLAYER_IFACE, "PlaybackStatus")
            .await
            .is_some_and(|reported| reported == status);
        let position_matches = mpris
            .property::<i64>(bus_name, PLAYER_IFACE, "Position")
            .await
            .is_some_and(|position| position.abs_diff(target_us) <= RESUME_TOLERANCE_US as u64);
        if status_matches && position_matches {
            return true;
        }
        tokio::time::sleep(RESUME_POLL).await;
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SeekDecision {
    Satisfied,
    Offset(i64),
    Impossible,
}

fn required_seek(current_us: i64, target_us: i64, can_seek: bool) -> SeekDecision {
    if current_us < 0 || target_us < 0 {
        return SeekDecision::Impossible;
    }
    if current_us.abs_diff(target_us) <= RESUME_TOLERANCE_US as u64 {
        return SeekDecision::Satisfied;
    }
    if can_seek {
        SeekDecision::Offset(target_us.saturating_sub(current_us))
    } else {
        SeekDecision::Impossible
    }
}

async fn open_uri_if_supported(
    mpris: &Mpris,
    connection: &zbus::Connection,
    bus_name: &str,
    uri: &str,
    authority: &Authority,
) -> bool {
    let scheme = uri.split_once(':').map_or("", |(scheme, _)| scheme);
    let supported = mpris
        .property::<Vec<String>>(bus_name, ROOT_IFACE, "SupportedUriSchemes")
        .await
        .is_some_and(|schemes| schemes.iter().any(|supported| supported == scheme));
    supported
        && call_authorized(
            connection,
            authority,
            bus_name,
            PLAYER_IFACE,
            "OpenUri",
            &(uri,),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Drive one authorized mutation to completion on the calling thread.
    fn mutate_now<T>(authority: &Authority, operation: impl FnOnce() -> T) -> Option<T> {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(authority.mutate(async move { operation() }))
    }

    #[test]
    fn destination_only_accepts_an_observed_or_seekable_exact_playhead() {
        assert_eq!(
            required_seek(4_100_000, 4_000_000, false),
            SeekDecision::Satisfied
        );
        assert_eq!(
            required_seek(1_000_000, 4_000_000, true),
            SeekDecision::Offset(3_000_000)
        );
        assert_eq!(
            required_seek(1_000_000, 4_000_000, false),
            SeekDecision::Impossible
        );
        assert_eq!(required_seek(-1, 4_000_000, true), SeekDecision::Impossible);
    }

    #[test]
    fn resumes_are_bounded_and_never_run_twice() {
        let resumes = Resumes::default();
        let first = resumes.begin([1; 16]).unwrap();
        assert!(resumes.begin([1; 16]).is_none());
        for index in 2..=u8::try_from(MAX_PENDING_RESUMES).unwrap() {
            assert!(resumes.begin([index; 16]).is_some());
        }
        assert!(resumes.begin([99; 16]).is_none(), "past the bound");
        resumes.finish(&first);
        assert!(
            resumes.begin([1; 16]).is_some(),
            "a finished resume frees its slot"
        );
    }

    #[test]
    fn cancellation_after_launch_blocks_every_later_mutation() {
        let resumes = Resumes::default();
        let authority = resumes.begin([22; 16]).unwrap();
        let mut mutations = Vec::new();

        assert_eq!(
            mutate_now(&authority, || mutations.push("open-uri")),
            Some(())
        );
        resumes.cancel([22; 16]);
        assert_eq!(mutate_now(&authority, || mutations.push("seek")), None);
        assert_eq!(mutate_now(&authority, || mutations.push("play")), None);
        assert_eq!(mutations, ["open-uri"]);
        assert!(!authority.is_active());
    }

    /// Whether thread `tid` of this process sleeps in the kernel, as one
    /// waiting on a held lock does.
    fn thread_is_blocked(tid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"))
            .ok()
            .and_then(|stat| Some(stat.rsplit_once(')')?.1.trim_start().starts_with('S')))
            .unwrap_or(false)
    }

    #[test]
    fn cancellation_is_linearized_after_an_in_flight_mutation() {
        let resumes = Arc::new(Resumes::default());
        let authority = resumes.begin([24; 16]).unwrap();
        let mutation_authority = authority.clone();
        let (mutation_started_tx, mutation_started_rx) = mpsc::channel();
        let (release_mutation_tx, release_mutation_rx) = mpsc::channel::<()>();
        let mutation = std::thread::spawn(move || {
            mutate_now(&mutation_authority, || {
                mutation_started_tx.send(()).unwrap();
                release_mutation_rx.recv().unwrap();
            })
        });
        mutation_started_rx.recv().unwrap();

        let cancel_resumes = Arc::clone(&resumes);
        let (cancel_started_tx, cancel_started_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let cancellation = std::thread::spawn(move || {
            cancel_started_tx
                .send(tilo_test_env::idle::current_thread_id().expect("a thread id"))
                .unwrap();
            cancel_resumes.cancel([24; 16]);
            cancelled_tx.send(()).unwrap();
        });
        let cancel_thread = cancel_started_rx.recv().unwrap();
        let early = tilo_test_env::wait_for("the cancel to answer or block", || {
            if cancelled_rx.try_recv().is_ok() {
                return Some(true);
            }
            thread_is_blocked(cancel_thread).then_some(false)
        });
        assert!(
            !early,
            "cancel must wait for the already-started mutation to leave its gate"
        );

        release_mutation_tx.send(()).unwrap();
        assert_eq!(mutation.join().unwrap(), Some(()));
        tilo_test_env::recv("the cancel returning", &cancelled_rx);
        cancellation.join().unwrap();
        assert_eq!(mutate_now(&authority, || panic!("late mutation ran")), None);
    }
}
