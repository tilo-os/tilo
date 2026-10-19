//! The emoji catalogue, as an engine library beside the shared runtime
//! (`libtilo_emoji_engine.so`).
//!
//! Chat's picker and the on-screen keyboard's suggestions name and find
//! emoji in the person's language (`emoji`): Unicode's emoji with their names
//! and keywords in every locale Tilo speaks, about half a megabyte of data
//! with the code that reads it. Linked into each of those tapps it was the
//! largest part of Chat's data; here it is a library of its own in the
//! runtime bundle that only those apps name `DT_NEEDED`, stored once.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides, which
//! records it as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
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
///
/// # Errors
/// Names the catalogue that read wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let english = emoji::Catalogue::english();
    let count = english.emoji().len();
    if count == 0 || english.search("smile", 1).is_empty() {
        return Err(format!("the English catalogue of {count} emoji finds no smile"));
    }
    let mut locales = 0_usize;
    for tag in emoji::Catalogue::locales() {
        let translated = emoji::Catalogue::for_locale(tag).emoji().len();
        if translated != count {
            return Err(format!("the {tag} catalogue names {translated} emoji, not {count}"));
        }
        locales = locales.saturating_add(1);
    }
    Ok(format!("{count} emoji in {locales} locales"))
}
