//! The shell's chooser service (`tilo_dialogs::shell`): one system service every app reaches for
//! the files it asks the person for and the links and files it opens.
//!
//! The launcher connects an app to this socket before its sandbox starts and writes the first
//! line, a [`Handshake`] naming the app and its workspace; nothing the app writes after can change
//! who it is. The server itself is portable ([`tilo_dialogs::chooser_service`]): this module fills
//! it in with the session's own questions ([`tilo_portal::file_chooser::choose`]) and with the
//! loop that opens a link or a file in the app that takes it ([`crate::apps::Apps::open_claimed`]).
//!
//! A file the person chooses inside a workspace's files tree is a grant to the app on the file's
//! tree node, written through the workspace's own governance as the person who chose it
//! ([`PolicyFiles`]): the app reaches exactly that node, follows it through renames and moves, and
//! opens it again only while the grant stands. Anything else the person chooses is kept in the
//! session's table of chosen files ([`tilo_dialogs::chosen`]): the device keeps it in its own
//! records for the person, and the app is handed the reference that names it again.

use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_action_policy::{Answer, Change, Grant, Grantee};
use tilo_actions::ApiError;
use tilo_dialogs::chooser_service::{
    Asked, Caller, Choice, Granted, Grants, Opens as ServiceOpens, Person, Question,
    Serving as ServiceServing,
};
use tilo_dialogs::chosen::{AppKey, ChosenTable, Kind};
use tilo_lock::MutexRecover as _;
use tilo_portal::PortalCaller;
use tilo_portal::file_chooser::{FileChooserQuestion, PickerFilter};
use tilo_portal::reply::Ended;
use tilo_service::{AskSender, Wake};

/// How long an open waits for the session's loop, which alone starts apps. The app waits a little
/// longer for the answer than this.
const OPEN_WAIT: Duration = Duration::from_secs(20);

const DID_NOT_OPEN: &str = "It didn't open. Try again.";
const NOT_IN_TIME: &str = "It didn't open in time. Try again.";

/// What the chooser service is served with.
pub(crate) struct Serving {
    /// Where the person is asked; `None` when nobody is here to answer one.
    pub questions: Option<AskSender>,
    /// The files people choose for apps outside any workspace, as the session keeps them for the
    /// app and for its next session: the one table the portal grants through too.
    pub chosen: Arc<ChosenTable>,
    /// The workspaces' file trees, for a choice that lies inside one: `None` when this session
    /// reaches no workspace mount.
    pub files: Option<Arc<dyn WorkspaceFiles>>,
    /// What wakes the session's loop when an open waits for it.
    pub wake: Wake,
}

/// An open the service leaves for the session's loop: only a pass of the loop decides which app
/// takes a link and starts it.
struct Open {
    workspace: String,
    uri: String,
    reply: SyncSender<Result<Option<()>, ApiError>>,
}

/// What opens a link or a file for the loop: the session's apps.
pub(crate) trait Opens {
    /// Open `uri` in `workspace` in the app that takes it; `None` when nothing does.
    ///
    /// # Errors
    /// The app that takes it did not start.
    fn open_claimed(&self, workspace: &str, uri: &str) -> Result<Option<()>, ApiError>;
}

impl Opens for crate::apps::Apps {
    fn open_claimed(&self, workspace: &str, uri: &str) -> Result<Option<()>, ApiError> {
        Self::open_claimed(self, workspace, uri)
    }
}

/// The opens the connection threads leave for the loop.
#[derive(Default)]
struct Pending {
    opens: Mutex<Vec<Open>>,
    wake: Mutex<Option<Wake>>,
}

impl Pending {
    fn push(&self, open: Open) {
        self.opens.lock_recover().push(open);
        if let Some(wake) = self.wake.lock_recover().as_ref() {
            wake();
        }
    }
}

impl ServiceOpens for Pending {
    fn open(&self, workspace: &str, uri: &str) -> Result<Option<()>, String> {
        let (reply, answered) = std::sync::mpsc::sync_channel(1);
        self.push(Open {
            workspace: workspace.to_owned(),
            uri: uri.to_owned(),
            reply,
        });
        match answered.recv_timeout(OPEN_WAIT) {
            Ok(Ok(opened)) => Ok(opened),
            Ok(Err(error)) => {
                tracing::warn!(%error, "what an app asked to open did not open");
                Err(DID_NOT_OPEN.to_owned())
            }
            Err(_) => Err(NOT_IN_TIME.to_owned()),
        }
    }
}

/// The chooser service, served until the session ends.
pub(crate) struct Served {
    pending: Arc<Pending>,
}

impl Served {
    /// Open what the apps asked to open since the last pass, on the loop.
    pub(crate) fn follow(&self, opens: &dyn Opens) {
        let asked = std::mem::take(&mut *self.pending.opens.lock_recover());
        for open in asked {
            let _ = open
                .reply
                .send(opens.open_claimed(&open.workspace, &open.uri));
        }
    }
}

/// Bind the chooser socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
pub(crate) fn serve(serving: Serving) -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    serve_at(
        &launcher::services::socket_for(launcher::services::CHOOSER_SERVICE),
        serving,
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the chooser service accepts and serves on its own threads"
)]
fn serve_at(path: &std::path::Path, serving: Serving) -> Result<Served, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove the stale chooser socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(path)
        .map_err(|error| format!("bind the chooser socket {}: {error}", path.display()))?;
    let pending = Arc::new(Pending {
        opens: Mutex::new(Vec::new()),
        wake: Mutex::new(Some(serving.wake)),
    });
    let shared = ServiceServing {
        person: serving
            .questions
            .map(|questions| Arc::new(SessionPerson { questions }) as Arc<dyn Person>),
        grants: Arc::new(ChosenGrants {
            chosen: serving.chosen,
            files: serving.files,
        }),
        opens: Arc::clone(&pending) as Arc<dyn ServiceOpens>,
    };
    let serving = Arc::new(shared);
    std::thread::Builder::new()
        .name("tilo-chooser".into())
        .spawn(move || tilo_dialogs::chooser_service::serve_listener(&listener, &serving))
        .map_err(|error| format!("start the chooser service: {error}"))?;
    tracing::info!(path = %path.display(), "choosing files and opening links for the apps");
    Ok(Served { pending })
}

/// The files the person chose for the apps of this session: a file of a workspace is a grant to
/// the app on the file's tree node, and anything else is kept in the session's chosen table.
struct ChosenGrants {
    chosen: Arc<ChosenTable>,
    files: Option<Arc<dyn WorkspaceFiles>>,
}

impl Grants for ChosenGrants {
    fn grant(&self, caller: &Caller, chosen: &Path, choice: Choice) -> Result<Granted, String> {
        // A file of a workspace's tree is a grant on its node, written where the workspace keeps
        // what it allows: it follows a rename or a move, shows in the file's sharing and ends
        // when the grant does, on every device that holds the workspace.
        if let Some(files) = self.files.as_deref()
            && let Some(node) = files.node(chosen).or_else(|| files.create(chosen, choice))
        {
            let write = choice.writes();
            files.grant(&node, &caller.app, write).map_err(|error| {
                tracing::warn!(%error, path = %chosen.display(), "a chosen file is not granted to the app");
                tilo_dialogs::chooser_service::DOES_NOT_FIT.to_owned()
            })?;
            return Ok(node_granted(&node, write));
        }
        let key = documents_key(caller)?;
        self.chosen
            .grant(&key, chosen, kind_of(choice))
            .map(granted_of)
            .map_err(|error| {
                tracing::warn!(
                    %error,
                    path = %chosen.display(),
                    "a chosen item is not kept for the app"
                );
                tilo_dialogs::chooser_service::DOES_NOT_FIT.to_owned()
            })
    }

    fn document(&self, caller: &Caller, reference: &Path) -> Option<Granted> {
        // A reference naming a workspace file opens again while the app's grant on its node
        // stands. Reopened by the node, it opens whatever the file is called now.
        if let Some((workspace, id)) = reference_node(reference) {
            let files = self.files.as_ref()?;
            let node = files.node_at(workspace, id)?;
            let write = files.holds(&node, &caller.app, true);
            if !write && !files.holds(&node, &caller.app, false) {
                return None;
            }
            return Some(node_granted(&node, write));
        }
        let key = documents_key(caller).ok()?;
        self.chosen.looked_up(&key, reference).map(granted_of)
    }
}

/// The path a choice of a workspace file answers with: the node it is, with the workspace whose
/// tree holds it, never a host path an app could use itself.
const CHOSEN_ROOT: &str = tilo_dialogs::chosen::CHOSEN_ROOT;

/// The tree node `reference` names, and the workspace whose tree holds it, when it names one.
fn reference_node(reference: &Path) -> Option<(&str, fs_protocol::FileDocumentId)> {
    let rest = reference
        .to_str()?
        .strip_prefix(CHOSEN_ROOT)?
        .strip_prefix('/')?;
    let (workspace, rest) = rest.split_once('/')?;
    tilo_desktop_model::WorkspaceId::parse(workspace)?;
    let id = rest.split('/').next()?;
    Some((
        workspace,
        fs_protocol::FileDocumentId(uuid::Uuid::parse_str(id).ok()?),
    ))
}

/// One file of a workspace's tree, as a choice names it: its stable record, where it is now, and
/// the workspace whose tree it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceNode {
    workspace: String,
    id: fs_protocol::FileDocumentId,
    host: PathBuf,
    directory: bool,
}

impl WorkspaceNode {
    /// What an app reaches it by: the node itself, so a rename leaves it opening the same file.
    fn reference(&self) -> PathBuf {
        PathBuf::from(CHOSEN_ROOT)
            .join(&self.workspace)
            .join(self.id.0.to_string())
    }

    fn name(&self) -> String {
        self.host.file_name().map_or_else(
            || "document".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// The kind the workspace's folder listing offers the node as: a file or a folder of the
    /// tree ([`fs_protocol::tree::FOLDER_KIND`], beside a file's
    /// [`tilo_workspace_records::search::FOUND_FILE`]).
    const fn kind(&self) -> &'static str {
        if self.directory {
            fs_protocol::tree::FOLDER_KIND
        } else {
            tilo_workspace_records::search::FOUND_FILE
        }
    }

    /// The node a grant names: the tree's app, the kind of node, and its stable record.
    fn node(&self) -> tilo_action_policy::TreePath {
        tilo_action_policy::TreePath::workspace(&self.workspace)
            .app(fs_protocol::tree::FILES_APP)
            .kind(self.kind())
            .resource(self.id.0.to_string())
    }
}

/// The document a node's grant answers with: a descriptor the chooser opens, and what the app
/// reaches it by.
fn node_granted(node: &WorkspaceNode, write: bool) -> Granted {
    let choice = if node.directory {
        Choice::Folder { write }
    } else if write {
        Choice::Save
    } else {
        Choice::Read
    };
    // A save writes the file the node is, in place: a file beside it would be a node of its own,
    // and renaming it over the file would leave the app's grant naming a node the workspace no
    // longer has. The workspace keeps the versions.
    let save_sibling = (write && !node.directory).then(|| node.name().into());
    Granted {
        reference: node.reference(),
        name: node.name(),
        host: node.host.clone(),
        choice,
        save_sibling,
    }
}

/// The workspaces' file trees as the host reaches them, and the grants a choice writes there.
pub(crate) trait WorkspaceFiles: Send + Sync {
    /// The tree node `host` is, when it is a file of a workspace mount.
    fn node(&self, host: &Path) -> Option<WorkspaceNode>;

    /// The node a file at `host` becomes for `choice`, making it there when the person saves to a
    /// name their workspace's tree does not have yet: `None` when `host` lies in no workspace
    /// tree, or it is not a save that makes one.
    fn create(&self, _host: &Path, _choice: Choice) -> Option<WorkspaceNode> {
        None
    }

    /// The node `workspace`/`id` names as it is now, wherever a rename or a move left it.
    fn node_at(&self, workspace: &str, id: fs_protocol::FileDocumentId) -> Option<WorkspaceNode>;

    /// Give `app` this node: to read, and to change when `write`, as the person who chose it.
    ///
    /// # Errors
    /// The workspace refused the grant, or this device could not write it.
    fn grant(&self, node: &WorkspaceNode, app: &str, write: bool) -> Result<(), String>;

    /// Whether `app` holds this node for reading, and for changing it when `write`.
    fn holds(&self, node: &WorkspaceNode, app: &str, write: bool) -> bool;
}

/// What the person gave `app` a choice on in `workspace`, as the launcher binds it into the app's
/// sandbox: every workspace file node its grants reach, at the reference it was answered with, and
/// nothing else. A grant whose person no longer reaches the node, or that was taken back, binds
/// nothing at the next launch.
pub(crate) fn launch_binds(
    view: &crate::workspace_view::WorkspaceView,
    workspace: &str,
    app: &str,
) -> Vec<launcher::run::GrantedPath> {
    use std::collections::BTreeMap;
    use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Level};
    let Ok(policy) = view.policy(workspace) else {
        return Vec::new();
    };
    let cx = Context::at_secs(tilo_clock::unix_now());
    let mut binds: BTreeMap<std::path::PathBuf, launcher::run::GrantedPath> = BTreeMap::new();
    for grant in &policy.grants {
        if grant.grantee != Grantee::app(app)
            || grant.answer != Answer::Allow
            || !grant.stands_in(&policy, &cx)
            || grant.node.workspace_id() != Some(workspace)
            || grant.node.id_at(Level::App) != Some(fs_protocol::tree::FILES_APP)
        {
            continue;
        }
        let Some(id) = grant.node.id_at(Level::Resource).and_then(|id| {
            tilo_failure::ignored!(
                "a grant on a node whose record names no file document is not a file to bind",
                uuid::Uuid::parse_str(id).ok()
            )
        }) else {
            continue;
        };
        let id = fs_protocol::FileDocumentId(id);
        let Some(node) = tilo_app_launch::downloaded::workspace_roots()
            .iter()
            .find_map(|root| mounted_node(root, workspace, id, None))
        else {
            continue;
        };
        let write = grant.kinds.iter().any(|kind| kind == ActionKind::Undoable);
        let target = node.reference();
        let bind = launcher::run::GrantedPath {
            source: node.host.clone(),
            target: target.clone(),
            write,
        };
        match binds.entry(target) {
            std::collections::btree_map::Entry::Occupied(mut held) => {
                held.get_mut().write |= write;
            }
            std::collections::btree_map::Entry::Vacant(free) => {
                free.insert(bind);
            }
        }
    }
    binds.into_values().collect()
}

/// How long a grant waits for the workspace to take it and this device to read it.
const GRANT_WAIT: Duration = Duration::from_secs(20);

/// The session's view of the workspaces, and the mounts Tilo's filesystem serves their trees on.
pub(crate) struct PolicyFiles {
    view: Arc<crate::workspace_view::WorkspaceView>,
    /// The person who chose, by the id their devices sign as.
    person: String,
}

impl PolicyFiles {
    pub(crate) fn new(view: Arc<crate::workspace_view::WorkspaceView>, person: String) -> Self {
        Self { view, person }
    }

    /// The workspace the mounts serve: the person's own, whose tree a device's filesystem
    /// publishes.
    fn workspace(&self) -> Option<String> {
        tilo_action_policy::roles::workspace_of_person(&self.person)
    }

    /// Write `change` into `workspace`'s governance as this device, waiting for the workspace to
    /// take it. The workspace's own admission decides it, by the person's grants.
    fn commit(&self, workspace: &str, change: Change) -> Result<(), String> {
        let workspace = tilo_desktop_model::WorkspaceId::parse(workspace)
            .ok_or_else(|| "the chosen file names no workspace".to_owned())?;
        let view = Arc::clone(&self.view);
        let (reply, answered) = std::sync::mpsc::sync_channel(1);
        let asked = async move {
            let membership = crate::membership::Membership::of(&*view, workspace);
            let _ = reply.send(membership.commit(&change).await);
        };
        let runtime =
            tilo_rt::try_app().map_err(|error| format!("no task writes the grant: {error}"))?;
        runtime.spawn(asked);
        match answered.recv_timeout(GRANT_WAIT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(reason),
            Err(_) => Err("the workspace did not take the grant in time".to_owned()),
        }
    }

    /// The grant `app` holds on `node` for what `write` adds to reading it, as the workspace's
    /// policy says.
    fn held(
        &self,
        node: &WorkspaceNode,
        app: &str,
        write: bool,
    ) -> Option<tilo_action_policy::KindSet> {
        let policy = self.view.policy(&node.workspace).ok()?;
        granted_kinds(&policy, node, app, write, tilo_clock::unix_now())
    }
}

impl WorkspaceFiles for PolicyFiles {
    fn node(&self, host: &Path) -> Option<WorkspaceNode> {
        let host = host.canonicalize().ok()?;
        let workspace = self.workspace()?;
        let id = {
            let bytes = host_fs::attributes::get(&host, collab_types::document::DOCUMENT_ID_XATTR)
                .ok()
                .flatten()?;
            fs_protocol::FileDocumentId(uuid::Uuid::from_slice(&bytes).ok()?)
        };
        // The record's own binding says where it is: a file that carries another record's id is
        // not that record.
        tilo_app_launch::downloaded::workspace_roots()
            .iter()
            .find_map(|root| mounted_node(root, &workspace, id, Some(&host)))
    }

    fn node_at(&self, workspace: &str, id: fs_protocol::FileDocumentId) -> Option<WorkspaceNode> {
        if self.workspace().as_deref() != Some(workspace) {
            return None;
        }
        tilo_app_launch::downloaded::workspace_roots()
            .iter()
            .find_map(|root| mounted_node(root, workspace, id, None))
    }

    fn create(&self, host: &Path, choice: Choice) -> Option<WorkspaceNode> {
        if choice != Choice::Save || host.symlink_metadata().is_ok() {
            return None;
        }
        let parent = host.parent()?.canonicalize().ok()?;
        tilo_app_launch::downloaded::workspace_roots()
            .into_iter()
            .find(|root| parent.starts_with(root))?;
        // A save makes the file where the person asked for it, in their own tree: the mount gives
        // it the record the grant is written on.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(host)
            .ok()?;
        self.node(host)
    }

    fn grant(&self, node: &WorkspaceNode, app: &str, write: bool) -> Result<(), String> {
        // Written once per app, node and what it allows: choosing it again keeps the one
        // grant, as every reader of it answers the same.
        if self.held(node, app, write).is_some() {
            return Ok(());
        }
        let grant = app_grant(node, app, &self.person, write);
        self.commit(&node.workspace, Change::Grant(grant))?;
        // The workspace's reading reaches this session a moment after it takes the change. Wait
        // for it, so a reopen right after the choice reads the grant and nothing this session
        // cannot read again is answered for.
        let deadline = std::time::Instant::now()
            .checked_add(GRANT_WAIT)
            .unwrap_or_else(std::time::Instant::now);
        while self.held(node, app, write).is_none() {
            if std::time::Instant::now() >= deadline {
                return Err(
                    "the workspace took the grant but this device did not read it".to_owned(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn holds(&self, node: &WorkspaceNode, app: &str, write: bool) -> bool {
        // The workspace's own reading decides, as it does on every device: a grant revoked there
        // is gone here too.
        self.held(node, app, write)
            .is_some_and(|held| covers(held, kinds_of(write)))
    }
}

/// The grant `app` is given on `node` as the person who chose it: reading it, and changing it
/// when `write`.
fn app_grant(node: &WorkspaceNode, app: &str, person: &str, write: bool) -> Grant {
    Grant::new(
        grant_id(node, app, write),
        Grantee::app(app),
        node.node(),
        kinds_of(write),
        Answer::Allow,
        Grantee::person(person),
    )
}

/// What `app`'s grant on `node` allows at `now`, when it holds one that is live: reading it, and
/// changing it when `write`.
fn granted_kinds(
    policy: &tilo_action_policy::WorkspacePolicy,
    node: &WorkspaceNode,
    app: &str,
    write: bool,
    now: u64,
) -> Option<tilo_action_policy::KindSet> {
    let id = grant_id(node, app, write);
    policy
        .grants
        .iter()
        .find(|held| held.id.as_str() == id)
        .filter(|held| {
            held.answer == Answer::Allow
                && held.stands_in(policy, &tilo_action_policy::Context::at_secs(now))
        })
        .map(|held| held.kinds)
}

/// What a choice gives: reading it, and changing it when `write`.
fn kinds_of(write: bool) -> tilo_action_policy::KindSet {
    if write {
        tilo_action_policy::KindSet::READ_UNDOABLE
    } else {
        tilo_action_policy::KindSet::READ
    }
}

/// Whether `held` covers every kind in `wanted`.
fn covers(held: tilo_action_policy::KindSet, wanted: tilo_action_policy::KindSet) -> bool {
    wanted.iter().all(|kind| held.contains(kind))
}

/// The grant `app` holds or is given on `node`: one per app, node and what it allows, so a choice
/// of the same file as a save does not change what an open was given.
fn grant_id(node: &WorkspaceNode, app: &str, write: bool) -> String {
    format!(
        "chosen:{app}:{}:{}",
        node.id.0,
        if write { "write" } else { "read" }
    )
}

/// The node `id` names in the workspace whose tree `root` mounts, when its record's path is
/// `expected` (or anywhere, for a reference naming the node alone).
fn mounted_node(
    root: &Path,
    workspace: &str,
    id: fs_protocol::FileDocumentId,
    expected: Option<&Path>,
) -> Option<WorkspaceNode> {
    let name = format!(
        "{}{}",
        fs_protocol::projection::DOCUMENT_INDEX_BY_ID_XATTR_PREFIX,
        id.0
    );
    let bytes = host_fs::attributes::get(root, name).ok().flatten()?;
    if bytes.len() > fs_protocol::projection::MAX_DOCUMENT_INDEX_PAGE_BYTES {
        return None;
    }
    let exact = fs_protocol::projection::DocumentIndexExactV2::from_json(&bytes).ok()?;
    exact.validate().ok()?;
    if exact.record.document_id != id {
        return None;
    }
    let relative = exact.record.path.strip_prefix('/')?;
    let path = Path::new(relative);
    if path
        .components()
        .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    let host = root.join(path);
    if let Some(expected) = expected
        && host.canonicalize().ok()? != expected
    {
        return None;
    }
    Some(WorkspaceNode {
        workspace: workspace.to_owned(),
        id,
        host,
        directory: exact.record.is_dir,
    })
}

/// The app's documents key, as the launcher computed it when it bound the app's view.
fn documents_key(caller: &Caller) -> Result<AppKey, String> {
    AppKey::new(&caller.workspace, &caller.app)
        .ok_or_else(|| tilo_dialogs::chooser_service::CANNOT_HAND.to_owned())
}

fn kind_of(choice: Choice) -> Kind {
    match choice {
        Choice::Read => Kind::Read,
        Choice::Folder { write } => Kind::Folder { write },
        Choice::Save => Kind::Save,
    }
}

fn granted_of(chosen: tilo_dialogs::chosen::Chosen) -> Granted {
    let choice = match chosen.kind {
        Kind::Read => Choice::Read,
        Kind::Folder { write } => Choice::Folder { write },
        Kind::Save => Choice::Save,
    };
    Granted {
        reference: chosen.reference,
        name: chosen.name,
        host: chosen.host,
        choice,
        save_sibling: chosen.save_sibling,
    }
}

/// The session's own file chooser, asked as the desktop portal asks it.
struct SessionPerson {
    questions: AskSender,
}

impl Person for SessionPerson {
    fn choose(
        &self,
        caller: &Caller,
        handle: String,
        question: &Question,
    ) -> Result<Vec<PathBuf>, Asked> {
        let questions = self.questions.clone();
        let caller = PortalCaller {
            workspace: Some(caller.workspace.clone()),
            sandboxed: false,
            ..PortalCaller::new(caller.app.clone())
        };
        let question = file_chooser_question(question);
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = async move {
            let _ = tx.send(
                tilo_portal::file_chooser::choose(&questions, &caller, handle, &question).await,
            );
        };
        match tilo_rt::try_app() {
            Ok(runtime) => {
                runtime.spawn(asked);
            }
            Err(error) => {
                tracing::warn!(%error, "no task asks the person for a file");
                return Err(Asked::Failed);
            }
        }
        match rx.recv() {
            Ok(Ok(paths)) => Ok(paths),
            Ok(Err(Ended::Cancelled)) => Err(Asked::Cancelled),
            Ok(Err(Ended::Failed)) | Err(_) => Err(Asked::Failed),
        }
    }
}

/// The question a chooser asks, as the session's file chooser shows it.
fn file_chooser_question(question: &Question) -> FileChooserQuestion {
    FileChooserQuestion {
        parent_window: None,
        title: question.title.clone(),
        save: question.save,
        directory: question.directory,
        writable_directory: question.writable_directory,
        multiple: question.multiple,
        accept_label: question.accept_label.clone(),
        current_name: question.current_name.clone(),
        filters: question.filters.iter().map(picker_filter).collect(),
    }
}

/// A filter the app named, as the session's file chooser shows one.
fn picker_filter(filter: &tilo_dialogs::portal::FileFilter) -> PickerFilter {
    let pairs: Vec<(u32, String)> = filter
        .globs()
        .iter()
        .map(|glob| (0, glob.clone()))
        .chain(filter.mimetypes().iter().map(|mime| (1, mime.clone())))
        .collect();
    PickerFilter::from_pairs(filter.name().to_owned(), &pairs)
}

#[cfg(test)]
mod tests;
