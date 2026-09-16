//! Explicit browser data-root routing. Defaults preserve existing normal paths.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
static ROOT: OnceLock<PathBuf> = OnceLock::new();
pub fn configure(root: Option<&str>) -> Result<(), String> {
    let root = root
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("QOS_BROWSER_APP_ROOT").map(PathBuf::from));
    if let Some(root) = root {
        if !root.is_absolute() {
            return Err("Browser app root must be an absolute path".into());
        }
        if let Some(existing) = ROOT.get() {
            if existing != &root {
                return Err("Browser app root is already configured".into());
            }
        } else {
            let _ = ROOT.set(root);
        }
    }
    Ok(())
}
pub fn explicit_root() -> Option<&'static Path> {
    ROOT.get().map(PathBuf::as_path)
}
pub fn data_dir() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| dirs::data_dir().map(|p| p.join("qos-browser")))
}
pub fn config_dir() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| dirs::config_dir().map(|p| p.join("qos-browser")))
}
pub fn cache_dir() -> Option<PathBuf> {
    ROOT.get()
        .map(|p| p.join("cache"))
        .or_else(|| dirs::cache_dir().map(|p| p.join("qos-browser")))
}
