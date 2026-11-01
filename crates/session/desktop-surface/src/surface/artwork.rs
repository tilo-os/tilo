//! The artwork each window brought of its own: the icon a program set for its
//! window, which that window's tab paints in place of its app kind's glyph,
//! and which the switcher shows
//! ([`DesktopHandle::set_window_artwork`]). Never part of the tree: a desk does
//! not keep a window's icon after the window it belonged to is gone.

use super::{DesktopHandle, DesktopSurface};
use tilo_desktop_model::TabId;

impl DesktopHandle {
    /// Give `window` the artwork it brought of its own — the icon a program
    /// set for its window — or take it away when it sets none; `window` is the
    /// compositor's id for it. The next pass paints it in the tab it holds,
    /// and wherever else a window's app is shown. Whether that changed
    /// anything.
    pub fn set_window_artwork(&self, window: u64, icon: Option<ui::icons::AppImage>) -> bool {
        let mut shared = self.shared.borrow_mut();
        let same = match (&icon, shared.artwork.get(&window)) {
            (Some(icon), Some(shown)) => *icon == *shown,
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        match icon {
            Some(icon) => {
                shared.artwork.insert(window, icon);
            }
            None => {
                shared.artwork.remove(&window);
            }
        }
        true
    }

    /// The artwork `window` brought of its own, as the compositor's id for it
    /// last said.
    #[must_use]
    pub fn window_artwork(&self, window: u64) -> Option<ui::icons::AppImage> {
        self.shared.borrow().artwork.get(&window).cloned()
    }
}

impl DesktopSurface {
    /// Paint each window's own artwork in its tab, and only there: a window
    /// whose program set an icon shows it, one that set none shows its app's
    /// kind. Whether that changed a tab.
    pub(super) fn sync_window_artwork(&mut self) -> bool {
        let wanted: std::collections::HashMap<TabId, ui::icons::AppImage> = {
            let shared = self.shared.borrow();
            let wearing = self.rows.values().any(|row| {
                row.groups
                    .iter()
                    .any(|group| group.tabs().iter().any(|tab| tab.icon.is_some()))
            });
            if shared.artwork.is_empty() && !wearing {
                return false;
            }
            shared
                .tree
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .filter_map(|tab| {
                    let tilo_desktop_model::TileContent::Foreign { surface_id, .. } = &tab.tile
                    else {
                        return None;
                    };
                    Some((tab.id, shared.artwork.get(surface_id)?.clone()))
                })
                .collect()
        };
        let mut changed = false;
        for row in self.rows.values_mut() {
            for group in &mut row.groups {
                let tabs: Vec<(TabId, Option<ui::icons::AppImage>)> = group
                    .tabs()
                    .iter()
                    .map(|tab| (tab.id, tab.icon.clone()))
                    .collect();
                for (tab, worn) in tabs {
                    let wanted = wanted.get(&tab).cloned();
                    if worn != wanted {
                        changed |= group.set_tab_icon(tab, wanted);
                    }
                }
            }
        }
        changed
    }
}
