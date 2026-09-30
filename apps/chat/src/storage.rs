//! Where Chat keeps its local, non-document files.

use std::path::PathBuf;

/// Resolve the data directory: `$TILO_CHAT_DATA` when set (the OS points chat
/// at a per-sandbox store, and tests pin a temp dir here to stay hermetic),
/// else `tilo-chat` in the platform's per-user data directory
/// ([`tilo_paths::data_dir`]). `None` only if none can be located (extreme
/// minimal containers).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_CHAT_DATA") {
        return Some(PathBuf::from(dir));
    }
    tilo_paths::AppDirs::new("tilo-chat").data()
}
