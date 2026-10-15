//! The `YouTube` `InnerTube` client, as an engine library beside the shared
//! runtime (`libtilo_youtube_engine.so`).
//!
//! Music and Tube are the apps that search and resolve `YouTube` through
//! `rustypipe`, which embeds `QuickJS` (`rquickjs`) to run the player
//! script. Inside `libruntime_dylib.so` every app process mapped and
//! relocated them. Here they are a library of their own in the runtime
//! bundle that only those two apps name `DT_NEEDED`, and each app keeps its
//! tapp free of a copy; the runtime keeps everything they share.
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
pub use rustypipe;

/// Run `rustypipe`'s own code without a network: its identifier grammar
/// and its locale table.
///
/// The engine's check binary and the apps' `--probe-youtube-engine` call it,
/// so a release proves the library loads and runs against the runtime it
/// ships with, and that the apps load it themselves.
///
/// # Errors
/// Names the identifier or locale `rustypipe` judged wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use rustypipe::param::Language;
    use rustypipe::validate;

    let accepted = [
        ("video", validate::video_id("dQw4w9WgXcQ")),
        ("channel", validate::channel_id("UC2DjFE7Xf11URZqWBigcVOQ")),
        (
            "playlist",
            validate::playlist_id("PL4lEESSgxM_5O81EvKCmBIm_JT5Q7JeaI"),
        ),
    ];
    for (kind, verdict) in accepted {
        verdict.map_err(|error| format!("rustypipe refused a valid {kind} id: {error}"))?;
    }
    if validate::video_id("dQw4w9WgXc@").is_ok() {
        return Err("rustypipe accepted an invalid video id".to_owned());
    }
    let language: Language = "de-AT"
        .parse()
        .map_err(|error| format!("rustypipe could not read the locale de-AT: {error}"))?;
    Ok(format!(
        "identifiers checked, locale de-AT is {}",
        language.name()
    ))
}
