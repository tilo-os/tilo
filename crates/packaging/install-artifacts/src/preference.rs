//! Private opaque advisory preference at Imager's compatible settings path.
use crate::{
    Error, ErrorKind, PreferenceCommit, PreferenceDurability, PreferenceFailure,
    PreferenceNamespace, PreferencePublication,
};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
pub(crate) const MAX_PREFERENCE_BYTES: usize = 4096;
pub(crate) struct PreferenceStore {
    path: Result<PathBuf, PreferenceFailure>,
    #[cfg(all(unix, any(test, feature = "test-support")))]
    hooks: Option<crate::test_support::PreferenceHooks>,
}
impl PreferenceStore {
    #[cfg(any(windows, all(target_os = "linux", not(test))))]
    pub(crate) fn live() -> Self {
        let path = dirs::data_local_dir()
            .filter(|root| root.is_absolute())
            .map(|root| root.join("qos/setup/internal-download-selection.json"))
            .ok_or_else(invalid_preference_path);
        Self {
            path,
            #[cfg(all(unix, any(test, feature = "test-support")))]
            hooks: None,
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self {
            path: Ok(path),
            #[cfg(all(unix, any(test, feature = "test-support")))]
            hooks: None,
        }
    }
    #[cfg(all(unix, any(test, feature = "test-support")))]
    pub(crate) fn with_hooks(mut self, hooks: crate::test_support::PreferenceHooks) -> Self {
        self.hooks = Some(hooks);
        self
    }
    fn path(&self) -> Result<&Path, PreferenceFailure> {
        let path = self.path.as_deref().map_err(Clone::clone)?;
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(invalid_preference_path());
        }
        Ok(path)
    }
    pub(crate) fn load(&self) -> Result<Option<Vec<u8>>, PreferenceFailure> {
        let path = self.path()?;
        let parent = path.parent().ok_or_else(invalid_preference_path)?;
        let name = path.file_name().ok_or_else(invalid_preference_path)?;
        let read = || {
            let directory = atomic_file::StorageDirectory::open(parent)?;
            directory.open_read(name, atomic_file::Privacy::Inherit)
        };
        let file = match read() {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(map_read_failure(e)),
        };
        if file.metadata().map_err(map_read_failure)?.len() > MAX_PREFERENCE_BYTES as u64 {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(512);
        file.take((MAX_PREFERENCE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(map_read_failure)?;
        Ok((bytes.len() <= MAX_PREFERENCE_BYTES).then_some(bytes))
    }
    pub(crate) fn save(&self, bytes: &[u8]) -> Result<PreferenceCommit, PreferenceFailure> {
        #[cfg(all(unix, any(test, feature = "test-support")))]
        if let Some(hooks) = &self.hooks {
            return hooks.save(|| self.save_native(bytes));
        }
        self.save_native(bytes)
    }
    fn save_native(&self, bytes: &[u8]) -> Result<PreferenceCommit, PreferenceFailure> {
        if bytes.len() > MAX_PREFERENCE_BYTES {
            return Err(PreferenceFailure {
                kind: ErrorKind::InvalidInput,
                message: "Saved download selection exceeds its size limit.".into(),
                publication: PreferencePublication::NotPublished,
                namespace: PreferenceNamespace::Unchanged,
            });
        }
        let path = self.path()?;
        #[cfg(unix)]
        {
            let parent = path.parent().ok_or_else(invalid_preference_path)?;
            let name = path.file_name().ok_or_else(invalid_preference_path)?;
            let directory = atomic_file::provision_directory(parent, atomic_file::Privacy::Inherit)
                .map_err(map_provision_failure)?;
            directory
                .validate_namespace_control()
                .map_err(map_admission_failure)?;
            directory
                .publish(
                    name,
                    bytes,
                    atomic_file::PublishMode::Replace,
                    atomic_file::Privacy::OwnerOnly,
                )
                .map_err(map_publication_failure)?;
            Ok(PreferenceCommit {
                durability: PreferenceDurability::Durable,
            })
        }
        #[cfg(windows)]
        {
            atomic_file::write_creating_parent_with_mode(path, bytes, 0o600).map_err(|error| {
                let publication = atomic_file::publication_state(&error)
                    .map(map_publication_state)
                    .unwrap_or(PreferencePublication::NotPublished);
                map_io_failure(error, publication, PreferenceNamespace::Unproven)
            })?;
            // The existing Windows wrapper cannot prove ancestor persistence.
            Ok(PreferenceCommit {
                durability: PreferenceDurability::NamespaceUnproven,
            })
        }
    }
}
fn invalid_preference_path() -> PreferenceFailure {
    PreferenceFailure {
        kind: ErrorKind::InvalidInput,
        message: "An absolute device-local settings path is unavailable.".into(),
        publication: PreferencePublication::NotPublished,
        namespace: PreferenceNamespace::Unchanged,
    }
}
fn map_io_failure(
    error: std::io::Error,
    publication: PreferencePublication,
    namespace: PreferenceNamespace,
) -> PreferenceFailure {
    let error = Error::io(error);
    PreferenceFailure {
        kind: error.kind(),
        message: error.to_string(),
        publication,
        namespace,
    }
}
fn map_read_failure(error: std::io::Error) -> PreferenceFailure {
    map_io_failure(
        error,
        PreferencePublication::NotPublished,
        PreferenceNamespace::Unchanged,
    )
}
fn map_publication_state(state: atomic_file::PublicationState) -> PreferencePublication {
    match state {
        atomic_file::PublicationState::NotPublished => PreferencePublication::NotPublished,
        atomic_file::PublicationState::PublishedDurabilityUnknown => {
            PreferencePublication::PublishedDurabilityUnknown
        }
        atomic_file::PublicationState::PublicationUnknown => {
            PreferencePublication::PublicationUnknown
        }
    }
}
#[cfg(unix)]
fn map_provision_failure(error: atomic_file::ProvisionError) -> PreferenceFailure {
    let namespace = match error.state() {
        atomic_file::DirectoryState::Unchanged => PreferenceNamespace::Unchanged,
        atomic_file::DirectoryState::CreatedDurabilityUnknown => {
            PreferenceNamespace::CreatedDurabilityUnknown
        }
        atomic_file::DirectoryState::NamespaceUnknown => PreferenceNamespace::NamespaceUnknown,
    };
    map_io_failure(
        error.into_io_error(),
        PreferencePublication::NotPublished,
        namespace,
    )
}
#[cfg(unix)]
fn map_admission_failure(error: std::io::Error) -> PreferenceFailure {
    map_io_failure(
        error,
        PreferencePublication::NotPublished,
        PreferenceNamespace::Durable,
    )
}
#[cfg(unix)]
fn map_publication_failure(error: atomic_file::PublicationError) -> PreferenceFailure {
    let publication = map_publication_state(error.state());
    map_io_failure(
        error.into_io_error(),
        publication,
        PreferenceNamespace::Durable,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use atomic_file::test_support::{Checkpoint, with_failure, with_observer};
    use std::fs;
    use std::{cell::Cell, rc::Rc};
    #[test]
    fn preference_size_refusal_has_no_storage_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/selection.json");
        let error = PreferenceStore::at(path.clone())
            .save(&vec![b'x'; 4097])
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert_eq!(error.publication, PreferencePublication::NotPublished);
        assert_eq!(error.namespace, PreferenceNamespace::Unchanged);
        assert!(!path.parent().unwrap().exists());
    }
    #[test]
    fn preference_absent_load_never_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/selection.json");
        assert_eq!(PreferenceStore::at(path.clone()).load().unwrap(), None);
        assert!(!path.parent().unwrap().exists());
    }
    #[cfg(unix)]
    #[test]
    fn preference_commit_failure_keeps_published_bytes_and_reports_uncertainty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        let store = PreferenceStore::at(path.clone());
        store.save(b"old").unwrap();
        let observed = Rc::new(Cell::new(false));
        let seen = observed.clone();
        let result = with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::BeforeCommit {
                    seen.set(true);
                }
            },
            || with_failure(Checkpoint::BeforeCommit, || store.save(b"new")),
        );
        assert!(
            observed.get(),
            "production commit checkpoint must be observed"
        );
        let failure = result.unwrap_err();
        assert_eq!(
            failure.publication,
            PreferencePublication::PublishedDurabilityUnknown
        );
        assert_eq!(failure.namespace, PreferenceNamespace::Durable);
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(PreferenceStore::at(path).load().unwrap().unwrap(), b"new");
    }
    #[cfg(unix)]
    #[test]
    fn preference_before_publish_failure_preserves_old_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = PreferenceStore::at(dir.path().join("selection.json"));
        store.save(b"old").unwrap();
        let observed = Rc::new(Cell::new(false));
        let seen = observed.clone();
        let result = with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::BeforePublish {
                    seen.set(true);
                }
            },
            || with_failure(Checkpoint::BeforePublish, || store.save(b"new")),
        );
        assert!(
            observed.get(),
            "production publication checkpoint must be observed"
        );
        let failure = result.unwrap_err();
        assert_eq!(failure.publication, PreferencePublication::NotPublished);
        assert_eq!(failure.namespace, PreferenceNamespace::Durable);
        assert_eq!(store.load().unwrap().unwrap(), b"old");
    }
    #[test]
    fn preference_zero_and_limit_bytes_are_opaque_and_oversized_old_data_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        let store = PreferenceStore::at(path.clone());
        for bytes in [vec![], vec![b'x'; 4096], b"malformed JSON".to_vec()] {
            let commit = store.save(&bytes).unwrap();
            #[cfg(unix)]
            assert_eq!(commit.durability, PreferenceDurability::Durable);
            #[cfg(windows)]
            assert_eq!(commit.durability, PreferenceDurability::NamespaceUnproven);
            assert_eq!(store.load().unwrap(), Some(bytes));
        }
        fs::write(path, vec![b'x'; 4097]).unwrap();
        assert_eq!(store.load().unwrap(), None);
    }
    #[cfg(unix)]
    #[test]
    fn preference_load_refuses_symlink_without_reading_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"secret").unwrap();
        let path = dir.path().join("selection.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(PreferenceStore::at(path).load().is_err());
        assert_eq!(fs::read(target).unwrap(), b"secret");
    }
    #[test]
    fn preference_relative_path_is_refused_before_any_effect() {
        let store = PreferenceStore::at(PathBuf::from("relative/selection.json"));
        for failure in [store.load().unwrap_err(), store.save(b"new").unwrap_err()] {
            assert_eq!(failure.kind, ErrorKind::InvalidInput);
            assert_eq!(failure.publication, PreferencePublication::NotPublished);
            assert_eq!(failure.namespace, PreferenceNamespace::Unchanged);
        }
    }
    #[cfg(unix)]
    #[test]
    fn preference_provision_failure_preserves_exact_namespace_knowledge() {
        use atomic_file::test_support::{
            MkdirFault, ProvisionCheckpoint as C, with_mkdir_result, with_provision_failure,
            with_provision_observer,
        };
        for (checkpoint, namespace) in [
            (C::BeforeCreate, PreferenceNamespace::Unchanged),
            (C::BeforeSync, PreferenceNamespace::CreatedDurabilityUnknown),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().canonicalize().unwrap().join("new");
            let store = PreferenceStore::at(parent.join("selection.json"));
            let observed = Rc::new(Cell::new(false));
            let seen = observed.clone();
            let target = parent.clone();
            let result = with_provision_observer(
                move |event| {
                    if event.path == target && event.checkpoint == checkpoint {
                        seen.set(true);
                    }
                },
                || with_provision_failure(checkpoint, &parent, || store.save(b"new")),
            );
            assert!(observed.get(), "actual provisioning checkpoint required");
            let failure = result.unwrap_err();
            assert_eq!(failure.namespace, namespace);
            assert_eq!(failure.publication, PreferencePublication::NotPublished);
            assert!(!parent.join("selection.json").exists());
        }
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().canonicalize().unwrap().join("unknown");
        let store = PreferenceStore::at(parent.join("selection.json"));
        let failure = with_mkdir_result(&parent, MkdirFault::AfterSuccess(libc::EIO), || {
            store.save(b"new")
        })
        .unwrap_err();
        assert!(
            parent.is_dir(),
            "injected lost acknowledgement must follow the real mkdir"
        );
        assert_eq!(failure.namespace, PreferenceNamespace::NamespaceUnknown);
        assert_eq!(failure.publication, PreferencePublication::NotPublished);
    }
    #[cfg(unix)]
    #[test]
    fn preference_publication_unknown_is_not_downgraded_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.json");
        let store = PreferenceStore::at(path.clone());
        let observed = Rc::new(Cell::new(false));
        let seen = observed.clone();
        let result = with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::AfterPublish {
                    seen.set(true);
                }
            },
            || with_failure(Checkpoint::AfterPublish, || store.save(b"new")),
        );
        assert!(observed.get());
        let failure = result.unwrap_err();
        assert_eq!(
            failure.publication,
            PreferencePublication::PublicationUnknown
        );
        assert_eq!(failure.namespace, PreferenceNamespace::Durable);
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }
}
