//! What the VMs on a host really take of its memory, and what it may still
//! admit.
//!
//! A VM is configured at its size — 4 GiB at `small`, as its tenant may fill
//! that one day — but a clone of the host's warm template with a workspace
//! and its Browser in it holds about a third of a gigabyte: the guest's
//! kernel is small, its page cache is the template's, and the pages it shares
//! with every other clone are mapped from one file. Counting such a VM at its
//! size makes a node hold three of the forty it has memory for.
//!
//! So the host counts what its kernel measures. Each running VM's Firecracker
//! process is read every second ([`resident_mib`]): its *proportional set
//! size*, which splits a page two processes share half in each — right for
//! clones of one template, which share most of what they hold — and counts
//! nothing of the VM's disk, whose page cache belongs to files and is
//! reclaimable.
//!
//! Admission is then the sum of what the awake VMs are measured at, what the
//! VM being started is expected to take until it is measured ([`Budget`]),
//! and the host's own reserve. Under pressure — the memory the kernel says it
//! can still hand out below that reserve — the host first asks the guests'
//! balloons for a step each ([`next_inflate_mib`]), and refuses new VMs while
//! that is not enough. It never kills a workspace's VM: a refused wake waits
//! or goes to another host, and a running workspace keeps its VM, its Browser
//! and its open page.

/// What a clone of the host's warm template takes, MiB, with a workspace and
/// its Browser in it: the session, Chromium and the page, the rest of what it
/// holds being the image's pages, shared with every other clone (measured on
/// a host running the browser guest, `t test hosted-vm-measure`).
pub const CLONE_MIB: u64 = 325;

/// How much one balloon step moves, MiB: enough to matter against a reserve
/// in the gigabytes, and small enough that a guest the balloon is pushing
/// too far notices one step at a time.
pub const BALLOON_STEP_MIB: u64 = 256;

/// What a host keeps free, and what it counts a VM at before it is measured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    /// The memory this host has for its VMs: the node's, or `None` when only
    /// its jail's slots and its free memory bound it.
    pub node_mib: Option<u64>,
    /// Kept free for the host itself: the kernel, the service, each VM's
    /// helper processes (its network and share servers, which its
    /// Firecracker process's size does not count), the page tables mapping
    /// every guest's memory, and the room a balloon needs to give memory back
    /// before admission stops.
    pub reserve_mib: u64,
    /// Added to [`CLONE_MIB`] for what a VM is counted at until it is
    /// measured: a VM that starts using what it was given between two
    /// measurements — or that boots cold, at about twice a clone's cost —
    /// must not be what pushes the node over.
    pub headroom_mib: u64,
}

impl Default for Budget {
    /// No node size (a host whose jail's slots bound it), two gigabytes kept
    /// free, and a mebibyte of headroom for every mebibyte a clone costs.
    fn default() -> Self {
        Self {
            node_mib: None,
            reserve_mib: Self::DEFAULT_RESERVE_MIB,
            headroom_mib: Self::DEFAULT_HEADROOM_MIB,
        }
    }
}

impl Budget {
    /// Two gigabytes: a kernel, this service and its ledger, a hundred VMs'
    /// helper processes, and enough slack that a balloon has time to work
    /// before the host stops admitting.
    pub const DEFAULT_RESERVE_MIB: u64 = 2048;

    /// A third of a clone on top of a clone: the measurement follows within a
    /// second, and this is what covers that second.
    pub const DEFAULT_HEADROOM_MIB: u64 = 128;

    /// The most the VMs may take together; `None` when only the slots and
    /// the free memory of the node bound them.
    #[must_use]
    pub fn for_vms_mib(&self) -> Option<u64> {
        self.node_mib
            .map(|node| node.saturating_sub(self.reserve_mib))
    }

    /// What one more VM is counted at until the host measures it. Its size
    /// bounds the CPU it gets and the memory it may grow into, not what it
    /// takes now.
    #[must_use]
    pub const fn expected_mib(&self) -> u64 {
        CLONE_MIB.saturating_add(self.headroom_mib)
    }

    /// What one more VM may take here now, `used_mib` being what the awake
    /// ones are counted at.
    #[must_use]
    pub fn free_mib(&self, used_mib: u64) -> u64 {
        self.for_vms_mib()
            .map_or(u64::MAX, |most| most.saturating_sub(used_mib))
    }

    /// Whether one more VM of `expected_mib` fits beside `used_mib`.
    #[must_use]
    pub fn admits(&self, used_mib: u64, expected_mib: u64) -> bool {
        self.for_vms_mib()
            .is_none_or(|most| used_mib.saturating_add(expected_mib) <= most)
    }
}

/// What the process `pid` holds of this host's memory now, MiB: its
/// proportional set size, from `/proc/PID/smaps_rollup`. `None` when it is
/// not there (yet, or any more).
#[must_use]
pub fn resident_mib(pid: u32) -> Option<u64> {
    let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    pss_mib(&rollup)
}

/// The `Pss:` of a `smaps_rollup`, in MiB, rounded up so a VM that holds any
/// of a mebibyte is counted for it.
#[must_use]
pub fn pss_mib(smaps_rollup: &str) -> Option<u64> {
    let kib = line_kib(smaps_rollup, "Pss:")?;
    Some(kib.saturating_add(1023) / 1024)
}

/// What this host can still hand out, MiB: `MemAvailable` of
/// `/proc/meminfo`, which counts the page cache of every VM's disk as the
/// reclaimable thing it is. `None` where the kernel says nothing.
#[must_use]
pub fn available_mib() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    Some(line_kib(&meminfo, "MemAvailable:")?.saturating_div(1024))
}

/// The kilobytes of the line starting `name` (`Pss: 1234 kB`).
fn line_kib(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with(name))?;
    let number = line.get(name.len()..)?.split_whitespace().next()?;
    number.parse().ok()
}

/// What a VM of `size_mib` should hold in its balloon next while the host is
/// short of memory: one step more than `current_mib`, and never more than
/// half the VM's memory. A guest holding its session and its Browser needs
/// the other half, and one the balloon still pushed too far gives the memory
/// back itself (Firecracker's `deflate_on_oom`), so a wrong step costs a
/// guest page's eviction and not the workspace.
#[must_use]
pub const fn next_inflate_mib(current_mib: u64, size_mib: u64) -> u64 {
    let ceiling = size_mib.saturating_div(2);
    let next = current_mib.saturating_add(BALLOON_STEP_MIB);
    if next < ceiling { next } else { ceiling }
}

/// What a VM's balloon should hold next once the host is not short of
/// memory: one step less than `current_mib`, giving the guest its memory
/// back.
#[must_use]
pub const fn next_deflate_mib(current_mib: u64) -> u64 {
    current_mib.saturating_sub(BALLOON_STEP_MIB)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host with a node of `node_mib`.
    fn budget(node_mib: u64) -> Budget {
        Budget {
            node_mib: Some(node_mib),
            ..Budget::default()
        }
    }

    #[test]
    fn admission_counts_a_vm_at_what_it_is_measured_at() {
        let budget = budget(16 * 1024);
        assert_eq!(
            budget.for_vms_mib(),
            Some(14 * 1024),
            "two gigabytes kept free"
        );
        assert_eq!(budget.expected_mib(), 453, "a clone and its headroom");
        // phase: what a clone takes, over and over, until one more would pass
        let mut admitted = 0;
        let mut used = 0;
        while budget.admits(used, budget.expected_mib()) {
            used = used.saturating_add(CLONE_MIB);
            admitted += 1;
        }
        assert_eq!(admitted, 43, "a 16 GiB node holds 43 clones, not 3 VMs");
        assert!(used <= 14 * 1024, "{used}");
        assert_eq!(budget.free_mib(used), 14 * 1024 - used);
        // phase: a VM that holds gigabytes is counted for them
        assert!(!budget.admits(14 * 1024, budget.expected_mib()));
        assert!(budget.admits(13 * 1024, budget.expected_mib()));
        // phase: a host with no node size is bounded by nothing here
        assert!(Budget::default().admits(1_000_000, 453));
        assert_eq!(Budget::default().free_mib(1_000), u64::MAX);
    }

    #[test]
    fn a_fleets_worth_of_clones_fits_the_nodes_it_has() {
        // The customer's scale: hundreds of workspaces, each holding a clone's
        // worth, across the nodes a stack runs them on.
        let budget = budget(64 * 1024); // a D16s_v5
        let mut admitted = 0;
        let mut used = 0;
        while budget.admits(used, budget.expected_mib()) {
            used = used.saturating_add(CLONE_MIB);
            admitted += 1;
        }
        assert_eq!(admitted, 194);
        let eight_nodes = admitted * 8;
        assert!(
            eight_nodes > 1_500,
            "{eight_nodes} workspaces over eight nodes"
        );
    }

    #[test]
    fn a_vms_size_is_read_from_the_kernels_own_accounting() {
        let rollup = "00400000-7fffffff ---p 00000000 00:00 0\n\
                      Rss:             700000 kB\n\
                      Pss:             332800 kB\n\
                      Pss_Dirty:        1024 kB\n";
        assert_eq!(pss_mib(rollup), Some(325));
        assert_eq!(pss_mib("Rss: 100 kB\n"), None);
        // phase: a part of a mebibyte is a mebibyte of the host's memory
        assert_eq!(pss_mib("Pss: 1 kB\n"), Some(1));
        assert_eq!(pss_mib("Pss: 0 kB\n"), Some(0));
        // phase: the host's free memory is what MemAvailable says
        let meminfo = "MemTotal:       16384000 kB\nMemFree:  100000 kB\n\
                       MemAvailable:    8388608 kB\n";
        assert_eq!(line_kib(meminfo, "MemAvailable:"), Some(8_388_608));
        assert_eq!(line_kib(meminfo, "MemFree:"), Some(100_000));
        assert_eq!(line_kib(meminfo, "SwapTotal:"), None);
    }

    #[test]
    fn the_balloon_takes_steps_and_never_more_than_half_the_vm() {
        assert_eq!(next_inflate_mib(0, 4096), 256);
        assert_eq!(next_inflate_mib(1792, 4096), 2048, "half of a small VM");
        assert_eq!(next_inflate_mib(2048, 4096), 2048, "no further than half");
        assert_eq!(next_inflate_mib(0, 8192), 256);
        assert_eq!(next_inflate_mib(4000, 8192), 4096);
        assert_eq!(next_deflate_mib(512), 256);
        assert_eq!(next_deflate_mib(256), 0);
        assert_eq!(next_deflate_mib(0), 0);
    }
}
