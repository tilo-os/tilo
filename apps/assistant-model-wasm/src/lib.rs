//! The Assistant model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Assistant installed applies its verbs from
//! the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server.

tilo_model_guest::model!("assistant", assistant_model::Window);
