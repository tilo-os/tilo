//! The window the person works in stays whole on its display as the strip changes around it.

use super::GroupRow;
use crate::time::Instant;
use crate::tokens::MotionSource;

/// A group standing in a run of the strip: its id, its run (`None` on an unpartitioned row), the
/// width it is going to in half pixels, and whether it fills the run ([`GroupRow::keep_in_view`]).
pub(super) type RunPlace = (u64, Option<usize>, f64, bool);

/// Whether group `id` moved from where it stood in the runs `was` to where it stands in `now`: onto
/// another run, past a window standing in both, or to another width. A window that came or went
/// beside it moves it nowhere.
fn moved_between(was: &[RunPlace], now: &[RunPlace], id: u64) -> bool {
    let (Some(from), Some(to)) = (
        was.iter().find(|run| run.0 == id),
        now.iter().find(|run| run.0 == id),
    ) else {
        return false;
    };
    let before = |runs: &[RunPlace], other: &[RunPlace], lane| {
        runs.iter()
            .take_while(|run| run.0 != id)
            .filter(|run| run.1 == lane && other.iter().any(|o| o.0 == run.0))
            .map(|run| run.0)
            .collect::<Vec<_>>()
    };
    from != to || before(was, now, from.1) != before(now, was, to.1)
}

impl GroupRow {
    /// Keep `group_id`, the window the person works in, wholly in view: once a layout places it
    /// elsewhere than the last one did (moved along the strip or onto another display, or to
    /// another width), its run pans to it as focusing it does. A window that only took the focus
    /// is not moved to: focus brings its window into view itself, and a pan the person made is
    /// theirs. Nothing moves while a window is carried, an edge dragged or the strip under the
    /// fingers. Call after a layout pass.
    pub fn keep_in_view(&mut self, group_id: Option<u64>, now: Instant, source: MotionSource) {
        if self.group_drag.is_some() || self.edge_drag.is_some() || self.direct_pan_in_progress() {
            return;
        }
        let Some(runs) = self.laid_runs() else {
            return;
        };
        let moved = group_id.filter(|&id| moved_between(&self.kept_runs, &runs, id));
        self.kept_runs = runs;
        if let Some(id) = moved {
            self.scroll_group_into_view_animated(id, now, source);
        }
    }

    /// The groups standing in the strip's runs, in order: each with its run, the width it is
    /// going to (in half pixels, so a width spring's rounding is no move) and whether it fills the
    /// run. `None` until a layout has laid the runs out in this order.
    fn laid_runs(&self) -> Option<Vec<RunPlace>> {
        let floating = self.floating_flags();
        let runs: Vec<RunPlace> = self
            .groups
            .iter()
            .enumerate()
            .filter(|&(i, group)| floating.get(i) != Some(&true) && !group.is_closing())
            .map(|(_, group)| {
                let width = (group.dest_width_px() * 2.0).round();
                (
                    group.id,
                    self.lane_of_group(group.id),
                    width,
                    group.maximized,
                )
            })
            .collect();
        let standing = |id: &u64| runs.iter().any(|run| run.0 == *id);
        (0..self.lanes.len().max(1))
            .all(|k| {
                let lane = (!self.lanes.is_empty()).then_some(k);
                let laid = self.pan_of_lane(k).laid_spans.iter().map(|&(id, _, _)| id);
                laid.filter(standing)
                    .eq(runs.iter().filter(|run| run.1 == lane).map(|run| run.0))
            })
            .then_some(runs)
    }
}
