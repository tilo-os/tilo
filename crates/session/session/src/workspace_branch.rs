//! Branching a workspace: a new workspace of the person's own that starts with everything the
//! source holds and goes on apart from it (`docs/architecture/workspace.md`, "Branching a
//! workspace").
//!
//! The desk names the branch and its id the moment it is asked (`branch-workspace`,
//! [`tilo_desktop::desktop_host::DesktopAsk::BranchWorkspace`]); the session does the rest, off
//! the loop, so the desk never waits for a copy:
//!
//! 1. The branch is founded in this device's engine as any workspace made here is
//!    ([`Engine::found`]): its governance is new, its only member the person, as its owner, and
//!    its channels get keys of their own the first time a record of it is sealed.
//! 2. Every record the source holds here is read whole, with its history, and written into the
//!    branch as its founding write ([`Engine::found_record`]), forked under a writer of its own and
//!    naming the branch as its workspace ([`ui::record::transfer::branch`]). A record an app keeps on
//!    this device too is the same document there: the local copy's changes are merged in first,
//!    and the branch's local copy is the very document the branch's engine holds.
//! 3. What the apps keep for the workspace on this device (`tilo-rooms/<workspace>/<app>`) is
//!    copied, sharing extents where the file system can, every document in it forked the same way.
//! 4. The workspace's own files (`.workspaces/<workspace>`) are copied the same way.
//! 5. Its Terminal environments are branched by the runtime that keeps them
//!    (`ush`'s `branch-workspace`).
//!
//! Only then does the branch go on the desk, with the source's strips and a window for each of the
//! source's windows of this device, which open again in the branch. Nothing opens in it before its
//! records are there, so no app founds an empty record a copy would then collide with.
//!
//! What stays behind is what is tied to other people: every conversation in Chat ([`stays`]), each
//! person's own place in the workspace's tilolang apps, the desk's record (the branch's desk is
//! written from its own windows), other people's windows, who is here, the membership and its
//! invitations. A record this device cannot read is not branched; the person who branches is
//! someone who reads the whole workspace ([`Engine::reads_whole`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
    WorkspaceSlot,
};
use tilo_desktop_surface::notifications::NotificationIconKind;
use tilo_files::native::tree::{Copied, Strategy, clone_tree};

use crate::engine::{Engine, SealKeys};
use crate::platform::notice::Notice;

/// Chat's records: every conversation of the workspace, which stays behind.
const CHAT: &str = "so.tilo.chat";
/// Terminal's records: its environments, which the runtime branches itself.
const TERMINAL: &str = "so.tilo.terminal";
/// The record each person's place in the workspace's tilolang apps is kept in, one for all of them.
const PERSONAL_PLACES: (&str, &str) = ("tilo.host", "tilolang.personal");
/// How long a branch waits for what it reads and writes to be here: a record its hosts send only
/// once asked, and the keys of the branch's channels, which its first sealed write asks for.
const PATIENCE: Duration = Duration::from_secs(60);
/// How often a waiting read or write asks again.
const ASK_AGAIN: Duration = Duration::from_millis(100);

/// Whether the record `record` of `app` stays behind when its workspace is branched: what is tied
/// to other people, and what the branch makes of its own.
#[must_use]
pub(crate) fn stays(app: &str, record: &str) -> bool {
    match app {
        // Every conversation, and each person's drafts and read marks. The workspace's own emoji
        // and agents are Chat's to keep but the workspace's, so they come along.
        CHAT => collab_types::workspace_record(app, record).is_none(),
        // The runtime branches the environments, with their own files; the branch's desk is
        // written from its own windows, never the source's record of everyone's.
        TERMINAL | tilo_desktop_model::ownership::DESKTOP_APP => true,
        _ => collab_types::workspace_record(app, record)
            .is_some_and(|known| (known.app, known.kind) == PERSONAL_PLACES),
    }
}

/// Whether `request` is a branch of a workspace other than `workspace` , or of one the desk does
/// not know: `workspaces` are the desk's, each by its decimal id, its name and the id it is shared
/// under. An agent reads only the workspace it works in, so it branches that one or none.
pub(crate) fn branches_elsewhere(
    request: &tilo_actions::remote::InvokeRequest,
    workspaces: &[[String; 3]],
    workspace: &str,
) -> bool {
    let address = &request.target.address;
    if address.resource != tilo_desktop::desk_verbs::CHROME_RESOURCE
        || address.action != tilo_desktop::desktop_host::BRANCH_WORKSPACE
    {
        return false;
    }
    let source = match &request.argument {
        tilo_actions::remote::InvokeArgument::Json(arguments) => arguments
            .get("source")
            .and_then(serde_json::Value::as_str)
            .map(str::trim),
        _ => None,
    };
    let Some(source) = source else {
        return true;
    };
    // As the desk resolves it: by name first, then by either id.
    workspaces
        .iter()
        .find(|[_, name, _]| name.eq_ignore_ascii_case(source))
        .or_else(|| workspaces.iter().find(|[slot, _, _]| slot == source))
        .or_else(|| workspaces.iter().find(|[_, _, id]| id == source))
        .is_none_or(|[_, _, id]| id != workspace)
}

/// A branch being made: what the desk asked, and the copy under way.
pub(crate) struct Branching {
    source: WorkspaceId,
    source_slot: WorkspaceSlot,
    source_name: String,
    slot: WorkspaceSlot,
    workspace: WorkspaceId,
    name: String,
    copied: Receiver<Branched>,
}

/// The branches being made, followed on every pass.
#[derive(Default)]
pub(crate) struct Branches(std::cell::RefCell<Vec<Branching>>);

impl Branches {
    /// When the loop looks again at a branch being made: soon, while one is.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        (!self.0.borrow().is_empty())
            .then(|| now.checked_add(ASK_AGAIN))
            .flatten()
    }
}

/// What a branch copied, and what it could not.
#[derive(Debug, Default)]
pub(crate) struct Branched {
    /// The records written into the branch's engine.
    pub(crate) records: usize,
    /// The records left behind.
    pub(crate) left: usize,
    /// What the apps keep for the workspace on this device.
    pub(crate) app_data: Copied,
    /// The workspace's own files.
    pub(crate) files: Copied,
    /// The environments the runtime branched.
    pub(crate) environments: u64,
    /// What did not come along, and why, in words for the person.
    pub(crate) missing: Vec<String>,
}

/// Where this device keeps what a branch copies: the person's data home (the apps' directories
/// per workspace, `tilo-rooms`) and the device's files (each workspace's own tree in them).
#[derive(Clone, Debug, Default)]
pub(crate) struct Places {
    pub(crate) data_home: Option<PathBuf>,
    pub(crate) files: Option<PathBuf>,
    /// The runtime's socket, which branches the environments.
    pub(crate) runtime: Option<PathBuf>,
}

impl Places {
    /// This device's.
    pub(crate) fn here() -> Self {
        Self {
            data_home: tilo_paths::xdg::data_home(),
            files: tilo_paths::workspace_root().or_else(|| {
                let mount = PathBuf::from(tilo_paths::WORKSPACE_MOUNT);
                mount.is_dir().then_some(mount)
            }),
            runtime: Some(launcher::services::socket_for("ush")),
        }
    }
}

/// The source's records, read here, by app and record.
type Sources = BTreeMap<(String, String), AutoCommit>;

/// Where in the branch each app on this device keeps its copy of a record, by app and record: the
/// app that owns it, and every app that keeps it beside its own.
type Local = BTreeMap<(String, String), Vec<PathBuf>>;

/// Where a branch reads the source's records and writes the branch's: this device's engine.
pub(crate) trait Held: Sync {
    /// Whether `workspace` is held here.
    fn holds(&self, workspace: WorkspaceId) -> bool;
    /// Every record `workspace` holds here, by app and record.
    ///
    /// # Errors
    /// The workspace's records cannot be listed.
    fn list(&self, workspace: WorkspaceId) -> Result<Vec<(String, String)>, String>;
    /// `record` of `app` whole, with its history: `None` while it is not here yet.
    ///
    /// # Errors
    /// This device may not read it, or it cannot be read.
    fn read(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String>;
    /// Found `record` of `app` in `workspace` as `changes` , a whole document.
    ///
    /// # Errors
    /// The workspace refused it, or it could not be sealed.
    fn found(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        changes: &[u8],
    ) -> Result<(), String>;
}

impl Held for Engine {
    fn holds(&self, workspace: WorkspaceId) -> bool {
        Self::holds(self, workspace)
    }

    fn list(&self, workspace: WorkspaceId) -> Result<Vec<(String, String)>, String> {
        self.records(&workspace.to_string())
    }

    fn read(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        self.record(&workspace.to_string(), app, record)
    }

    /// Sealed under the keys of the record's channel in the branch, which its first sealed
    /// write asks for and this device, the branch's only member, makes; in the clear where the
    /// branch's records are documents this device commits.
    fn found(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        changes: &[u8],
    ) -> Result<(), String> {
        use tilo_action_broker::owner_commits::OwnerCustody as _;
        let sealed = if self.commits_documents(workspace) {
            None
        } else {
            Some(seal_keys(self, workspace, app, record)?)
        };
        pollster::block_on(self.found_record(workspace, (app, record), changes, sealed.as_ref()))
    }
}

/// Make the branch `to` of `from`: everything but the desk, which the loop makes when this is done.
/// Runs off the loop.
pub(crate) fn copy(
    engine: Option<&dyn Held>,
    places: &Places,
    from: WorkspaceId,
    to: WorkspaceId,
) -> Branched {
    let mut branched = Branched::default();
    let mut sources = match engine {
        Some(engine) if engine.holds(from) && engine.holds(to) => {
            read_records(engine, from, &mut branched)
        }
        // The source's records are this device's to read, and the branch holds none: it could not
        // be founded here.
        Some(engine) if engine.holds(from) => {
            branched
                .missing
                .push("the branch's records: this device could not found it".to_owned());
            Sources::new()
        }
        _ => Sources::new(),
    };
    let mut local: Local = BTreeMap::new();
    if let Some(data_home) = &places.data_home {
        match copy_app_data(data_home, from, to, &mut sources, &mut local) {
            Ok(copied) => branched.app_data = copied,
            Err(error) => branched.missing.push(error),
        }
    }
    if let Some(engine) = engine.filter(|engine| engine.holds(to)) {
        write_records(engine, to, sources, &local, &mut branched);
    } else {
        for ((app, record), doc) in sources {
            for path in local.get(&(app, record)).into_iter().flatten() {
                if let Err(error) = write_local(&doc, path, to) {
                    branched.missing.push(error);
                }
            }
        }
    }
    if let Some(files) = &places.files {
        match copy_files(files, from, to) {
            Ok(copied) => branched.files = copied,
            Err(error) => branched.missing.push(error),
        }
    }
    match branch_environments(places, from, to) {
        Ok(environments) => branched.environments = environments,
        Err(error) => branched.missing.push(error),
    }
    branched
}

/// Every record of `from` this device reads, but those that stay behind.
fn read_records(engine: &dyn Held, from: WorkspaceId, branched: &mut Branched) -> Sources {
    let mut sources = Sources::new();
    let routes = match engine.list(from) {
        Ok(routes) => routes,
        Err(error) => {
            branched.missing.push(error);
            return sources;
        }
    };
    let until = Instant::now().checked_add(PATIENCE);
    let patient = || until.is_some_and(|until| Instant::now() < until);
    for (app, record) in routes {
        if stays(&app, &record) {
            branched.left = branched.left.saturating_add(1);
            continue;
        }
        // A record the workspace's hosts send only once asked, or one whose keys are not here yet,
        // reads as nothing until it is here.
        let read = loop {
            match engine.read(from, &app, &record) {
                Ok(None) if patient() => std::thread::sleep(ASK_AGAIN),
                other => break other,
            }
        };
        match read.map(|bytes| bytes.map(|bytes| AutoCommit::load(&bytes))) {
            Ok(Some(Ok(doc))) => {
                sources.insert((app, record), doc);
            }
            Ok(Some(Err(error))) => branched.missing.push(format!("{record} of {app}: {error}")),
            Ok(None) => branched
                .missing
                .push(format!("{record} of {app} did not reach this device")),
            Err(error) => branched.missing.push(format!("{record} of {app}: {error}")),
        }
    }
    sources
}

/// Write each of `sources` into the branch `to` , as its own record and, where an app keeps it on
/// this device too, as that app's copy of it in the branch (`local`).
fn write_records(
    engine: &dyn Held,
    to: WorkspaceId,
    sources: Sources,
    local: &Local,
    branched: &mut Branched,
) {
    let workspace = to.to_string();
    for ((app, record), source) in sources {
        let mut doc = match ui::record::transfer::branch(&source, &workspace) {
            Ok(doc) => doc,
            Err(error) => {
                branched.missing.push(format!("{record} of {app}: {error}"));
                continue;
            }
        };
        let changes = doc.save_after(&[]);
        if let Err(error) = engine.found(to, &app, &record, &changes) {
            branched.missing.push(format!("{record} of {app}: {error}"));
            continue;
        }
        branched.records = branched.records.saturating_add(1);
        // Every app's copy of it on this device: the app that keeps it, and each app that keeps
        // it beside its own records (the workspace's emoji, in every app's store). A copy left
        // out would be made again by its app, as a genesis of its own the branch's record does
        // not share.
        let bytes = doc.save();
        for path in local
            .get(&(app.clone(), record.clone()))
            .into_iter()
            .flatten()
        {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(path, &bytes))
                .map_err(|error| format!("{record} of {app} on this device: {error}"));
            if let Err(error) = written {
                branched.missing.push(error);
            }
        }
    }
}

/// The keys `record` of `app` is sealed with in the branch: its first sealed write asks the
/// branch's channel for them, which this device, its only member, makes.
fn seal_keys(
    engine: &Engine,
    workspace: WorkspaceId,
    app: &str,
    record: &str,
) -> Result<channel_crypto::ChannelContentKeys, String> {
    let until = Instant::now().checked_add(PATIENCE);
    loop {
        let patient = until.is_some_and(|until| Instant::now() < until);
        match engine.seal_keys(workspace, app, record)? {
            SealKeys::Here(keys) => return Ok(keys),
            SealKeys::NoSecretStore => {
                return Err("this device holds no secret store to keep its keys in".to_owned());
            }
            SealKeys::Waiting if patient => std::thread::sleep(ASK_AGAIN),
            SealKeys::Waiting => return Err("the branch's keys did not come".to_owned()),
        }
    }
}

/// The relative path `path` (of the source's app directory) takes in the branch's, or `None` for
/// what stays behind or is written apart: Chat's and the desk's directories, the runtime's
/// environment records, and the documents themselves with what their store keeps beside them.
fn placed(path: &Path, from: &str, to: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path.iter().filter_map(|part| part.to_str()).collect();
    match parts.first() {
        // Chat's directory stays behind but for the workspace's own records Chat keeps (its emoji,
        // its agents), which come along in the branch's engine and so in Chat's store too.
        Some(&CHAT) if record_of(&parts, from).is_none_or(|(app, record)| stays(&app, &record)) => {
            return None;
        }
        Some(&tilo_desktop_model::ownership::DESKTOP_APP) => return None,
        Some(&TERMINAL) if environment_records(&parts, from) => return None,
        _ => {}
    }
    let name = parts.last().copied().unwrap_or_default();
    if [
        app_state::DOCUMENT_FILE,
        app_state::ACTOR_FILE,
        app_state::LOAD_HINT_FILE,
        app_state::LOCK_FILE,
        app_state::CANONICAL_GATE,
        "successor-claim",
    ]
    .contains(&name)
    {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|part| if *part == from { to } else { part })
            .collect(),
    )
}

/// Whether `parts` are in Terminal's environment records of the workspace `from`
/// (`…/so.tilo.terminal/rooms/<from>/records`), which the runtime branches. Another app's records
/// Terminal keeps beside its own (the workspace's emoji) are not: they come along as every copy of
/// them does.
fn environment_records(parts: &[&str], from: &str) -> bool {
    parts.windows(4).any(|window| {
        window
            == [
                TERMINAL,
                app_state::WORKSPACES_DIR,
                from,
                app_state::RECORDS_DIR,
            ]
    })
}

/// The app and record a document at `path` (relative to the workspace's app directories) is,
/// when it is a record: `<app dir>/…/<app>/rooms/<workspace>/records/<record>`.
fn record_of(parts: &[&str], workspace: &str) -> Option<(String, String)> {
    let at = parts.windows(3).position(|window| {
        window == [app_state::WORKSPACES_DIR, workspace, app_state::RECORDS_DIR]
    })?;
    let app = parts.get(at.checked_sub(1)?)?;
    let record = parts.get(at.checked_add(3)?)?;
    Some(((*app).to_owned(), (*record).to_owned()))
}

/// Copy what the apps keep for `from` on this device into `to`'s directories: every file as it is,
/// and every document forked into the branch. A record the engine holds too (`sources`) takes this
/// copy's changes in, and is written where `local` says once the branch's engine holds it.
fn copy_app_data(
    data_home: &Path,
    from: WorkspaceId,
    to: WorkspaceId,
    sources: &mut Sources,
    local: &mut Local,
) -> Result<Copied, String> {
    let rooms = data_home.join(launcher::appdir::WORKSPACES_DIR);
    let (from_id, to_id) = (from.to_string(), to.to_string());
    let source = rooms.join(launcher::appdir::workspace_slug(Some(&from_id)));
    if !source.is_dir() {
        return Ok(Copied::default());
    }
    let target = rooms.join(launcher::appdir::workspace_slug(Some(&to_id)));
    let mut copied = clone_tree(
        &source,
        &target,
        Strategy::Clone,
        &mut |path| placed(path, &from_id, &to_id),
        &mut |_| {},
    )
    .map_err(|error| format!("what the apps keep for the workspace: {error}"))?;
    for document in documents(&source).map_err(|error| error.to_string())? {
        let Ok(relative) = document.strip_prefix(&source) else {
            continue;
        };
        let Some(directory) = relative.parent() else {
            continue;
        };
        // A document's place is its directory's: its file is never placed on its own.
        let Some(place) = placed(&directory.join("_"), &from_id, &to_id) else {
            continue;
        };
        let into = target.join(place.with_file_name(app_state::DOCUMENT_FILE));
        let parts: Vec<&str> = relative.iter().filter_map(|part| part.to_str()).collect();
        let bytes = std::fs::read(&document).map_err(|error| error.to_string())?;
        let loaded = AutoCommit::load_with_options(
            &bytes,
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore),
        )
        .map_err(|error| format!("{}: {error}", relative.display()));
        let mut held = loaded?;
        if let Some(route) = record_of(&parts, &from_id)
            && let Some(source) = sources.get_mut(&route)
        {
            // The same record, as an app on this device last wrote it: its changes are the
            // workspace's too.
            tilo_automerge::ApplyIncoming::merge_incoming(source, &mut held)
                .map_err(|error| format!("{}: {error}", relative.display()))?;
            local.entry(route).or_default().push(into);
        } else {
            write_local(&held, &into, to)?;
        }
        copied.files = copied.files.saturating_add(1);
        copied.bytes = copied
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    }
    Ok(copied)
}

/// Write the branch's copy of `doc` to `path` , a document of an app's directory in the branch.
fn write_local(doc: &AutoCommit, path: &Path, to: WorkspaceId) -> Result<(), String> {
    let mut copy = ui::record::transfer::branch(doc, &to.to_string())
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(path, copy.save()).map_err(|error| format!("{}: {error}", path.display()))
}

/// Every document under `root` .
fn documents(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut waiting = vec![root.to_path_buf()];
    while let Some(directory) = waiting.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                waiting.push(entry.path());
            } else if kind.is_file() && entry.file_name() == app_state::DOCUMENT_FILE {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Copy the workspace `from`'s own files into `to`'s tree.
fn copy_files(files: &Path, from: WorkspaceId, to: WorkspaceId) -> Result<Copied, String> {
    let (Some(source), Some(target)) = (
        tilo_paths::workspace_tree(files, &from.to_string()),
        tilo_paths::workspace_tree(files, &to.to_string()),
    ) else {
        return Err("the workspace has no tree of files".to_owned());
    };
    if !source.is_dir() {
        return Ok(Copied::default());
    }
    clone_tree(
        &source,
        &target,
        Strategy::Clone,
        &mut |path| Some(path.to_path_buf()),
        &mut |_| {},
    )
    .map_err(|error| format!("the workspace's files: {error}"))
}

/// Ask the runtime to branch `from`'s environments into `to`. A workspace that has none needs no
/// runtime to ask.
fn branch_environments(places: &Places, from: WorkspaceId, to: WorkspaceId) -> Result<u64, String> {
    let has_environments = places.data_home.as_ref().is_some_and(|data_home| {
        let room = from.to_string();
        data_home
            .join(launcher::appdir::WORKSPACES_DIR)
            .join(&room)
            .join(TERMINAL)
            .join(".local/share")
            .join(TERMINAL)
            .join(app_state::WORKSPACES_DIR)
            .join(&room)
            .join(app_state::RECORDS_DIR)
            .is_dir()
    });
    if !has_environments {
        return Ok(0);
    }
    let socket = places
        .runtime
        .as_deref()
        .ok_or("the Terminal environments: no runtime here")?;
    ask_runtime(socket, from, to).map_err(|error| format!("the Terminal environments: {error}"))
}

/// One `branch-workspace` request of the runtime at `socket` , answered with how many
/// environments the branch has.
fn ask_runtime(socket: &Path, from: WorkspaceId, to: WorkspaceId) -> Result<u64, String> {
    use std::io::{BufRead as _, Write as _};
    let stream = tilo_files::native::socket::connect(socket).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(600)))
        .map_err(|error| error.to_string())?;
    let mut line = serde_json::to_vec(&serde_json::json!({
        "op": "branch-workspace",
        "workspace": from.to_string(),
        "into": to.to_string(),
    }))
    .map_err(|error| error.to_string())?;
    line.push(b'\n');
    (&stream)
        .write_all(&line)
        .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    std::io::BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|error| error.to_string())?;
    let reply: serde_json::Value =
        serde_json::from_str(reply.trim()).map_err(|error| error.to_string())?;
    match reply.get("result").and_then(serde_json::Value::as_str) {
        Some("branched") => Ok(reply
            .get("environments")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()),
        _ => Err(reply
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("the runtime did not branch them")
            .to_owned()),
    }
}

/// The branch's own desk: `source`'s strips, and a window for each of the windows this device
/// shows in it — opened again in the branch by its app — with ids of its own (`mint`). Other
/// people's windows stay theirs. What the workspace grants its apps comes along, its own files
/// named as the branch's.
pub(crate) fn desk_of(
    source: &Workspace,
    slot: WorkspaceSlot,
    workspace: WorkspaceId,
    name: &str,
    files: Option<&Path>,
    mint: &mut dyn FnMut() -> u64,
) -> Workspace {
    let mut branch = Workspace::in_workspace(slot, workspace, name);
    let trees = files.map(|files| {
        (
            tilo_paths::workspace_tree(files, &source.id().to_string()),
            tilo_paths::workspace_tree(files, &workspace.to_string()),
        )
    });
    branch.sandbox = source
        .sandbox
        .iter()
        .cloned()
        .map(|mut grant| {
            if let (collab_types::caps::Scope::FsPath(path), Some((Some(from), Some(to)))) =
                (&grant.scope, &trees)
                && let Ok(rest) = Path::new(path).strip_prefix(from)
            {
                grant.scope =
                    collab_types::caps::Scope::FsPath(to.join(rest).to_string_lossy().into_owned());
            }
            grant
        })
        .collect();
    branch.presets.clone_from(&source.presets);
    branch.default_apps = source.default_apps.clone();
    for strip in &source.strips {
        let mut made = Strip::new(StripId(mint()), strip.name.clone());
        for group in &strip.window_groups {
            let tabs: Vec<Tab> = group
                .tabs
                .iter()
                .filter_map(|tab| {
                    let app = match &tab.tile {
                        TileContent::Foreign { app_id, .. }
                        | TileContent::LocalApp { app_id, .. } => app_id.clone(),
                        _ => return None,
                    };
                    (!app.trim().is_empty()).then(|| Tab {
                        id: mint(),
                        title: tab.title.clone(),
                        tile: TileContent::LocalApp {
                            app_id: app,
                            label: tab.title.clone(),
                            note: String::new(),
                        },
                        app_title: None,
                        view: tab.view.clone(),
                        pinned: tab.pinned,
                    })
                })
                .collect();
            let Some(front) = tabs.first().map(|tab| tab.id) else {
                continue;
            };
            made.window_groups.push(WindowGroup {
                id: mint(),
                tabs,
                active_tab: front,
                width: group.width,
                maximized: false,
                focus_history: Vec::new(),
            });
        }
        branch.strips.push(made);
    }
    if branch.strips.is_empty() {
        branch.strips.push(Strip::new(StripId(mint()), "Main"));
    }
    branch
}

impl crate::Shared {
    /// The desk branched the workspace `source` into `workspace` , called `name` , in `slot` : found
    /// it here and copy what the source holds into it, off the loop.
    pub(crate) fn branch_workspace(
        &self,
        source: WorkspaceSlot,
        slot: WorkspaceSlot,
        workspace: WorkspaceId,
        name: String,
    ) {
        let Some((from, source_name)) = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.slot == source)
                .map(|held| (held.id(), held.name.clone()))
        }) else {
            self.handle
                .acknowledge(tilo_desktop_surface::i18n::t("desktop-workspace-gone"));
            return;
        };
        let engine = self.engine.clone();
        if let Some(engine) = &engine {
            let endpoint = self.sync.as_ref().map(|sync| sync.endpoint_addr());
            engine.found(slot, workspace, &name, endpoint.as_ref());
        }
        let places = Places::here();
        let (done, copied) = std::sync::mpsc::channel();
        let started = std::thread::Builder::new()
            .name("workspace-branch".to_owned())
            .spawn(move || {
                let held = engine.as_deref().map(|engine| engine as &dyn Held);
                let branched = copy(held, &places, from, workspace);
                let _ = done.send(branched);
            });
        if let Err(error) = started {
            tracing::warn!(%error, "a branch of a workspace could not start");
            self.handle.acknowledge(error.to_string());
            return;
        }
        self.telling().show(
            &Notice::new(
                String::new(),
                1,
                tilo_desktop_surface::i18n::t_args(
                    "session-branching-workspace",
                    &[("name", source_name.clone().into())],
                ),
                tilo_desktop_surface::i18n::t_args(
                    "session-branching-workspace-body",
                    &[("branch", name.clone().into())],
                ),
            )
            .about(NotificationIconKind::Workspace),
        );
        self.branches.0.borrow_mut().push(Branching {
            source: from,
            source_slot: source,
            source_name,
            slot,
            workspace,
            name,
            copied,
        });
    }

    /// Put each branch whose copy is done on the desk, with the source's strips and windows, and go
    /// there when the person is still where they asked for it; say what did not come along.
    pub(crate) fn follow_branches(&self) {
        let mut landed = Vec::new();
        self.branches
            .0
            .borrow_mut()
            .retain(|branching| match branching.copied.try_recv() {
                Ok(branched) => {
                    landed.push((
                        branching.source,
                        branching.source_slot,
                        branching.source_name.clone(),
                        branching.slot,
                        branching.workspace,
                        branching.name.clone(),
                        branched,
                    ));
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => false,
            });
        for (source, source_slot, source_name, slot, workspace, name, branched) in landed {
            self.land_branch(
                (source, source_slot, &source_name),
                (slot, workspace, &name),
                &branched,
            );
        }
    }

    fn land_branch(
        &self,
        (source, source_slot, source_name): (WorkspaceId, WorkspaceSlot, &str),
        (slot, workspace, name): (WorkspaceSlot, WorkspaceId, &str),
        branched: &Branched,
    ) {
        let mut tree: DesktopTree = self.handle.read(Clone::clone);
        let files = Places::here().files;
        let made = tree
            .workspaces
            .iter()
            .find(|held| held.id() == source)
            .map(|held| {
                desk_of(held, slot, workspace, name, files.as_deref(), &mut || {
                    self.handle.mint_id()
                })
            });
        if let Some(mut branch) = made
            && !tree.workspaces.iter().any(|held| held.id() == workspace)
        {
            branch.set_owner(self.person.clone());
            let first = branch.strips.first().map(|strip| strip.id);
            tree.workspaces.push(branch);
            if let Some(first) = first {
                tree.active_strip.insert(slot, first);
            }
            // The person goes to the branch they asked for, unless they went elsewhere meanwhile.
            if tree.active == source_slot {
                tree.active = slot;
            }
            self.handle.replace_tree(tree.clone());
            #[cfg(target_os = "linux")]
            self.restore.borrow_mut().add(&tree, slot);
        }
        tracing::info!(
            %source,
            %workspace,
            records = branched.records,
            left = branched.left,
            app_files = branched.app_data.files,
            files = branched.files.files,
            cloned = branched.files.cloned,
            environments = branched.environments,
            missing = branched.missing.len(),
            "a workspace was branched"
        );
        let (urgency, title, body) = if branched.missing.is_empty() {
            (
                1,
                tilo_desktop_surface::i18n::t_args(
                    "session-branched-workspace",
                    &[("branch", name.to_owned().into())],
                ),
                tilo_desktop_surface::i18n::t_args(
                    "session-branched-workspace-body",
                    &[("name", source_name.to_owned().into())],
                ),
            )
        } else {
            for missing in &branched.missing {
                tracing::warn!(%workspace, %missing, "a branch of a workspace is not complete");
            }
            (
                1,
                tilo_desktop_surface::i18n::t_args(
                    "session-branch-failed",
                    &[("branch", name.to_owned().into())],
                ),
                branched.missing.join("\n"),
            )
        };
        self.telling().show(
            &Notice::new(String::new(), urgency, title, body)
                .about(NotificationIconKind::Workspace),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FROM: &str = "7a6b5c4d-0000-4000-8000-000000000001";
    const TO: &str = "7a6b5c4d-0000-4000-8000-000000000002";

    fn ids() -> (WorkspaceId, WorkspaceId) {
        (
            WorkspaceId::parse(FROM).unwrap(),
            WorkspaceId::parse(TO).unwrap(),
        )
    }

    #[test]
    fn chat_and_what_names_other_people_stay_behind_and_the_rest_comes_along() {
        let emoji = collab_types::workspace_record_id("tilo.workspace.emoji");
        let agents = collab_types::workspace_record_id("chat.agent");
        assert!(stays(CHAT, "messages"), "every conversation");
        assert!(stays(
            CHAT,
            &collab_types::workspace_record_id("chat.workspace")
        ));
        assert!(!stays(CHAT, &emoji), "the workspace's own emoji");
        assert!(!stays(CHAT, &agents), "the workspace's agents");
        assert!(
            stays(TERMINAL, "env-1"),
            "the runtime branches its environments"
        );
        assert!(stays(tilo_desktop_model::ownership::DESKTOP_APP, "desktop"));
        assert!(stays(
            "tilo.host",
            &collab_types::workspace_record_id("tilolang.personal")
        ));
        assert!(!stays("so.tilo.notes", "uuid:plan"));
    }

    /// A record document as an app keeps it on this device.
    fn record(workspace: &str, body: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        assert!(ui::record::init_scoped(
            &mut doc,
            &ui::record::RecordId::minted(),
            &ui::record::RecordKind::new("notes.page"),
            workspace,
            Some(workspace),
        ));
        automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "body", body).unwrap();
        doc.commit();
        doc
    }

    fn body(doc: &AutoCommit) -> String {
        use automerge::ReadDoc as _;
        match doc.get(automerge::ROOT, "body").unwrap() {
            Some((automerge::Value::Scalar(value), _)) => value.as_str().unwrap().to_owned(),
            other => panic!("no body: {other:?}"),
        }
    }

    fn load(path: &Path) -> AutoCommit {
        AutoCommit::load(&std::fs::read(path).unwrap()).unwrap()
    }

    /// What the apps keep for a workspace comes along with every document forked into the branch,
    /// without the source's writer or locks, and Chat's and the environments' stay behind; the
    /// workspace's own files come along; the source keeps all of it as it was.
    #[test]
    fn the_apps_data_and_the_files_of_the_workspace_come_along_and_chat_stays() {
        let (from, to) = ids();
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("data");
        let files = home.path().join("files");
        let rooms = data.join("tilo-rooms");
        let notes = rooms
            .join(FROM)
            .join("so.tilo.notes/.local/share/so.tilo.notes/rooms")
            .join(FROM)
            .join("records/uuid-plan");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("state.automerge"), record(FROM, "plan").save()).unwrap();
        std::fs::write(notes.join("actor"), b"source writer").unwrap();
        std::fs::write(notes.join("writer.lock"), b"").unwrap();
        let settings = rooms.join(FROM).join("so.tilo.notes/.config/notes.toml");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, b"theme = 'dark'").unwrap();
        let chat = rooms
            .join(FROM)
            .join("so.tilo.chat/.local/share/so.tilo.chat/rooms")
            .join(FROM);
        std::fs::create_dir_all(chat.join("records/messages")).unwrap();
        std::fs::write(
            chat.join("records/messages/state.automerge"),
            record(FROM, "hi").save(),
        )
        .unwrap();
        let environment = rooms
            .join(FROM)
            .join("so.tilo.terminal/.local/share/so.tilo.terminal/rooms")
            .join(FROM)
            .join("records/env-1");
        std::fs::create_dir_all(&environment).unwrap();
        std::fs::write(environment.join("state.automerge"), b"an environment").unwrap();
        let tree = files.join(".workspaces").join(FROM);
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/main.rs"), b"fn main() {}").unwrap();

        let places = Places {
            data_home: Some(data),
            files: Some(files.clone()),
            // A workspace with environments asks the runtime, and there is none here.
            runtime: None,
        };
        let branched = copy(None, &places, from, to);
        assert_eq!(
            branched.missing,
            vec!["the Terminal environments: no runtime here".to_owned()],
            "only the environments, which the runtime branches, did not come"
        );

        let copied = rooms
            .join(TO)
            .join("so.tilo.notes/.local/share/so.tilo.notes/rooms")
            .join(TO)
            .join("records/uuid-plan");
        let doc = load(&copied.join("state.automerge"));
        assert_eq!(body(&doc), "plan");
        assert_eq!(ui::record::workspace_scope(&doc).as_deref(), Some(TO));
        assert!(
            !copied.join("actor").exists(),
            "the branch writes as a writer of its own"
        );
        assert!(!copied.join("writer.lock").exists());
        assert_eq!(
            std::fs::read(rooms.join(TO).join("so.tilo.notes/.config/notes.toml")).unwrap(),
            b"theme = 'dark'"
        );
        assert!(
            !rooms.join(TO).join("so.tilo.chat").exists(),
            "chat stays behind"
        );
        assert!(
            !rooms
                .join(TO)
                .join("so.tilo.terminal/.local/share/so.tilo.terminal/rooms")
                .join(TO)
                .join("records")
                .exists(),
            "the runtime branches the environments, not a copy of their files"
        );
        assert_eq!(
            std::fs::read(files.join(".workspaces").join(TO).join("src/main.rs")).unwrap(),
            b"fn main() {}"
        );

        // The source is as it was, and an edit in either stays there.
        let source = load(&notes.join("state.automerge"));
        assert_eq!(ui::record::workspace_scope(&source).as_deref(), Some(FROM));
        let mut edited = doc;
        automerge::transaction::Transactable::put(&mut edited, automerge::ROOT, "body", "branch")
            .unwrap();
        edited.commit();
        std::fs::write(copied.join("state.automerge"), edited.save()).unwrap();
        assert_eq!(body(&load(&notes.join("state.automerge"))), "plan");
        std::fs::write(
            files.join(".workspaces").join(TO).join("src/main.rs"),
            b"changed",
        )
        .unwrap();
        assert_eq!(
            std::fs::read(tree.join("src/main.rs")).unwrap(),
            b"fn main() {}"
        );
    }

    /// The branch's desk has the source's strips and a window for each of this device's, made
    /// again by their apps; another person's window stays theirs.
    #[test]
    fn the_branchs_desk_has_the_sources_strips_and_this_devices_windows() {
        let (from, to) = ids();
        let mut source = Workspace::in_workspace(WorkspaceSlot(1), from, "Research");
        let tab = |id, tile| Tab {
            id,
            title: format!("window {id}"),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut strip = Strip::new(StripId(2), "Reading");
        strip.window_groups.push(WindowGroup {
            id: 3,
            tabs: vec![
                tab(
                    4,
                    TileContent::Foreign {
                        app_id: "so.tilo.notes".into(),
                        surface_id: 9,
                    },
                ),
                tab(
                    5,
                    TileContent::PeerWindow {
                        app_id: "so.tilo.browser".into(),
                        label: "theirs".into(),
                        note: String::new(),
                        missing: None,
                    },
                ),
            ],
            active_tab: 4,
            width: tilo_desktop_model::WindowWidth::Wide,
            maximized: true,
            focus_history: Vec::new(),
        });
        source.strips.push(strip);
        source.strips.push(Strip::new(StripId(6), "Writing"));
        let mut next = 100;
        let branch = desk_of(
            &source,
            WorkspaceSlot(7),
            to,
            "Research branch",
            None,
            &mut || {
                next += 1;
                next
            },
        );
        assert_eq!(branch.id(), to);
        assert_eq!(branch.name, "Research branch");
        let names: Vec<&str> = branch
            .strips
            .iter()
            .map(|strip| strip.name.as_str())
            .collect();
        assert_eq!(names, ["Reading", "Writing"]);
        let tabs: Vec<&Tab> = branch.tabs().collect();
        assert_eq!(tabs.len(), 1, "another person's window stays theirs");
        assert!(matches!(
            &tabs[0].tile,
            TileContent::LocalApp { app_id, .. } if app_id == "so.tilo.notes"
        ));
        assert!(tabs[0].id > 100, "ids of its own");
        assert!(branch.strips[0].id.0 > 100);
    }

    /// This device's engine, reading and sealing with a channel's keys the test holds itself, as
    /// the channel service hands them to a member.
    struct Keyed<'a> {
        engine: &'a Engine,
        keys: channel_crypto::ChannelContentKeys,
    }

    impl Held for Keyed<'_> {
        fn holds(&self, workspace: WorkspaceId) -> bool {
            self.engine.holds(workspace)
        }

        fn list(&self, workspace: WorkspaceId) -> Result<Vec<(String, String)>, String> {
            self.engine.records(&workspace.to_string())
        }

        fn read(
            &self,
            workspace: WorkspaceId,
            app: &str,
            record: &str,
        ) -> Result<Option<Vec<u8>>, String> {
            self.engine.record_opened_with(
                workspace,
                (app, record),
                &self.unit(workspace, app, record),
            )
        }

        fn found(
            &self,
            workspace: WorkspaceId,
            app: &str,
            record: &str,
            changes: &[u8],
        ) -> Result<(), String> {
            pollster::block_on(self.engine.found_record(
                workspace,
                (app, record),
                changes,
                Some(&self.unit(workspace, app, record)),
            ))
        }
    }

    impl Keyed<'_> {
        /// The keys as the record's own unit uses them, as the channel service names them for it.
        fn unit(
            &self,
            workspace: WorkspaceId,
            app: &str,
            record: &str,
        ) -> channel_crypto::ChannelContentKeys {
            self.keys.for_unit(collab_types::document_channel_id(
                app,
                &workspace.to_string(),
                record,
            ))
        }
    }

    /// A device of a person, certified for their root, that owns workspaces.
    /// This device's engine, opened again under `state` as a session that starts afresh does.
    fn reopened(
        state: &tempfile::TempDir,
        identity: &tilo_identity_host::custody::HostIdentity,
    ) -> Engine {
        Engine::open(
            identity.clone(),
            Some(state.path()),
            Vec::new(),
            crate::platform::custody::of_this_desk(),
        )
        .expect("this device's engine opens")
    }

    fn identity() -> tilo_identity_host::custody::HostIdentity {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Laptop".to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        tilo_identity_host::custody::HostIdentity::kept(cert, device)
    }

    /// Write `doc` into `workspace` as `record` of `app` , sealed, as an app's first write is.
    fn write(
        held: &Keyed<'_>,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        doc: &mut AutoCommit,
    ) {
        held.found(workspace, app, record, &doc.save_after(&[]))
            .expect("the workspace takes the record");
    }

    /// A record that comes along is one document wherever this device keeps it: the engine's copy,
    /// Chat's own copy of the workspace's emoji and the copy another app keeps beside its own records
    /// are the branch's same document, so no app of the branch makes the record again as a genesis
    /// the branch's record does not share (which left Chat's committed history blocked on a
    /// restart); the conversation Chat keeps stays behind.
    #[test]
    fn every_copy_of_a_record_that_comes_along_is_the_branchs_one_document() {
        let state = tempfile::tempdir().unwrap();
        let identity = identity();
        let engine = reopened(&state, &identity);
        let (from, to) = ids();
        engine.found(WorkspaceSlot(1), from, "Research", None);
        engine.found(WorkspaceSlot(2), to, "Research branch", None);
        let keys = channel_crypto::ChannelGroup::create(
            collab_types::workspace_scope(&to.to_string()),
            channel_crypto::MemberKey::generate("owner").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap();
        let held = Keyed {
            engine: &engine,
            keys,
        };
        let emoji = collab_types::workspace_record_id("tilo.workspace.emoji");
        let mut party = record(FROM, "party-parrot");
        write(&held, from, (CHAT, &emoji), &mut party);
        let home = tempfile::tempdir().unwrap();
        let rooms = home.path().join("data/tilo-rooms");
        // Where an app directory `app` keeps `owner`'s record `record` of `workspace`.
        let kept = |workspace: &str, app: &str, owner: &str, record: &str| {
            rooms
                .join(workspace)
                .join(app)
                .join(".local/share")
                .join(owner)
                .join("rooms")
                .join(workspace)
                .join("records")
                .join(record)
                .join("state.automerge")
        };
        for app in [CHAT, TERMINAL] {
            let path = kept(FROM, app, CHAT, &emoji);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, party.save()).unwrap();
        }
        let conversation = kept(FROM, CHAT, CHAT, "messages");
        std::fs::create_dir_all(conversation.parent().unwrap()).unwrap();
        std::fs::write(&conversation, record(FROM, "see you at nine").save()).unwrap();

        let places = Places {
            data_home: Some(home.path().join("data")),
            files: None,
            runtime: None,
        };
        let branched = copy(Some(&held), &places, from, to);
        assert!(branched.missing.is_empty(), "{:?}", branched.missing);

        let mut engines = AutoCommit::load(
            &held
                .read(to, CHAT, &emoji)
                .unwrap()
                .expect("the branch holds the emoji"),
        )
        .unwrap();
        for app in [CHAT, TERMINAL] {
            let path = kept(TO, app, CHAT, &emoji);
            assert!(path.exists(), "{app}'s copy of the emoji came along");
            let mut copy = load(&path);
            assert_eq!(body(&copy), "party-parrot");
            assert_eq!(ui::record::workspace_scope(&copy).as_deref(), Some(TO));
            assert_eq!(
                copy.get_heads(),
                engines.get_heads(),
                "{app}'s copy is the branch's document, not a fork of its own"
            );
        }
        assert!(
            !kept(TO, CHAT, CHAT, "messages").exists(),
            "the conversation stays behind"
        );
    }

    /// A workspace with notes, the workspace's emoji and a conversation, branched: the branch
    /// holds the notes and the emoji with their history, sealed under its own channel and naming
    /// it, and not the conversation; its only member is the person, as its owner; the source holds
    /// what it held. All of it is as it was after the engine opens again.
    #[test]
    fn a_branch_holds_the_sources_records_but_its_chat_and_only_the_person() {
        let state = tempfile::tempdir().unwrap();
        let identity = identity();
        let person = hex::encode(identity.device_cert.user_public_key);
        let engine = reopened(&state, &identity);
        let (from, to) = ids();
        engine.found(WorkspaceSlot(1), from, "Research", None);
        engine.found(WorkspaceSlot(2), to, "Research branch", None);
        assert!(engine.holds(from) && engine.holds(to));
        let channel = collab_types::workspace_scope(&to.to_string());
        let keys = channel_crypto::ChannelGroup::create(
            channel,
            channel_crypto::MemberKey::generate("owner").unwrap(),
        )
        .unwrap()
        .content_keys()
        .unwrap();
        let held = Keyed {
            engine: &engine,
            keys,
        };
        let notes = ("so.tilo.notes", "uuid-plan");
        let emoji = collab_types::workspace_record_id("tilo.workspace.emoji");
        let mut plan = record(FROM, "plan");
        write(&held, from, notes, &mut plan);
        write(
            &held,
            from,
            (CHAT, &emoji),
            &mut record(FROM, "party-parrot"),
        );
        write(
            &held,
            from,
            (CHAT, "messages"),
            &mut record(FROM, "see you at nine"),
        );

        let branched = copy(Some(&held), &Places::default(), from, to);
        assert!(branched.missing.is_empty(), "{:?}", branched.missing);
        assert_eq!((branched.records, branched.left), (2, 1));

        let read = |workspace, route| {
            AutoCommit::load(
                &held
                    .read(workspace, notes.0, route)
                    .unwrap()
                    .expect("the record is held"),
            )
            .unwrap()
        };
        let mut copied = read(to, notes.1);
        assert_eq!(body(&copied), "plan");
        assert_eq!(ui::record::workspace_scope(&copied).as_deref(), Some(TO));
        for change in plan.get_heads() {
            assert!(
                copied.get_change_by_hash(&change).is_some(),
                "the branch holds the record's history"
            );
        }
        let routes = engine.records(TO).unwrap();
        assert!(routes.contains(&(CHAT.to_owned(), emoji)), "{routes:?}");
        assert!(
            !routes
                .iter()
                .any(|(app, record)| app == CHAT && record == "messages"),
            "the conversation stays behind: {routes:?}"
        );
        // Its owner is the person who branched it, and nobody else is in it.
        assert_eq!(engine.owner_of(TO), Ok(person.clone()));
        let people = engine.people(TO).unwrap();
        assert!(
            people.iter().all(|member| member.person == person),
            "nobody but the person: {people:?}"
        );
        assert!(
            engine.reads_whole(to),
            "the person reads all of their branch"
        );

        // The source holds what it held, and the two are documents of their own.
        let mut source = read(from, notes.1);
        assert_eq!(ui::record::workspace_scope(&source).as_deref(), Some(FROM));
        assert_ne!(source.get_heads(), copied.get_heads());
        assert!(
            engine
                .records(FROM)
                .unwrap()
                .contains(&(CHAT.to_owned(), "messages".to_owned()))
        );

        drop(held);
        drop(engine);
        let engine = reopened(&state, &identity);
        assert!(
            engine.holds(to),
            "the branch is this device's after a restart"
        );
        assert_eq!(engine.owner_of(TO), Ok(person));
        assert_eq!(engine.records(TO).unwrap().len(), 2);
    }
    /// An agent branches the workspace it works in, by any name the desk knows it by, and no
    /// other: what it would copy of another is not its to read.
    #[test]
    fn an_agent_branches_its_own_workspace_and_no_other() {
        let workspaces = [
            ["1".to_owned(), "Studio".to_owned(), "studio-id".to_owned()],
            ["2".to_owned(), "Garden".to_owned(), "garden-id".to_owned()],
        ];
        let branch = |source: Option<&str>| tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    tilo_desktop::desk_verbs::CHROME_RESOURCE,
                    tilo_desktop::desktop_host::BRANCH_WORKSPACE,
                ),
            },
            argument: source.map_or(tilo_actions::remote::InvokeArgument::None, |source| {
                tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
                    "source".to_owned(),
                    serde_json::Value::from(source),
                )]))
            }),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        for own in ["Studio", "studio", "1", "studio-id"] {
            assert!(
                !branches_elsewhere(&branch(Some(own)), &workspaces, "studio-id"),
                "its own workspace, as {own}"
            );
        }
        for other in ["Garden", "2", "garden-id", "nowhere"] {
            assert!(
                branches_elsewhere(&branch(Some(other)), &workspaces, "studio-id"),
                "{other} is not its own"
            );
        }
        assert!(
            branches_elsewhere(&branch(None), &workspaces, "studio-id"),
            "a branch that names nothing names nothing of its own"
        );
        let mut other_verb = branch(Some("Garden"));
        other_verb.target.address.action = "new-workspace".to_owned();
        assert!(!branches_elsewhere(&other_verb, &workspaces, "studio-id"));
    }
}
