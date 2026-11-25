//! The person's personal vocabulary, shared by every input surface that checks or helps with
//! spelling: the word processor, the on-screen keyboard and dictation.
//!
//! The words are a section of the person's settings document ([`SECTION`]) beside their other
//! settings, so they follow them to their other devices. A consumer reads through a [`Reader`]
//! bound to one document; a reader is revoked when the session replaces that document.

pub use settings_model::personal_dictionary::*;

pub mod delivery;
