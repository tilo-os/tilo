//! Video Editor — a resource-native non-linear editor for Tilo, built on the `ui` toolkit.
//!
//! [`model`] holds the pure data and editing math, [`resource`] the project's verbs and the
//! app, and [`app`] the root widget: media bin, preview with transport, timeline, inspector
//! and export dialog, all drawn into one Vello scene. [`gpu`] is the compositor the preview
//! and the export both render through; [`preview`] decides what it composites.
//!
//! The app runs on the app framework ([`VideoEditor`]): the live window and every capture mode
//! host the same editor.

// An app runs as its own sandboxed process, so an integer that wraps here costs one app rather
// than the desktop. The lints that produce a *crash* stay denied — indexing_slicing,
// string_slice, unwrap_used, expect_used, panic — because a panicking app is a bad app whoever
// wrote it. `shell`, `ui` and everything under `crates/` keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod agent_render;
pub mod agent_timeline;
pub mod app;
pub mod export;
pub mod gpu;
pub(crate) mod layout;
#[cfg(not(any(feature = "gst", feature = "decode")))]
mod page_sound;
pub mod portal;
pub mod presence;
pub mod preview;
pub mod project_file;
pub mod resource;
pub mod search;
#[cfg(feature = "samples")]
pub mod seed;
pub mod shot;
pub mod thumbnail;

// The engine the app draws over: the model and its editing math, probing, the project file, the
// audio mix, waveforms and frames. Carried here under the names the app uses them by.
pub use video_edit::{
    angles, audio, curve, files, media, model, param, probe, timecode, waveform,
};
#[cfg(feature = "decode")]
pub use video_edit::video_frames;

pub use app::VideoEditorApp;

/// A probe's refusal as the words a person reads, from the app's catalogue: the message and,
/// where the file's real kind is known, the kind's own words.
#[must_use]
pub fn probe_words(error: &video_edit::probe::ProbeError) -> String {
    use video_edit::probe::ProbeError as E;
    match error {
        E::Io(e) => resource::CATALOG.t_args(
            "video-editor-probe-unreadable",
            &[("error", ui::i18n::ArgValue::Text(e.clone()))],
        ),
        E::Empty => resource::CATALOG.t("video-editor-probe-empty"),
        E::NotMedia { detected } => resource::CATALOG.t_args(
            "video-editor-probe-not-media",
            &[(
                "detected",
                ui::i18n::ArgValue::Text(resource::CATALOG.t(detected)),
            )],
        ),
        E::Unrecognised => resource::CATALOG.t("video-editor-probe-unrecognised"),
    }
}

/// A speed as the window says it, in the person's language: `2×`, `0.5× reverse`.
#[must_use]
pub fn speed_label(speed: &video_edit::model::Speed) -> String {
    let (rate, reverse) = speed.shown();
    resource::CATALOG.t_args(
        if reverse {
            "video-editor-speed-shown-reverse"
        } else {
            "video-editor-speed-shown"
        },
        &[("rate", ui::i18n::ArgValue::Text(rate))],
    )
}
pub use model::Project;
pub use resource::VideoEditor;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(VideoEditor)
}
