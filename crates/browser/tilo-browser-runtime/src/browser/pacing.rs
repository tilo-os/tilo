//! What a mount does about the frames its GPU has not finished: draw, hold
//! this frame for the one after the GPU catches up, or give up on a
//! completion the browser never reports and draw without it.

use std::sync::Arc;

use wasm_bindgen::JsValue;
use winit::window::Window;

use crate::frame_pacing::{FramePacing, Pacing, late_note};

/// What the frame loop may do with the frame it has ready.
pub(super) enum Gate {
    /// Draw it.
    Draw,
    /// The GPU is still drawing earlier frames: another frame was asked for,
    /// and this one waits for the one after the GPU catches up.
    Hold,
}

/// Whether the frame a mount has ready may draw now, asked of the GPU's own
/// outstanding frames. A frame the GPU never reported is drawn without, and
/// said; however many there are, the device is not taken for lost here: only
/// its own `lost` and errors say that. `held` is the mount's account of why a
/// frame was not drawn, for a first frame that never comes.
pub(super) fn gate(
    pacing: &mut FramePacing,
    held: &mut &'static str,
    window: &Arc<Window>,
) -> Gate {
    match pacing.verdict(ui::time::now()) {
        Pacing::Draw => Gate::Draw,
        Pacing::Wait => {
            *held = "the GPU drawing earlier frames";
            window.request_redraw();
            Gate::Hold
        }
        Pacing::Late {
            frame,
            waiting,
            stalls,
        } => {
            // Said each time: a page whose GPU leaves frames out is the one
            // thing a mount that keeps drawing can tell about a freeze.
            web_sys::console::warn_1(&JsValue::from_str(&late_note(frame, waiting, stalls)));
            *held = "a frame the GPU never reported drawn";
            Gate::Draw
        }
    }
}
