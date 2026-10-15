//! The Activity pane (Catch up, Explore, Replay and Tasks), loaded the first
//! time someone opens it.
//!
//! Like an app's `definition()` (`crate::split`), the export below is the
//! only way to `tilo_desktop_surface::open_activity`: the surface drives an
//! open pane through its vtable, so the splitter moves the pane's code into
//! its own module, which the runtime fetches through [`load`] the first time
//! the pane opens (`tilo_browser_runtime::defer_activity_pane`).

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;

use tilo_desktop_surface::OpenActivity;

use crate::split::Callback;

#[link(wasm_import_module = "./__wasm_split.js")]
#[allow(
    improper_ctypes,
    reason = "the function pointer only passes between Rust"
)]
unsafe extern "C" {
    #[link_name = "__wasm_split_load_activity_3d8495f7419df864_open"]
    fn load_module(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___activity___00_import_3d8495f7419df864_open"]
    fn open() -> OpenActivity;
}

#[unsafe(export_name = "__wasm_split_00___activity___00_export_3d8495f7419df864_open")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export() -> OpenActivity {
    tilo_desktop_surface::open_activity
}

fn load() -> Pin<Box<dyn Future<Output = Option<OpenActivity>>>> {
    Box::pin(async {
        if !crate::split::load(load_module).await {
            return None;
        }
        // SAFETY: the module that defines `open` is loaded.
        Some(unsafe { open() })
    })
}

pub(crate) fn register() {
    tilo_browser_runtime::defer_activity_pane(load);
}
