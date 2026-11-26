//! Private checkpoint protocol and native cache providers. No caller or support route
//! is activated by this module.

mod checkpoint;
pub(super) use checkpoint::*;
#[cfg(test)]
pub(super) mod tests;

#[cfg(any(target_os = "linux", all(test, unix)))]
pub(super) mod linux;

#[cfg(any(windows, test))]
pub(super) mod windows;

pub(super) mod coordinator;

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

pub(in crate::setup) trait HeldReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> HeldReadSeek for T {}

fn finish_held_read<T>(
    result: Result<T, String>,
    restored: Result<u64, String>,
    held: Result<(), String>,
    root: Result<(), String>,
    stopped: Result<(), String>,
) -> Result<T, String> {
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
