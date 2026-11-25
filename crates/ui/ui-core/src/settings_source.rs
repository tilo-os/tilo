//! The settings document this process reads: appearance, accessibility, locale, the person's
//! words and every app's typed settings come out of it.
//!
//! A host installs it ([`install_source`]): the session its workspace's settings, a sandboxed app
//! its channel to them, a page what its host gives it. With Tilo's model a process that installed
//! none reads what the profile's session left for processes outside it; a window of the toolkit
//! alone reads nothing, so every setting is its default and its appearance and language are the
//! system's.

use std::sync::Arc;

use serde_json::{Map, Value};
pub use settings_model::source::{
    ChangeOrigin, ChangeSignal, HostDocument, Listener, SettingsSource, SettingsStamp, SourceScope,
    install_source, installed_source, installed_version, keep_profile, load_section_in, parse,
    profile, profile_source, scoped_source,
};

/// The settings of this process, `None` where there are none and every setting reads as its
/// default.
#[must_use]
pub fn source() -> Option<Arc<dyn SettingsSource>> {
    #[cfg(feature = "model")]
    {
        settings_store::source()
    }
    #[cfg(not(feature = "model"))]
    {
        installed_source()
    }
}

/// The whole document; empty without settings.
#[must_use]
pub fn load() -> Map<String, Value> {
    source().map(|source| source.load()).unwrap_or_default()
}

/// Section `name` of the document as an object; empty when it is missing or not an object.
#[must_use]
pub fn load_section(name: &str) -> Map<String, Value> {
    source()
        .map(|source| load_section_in(&*source, name))
        .unwrap_or_default()
}

/// The version of the document, for readers that follow it.
#[must_use]
pub fn stamp() -> Option<SettingsStamp> {
    source().map(|source| SettingsStamp(source.revision()))
}

/// Make every settings change stored before going on, which the native runner does before the
/// process ends.
pub fn flush_pending_writes() {
    if let Some(source) = source() {
        source.flush_pending();
    }
}

/// The interface language as a BCP 47 tag: the one the person chose in Settings, or else the
/// locale the device runs in.
#[must_use]
pub fn preferred_language() -> Option<String> {
    #[cfg(feature = "model")]
    {
        settings_store::appearance::preferred_language()
    }
    #[cfg(not(feature = "model"))]
    {
        tilo_process::environment::environment()
            .locale
            .as_deref()
            .and_then(tilo_process::environment::parse_posix_locale)
            .map(|locale| locale.to_string())
    }
}
