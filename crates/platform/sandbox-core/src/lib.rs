//! Kernel confinement primitives: Landlock rulesets and seccomp-bpf filters, and what the dynamic
//! loader finds of an app's libraries inside a sandbox.

#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(unix)]
pub mod loader_preflight;
#[cfg(target_os = "linux")]
pub mod read_exclusion;
pub mod seccomp;
