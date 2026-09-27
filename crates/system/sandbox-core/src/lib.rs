//! Kernel confinement primitives: Landlock rulesets and seccomp-bpf filters.
//!
//! Every Tilo process that confines another — the application launcher
//! (`tilo-launcher`) and the shell runtime (`ush`) — builds its Landlock
//! ruleset and compiles its syscall filter here, so there is one
//! implementation of each to review and one place a fix lands.
//!
//! - [`landlock`] — a declarative [`landlock::RulesetSpec`] and the one
//!   function that installs it on the calling process.
//! - [`seccomp`] — deny-list and allow-list compilation to BPF, and the
//!   baseline application policy ([`seccomp::SeccompProfile`]).
//!
//! What stays with each caller is policy input: which paths an app may
//! read, whether a package is entitled to write-and-execute memory, how an
//! outcome is reported to a user. This crate holds the mechanism and the
//! baseline application filter, not those decisions.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(target_os = "linux")]
pub mod landlock;
pub mod seccomp;
