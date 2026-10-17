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

pub(super) mod coordinator;

/// Path of verified final component `index` for the elevated staging helper,
/// which opens and re-hashes it itself; the path grants no authority here. It
/// names the verified bytes only while a consumer holds the verified readers.
pub(in crate::setup) fn final_component_path(
    selected: &super::internal_download::SelectedManifest,
    index: usize,
) -> Result<std::path::PathBuf, String> {
    let component = selected
        .bundle()
        .component(index)
        .ok_or("Invalid cache component")?;
    #[cfg(any(target_os = "linux", windows))]
    {
        #[cfg(target_os = "linux")]
        let base = linux::cache_base()?;
        #[cfg(windows)]
        let base = windows::cache_base()?;
        Ok(base
            .join("tilo-internal-install")
            .join(selected.manifest_sha256())
            .join("payload")
            .join(component.filename))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = component;
        imager_backend::cache::unsupported()
    }
}

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
