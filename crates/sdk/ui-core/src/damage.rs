//! Conservative native-pixel damage coalescing.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    fn area(self) -> u64 {
        u64::from(self.width).saturating_mul(u64::from(self.height))
    }

    fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self
            .x
            .saturating_add(self.width)
            .max(other.x.saturating_add(other.width));
        let y1 = self
            .y
            .saturating_add(self.height)
            .max(other.y.saturating_add(other.height));
        Self {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        }
    }
}

/// Merge only overlap/touching damage whose bounding union costs no more
/// pixels than the independent passes. The cap bounds the repeated-pair scan
/// while preserving far-apart scrollbars and overlays.
#[allow(
    clippy::indexing_slicing,
    reason = "the bounded two-index scan checks i < j < len before every access"
)]
pub(crate) fn coalesce_regions(mut regions: Vec<Region>) -> Vec<Region> {
    if regions.len() > 128 {
        return regions;
    }
    let mut changed = true;
    while changed {
        changed = false;
        'scan: for i in 0..regions.len() {
            for j in i.saturating_add(1)..regions.len() {
                let union = regions[i].union(regions[j]);
                if union.area() <= regions[i].area().saturating_add(regions[j].area()) {
                    regions[i] = union;
                    regions.swap_remove(j);
                    changed = true;
                    break 'scan;
                }
            }
        }
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::{Region, coalesce_regions};
    #[test]
    fn damage_regions_merge_only_when_that_saves_raster_work() {
        // Case `overlaps_merge_without_extra_raster_work`.
        {
            let result = coalesce_regions(vec![
                Region {
                    x: 10,
                    y: 76,
                    width: 100,
                    height: 40,
                },
                Region {
                    x: 10,
                    y: 96,
                    width: 100,
                    height: 40,
                },
            ]);
            assert_eq!(
                result,
                vec![Region {
                    x: 10,
                    y: 76,
                    width: 100,
                    height: 60
                }]
            );
        }

        // Case `near_but_gapped_regions_do_not_expand_work`.
        {
            let result = coalesce_regions(vec![
                Region {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                },
                Region {
                    x: 12,
                    y: 0,
                    width: 10,
                    height: 10,
                },
            ]);
            assert_eq!(result.len(), 2);
        }

        // Case `far_regions_stay_separate`.
        {
            let result = coalesce_regions(vec![
                Region {
                    x: 0,
                    y: 0,
                    width: 20,
                    height: 20,
                },
                Region {
                    x: 3_800,
                    y: 2_000,
                    width: 20,
                    height: 20,
                },
            ]);
            assert_eq!(result.len(), 2);
        }
    }
}
