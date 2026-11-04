//! Alt+Tab's panel: what the desk's walk shows, kept in step with the chrome that draws it and
//! the compositor that pictures its windows.
//!
//! The walk itself is the desk's ([`crate::overlays`]): Alt+Tab's presses ask its verbs, and it
//! says which window it reached. This reads that once a pass, as the panel shows it, so the
//! chrome draws what the desk decided and nothing of its own.

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
    ///
    /// A walk held less than [`crate::overlays::REVEAL_DELAY`] shows nothing: a quick Alt+Tab
    /// switches windows without a panel flashing, and the window it reached is still where
    /// Alt's release goes.
    ///
    /// A locked session takes the walk down with everything else it shows over its windows.
    pub(crate) fn follow_switcher(&self, locked: bool) {
        if locked {
            self.desk_overlays
                .close(crate::overlays::Overlay::Switcher.name());
            self.desk_overlays.forget_walk();
        }
        let view = (!locked)
            .then(|| {
                crate::desk_resource::switcher_view(
                    &self.handle,
                    &self.desk_overlays,
                    std::time::Instant::now(),
                )
            })
            .flatten();
        if self.switcher.set(view) {
            // The panel stands over the desk's windows: the chrome lays itself out again for
            // what it now shows, as it does for any surface of the desk's going up or down.
            self.desktop
                .with_widgets(tilo_surface_host::WidgetHost::relayout);
        }
    }
}
