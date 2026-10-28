//! The Feedback composer, loaded the first time someone opens Feedback.
//!
//! Like an app's `definition()` (`crate::split`), the export below is the
//! only way to the composer's code, so the splitter moves it into its own
//! module. The composer registers with the application registry as a
//! deferred application: its windows reserve their place while the module
//! loads, and the load hands the runtime the composer's code
//! (`tilo_browser_runtime::install_feedback_composer`).

use std::ffi::c_void;

use tilo_browser_runtime::FeedbackComposerCode;
use ui::application::registry::{self, Loading};

use crate::split::Callback;

#[link(wasm_import_module = "./__wasm_split.js")]
#[allow(
    improper_ctypes,
    reason = "the function pointer only passes between Rust"
)]
unsafe extern "C" {
    #[link_name = "__wasm_split_load_feedback_6b0f3c92d1e84a57_composer"]
    fn load_module(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___feedback___00_import_6b0f3c92d1e84a57_composer"]
    fn composer() -> fn() -> FeedbackComposerCode;
}

#[unsafe(export_name = "__wasm_split_00___feedback___00_export_6b0f3c92d1e84a57_composer")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export() -> fn() -> FeedbackComposerCode {
    tilo_browser_runtime::builtin_feedback_composer
}

fn open() -> Loading {
    Box::pin(async {
        if !crate::split::load(load_module).await {
            return None;
        }
        // SAFETY: the module that defines `composer` is loaded.
        let make = unsafe { composer() };
        Some(tilo_browser_runtime::install_feedback_composer(make))
    })
}

pub(crate) fn register() {
    registry::defer(tilo_browser_runtime::FEEDBACK_COMPOSER, "Feedback", open);
}
