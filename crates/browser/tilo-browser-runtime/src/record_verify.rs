//! A large record verified in the page's record worker before it loads.
//!
//! Loading a record a peer saved rebuilds and hashes every change it holds,
//! seconds for a company's tracker, and the page's one thread would neither
//! draw nor answer input meanwhile. The page's worker (web/record-verify.mjs)
//! runs its own instance of this module and does that work there
//! ([`verified_change_hashes`]); the replica's read of the record waits for it
//! (`tilo_automerge_isolate::away`) and then loads the record with the hashes
//! the worker computed. What is left of that load (building the record's
//! indexes) runs a few milliseconds at a time between the page's frames
//! before the read takes it. Records that are ready show meanwhile.

use std::rc::Rc;

use js_sys::{Function, Promise, Reflect, Uint8Array};
use ui::automerge::ChangeHash;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// The hash of every change of the document `bytes` hold, as loading them
/// verifies it: 32 bytes each, in the order the document lists them. What the
/// record worker runs on its own instance of this module.
///
/// # Errors
/// The bytes are not one document, or do not load.
#[wasm_bindgen(js_name = verifiedChangeHashes)]
pub fn verified_change_hashes(bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    let hashes = tilo_automerge_isolate::verified_hashes(bytes)
        .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(hashes.iter().flat_map(|hash| hash.0).collect())
}

/// Hands records to the page's worker through `__tiloVerifyRecord`, which
/// the page host installs (web/host.mjs).
struct PageWorker;

impl tilo_automerge_isolate::away::Verifier for PageWorker {
    fn verify(&self, bytes: Vec<u8>, done: Box<dyn FnOnce(Result<Vec<ChangeHash>, String>)>) {
        let verify = Reflect::get(&js_sys::global(), &JsValue::from_str("__tiloVerifyRecord"))
            .ok()
            .and_then(|verify| verify.dyn_into::<Function>().ok());
        let Some(verify) = verify else {
            done(Err("this page runs no record worker".into()));
            return;
        };
        let answer = match verify.call1(&JsValue::NULL, &Uint8Array::from(bytes.as_slice())) {
            Ok(answer) => answer,
            Err(error) => {
                done(Err(format!("{error:?}")));
                return;
            }
        };
        wasm_bindgen_futures::spawn_local(async move {
            let answer = JsFuture::from(Promise::resolve(&answer))
                .await
                .map_err(|error| format!("{error:?}"))
                .and_then(|hashes| hashes_of(&Uint8Array::new(&hashes).to_vec()));
            if let Ok(hashes) = &answer {
                load_between_frames(bytes, hashes.clone()).await;
            }
            done(answer);
            crate::replication::verified();
        });
    }
}

/// How long one step of a load may hold the page's thread, in milliseconds:
/// as long as a pass over what landed, well inside a frame.
const STEP_MS: f64 = crate::replication::TURN_MS;

/// Load `bytes` with the `hashes` the worker verified, a step at a time with
/// the page's tasks between, sized to [`STEP_MS`]; the record's read then takes
/// a copy of the document.
async fn load_between_frames(bytes: Vec<u8>, hashes: Vec<ChangeHash>) {
    let Some(mut loading) = tilo_automerge_isolate::away::Loading::new(bytes, hashes) else {
        return;
    };
    let mut ops = 4_096_usize;
    loop {
        let began = js_sys::Date::now();
        if loading.step(ops) {
            break;
        }
        let spent = (js_sys::Date::now() - began).max(1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            clippy::cast_sign_loss,
            reason = "a step's op count, bounded to a few hundred thousand"
        )]
        {
            ops = ((ops as f64 * STEP_MS / spent) as usize).clamp(256, 262_144);
        }
        crate::replication::next_task().await;
    }
    loading.finish();
    // The read the record waited for is a task of its own.
    crate::replication::next_task().await;
}

fn hashes_of(bytes: &[u8]) -> Result<Vec<ChangeHash>, String> {
    if !bytes.len().is_multiple_of(32) {
        return Err("the worker's answer is not a list of hashes".into());
    }
    bytes
        .chunks_exact(32)
        .map(|hash| ChangeHash::try_from(hash).map_err(|error| error.to_string()))
        .collect()
}

/// Verify the large records this page's replicas read in its worker: done as
/// a replica starts, so this lives with the replica's code in the page's peer
/// module and the start module carries none of it.
pub(crate) fn install() {
    tilo_automerge_isolate::away::verify_with(Rc::new(PageWorker));
}
