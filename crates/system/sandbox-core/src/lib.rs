//! Kernel confinement primitives: Landlock rulesets and seccomp-bpf filters.
//!
//! Every Tilo process that confines another — the application launcher
//! (`tilo-launcher`), the shell runtime (`ush`) and the CI job sandbox
//! (`tilo-sandbox`'s `job` module) — builds its Landlock
//! ruleset and compiles its syscall filter here, so there is one
//! implementation of each to review and one place a fix lands.
//!
//! - [`landlock`] — a declarative [`landlock::RulesetSpec`] and the one
//!   function that installs it on the calling process.
//! - [`seccomp`] — deny-list and allow-list compilation to BPF, and the
//!   baseline application policy ([`seccomp::SeccompProfile`]), and the CI
//!   job policy ([`seccomp::CiProfile`]).
//!
//! What stays with each caller is policy input: which paths an app may
//! read, whether a package is entitled to write-and-execute memory, how an
//! outcome is reported to a user. This crate holds the mechanism and the
//! baseline application filter, not those decisions.

#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
pub mod read_exclusion;
pub mod seccomp;
