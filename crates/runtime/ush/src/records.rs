//! Environment records, opened where Terminal keeps its own.
//!
//! An environment is a Terminal record, so it lives in Terminal's record store and syncs
//! with them: `<data home>/tilo-rooms/<workspace>/so.tilo.terminal`, whose `.local/share`
//! is the data home Terminal's store writes under. The supervisor joins the record's
//! shared writers there and applies actions through the record layer, so every change
//! carries history and an author.
//!
//! Each workspace's environments are in its own room ([`crate::workspace`]). An app names
//! a record by key only ([`tilo_environment::is_record_key`]); the room and the owning app
//! are fixed here, never taken from a request.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use app_state::{DocStore, FsStore};
use collab_types::caps::CapOverride;
use durable_store::{DurableStore, StorageKey};
use tilo_environment::{DEFAULT_NAME, DEFAULT_RECORD, Environment, OWNER_APP_ID, is_record_key};
use tilo_resource::{Invoke, Replica, Resource};

use crate::images::Images;
use crate::workspace::Confinement;

pub const ROOM: &str = crate::workspace::PRIMARY;

pub const ROOMS_DIR: &str = "tilo-rooms";

pub const AUTHOR: &str = tilo_environment::RUNTIME_APP_ID;

pub fn terminal_data_home(data_home: &Path, room: &str) -> PathBuf {
    data_home
        .join(ROOMS_DIR)
        .join(room)
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

#[derive(Debug, Clone)]
pub struct Records {
    root: PathBuf,
    room: String,
    confinement: Option<Confinement>,
}

impl Records {
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            room: ROOM.to_owned(),
            confinement: None,
        }
    }

    /// The records of the workspace `store` is of
    /// ([`Store::for_workspace`](crate::store::Store::for_workspace)), held
    /// to its confinement: a grant of another workspace's files is refused,
    /// and one that reached the record anyway is not read.
    pub fn of(store: &crate::store::Store) -> Self {
        Self {
            root: store.records_root().to_path_buf(),
            room: store.workspace_id().as_str().to_owned(),
            confinement: store.confinement(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn room(&self) -> &str {
        &self.room
    }

    /// Refuse what `after` was granted that `before` was not and the
    /// confinement does not admit.
    fn confined(&self, before: &[CapOverride], after: &[CapOverride]) -> Result<(), RecordError> {
        let Some(confinement) = &self.confinement else {
            return Ok(());
        };
        match after
            .iter()
            .filter(|grant| !before.contains(grant))
            .find(|grant| !confinement.admits(grant))
        {
            Some(CapOverride {
                scope: collab_types::caps::Scope::FsPath(path),
                ..
            }) => Err(RecordError::Refused(confinement.refusal(path))),
            Some(_) => Err(RecordError::Refused(
                "that grant is not this workspace's".into(),
            )),
            None => Ok(()),
        }
    }

    /// The directory one record's document lives in.
    ///
    /// # Errors
    /// Returns [`RecordError::BadKey`] for a key that is not a record key.
    pub fn record_dir(&self, key: &str) -> Result<PathBuf, RecordError> {
        check_key(key)?;
        DocStore::workspace_record_dir_at(&self.root, OWNER_APP_ID, &self.room, key)
            .map_err(|e| RecordError::Store(e.to_string()))
    }

    fn open(&self, key: &str) -> Result<DocStore, RecordError> {
        check_key(key)?;
        std::fs::create_dir_all(&self.root).map_err(|e| RecordError::Store(e.to_string()))?;
        let store: Rc<dyn DurableStore> = Rc::new(FsStore::new(self.root.clone()));
        DocStore::shared(store, StorageKey::record(OWNER_APP_ID, &self.room, key))
            .map_err(|e| RecordError::Store(e.to_string()))
    }

    fn load_current(store: &mut DocStore) -> Result<Option<tilo_resource::Doc>, RecordError> {
        let Some(mut doc) = store
            .load()
            .map_err(|e| RecordError::Store(e.to_string()))?
        else {
            return Ok(None);
        };
        if tilo_resource::evolution::migrate::<Environment>(&mut doc)
            .map_err(|e| RecordError::Model(e.to_string()))?
        {
            store
                .append(&mut doc)
                .map_err(|e| RecordError::Store(e.to_string()))?;
        }
        Ok(Some(doc))
    }

    pub fn load(&self, key: &str) -> Result<Option<Environment>, RecordError> {
        let mut store = self.open(key)?;
        let Some(doc) = Self::load_current(&mut store)? else {
            return Ok(None);
        };
        let mut environment =
            Environment::read(&doc).map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_room(key, &environment)?;
        // A grant this room may not hold — written by a Tilo that did not
        // refuse it — is not read, so no session is built with it.
        if let Some(confinement) = &self.confinement {
            environment.grants.retain(|grant| confinement.admits(grant));
        }
        Ok(Some(environment))
    }

    /// A record kept in this room that names another workspace is not this
    /// room's to open. One that names none was made before environments
    /// belonged to a workspace, and is the room's.
    fn check_room(&self, key: &str, environment: &Environment) -> Result<(), RecordError> {
        let named =
            crate::workspace::WorkspaceId::of_record(environment).map_err(RecordError::Model)?;
        if environment.workspace.is_empty() || named.as_str() == self.room {
            Ok(())
        } else {
            Err(RecordError::Refused(format!(
                "the environment {key:?} kept with workspace {}'s belongs to workspace {named}",
                self.room
            )))
        }
    }

    /// Make the record `key` holding `env`, unless one exists; returns what
    /// the record holds afterwards. The record is made as this room's: it
    /// names the room's workspace.
    ///
    /// # Errors
    /// Returns an error for a bad key, a grant the room may not hold, or a
    /// store that refuses the write.
    pub fn create(&self, key: &str, env: &Environment) -> Result<Environment, RecordError> {
        let mut store = self.open(key)?;
        if Self::load_current(&mut store)?.is_some() {
            drop(store);
            return self
                .load(key)?
                .ok_or_else(|| RecordError::Missing(key.to_owned()));
        }
        let env = Environment {
            workspace: self.room.clone(),
            ..env.clone()
        };
        self.confined(&[], &env.grants)?;
        let mut doc = store.new_document();
        env.write_record(&mut doc)
            .map_err(|e| RecordError::Model(e.to_string()))?;
        doc.commit();
        store
            .append(&mut doc)
            .map_err(|e| RecordError::Store(e.to_string()))?;
        Ok(env)
    }

    /// Make the environment `key` of these records one of `into`'s, as a branch of this room's
    /// workspace is made: the record's whole history, forked under a writer of its own, with one
    /// more change naming `into`'s workspace as its own and its home and grants of this workspace's
    /// files as the same places in `into`'s. `rehome` names the path a path of this workspace's
    /// files takes in `into`'s, `None` for one outside them. File actions not carried out yet stay
    /// with the record they were asked of. Answers whether there was a record to branch.
    ///
    /// # Errors
    /// A bad key, a record `into` already holds, a grant `into` may not hold, or a store that
    /// refuses the write.
    pub fn branch_into(
        &self,
        key: &str,
        into: &Self,
        rehome: &dyn Fn(&str) -> Option<String>,
    ) -> Result<bool, RecordError> {
        let mut source = self.open(key)?;
        let Some(mut held) = Self::load_current(&mut source)? else {
            return Ok(false);
        };
        drop(source);
        let mut target = into.open(key)?;
        if target
            .load()
            .map_err(|e| RecordError::Store(e.to_string()))?
            .is_some()
        {
            return Err(RecordError::Refused(format!(
                "workspace {} has an environment {key:?} already",
                into.room
            )));
        }
        let mut doc = held.fork();
        let mut environment =
            Environment::read(&doc).map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_room(key, &environment)?;
        environment.workspace.clone_from(&into.room);
        if let Some(home) = rehome(&environment.home_dir) {
            environment.home_dir = home;
        }
        for grant in &mut environment.grants {
            let collab_types::caps::Scope::FsPath(path) = &grant.scope else {
                continue;
            };
            let Some(moved) = rehome(path) else {
                continue;
            };
            let write = grant.access == collab_types::caps::Access::Write;
            let made = tilo_environment::dir_grant(Path::new(&moved), write)
                .map_err(RecordError::Refused)?;
            grant.scope = made.scope;
            grant.id = made.id;
        }
        environment.files = tilo_environment::Environment::default().files;
        into.confined(&[], &environment.grants)?;
        environment
            .write_record(&mut doc)
            .map_err(|e| RecordError::Model(e.to_string()))?;
        doc.commit();
        target
            .append(&mut doc)
            .map_err(|e| RecordError::Store(e.to_string()))?;
        Ok(true)
    }

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

    pub fn apply(
        &self,
        key: &str,
        invoke: Invoke<Environment>,
    ) -> Result<Environment, RecordError> {
        let mut store = self.open(key)?;
        let doc =
            Self::load_current(&mut store)?.ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        let mut replica = Replica::<Environment>::from_doc(doc).as_author(AUTHOR);
        replica.set_now_ms(tilo_clock::unix_now_ms_i64());
        let before = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_room(key, &before)?;
        replica
            .apply_confirmed(invoke)
            .map_err(|e| RecordError::Refused(e.to_string()))?;
        let mut after = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.confined(&before.grants, &after.grants)?;
        store
            .append(replica.doc_mut())
            .map_err(|e| RecordError::Store(e.to_string()))?;
        if let Some(confinement) = &self.confinement {
            after.grants.retain(|grant| confinement.admits(grant));
        }
        Ok(after)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invoker {
    Person(String),
    Agent { agent: String, person: String },
}

impl Records {
    /// Open the record `key` as `invoker`, let `act` apply its actions to
    /// it, and append what they changed: one write, so a reader never sees
    /// half of what `act` did.
    ///
    /// # Errors
    /// What `act` returns (nothing is appended then), or an error reading
    /// or writing the record.
    pub fn act<T>(
        &self,
        key: &str,
        invoker: &Invoker,
        act: impl FnOnce(&mut Replica<Environment>) -> Result<T, RecordError>,
    ) -> Result<T, RecordError> {
        self.act_within(key, invoker, None, act)
    }

    pub fn act_within<T>(
        &self,
        key: &str,
        invoker: &Invoker,
        run: Option<&crate::broker::RunGroup>,
        act: impl FnOnce(&mut Replica<Environment>) -> Result<T, RecordError>,
    ) -> Result<T, RecordError> {
        let mut store = self.open(key)?;
        let doc =
            Self::load_current(&mut store)?.ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        let replica = Replica::<Environment>::from_doc(doc);
        let replica = match invoker {
            Invoker::Person(person) => replica.as_author(person),
            Invoker::Agent { agent, person } => replica.as_agent_for(agent, person),
        };
        let mut replica = match run {
            Some(run) => replica.within_group(&run.id, &run.label, crate::broker::COMMAND_RUN),
            None => replica,
        };
        replica.set_now_ms(tilo_clock::unix_now_ms_i64());
        let before = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_room(key, &before)?;
        let answer = act(&mut replica)?;
        if self.confinement.is_some() {
            let after = replica
                .state()
                .map_err(|e| RecordError::Model(e.to_string()))?;
            self.confined(&before.grants, &after.grants)?;
        }
        store
            .append(replica.doc_mut())
            .map_err(|e| RecordError::Store(e.to_string()))?;
        Ok(answer)
    }

    pub fn history(&self, key: &str) -> Result<Vec<tilo_resource::history::Version>, RecordError> {
        let mut store = self.open(key)?;
        let mut doc =
            Self::load_current(&mut store)?.ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        Ok(tilo_resource::history::versions(&mut doc))
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
        assert_eq!(stored.grants.len(), 1);
        assert!(matches!(
            records.apply("absent", Environment::RENAME.with("x")),
            Err(RecordError::Missing(_))
        ));
    }

    const W1: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    const W2: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

    fn workspace_store(root: &Path, files: &Path, id: &str) -> crate::store::Store {
        crate::store::Store::at(root)
            .with_workspace(files)
            .for_workspace(&crate::workspace::WorkspaceId::parse(Some(id)).unwrap())
    }

    #[test]
    fn a_workspace_keeps_its_environments_in_its_own_room() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        let w1 = Records::of(&workspace_store(&dir.path().join("ush"), &files, W1));
        let w2 = Records::of(&workspace_store(&dir.path().join("ush"), &files, W2));
        assert_eq!(w1.room(), W1);

        let (key, env) = w1.resolve(None, &OneRuntime(Some(image(1)))).unwrap();
        assert_eq!(env.workspace, W1, "the record names its workspace");
        assert!(
            w1.record_dir(&key)
                .unwrap()
                .starts_with(w1.root().join(OWNER_APP_ID).join("rooms").join(W1)),
            "in the workspace's room"
        );
        w1.create("env-only-w1", &env).unwrap();
        assert_eq!(w1.keys().unwrap(), vec!["default", "env-only-w1"]);
        assert!(
            w2.keys().unwrap().is_empty(),
            "another room lists none of them"
        );
        assert!(w2.load("env-only-w1").unwrap().is_none());
        assert!(
            Records::at(w1.root()).keys().unwrap().is_empty(),
            "nor does the primary workspace's"
        );

        // A record moved into another workspace's room is not its to open.
        let w1_dir = w1.record_dir("env-only-w1").unwrap();
        let w2_dir = w2.record_dir("env-only-w1").unwrap();
        std::fs::remove_dir_all(&w2_dir).unwrap();
        std::fs::rename(&w1_dir, &w2_dir).unwrap();
        assert!(matches!(
            w2.load("env-only-w1"),
            Err(RecordError::Refused(_))
        ));
        assert!(matches!(
            w2.apply("env-only-w1", Environment::RENAME.with("mine")),
            Err(RecordError::Refused(_))
        ));
    }

    #[test]
    fn an_environment_from_before_workspaces_is_the_primary_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        let store = crate::store::Store::at(dir.path().join("ush")).with_workspace(&files);
        let legacy = Records::at(store.records_root());
        let mut old = Environment::new("Old", "2026.10", tilo_environment::host_arch(), &image(1));
        old.grants
            .push(tilo_environment::dir_grant(Path::new("/var/home/you/src"), true).unwrap());
        let mut doc_store = legacy.open(DEFAULT_RECORD).unwrap();
        let mut doc = doc_store.new_document();
        old.write_record(&mut doc).unwrap();
        doc.commit();
        doc_store.append(&mut doc).unwrap();
        drop(doc_store);

        let primary = Records::of(&store);
        let (key, env) = primary.resolve(None, &OneRuntime(Some(image(2)))).unwrap();
        assert_eq!((key.as_str(), env.name.as_str()), (DEFAULT_RECORD, "Old"));
        assert_eq!(env.workspace, "", "unchanged");
        assert_eq!(env.grants, old.grants, "everything it held");
        assert!(
            crate::workspace::WorkspaceId::of_record(&env)
                .unwrap()
                .is_primary()
        );
        primary
            .apply(DEFAULT_RECORD, Environment::RENAME.with("Still mine"))
            .unwrap();

        let w1 = Records::of(&workspace_store(&dir.path().join("ush"), &files, W1));
        assert!(w1.keys().unwrap().is_empty());
        let (_, fresh) = w1.resolve(None, &OneRuntime(Some(image(2)))).unwrap();
        assert_eq!(fresh.name, DEFAULT_NAME, "a workspace's default is its own");
        assert_eq!(
            primary.load(DEFAULT_RECORD).unwrap().unwrap().name,
            "Still mine"
        );
    }

    #[test]
    fn a_workspace_environment_is_refused_another_workspaces_files() {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        let tree = |id: &str| files.join(".workspaces").join(id);
        let store = workspace_store(&dir.path().join("ush"), &files, W1);
        let records = Records::of(&store);
        records.resolve(None, &OneRuntime(Some(image(1)))).unwrap();
        let grant =
            |path: &Path| Environment::GRANT_DIR.with((path.to_string_lossy().into_owned(), true));

        let own = records
            .apply(DEFAULT_RECORD, grant(&tree(W1).join("src")))
            .unwrap();
        assert_eq!(own.grants.len(), 1);
        for refused in [
            tree(W2),
            tree(W2).join("src"),
            files.clone(),
            files.join("Documents"),
        ] {
            let err = records.apply(DEFAULT_RECORD, grant(&refused)).unwrap_err();
            assert!(matches!(err, RecordError::Refused(_)), "{refused:?}: {err}");
        }
        let err = records
            .apply(
                DEFAULT_RECORD,
                Environment::SET_HOME.with(tree(W2).to_string_lossy().into_owned()),
            )
            .unwrap_err();
        assert!(matches!(err, RecordError::Refused(_)), "{err}");
        let err = records
            .act(DEFAULT_RECORD, &Invoker::Person("you".into()), |replica| {
                replica
                    .apply_confirmed(grant(&tree(W2)))
                    .map(drop)
                    .map_err(|e| RecordError::Refused(e.to_string()))
            })
            .unwrap_err();
        assert!(matches!(err, RecordError::Refused(_)), "{err}");
        assert_eq!(
            records.load(DEFAULT_RECORD).unwrap().unwrap().grants,
            own.grants,
            "nothing refused was kept"
        );
        records
            .apply(DEFAULT_RECORD, grant(&dir.path().join("elsewhere")))
            .unwrap();

        // Written past the check (by a Tilo without it), it is not read.
        let unchecked = Records {
            confinement: None,
            ..records.clone()
        };
        unchecked.apply(DEFAULT_RECORD, grant(&tree(W2))).unwrap();
        let read = records.load(DEFAULT_RECORD).unwrap().unwrap();
        assert!(
            read.grants.iter().all(|g| {
                !matches!(&g.scope, collab_types::caps::Scope::FsPath(p) if Path::new(p).starts_with(tree(W2)))
            }),
            "{:?}",
            read.grants
        );
        assert_eq!(read.grants.len(), 2);
    }
}
