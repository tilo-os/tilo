//! The launcher environment an Android process is given; see
//! [`super::android`].

use std::path::{Path, PathBuf};

/// The base directories for an app whose `getFilesDir()` is `files`. `HOME`
/// is the sandbox root above it (`getDataDir()`), the boundary of what the app
/// may open; the cache Android clears under storage pressure is `cache` there.
pub(super) fn directories(files: &Path) -> [(&'static str, PathBuf); 7] {
    let root = files.parent().unwrap_or(files);
    let cache = root.join("cache");
    [
        ("HOME", root.to_path_buf()),
        ("XDG_DATA_HOME", files.join("data")),
        ("XDG_CONFIG_HOME", files.join("config")),
        ("XDG_STATE_HOME", files.join("state")),
        ("XDG_CACHE_HOME", cache.clone()),
        ("XDG_RUNTIME_DIR", cache.join("run")),
        ("TMPDIR", cache.join("tmp")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_environment_lives_in_the_app_sandbox() {
        let files = Path::new("/data/user/0/so.tilo.calculator/files");
        let vars = directories(files);
        let get = |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            get("HOME").as_deref(),
            Some(Path::new("/data/user/0/so.tilo.calculator"))
        );
        assert_eq!(get("XDG_DATA_HOME"), Some(files.join("data")));
        assert_eq!(get("XDG_CONFIG_HOME"), Some(files.join("config")));
        assert_eq!(get("XDG_STATE_HOME"), Some(files.join("state")));
        let cache = Path::new("/data/user/0/so.tilo.calculator/cache");
        assert_eq!(get("XDG_CACHE_HOME").as_deref(), Some(cache));
        assert_eq!(get("XDG_RUNTIME_DIR"), Some(cache.join("run")));
        assert_eq!(get("TMPDIR"), Some(cache.join("tmp")));
        assert!(
            vars.iter()
                .all(|(_, dir)| dir.starts_with("/data/user/0/so.tilo.calculator"))
        );
    }
}
