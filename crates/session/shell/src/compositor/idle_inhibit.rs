//! `zwp_idle_inhibit_manager_v1`: a client asking the screen to stay on.
//!
//! A video player or a slideshow creates an inhibitor on the surface showing
//! it. The request counts only while that surface is on screen — a video in a
//! tab nobody can see is no reason to keep the display lit or the session
//! unlocked — and while it counts it holds an idle hold in the session's
//! [`InhibitRegistry`], the same registry portal and D-Bus inhibitors use, so
//! the idle state machine and quick settings see one answer.

use std::collections::{HashMap, HashSet};

use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use tilo_power_mgmt::{InhibitRegistry, InhibitToken};

use crate::compositor::frame::FrameBuilder;
use crate::compositor::state::State;

/// Live inhibitor requests and the registry holds they currently earn.
#[derive(Default)]
pub struct IdleInhibitors {
    /// One entry per live `zwp_idle_inhibitor_v1`; a surface may carry several.
    requests: Vec<WlSurface>,
    /// Registry holds by the toplevel (root surface id) they are for.
    holds: HashMap<u64, InhibitToken>,
}

impl IdleInhibitors {
    pub fn inhibit(&mut self, surface: WlSurface) {
        self.requests.push(surface);
    }

    pub fn uninhibit(&mut self, surface: &WlSurface) {
        if let Some(index) = self.requests.iter().position(|held| held == surface) {
            self.requests.swap_remove(index);
        }
    }

    /// The window (root surface) of every live request. A client that
    /// disconnects without destroying its inhibitor never says `uninhibit`,
    /// so dead surfaces are dropped here.
    fn requesting_windows(&mut self) -> HashSet<u64> {
        self.requests.retain(Resource::is_alive);
        self.requests
            .iter()
            .map(|surface| {
                let root = crate::compositor::state::foreign_root(surface)
                    .unwrap_or_else(|| surface.clone());
                crate::compositor::state::surface_id(&root)
            })
            .collect()
    }

    /// Hold exactly the windows in `wanted` (id → the name shown to the user).
    fn reconcile(&mut self, wanted: HashMap<u64, String>, registry: &InhibitRegistry) {
        self.holds.retain(|window, _| wanted.contains_key(window));
        for (window, name) in wanted {
            self.holds.entry(window).or_insert_with(|| {
                registry.acquire_for(name, tilo_power_mgmt::inhibit::INHIBIT_IDLE)
            });
        }
    }

    /// Windows whose inhibitor currently holds the screen on.
    pub fn holding(&self) -> impl Iterator<Item = u64> + '_ {
        self.holds.keys().copied()
    }
}

/// Grant a registry hold to every window with a live inhibitor that is on
/// screen, and release the rest. Called every tick, since visibility changes
/// with every workspace switch, tab change and fullscreen toggle.
pub fn sync_idle_inhibitors(state: &mut State, fb: &FrameBuilder, registry: &InhibitRegistry) {
    let requesting = state.idle_inhibitors.requesting_windows();
    let wanted: HashMap<u64, String> = {
        let chrome = fb.chrome();
        let tree = chrome.tree();
        requesting
            .into_iter()
            .filter(|&window| {
                crate::shell::foreign::is_visible_tab(&tree, window)
                    || state.is_floating_toplevel(window)
                    || state.is_output_presentation_surface(window)
                    || state.is_transient_surface(window)
                    || fb
                        .fullscreen_surface_rect()
                        .is_some_and(|(fullscreen, _)| fullscreen == window)
            })
            .map(|window| {
                let name = crate::shell::tab_binding::app_id_for_surface(&tree, window)
                    .map_or_else(
                        || crate::shell::i18n::t("idle-inhibit-unnamed-app"),
                        crate::shell::tab_binding::tab_label_for_foreign,
                    );
                (window, name)
            })
            .collect()
    };
    state.idle_inhibitors.reconcile(wanted, registry);
}
