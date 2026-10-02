use crate::shell::state::{STATE_SCHEMA_VERSION, WorkspaceTree};
use automerge::AutoCommit;
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
            Self::NewerSchema { .. } => crate::shell::i18n::t("state-notice-newer-title"),
            Self::Unreadable { .. } => crate::shell::i18n::t("state-notice-unreadable-title"),
        }
    }

    /// One short line — a toast is 320x48 and does not grow. The claim
    /// that must survive the trim is "your work is still there".
    pub fn body(&self) -> String {
        match self {
            Self::NewerSchema { .. } => crate::shell::i18n::t("state-notice-newer-body"),
            Self::Unreadable { .. } => crate::shell::i18n::t("state-notice-unreadable-body"),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Self::NewerSchema {
                path,
                found,
                understood,
            } => crate::shell::i18n::t_args(
                "state-notice-newer-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("found", (*found).into()),
                    ("understood", (*understood).into()),
                ],
            ),
            Self::Unreadable { path, error } => crate::shell::i18n::t_args(
                "state-notice-unreadable-detail",
                &[
                    ("path", path.display().to_string().into()),
                    ("error", error.as_str().into()),
                ],
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
/// isn't the user's own session must set it — `xdg_paths::config_dir()`
/// **ignores a relative `XDG_CONFIG_HOME`**, so a harness isolating
/// itself with a relative path writes into the user's
/// `~/.config/tilo/state.json` instead.
pub fn state_json_path() -> PathBuf {
    if let Ok(v) = std::env::var("TILO_STATE_PATH")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    xdg_paths::config_dir().map_or_else(
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
    xdg_paths::config_dir().map_or_else(
        || PathBuf::from("fs-capabilities.json"),
        |c| c.join("tilo").join("fs-capabilities.json"),
    )
}

/// Native file envelope; the portable desktop schema remains independently versioned.
pub const NATIVE_STATE_SCHEMA_VERSION: u32 = 3;

pub(crate) fn encode_tree(tree: &WorkspaceTree) -> std::io::Result<Vec<u8>> {
    encode_durable(&durable_tree(tree)?)
}

/// The tree as it is written to disk: live surfaces become launch records,
/// and process-local launch progress and session-only grants are dropped.
fn durable_tree(tree: &WorkspaceTree) -> std::io::Result<WorkspaceTree> {
    // Prove encoded refusal bounds before cloning any tree or serialization buffer.
    super::native_workspace_owner::publication::validate_file_layout(
        NATIVE_STATE_SCHEMA_VERSION,
        &tree.native_authority,
    )
    .map_err(std::io::Error::other)?;
    super::native_workspace_owner::publication::preflight_tree(tree)
        .map_err(std::io::Error::other)?;
    let mut durable = tree.clone();
    durable.desktop.schema_version = NATIVE_STATE_SCHEMA_VERSION;
    // Native process surface IDs become durable symbolic launch records.
    symbolise_foreign_surfaces(&mut durable);
    clear_transient_launch_notes(&mut durable);
    durable.prune_local_app_records();
    for ws in &mut durable.desktop.workspaces {
        ws.capabilities
            .retain(ui::CapOverride::persists_across_restart);
    }
    // Symbolic launch records may grow (for example, copying a foreign title
    // into its durable label). Preserve the reserve on the exact output shape.
    super::native_workspace_owner::publication::preflight_tree(&durable)
        .map_err(std::io::Error::other)?;
    Ok(durable)
}

fn encode_durable(durable: &WorkspaceTree) -> std::io::Result<Vec<u8>> {
    use super::native_workspace_owner::store::{LimitedWriter, MAX_STATE_BYTES};
    let mut output = LimitedWriter::new(Vec::new(), MAX_STATE_BYTES);
    serde_json::to_writer_pretty(&mut output, durable).map_err(std::io::Error::other)?;
    Ok(output.into_inner())
}

/// Publish the complete native state file through its shared writer, then
/// the tree document beside it (see [`state_doc_path`]).
/// Session-only capabilities and native process surface IDs are not persisted.
///
/// # Errors
/// Refuses stale authority snapshots, encoded limits, competing writers, and
/// publication failures. Uncertain publication fences subsequent writes.
pub fn save_tree_to(tree: &WorkspaceTree, path: &Path) -> std::io::Result<()> {
    let mut durable = durable_tree(tree)?;
    let bytes = encode_durable(&durable)?;
    tree.native_runtime
        .publish(path, &tree.native_authority, &tree.native_authority, &bytes)?;
    save_doc_beside(&mut durable, path, &bytes);
    Ok(())
}

/// Largest tree document this build reads or writes. History only grows,
/// so this is far above the `state.json` cap.
const MAX_STATE_DOC_BYTES: usize = 256 * 1024 * 1024;

/// The tree document beside `state.json` (`state.automerge`): the desktop
/// with its whole change history, which Activity and Replay read. When it is
/// the companion of the `state.json` beside it, it is authoritative on load.
pub fn state_doc_path(state: &Path) -> PathBuf {
    state.with_extension("automerge")
}

/// Which `state.json` the document was written with, and the actor it was
/// written as.
fn state_doc_meta_path(state: &Path) -> PathBuf {
    let mut s = state_doc_path(state).into_os_string();
    s.push(".meta");
    PathBuf::from(s)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StateDocMeta {
    /// Kept so edits after a restart continue this device's history and are
    /// attributed like the edits before it. Safe to reuse: the tree document
    /// is never sent to a peer, so no change from this actor exists that is
    /// not in the saved document or lost with the process.
    actor: String,
    /// blake3 of the `state.json` bytes the document was written with.
    state_json: String,
}

fn state_json_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Write `durable`'s document next to the `state.json` just published from
/// it. The document first takes on exactly what `state.json` records, so the
/// two agree on load. Best-effort: `state.json` is already durable, and a
/// document that fails to write is caught on load by its meta.
fn save_doc_beside(durable: &mut WorkspaceTree, path: &Path, state_json: &[u8]) {
    durable.commit_room_change();
    durable.seed_doc_from_cache();
    let doc = durable.doc.save();
    if doc.len() > MAX_STATE_DOC_BYTES {
        tracing::warn!(
            bytes = doc.len(),
            "tree document exceeds its size cap; its history is not saved"
        );
        return;
    }
    let meta = StateDocMeta {
        actor: durable.doc.get_actor().to_hex_string(),
        state_json: state_json_digest(state_json),
    };
    let Ok(meta) = serde_json::to_vec(&meta) else {
        return;
    };
    // Document before meta: a meta naming this `state.json` must never sit
    // beside an older document.
    let written = tilo_files::native::atomic::replace(
        &state_doc_path(path),
        &doc,
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )
    .and_then(|()| {
        tilo_files::native::atomic::replace(
            &state_doc_meta_path(path),
            &meta,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )
    });
    if let Err(error) = written {
        tracing::warn!(path = %path.display(), %error, "could not save the tree document");
    }
}

/// [`save_doc_beside`] for a publication that encoded `tree` itself.
pub(crate) fn save_doc_for(tree: &WorkspaceTree, path: &Path, state_json: &[u8]) {
    match durable_tree(tree) {
        Ok(mut durable) => save_doc_beside(&mut durable, path, state_json),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not save the tree document");
        }
    }
}

enum SavedDoc {
    Absent,
    /// Written with the `state.json` being loaded.
    Current(AutoCommit),
    /// Written with an earlier `state.json`.
    Stale(AutoCommit),
}

fn read_capped(path: &Path, cap: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((cap as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(std::io::Error::other("tree document exceeds its size cap"));
    }
    Ok(bytes)
}

/// Keep a document that cannot be opened for recovery, out of the way of
/// the next save.
fn set_aside_unreadable_doc(doc_path: &Path, error: &dyn std::fmt::Display) {
    let mut aside = doc_path.as_os_str().to_os_string();
    aside.push(".unreadable");
    tracing::error!(
        path = %doc_path.display(),
        %error,
        "tree document cannot be opened; history restarts from state.json"
    );
    if let Err(e) = std::fs::rename(doc_path, &aside) {
        tracing::warn!(path = %doc_path.display(), error = %e, "could not set the unreadable tree document aside");
    }
}

fn load_saved_doc(path: &Path, state_json: &[u8]) -> SavedDoc {
    let doc_path = state_doc_path(path);
    let bytes = match read_capped(&doc_path, MAX_STATE_DOC_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SavedDoc::Absent,
        Err(error) => {
            set_aside_unreadable_doc(&doc_path, &error);
            return SavedDoc::Absent;
        }
    };
    let mut doc = match AutoCommit::load(&bytes) {
        Ok(doc) => doc,
        Err(error) => {
            set_aside_unreadable_doc(&doc_path, &error);
            return SavedDoc::Absent;
        }
    };
    let meta = std::fs::read(state_doc_meta_path(path))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StateDocMeta>(&bytes).ok());
    if let Some(actor) = meta
        .as_ref()
        .and_then(|meta| hex::decode(&meta.actor).ok())
        .filter(|actor| !actor.is_empty())
    {
        doc.set_actor(automerge::ActorId::from(actor));
    }
    if meta.is_some_and(|meta| meta.state_json == state_json_digest(state_json)) {
        SavedDoc::Current(doc)
    } else {
        SavedDoc::Stale(doc)
    }
}

/// Adopt the document saved beside `state.json`. A current one is
/// authoritative: the cache is rebuilt from it. A stale one — left when only
/// `state.json` was written, by an interrupted save or an older build —
/// keeps its history but not its content; the seed after load splices
/// `state.json`'s newer state into it. With no document, that seed starts
/// the history from `state.json` as one change.
fn attach_saved_doc(tree: &mut WorkspaceTree, path: &Path, state_json: &[u8]) {
    match load_saved_doc(path, state_json) {
        SavedDoc::Current(doc) => {
            tree.doc = doc;
            tree.resync_cache_from_doc();
        }
        SavedDoc::Stale(doc) => {
            tracing::info!(
                path = %path.display(),
                "tree document predates state.json; keeping its history and applying state.json"
            );
            tree.doc = doc;
        }
        SavedDoc::Absent => {}
    }
}

/// # Errors
///
/// Returns an error if `path` can't be read, its contents don't parse as
/// a [`WorkspaceTree`], or its `schema_version` is not
/// [`NATIVE_STATE_SCHEMA_VERSION`].
pub fn load_tree_from(path: &Path) -> std::io::Result<WorkspaceTree> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut s = String::new();
    file.take((super::native_workspace_owner::store::MAX_STATE_BYTES as u64).saturating_add(1))
        .read_to_string(&mut s)?;
    if s.len() > super::native_workspace_owner::store::MAX_STATE_BYTES {
        return Err(std::io::Error::other(
            "native encoded state refusal cap exceeded",
        ));
    }
    let mut tree: WorkspaceTree = serde_json::from_str::<WorkspaceTree>(&s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Refuse before anything else touches it: mutators here write the
    // whole tree back, so "load it and see" is "overwrite it and see".
    if tree.desktop.schema_version != NATIVE_STATE_SCHEMA_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "state file layout {} is not the {NATIVE_STATE_SCHEMA_VERSION} \
                 this build understands",
                tree.desktop.schema_version
            ),
        ));
    }
    super::native_workspace_owner::publication::validate_file_layout(
        tree.desktop.schema_version,
        &tree.native_authority,
    )
    .map_err(std::io::Error::other)?;
    super::native_workspace_owner::store::preflight(
        &tree.native_authority,
        super::native_workspace_owner::store::MAX_AUTHORITY_BYTES,
    )?;
    // The file envelope's version is stamped on write; in memory the tree
    // carries the portable desktop schema.
    tree.desktop.schema_version = STATE_SCHEMA_VERSION;
    // Before the repairs below, so they apply to what the document
    // restores too, and the seed after them records their outcome.
    attach_saved_doc(&mut tree, path, s.as_bytes());
    tree.prune_local_app_records();
    // `doc` is `#[serde(skip)]` in `state.json`. The seed is diff-aware: for
    // a current document it writes only what the repairs changed; for a
    // stale one, what `state.json` has that it lacks; with no document it
    // is the one-time migration that starts the history.
    tree.seed_doc_from_cache();
    // The `next_id` counter restarts at 1; without seeding, the next
    // allocation collides with a persisted id.
    tree.seed_next_id_from_self();
    Ok(tree)
}

/// The identity a live client window is relaunched by.
///
/// A mapped host window announces a toolkit/window-system identity, which
/// need not be the desktop-file id that `run-host` accepts. Keep the durable
/// record keyed by the launch identity whenever the installed catalogue can
/// correlate it. For example, Rygel Preferences announces
/// `org.gnome.Rygel.Preferences`, while its launchable entry is
/// `rygel-preferences.desktop`. Persisting the former made a normal
/// logout/login show "Not installed" even though a fresh palette launch
/// worked. Unknown foreign windows deliberately retain their announced
/// identity.
pub(crate) fn durable_app_id(surface_id: u64, app_id: &str) -> String {
    crate::shell::launcher::launch_key_for_surface(surface_id)
        .or_else(|| crate::shell::launcher::launch_key_for_app_id(app_id))
        .or_else(|| crate::shell::host_apps::app_for_window_id(app_id).map(|app| app.desktop_id))
        .unwrap_or_else(|| app_id.to_string())
}

/// Remove live compositor-client bindings from a durable tree and repair the
/// focus/index structures that referred to them.
///
/// `PeerWindow` and `AppPlaceholder` are intentionally retained: unlike a
/// `Foreign` tile they are symbolic records with no process-local object id,
/// and are the explicit continuation contract.
fn symbolise_foreign_surfaces(tree: &mut WorkspaceTree) {
    use crate::shell::state::TileContent;

    for workspace in &mut tree.desktop.workspaces {
        for strip in &mut workspace.strips {
            for group in &mut strip.window_groups {
                for tab in &mut group.tabs {
                    if let TileContent::Foreign { app_id, surface_id } = &tab.tile {
                        let launch_app_id = durable_app_id(*surface_id, app_id);
                        tab.tile = TileContent::LocalApp {
                            app_id: launch_app_id,
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

    for workspace in &mut tree.desktop.workspaces {
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
                Some(found) if found > NATIVE_STATE_SCHEMA_VERSION => StateNotice::NewerSchema {
                    path: path.to_path_buf(),
                    found,
                    understood: NATIVE_STATE_SCHEMA_VERSION,
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

    fn foreign_tabs(tree: &WorkspaceTree) -> Vec<(u64, u64)> {
        tree.desktop
            .workspaces
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
        tree.desktop
            .workspaces
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
        tree.desktop
            .workspaces
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
    fn opening_progress_is_never_persisted_and_migrates_back_to_retryable() {
        fn save_never_durably_records_process_local_opening_progress() {
            use crate::shell::state::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.desktop.workspaces[0].strips = vec![Strip {
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
            let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
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

        save_never_durably_records_process_local_opening_progress();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn foreign_surfaces_persist_symbolically_and_closed_ones_stay_closed() {
        fn live_foreign_surface_round_trip_preserves_symbolic_app_and_exact_placement() {
            use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_default();
            let workspace_id = tree.desktop.workspaces[0].id;
            let strip_id = tree.desktop.workspaces[0].strips[0].id;
            let survivor = 90_000;
            tree.desktop.workspaces[0].strips[0]
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
            let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            group.tabs.push(Tab {
                id: 90_001,
                title: "External editor".into(),
                tile: TileContent::Foreign {
                    app_id: "org.example.Editor".into(),
                    surface_id: 0xfeed_beef,
                },
            });
            group.active_tab = 90_001;
            tree.desktop.active = workspace_id;
            tree.desktop.active_strip.insert(workspace_id, strip_id);
            tree.desktop.active_group.insert(strip_id, 89_999);

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
            let loaded_group = &loaded.desktop.workspaces[0].strips[0].window_groups[0];
            assert_eq!(loaded.desktop.active, workspace_id);
            assert_eq!(
                loaded.desktop.active_strip.get(&workspace_id),
                Some(&strip_id)
            );
            assert_eq!(loaded.desktop.active_group.get(&strip_id), Some(&89_999));
            assert_eq!(loaded_group.active_tab, 90_001);
            assert!(
                loaded_group
                    .tabs
                    .iter()
                    .any(|tab| tab.id == loaded_group.active_tab),
                "symbolising a live surface must preserve the selected tab"
            );
        }

        fn host_foreign_surface_persists_the_launchable_desktop_id() {
            use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

            let data = tempfile::tempdir().expect("temporary XDG data home");
            let applications = data.path().join("applications");
            std::fs::create_dir_all(&applications).expect("applications directory");
            std::fs::write(
                applications.join("rygel-preferences.desktop"),
                "[Desktop Entry]\nType=Application\nName=Rygel Preferences\nExec=/bin/true\nNotShowIn=KDE;\n",
            )
            .expect("desktop entry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_DATA_HOME", data.path());
            crate::shell::host_apps::invalidate_cache();
            crate::shell::launcher::clear_launched_for_test();
            let app = crate::shell::host_apps::find("rygel-preferences")
                .expect("Rygel desktop entry should scan");
            let mut launch_identities = app.window_identities.clone();
            launch_identities.push("org.gnome.Rygel.Preferences".into());
            crate::shell::launcher::remember_launch_named(
                &app.desktop_id,
                &launch_identities,
                &app.name,
            );
            crate::shell::launcher::attribute_surface_launch(90_102, &app.desktop_id);
            crate::shell::launcher::set_pending_placement_for_identities(
                &app.desktop_id,
                &launch_identities,
                crate::shell::tab_binding::Placement::NewGroup,
            );
            assert_eq!(
                crate::shell::launcher::placement_for_toplevel(
                    "org.gnome.Rygel.Preferences",
                    90_102,
                ),
                Some(crate::shell::tab_binding::Placement::NewGroup)
            );

            let dir = tempfile::tempdir().expect("state directory");
            let path = dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.desktop.workspaces[0].strips[0]
                .window_groups
                .push(WindowGroup {
                    id: 90_100,
                    tabs: Vec::new(),
                    active_tab: 0,
                    width: WindowWidth::Normal,
                    maximized: false,
                });
            let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
            group.tabs.push(Tab {
                id: 90_101,
                title: "Rygel Preferences".into(),
                tile: TileContent::Foreign {
                    app_id: "org.gnome.Rygel.Preferences".into(),
                    surface_id: 90_102,
                },
            });
            group.active_tab = 90_101;

            save_tree_to(&tree, &path).expect("save host window");
            let loaded = load_tree_from(&path).expect("restart");
            assert_eq!(
                local_apps(&loaded),
                vec![(90_101, "rygel-preferences".into())],
                "restore must retain the desktop-file id accepted by run-host, not the transient WM class"
            );
            assert_eq!(
                crate::shell::host_apps::find("rygel-preferences").map(|app| app.name),
                Some("Rygel Preferences".into()),
                "the persisted identity must resolve directly on the next login"
            );

            crate::shell::host_apps::invalidate_cache();
            crate::shell::launcher::clear_launched_for_test();
            crate::shell::launcher::clear_pending_placements();
            drop(env);
        }

        fn a_closed_foreign_app_is_not_recreated_by_the_next_save() {
            use crate::shell::state::{Tab, TileContent, WindowGroup, WindowWidth};

            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.desktop.workspaces[0].strips[0]
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

        live_foreign_surface_round_trip_preserves_symbolic_app_and_exact_placement();
        host_foreign_surface_persists_the_launchable_desktop_id();
        a_closed_foreign_app_is_not_recreated_by_the_next_save();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn loaded_state_seeds_rooms_and_docs_but_not_presence() {
        /// Presence must not survive the process that observed it — a
        /// persisted roster would paint people who aren't there. Safe to
        /// drop: durable membership lives on `capabilities` and `owner`,
        /// held fixed here to prove the two aren't confused.
        fn a_persisted_roster_does_not_come_back_as_presence() {
            use crate::shell::state::Collaborator;

            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");

            let mut original = WorkspaceTree::bootstrap_default();
            let room = original
                .desktop
                .active_workspace()
                .expect("active workspace")
                .room;
            assert!(original.note_peer_present(room, 0x11, "alice"));
            assert!(original.admit_collaborator_in_room(
                room,
                Collaborator::new("bob", "Bob").with_cursor_color("#f97316"),
            ));
            let owner_before = original.desktop.active_workspace().unwrap().owner.clone();
            let caps_before = original
                .desktop
                .active_workspace()
                .unwrap()
                .capabilities
                .len();

            save_tree_to(&original, &path).expect("save");
            let (loaded, notice) = load_or_default(&path);
            assert!(notice.is_none(), "a file this build just wrote must load");

            for ws in &loaded.desktop.workspaces {
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
                .desktop
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

        /// The one-time migration: a `state.json` written before the tree
        /// document was saved beside it has no history to restore, so
        /// `load_or_default` starts one from it — a single change holding the
        /// whole desktop, so peer-sync exports the persisted workspaces
        /// (C-2 regression).
        fn a_state_file_without_a_document_starts_its_history_from_it() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");

            let mut original = WorkspaceTree::bootstrap_default();
            original.desktop.workspaces[0].name = "Migrated".into();
            save_tree_to(&original, &path).expect("save");
            // Simulate the writing process exiting and releasing its state-file
            // writer.
            drop(original);
            // What an older build leaves: `state.json` alone.
            std::fs::remove_file(state_doc_path(&path))
                .expect("the writer saves the document beside state.json");
            std::fs::remove_file(state_doc_meta_path(&path)).expect("and the meta that pairs them");

            let (mut loaded, notice) = load_or_default(&path);
            assert!(
                notice.is_none(),
                "a file this build just wrote must load without a notice"
            );
            assert_eq!(loaded.desktop.workspaces[0].name, "Migrated");
            assert_eq!(
                loaded.doc.get_changes(&[]).len(),
                1,
                "with no document to restore, the history starts as one change"
            );
            let mut from_doc = tilo_desktop_model::DesktopTree::default();
            tilo_desktop_model::desktop_doc::rebuild(&loaded.doc, &mut from_doc);
            assert_eq!(
                from_doc
                    .workspaces
                    .iter()
                    .map(|ws| (ws.id, ws.name.clone(), ws.room))
                    .collect::<Vec<_>>(),
                loaded
                    .desktop
                    .workspaces
                    .iter()
                    .map(|ws| (ws.id, ws.name.clone(), ws.room))
                    .collect::<Vec<_>>(),
                "the migrated document describes the loaded desktop"
            );
        }

        a_persisted_roster_does_not_come_back_as_presence();
        a_state_file_without_a_document_starts_its_history_from_it();
    }

    /// What the Activity pane shows for `tree`: rows as (id, author, summary),
    /// contributors, named ranges, and the workspace names at each Replay
    /// checkpoint.
    type ActivitySnapshot = (
        Vec<(ui::widget::HistoryEntryId, String, String)>,
        Vec<String>,
        Vec<String>,
        Vec<Vec<String>>,
    );

    fn activity_of(tree: &mut WorkspaceTree) -> ActivitySnapshot {
        let mut pane = tilo_desktop::activity::RoomActivityPane::new();
        pane.set_host(std::rc::Rc::new(
            crate::shell::room_activity::NativeActivityHost::default(),
        ));
        pane.set_scope(
            crate::shell::room_activity::RoomScope::AllWorkspaces,
            &*tree,
        );
        pane.refresh(tree);
        assert_eq!(
            pane.replay_unavailable_checkpoints(),
            0,
            "every Replay checkpoint must resolve in the document"
        );
        (
            pane.entries()
                .iter()
                .map(|entry| (entry.id, entry.author.name.clone(), entry.summary.clone()))
                .collect(),
            pane.contributors().to_vec(),
            pane.timeline()
                .named_ranges()
                .iter()
                .map(|range| range.label.clone())
                .collect(),
            pane.replay_checkpoint_names(),
        )
    }

    /// Activity and Replay read the tree document's history. Restarting
    /// used to rebuild that document from `state.json` as one fresh change,
    /// so every row, author, name and checkpoint was gone.
    #[test]
    fn activity_and_replay_survive_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();

        // Local edits: a rename and a second workspace.
        tree.desktop.workspaces[0].name = "Launch".into();
        tree.seed_doc_from_cache();
        let mut research = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(0x7e57_0001),
            "Research",
        );
        // Replay draws a workspace through its strips.
        research.strips.push(crate::shell::state::Strip {
            id: crate::shell::state::StripId(0x7e57_0002),
            name: "Main".into(),
            window_groups: Vec::new(),
        });
        tree.desktop.workspaces.push(research);
        tree.seed_doc_from_cache();
        // A peer's edit, under the peer's own actor.
        let base = tree.current_heads();
        let mut peer = tree.doc.fork();
        peer.set_actor(automerge::ActorId::from("ada".as_bytes()));
        let mut peer_desktop = tree.desktop.clone();
        peer_desktop.workspaces[1].name = "Ada's research".into();
        tilo_desktop_model::desktop_doc::seed(&mut peer, &peer_desktop);
        tilo_resource::history::commit_stamped(&mut peer);
        let changes: Vec<Vec<u8>> = peer
            .get_changes(&base)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        tree.apply_changes(&changes)
            .expect("the peer's change applies");
        // A named range over the first two rows.
        let (rows, ..) = activity_of(&mut tree);
        assert!(rows.len() >= 2, "the fixture must have history: {rows:?}");
        ui::history_groups::name_range(&mut tree.doc, &rows[0].0, Some(&rows[1].0), "Launch prep");
        tree.commit_room_change();

        let before = activity_of(&mut tree);
        let heads = tree.current_heads();
        let actor = tree.doc.get_actor().clone();
        assert!(
            before
                .0
                .iter()
                .any(|(_, author, _)| author.to_lowercase().contains("ada")),
            "the peer's row must be attributed to the peer: {:?}",
            before.0
        );
        assert_eq!(before.2, vec!["Launch prep".to_string()]);
        assert!(
            before
                .3
                .contains(&vec!["Launch".to_string(), "Ada's research".to_string()]),
            "Replay must have a checkpoint showing the current desktop: {:?}",
            before.3
        );

        save_tree_to(&tree, &path).expect("save");
        drop(tree);
        let mut restarted = load_tree_from(&path).expect("restart");

        assert_eq!(
            restarted.current_heads(),
            heads,
            "a restart neither loses history nor writes any of its own"
        );
        assert_eq!(
            restarted.doc.get_actor(),
            &actor,
            "local edits after a restart continue this device's history"
        );
        assert_eq!(activity_of(&mut restarted), before);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn state_file_schema_versions_are_stamped_and_anything_else_is_refused() {
        /// Every state file this build writes says which layout it is. Without
        /// this the version check has nothing to read on the next boot, and the
        /// refusal below can only ever fire for a hand-edited file.
        fn a_written_state_file_stamps_its_schema_version() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            save_tree_to(&WorkspaceTree::bootstrap_demo(), &path).expect("save");

            let raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
            assert_eq!(
                raw.get("schema_version")
                    .and_then(serde_json::Value::as_u64),
                Some(u64::from(NATIVE_STATE_SCHEMA_VERSION)),
                "the writer must stamp the current schema version"
            );
        }

        /// The one this whole thing exists for: an older binary meets a file
        /// a newer one wrote. It must refuse, leave the file byte-for-byte
        /// alone, and hand the caller something to show the person —
        /// silently handing them a bootstrapped demo tree is indistinguishable
        /// from having lost their session.
        fn a_newer_state_file_is_refused_preserved_and_reported() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");

            // Derived from the live writer, so it stays a *loadable* file as the
            // struct grows. A hand-written fixture that silently stopped parsing
            // would take the "unreadable" branch and this test would be asserting
            // about the wrong refusal.
            let mut original = WorkspaceTree::default();
            original
                .desktop
                .workspaces
                .push(crate::shell::state::Workspace::new(
                    crate::shell::state::WorkspaceId(7),
                    "Home",
                ));
            save_tree_to(&original, &path).expect("save");
            let mut raw: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
            let future = NATIVE_STATE_SCHEMA_VERSION + 1;
            raw["schema_version"] = serde_json::json!(future);
            let on_disk = serde_json::to_string_pretty(&raw).expect("re-json");
            std::fs::write(&path, &on_disk).expect("write future state");

            let (tree, notice) = load_or_default(&path);

            // Refused: this is a bootstrapped tree, not the one on disk.
            assert!(
                !tree.desktop.workspaces.iter().any(|w| w.id.0 == 7),
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
                    assert_eq!(*understood, NATIVE_STATE_SCHEMA_VERSION);
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

        /// Older layouts are refused too, and the file is left untouched.
        fn older_and_unversioned_state_files_are_refused_and_preserved() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");

            let mut original = WorkspaceTree::default();
            original
                .desktop
                .workspaces
                .push(crate::shell::state::Workspace::new(
                    crate::shell::state::WorkspaceId(9),
                    "Home",
                ));
            save_tree_to(&original, &path).expect("save");
            let written: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
            let mut unversioned = written.clone();
            assert!(
                unversioned
                    .as_object_mut()
                    .expect("object")
                    .remove("schema_version")
                    .is_some(),
                "the current writer must emit schema_version, or this fixture \
                 removes nothing and the test is vacuous"
            );
            let mut older = written;
            older["schema_version"] = serde_json::json!(NATIVE_STATE_SCHEMA_VERSION - 1);
            for raw in [unversioned, older] {
                let on_disk = serde_json::to_string_pretty(&raw).expect("re-json");
                std::fs::write(&path, &on_disk).expect("write older state");

                let (tree, notice) = load_or_default(&path);
                assert!(
                    matches!(notice, Some(StateNotice::Unreadable { .. })),
                    "an older state file must be refused, got {notice:?}"
                );
                assert!(
                    !tree.desktop.workspaces.iter().any(|w| w.id.0 == 9),
                    "the workspaces in an older file must not be loaded"
                );
                assert_eq!(
                    std::fs::read_to_string(&path).expect("re-read"),
                    on_disk,
                    "the state file must not have been touched"
                );
            }
        }

        a_written_state_file_stamps_its_schema_version();
        a_newer_state_file_is_refused_preserved_and_reported();
        older_and_unversioned_state_files_are_refused_and_preserved();
    }
}
