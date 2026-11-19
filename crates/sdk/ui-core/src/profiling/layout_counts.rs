//! What the layout solver did, in counts rather than guesses. Frames of a long list or a shell are
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

thread_local! {
    static RESTYLED: std::cell::RefCell<Vec<(&'static str, u64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A widget of type `name` changed its own style after a solve read its size, so the tree was solved
/// again: a size the widget could have stated before the first solve.
pub(crate) fn restyled(name: &'static str) {
    RESTYLED.with(|restyled| {
        let mut restyled = restyled.borrow_mut();
        match restyled.iter_mut().find(|(held, _)| *held == name) {
            Some((_, count)) => *count = count.saturating_add(1),
            None => restyled.push((name, 1)),
        }
    });
}

/// The widget types that changed their style after a solve on this thread since the last call, and
/// how often, the most often first.
pub fn take_restyled() -> Vec<(&'static str, u64)> {
    let mut restyled = RESTYLED.with(|restyled| std::mem::take(&mut *restyled.borrow_mut()));
    restyled.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    restyled
}

/// The widget types under `root` that ask for layout on their own: each reports
/// [`crate::widget::Widget::is_layout_dirty`] while none of its children does, so a frame that
/// handles any input lays the window out for them. A settled window names none.
pub fn asking_for_layout(root: &dyn crate::widget::Widget) -> Vec<(&'static str, u64)> {
    fn walk(widget: &dyn crate::widget::Widget, found: &mut Vec<(&'static str, u64)>) {
        if !widget.is_layout_dirty() {
            return;
        }
        let mut child_asks = false;
        widget.for_each_child(&mut |child| {
            if child.is_layout_dirty() {
                child_asks = true;
                walk(child, found);
            }
        });
        if child_asks {
            return;
        }
        let name = widget.concrete_type_name();
        match found.iter_mut().find(|(held, _)| *held == name) {
            Some((_, count)) => *count = count.saturating_add(1),
            None => found.push((name, 1)),
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    found
}
