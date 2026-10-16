//! Environment records, opened where Terminal keeps its own.
//!
//! An environment is a Terminal record (`#[resource(kind = "environment")]`,
//! see `tilo-environment`), so it lives in Terminal's personal room and syncs
//! with Terminal's records: `<data home>/tilo-rooms/personal/so.tilo.terminal`
//! is the directory the launcher gives Terminal, and its `.local/share` is the
//! data home Terminal's own record store writes under. The supervisor joins
//! the record's shared writers there, applies actions through the record
//! layer — so every change carries history and an author — and appends.
//!
//! An app names a record by key only ([`tilo_environment::is_record_key`]);
//! the room and the owning app are fixed here, never taken from a request.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use app_state::{DocStore, FsStore};
use durable_store::{DurableStore, StorageKey};
use tilo_environment::{DEFAULT_NAME, DEFAULT_RECORD, Environment, OWNER_APP_ID, is_record_key};
use tilo_resource::{Invoke, Replica, Resource};

use crate::images::Images;

/// The room environment records are kept in.
pub const ROOM: &str = "personal";

/// The author every change the supervisor makes is attributed to.
pub const AUTHOR: &str = tilo_environment::RUNTIME_APP_ID;

/// The data home Terminal's records are written under, for a user whose data
/// home is `data_home`.
pub fn terminal_data_home(data_home: &Path) -> PathBuf {
    data_home
        .join("tilo-rooms")
        .join(ROOM)
        .join(OWNER_APP_ID)
        .join(".local/share")
}

/// Why a record could not be read or changed.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("{0:?} does not name an environment record")]
    BadKey(String),
    #[error("there is no environment {0:?}")]
    Missing(String),
    #[error("the environment record could not be stored: {0}")]
    Store(String),
    #[error("the environment record is unreadable: {0}")]
    Model(String),
    #[error("{0}")]
    Refused(String),
    #[error("no environment can be created: {0}")]
    NoRuntime(String),
}

/// The environment records under one data home.
#[derive(Debug, Clone)]
pub struct Records {
    root: PathBuf,
}

impl Records {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory one record's document lives in.
    ///
    /// # Errors
    /// Returns [`RecordError::BadKey`] for a key that is not a record key.
    pub fn record_dir(&self, key: &str) -> Result<PathBuf, RecordError> {
        check_key(key)?;
        DocStore::room_record_dir_at(&self.root, OWNER_APP_ID, ROOM, key)
            .map_err(|e| RecordError::Store(e.to_string()))
    }

    fn open(&self, key: &str) -> Result<DocStore, RecordError> {
        check_key(key)?;
        std::fs::create_dir_all(&self.root).map_err(|e| RecordError::Store(e.to_string()))?;
        let store: Rc<dyn DurableStore> = Rc::new(FsStore::new(self.root.clone()));
        DocStore::shared(store, StorageKey::record(OWNER_APP_ID, ROOM, key))
            .map_err(|e| RecordError::Store(e.to_string()))
    }

    /// The environment `key`, or `None` when no record has been made.
    ///
    /// # Errors
    /// Returns an error for a bad key, or a stored record that cannot be read.
    pub fn load(&self, key: &str) -> Result<Option<Environment>, RecordError> {
        let mut store = self.open(key)?;
        let Some(doc) = store
            .load()
            .map_err(|e| RecordError::Store(e.to_string()))?
        else {
            return Ok(None);
        };
        Environment::read(&doc)
            .map(Some)
            .map_err(|e| RecordError::Model(e.to_string()))
    }

    /// Make the record `key` holding `env`, unless one exists; returns what
    /// the record holds afterwards.
    ///
    /// # Errors
    /// Returns an error for a bad key or a store that refuses the write.
    pub fn create(&self, key: &str, env: &Environment) -> Result<Environment, RecordError> {
        let mut store = self.open(key)?;
        if let Some(doc) = store
            .load()
            .map_err(|e| RecordError::Store(e.to_string()))?
        {
            return Environment::read(&doc).map_err(|e| RecordError::Model(e.to_string()));
        }
        let mut doc = store.new_document();
        env.write_record(&mut doc)
            .map_err(|e| RecordError::Model(e.to_string()))?;
        doc.commit();
        store
            .append(&mut doc)
            .map_err(|e| RecordError::Store(e.to_string()))?;
        Ok(env.clone())
    }

    /// Every environment record there is, by key.
    ///
    /// # Errors
    /// Returns an error when the record directory cannot be read.
    pub fn keys(&self) -> Result<Vec<String>, RecordError> {
        let dir = self.record_dir(DEFAULT_RECORD)?;
        let Some(room) = dir.parent() else {
            return Ok(Vec::new());
        };
        let entries = match std::fs::read_dir(room) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(RecordError::Store(e.to_string())),
        };
        let mut keys: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|key| is_record_key(key))
            .filter(|key| self.load(key).ok().flatten().is_some())
            .collect();
        keys.sort();
        Ok(keys)
    }

    /// Whether [`Self::resolve`] of `key` would succeed, without creating the
    /// default environment: the supervisor answers an open with this before
    /// it hands out a terminal, and leaves the first write to the session.
    ///
    /// # Errors
    /// What [`Self::resolve`] would return, except a failure to write.
    pub fn resolvable(&self, key: Option<&str>, images: &dyn Images) -> Result<(), RecordError> {
        let key = key.unwrap_or(DEFAULT_RECORD);
        if self.load(key)?.is_some() {
            return Ok(());
        }
        if key != DEFAULT_RECORD {
            return Err(RecordError::Missing(key.to_owned()));
        }
        images
            .current_runtime()
            .map(drop)
            .map_err(RecordError::NoRuntime)
    }

    /// The environment a session names, creating the default environment on
    /// first use with the system store's current `env-runtime`. Any other
    /// missing record is an error: only the default comes into being by
    /// being asked for.
    ///
    /// # Errors
    /// Returns an error when the record is missing (and not the default), no
    /// runtime is installed to create the default with, or the store fails.
    pub fn resolve(
        &self,
        key: Option<&str>,
        images: &dyn Images,
    ) -> Result<(String, Environment), RecordError> {
        let key = key.unwrap_or(DEFAULT_RECORD);
        if let Some(env) = self.load(key)? {
            return Ok((key.to_owned(), env));
        }
        if key != DEFAULT_RECORD {
            return Err(RecordError::Missing(key.to_owned()));
        }
        let (release, runtime) = images.current_runtime().map_err(RecordError::NoRuntime)?;
        let env = self.create(
            key,
            &Environment::new(
                DEFAULT_NAME,
                &release,
                tilo_environment::host_arch(),
                &runtime,
            ),
        )?;
        Ok((key.to_owned(), env))
    }

    /// Apply one action to the record `key` and return the environment as it
    /// stands after it.
    ///
    /// # Errors
    /// Returns the action's refusal, or an error reading or writing the record.
    pub fn apply(
        &self,
        key: &str,
        invoke: Invoke<Environment>,
    ) -> Result<Environment, RecordError> {
        let mut store = self.open(key)?;
        let doc = store
            .load()
            .map_err(|e| RecordError::Store(e.to_string()))?
            .ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        let mut replica = Replica::<Environment>::from_doc(doc).as_author(AUTHOR);
        replica.set_now_ms(tilo_clock::unix_now_ms_i64());
        // The person asked for this at the shell: that is the confirmation.
        replica
            .apply_confirmed(invoke)
            .map_err(|e| RecordError::Refused(e.to_string()))?;
        store
            .append(replica.doc_mut())
            .map_err(|e| RecordError::Store(e.to_string()))?;
        replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))
    }
}

fn check_key(key: &str) -> Result<(), RecordError> {
    if is_record_key(key) {
        Ok(())
    } else {
        Err(RecordError::BadKey(key.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_environment::PackageId;

    struct OneRuntime(Option<PackageId>);
    impl Images for OneRuntime {
        fn tree(&self, _id: &PackageId) -> Result<PathBuf, String> {
            Err("no trees in this test".into())
        }
        fn check_tool(&self, _tool: &PackageId, _runtime: &PackageId) -> Result<(), String> {
            Ok(())
        }
        fn current_runtime(&self) -> Result<(String, PackageId), String> {
            self.0
                .map(|id| ("2026.10".to_owned(), id))
                .ok_or_else(|| "no env-runtime is installed".into())
        }
    }

    fn image(byte: u8) -> PackageId {
        PackageId::from_bytes([byte; 32])
    }

    #[test]
    fn the_default_environment_is_created_on_first_use_and_only_it() {
        let dir = tempfile::tempdir().unwrap();
        let records = Records::at(dir.path());

        let err = records.resolve(None, &OneRuntime(None)).unwrap_err();
        assert!(matches!(err, RecordError::NoRuntime(_)), "{err}");

        let (key, env) = records.resolve(None, &OneRuntime(Some(image(1)))).unwrap();
        assert_eq!(key, DEFAULT_RECORD);
        assert_eq!(
            env.runtime_image(tilo_environment::host_arch()).unwrap(),
            image(1)
        );
        assert_eq!(env.runtime.release, "2026.10");
        assert_eq!(env.name, DEFAULT_NAME);
        assert!(
            records
                .record_dir(DEFAULT_RECORD)
                .unwrap()
                .join("state.automerge")
                .is_file(),
            "the record is a Terminal record document"
        );
        assert!(
            records
                .record_dir(DEFAULT_RECORD)
                .unwrap()
                .starts_with(dir.path().join(OWNER_APP_ID).join("rooms").join(ROOM)),
        );

        // A second resolve reads the record, not the current runtime.
        let (_, again) = records.resolve(None, &OneRuntime(Some(image(2)))).unwrap();
        assert_eq!(
            again.runtime_image(tilo_environment::host_arch()).unwrap(),
            image(1)
        );

        assert!(matches!(
            records.resolve(Some("work"), &OneRuntime(Some(image(1)))),
            Err(RecordError::Missing(_))
        ));
        assert!(matches!(
            records.resolve(Some("../x"), &OneRuntime(Some(image(1)))),
            Err(RecordError::BadKey(_))
        ));
    }

    /// The supervisor's check before it hands out a terminal answers as
    /// `resolve` would, but leaves the default's creation to the session.
    #[test]
    fn whether_an_environment_resolves_is_answered_without_creating_it() {
        let dir = tempfile::tempdir().unwrap();
        let records = Records::at(dir.path());

        assert!(matches!(
            records.resolvable(None, &OneRuntime(None)),
            Err(RecordError::NoRuntime(_))
        ));
        records
            .resolvable(None, &OneRuntime(Some(image(1))))
            .unwrap();
        assert!(
            records.load(DEFAULT_RECORD).unwrap().is_none(),
            "answering made the default record"
        );

        records.resolve(None, &OneRuntime(Some(image(1)))).unwrap();
        // An existing record resolves whatever the device's runtime is now.
        records.resolvable(None, &OneRuntime(None)).unwrap();
        assert!(matches!(
            records.resolvable(Some("work"), &OneRuntime(Some(image(1)))),
            Err(RecordError::Missing(_))
        ));
        assert!(matches!(
            records.resolvable(Some("../x"), &OneRuntime(Some(image(1)))),
            Err(RecordError::BadKey(_))
        ));
    }

    #[test]
    fn actions_change_the_stored_record_and_refusals_change_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let records = Records::at(dir.path());
        records.resolve(None, &OneRuntime(Some(image(1)))).unwrap();

        let env = records
            .apply(
                DEFAULT_RECORD,
                Environment::ADD_TOOL.with((
                    vec!["pandoc".into()],
                    tilo_environment::host_arch().to_owned(),
                    image(2).to_string(),
                )),
            )
            .unwrap();
        assert_eq!(env.tools.len(), 1);
        records
            .apply(
                DEFAULT_RECORD,
                Environment::GRANT_DIR.with(("/var/home/you/src".into(), true)),
            )
            .unwrap();

        let err = records
            .apply(
                DEFAULT_RECORD,
                Environment::GRANT_DIR.with(("/usr".into(), true)),
            )
            .unwrap_err();
        assert!(matches!(err, RecordError::Refused(_)), "{err}");

        let stored = records.load(DEFAULT_RECORD).unwrap().unwrap();
        assert_eq!(stored.tools[0].packages, vec!["pandoc".to_string()]);
        assert_eq!(stored.grants.0.len(), 1);
        assert!(matches!(
            records.apply("absent", Environment::RENAME.with("x")),
            Err(RecordError::Missing(_))
        ));
    }
}
