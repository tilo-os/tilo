//! A window an app asks for: it lands after the focused window of the shown
//! strip, which it makes when the workspace holds none yet.

/// Where a window opened at a point of the shown strip goes: before the
/// first window whose middle is right of the point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StripPart {
    /// Its place among the strip's windows, as [`DesktopHandle::open_at`]
    /// takes it.
    pub index: usize,
    /// Where it opens, in the surface's coordinates: the middle of the gap
    /// between the windows it goes between, or the point itself in a strip
    /// with none shown.
    pub x: f64,
}

/// The slot at `x` among windows at `windows` (each window's place
/// in its strip and its left and right edge, in strip order).
#[must_use]
pub fn part_among(windows: &[(usize, f64, f64)], x: f64) -> StripPart {
    let before = windows
        .iter()
        .rev()
        .find(|(_, left, right)| (left + right) / 2.0 <= x);
    let after = windows
        .iter()
        .find(|(_, left, right)| (left + right) / 2.0 > x);
    let index = before.map_or_else(
        || after.map_or(0, |(position, ..)| *position),
        |(position, ..)| position.saturating_add(1),
    );
    let x = match (before, after) {
        (Some((_, _, right)), Some((_, left, _))) => (right + left) / 2.0,
        (Some((_, _, right)), None) => right.max(x.min(right + 24.0)),
        (None, Some((_, left, _))) => left.min(x.max(left - 24.0)),
        (None, None) => x,
    };
    StripPart { index, x }
}

impl super::DesktopHandle {
    /// Where a window opened at `x` (the surface's coordinates) goes in the
    /// shown strip: what a drop of files on the shell between windows opens
    /// them at. `None` with no workspace shown.
    #[must_use]
    pub fn part_at(&self, x: f64) -> Option<StripPart> {
        let shared = self.shared.borrow();
        let workspace = shared.tree.active_workspace()?;
        let Some(strip) = shared
            .tree
            .active_strip_for(workspace)
            .and_then(|id| workspace.strips.iter().find(|strip| strip.id == id))
        else {
            return Some(StripPart { index: 0, x });
        };
        let windows: Vec<(usize, f64, f64)> = strip
            .window_groups
            .iter()
            .enumerate()
            .filter_map(|(position, group)| {
                let rect = shared.shown_tabs.window_rect(group.active_tab)?;
                Some((position, rect.x0, rect.x1))
            })
            .collect();
        Some(part_among(&windows, x))
    }

    /// Open `app` (an app id, or a launch naming a record or content) in a
    /// new window at the place `index` of the shown strip
    /// ([`Self::part_at`]).
    ///
    /// # Errors
    ///
    /// No workspace is shown, or `app` names no application.
    pub fn open_at(&self, app: &str, index: usize) -> Result<(), String> {
        let mut shared = self.shared.borrow_mut();
        shared
            .tree
            .active_workspace()
            .ok_or("Workspace unavailable")?;
        if app.is_empty() {
            return Err("Application unavailable".into());
        }
        shared
            .queue
            .push(crate::desktop::navigation::Admitted::LaunchAt(
                app.to_owned(),
                index,
            ));
        shared.wake.wake();
        Ok(())
    }
}

#[cfg(test)]
mod slot_tests {
    use super::{StripPart, part_among};

    #[test]
    fn a_point_between_windows_opens_between_them_and_the_ends_open_at_the_ends() {
        let windows = [(0, 0.0, 400.0), (1, 420.0, 820.0), (2, 840.0, 1240.0)];
        assert_eq!(
            part_among(&windows, 830.0),
            StripPart { index: 2, x: 830.0 }
        );
        assert_eq!(part_among(&windows, 100.0), StripPart { index: 0, x: 0.0 });
        assert_eq!(
            part_among(&windows, 1300.0),
            StripPart {
                index: 3,
                x: 1264.0
            }
        );
        assert_eq!(
            part_among(&windows, 1250.0),
            StripPart {
                index: 3,
                x: 1250.0
            }
        );
        assert_eq!(part_among(&windows, 700.0).index, 2);
        assert_eq!(part_among(&[], 300.0), StripPart { index: 0, x: 300.0 });
        // Windows scrolled out of view to the left still count before the point.
        let scrolled = [(3, -900.0, -500.0), (4, 100.0, 500.0)];
        assert_eq!(part_among(&scrolled, 600.0).index, 5);
        assert_eq!(part_among(&scrolled, 50.0).index, 4);
    }
}
