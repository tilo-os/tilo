//! Where the session's input path consults its overlay stack: the press that
//! falls outside a surface standing where the session placed it closes that
//! surface, and nothing else.
//!
//! The stack itself is the desktop's ([`tilo_desktop::overlays::Overlays`]),
//! published as `desktop:overlays` and drawn by the chrome and by the views
//! that hang over it. What the session adds here is the one decision that
//! needs the input path's own knowledge — where the press landed — because a
//! popover under the bar closes on a press anywhere else, and that press is
//! the compositor's to deliver.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_session_core::surface::{CursorIcon, KeyClaim};
use tilo_session_core::surface::{HostLayer, SurfaceHost};

use tilo_desktop::overlays::Overlays;

/// The session's reader of presses outside the overlays.
/// It shows nothing and takes nothing: it exists so the compositor's press reaches the stack with
/// the point it landed on. A surface the session cannot judge — one its own view places, like a
/// window's menu — is left to that view, which closes it as it always has.
pub(crate) struct SlotsHost {
    overlays: Overlays,
}

impl SlotsHost {
    /// The host feeding `overlays` the session's presses.
    pub(crate) fn new(overlays: Overlays) -> Self {
        Self { overlays }
    }
}

impl SurfaceHost for SlotsHost {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    /// A press outside the surface it landed in: the topmost popup the session placed, standing
    /// where the press is not, goes down — and stays down for that press, so it does not reopen
    /// under it.
    fn pressed_elsewhere(&mut self, x: f64, y: f64) {
        if let Some(slot) = self.overlays.press_outside(x, y) {
            self.overlays.dismiss(slot);
        }
    }

    /// The button came up: the press is over. What it closed stays down until what the press asked
    /// is carried out, which a quick click delivers after its release.
    fn press_ended(&mut self, _x: f64, _y: f64) {
        self.overlays.press_ended();
    }

    fn claim_key(
        &mut self,
        _key: &tilo_session_core::surface::HostKey,
        _locked: bool,
        _inhibited: bool,
    ) -> KeyClaim {
        KeyClaim::Pass
    }

    fn prepare_frame(&mut self) -> bool {
        false
    }

    fn cursor(&self) -> CursorIcon {
        CursorIcon::Default
    }
}

/// Mount the session's reader of presses outside the overlays over `compositor` .
pub(crate) fn mount(compositor: &mut tilo_compositor::Compositor, overlays: &Overlays) {
    compositor.add_host(Rc::new(RefCell::new(SlotsHost::new(overlays.clone()))));
}
