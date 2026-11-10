//! The workspaces this browser entered, changed by the rules every device keeps that list by
//! ([`tilo_session_host::entered`]). The page keeps the list in its browser's storage and hands it
//! over as the JSON it stores; each call answers the list as it is now and the workspaces that
//! stop being the one a bare entry reopens (`{entered, unchoose}`).

use tilo_session_host::entered::{self, Entered, Place};
use wasm_bindgen::prelude::*;

use crate::peer::js_error;

/// The stored list, each entry that is one: what storage holds may be damaged.
fn list(stored: &str) -> Vec<Entered> {
    serde_json::from_str::<Vec<serde_json::Value>>(stored)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect()
}

fn place(text: &str) -> Result<Place, JsError> {
    serde_json::from_str(text).map_err(js_error)
}

fn answer(value: &impl serde::Serialize) -> Result<String, JsError> {
    serde_json::to_string(value).map_err(js_error)
}

/// Remember that this browser opened `entry` (`{origin, room, hostDevice, name?}`) as a member
/// ([`entered::remember`]).
///
/// # Errors
///
/// Throws for an entry that is none.
#[wasm_bindgen(js_name = rememberEntered)]
pub fn remember_entered(stored: &str, entry: &str) -> Result<String, JsError> {
    let entry: Entered = serde_json::from_str(entry).map_err(js_error)?;
    answer(&entered::remember(list(stored), entry))
}

/// Name the remembered `place` (`{origin, room}`) `name`, if it has none yet. Answers the list.
///
/// # Errors
///
/// Throws for a place that is none.
#[wasm_bindgen(js_name = nameEntered)]
pub fn name_entered(stored: &str, place_json: &str, name: &str) -> Result<String, JsError> {
    answer(&entered::name(list(stored), &place(place_json)?, name))
}

/// Opening the remembered `place` failed ([`entered::mark_unavailable`]).
///
/// # Errors
///
/// Throws for a place that is none.
#[wasm_bindgen(js_name = markEnteredUnavailable)]
pub fn mark_entered_unavailable(stored: &str, place_json: &str) -> Result<String, JsError> {
    answer(&entered::mark_unavailable(
        list(stored),
        &place(place_json)?,
    ))
}

/// The host at `origin` answered as `hostDevice` ([`entered::forget_gone`]).
///
/// # Errors
///
/// Throws only when the answer cannot be written.
#[wasm_bindgen(js_name = forgetGoneWorkspaces)]
pub fn forget_gone_workspaces(
    stored: &str,
    origin: &str,
    host_device: &str,
) -> Result<String, JsError> {
    answer(&entered::forget_gone(list(stored), origin, host_device))
}

/// Opening the remembered `place` failed and its origin answers as `answering` (a host's device
/// key, hex), or did not answer ([`entered::settle_failed`]). Answers `{entered, unchoose,
/// settled}`, `settled` being `gone`, `unavailable` or `unknown`.
///
/// # Errors
///
/// Throws for a place that is none.
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen takes an optional string only as an owned one"
)]
#[wasm_bindgen(js_name = settleFailedWorkspace)]
pub fn settle_failed_workspace(
    stored: &str,
    place_json: &str,
    answering: Option<String>,
) -> Result<String, JsError> {
    let (changed, settled) =
        entered::settle_failed(list(stored), &place(place_json)?, answering.as_deref());
    answer(&serde_json::json!({
        "entered": changed.entered,
        "unchoose": changed.unchoose,
        "settled": settled,
    }))
}
