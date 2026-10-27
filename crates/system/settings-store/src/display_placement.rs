//! Pure geometry for arranging displays by dragging: edge snapping, overlap
//! resolution and keeping the desktop one connected area. Everything is in
//! desktop coordinates (the units `OutputConfig::x`/`y` are stored in), so
//! the rules are testable without a canvas.

/// One display's rectangle on the shared desktop plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Placement {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn right(self) -> i32 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(self) -> i32 {
        self.y.saturating_add(self.height)
    }

    #[must_use]
    pub const fn at(self, x: i32, y: i32) -> Self {
        Self { x, y, ..self }
    }

    /// Whether the interiors intersect. Rectangles that only share an edge
    /// do not overlap.
    pub fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// Whether the two share a stretch of edge, so the pointer can cross
    /// from one to the other.
    pub fn touches(self, other: Self) -> bool {
        let vertical_run = self.y < other.bottom() && other.y < self.bottom();
        let horizontal_run = self.x < other.right() && other.x < self.right();
        (vertical_run && (self.right() == other.x || other.right() == self.x))
            || (horizontal_run && (self.bottom() == other.y || other.bottom() == self.y))
    }

    /// Gap between the two along each axis; zero when their spans overlap.
    fn gaps(self, other: Self) -> (i32, i32) {
        let dx = other
            .x
            .saturating_sub(self.right())
            .max(self.x.saturating_sub(other.right()))
            .max(0);
        let dy = other
            .y
            .saturating_sub(self.bottom())
            .max(self.y.saturating_sub(other.bottom()))
            .max(0);
        (dx, dy)
    }
}

/// The lines a display offers along one axis: its two edges and its centre.
fn lines(start: i32, length: i32) -> [i32; 3] {
    [
        start,
        start.saturating_add(length / 2),
        start.saturating_add(length),
    ]
}

/// The closest offset within `threshold` that lines one of `edges` up with
/// one of `targets`.
fn nearest_offset(
    edges: [i32; 3],
    targets: impl Iterator<Item = i32>,
    threshold: i32,
) -> Option<i32> {
    let mut best: Option<i32> = None;
    for target in targets {
        for edge in edges {
            let offset = target.saturating_sub(edge);
            if offset.saturating_abs() <= threshold
                && best.is_none_or(|best| offset.saturating_abs() < best.saturating_abs())
            {
                best = Some(offset);
            }
        }
    }
    best
}

/// A line a dragged display lines up with: where it runs and how far, in
/// desktop units. The canvas draws it as an alignment guide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guide {
    /// A vertical line at `x == at`, else a horizontal one at `y == at`.
    pub vertical: bool,
    pub at: i32,
    /// Where the line starts and ends along its own direction: it spans
    /// both displays it joins.
    pub from: i32,
    pub to: i32,
}

/// Where a dragged display stands under the pointer, and what it lines up
/// with there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapped {
    pub placement: Placement,
    pub guides: Vec<Guide>,
}

/// Pull `moving` magnetically onto nearby lines of `others`: any of its
/// left edge, centre and right edge onto any of theirs, and likewise top,
/// middle and bottom. Each axis snaps on its own to the closest line within
/// `threshold`; beyond it the display stays exactly where it was put.
pub fn snap(moving: Placement, others: &[Placement], threshold: i32) -> Placement {
    snap_with_guides(moving, others, threshold).placement
}

/// [`snap`], with the guides along every line the snapped display now
/// shares with another.
pub fn snap_with_guides(moving: Placement, others: &[Placement], threshold: i32) -> Snapped {
    let dx = nearest_offset(
        lines(moving.x, moving.width),
        others.iter().flat_map(|other| lines(other.x, other.width)),
        threshold,
    );
    let dy = nearest_offset(
        lines(moving.y, moving.height),
        others.iter().flat_map(|other| lines(other.y, other.height)),
        threshold,
    );
    let placement = moving.at(
        moving.x.saturating_add(dx.unwrap_or(0)),
        moving.y.saturating_add(dy.unwrap_or(0)),
    );
    let mut guides: Vec<Guide> = Vec::new();
    let mut add = |guide: Guide| {
        if let Some(same) = guides
            .iter_mut()
            .find(|have| have.vertical == guide.vertical && have.at == guide.at)
        {
            same.from = same.from.min(guide.from);
            same.to = same.to.max(guide.to);
        } else {
            guides.push(guide);
        }
    };
    for other in others {
        if dx.is_some() {
            for line in lines(placement.x, placement.width) {
                if lines(other.x, other.width).contains(&line) {
                    add(Guide {
                        vertical: true,
                        at: line,
                        from: placement.y.min(other.y),
                        to: placement.bottom().max(other.bottom()),
                    });
                }
            }
        }
        if dy.is_some() {
            for line in lines(placement.y, placement.height) {
                if lines(other.y, other.height).contains(&line) {
                    add(Guide {
                        vertical: false,
                        at: line,
                        from: placement.x.min(other.x),
                        to: placement.right().max(other.right()),
                    });
                }
            }
        }
    }
    Snapped { placement, guides }
}

/// Whether `moving` may rest at its place among `others`: it covers none of
/// them and shares an edge with at least one, so the pointer can reach it.
/// Alone, any place is valid.
pub fn is_valid(moving: Placement, others: &[Placement]) -> bool {
    !others.iter().any(|other| moving.overlaps(*other))
        && (others.is_empty() || others.iter().any(|other| moving.touches(*other)))
}

/// Move `moving` out of every display it overlaps, each time along the
/// shortest way out that does not land on another display.
pub fn resolve_collisions(moving: Placement, others: &[Placement]) -> Placement {
    let mut current = moving;
    // Each step clears one overlap for good or moves strictly outward, so
    // a few passes per display always settle.
    for _ in 0..others.len().saturating_mul(4).max(1) {
        let Some(hit) = others
            .iter()
            .copied()
            .find(|other| current.overlaps(*other))
        else {
            return current;
        };
        let candidates = [
            current.at(hit.x.saturating_sub(current.width), current.y),
            current.at(hit.right(), current.y),
            current.at(current.x, hit.y.saturating_sub(current.height)),
            current.at(current.x, hit.bottom()),
        ];
        let distance = |candidate: &Placement| {
            i64::from(candidate.x)
                .abs_diff(i64::from(current.x))
                .saturating_add(i64::from(candidate.y).abs_diff(i64::from(current.y)))
        };
        current = candidates
            .iter()
            .copied()
            .filter(|candidate| !others.iter().any(|other| candidate.overlaps(*other)))
            .min_by_key(distance)
            .or_else(|| candidates.iter().copied().min_by_key(distance))
            .unwrap_or(current);
    }
    current
}

/// If `moving` touches none of `others`, slide it to the nearest one until
/// their edges meet: a display floating apart would leave a stretch of
/// desktop the pointer cannot reach.
pub fn attach(moving: Placement, others: &[Placement]) -> Placement {
    if others.is_empty() || others.iter().any(|other| moving.touches(*other)) {
        return moving;
    }
    let Some(nearest) = others.iter().copied().min_by_key(|other| {
        let (dx, dy) = moving.gaps(*other);
        i64::from(dx)
            .saturating_mul(i64::from(dx))
            .saturating_add(i64::from(dy).saturating_mul(i64::from(dy)))
    }) else {
        return moving;
    };
    let (dx, dy) = moving.gaps(nearest);
    let mut x = moving.x;
    let mut y = moving.y;
    if dx > 0 || dy == 0 {
        x = if moving.x >= nearest.right() {
            nearest.right()
        } else {
            nearest.x.saturating_sub(moving.width)
        };
        if moving.y >= nearest.bottom() || moving.bottom() <= nearest.y {
            // Diagonal: also bring the vertical spans into contact, bottom
            // edges aligned when it was below and top edges when above.
            y = if moving.y >= nearest.bottom() {
                nearest.bottom().saturating_sub(moving.height)
            } else {
                nearest.y
            };
        }
    } else {
        y = if moving.y >= nearest.bottom() {
            nearest.bottom()
        } else {
            nearest.y.saturating_sub(moving.height)
        };
    }
    moving.at(x, y)
}

/// Where a dropped display settles: snapped to nearby lines, clear of every
/// other display, and touching at least one of them. Always a valid place
/// ([`is_valid`]): when the shortest way out of one display runs into
/// another, it rests at the nearest place flush against a side of one.
pub fn settle(moving: Placement, others: &[Placement], threshold: i32) -> Placement {
    let snapped = snap(moving, others, threshold);
    let clear = resolve_collisions(snapped, others);
    let attached = attach(clear, others);
    let resolved = resolve_collisions(attached, others);
    if is_valid(resolved, others) {
        return resolved;
    }
    nearest_valid(snapped, others).unwrap_or(resolved)
}

/// The valid place nearest `moving`, among every place flush against a side
/// of another display. The outermost display's outer side is always free,
/// so there is one whenever there are others.
fn nearest_valid(moving: Placement, others: &[Placement]) -> Option<Placement> {
    let clamp = |value: i32, low: i32, high: i32| value.max(low).min(high.max(low));
    let distance = |candidate: &Placement| {
        let dx = i64::from(candidate.x).saturating_sub(i64::from(moving.x));
        let dy = i64::from(candidate.y).saturating_sub(i64::from(moving.y));
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    };
    others
        .iter()
        .flat_map(|other| {
            // Along a side, stay as near the drop as still shares a run.
            let y = clamp(
                moving.y,
                other.y.saturating_sub(moving.height).saturating_add(1),
                other.bottom().saturating_sub(1),
            );
            let x = clamp(
                moving.x,
                other.x.saturating_sub(moving.width).saturating_add(1),
                other.right().saturating_sub(1),
            );
            [
                moving.at(other.x.saturating_sub(moving.width), y),
                moving.at(other.right(), y),
                moving.at(x, other.y.saturating_sub(moving.height)),
                moving.at(x, other.bottom()),
            ]
        })
        .filter(|candidate| is_valid(*candidate, others))
        .min_by_key(distance)
}

/// A way an arrow key moves a display on the arrangement canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Left,
    Right,
    Up,
    Down,
}

/// Where an arrow key moves `moving`: the next valid place that way among
/// the places flush against a side of another display and aligned with its
/// start, centre or end. Of those, the one that stays most in line across
/// the step, then the nearest. `None` when nothing lies that way.
pub fn step(moving: Placement, others: &[Placement], way: Step) -> Option<Placement> {
    let across = |start: i32, length: i32, own: i32| {
        let slack = length.saturating_sub(own);
        [
            start,
            start.saturating_add(slack / 2),
            start.saturating_add(slack),
        ]
    };
    let gap = |a: i32, b: i32| i64::from(a).saturating_sub(i64::from(b)).abs();
    others
        .iter()
        .flat_map(|other| {
            let rows = across(other.y, other.height, moving.height);
            let columns = across(other.x, other.width, moving.width);
            rows.into_iter()
                .flat_map(move |y| {
                    [
                        moving.at(other.x.saturating_sub(moving.width), y),
                        moving.at(other.right(), y),
                    ]
                })
                .chain(columns.into_iter().flat_map(move |x| {
                    [
                        moving.at(x, other.y.saturating_sub(moving.height)),
                        moving.at(x, other.bottom()),
                    ]
                }))
        })
        .filter(|candidate| match way {
            Step::Left => candidate.x < moving.x,
            Step::Right => candidate.x > moving.x,
            Step::Up => candidate.y < moving.y,
            Step::Down => candidate.y > moving.y,
        })
        .filter(|candidate| is_valid(*candidate, others))
        .min_by_key(|candidate| match way {
            Step::Left | Step::Right => (gap(candidate.y, moving.y), gap(candidate.x, moving.x)),
            Step::Up | Step::Down => (gap(candidate.x, moving.x), gap(candidate.y, moving.y)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: Placement = Placement::new(0, 0, 1920, 1080);

    #[test]
    fn an_arrow_moves_a_display_to_the_next_place_that_way() {
        let right = Placement::new(1920, 0, 2560, 1440);
        assert_eq!(
            step(right, &[PANEL], Step::Left),
            Some(Placement::new(-2560, 0, 2560, 1440)),
            "to the other side, in line"
        );
        let below = step(right, &[PANEL], Step::Down).unwrap();
        assert_eq!(below.y, 1080, "under the panel");
        assert!(is_valid(below, &[PANEL]));
        assert_eq!(
            step(below, &[PANEL], Step::Down),
            None,
            "nothing further down"
        );
        assert_eq!(
            step(right, &[PANEL], Step::Right),
            None,
            "nothing further right"
        );
        for way in [Step::Left, Step::Right, Step::Up, Step::Down] {
            if let Some(next) = step(below, &[PANEL], way) {
                assert!(is_valid(next, &[PANEL]), "{way:?} lands touching and clear");
            }
        }
    }

    #[test]
    fn snapping_pulls_an_edge_onto_a_nearby_edge_only() {
        let near = Placement::new(1930, 12, 2560, 1440);
        assert_eq!(
            snap(near, &[PANEL], 24),
            Placement::new(1920, 0, 2560, 1440)
        );
        let far = Placement::new(2000, 300, 2560, 1440);
        assert_eq!(
            snap(far, &[PANEL], 24),
            far,
            "beyond the threshold nothing moves"
        );
    }

    #[test]
    fn snapping_aligns_bottom_edges() {
        let monitor = Placement::new(1920, -320, 2560, 1440);
        let snapped = snap(monitor, &[PANEL], 24);
        assert_eq!(
            snapped.bottom(),
            PANEL.bottom() + 40,
            "40 px is beyond the threshold"
        );
        let monitor = Placement::new(1920, -370, 2560, 1440);
        assert_eq!(snap(monitor, &[PANEL], 24).bottom(), PANEL.bottom());
    }

    #[test]
    fn an_overlapping_drop_takes_the_shortest_way_out() {
        let dropped = Placement::new(1500, 100, 1920, 1080);
        let resolved = resolve_collisions(dropped, &[PANEL]);
        assert_eq!(resolved, Placement::new(1920, 100, 1920, 1080));
        assert!(!resolved.overlaps(PANEL));
    }

    #[test]
    fn collisions_resolve_against_several_displays() {
        let right = Placement::new(1920, 0, 1920, 1080);
        let dropped = Placement::new(1000, 200, 1920, 1080);
        let resolved = resolve_collisions(dropped, &[PANEL, right]);
        assert!(!resolved.overlaps(PANEL) && !resolved.overlaps(right));
        assert_eq!(resolved.y, 1080, "down is the shortest way clear of both");
    }

    #[test]
    fn a_display_dropped_far_away_is_brought_back_into_contact() {
        let stray = Placement::new(5000, 100, 1280, 1024);
        let attached = attach(stray, &[PANEL]);
        assert_eq!(attached, Placement::new(1920, 100, 1280, 1024));
        assert!(attached.touches(PANEL));

        let above = attach(Placement::new(300, -3000, 1280, 1024), &[PANEL]);
        assert_eq!(above, Placement::new(300, -1024, 1280, 1024));

        let diagonal = attach(Placement::new(4000, 4000, 1280, 1024), &[PANEL]);
        assert_eq!(diagonal, Placement::new(1920, 56, 1280, 1024));
        assert!(diagonal.touches(PANEL), "{diagonal:?} must share an edge");
    }

    #[test]
    fn corner_contact_is_not_touching() {
        let corner = Placement::new(1920, 1080, 800, 600);
        assert!(!corner.touches(PANEL));
        assert!(Placement::new(1920, 1079, 800, 600).touches(PANEL));
        assert_eq!(
            attach(corner, &[PANEL]),
            Placement::new(1920, 480, 800, 600)
        );
    }

    #[test]
    fn settle_snaps_clears_and_attaches() {
        let right = Placement::new(1920, 0, 1920, 1080);
        let dropped = Placement::new(1905, 1090, 1920, 1080);
        let settled = settle(dropped, &[PANEL, right], 24);
        assert!(!settled.overlaps(PANEL) && !settled.overlaps(right));
        assert!(settled.touches(PANEL) || settled.touches(right));
        assert_eq!(
            (settled.x, settled.y),
            (1920, 1080),
            "snapped under the right display"
        );
    }

    #[test]
    fn centres_snap_too_and_each_shared_line_has_a_guide() {
        // A 1280-wide monitor dropped under the panel, its centre 10 px off
        // the panel's centre: it centres, flush below.
        let monitor = Placement::new(330, 1090, 1280, 720);
        let snapped = snap_with_guides(monitor, &[PANEL], 24);
        assert_eq!(snapped.placement, Placement::new(320, 1080, 1280, 720));
        assert!(
            snapped
                .guides
                .iter()
                .any(|guide| guide.vertical && guide.at == 960),
            "the shared centre line is shown: {:?}",
            snapped.guides
        );
        assert!(
            snapped
                .guides
                .iter()
                .any(|guide| !guide.vertical && guide.at == 1080),
            "the shared edge is shown: {:?}",
            snapped.guides
        );
        let far = snap_with_guides(Placement::new(2400, 300, 1280, 720), &[PANEL], 24);
        assert!(far.guides.is_empty(), "nothing lined up, no guide");
    }

    #[test]
    fn every_drop_settles_touching_and_clear_of_every_display() {
        // A row of three and a fourth display dropped all over the desk:
        // wherever it lands, it rests touching one and covering none.
        let others = [
            PANEL,
            Placement::new(1920, 0, 2560, 1440),
            Placement::new(4480, 200, 1280, 1024),
        ];
        let size = (1920, 1200);
        for x in (-6000..9000).step_by(370) {
            for y in (-4000..5000).step_by(290) {
                let dropped = Placement::new(x, y, size.0, size.1);
                let settled = settle(dropped, &others, 24);
                assert!(
                    is_valid(settled, &others),
                    "{dropped:?} settled at {settled:?}"
                );
                assert_eq!((settled.width, settled.height), size);
            }
        }
    }
}
