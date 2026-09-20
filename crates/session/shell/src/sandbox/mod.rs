//! Compositor-side sandbox primitives.
//!
//! ## Where enforcement actually happens
//!
//! The compositor does **not** spawn applications itself. It execs
//! `tilo-launcher` (the `launcher` crate), and that process is what
//! builds the bubblewrap namespace, compiles the seccomp filter,
//! installs Landlock and execs the app. It is the single enforcement
//! point, and it is where a workspace's capabilities become real kernel
//! restrictions — see [`crate::shell::launcher`] for the shell's side
//! of the handoff and `launcher::capabilities` for the other.
//!
//! What is left here:
//!
//! - [`landlock_shim`] — the compositor binary's own `__sandbox-exec`
//!   re-entry point, dispatched from `main.rs`. Live code.
//! - [`profile`] — [`SandboxProfile`], the value type describing a
//!   per-workspace bubblewrap sandbox and rendering it to argv. It is
//!   **not** on any launch path today; the spawn helper that used it
//!   had no callers anywhere in the repository and was removed rather
//!   than left standing as a decoy enforcement point.
//! - [`seccomp`] — the per-workspace filter compiler that feeds
//!   [`profile`].

#[cfg(target_os = "linux")]
pub mod landlock_shim;
pub mod profile;
#[cfg(target_os = "linux")]
pub mod seccomp;

#[cfg(target_os = "linux")]
pub use landlock_shim::run_sandbox_exec;
pub const SANDBOX_EXEC_SUBCOMMAND: &str = "__sandbox-exec";
pub use profile::SandboxProfile;
#[cfg(target_os = "linux")]
pub use seccomp::{cache_blob, cache_blob_at, compile_workspace_seccomp};
