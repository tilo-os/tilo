//! The emoji catalogue, as its own library in the runtime bundle
//! (`libtilo_emoji_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use emoji;

/// Run the engine's own code: read the English catalogue and search it for a
/// smile, then read every translated catalogue, each of which must name as
/// many emoji as the English one.
///
/// The engine's check binary calls it, so a release proves the library loads
/// and runs against the runtime it ships with.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let english = emoji::Catalogue::english();
    let count = english.emoji().len();
    if count == 0 || english.search("smile", 1).is_empty() {
        return Err(format!(
            "the English catalogue of {count} emoji finds no smile"
        ));
    }
    let mut locales = 0_usize;
    for tag in emoji::Catalogue::locales() {
        let translated = emoji::Catalogue::for_locale(tag).emoji().len();
        if translated != count {
            return Err(format!(
                "the {tag} catalogue names {translated} emoji, not {count}"
            ));
        }
        locales = locales.saturating_add(1);
    }
    Ok(format!("{count} emoji in {locales} locales"))
}
