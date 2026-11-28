//! The signed tilolang packages the APK carries: copied out under this app's data once per build,
//! and installed into this device's store through the one installer, as any cached install is. A
//! phone is a device like any other: it runs the packages it verified, and a workspace it founds
//! runs the defaults it carries. Android hands assets out only as streams it cannot list, so the
//! build writes an index (`tilo/apps/files.txt`: a `version <hash>` line, then each file's path)
//! and the app copies them out by that version, as it does its fonts.

use std::path::Path;

/// The index of the carried packages, beside them.
pub const INDEX: &str = "files.txt";

/// The packages the APK carries, copied out under `data` once per build and installed into this
/// device's store. Answers each package the store refused, with why in words.
///
/// # Errors
/// The APK's index holds no version, or a file it names cannot be copied out.
#[cfg(target_os = "android")]
pub fn installed(data: &Path) -> Result<Vec<(String, String)>, String> {
    let versions = data.join("tilo/apps");
    let staging = versions.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    let index_path = staging.join(INDEX);
    let asset = |name: &str| format!("tilo/apps/{name}");
    ui::platform::android::copy_asset(&asset(INDEX), &index_path)
        .map_err(|e| format!("{}: {e}", asset(INDEX)))?;
    let index = std::fs::read_to_string(&index_path).map_err(|e| e.to_string())?;
    let mut lines = index.lines();
    let version = lines
        .next()
        .and_then(|line| line.strip_prefix("version "))
        .ok_or_else(|| format!("{}: no version line", asset(INDEX)))?;
    let root = versions.join(version);
    if !root.join(INDEX).is_file() {
        for file in lines.filter(|line| !line.is_empty()) {
            ui::platform::android::copy_asset(&asset(file), &staging.join(file))
                .map_err(|e| format!("{}: {e}", asset(file)))?;
        }
        // Earlier builds' copies go: only this build's packages are installed here.
        if let Ok(entries) = std::fs::read_dir(&versions) {
            for entry in entries.flatten() {
                if entry.path() != staging {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        std::fs::rename(&staging, &root).map_err(|e| format!("{}: {e}", root.display()))?;
    }
    Ok(tilo_runtime::device::packages::install_carried_apps(&root))
}

/// Nothing off Android: a build with no APK carries no package.
#[cfg(not(target_os = "android"))]
pub fn installed(_data: &Path) -> Result<Vec<(String, String)>, String> {
    Ok(Vec::new())
}
