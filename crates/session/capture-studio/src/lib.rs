//! The capture studio: the bar that takes a screenshot, records the screen
//! into a file or keeps the last half minute ready to clip, as an app the
//! session shows over the desktop.
//!
//! A recorder is fed the desktop's own frames — what the compositor draws,
//! read back from it — and encodes them once with the shared media stack,
//! in the most compatible codec this machine can run
//! ([`tilo_callstream::recording_encoder`]). While it records, the access
//! units go into a file ([`tilo_callstream::mp4_recording`]); always, they
//! also go into the replay buffer, which keeps the last half minute
//! encoded so "clip what just happened" needs no second encode and no
//! waiting.
//!
//! The replay buffer is bounded in frames and in bytes and always starts at
//! a keyframe: a clip that began in the middle of a group of pictures could
//! not be decoded.
//!
//! The app itself draws and asks, and the session captures: only the session
//! has the compositor, so the bar puts what the person chose on a [`Link`]
//! ([`Ask`]) and the session carries it out and hands back what came of it
//! ([`Scene`]). Its actions ([`CaptureEffect`]) reach the same asks through
//! the normal action door, so a shortcut, the palette and an agent take a
//! screenshot the way the bar does.

mod recorder;

pub use recorder::{
    Frame, REPLAY_BYTES, REPLAY_FPS, REPLAY_FRAMES, REPLAY_SECONDS, Recorder, Settings, State,
};

pub mod link;
pub mod resource;
pub mod surface;

pub use link::{Ask, Kept, Link, Recording, Scene, Selection, Studio, Take};
pub use resource::{Capture, CaptureEffect, CaptureStudioApp};
pub use surface::StudioSurface;

/// The studio's package id.
pub const APP_ID: &str = "so.tilo.capture-studio";

/// The resource the studio's actions are published on, which a key that
/// takes a screenshot addresses.
pub const RESOURCE: &str = <Capture as ui::tilo_resource::Resource>::KIND;

/// The studio's toolkit definition, the one every host mounts, carrying its
/// committed surface.
#[must_use]
pub fn definition() -> &'static ui::prelude::ApplicationDefinition {
    ui::built_in_definition!(CaptureStudioApp)
}
