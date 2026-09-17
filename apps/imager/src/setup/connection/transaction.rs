//! Private primitive plumbing. No worker or store progression is exposed here.
mod native;

#[cfg(target_os = "linux")]
pub(super) fn maybe_run_lease_receiver(args: &[String]) -> Option<i32> {
    native::linux::maybe_run_lease_receiver(args)
}
