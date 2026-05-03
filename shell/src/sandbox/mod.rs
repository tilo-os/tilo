//! Per-workspace Bubblewrap sandbox primitives.
//!
//! Phase 1 of `docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md`:
//! a `SandboxProfile` value type that renders to a `bwrap`-compatible argv,
//! plus a thin spawn helper. Higher phases (seccomp, Wayland bridge, portal
//! grants) layer on top of this without changing the wire shape here.

pub mod profile;
pub mod seccomp;
pub mod spawn;

pub use profile::SandboxProfile;
pub use seccomp::{cache_blob, cache_blob_at, compile_workspace_seccomp};
pub use spawn::spawn_in_workspace;
