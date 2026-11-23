//! The artwork each window brought of its own: the icon a program set for its
//! window, which that window's tab paints in place of its app kind's glyph,
//! and which the switcher shows
//! ([`DesktopHandle::set_window_artwork`]). Never part of the tree: a shell does
//! not keep a window's icon after the window it belonged to is gone.

use super::DesktopHandle;

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

    /// The artwork `window` brought of its own, as the compositor's id for it last said.
    #[must_use]
    pub fn window_artwork(&self, window: u64) -> Option<ui::icons::AppImage> {
        self.shared.borrow().artwork.get(&window).cloned()
    }
}
