//! `registry-web` for JavaScript. Structured results cross the boundary as
//! JSON strings; `web-registry-client/registry-verifier.mjs` parses them
//! into the verifier interface `registry-client.mjs` expects.

use registry_web::{Checkpoint, Host, Selection};
use wasm_bindgen::prelude::*;

fn js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

/// A verified snapshot.
#[wasm_bindgen]
pub struct Snapshot(registry_web::VerifiedSnapshot);

#[wasm_bindgen]
impl Snapshot {
    /// The checkpoint JSON to store in place of the old one.
    pub fn checkpoint(&self) -> Vec<u8> {
        self.0.checkpoint().to_json()
    }

    pub fn sequence(&self) -> u64 {
        self.0.sequence()
    }

    /// The newest web version of `package_id` for these features and, when
    /// given, exactly this runtime ABI id; a `Selection` as JSON.
    ///
    /// # Errors
    ///
    /// Throws when the package is not listed or no version fits.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes JavaScript arrays and optional strings only as owned values"
    )]
    pub fn select(
        &self,
        package_id: &str,
        features: Vec<String>,
        runtime: Option<String>,
    ) -> Result<String, JsError> {
        let features: Vec<&str> = features.iter().map(String::as_str).collect();
        let selection = self
            .0
            .select(
                package_id,
                &Host {
                    features: &features,
                    runtime: runtime.as_deref(),
                },
            )
            .map_err(js_error)?;
        serde_json::to_string(&selection).map_err(js_error)
    }
}

/// Verify `/entries` bytes against the pinned owner key and the stored
/// checkpoint JSON (if any).
///
/// # Errors
///
/// Throws for every failed proof, including rollback.
#[wasm_bindgen(js_name = verifySnapshot)]
pub fn verify_snapshot(
    bytes: &[u8],
    owner_pubkey_hex: &str,
    checkpoint: Option<Vec<u8>>,
    now_unix: u64,
) -> Result<Snapshot, JsError> {
    let checkpoint = checkpoint
        .map(|bytes| Checkpoint::from_json(&bytes))
        .transpose()
        .map_err(js_error)?;
    registry_web::verify_snapshot(bytes, owner_pubkey_hex, checkpoint.as_ref(), now_unix)
        .map(Snapshot)
        .map_err(js_error)
}

/// Verify a tapp against a `Selection` JSON from [`Snapshot::select`]; a
/// `VerifiedPackage` as JSON.
///
/// # Errors
///
/// Throws when any signature or hash link fails.
#[wasm_bindgen(js_name = verifyPackage)]
pub fn verify_package(bytes: &[u8], selection_json: &str) -> Result<String, JsError> {
    let selection: Selection = serde_json::from_str(selection_json).map_err(js_error)?;
    let package = registry_web::verify_package(bytes, &selection).map_err(js_error)?;
    serde_json::to_string(&package).map_err(js_error)
}

/// Lower-case hex BLAKE3 of `bytes`.
#[wasm_bindgen(js_name = blake3Hex)]
pub fn blake3_hex(bytes: &[u8]) -> String {
    registry_web::blake3_hex(bytes)
}
