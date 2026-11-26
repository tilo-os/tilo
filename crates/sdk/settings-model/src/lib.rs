//! What settings are, as data: where a typed setting lives and how far it travels
//! ([`Scope`], [`SettingsSchema`]), which kind of device reads it ([`DeviceClass`]), the names a
//! theme is chosen by ([`theme`]), the sections every app reads ([`text_assistance`],
//! [`appearance`], [`accessibility`], [`day_schedule`], [`control_hints`],
//! [`personal_dictionary`]) and the document a host installs for its process to read
//! ([`source`]).
//!
//! It keeps no file and draws nothing, so it builds for WebAssembly: `settings-store` keeps the
//! person's document and its typed keys, the toolkit reads and draws it.

pub mod accessibility;
pub mod appearance;
pub mod control_hints;
pub mod day_schedule;
mod device_class;
pub mod dock_edge;
pub mod personal_dictionary;
mod schema;
pub mod source;
pub mod text_assistance;
pub mod theme;

pub use device_class::DeviceClass;
pub use schema::{
    APP_DEVICE_LOCAL_SECTION, APP_PERSONAL_SECTION, MAX_PERSONAL_VALUE_BYTES, Scope, SettingsSchema,
};

#[doc(hidden)]
pub use serde_json as __serde_json;
