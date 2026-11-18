//! The Poker model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Poker installed applies its actions from the
//! package, whether or not it ever builds the app: the same bytes on a laptop, a
//! phone, the page and a server.

tilo_model_guest::model!("poker", poker_model::Table);
