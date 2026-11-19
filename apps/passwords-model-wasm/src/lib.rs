//! The Passwords model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Passwords installed applies its actions from
//! the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server. The document is empty — the vault holds
//! no secret material — so the module carries only the resource's declaration.

tilo_model_guest::model!("passwords", passwords_model::Passwords);
