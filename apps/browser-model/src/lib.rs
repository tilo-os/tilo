//! The `browser.application` resource: its bookmarks, preferences, site exceptions and the
//! forms its pages remembered, with the verbs that change them.

pub mod application;
pub mod library;
pub mod settings;
pub mod site_memory;
pub mod urls;

pub use application::{Browser, browser};
