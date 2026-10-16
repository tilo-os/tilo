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

/// The closest offset within `threshold` that lines one of `edges` up with
/// one of `targets`.
fn nearest_offset(
    edges: [i32; 2],
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

/// Pull `moving` onto nearby edges of `others`: its left or right edge onto
/// another display's left or right edge, and likewise top and bottom. Each
/// axis snaps independently to the closest edge within `threshold`.
pub fn snap(moving: Placement, others: &[Placement], threshold: i32) -> Placement {
    let dx = nearest_offset(
        [moving.x, moving.right()],
        others.iter().flat_map(|other| [other.x, other.right()]),
        threshold,
    )
    .unwrap_or(0);
    let dy = nearest_offset(
        [moving.y, moving.bottom()],
        others.iter().flat_map(|other| [other.y, other.bottom()]),
        threshold,
    )
    .unwrap_or(0);
    moving.at(moving.x.saturating_add(dx), moving.y.saturating_add(dy))
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

/// Where a dropped display settles: snapped to nearby edges, clear of every
/// other display, and touching at least one of them.
pub fn settle(moving: Placement, others: &[Placement], threshold: i32) -> Placement {
    let snapped = snap(moving, others, threshold);
    let clear = resolve_collisions(snapped, others);
    let attached = attach(clear, others);
    resolve_collisions(attached, others)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: Placement = Placement::new(0, 0, 1920, 1080);

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
}
