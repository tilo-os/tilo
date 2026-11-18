//! The Chat model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Chat installed applies these verbs from the
//! package, whether or not it ever builds the app: the same bytes on a laptop,
//! a phone, the page and a server. The workspace's chat resource and each
//! person's drafts are the two resources it serves; the channels' messages are
//! kept in their own parts beside the resource.

/// Apply one request from the device that holds the workspace's keys.
#[unsafe(no_mangle)]
pub extern "C" fn tilo_model_apply() -> i32 {
    tilo_model_guest::serve(&[
        tilo_model_guest::Served::of::<chat_model::Chat>(),
        tilo_model_guest::Served::of::<chat_model::ChatDrafts>(),
    ])
}
