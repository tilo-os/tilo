//! Devices a workspace admitted through its door that have not subscribed
//! yet ([`tilo_relay_protocol::entering`]), and the frames committed for them
//! alone in the meantime: the Welcome to the workspace's channel group that
//! the request in their entry asked for. Their subscription's replay writes
//! these first, so the keys are the first thing a newcomer downloads.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_relay_protocol::entering::{ENTERING_DEADLINE, MAX_ENTERING};

use crate::transport::Envelope;

/// Frames kept for one arriving device: a Welcome, and a few commits more
/// should another device be admitted beside it.
const MAX_FRAMES: usize = 8;

/// How long an admitted device's frames wait for it to subscribe: as long as
/// its entry may take, which its subscription follows at once.
const WAITS: Duration = ENTERING_DEADLINE;

/// The devices of one workspace that arrive through its door.
#[derive(Default)]
pub(crate) struct Arriving {
    devices: HashMap<[u8; 32], Arrival>,
}

struct Arrival {
    since: Instant,
    frames: Vec<Arc<Envelope>>,
    keyed: Arc<tokio::sync::Notify>,
}

impl Arriving {
    /// Keep what is committed for `device` alone until it subscribes. The
    /// answer is woken once the first such frame is durable. Bounded as entries
    /// are: an arrival older than an entry may take is let go, and past
    /// [`MAX_ENTERING`] the oldest.
    pub(crate) fn expect(&mut self, device: [u8; 32]) -> Arc<tokio::sync::Notify> {
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
        let keyed = Arc::new(tokio::sync::Notify::new());
        self.devices.insert(
            device,
            Arrival {
                since: now,
                frames: Vec::new(),
                keyed: Arc::clone(&keyed),
            },
        );
        keyed
    }

    /// Keep `frame`, durable and addressed to `audience` alone, for each
    /// arriving device among it.
    pub(crate) fn address(&mut self, audience: &BTreeSet<[u8; 32]>, frame: &Arc<Envelope>) {
        for device in audience {
            if let Some(arrival) = self.devices.get_mut(device)
                && arrival.frames.len() < MAX_FRAMES
            {
                arrival.frames.push(Arc::clone(frame));
                arrival.keyed.notify_one();
            }
        }
    }

    /// What was kept for `device`, which subscribes now, oldest first.
    pub(crate) fn arrived(&mut self, device: &[u8; 32]) -> Vec<Arc<Envelope>> {
        self.devices
            .remove(device)
            .map(|arrival| arrival.frames)
            .unwrap_or_default()
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

    #[tokio::test]
    async fn a_frame_for_an_arriving_device_waits_for_its_subscription() {
        let mut arriving = Arriving::default();
        let keyed = arriving.expect([1; 32]);
        arriving.address(&BTreeSet::from([[2; 32]]), &frame(1));
        arriving.address(&BTreeSet::from([[1; 32], [2; 32]]), &frame(2));
        tokio::time::timeout(Duration::from_secs(1), keyed.notified())
            .await
            .expect("the arriving device's frame wakes its entry");
        assert_eq!(arriving.arrived(&[1; 32]), vec![frame(2)]);
        assert!(arriving.arrived(&[1; 32]).is_empty());
        assert!(arriving.arrived(&[2; 32]).is_empty());
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
        assert_eq!(arriving.arrived(&device).len(), MAX_FRAMES);
    }
}
