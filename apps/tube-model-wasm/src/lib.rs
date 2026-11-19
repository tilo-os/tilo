//! Tube's model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Tube installed applies these verbs from the
//! package, whether or not it ever builds the app: the same bytes on a laptop,
//! a phone, the page and a server. One module serves both resources the app
//! declares, and each request names the kind it is for.

tilo_model_guest::model!("tube", tube_model::Library, tube_model::WatchHistory);
