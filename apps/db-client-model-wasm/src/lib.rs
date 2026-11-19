//! The Database Client model as the WebAssembly module the app's package
//! carries.
//!
//! Every holder of a workspace with Database Client installed applies its verbs
//! from the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server.

tilo_model_guest::model!("db-client", db_client_model::Workspace);
