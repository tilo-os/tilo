//! The host's fluent catalogue: chrome only. What an app says comes from the app's own source.
//! The catalogue is the UI host's (`tilo_ui_host::i18n`), which says the window's chrome; the
//! standard library's frames say its phrases through the instance.

pub use tilo_ui_host::i18n::{CATALOG, ensure, t, t_args};
