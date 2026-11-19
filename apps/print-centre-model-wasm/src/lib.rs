//! The Print Centre model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Print Centre installed applies its defaults
//! from the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server.

tilo_model_guest::model!("print-centre", print_centre_model::PrintDefaults);
