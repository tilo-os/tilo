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

#[cfg(any(target_os = "linux", windows, all(test, unix)))]
pub(super) mod coordinator;

/// Scoped consumer interface: no File, raw handle, clone or path authority.
pub(in crate::setup) trait HeldReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> HeldReadSeek for T {}

/// Compose already-executed held-reader boundaries. Callers must perform every
/// guard and cursor restoration before passing the results here.
fn finish_held_read<T>(
    result: Result<T, String>,
    restored: Result<u64, String>,
    held: Result<(), String>,
    root: Result<(), String>,
    stopped: Result<(), String>,
) -> Result<T, String> {
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
            errors.push(format!("{stage}: {error}"));
        }
    }
    match result {
        Err(error) => {
            errors.insert(0, error);
            Err(errors.join("; "))
        }
        Ok(value) if errors.is_empty() => Ok(value),
        Ok(_) => Err(errors.join("; ")),
    }
}
