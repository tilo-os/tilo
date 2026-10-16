//! Integration tests for the job sandbox. They need a Linux host where
//! unprivileged user namespaces (or root) work; elsewhere each prints why it
//! skipped and passes, as the kernel can't be observed. Limits tests also
//! need a delegated cgroup; quota tests need an XFS directory mounted with
//! `prjquota` at `$JOB_SANDBOX_TEST_XFS`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

#[cfg(target_os = "linux")]
mod host_ids;
#[cfg(target_os = "linux")]
mod isolation;
#[cfg(target_os = "linux")]
mod limits;
#[cfg(target_os = "linux")]
mod seccomp;
#[cfg(target_os = "linux")]
mod startup;
#[cfg(target_os = "linux")]
mod support;
#[cfg(target_os = "linux")]
mod vm;

#[cfg(not(target_os = "linux"))]
#[test]
fn only_linux_runs_jobs() {
    eprintln!("skipped: the job sandbox is Linux-only");
}
