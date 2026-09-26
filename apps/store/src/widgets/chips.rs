//! Filter chip row — the `[All] [Tilo] [Flatpak] [Installed]` band.
//!
//! Implemented as a `ToggleGroup` because the segments are
//! mutually-exclusive and the widget already draws focus, active-state
//! and disabled treatments in a way the OS theme owns.

use ui::widget::ToggleGroup;
use ui::widget::toggle_group::Segment;

use crate::model::SourceFilter;

/// Result of a chip click, threaded back into the app via
/// [`FilterChipSelection::from_segment_index`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterChipSelection(pub SourceFilter);

impl FilterChipSelection {
    /// Map a raw segment index back into a `SourceFilter`. Order
    /// matches [`SourceFilter::ALL`]. Out-of-range → `All`.
    pub fn from_segment_index(idx: usize) -> Self {
        FilterChipSelection(
            SourceFilter::ALL
                .get(idx)
                .copied()
                .unwrap_or(SourceFilter::All),
        )
    }
}

/// Build the chip row for the given active filter.
pub fn filter_chip_row(active: SourceFilter) -> ToggleGroup {
    let segments: Vec<Segment> = SourceFilter::ALL
        .iter()
        .map(|f| Segment::new(f.label()))
        .collect();
    let selected_idx = SourceFilter::ALL
        .iter()
        .position(|f| *f == active)
        .unwrap_or(0);
    ToggleGroup::new(segments, selected_idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Segment indices round-trip and out-of-range falls back to All.
    #[test]
    fn segment_index_round_trips_and_falls_back_to_all() {
        // Case `segment_index_round_trips_through_the_enum`.
        {
            for (i, want) in SourceFilter::ALL.iter().enumerate() {
                assert_eq!(FilterChipSelection::from_segment_index(i).0, *want);
            }
        }

        // Case `out_of_range_segment_index_falls_back_to_all`.
        {
            assert_eq!(
                FilterChipSelection::from_segment_index(99).0,
                SourceFilter::All
            );
        }
    }
}
