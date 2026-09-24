use crate::shell::state::{STATE_SCHEMA_VERSION, WorkspaceTree};
use std::path::{Path, PathBuf};

/// Why this session is not the one the state file describes. The
/// session still starts, from a default tree, but the person has to be
/// told on screen that it isn't theirs and theirs is still on disk —
/// journal-only would look identical to "your workspaces are gone".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateNotice {
    /// The file was written by a newer Tilo than this build understands.
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

    /// One short line — a toast is 320x48 and does not grow. The claim
    /// that must survive the trim is "your work is still there".
    pub fn body(&self) -> String {
        match self {
            Self::NewerSchema { .. } => {
                "Saved by a newer version of Tilo. Nothing was changed.".into()
            }
            Self::Unreadable { .. } => "The file could not be read. Nothing was changed.".into(),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::NewerSchema {
                path,
                found,
                understood,
            } => format!(
                "{} was written by a newer version of Tilo (layout {found}; this \
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

    pub fn one_line(&self) -> String {
        format!("{}: {}", self.title(), self.detail())
    }
}

/// Beside the file it's about, for anyone not looking at the screen
/// when it happened — repair mode, support, the next login.
fn notice_path(state: &Path) -> PathBuf {
    let mut s = state.as_os_str().to_os_string();
    s.push(".not-opened");
    PathBuf::from(s)
}

fn leave_notice_beside_the_file(state: &Path, notice: &StateNotice) {
    let path = notice_path(state);
    let body = format!(
        "{}\n\n{}\n\nWritten by Tilo when it declined to open the file next to \
         this one. Delete this file once the situation is resolved; nothing \
         reads it.\n",
        notice.title(),
        notice.detail()
    );
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(path = %path.display(), error = %e, "could not leave the explanation beside the state file");
    }
}

/// `TILO_STATE_PATH` overrides it. Anything driving a compositor that
/// isn't the user's own session must set it — `dirs::config_dir()`
/// **ignores a relative `XDG_CONFIG_HOME`**, so a harness isolating
/// itself with a relative path writes into the user's
/// `~/.config/tilo/state.json` instead.
pub fn state_json_path() -> PathBuf {
    if let Ok(v) = std::env::var("TILO_STATE_PATH")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    dirs::config_dir().map_or_else(
        || PathBuf::from("state.json"),
        |c| c.join("tilo").join("state.json"),
    )
}

/// One path, named once, so the compositor and `tilo-fs --acl` cannot
/// end up enforcing different lists. `TILO_FS_ACL_PATH` overrides it for tests.
pub fn fs_capability_export_path() -> PathBuf {
    if let Ok(v) = std::env::var("TILO_FS_ACL_PATH")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    dirs::config_dir().map_or_else(
        || PathBuf::from("fs-capabilities.json"),
        |c| c.join("tilo").join("fs-capabilities.json"),
    )
}

/// Atomic. Capabilities whose expiry doesn't survive a restart
/// (`EndOfSession`, `AfterUse`) are dropped, or persisting a session
/// grant would quietly promote it to a permanent one.
///
/// # Errors
///
/// Returns an error if the parent directory can't be created, `tree`
/// can't be serialised, or the atomic write fails.
pub fn save_tree_to(tree: &WorkspaceTree, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut durable = tree.clone();
    // A Wayland surface id belongs to this compositor process. Preserve the
    // user's open-app topology as symbolic local launch records, never as
    // dead protocol ids; startup rebinds the replacement surface into the
    // same tab.
    symbolise_foreign_surfaces(&mut durable);
    clear_transient_launch_notes(&mut durable);
    for ws in &mut durable.workspaces {
        ws.capabilities
            .retain(ui::CapOverride::persists_across_restart);
    }
    let json = serde_json::to_string_pretty(&durable)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_file::write(path, json.as_bytes())
}

/// # Errors
///
/// Returns an error if `path` can't be read, its contents don't parse as
/// a [`WorkspaceTree`], or its `schema_version` is newer than
/// [`STATE_SCHEMA_VERSION`].
pub fn load_tree_from(path: &Path) -> std::io::Result<WorkspaceTree> {
    let s = std::fs::read_to_string(path)?;
    let mut tree: WorkspaceTree = serde_json::from_str::<WorkspaceTree>(&s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Refuse before anything else touches it: mutators here write the
    // whole tree back, so "load it and see" is "overwrite it and see".
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
    // Normalise so the next write stamps the current version rather
    // than carrying an old one forward for ever.
    tree.schema_version = STATE_SCHEMA_VERSION;
    // An early production bootstrap accidentally used the screenshot/demo
    // fixture. One of its own two-session demonstrations then renamed that
    // synthetic Home room to `Shared room <unix time>`, leaving the invented
    // Notes/Files/Terminal/Browser layout to be restored on every login. This
    // signature is deliberately much stricter than a name or tab-count check:
    // any genuine furniture edit makes it a near miss and preserves it.
    if is_legacy_demo_bootstrap(&tree) {
        tracing::warn!(
            path = %path.display(),
            "replacing the legacy synthetic demo workspace with an empty desktop"
        );
        tree = WorkspaceTree::bootstrap_default();
        // Make the migration one-shot and crash-safe. A second restart reads
        // the already-empty replacement rather than making the decision again.
        save_tree_to(&tree, path)?;
    }
    // Repair state written by older builds which serialised process-local
    // Wayland surface ids.  This must happen before seeding the room document,
    // otherwise the dead window is immediately promoted back into the
    // authoritative topology.
    repair_legacy_foreign_surfaces(&mut tree);
    // `Opening here…` describes work owned by the compositor process that
    // wrote this file.  After a restart there is no corresponding launcher
    // worker or pending placement left to complete it, so treating it as an
    // answered symbolic window makes the card permanent and the application
    // is never retried.  Older builds did persist this marker; normalise it
    // before seeding the document so both local restore and peer projection
    // see the retryable state.
    clear_transient_launch_notes(&mut tree);
    // `doc` is `#[serde(skip)]`; without re-seeding, peer-sync exports
    // nothing and existing workspaces never propagate after restart.
    tree.seed_doc_from_cache();
    // The `next_id` counter restarts at 1; without seeding, the next
    // allocation collides with a persisted id.
    tree.seed_next_id_from_self();
    Ok(tree)
}

/// Exact structural signature of the two known demo bootstraps that escaped
/// into user state. IDs are checked as one contiguous allocation sequence;
/// names, furniture, active pointers and every user-content-bearing side map
/// must also match. Governance is intentionally ignored because the demo's
/// join/adoption flow itself changes owners, capabilities and room authority.
#[allow(
    clippy::too_many_lines,
    reason = "the exact legacy-bootstrap signature is intentionally kept as one auditable predicate so migration cannot match a partial user workspace"
)]
fn is_legacy_demo_bootstrap(tree: &WorkspaceTree) -> bool {
    use crate::shell::state::{AppKind, TileContent, WindowWidth};

    let [home, research, design] = tree.workspaces.as_slice() else {
        return false;
    };
    let shared_demo_name = home
        .name
        .strip_prefix("Shared room ")
        .is_some_and(|suffix| {
            suffix.len() == 10 && suffix.bytes().all(|byte| byte.is_ascii_digit())
        });
    if home.name != "Home" && !shared_demo_name {
        return false;
    }
    if research.name != "Research" || design.name != "Design" {
        return false;
    }

    let base = home.id.raw();
    let id = |offset| base.checked_add(offset);
    let (
        Some(home_strip_id),
        Some(group_id),
        Some(notes_id),
        Some(files_id),
        Some(terminal_id),
        Some(fourth_id),
        Some(research_id),
        Some(research_strip_id),
        Some(design_id),
        Some(design_strip_id),
    ) = (
        id(1),
        id(2),
        id(3),
        id(4),
        id(5),
        id(6),
        id(7),
        id(8),
        id(9),
        id(10),
    )
    else {
        return false;
    };
    let ([home_strip], [research_strip], [design_strip]) = (
        home.strips.as_slice(),
        research.strips.as_slice(),
        design.strips.as_slice(),
    ) else {
        return false;
    };
    let [group] = home_strip.window_groups.as_slice() else {
        return false;
    };
    if !research_strip.window_groups.is_empty()
        || !design_strip.window_groups.is_empty()
        || home_strip.name != "Main"
        || research_strip.name != "Main"
        || design_strip.name != "Main"
        || home_strip.id.raw() != home_strip_id
        || group.id != group_id
        || research.id.raw() != research_id
        || research_strip.id.raw() != research_strip_id
        || design.id.raw() != design_id
        || design_strip.id.raw() != design_strip_id
        || tree.active != home.id
        || tree.active_strip.len() != 3
        || tree.active_strip.get(&home.id) != Some(&home_strip.id)
        || tree.active_strip.get(&research.id) != Some(&research_strip.id)
        || tree.active_strip.get(&design.id) != Some(&design_strip.id)
        || tree.active_group.len() != 1
        || tree.active_group.get(&home_strip.id) != Some(&group.id)
        || group.active_tab != notes_id
        || group.width != WindowWidth::Wide
        || group.maximized
        || home
            .strips
            .iter()
            .chain(&research.strips)
            .chain(&design.strips)
            .any(|strip| strip.name != "Main")
        || tree
            .workspaces
            .iter()
            .any(|workspace| !workspace.tasks.is_empty() || !workspace.shared_surfaces.is_empty())
        || !tree.room_peer_names.is_empty()
        || !tree.room_peer_identities.is_empty()
        || !tree.room_share_floor.is_empty()
        || !tree.continuation_tabs.is_empty()
        || !tree.continuation_rooms.is_empty()
        || !tree.continuation_owned_tabs.is_empty()
        || !tree.continuation_closed_tabs.is_empty()
        || !tree.continuation_file_references.is_empty()
        || !tree.native_document_tabs.is_empty()
    {
        return false;
    }

    let [notes, files, terminal, fourth] = group.tabs.as_slice() else {
        return false;
    };
    if [notes.id, files.id, terminal.id, fourth.id] != [notes_id, files_id, terminal_id, fourth_id]
        || notes.title != "Notes"
        || files.title != "Files"
        || terminal.title != "Terminal"
        || !matches!(
            &notes.tile,
            TileContent::AppPlaceholder { kind: AppKind::Other(kind), label }
                if kind == "Notes" && label == "Notes"
        )
        || !matches!(
            &files.tile,
            TileContent::AppPlaceholder { kind: AppKind::FileExplorer, label }
                if label == "Files"
        )
        || !matches!(
            &terminal.tile,
            TileContent::AppPlaceholder { kind: AppKind::Terminal, label }
                if label == "Terminal"
        )
    {
        return false;
    }

    // Browser was the original fourth fake tile; a later demo revision
    // replaced it with the in-process Document placeholder. Both are wholly
    // synthetic and otherwise share the exact allocation signature above.
    (fourth.title == "Browser"
        && matches!(
            &fourth.tile,
            TileContent::Stub { kind: AppKind::Browser, label } if label == "Browser"
        ))
        || (fourth.title == "Document"
            && matches!(
                &fourth.tile,
                TileContent::AppPlaceholder { kind: AppKind::Other(kind), label }
                    if kind == "Word" && label == "Document"
            ))
}

/// Remove live compositor-client bindings from a durable tree and repair the
/// focus/index structures that referred to them.
///
/// `PeerWindow` and `AppPlaceholder` are intentionally retained: unlike a
/// `Foreign` tile they are symbolic records with no process-local object id,
/// and are the explicit continuation contract.
fn symbolise_foreign_surfaces(tree: &mut WorkspaceTree) {
    use crate::shell::state::TileContent;

    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    if let TileContent::Foreign { app_id, .. } = &tab.tile {
                        tab.tile = TileContent::LocalApp {
                            app_id: app_id.clone(),
                            label: tab.title.clone(),
                            note: String::new(),
                        };
                    }
                }
            }
        }
    }
}

/// Remove process-local launch progress from a durable workspace snapshot.
///
/// Refusal notes (not installed, permission denied, and so on) are outcomes
/// and remain useful after restart.  `OPENING` is different: it is backed by
/// an in-memory launcher worker and pending-placement entry, neither of which
/// survives this process.  Persisting it would turn an interrupted launch
/// into a permanently answered window that [`peer_windows::pending`] skips.
fn clear_transient_launch_notes(tree: &mut WorkspaceTree) {
    use crate::shell::state::TileContent;

    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    match &mut tab.tile {
                        TileContent::LocalApp { note, .. }
                        | TileContent::PeerWindow { note, .. }
                            if note == crate::shell::peer_windows::OPENING =>
                        {
                            note.clear();
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Old builds wrote raw foreign ids before the save-side sanitizer existed.
/// Those records cannot be safely launched because their presence on disk is
/// ambiguous, so retain the defensive deletion only for legacy input.
fn repair_legacy_foreign_surfaces(tree: &mut WorkspaceTree) {
    use crate::shell::state::TileContent;

    let mut removed_tabs = std::collections::HashSet::new();
    let mut removed_surfaces = std::collections::HashSet::new();

    for workspace in &mut tree.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                group.tabs.retain(|tab| {
                    if let TileContent::Foreign { surface_id, .. } = &tab.tile {
                        removed_tabs.insert(tab.id);
                        removed_surfaces.insert(*surface_id);
                        false
                    } else {
                        true
                    }
                });
                if !group.tabs.iter().any(|tab| tab.id == group.active_tab) {
                    group.active_tab = group.tabs.first().map_or(0, |tab| tab.id);
                }
            }
            strip.window_groups.retain(|group| !group.tabs.is_empty());
        }
        for surface in &removed_surfaces {
            workspace.shared_surfaces.unshare(*surface);
        }
        for collaborator in &mut workspace.collaborators {
            if collaborator
                .active_tab
                .is_some_and(|tab| removed_tabs.contains(&tab))
            {
                collaborator.active_tab = None;
            }
        }
    }

    let valid_active_groups: std::collections::HashSet<_> = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| {
            strip
                .window_groups
                .iter()
                .map(move |group| (strip.id, group.id))
        })
        .collect();
    tree.active_group
        .retain(|strip_id, group_id| valid_active_groups.contains(&(*strip_id, *group_id)));
    for workspace in &tree.desktop.workspaces {
        for strip in &workspace.strips {
            if !tree.desktop.active_group.contains_key(&strip.id)
                && let Some(group) = strip.window_groups.first()
            {
                tree.desktop.active_group.insert(strip.id, group.id);
            }
        }
    }
    tree.continuation_tabs
        .retain(|tab| !removed_tabs.contains(tab));
    tree.continuation_owned_tabs
        .retain(|tab, _| !removed_tabs.contains(tab));
    tree.continuation_file_references
        .retain(|tab, _| !removed_tabs.contains(tab));
}

/// A missing file is the first boot and carries no notice; anything
/// else means an unreadable file exists, and the demo-tree fallback is
/// indistinguishable from a lost session unless [`StateNotice`] reaches
/// the person. Never modifies the file; a copy of the explanation is
/// left beside it.
pub fn load_or_default(path: &Path) -> (WorkspaceTree, Option<StateNotice>) {
    match load_tree_from(path) {
        Ok(t) => (t, None),
        Err(e) => {
            if !path.exists() {
                tracing::info!(path = %path.display(), "no state file; first boot");
                return (WorkspaceTree::bootstrap_default(), None);
            }
            // A newer layout means the data is intact and this build is
            // the wrong one; an unreadable file may be damage.
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

/// Parsed separately and tolerantly, since the full parse may have
/// failed on a field this build has never heard of.
fn schema_version_in(path: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("schema_version")?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escaped_demo_fixture() -> WorkspaceTree {
        use crate::shell::state::{AppKind, TileContent};

        let mut tree = WorkspaceTree::bootstrap_demo();
        let base = 12_u64;
        let home = &mut tree.workspaces[0];
        home.id = crate::shell::state::WorkspaceId(base);
        home.name = "Shared room 1786892560".into();
        home.strips[0].id = crate::shell::state::StripId(base + 1);
        home.strips[0].window_groups[0].id = base + 2;
        let group = &mut home.strips[0].window_groups[0];
        for (offset, tab) in (3_u64..=6).zip(&mut group.tabs) {
            tab.id = base.saturating_add(offset);
        }
        group.active_tab = base.saturating_add(3);
        group.tabs[3].title = "Browser".into();
        group.tabs[3].tile = TileContent::Stub {
            kind: AppKind::Browser,
            label: "Browser".into(),
        };
        tree.workspaces[1].id = crate::shell::state::WorkspaceId(base + 7);
        tree.workspaces[1].strips[0].id = crate::shell::state::StripId(base + 8);
        tree.workspaces[2].id = crate::shell::state::WorkspaceId(base + 9);
        tree.workspaces[2].strips[0].id = crate::shell::state::StripId(base + 10);
        tree.active = crate::shell::state::WorkspaceId(base);
        tree.active_strip = [
            (
                crate::shell::state::WorkspaceId(base),
                crate::shell::state::StripId(base + 1),
            ),
            (
                crate::shell::state::WorkspaceId(base + 7),
                crate::shell::state::StripId(base + 8),
            ),
            (
                crate::shell::state::WorkspaceId(base + 9),
                crate::shell::state::StripId(base + 10),
            ),
        ]
        .into_iter()
        .collect();
        tree.active_group = [(crate::shell::state::StripId(base + 1), base + 2)]
            .into_iter()
            .collect();
        // The leaked collaboration demo adopted/changed governance after
        // creating the furniture. Those mutations are evidence of the demo,
        // not user-created windows, and must not prevent cleanup.
        tree.workspaces[0].owner = Some("alice".into());
        let _ = tree.adopt_ownerless_workspaces(&"quinten".to_string());
        tree
    }

    fn assert_demo_near_miss_is_preserved(tree: &WorkspaceTree, label: &str) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        save_tree_to(tree, &path).expect("save near miss");
        let loaded = load_tree_from(&path).expect("load near miss");
        assert_eq!(
            loaded.workspaces.len(),
            3,
            "a user-modified demo-shaped layout must be preserved ({label})"
        );
    }

    #[test]
    fn escaped_demo_bootstrap_migrates_once_to_an_empty_desktop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let fixture = escaped_demo_fixture();
        assert!(fixture.workspaces[0].owner.is_some());
        assert!(fixture.workspaces[1].owner.is_some());
        assert!(!fixture.workspaces[1].capabilities.is_empty());
        assert!(
            is_legacy_demo_bootstrap(&fixture),
            "automatic collaboration governance changes remain part of the escaped demo signature"
        );
        save_tree_to(&fixture, &path).expect("save escaped fixture");

        let first = load_tree_from(&path).expect("migrate escaped fixture");
        assert_eq!(first.workspaces.len(), 1);
        assert_eq!(first.workspaces[0].name, "Home");
        assert!(first.workspaces[0].strips[0].window_groups.is_empty());
        let replacement = std::fs::read(&path).expect("read atomic replacement");

        let second = load_tree_from(&path).expect("load replacement again");
        assert_eq!(second.workspaces.len(), 1);
        assert!(second.workspaces[0].strips[0].window_groups.is_empty());
        assert_eq!(
            std::fs::read(&path).expect("read after second restart"),
            replacement,
            "the migration must be one-shot; a later restart does not rewrite it"
        );
    }

    #[test]
    fn demo_migration_preserves_every_near_miss_user_layout() {
        let mut renamed = escaped_demo_fixture();
        renamed.workspaces[0].name = "My actual room".into();
        assert_demo_near_miss_is_preserved(&renamed, "renamed room");

        let mut retitled = escaped_demo_fixture();
        retitled.workspaces[0].strips[0].window_groups[0].tabs[0].title = "My notes".into();
        assert_demo_near_miss_is_preserved(&retitled, "retitled tab");

        let mut reordered = escaped_demo_fixture();
        reordered.workspaces[0].strips[0].window_groups[0]
            .tabs
            .swap(0, 1);
        assert_demo_near_miss_is_preserved(&reordered, "reordered tabs");

        let mut resized = escaped_demo_fixture();
        resized.workspaces[0].strips[0].window_groups[0].width =
            crate::shell::state::WindowWidth::Normal;
        assert_demo_near_miss_is_preserved(&resized, "resized group");

        let mut continued = escaped_demo_fixture();
        continued.continuation_tabs.insert(15);
        assert_demo_near_miss_is_preserved(&continued, "continuation state");

        let mut non_contiguous = escaped_demo_fixture();
        non_contiguous.workspaces[2].strips[0].id = crate::shell::state::StripId(99_999);
        non_contiguous.desktop.active_strip.insert(
            non_contiguous.desktop.workspaces[2].id,
            crate::shell::state::StripId(99_999),
        );
        assert_demo_near_miss_is_preserved(&non_contiguous, "non-demo id allocation");
    }

    fn foreign_tabs(tree: &WorkspaceTree) -> Vec<(u64, u64)> {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match &tab.tile {
                crate::shell::state::TileContent::Foreign { surface_id, .. } => {
                    Some((tab.id, *surface_id))
                }
                _ => None,
            })
            .collect()
    }

    fn local_apps(tree: &WorkspaceTree) -> Vec<(u64, String)> {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match &tab.tile {
                crate::shell::state::TileContent::LocalApp { app_id, .. } => {
                    Some((tab.id, app_id.clone()))
                }
                _ => None,
            })
            .collect()
    }

    fn symbolic_note(tree: &WorkspaceTree, tab_id: u64) -> Option<&str> {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find_map(|tab| {
                (tab.id == tab_id).then(|| match &tab.tile {
                    crate::shell::state::TileContent::LocalApp { note, .. }
                    | crate::shell::state::TileContent::PeerWindow { note, .. } => note.as_str(),
                    other => panic!("expected symbolic app, got {other:?}"),
                })
            })
    }

    #[test]
    fn save_never_durably_records_process_local_opening_progress() {
        use crate::shell::state::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.workspaces[0].strips = vec![Strip {
            id: StripId::from(91_100),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: 91_099,
                tabs: Vec::new(),
                active_tab: 0,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        group.tabs = vec![
            Tab {
                id: 91_101,
                title: "XTerm".into(),
                tile: TileContent::LocalApp {
                    app_id: "XTerm".into(),
                    label: "XTerm".into(),
                    note: crate::shell::peer_windows::OPENING.into(),
                },
            },
            Tab {
                id: 91_102,
                title: "Missing".into(),
                tile: TileContent::PeerWindow {
                    app_id: "org.example.Missing".into(),
                    label: "Missing".into(),
                    note: "Not installed on this machine".into(),
                },
            },
        ];
        group.active_tab = 91_101;

        save_tree_to(&tree, &path).expect("save");
        let on_disk: WorkspaceTree =
            serde_json::from_slice(&std::fs::read(&path).expect("read durable state"))
                .expect("decode durable state");
        assert_eq!(symbolic_note(&on_disk, 91_101), Some(""));
        assert_eq!(
            symbolic_note(&on_disk, 91_102),
            Some("Not installed on this machine"),
            "a settled refusal remains an answered window after restart"
        );
    }

    #[test]
    fn load_migrates_stale_opening_progress_back_to_a_retryable_window() {
        use crate::shell::state::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut legacy = WorkspaceTree::bootstrap_default();
        legacy.workspaces[0].strips = vec![Strip {
            id: StripId::from(91_104),
            name: "Main".into(),
            window_groups: vec![WindowGroup {
                id: 91_105,
                tabs: Vec::new(),
                active_tab: 0,
                width: WindowWidth::Normal,
                maximized: false,
            }],
        }];
        let group = &mut legacy.workspaces[0].strips[0].window_groups[0];
        group.tabs = vec![Tab {
            id: 91_103,
            title: "XTerm".into(),
            tile: TileContent::LocalApp {
                app_id: "XTerm".into(),
                label: "XTerm".into(),
                note: crate::shell::peer_windows::OPENING.into(),
            },
        }];
        group.active_tab = 91_103;
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy).expect("encode"))
            .expect("write legacy state");

        let loaded = load_tree_from(&path).expect("load old in-flight state");
        assert_eq!(symbolic_note(&loaded, 91_103), Some(""));
        assert!(
            crate::shell::peer_windows::any_pending(&loaded),
            "an interrupted launch must be attempted by the new session"
        );
    }

    #[test]
    fn live_foreign_surface_round_trip_preserves_symbolic_app_and_exact_placement() {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let workspace_id = tree.workspaces[0].id;
        let strip_id = tree.workspaces[0].strips[0].id;
        let survivor = 90_000;
        tree.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 89_999,
                tabs: vec![Tab {
                    id: survivor,
                    title: "Notes".into(),
                    tile: TileContent::PeerWindow {
                        app_id: "tilo.notes".into(),
                        label: "Notes".into(),
                        note: String::new(),
                    },
                }],
                active_tab: survivor,
                width: WindowWidth::Normal,
                maximized: false,
            });
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        group.tabs.push(Tab {
            id: 90_001,
            title: "External editor".into(),
            tile: TileContent::Foreign {
                app_id: "org.example.Editor".into(),
                surface_id: 0xfeed_beef,
            },
        });
        group.active_tab = 90_001;
        tree.active = workspace_id;
        tree.active_strip.insert(workspace_id, strip_id);
        tree.active_group.insert(strip_id, 89_999);

        save_tree_to(&tree, &path).expect("save");
        let on_disk: WorkspaceTree =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read durable state"))
                .expect("decode durable state");
        assert!(
            foreign_tabs(&on_disk).is_empty(),
            "the writer itself must sanitize live ids; load repair is only migration defence"
        );
        let loaded = load_tree_from(&path).expect("load");

        assert!(foreign_tabs(&loaded).is_empty());
        assert_eq!(
            local_apps(&loaded),
            vec![(90_001, "org.example.Editor".into())],
            "a live app becomes a relaunchable record in the same tab"
        );
        let loaded_group = &loaded.workspaces[0].strips[0].window_groups[0];
        assert_eq!(loaded.active, workspace_id);
        assert_eq!(loaded.active_strip.get(&workspace_id), Some(&strip_id));
        assert_eq!(loaded.active_group.get(&strip_id), Some(&89_999));
        assert_eq!(loaded_group.active_tab, 90_001);
        assert!(
            loaded_group
                .tabs
                .iter()
                .any(|tab| tab.id == loaded_group.active_tab),
            "symbolising a live surface must preserve the selected tab"
        );
    }

    #[test]
    fn a_closed_foreign_app_is_not_recreated_by_the_next_save() {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 92_001,
                tabs: vec![Tab {
                    id: 92_002,
                    title: "Chat".into(),
                    tile: TileContent::Foreign {
                        app_id: "com.example.Chat".into(),
                        surface_id: 92_003,
                    },
                }],
                active_tab: 92_002,
                width: WindowWidth::Normal,
                maximized: false,
            });
        assert!(crate::shell::tab_binding::close_tab_for_destroyed_surface(
            &mut tree, 92_003
        ));

        save_tree_to(&tree, &path).expect("save after close");
        let loaded = load_tree_from(&path).expect("restart");
        assert!(foreign_tabs(&loaded).is_empty());
        assert!(local_apps(&loaded).is_empty());
    }

    #[test]
    fn sanitizer_repairs_every_reference_when_the_last_live_group_disappears() {
        use crate::shell::state::{Collaborator, Tab, TileContent, WindowGroup, WindowWidth};

        let mut tree = WorkspaceTree::bootstrap_default();
        let strip_id = tree.workspaces[0].strips[0].id;
        let tab_id = 91_001;
        let surface_id = 91_002;
        tree.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 91_003,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "Ephemeral".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.example.Ephemeral".into(),
                        surface_id,
                    },
                }],
                active_tab: tab_id,
                width: WindowWidth::Normal,
                maximized: false,
            });
        tree.active_group.insert(strip_id, 91_003);
        tree.workspaces[0].shared_surfaces.share(surface_id, 44);
        tree.workspaces[0]
            .collaborators
            .push(Collaborator::with_active_tab("peer", "Peer", tab_id));
        tree.continuation_tabs.insert(tab_id);
        tree.continuation_owned_tabs
            .insert(tab_id, tree.workspaces[0].room);

        repair_legacy_foreign_surfaces(&mut tree);

        assert!(tree.workspaces[0].strips[0].window_groups.is_empty());
        assert!(!tree.active_group.contains_key(&strip_id));
        assert!(tree.workspaces[0].shared_surfaces.is_empty());
        assert_eq!(tree.workspaces[0].collaborators[0].active_tab, None);
        assert!(!tree.continuation_tabs.contains(&tab_id));
        assert!(!tree.continuation_owned_tabs.contains_key(&tab_id));
    }

    #[test]
    fn load_repairs_legacy_foreign_tabs_but_keeps_explicit_placeholders() {
        use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("legacy-state.json");
        let mut legacy = WorkspaceTree::bootstrap_default();
        legacy.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 90_004,
                tabs: Vec::new(),
                active_tab: 0,
                width: WindowWidth::Normal,
                maximized: false,
            });
        let group = &mut legacy.workspaces[0].strips[0].window_groups[0];
        group.tabs.push(Tab {
            id: 90_002,
            title: "Stale client".into(),
            tile: TileContent::Foreign {
                app_id: "org.example.Editor".into(),
                surface_id: 1234,
            },
        });
        group.tabs.push(Tab {
            id: 90_003,
            title: "Continue here".into(),
            tile: TileContent::PeerWindow {
                app_id: "org.example.Editor".into(),
                label: "Continue here".into(),
                note: String::new(),
            },
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy).expect("encode"))
            .expect("stage legacy state");

        let loaded = load_tree_from(&path).expect("load legacy state");
        assert!(foreign_tabs(&loaded).is_empty());
        assert!(
            loaded.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .any(|tab| matches!(&tab.tile, TileContent::PeerWindow { .. }))
        );
    }

    /// Presence must not survive the process that observed it — a
    /// persisted roster would paint people who aren't there. Safe to
    /// drop: durable membership lives on `capabilities` and `owner`,
    /// held fixed here to prove the two aren't confused.
    #[test]
    fn a_persisted_roster_does_not_come_back_as_presence() {
        use crate::shell::state::Collaborator;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        let mut original = WorkspaceTree::bootstrap_default();
        let room = original.active_workspace().expect("active workspace").room;
        assert!(original.note_peer_present(room, 0x11, "alice"));
        assert!(original.admit_collaborator_in_room(
            room,
            Collaborator::new("bob", "Bob").with_cursor_color("#f97316"),
        ));
        let owner_before = original.active_workspace().unwrap().owner.clone();
        let caps_before = original.active_workspace().unwrap().capabilities.len();

        save_tree_to(&original, &path).expect("save");
        let (loaded, notice) = load_or_default(&path);
        assert!(notice.is_none(), "a file this build just wrote must load");

        for ws in &loaded.workspaces {
            assert!(
                ws.collaborators.is_empty(),
                "workspace {:?} came back from disk showing {:?} — nobody \
                 is connected to a shell that has only just started, and \
                 these rows can never be retired because the sessions \
                 they named are gone",
                ws.name,
                ws.collaborators,
            );
        }

        let after = loaded
            .workspaces
            .iter()
            .find(|w| w.room == room)
            .expect("the room survives the round trip");
        assert_eq!(
            after.owner, owner_before,
            "dropping presence must not disturb who owns the workspace"
        );
        assert_eq!(
            after.capabilities.len(),
            caps_before,
            "dropping presence must not disturb granted capabilities"
        );
    }

    /// A JSON-only load leaves the automerge doc empty; `load_or_default`
    /// must seed it from the cache so `current_heads()` reports real history.
    #[test]
    fn load_or_default_seeds_doc_from_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        let original = WorkspaceTree::bootstrap_demo();
        save_tree_to(&original, &path).expect("save");

        let (mut loaded, notice) = load_or_default(&path);
        assert!(
            notice.is_none(),
            "a file this build just wrote must load without a notice"
        );
        assert!(
            !loaded.workspaces.is_empty(),
            "bootstrap tree must round-trip workspaces"
        );
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

        // Derived from the live serialiser, then `room` stripped from
        // every workspace, so it stays *loadable* as the struct grows.
        let mut original = WorkspaceTree::default();
        original
            .workspaces
            .push(Workspace::new(WorkspaceId(7), "Home"));
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
        // Identify the tree by the id the fixture wrote, not by its
        // shape — `bootstrap_default` is also a single workspace, and
        // its id is minted fresh, never 7.
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
        second_boot.persist_path = Some(path);
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
            raw.get("schema_version")
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(STATE_SCHEMA_VERSION)),
            "the writer must stamp the current schema version"
        );
    }

    /// The one this whole thing exists for: an older binary meets a file
    /// a newer one wrote. It must refuse, leave the file byte-for-byte
    /// alone, and hand the caller something to show the person —
    /// silently handing them a bootstrapped demo tree is indistinguishable
    /// from having lost their session.
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
            .push(crate::shell::state::Workspace::new(
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
            other @ StateNotice::Unreadable { .. } => panic!(
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

    /// The complement, and the reason the default is 1 rather than 0:
    /// every state file written before the key existed must still open,
    /// or the upgrade that introduced the check would itself cause the
    /// exact failure the check is meant to prevent.
    #[test]
    fn a_state_file_written_before_versioning_still_opens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");

        let mut original = WorkspaceTree::default();
        original
            .workspaces
            .push(crate::shell::state::Workspace::new(
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
        assert!(
            notice.is_none(),
            "a pre-versioning file must not be refused"
        );
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
