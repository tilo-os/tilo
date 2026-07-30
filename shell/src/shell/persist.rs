use crate::shell::state::WorkspaceTree;
use std::path::{Path, PathBuf};

/// Where the compositor persists its workspace tree.
///
/// `QOS_STATE_PATH` overrides it, and anything driving a compositor that
/// is not the user's own session should set it. `dirs::config_dir()`
/// reads `XDG_CONFIG_HOME` but **ignores a relative value**, so a
/// harness that isolates itself with a relative path gets no isolation
/// at all and writes into the real `~/.config/qos/state.json` — which is
/// how a demo's workspaces, owned by identities the person at the
/// machine cannot administer, ended up in a user's live session.
pub fn state_json_path() -> PathBuf {
    if let Ok(v) = std::env::var("QOS_STATE_PATH")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    dirs::config_dir()
        .map(|c| c.join("qos").join("state.json"))
        .unwrap_or_else(|| PathBuf::from("state.json"))
}

/// Where the compositor exports the capability list the FUSE mount
/// enforces, and where `qos-fs --acl` reads it from.
///
/// One path, named in one place, so the two processes cannot end up
/// enforcing different lists. `QOS_FS_ACL_PATH` overrides it for tests
/// and for a session running a mount somewhere unusual.
pub fn fs_capability_export_path() -> PathBuf {
    if let Ok(v) = std::env::var("QOS_FS_ACL_PATH")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    dirs::config_dir()
        .map(|c| c.join("qos").join("fs-capabilities.json"))
        .unwrap_or_else(|| PathBuf::from("fs-capabilities.json"))
}

/// Write the tree to `path`, atomically.
///
/// Capabilities whose expiry does not survive a restart
/// (`EndOfSession`, `AfterUse`) are dropped on the way out. That
/// filtering is the whole difference between "Always" and "Until I log
/// out": persisting a session grant would quietly promote it to a
/// permanent one, which is the worse failure of the two.
pub fn save_tree_to(tree: &WorkspaceTree, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut durable = tree.clone();
    for ws in &mut durable.workspaces {
        ws.capabilities.retain(|c| c.persists_across_restart());
    }
    let json = serde_json::to_string_pretty(&durable)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_file::write(path, json.as_bytes())
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
        Err(e) => {
            // A missing file is the first boot and expected. Anything
            // else means a state file exists and could not be read, and
            // the fallback replaces the person's workspaces with a
            // freshly bootstrapped demo tree. Doing that in silence is
            // indistinguishable from having lost their session, so say
            // so — one field rename in this struct is enough to trigger
            // it, and the file is left in place to be recovered.
            if path.exists() {
                tracing::error!(
                    path = %path.display(),
                    error = %e,
                    "state file exists but could not be read; starting from a \
                     default tree. The file has not been modified."
                );
            } else {
                tracing::info!(path = %path.display(), "no state file; first boot");
            }
            WorkspaceTree::bootstrap_default()
        }
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

    /// A state file written before workspaces had a room identity must
    /// gain one exactly once. Minting on every load would be worse than
    /// not minting at all: the workspace's room would change every
    /// boot, so every peer previously invited to it would be talking to
    /// a room that no longer exists.
    #[test]
    fn a_pre_rooms_state_file_gains_one_room_and_keeps_it() {
        use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        // Write a real state file, then delete the `room` key from every
        // workspace — which is byte-for-byte what the writer produced
        // before rooms existed. Deriving the fixture from the live
        // serialiser rather than hand-rolling the JSON means it stays a
        // *loadable* file as the struct grows; a hand-written one that
        // silently stopped parsing would send `load_or_default` down its
        // bootstrap fallback and the test would be asserting about a
        // brand-new tree instead of a migrated one.
        let mut original = WorkspaceTree::default();
        original
            .workspaces
            .push(Workspace::test_default(WorkspaceId(7), "Home"));
        save_tree_to(&original, &path).expect("save");
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read"))
                .expect("state file parses");
        for ws in raw["workspaces"].as_array_mut().expect("workspaces array") {
            assert!(
                ws.as_object_mut()
                    .expect("workspace object")
                    .remove("room")
                    .is_some(),
                "the current writer must emit a room, or this fixture is \
                 not removing anything"
            );
        }
        std::fs::write(&path, serde_json::to_string_pretty(&raw).expect("re-json"))
            .expect("write legacy state");

        let mut first_boot = load_or_default(&path);
        assert_eq!(
            first_boot.workspaces.len(),
            1,
            "the legacy state file must still load — a fallback to \
             `bootstrap_default` here would make the rest of this test \
             vacuous"
        );
        assert!(
            first_boot.workspaces[0].room.is_nil(),
            "a workspace deserialised from a pre-rooms file has no identity \
             yet; that is what the boot-time migration is for"
        );

        first_boot.persist_path = Some(path.clone());
        let minted = first_boot.adopt_roomless_workspaces();
        assert_eq!(
            minted.len(),
            1,
            "the migration must mint for the one roomless workspace"
        );
        let room = first_boot.workspaces[0].room;
        assert!(!room.is_nil(), "the minted room must be a real identity");

        // The mint and the write are one operation, so the *next* boot
        // reads the same value rather than minting again.
        let mut second_boot = load_or_default(&path);
        assert_eq!(
            second_boot.workspaces[0].room, room,
            "the room minted on first boot must have been written to disk, \
             so the second boot loads it instead of minting a new one"
        );
        second_boot.persist_path = Some(path.clone());
        assert!(
            second_boot.adopt_roomless_workspaces().is_empty(),
            "a second boot must find nothing to migrate"
        );
        assert_eq!(
            second_boot.workspaces[0].room, room,
            "and must leave the room it loaded alone"
        );
    }
}
