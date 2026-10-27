//! Which epochs a device sends and accepts, so a recorded probe replayed
//! later or elsewhere does not make the person's devices answer.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::wire::Salt;

/// Length of one epoch.
pub const EPOCH: Duration = Duration::from_secs(10 * 60);
/// Most askers whose newest epoch is remembered.
const MAX_ASKERS: usize = 256;
/// Most probe salts remembered as answered.
const MAX_SALTS: usize = 1024;

/// The epoch of a synchronised wall clock reading `unix_secs`.
#[must_use]
pub fn epoch_at(unix_secs: u64) -> u32 {
    u32::try_from(unix_secs.checked_div(EPOCH.as_secs()).unwrap_or(0)).unwrap_or(u32::MAX)
}

/// Freshness state of one device. Askers' newest epochs survive restarts
/// through the file at `path`.
pub struct Freshness {
    path: Option<PathBuf>,
    /// Newest epoch accepted from each asking device.
    newest: BTreeMap<[u8; 32], u32>,
    /// The newest epoch a trusted device's probe carried, and when.
    learned: Option<(u32, Instant)>,
    answered: HashSet<Salt>,
    answered_order: VecDeque<Salt>,
}

impl Freshness {
    /// State kept at `path` (`None`: memory only), loaded if present.
    #[must_use]
    pub fn open(path: Option<PathBuf>) -> Self {
        let newest = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<BTreeMap<String, u32>>(&bytes).ok())
            .map(|map| {
                map.into_iter()
                    .filter_map(|(device, epoch)| {
                        let bytes = hex::decode(device).ok()?;
                        Some((<[u8; 32]>::try_from(bytes).ok()?, epoch))
                    })
                    .take(MAX_ASKERS)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path,
            newest,
            learned: None,
            answered: HashSet::new(),
            answered_order: VecDeque::new(),
        }
    }

    /// The epoch this device's probes carry. `clock` is the synchronised
    /// wall clock's epoch, `None` when the clock is not synchronised: then
    /// the newest epoch learned from a trusted device's probe, advanced by
    /// the time since, or 0. An unsynchronised clock may be the image's
    /// build date and would single the device out.
    #[must_use]
    pub fn send_epoch(&self, clock: Option<u32>, now: Instant) -> u32 {
        clock.unwrap_or_else(|| {
            self.learned.map_or(0, |(epoch, at)| {
                let since = now
                    .saturating_duration_since(at)
                    .as_secs()
                    .checked_div(EPOCH.as_secs())
                    .unwrap_or(0);
                epoch.saturating_add(u32::try_from(since).unwrap_or(u32::MAX))
            })
        })
    }

    /// Whether to answer a probe from `asker` (already known to be one of
    /// the person's devices) carrying `epoch` and `salt`, recording it if
    /// so. A synchronised clock accepts its own epoch and its neighbours;
    /// either way an asker's epoch never goes back, and each probe is
    /// answered once.
    pub fn accept(
        &mut self,
        asker: [u8; 32],
        epoch: u32,
        salt: Salt,
        clock: Option<u32>,
        now: Instant,
    ) -> bool {
        if clock.is_some_and(|current| epoch.abs_diff(current) > 1)
            || self
                .newest
                .get(&asker)
                .is_some_and(|newest| epoch < *newest)
            || self.answered.contains(&salt)
        {
            return false;
        }
        if self.answered_order.len() >= MAX_SALTS
            && let Some(oldest) = self.answered_order.pop_front()
        {
            self.answered.remove(&oldest);
        }
        self.answered.insert(salt);
        self.answered_order.push_back(salt);
        if clock.is_none() && self.learned.is_none_or(|(learned, _)| epoch > learned) {
            self.learned = Some((epoch, now));
        }
        if self.newest.get(&asker) != Some(&epoch) {
            if self.newest.len() >= MAX_ASKERS && !self.newest.contains_key(&asker) {
                // Forget the asker heard from longest ago.
                if let Some(stalest) = self
                    .newest
                    .iter()
                    .min_by_key(|(_, epoch)| **epoch)
                    .map(|(device, _)| *device)
                {
                    self.newest.remove(&stalest);
                }
            }
            self.newest.insert(asker, epoch);
            self.save();
        }
        true
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let map: BTreeMap<String, u32> = self
            .newest
            .iter()
            .map(|(device, epoch)| (hex::encode(device), *epoch))
            .collect();
        let saved = serde_json::to_vec(&map)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                tilo_files::native::atomic::write(path, &bytes)
            });
        if let Err(error) = saved {
            tracing::warn!(%error, "could not keep LAN discovery freshness");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_synchronised_clock_accepts_its_epoch_and_neighbours_once_each() {
        let mut fresh = Freshness::open(None);
        let now = Instant::now();
        assert!(!fresh.accept([1; 32], 7, [1; 16], Some(10), now), "too old");
        assert!(
            !fresh.accept([1; 32], 12, [2; 16], Some(10), now),
            "too new"
        );
        assert!(fresh.accept([1; 32], 9, [3; 16], Some(10), now));
        assert!(
            !fresh.accept([1; 32], 9, [3; 16], Some(10), now),
            "answered once"
        );
        assert!(fresh.accept([1; 32], 11, [4; 16], Some(10), now));
        assert!(
            !fresh.accept([1; 32], 10, [5; 16], Some(10), now),
            "an asker's epoch never goes back"
        );
        assert_eq!(fresh.send_epoch(Some(10), now), 10);
    }

    #[test]
    fn without_a_clock_a_device_follows_its_peers_epochs_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lan-discovery.json");
        let now = Instant::now();
        let mut fresh = Freshness::open(Some(path.clone()));
        assert_eq!(
            fresh.send_epoch(None, now),
            0,
            "its own clock is never sent"
        );
        assert!(fresh.accept([1; 32], 500, [1; 16], None, now));
        assert_eq!(fresh.send_epoch(None, now), 500);
        assert_eq!(fresh.send_epoch(None, now + EPOCH * 2), 502);

        let mut restarted = Freshness::open(Some(path));
        assert!(
            !restarted.accept([1; 32], 499, [2; 16], None, now),
            "a recording older than what the asker sent before is refused after a restart"
        );
        assert!(restarted.accept([1; 32], 500, [3; 16], None, now));
        assert!(
            restarted.accept([2; 32], 3, [4; 16], None, now),
            "per asker"
        );
    }
}
