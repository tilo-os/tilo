//! End-to-end orchestration for cross-host collaboration surfaces.
//!
//! Ties together `network::streaming` (encoder loop + share registry),
//! `shell::peer_view` (receive-side widget that decodes + paints) and
//! `network::actor` (the Iroh gossip actor that ships bytes) into
//! a story a user can point at: **"share a Flatpak app window to my
//! workspace peers"**. See [`flatpak_share::SharedFlatpakSession`].
//!
//! Other collab-scoped modules (peer invites, per-user permission
//! prompts) will land here in follow-up work; today the module has
//! only `flatpak_share`.

pub mod colorimetry;
pub mod encoder;
pub mod flatpak_share;
pub mod hdr_metadata;
