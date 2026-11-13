//! What the layout solver did, in counts rather than guesses. Frames of a long list or a desktop are
//! dominated by layout, and which part of the solver spends that is invisible in wall time: a check or
//! a test reads what a run cost here, and two builds of the same sources compare their numbers, on any
//! machine and in any profile.

use std::cell::Cell;

/// What the layout solver did on this thread since the counts were last taken ([`take`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct LayoutCounts {
    /// Tree solves: every [`crate::widget::compute_widget_layout`] call, the probes a manual region
    /// measures its children with included.
    pub solves: u64,
    /// Passes the settle loop ran over a tree: each solves the whole tree again, and stops once a pass
    /// changes nothing.
    pub passes: u64,
    /// Manual region solves: the policies (`Manual`, a side bar that places its parts) the settle loop
    /// ran.
    pub regions: u64,
    /// Manual regions whose policy was kept instead of run again: nothing it reads had changed since
    /// its last solve, so its answer stands.
    pub kept: u64,
    /// Child measurements solved in a probe tree.
    pub probes: u64,
    /// Taffy trees copied to probe in.
    pub copies: u64,
    /// Widget measure callbacks: what Taffy asked a widget's own measurement.
    pub measures: u64,
    /// Wall time inside solves, probes included.
    pub solve_nanos: u64,
}

impl LayoutCounts {
    const ZERO: Self = Self {
        solves: 0,
        passes: 0,
        regions: 0,
        kept: 0,
        probes: 0,
        copies: 0,
        measures: 0,
        solve_nanos: 0,
    };

    /// Add another reading to this one: a run that laid windows out on several threads.
    pub fn add(&mut self, other: Self) {
        self.solves = self.solves.saturating_add(other.solves);
        self.passes = self.passes.saturating_add(other.passes);
        self.regions = self.regions.saturating_add(other.regions);
        self.kept = self.kept.saturating_add(other.kept);
        self.probes = self.probes.saturating_add(other.probes);
        self.copies = self.copies.saturating_add(other.copies);
        self.measures = self.measures.saturating_add(other.measures);
        self.solve_nanos = self.solve_nanos.saturating_add(other.solve_nanos);
    }

    /// The time spent solving, in milliseconds.
    #[must_use]
    pub fn solve_ms(&self) -> u64 {
        self.solve_nanos / 1_000_000
    }
}

thread_local! {
    static COUNTS: Cell<LayoutCounts> = const { Cell::new(LayoutCounts::ZERO) };
}

/// What this thread's layout solver did since the last call, and a fresh count starts here.
pub fn take() -> LayoutCounts {
    COUNTS.with(|counts| counts.replace(LayoutCounts::ZERO))
}

/// One reading of this thread's counters, leaving them running.
pub fn read() -> LayoutCounts {
    COUNTS.with(Cell::get)
}

fn counted(change: impl FnOnce(&mut LayoutCounts)) {
    COUNTS.with(|counts| {
        let mut counts_now = counts.get();
        change(&mut counts_now);
        counts.set(counts_now);
    });
}

pub(crate) fn solved(nanos: u64) {
    counted(|counts| {
        counts.solves = counts.solves.saturating_add(1);
        counts.solve_nanos = counts.solve_nanos.saturating_add(nanos);
    });
}

pub(crate) fn pass() {
    counted(|counts| counts.passes = counts.passes.saturating_add(1));
}

pub(crate) fn region() {
    counted(|counts| counts.regions = counts.regions.saturating_add(1));
}

pub(crate) fn kept() {
    counted(|counts| counts.kept = counts.kept.saturating_add(1));
}

pub(crate) fn probe() {
    counted(|counts| counts.probes = counts.probes.saturating_add(1));
}

pub(crate) fn copy() {
    counted(|counts| counts.copies = counts.copies.saturating_add(1));
}

pub(crate) fn measure() {
    counted(|counts| counts.measures = counts.measures.saturating_add(1));
}
