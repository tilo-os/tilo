//! The `YouTube` `InnerTube` client, as its own library in the runtime bundle
//! (`libtilo_youtube_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use rustypipe;
// The queries themselves, with their response models' decoding: generic in
// rustypipe, so without these each app compiled its own copy.
#[cfg(feature = "thin")]
pub use tilo_innertube;

/// Run `rustypipe`'s own code without a network: its identifier grammar
/// and its locale table.
///
/// The engine's check binary and the app's `--probe-…` call it, so a release
/// proves the library loads and runs against its runtime.
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
