//! Tilo application launcher library surface.
//!
//! The launcher is implemented mostly as pure functions so the binary in `main.rs` is
//! just argv parsing + orchestration.

/// What starts each line the launcher itself writes to the app's error output, apart from what
/// the app says there: whoever shows those lines to a person takes it off.
pub const SAYS: &str = "tilo-launcher: ";

pub mod app_cgroup;
pub mod appdir;
#[cfg(target_os = "linux")]
pub mod audio_graph;
pub mod capabilities;
pub mod capture;
#[cfg(target_os = "linux")]
pub mod chromium_frame;
#[cfg(target_os = "linux")]
pub mod dbus_proxy;
pub mod dev_env;
pub mod egress_proxy;
#[cfg(target_os = "linux")]
mod fd;
#[cfg(target_os = "linux")]
pub mod gpu;
pub mod hardening;
#[cfg(target_os = "linux")]
mod headless_console;
#[cfg(target_os = "linux")]
pub mod host;
#[cfg(target_os = "linux")]
pub mod inherit;
#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
pub mod landlock_shim;
#[cfg(target_os = "linux")]
pub mod lease;
#[cfg(target_os = "linux")]
#[cfg(unix)]
pub mod loader_preflight;
pub mod manifest;
#[cfg(target_os = "linux")]
pub mod memory_monitor;
pub mod memory_pressure;
pub mod mesa_seed;
#[cfg(target_os = "linux")]
pub mod netproxy;
#[cfg(target_os = "linux")]
pub mod owned_service;
pub mod performance;
pub mod priority;
pub mod profile;
#[cfg(target_os = "linux")]
pub mod provider_sandbox;
// Where it lives: hosts that start no launcher hold the apps they start themselves
// (`runtime_portal::launch_hold`).
#[cfg(unix)]
pub use runtime_portal::launch_hold;
#[cfg(target_os = "linux")]
pub mod run;
#[cfg(target_os = "linux")]
pub mod runtime_resident;
#[cfg(target_os = "linux")]
pub mod sandbox;
#[cfg(target_os = "linux")]
pub mod sandbox_view;
pub mod scope;
pub mod seccomp;
pub mod services;
#[cfg(target_os = "linux")]
pub mod system_authority;
#[cfg(target_os = "linux")]
pub mod system_stats;
pub mod user_namespaces;
