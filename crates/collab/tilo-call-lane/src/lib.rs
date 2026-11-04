//! The connections a call's media travels on: the lanes two devices open to
//! each other, the page's side of a call and the bridge that carries it.
//!
//! The frames themselves are [`tilo_live`]'s: how a stream's frames are
//! sealed under its scope's content keys ([`frame`]), which devices may send
//! them ([`roster`]), and the tilo-voice/1 and tilo-video/1 object profiles
//! ([`voice`], [`picture`]) are the live plane's, so a lane, a plane and a
//! page all seal, open and read the same objects. This crate re-exports them
//! and adds only what is a lane's own.
//!
//! Voice frames travel as the datagrams of a tilo-voice/1 bundle
//! ([`audio::Microphone`], [`audio::heard`]) on the connections two devices
//! open for one call ([`carrier`]): the endpoint those connections run on,
//! and what a call does with a datagram it opens, are the caller's.
//!
//! Pictures travel the same connections' streams: one camera and one screen
//! per device, encoded once per layer a device in the call shows
//! ([`video::Video`]), each tilo-video/1 object sealed under its stream's
//! key and read back into the app's [`video::Inbox`].
//!
//! A page has no endpoint to open a lane on, so it reaches the call through
//! its own device: [`page`] is the page's side, sealing its media as the
//! device that bridges it and opening what the call sends, and [`bridge`] is
//! that device's side, carrying the sealed frames on the lane without ever
//! holding a key of the call. Both speak the frames' codes ([`wire`]) and
//! neither needs the lane's transport; [`page`] builds in a page, and the
//! lane ([`carrier`], [`video`]) is behind the `lanes` feature.

pub mod audio;
#[cfg(feature = "lanes")]
pub mod bridge;
#[cfg(feature = "lanes")]
pub mod carrier;
pub mod page;
#[cfg(feature = "lanes")]
pub mod video;
pub mod wire;

// The frames, the roster and the two object profiles are the live plane's,
// which seals and opens the same way whichever carrier it runs on.
pub use tilo_live::video as picture;
pub use tilo_live::{frame, roster, voice};

pub use audio::{Heard, Microphone, heard};
#[cfg(feature = "lanes")]
pub use bridge::Bridge;
#[cfg(feature = "lanes")]
pub use carrier::{ALPN, Calls, Failed, Hello, Peer};
pub use frame::{Lane, OVERHEAD, Opener, Refusal, Sealer, Stream, TrackName};
pub use page::Page;
pub use picture::{Layer, Source};
pub use roster::Roster;
#[cfg(feature = "lanes")]
pub use video::{
    Asking, CAMERA_FPS, CAMERA_SIZE, Camera, Feed, Frame, Inbox, Picture, PicturePeer, SCREEN_FPS,
    Sending, Sink, Video, even_rgba, fitted,
};
