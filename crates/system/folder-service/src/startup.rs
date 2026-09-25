//! Early self-reexecution dispatch for native administrative modes.
//!
//! Hosts must call this immediately after collecting arguments, before creating
//! UI, a runtime, or threads. Native receivers self-reexecute the current binary;
//! every production host must compose this dispatcher into its entry point.
pub fn dispatch_early(args: &[String]) -> std::io::Result<Option<i32>> {
    #[cfg(target_os = "linux")]
    {
        if let Some(code) = crate::connection::maybe_run_lease_receiver(args) {
            return Ok(Some(code));
        }
        return crate::migration_native::maybe_run_command(args);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        Ok(None)
    }
}
