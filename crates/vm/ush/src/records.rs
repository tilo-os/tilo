//! Environment records, kept where Terminal keeps its own: as Terminal's documents in their
//! workspace's one store, which this device's workspace service holds and syncs.
//!
//! The runtime holds no connection to the workspace of its own. It asks the session, which
//! holds this device's workspace service, to read and write Terminal's documents for it
//! ([`runtime_portal::workspace_documents`]), proving that it holds the device identity. Each
//! read takes the record whole, and each change is written as the changes after what was read,
//! so two processes changing one record merge, and every change carries history and an author.
//! Without the session there is no record to read: the person is told so
//! ([`RecordError::Unreachable`]), and nothing is kept on the side.
//!
//! Each workspace has its own environments ([`crate::workspace`]). An app names a record by key
//! only ([`tilo_environment::is_record_key`]); the workspace and the owning app are fixed here,
//! never taken from a request.

use std::path::{Path, PathBuf};

use automerge::ChangeHash;
use collab_types::caps::CapOverride;
use runtime_portal::workspace_documents;
use tilo_environment::{DEFAULT_NAME, DEFAULT_RECORD, Environment, is_record_key};
use tilo_resource::{Doc, Invoke, Replica, Resource};
use tilo_resource_store::wire::{Bytes, Command, RECORD_PAGE_BYTES, Reply};

use crate::images::Images;
use crate::workspace::Confinement;

pub const PRIMARY_WORKSPACE: &str = crate::workspace::PRIMARY;

pub const AUTHOR: &str = tilo_environment::RUNTIME_APP_ID;

/// What every environment record's document id starts with: Terminal keeps documents of its own
/// beside them.
const DOCUMENT_PREFIX: &str = "environment:";

/// Why a record could not be read or changed.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("{0:?} does not name an environment record")]
    BadKey(String),
    #[error("there is no environment {0:?}")]
    Missing(String),
    #[error("the environment record could not be stored: {0}")]
    Store(String),
    #[error(
        "environments are kept in your workspace, and the Tilo session that reaches it is not \
         running on this device"
    )]
    Unreachable,
    #[error("the environment record is unreadable: {0}")]
    Model(String),
    #[error("{0}")]
    Refused(String),
    #[error("no environment can be created: {0}")]
    NoRuntime(String),
}

/// One record as the workspace holds it: its document, and the heads a change to it is written
/// after.
struct Held {
    doc: Doc,
    heads: Vec<ChangeHash>,
}

#[derive(Debug, Clone)]
pub struct Records {
    /// Where the session serves Terminal's documents.
    documents: PathBuf,
    workspace: String,
    confinement: Option<Confinement>,
}

impl Records {
    /// The primary workspace's records, served at `documents`.
    pub fn at(documents: impl Into<PathBuf>) -> Self {
        Self {
            documents: documents.into(),
            workspace: PRIMARY_WORKSPACE.to_owned(),
            confinement: None,
        }
    }

    /// The records of the workspace `store` is of
    /// ([`Store::for_workspace`](crate::store::Store::for_workspace)), held
    /// to its confinement: a grant of another workspace's files is refused,
    /// and one that reached the record anyway is not read.
    pub fn of(store: &crate::store::Store) -> Self {
        Self {
            documents: store.records_root().to_path_buf(),
            workspace: store.workspace_id().as_str().to_owned(),
            confinement: store.confinement(),
        }
    }

    /// Where the session serves Terminal's documents.
    pub fn documents(&self) -> &Path {
        &self.documents
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    fn is_primary(&self) -> bool {
        self.workspace == PRIMARY_WORKSPACE
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

    /// What this workspace's records' document ids start with. The primary workspace, which
    /// the session knows by no id, keeps its records in the workspace in front of the person,
    /// beside that workspace's own.
    fn prefix(&self) -> String {
        if self.is_primary() {
            format!("{DOCUMENT_PREFIX}{PRIMARY_WORKSPACE}:")
        } else {
            DOCUMENT_PREFIX.to_owned()
        }
    }

    /// The id of the document holding the record `key`, among Terminal's documents in the
    /// workspace.
    ///
    /// # Errors
    /// Returns [`RecordError::BadKey`] for a key that is not a record key.
    pub fn document_id(&self, key: &str) -> Result<String, RecordError> {
        check_key(key)?;
        Ok(format!("{}{key}", self.prefix()))
    }

    /// Carry out `command` on Terminal's documents in this workspace, through the session.
    fn command(&self, command: &Command) -> Result<Reply, RecordError> {
        // Outside a login there is no session to ask.
        if self.documents.as_os_str().is_empty() {
            return Err(RecordError::Unreachable);
        }
        let request = workspace_documents::Request::new(
            (!self.is_primary()).then_some(self.workspace.as_str()),
        );
        let command = serde_json::to_vec(command).map_err(|e| RecordError::Store(e.to_string()))?;
        let reply = workspace_documents::ask(&self.documents, &request, &command).map_err(
            |error| match error.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    tracing::info!(%error, session = %self.documents.display(), "ush: no session serves Terminal's documents");
                    RecordError::Unreachable
                }
                std::io::ErrorKind::PermissionDenied => RecordError::Refused(error.to_string()),
                _ => RecordError::Store(error.to_string()),
            },
        )?;
        serde_json::from_slice(&reply).map_err(|e| RecordError::Store(e.to_string()))
    }

    /// The record `key`'s changes past `after` as the workspace holds them, joined from every
    /// page, and the heads they bring it to: none of either for a record it holds nothing of.
    fn record_changes(
        &self,
        key: &str,
        after: &[[u8; 32]],
    ) -> Result<(Vec<u8>, Vec<[u8; 32]>), RecordError> {
        let record = self.document_id(key)?;
        let after: Vec<String> = after
            .iter()
            .map(|head| ChangeHash(*head).to_string())
            .collect();
        'answer: loop {
            let (mut changes, mut first, mut from) = (Vec::new(), None::<Vec<String>>, 0);
            loop {
                let reply = self.command(&Command::RecordChanges {
                    record: Some(record.clone()),
                    after: after.clone(),
                    from,
                })?;
                let Reply::RecordChanges {
                    changes: page,
                    heads,
                    next,
                    pending,
                } = reply
                else {
                    return Err(RecordError::Store(format!(
                        "the workspace answered a read with {reply:?}"
                    )));
                };
                if pending {
                    return Err(RecordError::Store(
                        "it is still on its way to this device; try again in a moment".into(),
                    ));
                }
                match &first {
                    Some(first) if *first != heads => continue 'answer,
                    Some(_) => {}
                    None => first = Some(heads),
                }
                changes.extend(page.0);
                if let Some(next) = next {
                    from = next;
                    continue;
                }
                let heads = first
                    .unwrap_or_default()
                    .iter()
                    .map(|head| head.parse::<ChangeHash>().map(|head| head.0))
                    .collect::<Result<_, _>>()
                    .map_err(|_| RecordError::Store("the workspace named no change".into()))?;
                return Ok((changes, heads));
            }
        }
    }

    fn read(&self, key: &str) -> Result<Option<Held>, RecordError> {
        let (changes, heads) = self.record_changes(key, &[])?;
        if heads.is_empty() {
            return Ok(None);
        }
        let mut doc = Doc::load(&changes).map_err(|e| RecordError::Model(e.to_string()))?;
        let heads = doc.get_heads();
        Ok(Some(Held { doc, heads }))
    }

    /// The heads the record `key` stands at, when they are not `heads`: what tells a holder of
    /// the record that it changed. `None` while it is unchanged.
    ///
    /// # Errors
    /// A bad key, or a workspace that could not be asked.
    pub fn changed_since(
        &self,
        key: &str,
        heads: &[[u8; 32]],
    ) -> Result<Option<Vec<[u8; 32]>>, RecordError> {
        let (_, now) = self.record_changes(key, heads)?;
        Ok((now.as_slice() != heads).then_some(now))
    }

    /// Write what `doc` holds after `stored`, or all of it, founding the record, with none.
    fn write(
        &self,
        key: &str,
        doc: &mut Doc,
        stored: Option<&[ChangeHash]>,
    ) -> Result<(), RecordError> {
        let changes = doc.save_after(stored.unwrap_or_default());
        if changes.is_empty() {
            return Ok(());
        }
        let record = self.document_id(key)?;
        let runs: Vec<&[u8]> = changes.chunks(RECORD_PAGE_BYTES).collect();
        for (index, run) in runs.iter().enumerate() {
            match self.command(&Command::RecordChange {
                record: Some(record.clone()),
                changes: Bytes(run.to_vec()),
                more: index < runs.len().saturating_sub(1),
                commit: None,
            })? {
                Reply::Written => {}
                other => {
                    return Err(RecordError::Store(format!(
                        "the workspace answered a write with {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn load_current(&self, key: &str) -> Result<Option<Held>, RecordError> {
        let Some(mut held) = self.read(key)? else {
            return Ok(None);
        };
        if tilo_resource::evolution::migrate::<Environment>(&mut held.doc)
            .map_err(|e| RecordError::Model(e.to_string()))?
        {
            self.write(key, &mut held.doc, Some(held.heads.as_slice()))?;
            held.heads = held.doc.get_heads();
        }
        Ok(Some(held))
    }

    pub fn load(&self, key: &str) -> Result<Option<Environment>, RecordError> {
        let Some(held) = self.load_current(key)? else {
            return Ok(None);
        };
        let mut environment =
            Environment::read(&held.doc).map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_workspace(key, &environment)?;
        // A grant this workspace may not hold, written by a Tilo that did not refuse it,
        // is not read, so no session is built with it.
        if let Some(confinement) = &self.confinement {
            environment.grants.retain(|grant| confinement.admits(grant));
        }
        Ok(Some(environment))
    }

    /// A record here that names another workspace is not this one's to open.
    /// One that names none was made before environments belonged to a workspace.
    fn check_workspace(&self, key: &str, environment: &Environment) -> Result<(), RecordError> {
        let named =
            crate::workspace::WorkspaceId::of_record(environment).map_err(RecordError::Model)?;
        if environment.workspace.is_empty() || named.as_str() == self.workspace {
            Ok(())
        } else {
            Err(RecordError::Refused(format!(
                "the environment {key:?} kept with workspace {}'s belongs to workspace {named}",
                self.workspace
            )))
        }
    }

    /// Make the record `key` holding `env`, unless one exists; returns what
    /// the record holds afterwards. The record is made as this workspace's.
    ///
    /// # Errors
    /// Returns an error for a bad key, a grant the workspace may not hold, or a
    /// store that refuses the write.
    pub fn create(&self, key: &str, env: &Environment) -> Result<Environment, RecordError> {
        if self.load_current(key)?.is_some() {
            return self
                .load(key)?
                .ok_or_else(|| RecordError::Missing(key.to_owned()));
        }
        let env = Environment {
            workspace: self.workspace.clone(),
            ..env.clone()
        };
        self.confined(&[], &env.grants)?;
        let mut doc = Doc::new();
        env.write_resource(&mut doc)
            .map_err(|e| RecordError::Model(e.to_string()))?;
        doc.commit();
        self.write(key, &mut doc, None)?;
        Ok(env)
    }

    /// Make the environment `key` of these records one of `into`'s: the record's whole
    /// history, forked under a writer of its own, with one more change naming `into`'s
    /// workspace as its own and its home and grants of this workspace's files as the same
    /// places in `into`'s. `rehome` names the path a path of this workspace's
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
        let Some(mut held) = self.load_current(key)? else {
            return Ok(false);
        };
        if into.read(key)?.is_some() {
            return Err(RecordError::Refused(format!(
                "workspace {} has an environment {key:?} already",
                into.workspace
            )));
        }
        let mut doc = held.doc.fork();
        let mut environment =
            Environment::read(&doc).map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_workspace(key, &environment)?;
        environment.workspace.clone_from(&into.workspace);
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
            .write_resource(&mut doc)
            .map_err(|e| RecordError::Model(e.to_string()))?;
        doc.commit();
        into.write(key, &mut doc, None)?;
        Ok(true)
    }

    pub fn keys(&self) -> Result<Vec<String>, RecordError> {
        let reply = self.command(&Command::Records)?;
        let Reply::Ids(ids) = reply else {
            return Err(RecordError::Store(format!(
                "the workspace answered a listing with {reply:?}"
            )));
        };
        let prefix = self.prefix();
        let mut keys: Vec<String> = ids
            .into_iter()
            .filter_map(|id| Some(id.strip_prefix(&prefix)?.to_owned()))
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
        let held = self
            .load_current(key)?
            .ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        let mut replica = Replica::<Environment>::from_doc(held.doc).as_author(AUTHOR);
        replica.set_now_ms(tilo_clock::unix_now_ms_i64());
        let before = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_workspace(key, &before)?;
        replica
            .apply_confirmed(invoke)
            .map_err(|e| RecordError::Refused(e.to_string()))?;
        let mut after = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.confined(&before.grants, &after.grants)?;
        self.write(key, replica.doc_mut(), Some(held.heads.as_slice()))?;
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
    /// it, and write what they changed: one write, so a reader never sees
    /// half of what `act` did.
    ///
    /// # Errors
    /// What `act` returns (nothing is written then), or an error reading
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
        let held = self
            .load_current(key)?
            .ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        let replica = Replica::<Environment>::from_doc(held.doc);
        let replica = match invoker {
            Invoker::Person(person) => replica.as_author(person),
            Invoker::Agent { agent, person } => replica.as_agent_for(agent, person),
        };
        let mut replica = match run {
            Some(run) => replica.within_group(&run.id, &run.label, crate::broker::COMMAND_GROUP),
            None => replica,
        };
        replica.set_now_ms(tilo_clock::unix_now_ms_i64());
        let before = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.check_workspace(key, &before)?;
        let answer = act(&mut replica)?;
        if self.confinement.is_some() {
            let after = replica
                .state()
                .map_err(|e| RecordError::Model(e.to_string()))?;
            self.confined(&before.grants, &after.grants)?;
        }
        self.write(key, replica.doc_mut(), Some(held.heads.as_slice()))?;
        Ok(answer)
    }

    pub fn history(&self, key: &str) -> Result<Vec<tilo_resource::history::Version>, RecordError> {
        let mut held = self
            .load_current(key)?
            .ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        Ok(tilo_resource::history::versions(&mut held.doc))
    }
}

fn check_key(key: &str) -> Result<(), RecordError> {
    if is_record_key(key) {
        Ok(())
    } else {
        Err(RecordError::BadKey(key.to_owned()))
    }
}

/// A workspace's one store, served where the session serves Terminal's documents and answering
/// the commands it answers: for tests, which run with no session.
#[cfg(any(test, feature = "test-workspace"))]
#[allow(
    clippy::expect_used,
    reason = "a fixture that cannot serve is a test that cannot run"
)]
pub mod testing {
    use std::collections::BTreeMap;
    use tilo_files::native::socket;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use automerge::AutoCommit;
    use runtime_portal::workspace_documents;
    use tilo_automerge::ApplyIncoming as _;
    use tilo_resource_store::wire::{Bytes, Command, Reply};

    /// The workspace a command naming none is carried out in, as the session carries it out in
    /// the one in front of the person.
    pub const FRONT: &str = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0";

    type Documents = Arc<Mutex<BTreeMap<(String, String), AutoCommit>>>;

    pub struct Workspace {
        socket: PathBuf,
        documents: Documents,
        stopping: Arc<AtomicBool>,
        served: Option<std::thread::JoinHandle<()>>,
    }

    impl Workspace {
        /// Serve Terminal's documents at `socket` until dropped.
        #[must_use]
        pub fn serve(socket: &Path) -> Self {
            if let Some(parent) = socket.parent() {
                std::fs::create_dir_all(parent).expect("the workspace socket's directory");
            }
            let listener = socket::bind(socket).expect("the workspace's socket");
            let documents = Documents::default();
            let stopping = Arc::new(AtomicBool::new(false));
            let served = {
                let (documents, stopping) = (Arc::clone(&documents), Arc::clone(&stopping));
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        if stopping.load(Ordering::Acquire) {
                            return;
                        }
                        let Ok(stream) = stream else { continue };
                        // One asker that says nothing holds up no other for long.
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                        let Ok(Some(request)) = workspace_documents::read_request(&stream) else {
                            continue;
                        };
                        let workspace = request.workspace.unwrap_or_else(|| FRONT.to_owned());
                        let _ = workspace_documents::serve(stream, |command| {
                            answer(&documents, &workspace, command)
                        });
                    }
                })
            };
            Self {
                socket: socket.to_owned(),
                documents,
                stopping,
                served: Some(served),
            }
        }

        #[must_use]
        pub fn socket(&self) -> &Path {
            &self.socket
        }

        /// Whether `workspace` holds Terminal's document `id`.
        #[must_use]
        pub fn holds(&self, workspace: &str, id: &str) -> bool {
            self.documents
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&(workspace.to_owned(), id.to_owned()))
        }

        /// Move the document `id` from `from` to `to`, as a copy of another Tilo's could arrive.
        pub fn move_document(&self, from: &str, to: &str, id: &str) {
            let mut documents = self
                .documents
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let doc = documents
                .remove(&(from.to_owned(), id.to_owned()))
                .expect("the document to move");
            documents.insert((to.to_owned(), id.to_owned()), doc);
        }
    }

    /// Serve Terminal's documents at `socket` for as long as this process runs, unless they
    /// are served there already: for a test whose sessions and supervisors come and go around one
    /// workspace.
    pub fn keep_serving(socket: &Path) {
        SERVED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(socket.to_owned())
            .or_insert_with(|| Workspace::serve(socket));
    }

    /// Whether the workspace [`keep_serving`] serves at `socket` holds Terminal's document `id`
    /// in `workspace`.
    #[must_use]
    pub fn kept(socket: &Path, workspace: &str, id: &str) -> bool {
        SERVED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(socket)
            .is_some_and(|served| served.holds(workspace, id))
    }

    static SERVED: Mutex<BTreeMap<PathBuf, Workspace>> = Mutex::new(BTreeMap::new());

    impl Drop for Workspace {
        fn drop(&mut self) {
            self.stopping.store(true, Ordering::Release);
            // The accept that waits is answered, and sees it is to stop.
            drop(socket::connect(&self.socket));
            if let Some(served) = self.served.take() {
                let _ = served.join();
            }
            let _ = std::fs::remove_file(&self.socket);
        }
    }

    fn answer(documents: &Documents, workspace: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: Command =
            serde_json::from_slice(command).map_err(|error| error.to_string())?;
        let reply = carry_out(
            &mut documents.lock().unwrap_or_else(PoisonError::into_inner),
            workspace,
            command,
        )?;
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }

    fn carry_out(
        documents: &mut BTreeMap<(String, String), AutoCommit>,
        workspace: &str,
        command: Command,
    ) -> Result<Reply, String> {
        Ok(match command {
            Command::Records => Reply::Ids(
                documents
                    .keys()
                    .filter(|(held, _)| held == workspace)
                    .map(|(_, id)| id.clone())
                    .collect(),
            ),
            Command::RecordChanges {
                record: Some(id),
                after,
                ..
            } => match documents.get_mut(&(workspace.to_owned(), id)) {
                None => Reply::RecordChanges {
                    changes: Bytes::default(),
                    heads: Vec::new(),
                    next: None,
                    pending: false,
                },
                Some(doc) => {
                    let known: Vec<automerge::ChangeHash> = after
                        .iter()
                        .filter_map(|head| head.parse().ok())
                        .filter(|head| doc.get_change_by_hash(head).is_some())
                        .collect();
                    Reply::RecordChanges {
                        changes: Bytes(doc.save_after(&known)),
                        heads: doc.get_heads().iter().map(ToString::to_string).collect(),
                        next: None,
                        pending: false,
                    }
                }
            },
            Command::RecordChange {
                record: Some(id),
                changes,
                more: false,
                ..
            } => {
                documents
                    .entry((workspace.to_owned(), id))
                    .or_default()
                    .load_incoming(&changes.0)
                    .map_err(|error| error.to_string())?;
                Reply::Written
            }
            other => return Err(format!("not a command of Terminal's documents: {other:?}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{FRONT, Workspace};
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

    tilo_test_env::scenario! {
        /// `Records`: the environment records of one store.
        fn records_are_created_resolved_and_changed_through_their_actions;

        phase the_default_environment_is_created_on_first_use_and_only_it;
        {
            let dir = tempfile::tempdir().unwrap();
            let workspace = Workspace::serve(&dir.path().join("session.sock"));
            let records = Records::at(workspace.socket());

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
                workspace.holds(FRONT, &records.document_id(DEFAULT_RECORD).unwrap()),
                "the record is a Terminal document of the workspace in front"
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

        phase whether_an_environment_resolves_is_answered_without_creating_it;
        {
            let dir = tempfile::tempdir().unwrap();
            let workspace = Workspace::serve(&dir.path().join("session.sock"));
            let records = Records::at(workspace.socket());

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

        phase actions_change_the_stored_record_and_refusals_change_nothing;
        {
            let dir = tempfile::tempdir().unwrap();
            let workspace = Workspace::serve(&dir.path().join("session.sock"));
            let records = Records::at(workspace.socket());
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

        phase without_the_session_the_person_is_told_where_environments_are;
        {
            let dir = tempfile::tempdir().unwrap();
            let records = Records::at(dir.path().join("session.sock"));
            let err = records.resolve(None, &OneRuntime(Some(image(1)))).unwrap_err();
            assert!(matches!(err, RecordError::Unreachable), "{err}");
            assert_eq!(
                err.to_string(),
                "environments are kept in your workspace, and the Tilo session that reaches it \
                 is not running on this device"
            );
        }
    }

    const W1: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
    const W2: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

    fn workspace_store(root: &Path, files: &Path, id: &str) -> crate::store::Store {
        crate::store::Store::at(root)
            .with_workspace(files)
            .for_workspace(&crate::workspace::WorkspaceId::parse(Some(id)).unwrap())
    }

    tilo_test_env::scenario! {
        /// `Records` per workspace: each workspace's environments and files.
        fn a_workspaces_environments_and_files_are_its_own;

        phase a_workspace_keeps_its_own_environments;
        {
            let dir = tempfile::tempdir().unwrap();
            let files = dir.path().join("files");
            let w1 = Records::of(&workspace_store(&dir.path().join("ush"), &files, W1));
            let w2 = Records::of(&workspace_store(&dir.path().join("ush"), &files, W2));
            let workspace = Workspace::serve(w1.documents());
            assert_eq!(w1.workspace(), W1);

            let (key, env) = w1.resolve(None, &OneRuntime(Some(image(1)))).unwrap();
            assert_eq!(env.workspace, W1, "the record names its workspace");
            assert!(
                workspace.holds(W1, &w1.document_id(&key).unwrap()),
                "kept in the workspace"
            );
            w1.create("env-only-w1", &env).unwrap();
            assert_eq!(w1.keys().unwrap(), vec!["default", "env-only-w1"]);
            assert!(
                w2.keys().unwrap().is_empty(),
                "another workspace lists none of them"
            );
            assert!(w2.load("env-only-w1").unwrap().is_none());
            assert!(
                Records::at(w1.documents()).keys().unwrap().is_empty(),
                "nor does the primary workspace's"
            );

            // A record moved into another workspace is not its to open.
            workspace.move_document(W1, W2, &w1.document_id("env-only-w1").unwrap());
            assert!(matches!(
                w2.load("env-only-w1"),
                Err(RecordError::Refused(_))
            ));
            assert!(matches!(
                w2.apply("env-only-w1", Environment::RENAME.with("mine")),
                Err(RecordError::Refused(_))
            ));
        }

        phase an_environment_from_before_workspaces_is_the_primary_workspaces;
        {
            let dir = tempfile::tempdir().unwrap();
            let files = dir.path().join("files");
            let store = crate::store::Store::at(dir.path().join("ush")).with_workspace(&files);
            let _workspace = Workspace::serve(store.records_root());
            let legacy = Records::at(store.records_root());
            let mut old = Environment::new("Old", "2026.10", tilo_environment::host_arch(), &image(1));
            old.grants
                .push(tilo_environment::dir_grant(Path::new("/var/home/you/src"), true).unwrap());
            let mut doc = Doc::new();
            old.write_resource(&mut doc).unwrap();
            doc.commit();
            legacy.write(DEFAULT_RECORD, &mut doc, None).unwrap();

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

        phase a_workspace_environment_is_refused_another_workspaces_files;
        {
            let dir = tempfile::tempdir().unwrap();
            let files = dir.path().join("files");
            let tree = |id: &str| files.join(".workspaces").join(id);
            let store = workspace_store(&dir.path().join("ush"), &files, W1);
            let _workspace = Workspace::serve(store.records_root());
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
}
