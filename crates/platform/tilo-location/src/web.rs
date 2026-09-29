//! The browser backend: see `web/location.mjs`.

use js_sys::{Promise, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::{LocationChannel, LocationOutcome};

/// How long the browser may take to find a fix, as on Linux.
const FIX_TIMEOUT_MS: u32 = 20_000;

#[wasm_bindgen(module = "/web/location.mjs")]
extern "C" {
    fn locate(timeout_ms: u32) -> Promise;
}

fn number(value: &JsValue, field: &str) -> Option<f64> {
    Reflect::get(value, &field.into()).ok()?.as_f64()
}

pub(crate) struct Geolocation;

impl crate::LocationProvider for Geolocation {
    fn locate(&self) -> LocationChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = tilo_rt::wake::WakeHandle::new();
        let asked = locate(FIX_TIMEOUT_MS);
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = match JsFuture::from(asked).await {
                Ok(fix) => match (number(&fix, "lat"), number(&fix, "lon")) {
                    (Some(lat), Some(lon)) => LocationOutcome::Located {
                        lat,
                        lon,
                        accuracy_m: number(&fix, "accuracy").unwrap_or(0.0),
                    },
                    _ => LocationOutcome::Unavailable("no location fix available".into()),
                },
                Err(error) => LocationOutcome::Unavailable(
                    Reflect::get(&error, &"message".into())
                        .ok()
                        .and_then(|m| m.as_string())
                        .unwrap_or_else(|| "no location fix available".into()),
                ),
            };
            if tx.send(outcome).is_ok() {
                wake.wake();
            }
        });
        rx
    }
}
