//! End-to-end orchestration for cross-host collaboration surfaces.
//!
//! Historically the shell's collab plumbing lived in three separate
//! modules that never composed:
//!
//! - `network::streaming` — the encoder loop + share registry primitives.
//! - `shell::peer_view` — the receive-side widget that decodes + paints.
//! - `network::actor` — the libp2p gossipsub actor that ships bytes.
//!
//! This module is the "session" layer that ties them into a story a
//! user can point at: **"share a Flatpak app window to my workspace
//! peers"**. See [`flatpak_share::SharedFlatpakSession`].
//!
//! Other collab-scoped modules (peer invites, per-user permission
//! prompts) will land here in follow-up work; today the module has
//! only `flatpak_share`.

pub mod colorimetry;
pub mod encoder;
pub mod flatpak_share;
pub mod hdr_metadata;
