//! The Chat model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Chat installed applies these verbs from the
//! package, whether or not it ever builds the app: the same bytes on a laptop,
//! a phone, the page and a server.
//!
//! The module serves the workspace's chat resource: its channels and
//! categories, each person's read marks, reviewed feedback, moderation and what
//! an agent keeps of a conversation. Each person's drafts are their own, written
//! on their own devices, so no other holder applies them.

/// Apply one request from the device that holds the workspace's keys.
#[unsafe(no_mangle)]
pub extern "C" fn tilo_model_apply() -> i32 {
    tilo_model_guest::serve(&[tilo_model_guest::Served::of::<chat_model::Chat>()])
}
