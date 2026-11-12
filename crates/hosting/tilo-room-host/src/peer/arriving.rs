//! Devices a workspace admitted through its door that have not subscribed
//! yet ([`tilo_relay_protocol::entering`]), and the frames committed for them
//! alone in the meantime: the Welcome to the workspace's channel group that
//! the request in their entry asked for. Their subscription's replay writes
//! these first, so the keys are the first thing a newcomer downloads.
//!
//! The door answers the entry without waiting for the Welcome: the
//! participants commit it while the answer travels and the device subscribes.
//! A device that subscribes before its Welcome is committed has its replay
//! hold the first batch until the Welcome arrives ([`KEYS_WAIT`] at most), and
//! the Welcome goes to that replay as it is committed.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tilo_relay_protocol::entering::{ENTERING_DEADLINE, MAX_ENTERING};
use tokio::time::Instant;

use super::replay::ReplayCatchup;
use crate::transport::Envelope;

/// Frames kept for one arriving device: a Welcome, and a few commits more
/// should another device be admitted beside it.
const MAX_FRAMES: usize = 8;

/// How long an admitted device's frames wait for it to subscribe: as long as
/// its entry may take, which its subscription follows at once.
const WAITS: Duration = ENTERING_DEADLINE;

/// How long after its admission a device's first replay batch waits for the
/// Welcome its entry asked for: the participants commit it within tens of
/// milliseconds, and a device whose Welcome is later asks again as a member,
/// as every member does.
pub(crate) const KEYS_WAIT: Duration = Duration::from_secs(2);

/// The devices of one workspace that arrive through its door.
#[derive(Default)]
pub(crate) struct Arriving {
    devices: HashMap<[u8; 32], Arrival>,
}

struct Arrival {
    since: Instant,
    frames: Vec<Arc<Envelope>>,
    /// The replay of the device's subscription, once it subscribed before
    /// anything was committed for it: what is committed for it from then on
    /// goes there.
    replay: Option<Arc<ReplayCatchup>>,
}

impl Arriving {
    /// Keep what is committed for `device` alone until it subscribes. Bounded
    /// as entries are: an arrival older than an entry may take is let go, and
    /// past [`MAX_ENTERING`] the oldest.
    pub(crate) fn expect(&mut self, device: [u8; 32]) {
        let now = Instant::now();
        self.devices
            .retain(|_, arrival| now.duration_since(arrival.since) < WAITS);
        if self.devices.len() >= MAX_ENTERING
            && let Some(oldest) = self
                .devices
                .iter()
                .min_by_key(|(_, arrival)| arrival.since)
                .map(|(device, _)| *device)
        {
            self.devices.remove(&oldest);
        }
        self.devices.insert(
            device,
            Arrival {
                since: now,
                frames: Vec::new(),
                replay: None,
            },
        );
    }

    /// Keep `frame`, durable and addressed to `audience` alone, for each
    /// arriving device among it, or hand it to the replay of one that
    /// subscribed already.
    pub(crate) fn address(&mut self, audience: &BTreeSet<[u8; 32]>, frame: &Arc<Envelope>) {
        for device in audience {
            let Some(arrival) = self.devices.get_mut(device) else {
                continue;
            };
            if let Some(replay) = &arrival.replay {
                replay.address(Arc::clone(frame));
                self.devices.remove(device);
            } else if arrival.frames.len() < MAX_FRAMES {
                arrival.frames.push(Arc::clone(frame));
            }
        }
    }

    /// `device` subscribes now with `replay`: what was kept for it goes to the
    /// replay, oldest first. When nothing was yet, the replay's first batch
    /// waits for the device's Welcome until [`KEYS_WAIT`] after its admission,
    /// and the Welcome goes to it as it is committed.
    pub(super) fn arrived(&mut self, device: &[u8; 32], replay: &Arc<ReplayCatchup>) {
        let Some(arrival) = self.devices.get_mut(device) else {
            return;
        };
        if arrival.frames.is_empty()
            && let Some(due) = arrival.since.checked_add(KEYS_WAIT)
            && Instant::now() < due
        {
            replay.expect_keys(due);
            arrival.replay = Some(Arc::clone(replay));
            return;
        }
        for frame in std::mem::take(&mut arrival.frames) {
            replay.address(frame);
        }
        self.devices.remove(device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: u8) -> Arc<Envelope> {
        Arc::new(Envelope::Ready {
            workspace_id: u64::from(n),
            pending_deltas: 0,
            records_when_opened: false,
        })
    }

    fn replay() -> Arc<ReplayCatchup> {
        Arc::new(ReplayCatchup::new(std::path::Path::new("unused")))
    }

    #[tokio::test]
    async fn a_frame_for_an_arriving_device_waits_for_its_subscription() {
        let mut arriving = Arriving::default();
        arriving.expect([1; 32]);
        arriving.address(&BTreeSet::from([[2; 32]]), &frame(1));
        arriving.address(&BTreeSet::from([[1; 32], [2; 32]]), &frame(2));
        let first = replay();
        arriving.arrived(&[1; 32], &first);
        assert_eq!(first.take_addressed(), vec![frame(2)]);
        // Its keys are here: the replay has nothing to wait for.
        tokio::time::timeout(Duration::from_millis(100), first.keys())
            .await
            .expect("a replay whose keys came first does not wait");
        let again = replay();
        arriving.arrived(&[1; 32], &again);
        assert!(again.take_addressed().is_empty());
        let stranger = replay();
        arriving.arrived(&[2; 32], &stranger);
        assert!(stranger.take_addressed().is_empty());
    }

    #[tokio::test]
    async fn a_device_that_subscribes_before_its_welcome_has_its_replay_wait_for_it() {
        let mut arriving = Arriving::default();
        arriving.expect([1; 32]);
        let subscribed = replay();
        arriving.arrived(&[1; 32], &subscribed);
        let waiting = tokio::spawn({
            let subscribed = Arc::clone(&subscribed);
            async move { subscribed.keys().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiting.is_finished(), "the replay waits for the Welcome");
        arriving.address(&BTreeSet::from([[1; 32]]), &frame(3));
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("the Welcome ends the replay's wait")
            .unwrap();
        assert_eq!(subscribed.take_addressed(), vec![frame(3)]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_welcome_that_never_comes_holds_the_replay_no_longer_than_the_wait() {
        let mut arriving = Arriving::default();
        arriving.expect([1; 32]);
        let subscribed = replay();
        arriving.arrived(&[1; 32], &subscribed);
        let started = Instant::now();
        subscribed.keys().await;
        assert!(started.elapsed() >= KEYS_WAIT);
        assert!(started.elapsed() < KEYS_WAIT + Duration::from_millis(100));
    }

    #[test]
    fn arrivals_are_bounded_as_entries_are() {
        let mut arriving = Arriving::default();
        for n in 0..=MAX_ENTERING {
            let mut device = [0; 32];
            device[..8].copy_from_slice(&n.to_le_bytes());
            arriving.expect(device);
        }
        assert_eq!(arriving.devices.len(), MAX_ENTERING);
        assert!(!arriving.devices.contains_key(&[0; 32]));
        let device = [9; 32];
        arriving.expect(device);
        for n in 0..20 {
            arriving.address(&BTreeSet::from([device]), &frame(n));
        }
        let subscribed = replay();
        arriving.arrived(&device, &subscribed);
        assert_eq!(subscribed.take_addressed().len(), MAX_FRAMES);
    }
}
