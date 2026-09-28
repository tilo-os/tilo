//! Where Chat keeps its local, non-document files.

use std::path::PathBuf;

/// Resolve the data directory: `$TILO_CHAT_DATA` when set (the OS points chat
/// at a per-sandbox store, and tests pin a temp dir here to stay hermetic),
/// else `$XDG_DATA_HOME/tilo-chat`, falling back to `$HOME/.local/share/tilo-chat`.
/// `None` only if none can be located (extreme minimal containers).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_CHAT_DATA") {
        return Some(PathBuf::from(dir));
    }
    xdg_paths::data_dir()
        .or_else(|| xdg_paths::home().map(|h| h.join(".local/share")))
        .map(|d| d.join("tilo-chat"))
}
