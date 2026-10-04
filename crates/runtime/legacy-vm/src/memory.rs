//! How much memory the VM holds: little when idle, more under pressure.
//!
//! The domain is defined with [`MemoryPolicy::max_mib`] and boots ballooned
//! down to [`MemoryPolicy::base_mib`]. The supervisor reads the guest's
//! available memory and moves the virtio-balloon target one step at a time.

/// Balloon bounds and step, in MiB.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryPolicy {
    pub base_mib: u64,
    pub max_mib: u64,
    pub step_mib: u64,
    /// Grow when the guest has less than this available.
    pub low_water_mib: u64,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            base_mib: 1536,
            max_mib: 6144,
            step_mib: 512,
            low_water_mib: 384,
        }
    }
}

impl MemoryPolicy {
    /// The next balloon target, or `None` to leave it.
    ///
    /// Grows by a step while available memory is under the low-water mark or
    /// a quarter of the current size; gives a step back while more than half
    /// plus a step is free, never below the base.
    #[must_use]
    pub fn next_target(&self, current_mib: u64, available_mib: u64) -> Option<u64> {
        let low = self.low_water_mib.max(current_mib / 4);
        if available_mib < low && current_mib < self.max_mib {
            return Some(current_mib.saturating_add(self.step_mib).min(self.max_mib));
        }
        let generous = (current_mib / 2).saturating_add(self.step_mib);
        if available_mib > generous && current_mib > self.base_mib {
            return Some(current_mib.saturating_sub(self.step_mib).max(self.base_mib));
        }
        None
    }
}

/// The balloon's current size in MiB from `virsh dommemstat` (`actual` KiB).
#[must_use]
pub fn actual_mib(dommemstat: &str) -> Option<u64> {
    dommemstat.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == "actual")
            .then(|| words.next()?.parse::<u64>().ok())
            .flatten()
            .map(|kib| kib / 1024)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_balloon_follows_guest_pressure_within_bounds() {
        let policy = MemoryPolicy::default();
        // phase: pressure grows one step at a time up to the ceiling
        {
            assert_eq!(policy.next_target(1536, 200), Some(2048));
            assert_eq!(policy.next_target(5888, 100), Some(6144));
            assert_eq!(policy.next_target(6144, 100), None);
            // A quarter of a large VM is the low-water mark there.
            assert_eq!(policy.next_target(4096, 900), Some(4608));
        }
        // phase: a mostly idle guest gives memory back, never below the base
        {
            assert_eq!(policy.next_target(4096, 3000), Some(3584));
            assert_eq!(policy.next_target(1792, 1500), Some(1536));
            assert_eq!(policy.next_target(1536, 1500), None);
            assert_eq!(policy.next_target(2048, 1000), None);
        }
        // phase: virsh's balloon size is read in MiB
        {
            assert_eq!(actual_mib("actual 2097152\nrss 100\n"), Some(2048));
            assert_eq!(actual_mib("rss 100\n"), None);
        }
    }
}
