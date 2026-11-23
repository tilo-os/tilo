//! "Kim opened Notes": a hint at the edge of the strip when someone else opens a window out of
//! sight.
//! On a shared desktop the view stays where the person left it when someone else opens a window:
//! being pulled along would take them away from what they were doing. A window opened off to one
//! side then changed nothing they could see. Now, for a few seconds, a small pill at that edge says
//! who opened what, in their presence colour, with an arrow toward it; a click on it brings the
//! window into view. It never covers more than its own pill, takes no focus, and goes once the
//! window is seen or its time is up.

use std::collections::{HashMap, HashSet};

use tilo_desktop_model::{Layout, TabId};
use ui::time::{Duration, Instant};

use super::DesktopHandle;

/// How long a hint stays: long enough to notice from the corner of an eye and decide whether to
/// look, short enough not to linger as clutter.
pub const ARRIVAL_HOLD: Duration = Duration::from_secs(8);

/// A window someone else opened, and when it arrived here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    pub tab: TabId,
    pub at: Instant,
    /// The window was brought into view as one the person's own agent opened
    /// (the surface reveals them).
    pub revealed: bool,
}

/// Which edge of the strip a window lies beyond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// The windows `tree` has in strips `before` already had, which neither `before` nor this page
/// (`local`) holds: what someone else just opened.
pub fn opened_elsewhere(before: &Layout, tree: &Layout, local: &[TabId]) -> Vec<TabId> {
    let strips = |tree: &Layout| -> HashMap<u64, HashSet<TabId>> {
        tree.workspaces
            .iter()
            .flat_map(|w| &w.strips)
            .map(|s| {
                let tabs = s
                    .window_groups
                    .iter()
                    .flat_map(|g| g.tabs.iter().map(|t| t.id))
                    .collect();
                (s.id.0, tabs)
            })
            .collect()
    };
    let known = strips(before);
    let mut opened: Vec<TabId> = strips(tree)
        .into_iter()
        .filter_map(|(strip, tabs)| Some((known.get(&strip)?, tabs)))
        .flat_map(|(had, tabs)| tabs.into_iter().filter(move |tab| !had.contains(tab)))
        .filter(|tab| !local.contains(tab))
        .collect();
    opened.sort_unstable();
    opened
}

impl DesktopHandle {
    /// Note `tabs` as just opened by someone else, for the edge hint.
    pub fn note_arrivals(&self, tabs: Vec<TabId>, at: Instant) {
        if tabs.is_empty() {
            return;
        }
        let mut shared = self.shared.borrow_mut();
        shared
            .arrivals
            .retain(|arrival| !tabs.contains(&arrival.tab));
        shared.arrivals.extend(tabs.into_iter().map(|tab| Arrival {
            tab,
            at,
            revealed: false,
        }));
    }
}

/// The name of the window showing `tab`, by the one rule every surface names a window by.
pub fn title_of(tree: &Layout, tab: TabId) -> Option<String> {
    tree.workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|s| &s.window_groups)
        .flat_map(|g| &g.tabs)
        .find(|t| t.id == tab)
        .map(super::window_title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{IdMinter, StripId, Tab, TileContent, WindowGroup, WindowWidth};

    fn workspace_window(group: u64, tab: TabId, title: &str) -> WindowGroup {
        WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: title.into(),
                tile: TileContent::LocalApp {
                    app_id: format!("so.tilo.{}", title.to_lowercase()),
                    label: title.into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    fn desktop() -> Layout {
        let mut minter = IdMinter::new(7);
        let tab = Tab {
            id: minter.mint(),
            title: "App0".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.app0".into(),
                label: "App0".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        crate::desktop::seed_tree(&mut minter, "Office", vec![tab])
    }

    /// Only what someone else opened in a strip already here is pointed at: not this page's own
    /// windows, and not a whole strip arriving.
    #[test]
    fn only_windows_others_open_in_a_known_strip_are_arrivals() {
        let before = desktop();
        let mut after = before.clone();
        let (own, theirs) = (9_100_001, 9_100_002);
        for (group, tab) in [(9_100_100, own), (9_100_200, theirs)] {
            after.workspaces[0].strips[0]
                .window_groups
                .push(workspace_window(group, tab, "Notes"));
        }
        assert_eq!(opened_elsewhere(&before, &after, &[own]), vec![theirs]);
        let mut new_strip = after;
        new_strip.workspaces[0].strips[0].id = StripId(9_100_900);
        assert!(opened_elsewhere(&before, &new_strip, &[]).is_empty());
    }
}
