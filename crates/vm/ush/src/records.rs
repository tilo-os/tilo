//! Environment records, kept where Terminal keeps its own: as Terminal's documents in their
//! workspace's one store, which this device's workspace service holds and syncs.
//!
//! The runtime holds no connection to the workspace of its own. It asks the device's daemon,
//! which holds this device's workspaces, to read and write Terminal's documents for it (its
//! `terminalRecords` device action), naming the workspace the Terminal window runs for: a
//! workspace it cannot name is no workspace's, and the daemon refuses it. Each read takes the
//! record whole, and each change is written as the changes after what was read, so two processes
//! changing one record merge, and every change carries history and an author. Without the daemon
//! there is no record to read: the person is told so ([`RecordError::Unreachable`]), and nothing
//! is kept on the side.
//!
//! Each workspace has its own environments ([`crate::workspace`]). An app names a record by key
//! only ([`tilo_environment::is_record_key`]); the workspace and the owning app are fixed here,
//! never taken from a request.

use std::path::{Path, PathBuf};

use automerge::ChangeHash;
use collab_types::caps::CapOverride;
use collab_types::store::wire::{Bytes, Command, RecordPage, RecordPages, Reply};
use tilo_device_identity::login::TERMINAL_RECORDS;
use tilo_device_identity::login::daemon::DeviceActions;
use tilo_environment::{DEFAULT_NAME, DEFAULT_RECORD, Environment, is_record_key};
use tilo_resource::{AppNamed, Doc, Invoke, Replica, Resource};

use crate::images::Images;
use crate::workspace::Confinement;

pub const PRIMARY_WORKSPACE: &str = crate::workspace::PRIMARY;

/// What every environment record's document id starts with: Terminal keeps documents of its own
/// beside them.
const DOCUMENT_PREFIX: &str = "environment:";

/// How many bytes of changes one write carries: what fits a request line to the daemon once
/// written as base64.
const RUN_BYTES: usize = 512 * 1024;

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
        "environments are kept in your workspace, and Tilo's device service that keeps it is not \
         running on this computer"
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
    /// The device daemon's native endpoint, which keeps Terminal's documents.
    documents: PathBuf,
    workspace: String,
    confinement: Option<Confinement>,
    /// The person the daemon writes the records as, as its answers name them: whom each change
    /// made here names as its maker.
    person: std::sync::OnceLock<String>,
}

impl Records {
    /// The primary workspace's records, kept by the daemon at `documents`.
    pub fn at(documents: impl Into<PathBuf>) -> Self {
        Self {
            documents: documents.into(),
            workspace: PRIMARY_WORKSPACE.to_owned(),
            confinement: None,
            person: std::sync::OnceLock::new(),
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
            person: std::sync::OnceLock::new(),
        }
    }

    /// The device daemon's native endpoint, which keeps Terminal's documents.
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

    /// The id of the document holding the record `key`, among Terminal's documents in the
    /// workspace.
    ///
    /// # Errors
    /// Returns [`RecordError::BadKey`] for a key that is not a record key.
    pub fn document_id(&self, key: &str) -> Result<String, RecordError> {
        check_key(key)?;
        Ok(format!("{DOCUMENT_PREFIX}{key}"))
    }

    /// Carry out `command` on Terminal's documents in this workspace, through the device's
    /// daemon. The primary workspace is no workspace the daemon knows, and it says so.
    fn command(&self, command: &Command) -> Result<Reply, RecordError> {
        // Without a device of the person's there is no daemon to ask.
        if self.documents.as_os_str().is_empty() {
            return Err(RecordError::Unreachable);
        }
        let workspace = (!self.is_primary()).then_some(self.workspace.as_str());
        let answer = DeviceActions::at(&self.documents)
            .ask(
                TERMINAL_RECORDS,
                serde_json::json!({ "workspace": workspace, "command": command }),
            )
            .map_err(|refused| {
                if refused.not_running() {
                    tracing::info!(%refused, "ush: no daemon keeps Terminal's documents");
                    RecordError::Unreachable
                } else {
                    RecordError::Refused(refused.message)
                }
            })?;
        if let Some(person) = answer.get("person").and_then(serde_json::Value::as_str) {
            let _ = self.person.set(person.to_owned());
        }
        let reply = answer
            .get("reply")
            .cloned()
            .ok_or_else(|| RecordError::Store(format!("the daemon answered {answer}")))?;
        serde_json::from_value(reply).map_err(|e| RecordError::Store(e.to_string()))
    }

    /// The person the daemon writes the records as, asked of it once.
    fn person(&self) -> Result<&str, RecordError> {
        if self.person.get().is_none() {
            self.command(&Command::Records)?;
        }
        self.person
            .get()
            .map(String::as_str)
            .ok_or_else(|| RecordError::Store("the daemon named nobody it writes as".into()))
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
        let (mut pages, mut from) = (RecordPages::default(), 0);
        loop {
            let reply = self.command(&Command::RecordChanges {
                record: Some(record.clone()),
                after: after.clone(),
                from,
            })?;
            match pages.join(reply) {
                Ok(RecordPage::From(next)) => from = next,
                Ok(RecordPage::Pending) => {
                    return Err(RecordError::Store(
                        "it is still on its way to this device; try again in a moment".into(),
                    ));
                }
                Ok(RecordPage::Done { changes, heads }) => {
                    let heads = heads
                        .iter()
                        .map(|head| head.parse::<ChangeHash>().map(|head| head.0))
                        .collect::<Result<_, _>>()
                        .map_err(|_| RecordError::Store("the workspace named no change".into()))?;
                    return Ok((changes, heads));
                }
                Err(reply) => {
                    return Err(RecordError::Store(format!(
                        "the workspace answered a read with {reply:?}"
                    )));
                }
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
        let runs: Vec<&[u8]> = changes.chunks(RUN_BYTES).collect();
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
        let record = self.document_id(key)?;
        if tilo_resource::founding::bring_up_resource::<Environment>(
            &self.workspace,
            &record,
            &mut held.doc,
        )
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
        // Made through the record's own verbs, as Terminal's: the workspace takes each change by
        // the verb it names, on the founding every holder derives of the record.
        let mut replica = Replica::<Environment>::from_doc(self.founded(key)?)
            .as_author(self.person()?)
            .in_app(tilo_environment::OWNER_APP_ID, "Terminal");
        replica.set_now_ms(tilo_clock::unix_now_ms_i64());
        let mut made = vec![Environment::MAKE.with((
            env.name.clone(),
            env.runtime.clone(),
            env.workspace.clone(),
        ))];
        for tool in &env.tools {
            for (arch, image) in &tool.images {
                made.push(Environment::ADD_TOOL.with((
                    tool.packages.clone(),
                    arch.clone(),
                    image.clone(),
                )));
            }
        }
        if !env.home_dir.is_empty() {
            made.push(Environment::SET_HOME.with(env.home_dir.clone()));
        }
        for grant in &env.grants {
            made.push(granting(grant)?);
        }
        for invoke in made {
            replica
                .apply_confirmed(invoke)
                .map_err(|e| RecordError::Refused(e.to_string()))?;
        }
        let made = replica
            .state()
            .map_err(|e| RecordError::Model(e.to_string()))?;
        self.write(key, replica.doc_mut(), None)?;
        Ok(made)
    }

    /// The record `key` as it is founded: the derived founding of its kind and id in this
    /// workspace, which every holder makes alike and the workspace admits only as that, then the
    /// record's initial content as the person's own edit of the record they made, as an app's
    /// record is founded.
    fn founded(&self, key: &str) -> Result<Doc, RecordError> {
        let model = |e: &dyn std::fmt::Display| RecordError::Model(e.to_string());
        let record = self.document_id(key)?;
        let containers = tilo_resource::founding::of_kind(&Environment::surface());
        let mut doc = Doc::new();
        let scope = tilo_resource::founding::Scope {
            workspace: &self.workspace,
            kind: Environment::KIND,
            id: &record,
        };
        let founding = tilo_resource::founding::chain(scope, Environment::VERSION, &containers)
            .map_err(|e| model(&e))?;
        tilo_automerge::ApplyIncoming::apply_incoming(&mut doc, founding).map_err(|e| model(&e))?;
        Environment::initialize(&mut doc).map_err(|e| model(&e))?;
        doc.commit_with(
            automerge::transaction::CommitOptions::default().with_message(
                tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::person(
                    self.person()?,
                )),
            ),
        );
        Ok(doc)
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
        // The step that founds the record in the branch names the store's branch verb: the fork
        // keeps the history it was copied with, and this one step says where it is now.
        let step = collab_types::document::identity::branch_step(
            key,
            Environment::KIND,
            Environment::LABEL,
        );
        let message = step
            .change_message()
            .ok_or_else(|| RecordError::Model("the branch's step names no entry".to_owned()))?;
        doc.commit_with(automerge::transaction::CommitOptions::default().with_message(message));
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
        let prefix = DOCUMENT_PREFIX;
        let mut keys: Vec<String> = ids
            .into_iter()
            .filter_map(|id| Some(id.strip_prefix(prefix)?.to_owned()))
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
        let mut replica = Replica::<Environment>::from_doc(held.doc)
            .as_author(self.person()?)
            .in_app(tilo_environment::OWNER_APP_ID, "Terminal");
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

/// The verb that grants what `grant` does: a directory, or a host.
fn granting(grant: &collab_types::caps::CapOverride) -> Result<Invoke<Environment>, RecordError> {
    use collab_types::caps::{Access, Scope};
    match &grant.scope {
        Scope::FsPath(path) => {
            Ok(Environment::GRANT_DIR.with((path.clone(), grant.access == Access::Write)))
        }
        Scope::NetHost(host) => Ok(Environment::ALLOW_HOST.with(host.clone())),
        other => Err(RecordError::Refused(format!(
            "an environment is made granting a directory or a host, not {other:?}"
        ))),
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
        act: impl FnOnce(&mut Replica<Environment, AppNamed>) -> Result<T, RecordError>,
    ) -> Result<T, RecordError> {
        self.act_within(key, invoker, None, act)
    }

    pub fn act_within<T>(
        &self,
        key: &str,
        invoker: &Invoker,
        run: Option<&crate::broker::RunGroup>,
        act: impl FnOnce(&mut Replica<Environment, AppNamed>) -> Result<T, RecordError>,
    ) -> Result<T, RecordError> {
        let held = self
            .load_current(key)?
            .ok_or_else(|| RecordError::Missing(key.to_owned()))?;
        let replica = Replica::<Environment>::from_doc(held.doc)
            .in_app(tilo_environment::OWNER_APP_ID, "Terminal");
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

/// The device's daemon as it keeps Terminal's documents: its native endpoint answering the
/// `terminalRecords` device action as the daemon does, a command naming no workspace refused. For
/// tests, which run with no daemon.
#[cfg(any(test, feature = "test-workspace"))]
#[allow(
    clippy::expect_used,
    reason = "a fixture that cannot serve is a test that cannot run"
)]
pub mod testing {
    use std::collections::BTreeMap;
    use std::io::{BufRead as _, Write as _};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use tilo_files::native::socket;

    use automerge::AutoCommit;
    use collab_types::store::wire::{Bytes, Command, Reply};
    use serde_json::{Value, json};
    use tilo_automerge::ApplyIncoming as _;

    /// What the daemon says of a command naming no workspace.
    pub const NO_WORKSPACE: &str = "Terminal's environments and shells are a workspace's, and \
                                    this names none: open Terminal in a workspace";

    type Documents = Arc<Mutex<BTreeMap<(String, String), AutoCommit>>>;

    pub struct Workspace {
        socket: PathBuf,
        documents: Documents,
        stopping: Arc<AtomicBool>,
        served: Option<std::thread::JoinHandle<()>>,
    }

    impl Workspace {
        /// Answer the daemon's Terminal documents at `socket`, its native endpoint, until
        /// dropped.
        #[must_use]
        pub fn serve(socket: &Path) -> Self {
            Self::serving(socket, None)
        }

        /// [`Self::serve`], carrying a command that names no workspace out in `unnamed`, when
        /// one is named here, rather than refusing it as the daemon does.
        fn serving(socket: &Path, unnamed: Option<&'static str>) -> Self {
            if let Some(parent) = socket.parent() {
                std::fs::create_dir_all(parent).expect("the daemon socket's directory");
            }
            let listener = socket::bind(socket).expect("the daemon's socket");
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
                        let mut line = String::new();
                        let Ok(read) = std::io::BufReader::new(&stream).read_line(&mut line) else {
                            continue;
                        };
                        if read == 0 {
                            continue;
                        }
                        let answer = called(&documents, unnamed, &line);
                        let _ = writeln!(&stream, "{answer}");
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
    /// workspace. Such a test is of what ush does with a record, not of where it is kept, so a
    /// store of no workspace (`Store::at`) keeps its records here as the primary workspace's,
    /// which the daemon itself refuses ([`Workspace::serve`] holds that).
    pub fn keep_serving(socket: &Path) {
        SERVED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(socket.to_owned())
            .or_insert_with(|| Workspace::serving(socket, Some(crate::workspace::PRIMARY)));
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

    /// The daemon's answer line to the tool call `line`.
    fn called(documents: &Documents, unnamed: Option<&str>, line: &str) -> Value {
        let call: Value = serde_json::from_str(line).unwrap_or_default();
        let args = call
            .pointer("/params/arguments/args")
            .cloned()
            .unwrap_or_default();
        let path = call
            .pointer("/params/arguments/path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let answer = if path.ends_with(&format!(
            "#{}",
            tilo_device_identity::login::TERMINAL_RECORDS
        )) {
            answer(documents, unnamed, &args)
        } else {
            Err(format!("this daemon answers no {path}"))
        };
        let structured = match answer {
            // The daemon names the person it writes as, as every answer of the device's does.
            Ok(reply) => json!({"status": "ok", "result": {"reply": reply, "person": "ada"}}),
            Err(message) => {
                json!({"status": "failed", "error": {"code": "shell-runtime", "message": message}})
            }
        };
        json!({"jsonrpc": "2.0", "id": call.get("id"), "result": {
            "content": [{"type": "text", "text": ""}],
            "structuredContent": structured,
        }})
    }

    fn answer(documents: &Documents, unnamed: Option<&str>, args: &Value) -> Result<Value, String> {
        let workspace = args
            .get("workspace")
            .and_then(Value::as_str)
            .filter(|workspace| !workspace.is_empty())
            .or(unnamed)
            .ok_or(NO_WORKSPACE)?;
        let command: Command =
            serde_json::from_value(args.get("command").cloned().unwrap_or_default())
                .map_err(|error| error.to_string())?;
        let reply = carry_out(
            &mut documents.lock().unwrap_or_else(PoisonError::into_inner),
            workspace,
            command,
        )?;
        serde_json::to_value(&reply).map_err(|error| error.to_string())
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
