//! Owner-local durable watch history.
//!
//! This is deliberately a *separate* Automerge document from Tube's shared
//! collaboration state. An owner key inside a shared document is only a UI
//! filter, never a confidentiality or replication boundary.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionContext, ActionError, ExecutionLocus};

/// This type is intentionally never registered in the shared Tube action
/// registry. It is dispatched only into [`OwnerHistoryStore`].
pub const OWNER_HISTORY_ACTION_TYPE: &str = "tube.owner-history.action";
const MAX_HISTORY_BYTES: u64 = 4 * 1024 * 1024;
const COMPACT_AT_BYTES: usize = 512 * 1024;
pub const MAX_HISTORY_ENTRIES: usize = 50;
const HISTORY_ROOT: &str = "owner_history";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntryRecord {
    pub present: bool,
    pub watched_at_ms: u64,
    pub mutation_id: String,
}

/// Canonical, local-only operations for one signed-in owner's watch history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerHistoryAction {
    SetEntry {
        video_id: String,
        watched_at_ms: u64,
        present: bool,
        mutation_id: String,
    },
    Clear {
        cleared_at_ms: u64,
        clear_id: String,
    },
    /// Records an explicit one-way import from an old shared document. This
    /// marker prevents future shared-document reads; it does not and cannot
    /// recall bytes that old collaborators already replicated.
    LegacyMigrationComplete,
}

impl Action for OwnerHistoryAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        let history = history_map(doc)?;
        match self {
            Self::SetEntry {
                video_id,
                watched_at_ms,
                present,
                mutation_id,
            } => {
                let entries = child_map(doc, &history, "entries")?;
                let record = HistoryEntryRecord {
                    present: *present,
                    watched_at_ms: *watched_at_ms,
                    mutation_id: mutation_id.clone(),
                };
                let encoded = serde_json::to_string(&record).map_err(|error| {
                    ActionError::Refused(format!("could not encode owner history entry: {error}"))
                })?;
                doc.put(&entries, video_id, ScalarValue::Str(encoded.into()))?;
            }
            Self::Clear {
                cleared_at_ms,
                clear_id,
            } => {
                let clears = child_map(doc, &history, "clears")?;
                doc.put(
                    &clears,
                    clear_id,
                    ScalarValue::Str(cleared_at_ms.to_string().into()),
                )?;
            }
            Self::LegacyMigrationComplete => {
                doc.put(
                    &history,
                    "legacy_shared_migration_v1",
                    ScalarValue::Boolean(true),
                )?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

fn history_map(doc: &mut tilo_actions::Doc) -> Result<ObjId, ActionError> {
    match doc.get(&ROOT, HISTORY_ROOT)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Ok(doc.put_object(&ROOT, HISTORY_ROOT, ObjType::Map)?),
    }
}

fn child_map(doc: &mut tilo_actions::Doc, parent: &ObjId, key: &str) -> Result<ObjId, ActionError> {
    match doc.get(parent, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Ok(doc.put_object(parent, key, ObjType::Map)?),
    }
}

pub struct OwnerHistoryStore {
    doc: AutoCommit,
    path: Option<PathBuf>,
    recovery_disabled: bool,
}

impl OwnerHistoryStore {
    pub fn open(owner_id: &str) -> Self {
        let Some(base) = xdg_paths::data_local_dir() else {
            return Self::memory();
        };
        Self::open_path(
            base.join("tilo")
                .join("tube")
                .join("history")
                .join(format!("{}.automerge", owner_file_id(owner_id))),
        )
    }

    fn open_path(path: PathBuf) -> Self {
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                doc: AutoCommit::new(),
                path: Some(path),
                recovery_disabled: false,
            },
            Ok(_) => match read_private_document(&path) {
                Ok(doc) => Self {
                    doc,
                    path: Some(path),
                    recovery_disabled: false,
                },
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "owner history recovery required; refusing to overwrite existing store");
                    Self {
                        doc: AutoCommit::new(),
                        path: None,
                        recovery_disabled: true,
                    }
                }
            },
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "could not inspect owner history store; persistence disabled");
                Self {
                    doc: AutoCommit::new(),
                    path: None,
                    recovery_disabled: true,
                }
            }
        }
    }

    #[must_use]
    pub fn memory() -> Self {
        Self {
            doc: AutoCommit::new(),
            path: None,
            recovery_disabled: false,
        }
    }

    #[must_use]
    pub fn document(&self) -> &AutoCommit {
        &self.doc
    }

    #[must_use]
    pub fn has_legacy_migration_marker(&self) -> bool {
        self.doc
            .get(&ROOT, HISTORY_ROOT)
            .ok()
            .flatten()
            .and_then(|(value, history)| {
                matches!(value, Value::Object(ObjType::Map)).then_some(history)
            })
            .and_then(|history| {
                self.doc
                    .get(&history, "legacy_shared_migration_v1")
                    .ok()
                    .flatten()
                    .and_then(|(value, _)| value.as_bool())
            })
            .unwrap_or(false)
    }

    #[must_use]
    pub fn recovery_disabled(&self) -> bool {
        self.recovery_disabled
    }

    /// Apply a canonical owner-local operation and durably save it. A failed
    /// save restores the exact pre-operation document and returns false; it
    /// never reroutes the operation through shared collaboration state.
    pub fn dispatch(&mut self, host: &mut ui::ActionHost, action: &OwnerHistoryAction) -> bool {
        let before = self.doc.save();
        let context =
            ActionContext::for_action("org.tilo.Tube", "Tube owner-local history", action);
        if let Err(error) = host.try_dispatch_with_context(
            &mut self.doc,
            OWNER_HISTORY_ACTION_TYPE,
            action,
            context,
        ) {
            tracing::warn!(%error, "refused owner-local Tube history action");
            return false;
        }
        if let Err(error) = self.compact_if_needed() {
            tracing::warn!(%error, "could not compact owner-local Tube history");
            self.restore_after_failed_dispatch(&before);
            return false;
        }
        if let Err(error) = self.persist() {
            tracing::warn!(%error, "could not persist owner-local Tube history");
            self.restore_after_failed_dispatch(&before);
            return false;
        }
        true
    }

    /// Persist a bounded local document. Failure stays local to this store.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery disabled persistence, directory setup,
    /// bounded serialization, or the atomic local write fails.
    pub fn persist(&mut self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return (!self.recovery_disabled).then_some(()).ok_or_else(|| {
                std::io::Error::other("owner history persistence is disabled pending recovery")
            });
        };
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("history path has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let bytes = self.doc.save();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_HISTORY_BYTES {
            return Err(std::io::Error::other(
                "history store exceeds its size limit",
            ));
        }
        // Atomic helper writes a private sibling, fsyncs, renames, then fsyncs
        // the directory. Rename replaces a final symlink rather than following it.
        atomic_file::write_creating_parent_with_mode(path, &bytes, 0o600)
    }

    fn restore_after_failed_dispatch(&mut self, before: &[u8]) {
        match AutoCommit::load(before) {
            Ok(previous) => self.doc = previous,
            Err(restore_error) => {
                tracing::error!(%restore_error, "could not restore owner history after persistence failure");
            }
        }
    }

    fn compact_if_needed(&mut self) -> std::io::Result<()> {
        if self.doc.save().len() <= COMPACT_AT_BYTES {
            return Ok(());
        }
        self.compact_snapshot()
    }

    /// Retain the current visible set in a fresh owner-local document. This
    /// bounds Automerge change history before the hard persistence cap; it is
    /// safe because the document is never merged with collaborators and device
    /// transfer uses the separately bounded sync package.
    fn compact_snapshot(&mut self) -> std::io::Result<()> {
        let Some((Value::Object(ObjType::Map), history)) =
            self.doc.get(&ROOT, HISTORY_ROOT).ok().flatten()
        else {
            return Ok(());
        };
        let migration_marker = self.has_legacy_migration_marker();
        let cleared_at_ms = self
            .doc
            .get(&history, "clears")
            .ok()
            .flatten()
            .and_then(|(value, clears)| {
                matches!(value, Value::Object(ObjType::Map)).then_some(clears)
            })
            .map_or(0, |clears| {
                self.doc
                    .keys(&clears)
                    .filter_map(|id| {
                        self.doc
                            .get_all(&clears, id.as_str())
                            .ok()
                            .and_then(|values| {
                                values
                                    .into_iter()
                                    .filter_map(|(value, _)| {
                                        value.as_str().and_then(|value| value.parse::<u64>().ok())
                                    })
                                    .max()
                            })
                    })
                    .max()
                    .unwrap_or(0)
            });
        let mut entries = self
            .doc
            .get(&history, "entries")
            .ok()
            .flatten()
            .and_then(|(value, entries)| {
                matches!(value, Value::Object(ObjType::Map)).then_some(entries)
            })
            .map_or_else(Vec::new, |entries| {
                self.doc
                    .keys(&entries)
                    .filter_map(|video_id| {
                        let record = newest_record(
                            self.doc
                                .get_all(&entries, video_id.as_str())
                                .ok()?
                                .into_iter()
                                .filter_map(|(value, _)| value.as_str().map(str::to_owned))
                                .filter_map(|value| {
                                    serde_json::from_str::<HistoryEntryRecord>(&value).ok()
                                }),
                        )?;
                        (record.present && record.watched_at_ms > cleared_at_ms)
                            .then_some((video_id, record))
                    })
                    .collect()
            });
        entries.sort_by(|(left_id, left), (right_id, right)| {
            right
                .watched_at_ms
                .cmp(&left.watched_at_ms)
                .then_with(|| left_id.cmp(right_id))
        });
        entries.truncate(MAX_HISTORY_ENTRIES);
        let mut snapshot = AutoCommit::new();
        for (video_id, record) in entries {
            OwnerHistoryAction::SetEntry {
                video_id,
                watched_at_ms: record.watched_at_ms,
                present: true,
                mutation_id: record.mutation_id,
            }
            .apply(&mut snapshot)
            .map_err(std::io::Error::other)?;
        }
        if cleared_at_ms > 0 {
            OwnerHistoryAction::Clear {
                cleared_at_ms,
                clear_id: format!("compacted-clear:{cleared_at_ms}"),
            }
            .apply(&mut snapshot)
            .map_err(std::io::Error::other)?;
        }
        if migration_marker {
            OwnerHistoryAction::LegacyMigrationComplete
                .apply(&mut snapshot)
                .map_err(std::io::Error::other)?;
        }
        snapshot.commit();
        self.doc = snapshot;
        Ok(())
    }

    #[cfg(test)]
    fn force_compact_for_test(&mut self) -> std::io::Result<()> {
        self.compact_snapshot()
    }

    #[cfg(test)]
    pub(crate) fn recovery_disabled_for_test() -> Self {
        Self {
            doc: AutoCommit::new(),
            path: None,
            recovery_disabled: true,
        }
    }
}

fn newest_record(records: impl Iterator<Item = HistoryEntryRecord>) -> Option<HistoryEntryRecord> {
    records.max_by(|left, right| {
        left.watched_at_ms
            .cmp(&right.watched_at_ms)
            .then_with(|| left.mutation_id.cmp(&right.mutation_id))
    })
}

/// A cryptographic full-length identifier makes profile path aliases infeasible.
fn owner_file_id(owner_id: &str) -> String {
    blake3::hash(owner_id.as_bytes()).to_hex().to_string()
}

fn current_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and only reads the calling
    // process's credential, which is exactly the ownership boundary checked
    // before tightening an existing history file.
    unsafe { libc::geteuid() }
}

fn read_private_document(path: &Path) -> std::io::Result<AutoCommit> {
    // Never follow a history-file symlink. Check the opened descriptor too,
    // so a rename between metadata and open cannot turn into a privileged read.
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::other(
            "history target is not a regular file",
        ));
    }
    if metadata.uid() != current_uid() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "history file is not owned by the current user",
        ));
    }
    if metadata.len() > MAX_HISTORY_BYTES {
        return Err(std::io::Error::other("history file exceeds size limit"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if !opened.file_type().is_file()
        || opened.len() > MAX_HISTORY_BYTES
        || opened.uid() != current_uid()
        || opened.dev() != metadata.dev()
        || opened.ino() != metadata.ino()
    {
        return Err(std::io::Error::other(
            "history target changed while opening",
        ));
    }
    if opened.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "could not enforce owner-only history file permissions",
            ));
        }
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(opened.len()).map_err(std::io::Error::other)?);
    file.take(MAX_HISTORY_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(std::io::Error::other)? > MAX_HISTORY_BYTES {
        return Err(std::io::Error::other("history read exceeds size limit"));
    }
    AutoCommit::load(&bytes).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn in_memory_store_is_a_separate_document() {
        let mut store = OwnerHistoryStore::memory();
        let mut host = ui::ActionHost::new();
        assert!(store.dispatch(
            &mut host,
            &OwnerHistoryAction::SetEntry {
                video_id: "not-in-shared-doc".into(),
                watched_at_ms: 1,
                present: true,
                mutation_id: "watch:1".into(),
            }
        ));
        let shared = AutoCommit::new();
        assert!(shared.get(&ROOT, HISTORY_ROOT).unwrap().is_none());
        assert!(store.document().get(&ROOT, HISTORY_ROOT).unwrap().is_some());
    }

    #[test]
    fn persisted_store_reopens_with_owner_only_permissions() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("alice.automerge");
        let mut store = OwnerHistoryStore::open_path(path.clone());
        let mut host = ui::ActionHost::new();
        assert!(store.dispatch(
            &mut host,
            &OwnerHistoryAction::SetEntry {
                video_id: "restart-video".into(),
                watched_at_ms: 1,
                present: true,
                mutation_id: "watch:1".into(),
            }
        ));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let reopened = OwnerHistoryStore::open_path(path);
        assert!(
            reopened
                .document()
                .get(&ROOT, HISTORY_ROOT)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn unsafe_existing_file_or_symlink_is_not_overwritten() {
        let temp = TempDir::new().unwrap();
        let unsafe_path = temp.path().join("unsafe.automerge");
        fs::write(&unsafe_path, b"not an automerge document").unwrap();
        fs::set_permissions(&unsafe_path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut unsafe_store = OwnerHistoryStore::open_path(unsafe_path.clone());
        let mut host = ui::ActionHost::new();
        assert!(unsafe_store.recovery_disabled());
        assert!(!unsafe_store.dispatch(
            &mut host,
            &OwnerHistoryAction::SetEntry {
                video_id: "must-not-overwrite".into(),
                watched_at_ms: 1,
                present: true,
                mutation_id: "x".into(),
            }
        ));
        assert_eq!(
            fs::read(&unsafe_path).unwrap(),
            b"not an automerge document"
        );

        let target = temp.path().join("target.automerge");
        fs::write(&target, b"keep-the-target").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("link.automerge");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut linked_store = OwnerHistoryStore::open_path(link);
        assert!(linked_store.recovery_disabled());
        assert!(!linked_store.dispatch(
            &mut host,
            &OwnerHistoryAction::SetEntry {
                video_id: "must-not-follow".into(),
                watched_at_ms: 1,
                present: true,
                mutation_id: "y".into(),
            }
        ));
        assert_eq!(fs::read(&target).unwrap(), b"keep-the-target");
    }

    #[test]
    fn owned_broad_permissions_are_safely_tightened_before_loading() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("broad.automerge");
        fs::write(&path, AutoCommit::new().save()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let store = OwnerHistoryStore::open_path(path.clone());
        assert!(!store.recovery_disabled());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn owner_names_do_not_alias_for_distinct_or_adversarial_ids() {
        assert_ne!(owner_file_id("user:alice"), owner_file_id("user:bob"));
        assert_ne!(owner_file_id("AaAaAaAa"), owner_file_id("BBBBBBBB"));
        assert_eq!(owner_file_id("user:alice").len(), 64);
    }

    #[test]
    fn compaction_keeps_bounded_visible_history_and_migration_on_restart() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("compact.automerge");
        let mut store = OwnerHistoryStore::open_path(path.clone());
        let mut host = ui::ActionHost::new();
        assert!(store.dispatch(&mut host, &OwnerHistoryAction::LegacyMigrationComplete));
        for watched_at_ms in 0..55_u64 {
            assert!(store.dispatch(
                &mut host,
                &OwnerHistoryAction::SetEntry {
                    video_id: format!("video-{watched_at_ms}"),
                    watched_at_ms,
                    present: true,
                    mutation_id: format!("watch-{watched_at_ms}"),
                }
            ));
        }
        store.force_compact_for_test().unwrap();
        store.persist().unwrap();
        let reopened = OwnerHistoryStore::open_path(path);
        assert!(reopened.has_legacy_migration_marker());
        let (_, history) = reopened
            .document()
            .get(&ROOT, HISTORY_ROOT)
            .unwrap()
            .unwrap();
        let (_, entries) = reopened
            .document()
            .get(&history, "entries")
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened.document().keys(&entries).count(),
            MAX_HISTORY_ENTRIES
        );
        assert!(
            reopened
                .document()
                .get(&entries, "video-54")
                .unwrap()
                .is_some()
        );
        assert!(
            reopened
                .document()
                .get(&entries, "video-0")
                .unwrap()
                .is_none()
        );
    }
}
