//! Kernel confinement primitives: Landlock rulesets and seccomp-bpf filters.

#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
pub mod read_exclusion;
pub mod seccomp;
