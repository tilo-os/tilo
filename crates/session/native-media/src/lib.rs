//! Voice calls on the native desk: the channel an app's call is in, who is
//! in each channel of the record for it to show, and this device's own call,
//! which the session says in its presence so everyone in the room reads where
//! this person is.
//!
//! What lives here:
//!
//! - [`media`] — what carries a call: the factory that puts this device into
//!   a channel and knows who is in the record's channels, and the session
//!   that says where one call stands and what microphone it sends.
//! - [`service::NativeMediaService`] — the call service an app reaches
//!   through its session's action door: joining, leaving, muting, deafening
//!   and watching, each taken only above the last intent its own admitted
//!   connection carried.

pub mod media;
pub mod service;

pub use media::{CallTarget, MediaFactory, MediaSession, Prepare, Rosters};
pub use service::{Caller, NativeMediaService, OwnCall};
