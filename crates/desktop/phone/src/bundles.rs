//! The tilolang apps the phone carries. `tilolang build --platform android` packs each app's
//! bundle into the APK's `assets/tilo/apps`, with an index (`files.txt`: a `version <hash>` line,
//! then each file's path). Android hands assets out only as streams it cannot list, so on the
//! first start of each build the phone copies them out under its data directory, by that version,
//! as it does its fonts; every later start reads them from there.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use tilolang_tilo::bundle::Bundle;

/// The index of the packed apps, beside them.
pub const INDEX: &str = "files.txt";

/// Every bundle under `dir`, one directory per app, by app id.
///
/// # Errors
/// `dir` cannot be read, or a bundle in it is damaged.
pub fn read(dir: &Path) -> Result<BTreeMap<String, Arc<Bundle>>, String> {
    let mut bundles = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if !path.is_dir() {
            continue;
        }
        let bundle = Bundle::read(&path).map_err(|e| e.to_string())?;
        bundles.insert(bundle.meta.app.clone(), Arc::new(bundle));
    }
    Ok(bundles)
}

/// The bundles the APK carries, copied out under `data` once per build.
///
/// # Errors
/// The APK holds no index or a file it names, or the copy cannot be written.
#[cfg(target_os = "android")]
pub fn installed(data: &Path) -> Result<BTreeMap<String, Arc<Bundle>>, String> {
    let versions = data.join("tilo/apps");
    let staging = versions.join(".staging");
    let _ = std::fs::remove_dir_all(&staging);
    let index_path = staging.join(INDEX);
    let asset = |name: &str| format!("tilo/apps/{name}");
    tilo_ui_base::platform::android::copy_asset(&asset(INDEX), &index_path)
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
            tilo_ui_base::platform::android::copy_asset(&asset(file), &staging.join(file))
                .map_err(|e| format!("{}: {e}", asset(file)))?;
        }
        // Earlier builds' copies go: only this build's apps run here.
        if let Ok(entries) = std::fs::read_dir(&versions) {
            for entry in entries.flatten() {
                if entry.path() != staging {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        std::fs::rename(&staging, &root).map_err(|e| format!("{}: {e}", root.display()))?;
    }
    read(&root)
}
