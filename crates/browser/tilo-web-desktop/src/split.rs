//! Apps loaded when they first open, the Activity pane (`crate::activity`),
//! and framework code loaded when first used (`crate::lazy`).
//!
//! `build.sh` splits the linked module with tools/web/wasm-split: what only
//! one app's `definition()` reaches moves into that app's own module, which
//! the page fetches the first time the app is needed. Each split app is a
//! pair of functions named by the splitter's convention (generated in
//! `apps.rs`): an export that calls the app's `definition()`, kept in the
//! app's module, and an import of the same function, which the splitter
//! turns into a call through the function table once the module is in. A
//! `load` import from `__wasm_split.js` fetches and instantiates the module
//! and reports back through [`loaded`].
//!
//! `build.sh` splits every release desktop bundle. A quick development
//! bundle may be left whole (`build.sh --no-split`), with a
//! `__wasm_split.js` that answers each split point from the module itself.

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use tilo_lock::MutexRecover as _;
use ui::application::ApplicationDefinition;

/// How the page reports a module's load: `(data, loaded)`.
pub(crate) type Callback = unsafe extern "C" fn(*const c_void, bool);

/// The page's loader for one app's module.
pub(crate) type LoadModule = unsafe extern "C" fn(Callback, *const c_void);

/// The app's `definition()`, callable once its module is loaded.
pub(crate) type Definition = unsafe extern "C" fn() -> *const ApplicationDefinition;

/// Where the page's callback meets the waiting future. Shared and locked
/// although the page is single-threaded, so the load is a `Send` future the
/// app registry can hold for any host.
#[derive(Default)]
struct Slot {
    state: Mutex<SlotState>,
}

#[derive(Default)]
struct SlotState {
    loaded: Option<bool>,
    waker: Option<Waker>,
}

impl Slot {
    fn state(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock_recover()
    }
}

/// Called by `__wasm_split.js` through the function table when a module
/// finished loading (or failed to).
unsafe extern "C" fn loaded(data: *const c_void, loaded: bool) {
    // SAFETY: `data` is the `Arc<Slot>` `Load::poll` leaked into the call,
    // and the loader calls back exactly once per call.
    let slot = unsafe { Arc::from_raw(data.cast::<Slot>()) };
    let waker = {
        let mut state = slot.state();
        state.loaded = Some(loaded);
        state.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// [`loaded`] for a module left whole (`wasm-split --unsplit`, the quick
/// development bundle): its loaders cannot reach the function table, so
/// they report every app loaded through this export.
#[unsafe(export_name = "tilo_split_loaded_in_place")]
unsafe extern "C" fn loaded_in_place(data: *const c_void) {
    // SAFETY: as for `loaded`, which the loader would have called.
    unsafe { loaded(data, true) }
}

struct Load {
    module: LoadModule,
    slot: Option<Arc<Slot>>,
}

impl Future for Load {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<bool> {
        if let Some(slot) = &self.slot {
            let mut state = slot.state();
            if let Some(loaded) = state.loaded {
                return Poll::Ready(loaded);
            }
            state.waker = Some(context.waker().clone());
            drop(state);
            return Poll::Pending;
        }
        let slot = Arc::new(Slot::default());
        slot.state().waker = Some(context.waker().clone());
        let data = Arc::into_raw(Arc::clone(&slot)).cast::<c_void>();
        // SAFETY: the page's loader keeps `data` until it calls `loaded`.
        unsafe { (self.module)(loaded, data) };
        self.slot = Some(slot);
        Poll::Pending
    }
}

/// Load a split point's module; `false` when the page could not.
pub(crate) async fn load(module: LoadModule) -> bool {
    Load { module, slot: None }.await
}

/// Load an app's module, then ask it for the app's definition.
pub(crate) async fn definition(
    module: LoadModule,
    definition: Definition,
) -> Option<&'static ApplicationDefinition> {
    if !load(module).await {
        return None;
    }
    // SAFETY: the module that defines `definition` is loaded; it returns a
    // leaked, never freed definition.
    unsafe { definition().as_ref() }
}
