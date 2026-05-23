use std::path::{Path, PathBuf};
use std::io::Write;
use crate::shell::state::WorkspaceTree;

pub fn state_json_path() -> PathBuf {
    dirs::config_dir()
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
    let mut tree: WorkspaceTree = serde_json::from_str::<WorkspaceTree>(&s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // C-2: `WorkspaceTree::doc` is `#[serde(skip)]`, so deserialised
    // trees come back with an empty `AutoCommit`. Without re-seeding,
    // peer-sync's `export_changes_since(&[])` returns an empty vec
    // immediately after restart — the local user's existing
    // workspaces never propagate. Rebuild the doc from the cache so
    // subsequent sync rounds carry the real history.
    tree.seed_doc_from_cache();
    // D-9: the process-wide `next_id` counter starts at `1` on every
    // restart. Persisted ids can be much higher; without seeding the
    // next allocation collides with an existing entry and
    // `rebuild_cache_from_doc` folds two distinct rows into one.
    tree.seed_next_id_from_self();
    Ok(tree)
}

pub fn load_or_default(path: &Path) -> WorkspaceTree {
    match load_tree_from(path) {
        Ok(t) => t,
        Err(_) => WorkspaceTree::bootstrap_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-2 regression: `WorkspaceTree::doc` is `#[serde(skip)]`, so a
    /// JSON-only load leaves the automerge backing doc empty. Peer-sync
    /// then ships nothing on restart. `load_or_default` must seed the
    /// doc from the cache before returning so `current_heads()` reports
    /// the real history.
    #[test]
    fn load_or_default_seeds_doc_from_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        // Persist a non-empty bootstrap tree. `bootstrap_default`
        // already calls `seed_doc_from_cache`, so on disk the cache
        // alone carries the workspace data.
        let original = WorkspaceTree::bootstrap_default();
        save_tree_to(&original, &path).expect("save");

        // Round-trip via the production load path.
        let mut loaded = load_or_default(&path);

        // The cache must have round-tripped.
        assert!(
            !loaded.workspaces.is_empty(),
            "bootstrap tree must round-trip workspaces"
        );

        // And the doc must have been re-seeded so peer-sync can export
        // changes. Without the fix, `current_heads()` returns an empty
        // vec because the deserialised tree's `doc` is `AutoCommit::new()`.
        assert!(
            !loaded.current_heads().is_empty(),
            "load_or_default must seed the automerge doc so peer-sync \
             exports the persisted workspaces (C-2 regression)"
        );
    }
}
