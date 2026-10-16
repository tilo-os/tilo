//! The browser backend: `<input type=file>` choosers that deliver contents,
//! downloads, and `window.open` for web and composing links.
//!
//! Browsers grant pickers and popups only inside a user gesture. Every call
//! here comes from the app's handling of an input event, which the runner
//! dispatches from the DOM listener, so it runs within that gesture's
//! activation. A page has no local files: path choosers refuse.

use std::sync::mpsc;

use js_sys::{Array, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::portal::{
    ContentsChannel, ContentsOutcome, LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel,
    MultiOutcome, OpenFileChannel, OpenFileOutcome, PickRequest, PickedContents, PortalChannel,
    PortalOutcome, SaveChannel, SaveOutcome,
};
use crate::web_policy::{accept_attribute, openable_uri};

#[wasm_bindgen(module = "/web/dialogs.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = pickFiles, catch)]
    fn pick_files(
        accept: &str,
        multiple: bool,
        max_contents_bytes: Option<f64>,
    ) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = saveFile, catch)]
    fn save_file(name: &str, bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(js_name = openUri, catch)]
    fn open_uri_js(uri: &str) -> Result<bool, JsValue>;
}

fn text(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            Reflect::get(error, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{error:?}"))
}

fn answered<T>(outcome: T) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

/// `[{name, type, bytes}]` from `pickFiles`; `null` when cancelled.
fn contents(value: &JsValue) -> Result<Option<Vec<PickedContents>>, String> {
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    Array::from(value)
        .iter()
        .map(|file| {
            let field = |name: &str| Reflect::get(&file, &name.into()).map_err(|e| text(&e));
            Ok(PickedContents {
                name: field("name")?.as_string().unwrap_or_default(),
                media_type: field("type")?.as_string().filter(|t| !t.is_empty()),
                bytes: Uint8Array::new(&field("bytes")?).to_vec(),
                location: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|files| (!files.is_empty()).then_some(files))
}

pub(crate) struct PageFiles;

impl crate::FilePicker for PageFiles {
    fn pick(&self, _request: PickRequest) -> PortalChannel {
        answered(PortalOutcome::Failed(crate::caps::WEB_PATHS.to_owned()))
    }

    fn pick_multiple(&self, _request: PickRequest) -> MultiChannel {
        answered(MultiOutcome::Failed(crate::caps::WEB_PATHS.to_owned()))
    }

    fn open_file(&self, _request: PickRequest) -> OpenFileChannel {
        answered(OpenFileOutcome::Failed(crate::caps::WEB_PATHS.to_owned()))
    }

    fn launch(&self, _request: LaunchRequest) -> LaunchChannel {
        answered(LaunchOutcome::Failed(crate::caps::WEB_LAUNCH.to_owned()))
    }

    fn pick_contents(&self, request: PickRequest) -> ContentsChannel {
        if !request.is_open() {
            return answered(ContentsOutcome::Failed(
                "only an Open chooser delivers contents".to_owned(),
            ));
        }
        let filters = request.filters();
        let accept = accept_attribute(
            filters
                .iter()
                .flat_map(|f| f.globs().iter().map(String::as_str)),
            filters
                .iter()
                .flat_map(|f| f.mimetypes().iter().map(String::as_str)),
        );
        #[expect(
            clippy::cast_precision_loss,
            reason = "JavaScript numbers carry byte limits exactly up to 2^53"
        )]
        let limit = request.contents_byte_limit().map(|limit| limit as f64);
        let promise = match pick_files(&accept, request.is_multiple(), limit) {
            Ok(promise) => promise,
            Err(error) => return answered(ContentsOutcome::Failed(text(&error))),
        };
        let (tx, rx) = mpsc::channel();
        let wake = tilo_rt::wake::WakeHandle::new();
        wasm_bindgen_futures::spawn_local(async move {
            let outcome = match JsFuture::from(promise).await {
                Ok(value) => match contents(&value) {
                    Ok(Some(files)) => ContentsOutcome::Picked(files),
                    Ok(None) => ContentsOutcome::Cancelled,
                    Err(reason) => ContentsOutcome::Failed(reason),
                },
                Err(error) => ContentsOutcome::Failed(text(&error)),
            };
            let _ = tx.send(outcome);
            wake.wake();
        });
        rx
    }

    fn save_contents(&self, request: PickRequest, bytes: Vec<u8>) -> SaveChannel {
        let Some(name) = request.suggested_name() else {
            return answered(SaveOutcome::Failed(
                "only a Save chooser saves contents".to_owned(),
            ));
        };
        answered(match save_file(name, &bytes) {
            Ok(()) => SaveOutcome::Saved,
            Err(error) => SaveOutcome::Failed(text(&error)),
        })
    }
}

/// Open `uri` in a new browsing context: web pages and composing links only.
pub(crate) fn open_uri(uri: &str) -> std::io::Result<()> {
    openable_uri(uri)
        .map_err(|reason| std::io::Error::new(std::io::ErrorKind::PermissionDenied, reason))?;
    match open_uri_js(uri) {
        Ok(true) => Ok(()),
        Ok(false) => Err(std::io::Error::other(
            "the browser blocked the new window; allow pop-ups for this site",
        )),
        Err(error) => Err(std::io::Error::other(text(&error))),
    }
}
