//! Alt+Tab's panel: what the desk's walk shows, kept in step with the chrome that draws it and the
//! windows it pictures.
//! The walk itself is the desk's ([`crate::overlays`]): Alt+Tab's presses ask its verbs, and it
//! says which window it reached. This reads that once a pass, as the panel shows it, so the chrome
//! draws what the desk decided and nothing of its own.

#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page mounts this with the chrome")
)]

use crate::Shared;

/// A window of the desk: its id, and the logical rectangle `(x, y, w, h)` it stands in.
type PlacedWindow = (u64, (i32, i32, i32, i32));

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
    pub(crate) fn follow_switcher(
        &self,
        desk: &dyn crate::platform::surfaces::Desk,
        locked: bool,
        windows: &[PlacedWindow],
    ) {
        if locked {
            self.desk_overlays
                .close(crate::overlays::Overlay::Switcher.name());
            self.desk_overlays.forget_walk();
        }
        let now = tilo_clock::Instant::now();
        // A flick shorter than the panel's reveal delay shows nothing, and costs no reading of the
        // desk's windows.
        let view = (!locked && self.desk_overlays.revealed(now))
            .then(|| {
                crate::desk_windows::switcher_view(
                    &self.handle,
                    &self.desk_overlays,
                    &window_sizes(windows),
                )
            })
            .flatten();
        if self.switcher.set(view) {
            // The panel stands over the desk's windows: the chrome lays itself out again for what
            // it now shows, as it does for any surface of the desk's going up or down.
            desk.relayout();
        }
    }
}

/// Each window's size in logical pixels, as the desk lays it out: the shape a card of the panel
/// takes, so the window's picture fills it.
fn window_sizes(windows: &[PlacedWindow]) -> std::collections::HashMap<u64, (u32, u32)> {
    windows
        .iter()
        .filter_map(|(id, (_, _, width, height))| {
            Some((
                *id,
                (u32::try_from(*width).ok()?, u32::try_from(*height).ok()?),
            ))
        })
        .filter(|(_, (width, height))| *width > 0 && *height > 0)
        .collect()
}
