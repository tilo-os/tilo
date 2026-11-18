//! What a mount does about the frames its GPU has not finished: draws, and
//! says when a frame it owes has been out so long the GPU is not answering.

use wasm_bindgen::JsValue;

use crate::frame_pacing::{FramePacing, Pacing, late_note};

/// Ask the GPU's own outstanding frames, and say a frame it has owed longer
/// than [`crate::frame_pacing::LOST_AFTER`] without a word. The frame draws
/// either way: a page that stops for a completion is a page whose completion
/// comes late. However many frames are out, the device is not taken for lost
/// here: only its own `lost` and errors say that.
pub(super) fn check(pacing: &mut FramePacing) {
    if let Pacing::Late {
        frame,
        waiting,
        stalls,
    } = pacing.verdict(ui::time::now())
    {
        // Said each time: a page whose GPU leaves frames out is the one thing
        // a mount that keeps drawing can tell about a freeze.
        web_sys::console::warn_1(&JsValue::from_str(&late_note(frame, waiting, stalls)));
    }
}
