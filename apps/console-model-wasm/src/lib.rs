//! The Console model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Console installed applies its verb from the
//! package, whether or not it ever builds the app: the same bytes on a laptop,
//! a phone, the page and a server.

tilo_model_guest::model!("console", console_model::Console);
