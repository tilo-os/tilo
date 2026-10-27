//! Conservative native-pixel damage coalescing, and where a relayout moved
//! widgets.

use vello::kurbo::Rect;

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

// What a relayout moved, from every widget's layout box before and after:
// the native app host's own window damage and a tile's hint for the app it
// hosts both say where moved widgets were and are with these.

/// A widget's identity within one tree walk: its address and its depth (a
/// widget held at the start of its parent shares the parent's address).
pub(crate) type WidgetIdentity = (*const (), usize);

/// Every widget that may paint in view, in tree order, by identity, with its
/// layout box: in a long scrolled collection only the children near its
/// viewport ([`crate::widget::Widget::for_each_child_in_clip`]), so the walk
/// costs what is shown, not what is held.
pub(crate) fn widget_boxes(root: &dyn crate::widget::Widget) -> Vec<(WidgetIdentity, Rect)> {
    fn walk(
        widget: &dyn crate::widget::Widget,
        depth: usize,
        out: &mut Vec<(WidgetIdentity, Rect)>,
    ) {
        out.push((
            (std::ptr::from_ref(widget).cast::<()>(), depth),
            widget.get_rect(),
        ));
        widget.for_each_child_in_clip(&mut |child| walk(child, depth.saturating_add(1), out));
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

/// Every widget whose own damage covers a change to its box this frame
/// ([`crate::widget::Widget::damage_covers_own_box`]), by identity as [`widget_boxes`] has it.
#[cfg(feature = "native")]
pub(crate) fn boxes_covered_by_own_damage(
    root: &dyn crate::widget::Widget,
) -> std::collections::HashSet<WidgetIdentity> {
    fn walk(
        widget: &dyn crate::widget::Widget,
        depth: usize,
        out: &mut std::collections::HashSet<WidgetIdentity>,
    ) {
        if widget.damage_covers_own_box() {
            out.insert((std::ptr::from_ref(widget).cast::<()>(), depth));
        }
        widget.for_each_child_in_clip(&mut |child| walk(child, depth.saturating_add(1), out));
    }
    let mut out = std::collections::HashSet::new();
    walk(root, 0, &mut out);
    out
}

/// The old and new box of every widget that a relayout (and any rebuild)
/// moved, and the box of every widget that came or went, matched by
/// identity: a row a scroll brought near the view comes, one it took away
/// goes.
pub(crate) fn changed_boxes(
    before: &[(WidgetIdentity, Rect)],
    after: &[(WidgetIdentity, Rect)],
) -> Vec<Rect> {
    let old: std::collections::HashMap<_, _> = before.iter().copied().collect();
    let new: std::collections::HashMap<_, _> = after.iter().copied().collect();
    let mut changed = Vec::new();
    for (widget, rect) in after {
        match old.get(widget) {
            Some(was) if was == rect => {}
            Some(was) => changed.extend([*was, *rect]),
            None => changed.push(*rect),
        }
    }
    changed.extend(
        before
            .iter()
            .filter(|(widget, _)| !new.contains_key(widget))
            .map(|(_, rect)| *rect),
    );
    changed
}

#[cfg(test)]
mod tests {
    use super::{Region, coalesce_regions};
    #[test]
    fn damage_regions_merge_only_when_that_saves_raster_work() {
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
