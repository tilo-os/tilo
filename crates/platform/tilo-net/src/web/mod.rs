//! Browser backends and the page glue they share.
//!
//! A page has one thread. Browser objects are wrapped in [`SendWrapper`] so
//! the futures holding them are `Send` like their desktop counterparts; the
//! wrapper would panic if one were touched from another thread, which a
//! page without threads cannot do.

#![allow(
    clippy::future_not_send,
    reason = "page futures are made `Send` by `SendWrapper` where they leave this module"
)]

#[cfg(feature = "http")]
pub(crate) mod fetch;
#[cfg(feature = "webtransport")]
pub(crate) mod webtransport;
#[cfg(feature = "ws")]
pub(crate) mod ws;

#[cfg(feature = "http")]
use std::time::Duration;

#[cfg(any(feature = "http", feature = "webtransport"))]
use js_sys::Promise;
use js_sys::{Function, Reflect};
use send_wrapper::SendWrapper;
use tokio::sync::mpsc;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
#[cfg(any(feature = "http", feature = "webtransport"))]
use wasm_bindgen_futures::JsFuture;

#[cfg(any(feature = "http", feature = "webtransport"))]
use crate::NetError;

#[cfg(feature = "http")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &Function, milliseconds: i32) -> JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(handle: &JsValue);
}

/// The page's global object (`window` or a worker's `self`).
pub(crate) fn global() -> Option<JsValue> {
    let global: JsValue = js_sys::global().into();
    (!global.is_undefined()).then_some(global)
}

/// `object[key]`, unless it is `undefined` or `null`.
pub(crate) fn get(object: &JsValue, key: &str) -> Option<JsValue> {
    Reflect::get(object, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_undefined() && !value.is_null())
}

/// `globalThis[name]`, unless it is `undefined` or `null`.
pub(crate) fn global_get(name: &str) -> Option<JsValue> {
    get(&global()?, name)
}

pub(crate) fn has_global(name: &str) -> bool {
    global_get(name).is_some()
}

/// The page's URL, from `location.href`.
pub(crate) fn page_url() -> Option<url::Url> {
    let href = get(&global_get("location")?, "href")?.as_string()?;
    url::Url::parse(&href).ok()
}

#[cfg(feature = "http")]
pub(crate) fn page_origin() -> Option<url::Origin> {
    Some(page_url()?.origin())
}

/// A browser error's message.
#[cfg(any(feature = "http", feature = "ws", feature = "webtransport"))]
pub(crate) fn message(error: &JsValue) -> String {
    if let Some(text) = error.as_string() {
        return text;
    }
    if let Some(text) = get(error, "message").and_then(|message| message.as_string()) {
        let name = get(error, "name")
            .and_then(|name| name.as_string())
            .unwrap_or_default();
        return if name.is_empty() {
            text
        } else {
            format!("{name}: {text}")
        };
    }
    "the browser gave no reason".to_owned()
}

/// Whether a browser error is a `DOMException` or `Error` named `name`.
#[cfg(feature = "http")]
pub(crate) fn is_named(error: &JsValue, name: &str) -> bool {
    get(error, "name")
        .and_then(|value| value.as_string())
        .is_some_and(|value| value == name)
}

#[cfg(any(feature = "http", feature = "webtransport"))]
pub(crate) fn promise(
    promise: Promise,
) -> impl Future<Output = Result<JsValue, JsValue>> + Send + use<> {
    SendWrapper::new(JsFuture::from(promise))
}

/// Call `object.method(...args)` and await the promise it returns.
#[cfg(feature = "webtransport")]
pub(crate) fn call_promise(
    object: &JsValue,
    method: &str,
    args: &[JsValue],
) -> impl Future<Output = Result<JsValue, NetError>> + Send + use<> {
    let called = (|| {
        let function: Function = get(object, method)
            .ok_or_else(|| JsValue::from_str(&format!("{method} is not available")))?
            .dyn_into()?;
        let args: js_sys::Array = args.iter().collect();
        let result = function.apply(object, &args)?;
        Ok::<Promise, JsValue>(Promise::resolve(&result))
    })();
    let pending = called
        .map(promise)
        .map_err(|error| NetError::Protocol(message(&error)));
    async move {
        pending?
            .await
            .map_err(|error| NetError::Protocol(message(&error)))
    }
}

/// Resolve after `duration` with a `setTimeout`, cleared if dropped first.
#[cfg(feature = "http")]
pub(crate) fn sleep(duration: Duration) -> impl Future<Output = ()> + Send + use<> {
    let milliseconds = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
    let handle = std::rc::Rc::new(std::cell::RefCell::new(JsValue::UNDEFINED));
    let registered = std::rc::Rc::clone(&handle);
    let promise_ = Promise::new(&mut |resolve, _reject| {
        *registered.borrow_mut() = set_timeout(&resolve, milliseconds);
    });
    let timer = Timer {
        handle: SendWrapper::new(handle),
    };
    let wait = promise(promise_);
    async move {
        let _ = wait.await;
        timer.disarm();
    }
}

#[cfg(feature = "http")]
struct Timer {
    handle: SendWrapper<std::rc::Rc<std::cell::RefCell<JsValue>>>,
}

#[cfg(feature = "http")]
impl Timer {
    fn disarm(&self) {
        *self.handle.borrow_mut() = JsValue::UNDEFINED;
    }
}

#[cfg(feature = "http")]
impl Drop for Timer {
    fn drop(&mut self) {
        let handle = self.handle.borrow();
        if !handle.is_undefined() {
            clear_timeout(&handle);
        }
    }
}

/// Race `future` against an optional deadline.
#[cfg(feature = "http")]
pub(crate) async fn deadline<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = Result<T, NetError>>,
) -> Result<T, NetError> {
    let Some(timeout) = timeout else {
        return future.await;
    };
    tokio::select! {
        result = future => result,
        () = sleep(timeout) => Err(NetError::Timeout),
    }
}

/// A target, an event name and the closure listening for it.
type Listener = (JsValue, String, Closure<dyn FnMut(JsValue)>);

/// Browser events delivered to a task: each listened event queues a wake.
pub(crate) struct Events {
    receiver: mpsc::UnboundedReceiver<()>,
    sender: mpsc::UnboundedSender<()>,
    listeners: SendWrapper<Vec<Listener>>,
}

impl Events {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            receiver,
            sender,
            listeners: SendWrapper::new(Vec::new()),
        }
    }

    pub(crate) fn listen(&mut self, target: &JsValue, names: &[&str]) {
        for name in names {
            let sender = self.sender.clone();
            let closure = Closure::<dyn FnMut(JsValue)>::new(move |_event: JsValue| {
                let _ = sender.send(());
            });
            let added = get(target, "addEventListener")
                .and_then(|add| add.dyn_into::<Function>().ok())
                .map(|add| {
                    add.call2(
                        target,
                        &JsValue::from_str(name),
                        closure.as_ref().unchecked_ref(),
                    )
                });
            if matches!(added, Some(Ok(_))) {
                self.listeners
                    .push((target.clone(), (*name).to_owned(), closure));
            }
        }
    }

    /// Wait for the next event; `None` when nothing is listened to.
    pub(crate) async fn next(&mut self) -> Option<()> {
        if self.listeners.is_empty() {
            return None;
        }
        self.receiver.recv().await
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        for (target, name, closure) in self.listeners.drain(..) {
            if let Some(remove) =
                get(&target, "removeEventListener").and_then(|f| f.dyn_into::<Function>().ok())
            {
                let _ = remove.call2(
                    &target,
                    &JsValue::from_str(&name),
                    closure.as_ref().unchecked_ref(),
                );
            }
        }
    }
}
