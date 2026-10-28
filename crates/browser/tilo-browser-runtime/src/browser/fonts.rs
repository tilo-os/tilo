//! Optional fonts the page fetches for the mount: what shaping asked for,
//! and each fetched font handed back, or its failure.

use wasm_bindgen::prelude::*;

use super::{error, proxy, with_mount, with_mount_quietly};

/// Optional canonical fonts the last shaping pass needed, as a JSON array.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn font_demands(id: u64) -> Result<String, JsValue> {
    // Takes what the last shaping asked to fetch; the tree is the same.
    with_mount_quietly(id, |m| {
        serde_json::to_string(&m.frame.take_browser_font_demands()).map_err(error)
    })
}

/// Install one fetched optional font. `sha256` is the page's lowercase hex
/// digest of `bytes` from `crypto.subtle.digest`, which runs natively off the
/// main thread; the runtime checks it against the canonical manifest compiled
/// into this build instead of hashing the font again.
#[wasm_bindgen]
pub fn install_font(id: u64, name: &str, bytes: Vec<u8>, sha256: &str) -> Result<bool, JsValue> {
    let (changed, wake) = with_mount(id, |m| {
        let changed = m
            .frame
            .install_browser_font(name, bytes, sha256)
            .map_err(error)?;
        let wake = changed && !m.hidden;
        if wake {
            m.pending = true;
            m.window.request_redraw();
        }
        Ok((changed, wake))
    })?;
    if wake {
        proxy()?.send_event(id).map_err(error)?;
    }
    Ok(changed)
}

/// A font fetch failed; demand is re-reported by a later frame.
///
/// # Errors
///
/// Rejects a closed mount.
#[wasm_bindgen]
pub fn font_failed(id: u64) -> Result<(), JsValue> {
    with_mount(id, |m| {
        m.frame.browser_font_failed();
        Ok(())
    })
}
