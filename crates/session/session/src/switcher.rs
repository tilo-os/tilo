//! Alt+Tab's panel: what the desk's walk shows, kept in step with the chrome that draws it and the
//! compositor that pictures its windows.
//! The walk itself is the desk's ([`crate::overlays`]): Alt+Tab's presses ask its verbs, and it
//! says which window it reached. This reads that once a pass, as the panel shows it, so the chrome
//! draws what the desk decided and nothing of its own.

use crate::Shared;

impl crate::Session {
    /// Alt+Tab's panel: what it shows and where its cards stand, as the chrome last drew it.
    #[must_use]
    pub fn window_switcher(&self) -> tilo_desktop::window_switcher::Switcher {
        self.shared.switcher.clone()
    }
}

impl Shared {
    /// Show what the desk's walk reached this pass, or take the panel down.
    /// A walk held less than [`crate::overlays::REVEAL_DELAY`] shows nothing: a quick Alt+Tab
    /// switches windows without a panel flashing, and the window it reached is still where Alt's
    /// release goes.
    /// A locked session takes the walk down with everything else it shows over its windows.
    pub(crate) fn follow_switcher(&self, compositor: &tilo_compositor::Compositor) {
        let locked = compositor.locked();
        if locked {
            self.desk_overlays
                .close(crate::overlays::Overlay::Switcher.name());
            self.desk_overlays.forget_walk();
        }
        let now = std::time::Instant::now();
        // A flick shorter than the panel's reveal delay shows nothing, and costs no reading of the
        // desk's windows.
        let view = (!locked && self.desk_overlays.revealed(now))
            .then(|| {
                crate::desk_resource::switcher_view(
                    &self.handle,
                    &self.desk_overlays,
                    &window_sizes(compositor),
                )
            })
            .flatten();
        if self.switcher.set(view) {
            // The panel stands over the desk's windows: the chrome lays itself out again for what
            // it now shows, as it does for any surface of the desk's going up or down.
            self.desktop
                .with_widgets(crate::platform::surfaces::WidgetHost::relayout);
        }
    }
}

/// Each window's size in logical pixels, as the compositor lays it out: the shape a card of the
/// panel takes, so the window's picture fills it.
fn window_sizes(
    compositor: &tilo_compositor::Compositor,
) -> std::collections::HashMap<u64, (u32, u32)> {
    compositor
        .windows()
        .into_iter()
        .filter_map(|(window, (_, _, width, height))| {
            Some((
                window.id,
                (u32::try_from(width).ok()?, u32::try_from(height).ok()?),
            ))
        })
        .filter(|(_, (width, height))| *width > 0 && *height > 0)
        .collect()
}
