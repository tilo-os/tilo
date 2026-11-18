//! Hex Editor's resource: a file's bytes kept in the document, so they sync,
//! undo and show in history like any document, as the app that draws them and
//! an agent that reads and edits them both see them.
//!
//! Reading a file from the device is the app's; the bytes a file had reach the
//! document through the `load` verb, which no undo takes back.

// Offsets and page arithmetic are bounded by the buffer's own length, and a
// byte written to the document is one of the 256; neither can overflow an
// address. Panic-producing lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "bounded offset and byte arithmetic on the buffer's own length"
)]

pub mod buffer;
pub mod model;
mod resource;

pub use buffer::Hex;
pub use resource::{HexEditor, hex_editor};
