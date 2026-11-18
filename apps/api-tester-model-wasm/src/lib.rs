//! The API Tester model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with API Tester installed applies its verbs from
//! the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server.

/// Apply one request from the device that holds the workspace's keys.
#[unsafe(no_mangle)]
pub extern "C" fn tilo_model_apply() -> i32 {
    tilo_model_guest::serve(&[tilo_model_guest::Served::of::<api_tester_model::Workspace>()])
}
