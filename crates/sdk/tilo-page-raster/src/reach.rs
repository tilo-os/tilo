//! Which pages a frame wants rendered: those on screen, then those the
//! scroll reaches next.

/// The direction the view last scrolled, which decides where it reaches
/// next: a screen ahead in that direction, half a screen behind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Heading {
    last_y: f64,
    sign: f64,
}

impl Default for Heading {
    fn default() -> Self {
        Self {
            last_y: 0.0,
            sign: 1.0,
        }
    }
}

impl Heading {
    /// Note the view's vertical scroll offset this frame.
    pub fn follow(&mut self, scroll_y: f64) {
        if (scroll_y - self.last_y).abs() > f64::EPSILON {
            self.sign = (scroll_y - self.last_y).signum();
            self.last_y = scroll_y;
        }
    }

    /// How far beyond a view `height` tall pages are rendered ahead of
    /// time: `(below, above)` its bottom and top edges.
    pub fn reach(&self, height: f64) -> (f64, f64) {
        if self.sign < 0.0 {
            (height * 0.5, height)
        } else {
            (height, height * 0.5)
        }
    }
}

/// The pages within reach of a view, each nearest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InReach {
    /// Pages on screen, with the distance of their middle from the view's.
    pub visible: Vec<(usize, f64)>,
    /// Pages the scroll reaches next, with their distance from the view.
    pub prefetch: Vec<(usize, f64)>,
}

/// The pages among `candidates` within reach of the view spanning `view`
/// (`top, bottom` in screen pixels): on screen, or within `reach` (`below,
/// above`, as [`Heading::reach`] gives) of it. `extent` gives a page's
/// screen `top, bottom`. A page on screen joins `visible` unless `on_screen`
/// (given its index and top) takes it, such as a page drawn from tiles.
pub fn pages_in_reach(
    candidates: impl IntoIterator<Item = usize>,
    (view_top, view_bottom): (f64, f64),
    (below, above): (f64, f64),
    extent: impl Fn(usize) -> Option<(f64, f64)>,
    mut on_screen: impl FnMut(usize, f64) -> bool,
) -> InReach {
    let mut reach = InReach::default();
    let centre = f64::midpoint(view_top, view_bottom);
    for i in candidates {
        let Some((top, bottom)) = extent(i) else {
            continue;
        };
        if top <= view_bottom && bottom >= view_top {
            if !on_screen(i, top) {
                reach
                    .visible
                    .push((i, (f64::midpoint(top, bottom) - centre).abs()));
            }
        } else if top <= view_bottom + below && bottom >= view_top - above {
            let distance = if top > view_bottom {
                top - view_bottom
            } else {
                view_top - bottom
            };
            reach.prefetch.push((i, distance));
        }
    }
    reach.visible.sort_by(|a, b| a.1.total_cmp(&b.1));
    reach.prefetch.sort_by(|a, b| a.1.total_cmp(&b.1));
    reach
}

/// The index range of pages stacked at content `tops` (ascending) with
/// content `bottoms` that may meet `top..=bottom` in content space, found by
/// bisection so a long document costs nothing per frame.
pub fn stacked_span(
    tops: &[f64],
    bottoms: &[f64],
    top: f64,
    bottom: f64,
) -> std::ops::Range<usize> {
    let first = bottoms.partition_point(|b| *b < top);
    let end = tops.partition_point(|t| *t <= bottom);
    first..end.max(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_reaches_ahead_of_the_scroll_and_takes_visible_pages_nearest_first() {
        let mut heading = Heading::default();
        assert_eq!(heading.reach(800.0), (800.0, 400.0));
        heading.follow(500.0);
        heading.follow(300.0);
        assert_eq!(heading.reach(800.0), (400.0, 800.0));

        // Pages 1000 px tall with 20 px gaps; the view shows 0..800 of a
        // stack scrolled to 2500.
        let extent = |i: usize| {
            let i = f64::from(u32::try_from(i).ok()?);
            Some((i * 1020.0 - 2500.0, i * 1020.0 - 1500.0))
        };
        let reach = pages_in_reach(0..10, (0.0, 800.0), (800.0, 400.0), extent, |_, _| false);
        assert_eq!(
            reach.visible.iter().map(|v| v.0).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(
            reach.prefetch.iter().map(|v| v.0).collect::<Vec<_>>(),
            vec![4]
        );

        let mut tiled = Vec::new();
        let reach = pages_in_reach(0..10, (0.0, 800.0), (800.0, 400.0), extent, |i, _| {
            tiled.push(i);
            i == 2
        });
        assert_eq!(
            reach.visible.iter().map(|v| v.0).collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(tiled, vec![2, 3]);
    }

    #[test]
    fn a_stacked_span_bisects_to_the_pages_that_meet_it() {
        let tops: Vec<f64> = (0..100).map(|i| f64::from(i) * 110.0).collect();
        let bottoms: Vec<f64> = tops.iter().map(|t| t + 100.0).collect();
        assert_eq!(stacked_span(&tops, &bottoms, 0.0, 50.0), 0..1);
        assert_eq!(stacked_span(&tops, &bottoms, 105.0, 225.0), 1..3);
        assert_eq!(stacked_span(&tops, &bottoms, 20_000.0, 30_000.0), 100..100);
    }
}
