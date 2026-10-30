//! The parts of the session that are assembled beside the desk: each in a
//! file of its own, installed in one place, so a part can be written and
//! read without reading the whole host.

pub(crate) mod clipboard;
pub(crate) mod clipboard_files;
pub mod apps;
pub mod audio;
pub mod comforts;
pub mod firstboot;
pub mod lock;
pub(crate) mod power;
