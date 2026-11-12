//! Which way the page takes into the workspace a link names, by the rules every device takes
//! ([`tilo_session_client::way_in`]). The page hands over what the link carries and what it knows of
//! the host, as JSON, and takes the way answered (kebab-case, as [`way_in::Way`] names it).

use tilo_session_client::way_in::{self, Failed, Here, Link};
use wasm_bindgen::prelude::*;

use crate::peer::js_error;

fn read<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, JsError> {
    serde_json::from_str(text).map_err(js_error)
}

fn answer(way: way_in::Way) -> Result<String, JsError> {
    serde_json::to_value(way)
        .map_err(js_error)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| JsError::new("a way is named by a string"))
}

/// The first way into the workspace `link` (`{scope, malformed?, invite?, ticket?, pair?}`)
/// names, from what the page knows of its host, `here` (`{door?, entered?, accounts?, tickets?,
/// pairing?}`) ([`way_in::way_in`]).
///
/// # Errors
///
/// Throws for a link or `here` that is none.
#[wasm_bindgen(js_name = wayIn)]
pub fn way_in(link: &str, here: &str) -> Result<String, JsError> {
    answer(way_in::way_in(&read::<Link>(link)?, &read::<Here>(here)?))
}

/// The way in after the way taken `failed` (`member`, `reconnect`) ([`way_in::after`]).
///
/// # Errors
///
/// Throws for a failure or `here` that is none.
#[wasm_bindgen(js_name = wayInAfter)]
pub fn way_in_after(failed: &str, here: &str) -> Result<String, JsError> {
    let failed: Failed =
        serde_json::from_value(serde_json::Value::String(failed.to_owned())).map_err(js_error)?;
    answer(way_in::after(failed, &read::<Here>(here)?))
}

/// Whether a page admitted to the workspace `admitted` got into the one the link `asked` for
/// ([`way_in::as_asked`]).
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen takes an optional string only as an owned one"
)]
#[wasm_bindgen(js_name = admittedAsAsked)]
#[must_use]
pub fn admitted_as_asked(asked: Option<String>, admitted: Option<String>) -> bool {
    way_in::as_asked(asked.as_deref(), admitted.as_deref())
}
