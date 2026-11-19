//! The App Publisher model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with App Publisher installed applies its checks
//! from the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server.

tilo_model_guest::model!("publisher", publisher_model::Publisher);
