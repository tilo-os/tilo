//! A call's media frames: what one device's stream is, how its frames are
//! sealed, which devices may send them, and the lane they travel on.
//!
//! A call lives in a channel, so everyone in it already holds the keys the
//! channel's content is sealed with: the per-epoch content keys MLS derives
//! (`channel_crypto`). This crate derives one media key per stream from them
//! ([`channel_crypto::ChannelContentKeys::media_key`]) and seals every frame
//! under it, instead of spending a random nonce and a whole at-rest envelope
//! on each 20 ms frame.
//!
//! A frame on the wire is its header followed by its ciphertext:
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 8 | epoch (LE) |
//! | 4 | sequence (LE), the sender's counter under that key |
//! | .. | body sealed with XChaCha20-Poly1305, 16-byte tag appended |
//!
//! The epoch and the sequence travel in the clear — a receiver picks the key
//! by them — and both, with the stream, are the tag's associated data, so a
//! frame does not authenticate as another stream's, another position's, or
//! another epoch's. The sender is not in the frame: the lane knows which
//! device it is talking to, and the check that the device is on the roster
//! happens before anything is decrypted ([`Roster`]).
//!
//! Sealing derives the key once per stream ([`Sealer`]); opening derives it
//! per frame from whichever epoch the frame names, which is what lets a
//! member that joined later open frames a lagging sender still seals under
//! an earlier epoch. A device removed from the channel holds no key for the
//! epochs after its removal and so derives nothing for them ([`Refusal::NoKey`]).
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

pub mod audio;
pub mod carrier;
pub mod frame;
pub mod roster;
pub mod video;

pub use audio::{Heard, Microphone, heard};
pub use carrier::{ALPN, Calls, Failed, Hello, Peer};
pub use frame::{Lane, OVERHEAD, Opener, Refusal, Sealer, Stream};
pub use roster::Roster;
pub use tilo_moq::video::{Layer, Source};
pub use video::{
    Asking, CAMERA_FPS, CAMERA_SIZE, Camera, Feed, Frame, Inbox, Picture, PicturePeer, SCREEN_FPS,
    Sending, Sink, Video, even_rgba, fitted,
};
