//! What settings are, as data: where a typed setting lives and how far it travels
//! ([`Scope`], [`SettingsSchema`]), which kind of device reads it ([`DeviceClass`]), the names a
//! theme is chosen by ([`theme`]) and the sections every app reads ([`text_assistance`]).
//!
//! It holds no store and draws nothing, so it builds for WebAssembly: `settings-store` keeps the
//! document, the toolkit reads and draws it.

mod device_class;
mod schema;
pub mod text_assistance;
pub mod theme;

pub use device_class::DeviceClass;
pub use schema::{APP_DEVICE_LOCAL_SECTION, APP_PERSONAL_SECTION, Scope, SettingsSchema};

#[doc(hidden)]
pub use serde_json as __serde_json;
