//! The page's device pixel ratio, as the browser changes it.
//!
//! The canvas's box is in CSS pixels, and every mount lays the desk out in
//! them: a phone's 390 CSS pixels are a phone's desk at any density. What a
//! change of density moves is the extent the page draws into, and the extent
//! is not the page's own to see: winit reports the canvas in device pixels
//! when its box changes, and a change of the ratio leaves the box alone, so
//! nothing arrives. Pairing the ratio of the moment with the extent of
//! before laid a phone's desk out at 780 CSS pixels — a laptop's.
//!
//! A ratio is the one thing `(resolution: Xdppx)` matches, so a page asking
//! for the ratio it draws at is told the moment it is not that ratio any
//! more: browser zoom, a window moved to a display of another density, or an
//! emulated scale. The query is made again for the ratio now, and the mounts
//! draw again, laying out at that ratio ([`crate::viewport::ViewportState::observe`]).

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;

/// The browser's media query list for the ratio the page drew at, and the
/// listener that armed it: together they watch that ratio leave.
struct Armed {
    list: JsValue,
    listener: Closure<dyn FnMut()>,
}

impl Drop for Armed {
    fn drop(&mut self) {
        if let Ok(remove) = js_sys::Reflect::get(&self.list, &"removeEventListener".into())
            && let Some(remove) = remove.dyn_ref::<js_sys::Function>()
        {
            let _ = remove.call2(&self.list, &"change".into(), self.listener.as_ref());
        }
    }
}

thread_local! {
    static ARMED: RefCell<Option<Armed>> = const { RefCell::new(None) };
}

/// Draw every mount again whenever the page's device pixel ratio changes.
/// A page installs it once, before anything mounts.
pub(super) fn install(wake: impl Fn() + 'static) {
    let wake: Rc<dyn Fn()> = Rc::new(wake);
    arm(&wake);
}

/// Listen for the ratio the page draws at now leaving, and `wake` the mounts
/// when it does. The listener for the ratio before is dropped: it cannot fire
/// again, as the page draws at one ratio at a time.
fn arm(wake: &Rc<dyn Fn()>) {
    let ratio = device_ratio();
    let Some(list) = crate::page_theme::media_query(&format!("(resolution: {ratio}dppx)")) else {
        return;
    };
    let listener = {
        let wake = Rc::clone(wake);
        Closure::<dyn FnMut()>::new(move || {
            wake();
            arm(&wake);
        })
    };
    if let Ok(listen) = js_sys::Reflect::get(&list, &"addEventListener".into())
        && let Some(listen) = listen.dyn_ref::<js_sys::Function>()
    {
        let _ = listen.call2(&list, &"change".into(), listener.as_ref());
    }
    ARMED.with_borrow_mut(|armed| *armed = Some(Armed { list, listener }));
}

/// The ratio the page draws at now (`window.devicePixelRatio`).
fn device_ratio() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &"devicePixelRatio".into())
        .ok()
        .and_then(|ratio| ratio.as_f64())
        .filter(|ratio| *ratio > 0.0)
        .unwrap_or(1.0)
}
