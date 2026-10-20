use crate::shell::state::{RoomId, STATE_SCHEMA_VERSION, TabId, WorkspaceTree};
use std::path::{Path, PathBuf};

mod doc_log;
pub(crate) use doc_log::DocLog;

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
/// isn't the user's own session must set it — `tilo_paths::config_dir()`
/// **ignores a relative `XDG_CONFIG_HOME`**, so a harness isolating
/// itself with a relative path writes into the user's
/// `~/.config/tilo/state.json` instead.
pub fn state_json_path() -> PathBuf {
    if let Ok(v) = std::env::var("TILO_STATE_PATH")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    tilo_paths::config_dir().map_or_else(
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
    tilo_paths::config_dir().map_or_else(
        || PathBuf::from("fs-capabilities.json"),
        |c| c.join("tilo").join("fs-capabilities.json"),
    )
}

/// Native file envelope; the portable desktop schema remains independently versioned.
pub const NATIVE_STATE_SCHEMA_VERSION: u32 = 4;

/// `state.json` as a versioned format: a file of an older version comes up
/// to [`NATIVE_STATE_SCHEMA_VERSION`] when it is loaded, after a snapshot of
/// it is kept beside it (docs/architecture/compatibility.md). A change to the
/// file's shape bumps the version and adds a step from the one before;
/// `oldest` never rises, so a machine that missed updates still loads.
pub(crate) const STATE_FORMAT: tilo_files::native::migration::Format =
    tilo_files::native::migration::Format {
        name: "desktop state",
        version: NATIVE_STATE_SCHEMA_VERSION,
        oldest: 3,
        steps: &[drop_untouched_lounges],
    };

/// Version 3 to 4: workspaces no longer get a Lounge. Version 3 gave every
/// workspace one, a strip and its first windows (chat, activity, notes)
/// under ids in the [`RESERVED_SALT`] namespace, which nobody but that code
/// minted in. A Lounge the person never put a window or tab of their own
/// into (every window and tab id of it in that namespace) goes; one they
/// used stays as it is, and so does one that is a workspace's only strip.
/// Focus on a dropped strip moves to the workspace's first strip.
///
/// [`RESERVED_SALT`]: tilo_desktop_model::ids::RESERVED_SALT
#[allow(
    clippy::needless_pass_by_value,
    reason = "a migration step: every step of a file's migrations takes its bytes and returns them"
)]
fn drop_untouched_lounges(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    use serde_json::Value;
    use tilo_desktop_model::ids::{RESERVED_SALT, peer_salt_of};
    fn reserved(id: Option<&Value>) -> bool {
        id.and_then(Value::as_u64)
            .is_some_and(|id| peer_salt_of(id) == RESERVED_SALT)
    }
    fn untouched(strip: &Value) -> bool {
        reserved(strip.get("id"))
            && strip
                .get("window_groups")
                .and_then(Value::as_array)
                .is_none_or(|groups| {
                    groups.iter().all(|group| {
                        reserved(group.get("id"))
                            && group
                                .get("tabs")
                                .and_then(Value::as_array)
                                .is_none_or(|tabs| tabs.iter().all(|tab| reserved(tab.get("id"))))
                    })
                })
    }
    let mut file: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let mut dropped: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    // Workspace id, as the focus maps key it, to its first strip left.
    let mut first_left: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for workspace in file
        .get_mut("workspaces")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let key = workspace
            .get("id")
            .and_then(Value::as_u64)
            .map(|id| id.to_string());
        let Some(strips) = workspace.get_mut("strips").and_then(Value::as_array_mut) else {
            continue;
        };
        if strips.iter().all(untouched) {
            continue;
        }
        strips.retain(|strip| {
            if !untouched(strip) {
                return true;
            }
            if let Some(id) = strip.get("id").and_then(Value::as_u64) {
                dropped.insert(id);
            }
            false
        });
        if let (Some(key), Some(first)) = (
            key,
            strips
                .first()
                .and_then(|strip| strip.get("id"))
                .and_then(Value::as_u64),
        ) {
            first_left.insert(key, first);
        }
    }
    if let Some(active) = file.get_mut("active_strip").and_then(Value::as_object_mut) {
        for (workspace, strip) in active.iter_mut() {
            if strip.as_u64().is_some_and(|id| dropped.contains(&id))
                && let Some(first) = first_left.get(workspace)
            {
                *strip = Value::from(*first);
            }
        }
    }
    if let Some(groups) = file.get_mut("active_group").and_then(Value::as_object_mut) {
        groups.retain(|strip, _| !strip.parse::<u64>().is_ok_and(|id| dropped.contains(&id)));
    }
    if let Some(retained) = file
        .get_mut("strips_to_retain")
        .and_then(Value::as_array_mut)
    {
        retained.retain(|strip| strip.as_u64().is_none_or(|id| !dropped.contains(&id)));
    }
    let Some(fields) = file.as_object_mut() else {
        return Err("a state file is a JSON object".into());
    };
    fields.insert("schema_version".into(), Value::from(4_u32));
    serde_json::to_vec(&file).map_err(|error| error.to_string())
}

/// A Lounge as version 3 made it: a strip and three windows under ids in
/// the reserved namespace, derived from `seed`; with `touched_by`, a window
/// under that device's salt that the person put into it.
#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixture ids: a seed below a million, times eight, plus a counter below eight"
)]
fn v3_lounge(seed: u64, touched_by: Option<u16>) -> tilo_desktop_model::Strip {
    use tilo_desktop_model::ids::RESERVED_SALT;
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, compose_id};
    let base = (seed % 1_000_000) * 8;
    let group = |id: u64, title: &str| WindowGroup {
        id,
        tabs: vec![Tab {
            id,
            title: title.into(),
            tile: TileContent::WorkspaceActivity,
        }],
        active_tab: id,
        width: tilo_desktop_model::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut lounge = Strip::new(StripId(compose_id(RESERVED_SALT, base + 1)), "Lounge");
    for (counter, title) in [(2, "Chat"), (3, "Activity"), (4, "Notes")] {
        lounge
            .window_groups
            .push(group(compose_id(RESERVED_SALT, base + counter), title));
    }
    if let Some(salt) = touched_by {
        lounge
            .window_groups
            .push(group(compose_id(salt, base + 7), "Sketch"));
    }
    lounge
}

/// A file the previous version of [`STATE_FORMAT`] wrote, which its step
/// is tested against: version 3, whose every workspace had a Lounge first.
#[cfg(test)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a format at its first version has no previous file, which the test reads as None"
)]
fn previous_state_file() -> Option<Vec<u8>> {
    let mut tree = WorkspaceTree::bootstrap_default();
    let workspace = &mut tree.desktop.workspaces[0];
    workspace.strips.insert(0, v3_lounge(workspace.id.0, None));
    let bytes = encode_tree(&tree).expect("encoded").bytes;
    let mut file: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    file["schema_version"] = serde_json::Value::from(3_u32);
    Some(serde_json::to_vec(&file).expect("json"))
}

/// The version a state file says it is, read before anything else of it.
#[derive(serde::Deserialize)]
struct StoredVersion {
    #[serde(default)]
    schema_version: u32,
}

/// Encode `tree` as its state file, refusing what exceeds the file's bounds.
///
/// # Errors
/// The authority's layout is invalid, or the encoded file would leave no
/// room for the recovery its enrolled rooms reserved.
pub(crate) fn encode_tree(tree: &WorkspaceTree) -> std::io::Result<EncodedTree> {
    // Refuse a malformed authority before copying anything.
    check_authority(&tree.native_authority)?;
    encode_durable(&DurableTree::of(tree))
}

/// A state file, encoded, and the digest of the owner record it holds.
pub(crate) struct EncodedTree {
    pub(crate) bytes: Vec<u8>,
    /// What [`NativeStateRuntime::publish`] expects of the file once this is
    /// in it: computed in the one pass that encoded the record.
    ///
    /// [`NativeStateRuntime::publish`]: super::native_workspace_owner::NativeStateRuntime
    pub(crate) authority: [u8; 32],
}

fn check_authority(
    authority: &super::native_workspace_owner::NativeAuthorityStorage,
) -> std::io::Result<()> {
    super::native_workspace_owner::publication::validate_file_layout(
        NATIVE_STATE_SCHEMA_VERSION,
        authority,
    )
    .map_err(std::io::Error::other)
}

/// Exactly what `state.json` holds, in [`WorkspaceTree`]'s serde shape, and
/// nothing else: none of the tree's documents, which are saved beside it
/// (see [`state_doc_path`]). Copying it costs the size of the file, not of
/// the history.
#[derive(serde::Serialize)]
pub(crate) struct DurableTree {
    /// First, so the file can be written as this record, encoded once,
    /// followed by [`DurableRest`].
    native_authority: super::native_workspace_owner::NativeAuthorityStorage,
    #[serde(flatten)]
    rest: DurableRest,
}

/// Every field of [`DurableTree`] after the owner record.
#[derive(serde::Serialize)]
pub(crate) struct DurableRest {
    #[serde(flatten)]
    desktop: crate::shell::state::DesktopTree,
    room_authority: std::collections::HashMap<RoomId, crate::shell::state::RoomAuthority>,
    room_peer_names: std::collections::HashMap<RoomId, std::collections::BTreeMap<String, String>>,
    room_peer_identities: std::collections::HashMap<
        RoomId,
        std::collections::BTreeMap<String, crate::shell::state::GovernedPeerIdentity>,
    >,
    room_share_floor: std::collections::HashMap<RoomId, u64>,
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    strips_to_retain: std::collections::BTreeSet<crate::shell::state::StripId>,
    continuation_tabs: std::collections::BTreeSet<TabId>,
    continuation_rooms: std::collections::BTreeSet<RoomId>,
    continuation_owned_tabs: std::collections::HashMap<TabId, RoomId>,
    continuation_closed_tabs: std::collections::HashMap<TabId, RoomId>,
    continuation_file_references:
        std::collections::HashMap<TabId, crate::shell::personal_continuity::LaunchIntent>,
    native_document_tabs:
        std::collections::HashMap<TabId, crate::shell::state::NativeDocumentReference>,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    local_app_records: std::collections::HashMap<TabId, crate::shell::state::LocalAppRecord>,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    package_launches: std::collections::HashMap<TabId, String>,
}

impl DurableTree {
    /// `tree` as it is written to disk: live surfaces become launch records,
    /// and process-local launch progress and session-only grants are dropped.
    /// Unchecked: the encoded size bounds are for whoever encodes it.
    pub(crate) fn of(tree: &WorkspaceTree) -> Self {
        let mut desktop = tree.desktop.clone();
        desktop.schema_version = NATIVE_STATE_SCHEMA_VERSION;
        // Native process surface IDs become durable symbolic launch records.
        symbolise_foreign_surfaces(&mut desktop);
        clear_transient_launch_notes(&mut desktop);
        let mut local_app_records = tree.local_app_records.clone();
        WorkspaceTree::prune_local_app_records_of(&desktop, &mut local_app_records);
        let mut package_launches = tree.package_launches.clone();
        WorkspaceTree::prune_package_launches_of(&desktop, &mut package_launches);
        for ws in &mut desktop.workspaces {
            ws.sandbox.retain(ui::CapOverride::persists_across_restart);
        }
        Self {
            native_authority: tree.native_authority.clone(),
            rest: DurableRest {
                desktop,
                room_authority: tree.room_authority.clone(),
                room_peer_names: tree.room_peer_names.clone(),
                room_peer_identities: tree.room_peer_identities.clone(),
                room_share_floor: tree.room_share_floor.clone(),
                strips_to_retain: tree.strips_to_retain.clone(),
                continuation_tabs: tree.continuation_tabs.clone(),
                continuation_rooms: tree.continuation_rooms.clone(),
                continuation_owned_tabs: tree.continuation_owned_tabs.clone(),
                continuation_closed_tabs: tree.continuation_closed_tabs.clone(),
                continuation_file_references: tree.continuation_file_references.clone(),
                native_document_tabs: tree.native_document_tabs.clone(),
                local_app_records,
                package_launches,
            },
        }
    }
}

/// Encode `durable` once, refusing it when the authority is malformed or the
/// file would leave no room for the recovery its enrolled rooms reserved.
///
/// The owner record, by far the file's largest part, is pretty-printed once:
/// that one pass also refuses it as `preflight_storage` would and hashes it
/// for the writer's digest. The bytes are exactly `durable` pretty-printed.
fn encode_durable(durable: &DurableTree) -> std::io::Result<EncodedTree> {
    use super::native_workspace_owner::store::LimitedWriter;
    /// How the file opens, up to the record: its first field.
    const OPEN: &[u8] = b"{\n  \"native_authority\": ";
    check_authority(&durable.native_authority)?;
    let limit =
        super::native_workspace_owner::publication::state_file_limit(&durable.native_authority)
            .map_err(std::io::Error::other)?;
    let mut embedded = Vec::new();
    embedded.extend_from_slice(OPEN);
    let authority = super::native_workspace_owner::store::encode_authority(
        &durable.native_authority,
        Some(&mut embedded),
    )?;
    if embedded.len() > limit {
        return Err(std::io::Error::other(
            "native encoded state refusal cap exceeded",
        ));
    }
    // The other fields alone are an object, brace and all: with the record's
    // comma in place of that brace, they continue the file. Never empty: the
    // desktop has fields.
    let mut fields = LimitedWriter::new(Vec::new(), limit.saturating_sub(embedded.len()));
    serde_json::to_writer_pretty(&mut fields, &durable.rest).map_err(std::io::Error::other)?;
    let mut fields = fields.into_inner();
    if !fields.starts_with(b"{\n") {
        return Err(std::io::Error::other(
            "durable tree fields are not an object",
        ));
    }
    if let Some(brace) = fields.first_mut() {
        *brace = b',';
    }
    embedded.append(&mut fields);
    #[cfg(test)]
    ENCODES_ON_THIS_THREAD.with(|encodes| encodes.set(encodes.get().saturating_add(1)));
    Ok(EncodedTree {
        bytes: embedded,
        authority,
    })
}

/// What a save writes, taken from the tree while the caller holds it: a copy
/// of the fields `state.json` holds, and the tree document's changes since
/// the previous save, cut from it (see `doc_log`). No document is copied;
/// encoding, the size checks and every write happen in
/// [`TreeSnapshot::write_to`].
pub struct TreeSnapshot {
    durable: DurableTree,
    runtime: super::native_workspace_owner::NativeStateRuntime,
    sequence: super::native_workspace_owner::store::StateSequence,
}

impl TreeSnapshot {
    /// Take `tree` as it would be saved now.
    #[must_use]
    pub fn of(tree: &WorkspaceTree) -> Self {
        tree.native_runtime.document_log().cut(&tree.doc);
        Self {
            sequence: tree.native_runtime.next_sequence(),
            runtime: tree.native_runtime.clone(),
            durable: DurableTree::of(tree),
        }
    }

    /// Publish the snapshot to `path` unless a later snapshot of the same
    /// tree already reached the file, then write the tree document's pending
    /// changes beside it either way.
    ///
    /// # Errors
    /// As [`save_tree_to`].
    pub(crate) fn write_to(
        self,
        path: &Path,
    ) -> std::io::Result<super::native_workspace_owner::store::Publication> {
        let encoded = encode_durable(&self.durable)?;
        // A snapshot changes no record: it holds the one the file holds.
        let published = self.runtime.publish(
            path,
            encoded.authority,
            &self.durable.native_authority,
            &encoded,
            self.sequence,
        )?;
        // Superseded or not, the changes cut for this snapshot are written:
        // the snapshot that superseded it may have been cut before them.
        write_document(&self.runtime, path);
        #[cfg(test)]
        WRITES_ON_THIS_THREAD.with(|writes| {
            if published == super::native_workspace_owner::store::Publication::Written {
                writes.set(writes.get().saturating_add(1));
            }
        });
        Ok(published)
    }
}

#[cfg(test)]
thread_local! {
    /// State files this thread published, so a test can tell a save it
    /// queued from one it performed.
    pub(crate) static WRITES_ON_THIS_THREAD: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// State files this thread encoded, so a test can count the encodes a
    /// publication costs.
    pub(crate) static ENCODES_ON_THIS_THREAD: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Publish the complete native state file through its shared writer, then
/// the tree document beside it (see [`state_doc_path`]), on this thread.
/// Session-only capabilities and native process surface IDs are not persisted.
///
/// # Errors
/// Refuses stale authority snapshots, encoded limits, competing writers, and
/// publication failures. Uncertain publication fences subsequent writes.
pub fn save_tree_to(tree: &WorkspaceTree, path: &Path) -> std::io::Result<()> {
    // Refuse a malformed authority before copying anything.
    check_authority(&tree.native_authority)?;
    TreeSnapshot::of(tree).write_to(path).map(drop)
}

/// Save `tree` to `path` on the state writer thread, then run
/// `after_save` there if this snapshot is the one the file now holds.
/// Returns once the snapshot is taken; a failure is logged there. See
/// [`wait_for_saves`].
pub fn queue_save(tree: &WorkspaceTree, path: &Path, after_save: impl FnOnce() + Send + 'static) {
    queue_save_or_report(tree, path, after_save, |_| {});
}

/// [`queue_save`], also telling `after_failure` why a write failed, so what
/// waits on this save ends with its error instead of waiting for a save that
/// will not happen.
pub fn queue_save_or_report(
    tree: &WorkspaceTree,
    path: &Path,
    after_save: impl FnOnce() + Send + 'static,
    after_failure: impl FnOnce(&std::io::Error) + Send + 'static,
) {
    let snapshot = TreeSnapshot::of(tree);
    let path = path.to_path_buf();
    super::durable_writer::global().submit(path.clone(), move || {
        use super::native_workspace_owner::store::Publication;
        let started = std::time::Instant::now();
        match snapshot.write_to(&path) {
            Ok(Publication::Written) => after_save(),
            Ok(Publication::Superseded) => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "workspace tree persistence failed");
                after_failure(&error);
                return;
            }
        }
        let took = started.elapsed();
        if took > std::time::Duration::from_millis(100) {
            tracing::info!(
                target: "tilo::latency",
                took_ms = took.as_secs_f64() * 1_000.0,
                "workspace tree save was slow (off the frame thread)"
            );
        }
    });
}

/// Wait until every save queued so far is on disk: before the session ends
/// or sleeps, and before anything reads the files back.
pub fn wait_for_saves() {
    settings_store::flush_pending_writes();
    super::durable_writer::global().flush();
}

/// [`wait_for_saves`] for a deadline that must not be missed, like the end
/// of the session. Returns whether every save made it.
pub fn wait_for_saves_within(limit: std::time::Duration) -> bool {
    saves_within(
        super::durable_writer::global(),
        settings_store::flush_pending_writes,
        limit,
    )
}

/// The key the settings flush is queued under: flushing is idempotent, so a
/// flush still queued stands for a later one.
const SETTINGS_FLUSH_KEY: &str = "settings-store pending writes";

/// Make the pending settings writes and every queued save durable on
/// `writer`, waiting at most `limit` for both. The settings flush syncs the
/// settings records and takes their cross-process lock, which another
/// process may hold: it runs on the writer behind the saves queued before
/// it, never on the caller's thread, so the suspend path on the compositor
/// loop waits no longer than its bound.
fn saves_within(
    writer: &super::durable_writer::DurableWriter,
    flush_settings: impl FnOnce() + Send + 'static,
    limit: std::time::Duration,
) -> bool {
    let started = std::time::Instant::now();
    writer.submit(PathBuf::from(SETTINGS_FLUSH_KEY), flush_settings);
    let saved = writer.flush_timeout(limit);
    if !saved {
        tracing::error!(
            waited_ms = started.elapsed().as_millis(),
            "workspace tree saves did not finish in time; the newest changes may be lost"
        );
    }
    saved
}

/// Largest tree document this build reads or writes, and largest log beside
/// it. History only grows, so this is far above the `state.json` cap.
const MAX_STATE_DOC_BYTES: usize = 256 * 1024 * 1024;

/// The tree document beside `state.json` (`state.automerge`): the desktop
/// with its whole change history, which Activity and Replay read. Changes
/// since it was last compacted are in the log beside it
/// (`state.automerge.log`, see `doc_log`).
pub fn state_doc_path(state: &Path) -> PathBuf {
    state.with_extension("automerge")
}

/// The actor the document is written as.
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
    /// Builds before the log named here the `state.json` the document was
    /// written with, and then rebuilt the desktop from the document instead.
    /// The document now records the live desktop and its base can lag the
    /// log, so this build never names one: an older build keeps the
    /// document's history and applies `state.json` over it.
    #[serde(default)]
    state_json: String,
}

fn write_meta(state: &Path, actor: &str) -> std::io::Result<()> {
    let meta = serde_json::to_vec(&StateDocMeta {
        actor: actor.to_owned(),
        state_json: String::new(),
    })
    .map_err(std::io::Error::other)?;
    tilo_files::native::atomic::replace(
        &state_doc_meta_path(state),
        &meta,
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )
    .map_err(tilo_files::native::atomic::PublicationError::into_io_error)
}

/// Write the tree document's pending changes beside the state file at
/// `path`, and its meta after them. Best-effort: `state.json` is already
/// durable, and on load it is what the desktop is rebuilt from.
fn write_document(runtime: &super::native_workspace_owner::NativeStateRuntime, path: &Path) {
    if let Err(error) =
        runtime
            .document_log()
            .write(&state_doc_path(path), MAX_STATE_DOC_BYTES, |actor| {
                write_meta(path, actor)
            })
    {
        tracing::warn!(path = %path.display(), %error, "could not save the tree document");
    }
}

/// Write `tree`'s document beside the state file at `path` now, for a
/// publication that encoded and published `tree` itself.
pub(crate) fn save_document(tree: &WorkspaceTree, path: &Path) {
    tree.native_runtime.document_log().cut(&tree.doc);
    write_document(&tree.native_runtime, path);
}

/// Keep a document and log that cannot be opened for recovery, out of the
/// way of the next save.
fn set_aside_unreadable_doc(doc_path: &Path, error: &dyn std::fmt::Display) {
    tracing::error!(
        path = %doc_path.display(),
        %error,
        "tree document cannot be opened; history restarts from state.json"
    );
    for path in [doc_path.to_path_buf(), doc_log::log_path(doc_path)] {
        let mut aside = path.as_os_str().to_os_string();
        aside.push(".unreadable");
        match std::fs::rename(&path, &aside) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "could not set the unreadable tree document aside");
            }
        }
    }
}

/// Adopt the document saved beside `state.json`, with the history its log
/// adds, and continue its files from there. `state.json` stays what the
/// desktop is: the seed after load splices whatever it has that the
/// document lacks into it — changes still unwritten when the last session
/// ended, or the durable form of its live windows. With no document, that
/// seed starts the history from `state.json` as one change.
fn attach_saved_doc(tree: &mut WorkspaceTree, path: &Path) {
    let doc_path = state_doc_path(path);
    let loaded = match doc_log::load(&doc_path, MAX_STATE_DOC_BYTES) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return,
        Err(error) => {
            set_aside_unreadable_doc(&doc_path, &error);
            return;
        }
    };
    if loaded.extent.torn {
        tracing::warn!(
            path = %doc_path.display(),
            "the tree document's log ends in a torn write; the history before it is kept"
        );
    }
    let mut doc = loaded.doc;
    let meta = std::fs::read(state_doc_meta_path(path))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StateDocMeta>(&bytes).ok());
    let actor = meta
        .and_then(|meta| hex::decode(&meta.actor).ok())
        .filter(|actor| !actor.is_empty())
        .map(automerge::ActorId::from);
    let meta_actor = actor.as_ref().map(automerge::ActorId::to_hex_string);
    if let Some(actor) = actor {
        doc.set_actor(actor);
    }
    tree.native_runtime
        .document_log()
        .attach(&doc_path, &doc, loaded.extent, meta_actor);
    tree.doc = doc;
}

/// # Errors
///
/// Returns an error if `path` can't be read, its contents don't parse as
/// a [`WorkspaceTree`], or its `schema_version` is not
/// [`NATIVE_STATE_SCHEMA_VERSION`].
pub fn load_tree_from(path: &Path) -> std::io::Result<WorkspaceTree> {
    use std::io::Read;
    // A save still queued in this process is newer than the file.
    wait_for_saves();
    let file = std::fs::File::open(path)?;
    let mut s = String::new();
    file.take((super::native_workspace_owner::store::MAX_STATE_BYTES as u64).saturating_add(1))
        .read_to_string(&mut s)?;
    if s.len() > super::native_workspace_owner::store::MAX_STATE_BYTES {
        return Err(std::io::Error::other(
            "native encoded state refusal cap exceeded",
        ));
    }
    // An older file comes up to date first, a snapshot of it kept; a newer
    // one is refused untouched.
    let stored = serde_json::from_str::<StoredVersion>(&s)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .schema_version;
    let s = String::from_utf8(
        STATE_FORMAT
            .bring_up(path, s.into_bytes(), stored)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
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
    // Before the repairs below, so the seed after them records their outcome.
    attach_saved_doc(&mut tree, path);
    drop_unidentifiable_windows(&mut tree);
    tree.prune_local_app_records();
    tree.prune_package_launches();
    // `doc` is `#[serde(skip)]` in `state.json`. The seed is diff-aware: it
    // writes only what `state.json` and the repairs have that the document
    // lacks — nothing, after a clean shutdown; with no document it is the
    // one-time migration that starts the history.
    tree.seed_doc_from_cache();
    // The `next_id` counter restarts at 1; without seeding, the next
    // allocation collides with a persisted id.
    tree.seed_next_id_from_self();
    Ok(tree)
}

/// Drop the saved windows whose program can't be told (no app id, so the
/// stand-in `anon-<surface>`): nothing can relaunch them, so restoring one
/// only left a card that could never open. Each is recorded in the log.
/// A window whose program is named is always kept, installed or not.
fn drop_unidentifiable_windows(tree: &mut WorkspaceTree) {
    use crate::shell::state::TileContent;

    let unidentifiable: Vec<(crate::shell::state::TabId, String, String)> = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::LocalApp { app_id, .. }
                if !crate::shell::peer_windows::identifiable_app_id(app_id) =>
            {
                Some((tab.id, app_id.clone(), tab.title.clone()))
            }
            _ => None,
        })
        .collect();
    for (tab, app_id, title) in unidentifiable {
        if crate::shell::tab_binding::remove_tab(tree, tab) {
            tracing::info!(
                tab,
                app = %app_id,
                title = %title,
                "not restoring a saved window whose program can't be identified: nothing can reopen it"
            );
        }
    }
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
fn symbolise_foreign_surfaces(desktop: &mut crate::shell::state::DesktopTree) {
    use crate::shell::state::TileContent;

    for workspace in &mut desktop.workspaces {
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
fn clear_transient_launch_notes(desktop: &mut crate::shell::state::DesktopTree) {
    use crate::shell::state::TileContent;

    for workspace in &mut desktop.workspaces {
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

    /// The state file keeps every version a machine may hold loadable: each
    /// step is tested against the file the version before it wrote, and a
    /// file from a newer build is refused untouched.
    #[test]
    fn the_previous_state_file_comes_up_to_date_and_a_newer_one_is_left_alone() {
        assert!(STATE_FORMAT.is_whole());
        match previous_state_file() {
            Some(previous) => {
                let previous = previous.as_slice();
                let stored: StoredVersion = serde_json::from_slice(previous).unwrap();
                assert_eq!(
                    stored.schema_version,
                    STATE_FORMAT.version - 1,
                    "previous_state_file is a file the previous version wrote"
                );
                tilo_files::native::migration::assert_step_reads_previous(
                    &STATE_FORMAT,
                    previous,
                    |current| {
                        serde_json::from_slice::<WorkspaceTree>(current)
                            .expect("the step's output reads as the current state");
                    },
                );
            }
            None => assert_eq!(
                STATE_FORMAT.version, STATE_FORMAT.oldest,
                "a state format past its first version keeps a file its previous version wrote \
                 in previous_state_file"
            ),
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let newer = format!("{{\"schema_version\":{}}}", NATIVE_STATE_SCHEMA_VERSION + 1);
        std::fs::write(&path, &newer).unwrap();
        assert!(load_tree_from(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    // USER REQUIREMENT (2026-10-19): "if i create a workspace I want it to be empty, remove this crap"
    /// A state file from when every workspace got a Lounge comes up without
    /// the Lounges nobody used: a workspace keeps only the strips its person
    /// made or used, and focus on a dropped Lounge goes to the first strip
    /// left. A Lounge someone put a window into, or a workspace's only strip,
    /// stays. The file as it was is kept beside it.
    #[test]
    fn a_v3_state_file_loses_the_lounges_nobody_used() {
        use tilo_desktop_model::ids::{RESERVED_SALT, peer_salt_of};
        let mut tree = WorkspaceTree::bootstrap_default();
        let home = tree.desktop.workspaces[0].id;
        let home_main = tree.desktop.workspaces[0].strips[0].id;
        let used = tree.append_empty_workspace("Used");
        let alone = tree.append_empty_workspace("Alone");
        let salt = 0x1234;
        let (home_lounge, used_lounge) = {
            let ws = |tree: &mut WorkspaceTree, id| {
                tree.desktop
                    .workspaces
                    .iter()
                    .position(|w| w.id == id)
                    .unwrap()
            };
            let h = ws(&mut tree, home);
            let lounge = v3_lounge(home.0, None);
            let home_lounge = lounge.id;
            tree.desktop.workspaces[h].strips.insert(0, lounge);
            let u = ws(&mut tree, used);
            let lounge = v3_lounge(used.0, Some(salt));
            let used_lounge = lounge.id;
            tree.desktop.workspaces[u].strips.insert(0, lounge);
            let a = ws(&mut tree, alone);
            tree.desktop.workspaces[a].strips = vec![v3_lounge(alone.0, None)];
            (home_lounge, used_lounge)
        };
        tree.desktop.active_strip.insert(home, home_lounge);
        tree.desktop.active_group.insert(
            home_lounge,
            tree.desktop.workspaces[0].strips[0].window_groups[0].id,
        );
        let bytes = encode_tree(&tree).expect("encoded").bytes;
        let mut file: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        file["schema_version"] = serde_json::Value::from(3_u32);
        let v3 = serde_json::to_vec(&file).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, &v3).unwrap();

        let loaded = load_tree_from(&path).expect("a v3 file comes up to date");
        let strips_of = |id| {
            loaded
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .unwrap()
                .strips
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(strips_of(home), [home_main], "the unused Lounge is gone");
        assert!(
            !loaded.desktop.workspaces.iter().any(|w| w.id == home
                && w.strips
                    .iter()
                    .any(|s| peer_salt_of(s.id.raw()) == RESERVED_SALT)),
            "and nothing of it is left"
        );
        assert_eq!(loaded.desktop.active_strip.get(&home), Some(&home_main));
        assert!(!loaded.desktop.active_group.contains_key(&home_lounge));
        assert_eq!(
            strips_of(used)[0],
            used_lounge,
            "a Lounge someone put a window into stays"
        );
        assert_eq!(strips_of(used).len(), 2);
        assert_eq!(strips_of(alone).len(), 1, "a workspace keeps a strip");
        assert_eq!(
            std::fs::read(tilo_files::native::migration::Format::snapshot_path(
                &path, 3
            ))
            .unwrap(),
            v3,
            "the v3 file is kept as it was"
        );
        let now: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(now["schema_version"], NATIVE_STATE_SCHEMA_VERSION);
    }

    /// Preparing for sleep makes the settings and the queued saves durable
    /// from the compositor loop under a 2 s bound. A settings flush stuck
    /// on the disk, or on the records' lock another process holds, runs on
    /// the state writer: the loop's wait ends at its bound, nothing syncs on
    /// the loop's thread, and the flush still completes once it can.
    #[cfg(target_os = "linux")]
    #[test]
    fn preparing_for_sleep_never_waits_past_its_bound_for_the_settings_flush() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};
        let writer = super::super::durable_writer::DurableWriter::spawn("test-state-writer");
        let (release, stuck) = mpsc::channel::<()>();
        let flushed_on = Arc::new(Mutex::new(None));
        let recorded = Arc::clone(&flushed_on);
        let (returned, waited, loop_thread) =
            crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
                let started = Instant::now();
                let returned = saves_within(
                    &writer,
                    move || {
                        let _ = stuck.recv();
                        *recorded.lock().unwrap() = Some(std::thread::current().id());
                    },
                    Duration::from_millis(100),
                );
                (returned, started.elapsed(), std::thread::current().id())
            });
        assert!(!returned, "a stuck flush is reported as not saved");
        assert!(
            waited < Duration::from_secs(2),
            "the loop waited {waited:?} for a flush bounded at 100 ms"
        );
        assert!(
            flushed_on.lock().unwrap().is_none(),
            "the flush was still stuck"
        );
        release.send(()).unwrap();
        writer.flush();
        let ran_on = *flushed_on.lock().unwrap();
        let ran_on = ran_on.expect("the flush completed");
        assert_ne!(ran_on, loop_thread, "the flush ran on the state writer");
    }

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

    /// A saved window whose program can't be identified (`anon-<surface>`,
    /// no app id) is not restored: nothing can reopen it. A named app is
    /// always kept, whether this computer has it or not.
    #[test]
    #[serial_test::serial(process_env)]
    fn restore_drops_only_windows_whose_program_cant_be_identified() {
        use crate::shell::state::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let local = |id: u64, app_id: &str, title: &str, note: &str| Tab {
            id,
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: app_id.into(),
                label: title.into(),
                note: note.into(),
            },
        };
        tree.desktop.workspaces[0].strips = vec![Strip {
            id: StripId::from(92_100),
            name: "Main".into(),
            window_groups: vec![
                WindowGroup {
                    id: 92_099,
                    tabs: vec![
                        local(
                            92_101,
                            "anon-1524713390118",
                            "tilo",
                            "Not installed on this machine",
                        ),
                        local(
                            92_102,
                            "org.example.Missing",
                            "Missing",
                            "Not installed on this machine",
                        ),
                    ],
                    active_tab: 92_101,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
                WindowGroup {
                    id: 92_098,
                    tabs: vec![local(92_103, "", "untitled", "")],
                    active_tab: 92_103,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
                WindowGroup {
                    id: 92_097,
                    tabs: vec![local(92_104, "org.gnome.TextEditor", "Notes.txt", "")],
                    active_tab: 92_104,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
                },
            ],
        }];
        save_tree_to(&tree, &path).expect("save");

        let loaded = load_tree_from(&path).expect("load");
        assert_eq!(
            local_apps(&loaded),
            vec![
                (92_102, "org.example.Missing".to_owned()),
                (92_104, "org.gnome.TextEditor".to_owned()),
            ],
            "the unidentifiable windows go; every named app stays"
        );
        let strip = &loaded.desktop.workspaces[0].strips[0];
        assert_eq!(
            strip.window_groups.len(),
            2,
            "a group left empty goes with it"
        );
        assert_eq!(strip.window_groups[0].active_tab, 92_102);
        // The document agrees, so the next save and every peer see the same.
        let mut reread = loaded.desktop.clone();
        tilo_desktop_model::desktop_doc::rebuild(&loaded.doc, &mut reread);
        let in_doc: Vec<u64> = reread.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|group| group.tabs.iter().map(|tab| tab.id))
            .collect();
        assert_eq!(in_doc, vec![92_102, 92_104]);
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
                    focus_history: Vec::new(),
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
                        missing: None,
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
                            missing: None,
                        },
                    }],
                    active_tab: survivor,
                    width: WindowWidth::Normal,
                    maximized: false,
                    focus_history: Vec::new(),
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
                    focus_history: Vec::new(),
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
                    focus_history: Vec::new(),
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
            assert!(original.admit_collaborator_in_room(
                room,
                Collaborator::new("alice", "alice").with_session(0x11),
            ));
            assert!(original.admit_collaborator_in_room(
                room,
                Collaborator::new("bob", "Bob").with_cursor_color("#f97316"),
            ));
            let policy_before = original.desktop.active_workspace().unwrap().policy.clone();
            let caps_before = original.desktop.active_workspace().unwrap().sandbox.len();

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
                after.policy, policy_before,
                "dropping presence must not disturb who owns the workspace or its grants"
            );
            assert_eq!(
                after.sandbox.len(),
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

        /// Layouts older than any step brings up are refused too, and the
        /// file is left untouched. (A file of a version a step reads comes up
        /// to date instead; the migration test covers that.)
        fn too_old_and_unversioned_state_files_are_refused_and_preserved() {
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
            older["schema_version"] = serde_json::json!(STATE_FORMAT.oldest - 1);
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
        too_old_and_unversioned_state_files_are_refused_and_preserved();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn saves_run_on_the_state_writer_newest_last_and_never_torn() {
        fn two_workspaces(path: &Path) -> (WorkspaceTree, [crate::shell::state::WorkspaceId; 2]) {
            let mut tree = WorkspaceTree::bootstrap_default();
            let first = tree.desktop.workspaces[0].id;
            let second = tree.append_empty_workspace("Second");
            tree.desktop.active = first;
            tree.seed_doc_from_cache();
            tree.persist_path = Some(path.to_path_buf());
            save_tree_to(&tree, path).expect("baseline save");
            (tree, [first, second])
        }

        // Phase `a_workspace_switch_writes_nothing_on_the_calling_thread`.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let (mut tree, [first, second]) = two_workspaces(&path);
            let before = WRITES_ON_THIS_THREAD.with(std::cell::Cell::get);

            assert!(tree.switch_down());
            assert!(tree.switch_up());
            assert!(tree.switch_down());

            assert_eq!(
                WRITES_ON_THIS_THREAD.with(std::cell::Cell::get),
                before,
                "a workspace switch must leave the state file to the writer thread"
            );
            wait_for_saves();
            let restarted = load_tree_from(&path).expect("restart");
            assert_eq!(
                restarted.desktop.active, second,
                "the newest switch is what a restart opens"
            );
            assert_ne!(restarted.desktop.active, first);
        }

        // Phase `a_queued_save_never_overwrites_a_later_synchronous_one`.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let (mut tree, [first, second]) = two_workspaces(&path);
            // Taken on the frame thread, written late by the writer thread.
            let stale = TreeSnapshot::of(&tree);
            tree.set_active(second);
            save_tree_to(&tree, &path).expect("synchronous save");
            assert_eq!(
                stale
                    .write_to(&path)
                    .expect("a superseded snapshot is not an error"),
                crate::shell::native_workspace_owner::store::Publication::Superseded
            );

            let restarted = load_tree_from(&path).expect("restart");
            assert_eq!(
                restarted.desktop.active, second,
                "the file keeps the later state, and its document agrees"
            );
            assert_ne!(restarted.desktop.active, first);
        }

        // Phase `a_burst_of_saves_lands_as_its_newest_state`.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let (mut tree, [_, _]) = two_workspaces(&path);
            let mut names = Vec::new();
            for index in 0..40 {
                let name = format!("Burst {index}");
                tree.append_empty_workspace(&name);
                tree.queue_save();
                names.push(name);
            }
            wait_for_saves();
            let restarted = load_tree_from(&path).expect("restart");
            let on_disk: Vec<&str> = restarted
                .desktop
                .workspaces
                .iter()
                .map(|workspace| workspace.name.as_str())
                .collect();
            for name in &names {
                assert!(on_disk.contains(&name.as_str()), "{name} was lost");
            }
        }

        // Phase `a_reader_racing_the_writer_only_sees_whole_files`.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("state.json");
            let (mut tree, [_, _]) = two_workspaces(&path);
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reader = {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut reads = 0_u32;
                    while !stop.load(std::sync::atomic::Ordering::Acquire) {
                        let bytes = std::fs::read(&path).expect("the state file always exists");
                        serde_json::from_slice::<WorkspaceTree>(&bytes)
                            .expect("a reader never sees a partly written state file");
                        reads = reads.saturating_add(1);
                    }
                    reads
                })
            };
            for index in 0..30 {
                tree.append_empty_workspace(&format!("Racing {index}"));
                tree.queue_save();
                if index % 5 == 0 {
                    wait_for_saves();
                }
            }
            wait_for_saves();
            stop.store(true, std::sync::atomic::Ordering::Release);
            assert!(reader.join().expect("reader") > 0);
            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .expect("state dir")
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with('.') && !name.ends_with(".writer-lock"))
                .collect();
            assert!(
                leftovers.is_empty(),
                "every replacement completed its rename: {leftovers:?}"
            );
        }
    }

    /// The durable copy as it was made before [`DurableTree`]: the whole
    /// tree, documents included, cloned and then transformed. Kept as the
    /// oracle for `state.json`'s shape.
    fn durable_copy_before(tree: &WorkspaceTree) -> WorkspaceTree {
        let mut durable = tree.clone();
        durable.desktop.schema_version = NATIVE_STATE_SCHEMA_VERSION;
        symbolise_foreign_surfaces(&mut durable.desktop);
        clear_transient_launch_notes(&mut durable.desktop);
        durable.prune_local_app_records();
        durable.prune_package_launches();
        for ws in &mut durable.desktop.workspaces {
            ws.sandbox.retain(ui::CapOverride::persists_across_restart);
        }
        durable
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn durable_tree_serialises_exactly_like_workspace_tree() {
        use crate::shell::state::{
            Access, CapOverride, Expiry, LocalAppRecord, NativeDocumentReference, RoomAuthority,
            Scope, Subject, Tab, TileContent,
        };

        let mut tree = WorkspaceTree::bootstrap_demo();
        let room = tree.desktop.workspaces[0].room;
        let group = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
        group.tabs.push(Tab {
            id: 93_001,
            title: "Editor".into(),
            tile: TileContent::Foreign {
                app_id: "org.example.ParityEditor".into(),
                surface_id: 93_002,
            },
        });
        group.tabs.push(Tab {
            id: 93_003,
            title: "Terminal".into(),
            tile: TileContent::LocalApp {
                app_id: "org.example.Terminal".into(),
                label: "Terminal".into(),
                note: crate::shell::peer_windows::OPENING.into(),
            },
        });
        let capabilities = &mut tree.desktop.workspaces[0].sandbox;
        capabilities.push(
            CapOverride::grant(
                Subject::app("so.tilo.books"),
                Scope::FsPath("/var/workspace/kept".into()),
                Access::Read,
            )
            .with_id("kept"),
        );
        capabilities.push(
            CapOverride::grant(
                Subject::app("so.tilo.books"),
                Scope::FsPath("/var/workspace/session".into()),
                Access::Write,
            )
            .with_id("session-only")
            .expiring(Expiry::EndOfSession),
        );
        tree.room_authority.insert(room, RoomAuthority::Local);
        tree.room_peer_names
            .entry(room)
            .or_default()
            .insert("peer".into(), "Ada".into());
        tree.room_share_floor.insert(room, 41);
        tree.strips_to_retain
            .insert(crate::shell::state::StripId(93_010));
        tree.continuation_tabs.insert(93_003);
        tree.continuation_rooms.insert(room);
        tree.continuation_owned_tabs.insert(93_003, room);
        tree.continuation_closed_tabs.insert(93_004, room);
        tree.native_document_tabs.insert(
            93_003,
            NativeDocumentReference {
                provider_id: "notes".into(),
                path: "/home/ada/notes.md".into(),
            },
        );
        for (tab, app_id) in [
            (93_003, "org.example.Terminal"),
            (93_005, "org.example.Gone"),
        ] {
            tree.local_app_records.insert(
                tab,
                LocalAppRecord {
                    room,
                    app_id: app_id.into(),
                    record_instance: 7,
                },
            );
            tree.package_launches.insert(tab, app_id.into());
        }
        tree.seed_doc_from_cache();

        let before = serde_json::to_vec_pretty(&durable_copy_before(&tree)).expect("oracle");
        let now = serde_json::to_vec_pretty(&DurableTree::of(&tree)).expect("durable tree");
        assert_eq!(
            String::from_utf8_lossy(&now),
            String::from_utf8_lossy(&before),
            "state.json keeps its exact shape"
        );
        assert_eq!(
            String::from_utf8_lossy(&encode_tree(&tree).expect("encode").bytes),
            String::from_utf8_lossy(&now),
            "the file written with its record encoded once is the same file"
        );
        let written: serde_json::Value = serde_json::from_slice(&now).expect("json");
        assert!(written["local_app_records"].get("93003").is_some());
        assert!(
            written["local_app_records"].get("93005").is_none(),
            "the fixture prunes something"
        );
        assert!(
            !String::from_utf8_lossy(&now).contains("session-only"),
            "the fixture drops a session grant"
        );
    }

    #[test]
    fn a_save_hands_the_writer_only_what_changed_and_restores_the_live_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let second = tree.append_empty_workspace("Second");
        // No `persist_path`: the saves below are this test's own, not the
        // writer thread's racing them.
        for index in 0..400 {
            tree.desktop.workspaces[0].name = format!("Renamed {index}");
            tree.seed_doc_from_cache();
        }
        save_tree_to(&tree, &path).expect("baseline save");

        tree.set_active(second);
        tree.commit_room_change();
        let snapshot = TreeSnapshot::of(&tree);
        let handed = tree.native_runtime.document_log().pending_bytes();
        // What handing over the whole history would cost, change by change.
        let history: usize = tree.export_changes_since(&[]).iter().map(Vec::len).sum();
        assert!(handed > 0, "the switch is handed to the writer");
        assert!(
            handed.saturating_mul(20) < history,
            "a save hands over the switch ({handed} bytes), not the history ({history} bytes)"
        );
        snapshot.write_to(&path).expect("save");
        assert_eq!(tree.native_runtime.document_log().pending_bytes(), 0);
        assert!(
            doc_log::log_path(&state_doc_path(&path)).exists(),
            "the change was appended, not the document rewritten"
        );

        let heads = tree.current_heads();
        let actor = tree.doc.get_actor().clone();
        drop(tree);
        let mut restarted = load_tree_from(&path).expect("restart");
        assert_eq!(restarted.desktop.active, second);
        assert_eq!(
            restarted.current_heads(),
            heads,
            "base and log together are the live history"
        );
        assert_eq!(restarted.doc.get_actor(), &actor);
    }

    #[test]
    fn a_torn_document_log_loses_only_its_last_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        let first = tree.desktop.workspaces[0].id;
        save_tree_to(&tree, &path).expect("baseline save");
        for name in ["One", "Two", "Three"] {
            tree.append_empty_workspace(name);
            tree.commit_room_change();
            save_tree_to(&tree, &path).expect("save");
        }
        drop(tree);
        let log = doc_log::log_path(&state_doc_path(&path));
        let len = std::fs::metadata(&log).expect("log").len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .expect("open log")
            .set_len(len.saturating_sub(5))
            .expect("tear the last append");

        let restarted = load_tree_from(&path).expect("a torn log still loads");
        let names = |desktop: &tilo_desktop_model::DesktopTree| -> Vec<String> {
            desktop
                .workspaces
                .iter()
                .map(|ws| ws.name.clone())
                .collect()
        };
        assert!(
            names(&restarted.desktop).iter().any(|name| name == "Three"),
            "state.json, not the document, is what the desktop is"
        );
        let mut from_doc = tilo_desktop_model::DesktopTree::default();
        tilo_desktop_model::desktop_doc::rebuild(&restarted.doc, &mut from_doc);
        assert_eq!(
            names(&from_doc),
            names(&restarted.desktop),
            "the seed after load restores what the torn frame held"
        );
        assert_eq!(restarted.desktop.workspaces[0].id, first);
        let set_aside: Vec<_> = std::fs::read_dir(dir.path())
            .expect("state dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".unreadable"))
            .collect();
        assert!(
            set_aside.is_empty(),
            "a torn tail sets nothing aside: {set_aside:?}"
        );
    }

    #[test]
    fn a_document_saved_by_the_previous_build_loads_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.desktop.workspaces[0].name = "Before the log".into();
        tree.seed_doc_from_cache();
        save_tree_to(&tree, &path).expect("save");
        // What the previous build left: the whole document, rewritten on
        // every save, and a meta naming the state.json it was written with.
        let state_json = std::fs::read(&path).expect("state.json");
        std::fs::write(state_doc_path(&path), tree.doc.save()).expect("document");
        std::fs::write(
            state_doc_meta_path(&path),
            serde_json::json!({
                "actor": tree.doc.get_actor().to_hex_string(),
                "state_json": blake3::hash(&state_json).to_hex().to_string(),
            })
            .to_string(),
        )
        .expect("meta");
        assert!(!doc_log::log_path(&state_doc_path(&path)).exists());
        let heads = tree.current_heads();
        let actor = tree.doc.get_actor().clone();
        drop(tree);

        let mut restarted = load_tree_from(&path).expect("restart");
        assert_eq!(restarted.current_heads(), heads);
        assert_eq!(restarted.doc.get_actor(), &actor);
        assert_eq!(restarted.desktop.workspaces[0].name, "Before the log");

        // And the history continues as a log beside it.
        restarted.desktop.workspaces[0].name = "After the log".into();
        restarted.seed_doc_from_cache();
        save_tree_to(&restarted, &path).expect("save");
        assert!(doc_log::log_path(&state_doc_path(&path)).exists());
        let heads = restarted.current_heads();
        drop(restarted);
        let mut again = load_tree_from(&path).expect("restart again");
        assert_eq!(again.current_heads(), heads);
    }
}
