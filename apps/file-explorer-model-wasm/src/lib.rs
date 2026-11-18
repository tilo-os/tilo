//! The Files model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Files installed applies its verbs from the
//! package, whether or not it ever builds the app: the same bytes on a laptop, a
//! phone, the page and a server.

/// Apply one request from the device that holds the workspace's keys.
#[unsafe(no_mangle)]
pub extern "C" fn tilo_model_apply() -> i32 {
    tilo_model_guest::serve::<file_explorer_model::recent::RecentResource>()
}
