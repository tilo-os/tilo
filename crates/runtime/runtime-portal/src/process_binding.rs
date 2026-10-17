//! Bind an inherited native service to its executing task, not its launcher.
//! This proves kernel process facts only. The host must separately admit that
//! task under the exact original launch; neither a socket nor ancestry is a grant.

/// Launcher-authored protocol selection, never authority or an identity claim.
pub const ENV: &str = "TILO_COLLAB_EXEC_PEER";
/// One-time native process binding before Cap'n Proto traffic begins.
pub const MAGIC: [u8; 4] = *b"QPE1";

mod proof;
pub use proof::Proof;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    ExecutingPeer, Receiver, arm, complete, receive, receive_nonroot_for_authentication,
};
