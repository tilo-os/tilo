use crate::shell::state::{STATE_SCHEMA_VERSION, WorkspaceTree};
use std::path::{Path, PathBuf};

/// Why this session is not the one the state file describes.
///
/// The session still starts — refusing to boot a desktop because a file is
/// unexpected would be worse than starting one — but it starts from a default
/// tree, and the person has to be told, on screen, that the tree they are looking
/// at is not theirs and that theirs is still on the disk.
///
/// Journal-only was the previous answer and it is not one: the two states
/// "your workspaces are gone" and "your workspaces are on the disk and this
/// build declined to open them" look identical from the desk, and only one of
/// them is worth doing anything about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateNotice {
    /// The file was written by a newer qOS than this build understands.
    NewerSchema {
        path: PathBuf,
        found: u32,
        understood: u32,
    },
    /// The file exists and could not be read at all.
    Unreadable { path: PathBuf, error: String },
}

impl StateNotice {
    pub fn title(&self) -> String {
        match self {
            Self::NewerSchema { .. } => "Your workspaces were not opened".into(),
            Self::Unreadable { .. } => "Your workspaces could not be read".into(),
        }
    }

    /// What goes on the screen.
    ///
    /// Deliberately one short line. A toast is 320x48 and does not grow, so a
    /// paragraph in here is a paragraph spilling off the card and over the
    /// desktop — which is what the first version of this did. The claim that
    /// has to survive the trim is "your work is still there", because that is
    /// the one a person cannot infer from an empty desktop. Everything else —
    /// the path, the two version numbers, what to do next — is in
    /// [`StateNotice::detail`], which goes to the log and to the file left
    /// beside the state file.
    pub fn body(&self) -> String {
        match self {
            Self::NewerSchema { .. } => {
                "Saved by a newer version of qOS. Nothing was changed.".into()
            }
            Self::Unreadable { .. } => {
                "The file could not be read. Nothing was changed.".into()
            }
        }
    }

    /// The whole story, for a log, a console, or the file left on disk.
    pub fn detail(&self) -> String {
        match self {
            Self::NewerSchema {
                path,
                found,
                understood,
            } => format!(
                "{} was written by a newer version of qOS (layout {found}; this \
                 one understands {understood}). It has not been changed, and \
                 nothing has been written over it — this session runs without \
                 saving, so it cannot be. Update again, or roll forward, to get \
                 back to your own workspaces.",
                path.display()
            ),
            Self::Unreadable { path, error } => format!(
                "{} exists but could not be read ({error}). It has not been \
                 changed, and this session runs without saving, so it cannot \
                 be.",
                path.display()
            ),
        }
    }

    /// The whole message on one line, for a log or a console.
    pub fn one_line(&self) -> String {
        format!("{}: {}", self.title(), self.detail())
    }
}

/// Where the explanation is left for anyone who was not looking at the screen.
///
/// A notification needs a session and someone in front of it. This is the copy
/// repair mode, a support conversation, or the person's next login can find, and
/// it sits beside the file it is about so the two cannot be separated.
fn notice_path(state: &Path) -> PathBuf {
    let mut s = state.as_os_str().to_os_string();
    s.push(".not-opened");
    PathBuf::from(s)
}

fn leave_notice_beside_the_file(state: &Path, notice: &StateNotice) {
    let path = notice_path(state);
    let body = format!(
        "{}\n\n{}\n\nWritten by qOS when it declined to open the file next to \
         this one. Delete this file once the situation is resolved; nothing \
         reads it.\n",
        notice.title(),
        notice.detail()
    );
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(path = %path.display(), error = %e, "could not leave the explanation beside the state file");
    }
}

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
    // Refuse before anything else touches it. A newer file may use a field this
    // build reads with a different meaning, and the mutators here write the
    // whole tree back — so "load it and see" is "overwrite it and see".
    if tree.schema_version > STATE_SCHEMA_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "state file layout {} is newer than the {STATE_SCHEMA_VERSION} \
                 this build understands",
                tree.schema_version
            ),
        ));
    }
    // A file that predates the key is version 1 by construction (the serde
    // default), and one this build wrote says so. Normalise either way, so the
    // next write stamps the current version rather than carrying an old one
    // forward for ever.
    tree.schema_version = STATE_SCHEMA_VERSION;
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

/// Load the tree, or start a fresh one and say why.
///
/// The second half of the pair is the whole point. A missing file is the first
/// boot and carries no notice. Anything else means a state file exists and could
/// not be read, and the fallback replaces the person's workspaces with a freshly
/// bootstrapped demo tree — which is indistinguishable from having lost their
/// session. The caller is expected to put [`StateNotice`] somewhere the person
/// will actually see it; the journal is not that place.
///
/// The file is never modified on this path, and a copy of the explanation is left
/// beside it.
pub fn load_or_default(path: &Path) -> (WorkspaceTree, Option<StateNotice>) {
    match load_tree_from(path) {
        Ok(t) => (t, None),
        Err(e) => {
            if !path.exists() {
                tracing::info!(path = %path.display(), "no state file; first boot");
                return (WorkspaceTree::bootstrap_default(), None);
            }
            // Distinguish the two, because they need different things from the
            // person: a newer layout means their data is intact and this build
            // is the wrong one, while an unreadable file may be damage.
            let notice = match schema_version_in(path) {
                Some(found) if found > STATE_SCHEMA_VERSION => StateNotice::NewerSchema {
                    path: path.to_path_buf(),
                    found,
                    understood: STATE_SCHEMA_VERSION,
                },
                _ => StateNotice::Unreadable {
                    path: path.to_path_buf(),
                    error: e.to_string(),
                },
            };
            tracing::error!(
                path = %path.display(),
                error = %e,
                "{}",
                notice.one_line()
            );
            leave_notice_beside_the_file(path, &notice);
            (WorkspaceTree::bootstrap_default(), Some(notice))
        }
    }
}

/// Read just the `schema_version` out of a file the full parse rejected.
///
/// Parsed separately and tolerantly: the reason the full parse failed may be a
/// field this build has never heard of, and the version is exactly the thing that
/// explains that. Reporting "unreadable" for a file that says plainly which
/// layout it is would be throwing away the only useful part of it.
fn schema_version_in(path: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("schema_version")?.as_u64().map(|n| n as u32)
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

        // Persist a non-empty bootstrap tree. `bootstrap_demo`
        // already calls `seed_doc_from_cache`, so on disk the cache
        // alone carries the workspace data.
        let original = WorkspaceTree::bootstrap_demo();
        save_tree_to(&original, &path).expect("save");

        // Round-trip via the production load path.
        let (mut loaded, notice) = load_or_default(&path);
        assert!(
            notice.is_none(),
            "a file this build just wrote must load without a notice"
        );

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

        let (mut first_boot, _) = load_or_default(&path);
        // Identify the tree by the id the fixture wrote, not by its shape.
        // `bootstrap_default` is also a single workspace, so a length check
        // no longer tells a loaded file apart from the bootstrap fallback —
        // and a silent fallback would leave the rest of this test asserting
        // about a brand-new tree instead of a migrated one. The id is minted
        // fresh by the fallback and can never be 7.
        assert_eq!(
            first_boot
                .workspaces
                .iter()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            vec![WorkspaceId(7)],
            "the legacy state file must still load; got the bootstrap \
             fallback or something else entirely"
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
        let (mut second_boot, _) = load_or_default(&path);
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

    /// Every state file this build writes says which layout it is. Without
    /// this the version check has nothing to read on the next boot, and the
    /// refusal below can only ever fire for a hand-edited file.
    #[test]
    fn a_written_state_file_stamps_its_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        save_tree_to(&WorkspaceTree::bootstrap_demo(), &path).expect("save");

        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(
            raw.get("schema_version").and_then(|v| v.as_u64()),
            Some(u64::from(STATE_SCHEMA_VERSION)),
            "the writer must stamp the current schema version"
        );
    }

    /// The one this whole thing exists for: an older binary meets a file a
    /// newer one wrote.
    ///
    /// It must refuse, leave the file byte-for-byte alone, and hand the
    /// caller something to show the person. The old behaviour — hand them a
    /// bootstrapped demo tree and log a line — is indistinguishable from
    /// having lost their session.
    #[test]
    fn a_newer_state_file_is_refused_preserved_and_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        // Derived from the live writer, so it stays a *loadable* file as the
        // struct grows. A hand-written fixture that silently stopped parsing
        // would take the "unreadable" branch and this test would be asserting
        // about the wrong refusal.
        let mut original = WorkspaceTree::default();
        original
            .workspaces
            .push(crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(7),
                "Home",
            ));
        save_tree_to(&original, &path).expect("save");
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let future = STATE_SCHEMA_VERSION + 1;
        raw["schema_version"] = serde_json::json!(future);
        let on_disk = serde_json::to_string_pretty(&raw).expect("re-json");
        std::fs::write(&path, &on_disk).expect("write future state");

        let (tree, notice) = load_or_default(&path);

        // Refused: this is a bootstrapped tree, not the one on disk.
        assert!(
            !tree.workspaces.iter().any(|w| w.id.0 == 7),
            "a file from a newer layout must NOT be loaded; workspace 7 came \
             from it and must not be in the session"
        );

        // Reported, with the facts that make it actionable.
        let notice = notice.expect(
            "a refused state file must produce a notice — silently handing the \
             person a bootstrapped tree is the behaviour this replaces",
        );
        match &notice {
            StateNotice::NewerSchema {
                found, understood, ..
            } => {
                assert_eq!(*found, future);
                assert_eq!(*understood, STATE_SCHEMA_VERSION);
            }
            other => panic!(
                "expected NewerSchema so the person is told their data is \
                 intact and this build is the wrong one, got {other:?}"
            ),
        }
        assert!(
            notice.detail().contains(&path.display().to_string()),
            "the detail must name the file, or there is nothing to act on"
        );
        // The on-screen line is short on purpose — a toast does not grow — but
        // it still has to carry the claim a person cannot infer from an empty
        // desktop.
        assert!(
            notice.body().len() < 80,
            "the on-screen body must fit a 320x48 toast; it was {} chars: {:?}",
            notice.body().len(),
            notice.body()
        );
        assert!(
            notice.body().to_lowercase().contains("nothing was changed"),
            "the screen has to say the file is intact, got: {:?}",
            notice.body()
        );

        // Preserved, byte for byte.
        assert_eq!(
            std::fs::read_to_string(&path).expect("re-read"),
            on_disk,
            "the state file must not have been touched"
        );

        // And the explanation is on the disk too, for anyone who was not
        // watching the screen when the session came up.
        let beside = notice_path(&path);
        let left = std::fs::read_to_string(&beside)
            .unwrap_or_else(|e| panic!("no explanation at {}: {e}", beside.display()));
        assert!(
            left.contains(&future.to_string()) && left.contains("not"),
            "the file left beside it must say what happened, got: {left}"
        );
    }

    /// The complement, and the reason the default is 1 rather than 0: every
    /// state file written before the key existed must still open. Getting
    /// this wrong would condemn every existing session on the upgrade that
    /// introduced the check — the exact failure the check is meant to prevent,
    /// caused by the check.
    #[test]
    fn a_state_file_written_before_versioning_still_opens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        let mut original = WorkspaceTree::default();
        original
            .workspaces
            .push(crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(9),
                "Home",
            ));
        save_tree_to(&original, &path).expect("save");
        let mut raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert!(
            raw.as_object_mut()
                .expect("object")
                .remove("schema_version")
                .is_some(),
            "the current writer must emit schema_version, or this fixture \
             removes nothing and the test is vacuous"
        );
        std::fs::write(&path, serde_json::to_string_pretty(&raw).expect("re-json"))
            .expect("write pre-versioning state");

        let (tree, notice) = load_or_default(&path);
        assert!(notice.is_none(), "a pre-versioning file must not be refused");
        assert!(
            tree.workspaces.iter().any(|w| w.id.0 == 9),
            "the workspaces in a pre-versioning file must still be loaded"
        );
        assert_eq!(
            tree.schema_version, STATE_SCHEMA_VERSION,
            "and the loaded tree must carry the current version, so the next \
             write stamps it rather than leaving the file unversioned for ever"
        );
    }
}
