//! Private checkpoint protocol and native cache providers. No caller or support
//! route is activated by this module.
mod checkpoint;
pub(super) use checkpoint::*;
#[cfg(test)]
pub(super) mod tests;

#[cfg(any(target_os = "linux", all(test, unix)))]
pub(super) mod linux;

#[cfg(any(windows, test))]
pub(super) mod windows;

#[cfg(all(unix, any(test, feature = "test-support")))]
pub(crate) mod fixture_coordinator;
#[cfg(all(unix, any(test, feature = "test-support")))]
mod fixture_platform;
#[cfg(any(test, all(unix, feature = "test-support")))]
mod fixture_storage;
#[cfg(any(windows, all(target_os = "linux", not(test))))]
pub(crate) mod native_coordinator;

/// Scoped consumer interface: no File, raw handle, clone or path authority.
pub(crate) trait HeldReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> HeldReadSeek for T {}

/// Compose already-executed held-reader boundaries. Callers must perform every
/// guard and cursor restoration before passing the results here.
fn finish_held_read<T>(
    result: Result<T, crate::error::Failure>,
    restored: Result<u64, crate::error::Failure>,
    held: Result<(), crate::error::Failure>,
    root: Result<(), crate::error::Failure>,
    stopped: Result<(), crate::error::Failure>,
) -> Result<T, crate::error::Failure> {
    // Cancellation determines the coordinator's terminal phase, not which
    // already-observed verification or boundary failures are allowed to survive.
    let mut errors = Vec::new();
    for (stage, check) in [
        ("cursor restoration", restored.map(|_| ())),
        ("held final", held),
        ("cache root", root),
        ("stop", stopped),
    ] {
        if let Err(error) = check {
            let message = format!("{stage}: {error}");
            errors.push(error.with_message(message));
        }
    }
    match result {
        Err(error) => {
            errors.insert(0, error.clone());
            let message = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            Err(error.with_message(message).with_uncertainty())
        }
        Ok(value) if errors.is_empty() => Ok(value),
        Ok(_) => {
            let message = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            Err(errors.remove(0).with_message(message).with_uncertainty())
        }
    }
}
