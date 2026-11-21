//! The Activity model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Activity installed applies its verbs from
//! the package, whether or not it ever builds the app: the same bytes on a
//! laptop, a phone, the page and a server. The window's record has none of its
//! own; the module is what makes that record one a device without the app
//! still reads.

tilo_model_guest::model!("activity", activity_model::Window);
