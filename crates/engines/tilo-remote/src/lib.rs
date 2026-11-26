//! Remote's engine: the protocols a remote session speaks, with no view of its own.
//!
//! A saved connection says where a machine is and how it signs in; this crate reaches it. An
//! SSH connection is a shell on a channel, with a device-private host key it verifies before
//! signing in, and files moved over its own channels; a VNC connection is a framebuffer the
//! server updates and the input a person sends back. RDP profiles stay saved and manageable,
//! and this crate opens none: the app says so rather than reporting a failed connection.
//!
//! What arrives is handed to whoever asked: a session's output, its frames, how it stands and
//! why it stopped. Nothing here records, draws or trusts on its own: the host key a connection
//! expects comes from the app, which keeps the device's own trust; a password or passphrase is
//! kept in the store the device keeps the app's secrets in, and read from there for one connection
//! ([`asked`]).

#![cfg(not(target_arch = "wasm32"))]
// This runs in the Remote app's own sandboxed process (it is loaded by the app's engine): a
// crash costs one app, not the desktop, so wrapping arithmetic and casts are allowed in the
// protocol code, which checks every size it allocates for before it does. The panic lints stay
// denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "protocol arithmetic in the app's own process; see the note above"
)]

pub mod asked;
pub mod input;
pub mod rfb;
pub mod ssh;
pub mod trust;
pub mod vnc;

pub use input::{Held, Key, Named, VncInput, keysym, vnc_wheel_clicks};
pub use rfb::{Framebuffer, PixelFormat};
pub use ssh::{
    SshAuth, SshConfig, SshSession, SshStatus, Transfer, TransferDirection, default_key_paths,
    resolve_auth, transfer_name,
};
pub use trust::{HostKeyIdentity, HostKeyIssue};
pub use vnc::{VncConfig, VncSession, VncStatus};

/// How far a file has moved, as its job reports it: `done` of `total` bytes when the file's size
/// is known, or the bytes seen when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    Measured { done: u64, total: u64 },
    Seen { seen: u64 },
}
