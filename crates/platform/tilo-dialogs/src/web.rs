//! The browser backend: `<input type=file>` choosers that deliver contents,
//! the browser's Save dialog or downloads, and `window.open` for web and
//! composing links.
//!
//! Browsers grant pickers and popups only inside a user gesture. The app asks
//! while handling an input event, usually within that gesture's activation;
//! when an effect reaches the chooser after the activation expired,
//! `dialogs.mjs` asks for one more click on the page rather than letting the
//! browser drop the chooser silently, and a save downloads. A page has no
//! local files: path choosers refuse, and apps choose by contents or save
//! through a [`SaveDestination`].

use std::sync::mpsc;

use js_sys::{Array, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::portal::{
    ContentsChannel, ContentsOutcome, DestinationChannel, DestinationOutcome, LaunchChannel,
    LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel, OpenFileOutcome,
    PickRequest, PickedContents, PortalChannel, PortalOutcome, SaveChannel, SaveDestination,
    SaveOutcome,
};
use crate::web_policy::{accept_attribute, openable_uri};

#[wasm_bindgen(module = "/web/dialogs.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = pickFiles, catch)]
    fn pick_files(
        accept: &str,
        multiple: bool,
        max_contents_bytes: Option<f64>,
        title: &str,
        accept_label: &str,
    ) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = saveFile, catch)]
    fn save_file(name: &str, bytes: &[u8]) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = chooseSaveTarget, catch)]
    fn choose_save_target(name: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = writeSaveTarget, catch)]
    fn write_save_target(target: &JsValue, bytes: &[u8]) -> Result<Promise, JsValue>;
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

/// Settle `promise` into the channel it answers, waking the app's loop.
fn settled<T: 'static>(
    promise: Promise,
    answer: impl FnOnce(Result<JsValue, String>) -> T + 'static,
) -> mpsc::Receiver<T> {
    let (tx, rx) = mpsc::channel();
    let wake = tilo_rt::wake::WakeHandle::new();
    wasm_bindgen_futures::spawn_local(async move {
        let outcome = answer(JsFuture::from(promise).await.map_err(|error| text(&error)));
        let _ = tx.send(outcome);
        wake.wake();
    });
    rx
}

thread_local! {
    /// Save targets chosen and still held by a destination, by key: the
    /// browser's file handles are page objects a destination cannot carry
    /// across threads. A held target takes every later save of its document.
    static TARGETS: RefCell<HashMap<u64, JsValue>> = RefCell::new(HashMap::new());
    static NEXT_TARGET: Cell<u64> = const { Cell::new(0) };
}

fn write_target(key: u64, bytes: &[u8]) -> SaveChannel {
    let Some(target) = TARGETS.with(|targets| targets.borrow().get(&key).cloned()) else {
        return answered(SaveOutcome::Failed(
            "the chosen file is no longer held".to_owned(),
        ));
    };
    match write_save_target(&target, bytes) {
        Ok(promise) => settled(promise, |written| match written {
            Ok(_) => SaveOutcome::Saved,
            Err(reason) => SaveOutcome::Failed(reason),
        }),
        Err(error) => answered(SaveOutcome::Failed(text(&error))),
    }
}

/// A target's key, let go when its destination is dropped.
struct HeldTarget(u64);

impl Drop for HeldTarget {
    fn drop(&mut self) {
        TARGETS.with(|targets| targets.borrow_mut().remove(&self.0));
    }
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
        let promise = match pick_files(
            &accept,
            request.is_multiple(),
            limit,
            request.title(),
            request.accept_label(),
        ) {
            Ok(promise) => promise,
            Err(error) => return answered(ContentsOutcome::Failed(text(&error))),
        };
        settled(promise, |picked| match picked {
            Ok(value) => match contents(&value) {
                Ok(Some(files)) => ContentsOutcome::Picked(files),
                Ok(None) => ContentsOutcome::Cancelled,
                Err(reason) => ContentsOutcome::Failed(reason),
            },
            Err(reason) => ContentsOutcome::Failed(reason),
        })
    }

    fn save_contents(&self, request: PickRequest, bytes: Vec<u8>) -> SaveChannel {
        let Some(name) = request.suggested_name() else {
            return answered(SaveOutcome::Failed(
                "only a Save chooser saves contents".to_owned(),
            ));
        };
        match save_file(name, &bytes) {
            Ok(promise) => settled(promise, |saved| match saved {
                Ok(value) if value.as_bool() == Some(false) => SaveOutcome::Cancelled,
                Ok(_) => SaveOutcome::Saved,
                Err(reason) => SaveOutcome::Failed(reason),
            }),
            Err(error) => answered(SaveOutcome::Failed(text(&error))),
        }
    }

    fn save_destination(&self, request: PickRequest) -> DestinationChannel {
        let Some(name) = request.suggested_name().map(str::to_owned) else {
            return DestinationChannel::answered(DestinationOutcome::Failed(
                "only a Save chooser chooses where to save".to_owned(),
            ));
        };
        let promise = match choose_save_target(&name) {
            Ok(promise) => promise,
            Err(error) => {
                return DestinationChannel::answered(DestinationOutcome::Failed(text(&error)));
            }
        };
        DestinationChannel::from_outcomes(settled(promise, move |chosen| match chosen {
            Ok(target) if target.is_null() || target.is_undefined() => {
                DestinationOutcome::Cancelled
            }
            Ok(target) => {
                let shown = Reflect::get(&target, &"name".into())
                    .ok()
                    .and_then(|name| name.as_string())
                    .filter(|shown| !shown.is_empty())
                    .unwrap_or(name);
                let key = NEXT_TARGET.with(|next| {
                    let key = next.get();
                    next.set(key.wrapping_add(1));
                    key
                });
                TARGETS.with(|targets| targets.borrow_mut().insert(key, target));
                let held = HeldTarget(key);
                DestinationOutcome::Chosen(SaveDestination::kept(shown, move |bytes| {
                    write_target(held.0, &bytes)
                }))
            }
            Err(reason) => DestinationOutcome::Failed(reason),
        }))
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
