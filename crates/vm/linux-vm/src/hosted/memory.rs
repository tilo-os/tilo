//! What the VMs on a host really take of its memory, and what it may still admit. A
//! clone of the warm template holds about a third of a gigabyte, so the host counts
//! each VM's proportional set size rather than its configured size; under pressure
//! it asks the guests' balloons for a step each and refuses new VMs while that is
//! not enough. It never kills a workspace's VM.

/// What a clone of the host's warm template takes, MiB, with a workspace and its Browser in it,
/// measured on a host running the browser guest (`t test hosted-vm-measure`).
pub const CLONE_MIB: u64 = 325;

/// How much one balloon step moves, MiB: enough to matter against a reserve in the gigabytes.
pub const BALLOON_STEP_MIB: u64 = 256;

/// What a host keeps free, and what it counts a VM at before it is measured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    /// The memory this host has for its VMs: the node's, or `None` when only
    /// its jail's slots and its free memory bound it.
    pub node_mib: Option<u64>,
    /// Kept free for the host: its kernel, the service, each VM's helper processes and page tables,
    /// and room for a balloon to give memory back.
    pub reserve_mib: u64,
    /// Added to [`CLONE_MIB`] until a VM is measured: a cold boot costs about twice a clone.
    pub headroom_mib: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            node_mib: None,
            reserve_mib: Self::DEFAULT_RESERVE_MIB,
            headroom_mib: Self::DEFAULT_HEADROOM_MIB,
        }
    }
}

impl Budget {
    pub const DEFAULT_RESERVE_MIB: u64 = 2048;

    /// A third of a clone on top of a clone: the measurement follows within a
    /// second, and this is what covers that second.
    pub const DEFAULT_HEADROOM_MIB: u64 = 128;

    #[must_use]
    pub fn for_vms_mib(&self) -> Option<u64> {
        self.node_mib
            .map(|node| node.saturating_sub(self.reserve_mib))
    }

    /// What one more VM is counted at until the host measures it. Its size bounds the CPU it gets
    /// and the memory it may grow into, not what it takes now.
    #[must_use]
    pub const fn expected_mib(&self) -> u64 {
        CLONE_MIB.saturating_add(self.headroom_mib)
    }

    #[must_use]
    pub fn admits(&self, used_mib: u64, expected_mib: u64) -> bool {
        self.for_vms_mib()
            .is_none_or(|most| used_mib.saturating_add(expected_mib) <= most)
    }
}

/// What the process `pid` holds of this host's memory now, MiB: its proportional set size, from
/// `/proc/PID/smaps_rollup`.
#[must_use]
pub fn resident_mib(pid: u32) -> Option<u64> {
    let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    pss_mib(&rollup)
}

/// What `pid` and every process below it hold, MiB: a jailed VM is the jailer, Firecracker and its
/// helper processes, and closing the VM frees all of it.
#[must_use]
pub fn tree_mib(pid: u32) -> Option<u64> {
    let mut total = 0_u64;
    let mut found = false;
    let mut next = vec![pid];
    while let Some(pid) = next.pop() {
        if let Some(mib) = resident_mib(pid) {
            total = total.saturating_add(mib);
            found = true;
            // The VM's processes are below the one the service holds.
            let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .unwrap_or_default();
            next.extend(
                children
                    .split_whitespace()
                    .filter_map(|child| child.parse::<u32>().ok()),
            );
        }
    }
    found.then_some(total)
}

/// The `Pss:` of a `smaps_rollup`, in MiB, rounded up so a VM that holds any
/// of a mebibyte is counted for it.
#[must_use]
pub fn pss_mib(smaps_rollup: &str) -> Option<u64> {
    let kib = line_kib(smaps_rollup, "Pss:")?;
    Some(kib.saturating_add(1023) / 1024)
}

/// What this host can still hand out, MiB: `MemAvailable`, which counts every VM's disk page cache
/// as reclaimable.
#[must_use]
pub fn available_mib() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    Some(line_kib(&meminfo, "MemAvailable:")?.saturating_div(1024))
}

fn line_kib(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with(name))?;
    let number = line.get(name.len()..)?.split_whitespace().next()?;
    number.parse().ok()
}

/// What a VM of `size_mib` should hold in its balloon next while the host is short: one step more,
/// never more than half the VM's memory, since a guest pushed too far gives memory back itself
/// (Firecracker's `deflate_on_oom`).
#[must_use]
pub const fn next_inflate_mib(current_mib: u64, size_mib: u64) -> u64 {
    let ceiling = size_mib.saturating_div(2);
    let next = current_mib.saturating_add(BALLOON_STEP_MIB);
    if next < ceiling { next } else { ceiling }
}

#[must_use]
pub const fn next_deflate_mib(current_mib: u64) -> u64 {
    current_mib.saturating_sub(BALLOON_STEP_MIB)
}
