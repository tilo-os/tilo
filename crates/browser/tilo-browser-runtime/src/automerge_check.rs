//! The check `automerge-check-worker.mjs` runs in a Web Worker on Automerge
//! bytes forwarded by other browsers, before the page admits them.
use tilo_automerge_isolate::{Dependencies, Limits};
use wasm_bindgen::prelude::*;

/// Apply `incoming` to the document saved as `base` (empty for none) in this
/// instance. Called only in the worker's own instance, whose crash, overrun
/// or termination costs nothing but the check.
#[wasm_bindgen]
pub fn check_automerge(base: &[u8], incoming: &[u8]) -> Result<(), JsValue> {
    tilo_automerge_isolate::check_here(
        base,
        incoming,
        Dependencies::RetainOrphans,
        &Limits::default(),
    )
    .map(|_| ())
    .map_err(|rejected| JsValue::from_str(&rejected.to_string()))
}
