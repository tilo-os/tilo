use std::path::{Path, PathBuf};
use std::io::Write;
use crate::shell::state::WorkspaceTree;

pub fn state_json_path() -> PathBuf {
    dirs_next::config_dir()
        .map(|c| c.join("quick-desktop").join("state.json"))
        .unwrap_or_else(|| PathBuf::from("state.json"))
}

pub fn save_tree_to(tree: &WorkspaceTree, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(tree).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(json.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_tree_from(path: &Path) -> std::io::Result<WorkspaceTree> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str::<WorkspaceTree>(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn load_or_default(path: &Path) -> WorkspaceTree {
    match load_tree_from(path) {
        Ok(t) => t,
        Err(_) => WorkspaceTree::bootstrap_default(),
    }
}
