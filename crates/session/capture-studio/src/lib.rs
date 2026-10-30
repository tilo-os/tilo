//! Recording the screen into a file, and the replay buffer.
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

mod recorder;

pub use recorder::{
    Frame, REPLAY_BYTES, REPLAY_FPS, REPLAY_FRAMES, REPLAY_SECONDS, Recorder, Settings, State,
};
