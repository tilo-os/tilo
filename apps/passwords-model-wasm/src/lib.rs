//! The Passwords model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Passwords installed applies its actions from
//! the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server. The document is empty — the vault holds
//! no secret material — so the module carries only the resource's declaration.

/// Apply one request from the device that holds the workspace's keys.
#[unsafe(no_mangle)]
pub extern "C" fn tilo_model_apply() -> i32 {
    tilo_model_guest::serve::<passwords_model::Passwords>()
}
