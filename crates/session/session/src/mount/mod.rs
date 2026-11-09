//! The parts of the session that are assembled beside the desk: each in a file of its own,
//! installed in one place, so a part can be written and read without reading the whole host.

pub mod agents;
pub mod app_tabs;
pub mod apps;
pub mod audio;
pub(crate) mod clipboard;
pub(crate) mod clipboard_files;
pub mod comforts;
pub mod connectors;
pub(crate) mod devices;
pub(crate) mod doors;
pub mod drives;
pub mod firstboot;
pub(crate) mod host_shell;
pub mod indexes;
pub(crate) mod language;
pub(crate) mod overview;
pub(crate) mod panels;
pub(crate) mod power;
pub(crate) mod render;
pub(crate) mod renewal;
pub(crate) mod search;
pub(crate) mod services;
pub(crate) mod settings_sync;
pub(crate) mod sheets;
pub mod shell;
pub mod slots;
pub(crate) mod tools;
pub(crate) mod vpn;
pub(crate) mod wake;
