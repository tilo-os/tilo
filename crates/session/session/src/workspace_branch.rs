//! Branching a workspace: a new workspace of the person's own that starts with everything the
//! source holds and goes on apart from it (`docs/architecture/workspace.md`, "Branching a
//! workspace").
//!
//! The session names the branch and its id the moment it is asked (`branch-workspace`,
//! [`tilo_desktop::desktop_host::DesktopAsk::BranchWorkspace`]); the session does the rest
//! without holding its loop, so the session never waits for a copy:
//!
//! 1. The branch is founded in this device's engine as any workspace made here is
//!    ([`Engine::found`]): its governance is new, its only member the person, as its owner, and
//!    its channels get keys of their own the first time a record of it is sealed.
//! 2. Every record the source holds here is read whole, with its history, and written into the
//!    branch as its founding write, through this device's workspace service
//!    ([`tilo_peer_state::workspace_documents::DeviceWrites`]), forked under a writer of its own and
//!    naming the branch as its workspace ([`ui::resource::transfer::branch`]). A record an app keeps on
//!    this device too is the same document there: the local copy's changes are merged in first,
//!    and the branch's local copy is the very document the branch's engine holds.
//! 3. What the apps keep for the workspace on this device (`tilo-workspaces/<workspace>/<app>`) is
//!    copied, sharing extents where the file system can, every document in it forked the same way.
//! 4. The workspace's own files (`.workspaces/<workspace>`) are copied the same way.
//! 5. Its Terminal environments are branched by the runtime that keeps them
//!    (`ush`'s `branch-workspace`).
//!
//! Only then does the branch go on the shell, with the source's strips and a window for each of the
//! source's windows of this device, which open again in the branch. Nothing opens in it before its
//! records are there, so no app founds an empty record a copy would then collide with.
//!
//! What stays behind is what is tied to other people: every conversation in Chat ([`stays`]), each
//! person's own place in the workspace's tilolang apps, the session's record (the branch's session is
//! written from its own windows), other people's windows, who is here, the membership and its
//! invitations. A record this device cannot read is not branched; the person who branches is
//! someone who reads the whole workspace ([`Engine::reads_whole`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
// What stays behind is one rule for a device and for a host that branches a workspace.
use tilo_desktop_model::branch::{CHAT, TERMINAL, stays};
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, Workspace, WorkspaceId,
};
use tilo_desktop_surface::notifications::NotificationIconKind;
use tilo_files::native::tree::{Copied, Strategy, clone_tree};

use crate::engine::{Engine, SealKeys};
use crate::engine_write::Polled;
use crate::platform::notice::Notice;

/// How long a branch waits for what it reads and writes to be here: a record its hosts send only
/// once asked, and the keys of the branch's channels, which its first sealed write asks for.
const PATIENCE: Duration = Duration::from_secs(60);

/// Whether `request` is a branch of a workspace other than `workspace` , or of one the session does
/// not know: `workspaces` are the session's, each by its id and its name. An agent reads only the workspace it works in, so it branches that one or none.
pub(crate) fn branches_elsewhere(
    request: &tilo_actions::remote::InvokeRequest,
    workspaces: &[[String; 2]],
    workspace: &str,
) -> bool {
    let address = &request.target.address;
    if address.resource != tilo_desktop::verbs::CHROME_RESOURCE
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
    // As the session resolves it: by name first, then by id.
    workspaces
        .iter()
        .find(|[_, name]| name.eq_ignore_ascii_case(source))
        .or_else(|| workspaces.iter().find(|[id, _]| id == source))
        .is_none_or(|[id, _]| id != workspace)
}

/// A branch being made: what the session asked, and the copy under way.
pub(crate) struct Branching {
    source: WorkspaceId,
    source_name: String,
    workspace: WorkspaceId,
    name: String,
    copied: Polled<Branched>,
    /// When the copy stops waiting for what is not here yet.
    until: Instant,
}

/// The branches being made, followed on every pass.
#[derive(Default)]
pub(crate) struct Branches(std::cell::RefCell<Vec<Branching>>);

impl Branches {
    /// When the loop looks again at a branch being made with nothing arriving: when it stops
    /// waiting.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.0
            .borrow()
            .iter()
            .map(|branching| branching.until)
            .filter(|until| *until > now)
            .min()
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
/// per workspace, `tilo-workspaces`) and the device's files (each workspace's own tree in them).
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

/// What ticks when a read or a seal waiting for what is not here yet may answer otherwise: a write
/// landing from another device, a host's answer or replay, a channel's keys changing.
fn events(engine: &Engine) -> Vec<tokio::sync::watch::Receiver<u64>> {
    let mut events = vec![engine.arrivals().watching()];
    events.extend(engine.replays());
    events.extend(engine.record_answers());
    events.extend(engine.keys().get().and_then(|keys| keys.changes()));
    events
}

/// Found `record` of `app` in the branch `workspace` as `changes` , a whole document: sealed under
/// the keys of the record's channel in the branch, which its first sealed write asks for and this
/// device, the branch's only member, makes, waiting for them until `until`.
async fn found(
    engine: &Engine,
    writes: &dyn tilo_peer_state::workspace_documents::DeviceWrites,
    workspace: WorkspaceId,
    (app, document): (&str, &str),
    changes: &[u8],
    until: Instant,
) -> Result<(), String> {
    let sealed = Some(seal_keys(engine, workspace, (app, document), until).await?);
    writes
        .write(
            &workspace.to_string(),
            app,
            document,
            changes,
            true,
            sealed.as_ref(),
        )
        .await
        .map(|_| ())
        .map_err(String::from)
}

/// Wait for one of `events` to tick: false once `until` passed, or when none of them can tick.
/// The loop looks again at `until` ([`Branches::next_deadline`]).
async fn moved(events: &mut [tokio::sync::watch::Receiver<u64>], until: Instant) -> bool {
    if events.is_empty() {
        return false;
    }
    let passed = std::future::poll_fn(|_| {
        if Instant::now() < until {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    });
    let ticked =
        futures_util::future::select_all(events.iter_mut().map(|event| Box::pin(event.changed())));
    match futures_util::future::select(ticked, std::pin::pin!(passed)).await {
        futures_util::future::Either::Left(((Ok(()), _, _), _)) => Instant::now() < until,
        _ => false,
    }
}

/// Make the branch `to` of `from`: everything but the session, which the loop makes when this is done.
/// What it reads and founds it awaits on the loop, waiting until `until` for what is not here yet;
/// its copies run off the loop.
pub(crate) async fn copy(
    engine: Option<&Engine>,
    writes: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    reads: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    places: Places,
    from: WorkspaceId,
    to: WorkspaceId,
    until: Instant,
) -> Branched {
    let mut branched = Branched::default();
    let reads_sources = match engine {
        Some(engine) if engine.holds(from) && engine.holds(to) => true,
        // The source's records are this device's to read, and the branch holds none: it could not
        // be founded here.
        Some(engine) if engine.holds(from) => {
            branched
                .missing
                .push("the branch's records: this device could not found it".to_owned());
            false
        }
        _ => false,
    };
    let founds = engine.filter(|engine| engine.holds(to));
    let can_read = engine
        .filter(|engine| engine.holds(from))
        .map(|engine| engine.document_read_policy(from));
    let reads = std::sync::Arc::clone(reads);
    // Read off the session runtime: a task holding the workspace's lock may need that runtime
    // to finish its write before the read can proceed.
    let copied = tilo_rt::io::run(move || {
        let sources = if reads_sources {
            read_resources(&reads, from, &mut branched)
        } else {
            Sources::new()
        };
        let permits = |app: &str, document_id: &str| {
            can_read
                .as_ref()
                .is_none_or(|check| check(app, document_id))
        };
        let documents = make_copies(&places, from, to, sources, &mut branched, &permits);
        Ok((branched, documents))
    });
    let (mut branched, records) = copied.await.unwrap_or_else(|error| {
        let mut branched = Branched::default();
        branched.missing.push(error.to_string());
        (branched, Vec::new())
    });
    for ((app, document), changes) in records {
        let Some(engine) = founds else { break };
        match found(
            engine,
            writes.as_ref(),
            to,
            (&app, &document),
            &changes,
            until,
        )
        .await
        {
            Ok(()) => branched.records = branched.records.saturating_add(1),
            Err(error) => branched
                .missing
                .push(format!("{document} of {app}: {error}")),
        }
    }
    branched
}

/// Off the loop: copy what the apps keep for `from` on this device, fork each of `sources` into
/// the branch `to` — written as each app's copy of it there, and answered with the changes that
/// found it as the branch's own record — and copy the workspace's files and environments.
fn make_copies(
    places: &Places,
    from: WorkspaceId,
    to: WorkspaceId,
    mut sources: Sources,
    branched: &mut Branched,
    can_read: &dyn Fn(&str, &str) -> bool,
) -> Vec<((String, String), Vec<u8>)> {
    let mut local: Local = BTreeMap::new();
    if let Some(data_home) = &places.data_home {
        match copy_app_data(data_home, from, to, &mut sources, &mut local, can_read) {
            Ok(copied) => branched.app_data = copied,
            Err(error) => branched.missing.push(error),
        }
    }
    let workspace = to.to_string();
    let mut records = Vec::new();
    for ((app, record), source) in sources {
        if !can_read(&app, &record) {
            continue;
        }
        let mut doc = match ui::resource::transfer::branch(&source, &workspace) {
            Ok(doc) => doc,
            Err(error) => {
                branched.missing.push(format!("{record} of {app}: {error}"));
                continue;
            }
        };
        let changes = doc.save_after(&[]);
        // Every app's copy of it on this device: the app that keeps it, and each app that keeps
        // it beside its own records (the workspace's emoji, in every app's store), the very
        // document the branch's engine founds. A copy left out would be made again by its app, as
        // a genesis of its own the branch's record does not share.
        let bytes = doc.save();
        for path in local
            .get(&(app.clone(), record.clone()))
            .into_iter()
            .flatten()
        {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(path, &bytes));
            if let Err(error) = written {
                branched
                    .missing
                    .push(format!("{record} of {app} on this device: {error}"));
            }
        }
        records.push(((app, record), changes));
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
    records
}

/// Every record of `from` this device reads, but those that stay behind.
fn read_resources(
    reads: &std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    from: WorkspaceId,
    branched: &mut Branched,
) -> Sources {
    let mut sources = Sources::new();
    let routes = match reads.listing(&from.to_string()) {
        Ok(routes) => routes,
        Err(error) => {
            branched.missing.push(error);
            return sources;
        }
    };
    for (app, record) in routes {
        if stays(&app, &record) {
            branched.left = branched.left.saturating_add(1);
            continue;
        }
        // The service holds the source's documents for this device: one whose keys are not here
        // yet reads as nothing, and is named among what the branch left behind.
        let read = reads
            .read(&from.to_string(), &app, &record, None)
            .map(|read| match read {
                tilo_peer_state::workspace_documents::DocumentRead::Whole { bytes, .. } => {
                    Some(AutoCommit::load(&bytes))
                }
                _ => None,
            });
        match read {
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

/// The keys the branch's records are sealed with: its first sealed write asks the branch's group
/// for them, which this device, its only member, makes.
async fn seal_keys(
    engine: &Engine,
    workspace: WorkspaceId,
    route: (&str, &str),
    until: Instant,
) -> Result<key_group::ContentKeys, String> {
    loop {
        let mut events = events(engine);
        match engine.resource_write_keys(workspace, route)? {
            SealKeys::Here(keys) => return Ok(keys),
            SealKeys::NoSecretStore => {
                return Err("this device holds no secret store to keep its keys in".to_owned());
            }
            SealKeys::Waiting if moved(&mut events, until).await => {}
            SealKeys::Waiting => return Err("the branch's keys did not come".to_owned()),
        }
    }
}

/// The relative path `path` (of the source's app directory) takes in the branch's, or `None` for
/// what stays behind or is written apart: Chat's and the session's directories, the runtime's
/// environment records, and the documents themselves with what their store keeps beside them.
fn placed(path: &Path, from: &str, to: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = path.iter().filter_map(|part| part.to_str()).collect();
    match parts.first() {
        // Chat's directory stays behind but for the workspace's own records Chat keeps (its emoji,
        // its agents), which come along in the branch's engine and so in Chat's store too.
        Some(&CHAT)
            if resource_of(&parts, from).is_none_or(|(app, record)| stays(&app, &record)) =>
        {
            return None;
        }
        Some(&tilo_desktop_model::ownership::DESKTOP_APP) => return None,
        // The runtime branches the environments; a workspace's own record another app keeps in
        // Terminal's store (the emoji) comes along like every copy of it.
        Some(&TERMINAL)
            if environment_records(&parts, from)
                && resource_of(&parts, from).is_none_or(|(app, record)| stays(&app, &record)) =>
        {
            return None;
        }
        _ => {}
    }
    let name = parts.last().copied().unwrap_or_default();
    if [
        app_state::DOCUMENT_FILE,
        app_state::ACTOR_FILE,
        app_state::LOAD_HINT_FILE,
        app_state::LOCK_FILE,
        app_state::CANONICAL_LOCK,
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

/// Whether `parts` are in Terminal's documents of the workspace `from`
/// (`…/so.tilo.terminal/workspaces/<from>/documents`), which the runtime branches. Another app's
/// documents Terminal keeps beside its own are not: they come along as every copy of them does.
fn environment_records(parts: &[&str], from: &str) -> bool {
    parts.windows(4).any(|window| {
        window
            == [
                TERMINAL,
                app_state::WORKSPACES_DIR,
                from,
                app_state::DOCUMENTS_DIR,
            ]
    })
}

/// The app and record a document at `path` (relative to the workspace's app directories) is,
/// when it is a record: `<app dir>/…/<app>/workspaces/<workspace>/documents/<record>`.
fn resource_of(parts: &[&str], workspace: &str) -> Option<(String, String)> {
    let at = parts.windows(3).position(|window| {
        window
            == [
                app_state::WORKSPACES_DIR,
                workspace,
                app_state::DOCUMENTS_DIR,
            ]
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
    can_read: &dyn Fn(&str, &str) -> bool,
) -> Result<Copied, String> {
    let workspaces = data_home.join(launcher::appdir::WORKSPACES_DIR);
    let (from_id, to_id) = (from.to_string(), to.to_string());
    let source = workspaces.join(launcher::appdir::workspace_slug(Some(&from_id)));
    if !source.is_dir() {
        return Ok(Copied::default());
    }
    let target = workspaces.join(launcher::appdir::workspace_slug(Some(&to_id)));
    let mut copied = clone_tree(
        &source,
        &target,
        Strategy::Clone,
        &mut |path| {
            let parts: Vec<_> = path.iter().filter_map(|part| part.to_str()).collect();
            if let Some((app, document_id)) = resource_of(&parts, &from_id)
                && !can_read(&app, &document_id)
            {
                return None;
            }
            placed(path, &from_id, &to_id)
        },
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
        let route = resource_of(&parts, &from_id);
        if let Some((app, document_id)) = &route
            && !can_read(app, document_id)
        {
            continue;
        }
        let bytes = std::fs::read(&document).map_err(|error| error.to_string())?;
        let loaded = AutoCommit::load_with_options(
            &bytes,
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore),
        )
        .map_err(|error| format!("{}: {error}", relative.display()));
        let mut held = loaded?;
        if let Some(route) = route
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
    let mut copy = ui::resource::transfer::branch(doc, &to.to_string())
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
            .join(app_state::DOCUMENTS_DIR)
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

/// The branch's own session: `source`'s strips, and a window for each of the windows this device
/// shows in it — opened again in the branch by its app — with ids of its own (`mint`). Other
/// people's windows stay theirs. What the workspace grants its apps comes along, its own files
/// named as the branch's.
pub(crate) fn shell_of(
    source: &Workspace,
    workspace: WorkspaceId,
    name: &str,
    files: Option<&Path>,
    mint: &mut dyn FnMut() -> u64,
) -> Workspace {
    let mut branch = Workspace::new(workspace, name);
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
    /// The session branched the workspace `source` into `workspace` , called `name` : found it here and copy what the source holds into it, without holding the loop.
    pub(crate) fn branch_workspace(
        &self,
        source: WorkspaceId,
        workspace: WorkspaceId,
        name: String,
    ) {
        let Some((from, source_name)) = self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id() == source)
                .map(|held| (held.id(), held.name.clone()))
        }) else {
            self.handle
                .acknowledge(tilo_desktop_surface::i18n::t("desktop-workspace-gone"));
            return;
        };
        let engine = self.engine.clone();
        let writes = std::sync::Arc::clone(&self.document_writes);
        let reads = std::sync::Arc::clone(&self.document_reads);
        let until = Instant::now()
            .checked_add(PATIENCE)
            .unwrap_or_else(Instant::now);
        let founded = name.clone();
        let copied = Polled::new(
            Arc::clone(&self.wake_handle),
            async move {
                if let Some(engine) = &engine
                    && let Err(error) = engine.found(workspace, &founded).await
                {
                    // The workspace is the layout's from here either way; a founding that failed
                    // is asked for again, and the person hears why.
                    tracing::warn!(%error, %workspace, "the branch's workspace was not founded");
                }
                copy(
                    engine.as_deref(),
                    &writes,
                    &reads,
                    Places::here(),
                    from,
                    workspace,
                    until,
                )
                .await
            },
            || Branched {
                missing: vec!["the branch's copy stopped before it answered".to_owned()],
                ..Branched::default()
            },
        );
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
            source_name,
            workspace,
            name,
            copied,
            until,
        });
    }

    /// Put each branch whose copy is done on the shell, with the source's strips and windows, and go
    /// there when the person is still where they asked for it; say what did not come along.
    pub(crate) fn follow_branches(&self) {
        let mut landed = Vec::new();
        self.branches
            .0
            .borrow_mut()
            .retain_mut(|branching| match branching.copied.done() {
                Some(branched) => {
                    landed.push((
                        branching.source,
                        branching.source_name.clone(),
                        branching.workspace,
                        branching.name.clone(),
                        branched,
                    ));
                    false
                }
                None => true,
            });
        for (source, source_name, workspace, name, branched) in landed {
            self.land_branch((source, &source_name), (workspace, &name), &branched);
        }
    }

    fn land_branch(
        &self,
        (source, source_name): (WorkspaceId, &str),
        (workspace, name): (WorkspaceId, &str),
        branched: &Branched,
    ) {
        let mut tree: Layout = self.handle.read(Clone::clone);
        let files = Places::here().files;
        let made = tree
            .workspaces
            .iter()
            .find(|held| held.id() == source)
            .map(|held| {
                shell_of(held, workspace, name, files.as_deref(), &mut || {
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
                tree.active_strip.insert(workspace, first);
            }
            // The person goes to the branch they asked for, unless they went elsewhere meanwhile.
            if tree.active == source {
                tree.active = workspace;
            }
            self.handle.replace_tree(tree.clone());
            #[cfg(target_os = "linux")]
            self.restore.borrow_mut().add(&tree, workspace);
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
        for missing in &branched.missing {
            tracing::warn!(%workspace, %missing, "a branch of a workspace is not complete");
        }
        let (title, body) = told(branched, name, source_name);
        self.telling().show(
            &Notice::new(String::new(), 1, title, body).about(NotificationIconKind::Workspace),
        );
    }
}

/// What the person is told of the branch `name` of `source_name`: that it is ready, or that it is
/// not complete, the notice's body then naming every part that did not come along and why, one
/// a line (the same lines the session logs at warn).
fn told(branched: &Branched, name: &str, source_name: &str) -> (String, String) {
    if branched.missing.is_empty() {
        (
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
        (
            tilo_desktop_surface::i18n::t_args(
                "session-branch-failed",
                &[("branch", name.to_owned().into())],
            ),
            branched.missing.join("\n"),
        )
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

    tilo_test_env::scenario! {
        #[allow(clippy::arithmetic_side_effects, reason = "test arithmetic in a phase's closure")]
        fn a_branch_takes_the_workspaces_records_and_leaves_its_chat;

        phase chat_and_what_names_other_people_stay_behind_and_the_rest_comes_along;
        {
            let emoji = collab_types::workspace_document_id("tilo.workspace.emoji");
            let agents = collab_types::workspace_document_id("tilo.workspace.agent");
            assert!(stays(CHAT, "messages"), "every conversation");
            assert!(stays(
                CHAT,
                &collab_types::workspace_document_id("chat.workspace")
            ));
            assert!(!stays(CHAT, &emoji), "the workspace's own emoji");
            assert!(
                !stays("so.tilo.assistant", &agents),
                "the workspace's agents"
            );
            assert!(
                stays(TERMINAL, "env-1"),
                "the runtime branches its environments"
            );
            assert!(stays(tilo_desktop_model::ownership::DESKTOP_APP, "desktop"));
            assert!(stays(
                "tilo.host",
                &collab_types::workspace_document_id("tilolang.personal")
            ));
            assert!(!stays("so.tilo.notes", "uuid:plan"));
        }

        phase the_apps_data_and_the_files_of_the_workspace_come_along_and_chat_stays;
        // What the apps keep for a workspace comes along with every document forked into the branch,
        // without the source's writer or locks, and Chat's and the environments' stay behind; the
        // workspace's own files come along; the source keeps all of it as it was.
        {
            let (from, to) = ids();
            let home = tempfile::tempdir().unwrap();
            let data = home.path().join("data");
            let files = home.path().join("files");
            let workspaces = data.join("tilo-workspaces");
            let notes = workspaces
                .join(FROM)
                .join("so.tilo.notes/.local/share/so.tilo.notes/workspaces")
                .join(FROM)
                .join("documents/uuid-plan");
            std::fs::create_dir_all(&notes).unwrap();
            std::fs::write(notes.join("state.automerge"), record(FROM, "plan").save()).unwrap();
            std::fs::write(notes.join("actor"), b"source writer").unwrap();
            std::fs::write(notes.join("writer.lock"), b"").unwrap();
            let settings = workspaces.join(FROM).join("so.tilo.notes/.config/notes.toml");
            std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
            std::fs::write(&settings, b"theme = 'dark'").unwrap();
            let chat = workspaces
                .join(FROM)
                .join("so.tilo.chat/.local/share/so.tilo.chat/workspaces")
                .join(FROM);
            std::fs::create_dir_all(chat.join("documents/messages")).unwrap();
            std::fs::write(
                chat.join("documents/messages/state.automerge"),
                record(FROM, "hi").save(),
            )
            .unwrap();
            let environment = workspaces
                .join(FROM)
                .join("so.tilo.terminal/.local/share/so.tilo.terminal/workspaces")
                .join(FROM)
                .join("documents/env-1");
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
            let branched = branch(None, places, from, to);
            assert_eq!(
                branched.missing,
                vec!["the Terminal environments: no runtime here".to_owned()],
                "only the environments, which the runtime branches, did not come"
            );

            let copied = workspaces
                .join(TO)
                .join("so.tilo.notes/.local/share/so.tilo.notes/workspaces")
                .join(TO)
                .join("documents/uuid-plan");
            let doc = load(&copied.join("state.automerge"));
            assert_eq!(body(&doc), "plan");
            assert_eq!(ui::resource::workspace_scope(&doc).as_deref(), Some(TO));
            assert!(
                !copied.join("actor").exists(),
                "the branch writes as a writer of its own"
            );
            assert!(!copied.join("writer.lock").exists());
            assert_eq!(
                std::fs::read(workspaces.join(TO).join("so.tilo.notes/.config/notes.toml")).unwrap(),
                b"theme = 'dark'"
            );
            assert!(
                !workspaces.join(TO).join("so.tilo.chat").exists(),
                "chat stays behind"
            );
            assert!(
                !workspaces
                    .join(TO)
                    .join("so.tilo.terminal/.local/share/so.tilo.terminal/workspaces")
                    .join(TO)
                    .join("documents")
                    .exists(),
                "the runtime branches the environments, not a copy of their files"
            );
            assert_eq!(
                std::fs::read(files.join(".workspaces").join(TO).join("src/main.rs")).unwrap(),
                b"fn main() {}"
            );

            // The source is as it was, and an edit in either stays there.
            let source = load(&notes.join("state.automerge"));
            assert_eq!(ui::resource::workspace_scope(&source).as_deref(), Some(FROM));
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

        phase the_branchs_shell_has_the_sources_strips_and_this_devices_windows;
        // The branch's session has the source's strips and a window for each of this device's, made
        // again by their apps; another person's window stays theirs.
        {
            let (from, to) = ids();
            let mut source = Workspace::new(from, "Research");
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
                        TileContent::HostedWindow {
                            app_id: "org.gnome.TextEditor".into(),
                            label: "theirs".into(),
                            host: "ab".repeat(32),
                            host_label: "Laptop".into(),
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
            let branch = shell_of(&source, to, "Research branch", None, &mut || {
                next += 1;
                next
            });
            assert_eq!(branch.id(), to);
            assert_eq!(branch.name, "Research branch");
            let names: Vec<&str> = branch
                .strips
                .iter()
                .map(|strip| strip.name.as_str())
                .collect();
            assert_eq!(names, ["Reading", "Writing"]);
            let tabs: Vec<&Tab> = branch.tabs().collect();
            assert_eq!(
                tabs.len(),
                1,
                "a window that runs on another device stays there"
            );
            assert!(matches!(
                &tabs[0].tile,
                TileContent::LocalApp { app_id, .. } if app_id == "so.tilo.notes"
            ));
            assert!(tabs[0].id > 100, "ids of its own");
            assert!(branch.strips[0].id.0 > 100);
        }

        phase every_copy_of_a_record_that_comes_along_is_the_branchs_one_document;
        // A record that comes along is one document wherever this device keeps it: the engine's copy,
        // Chat's own copy of the workspace's emoji and the copy another app keeps beside its own records
        // are the branch's same document, so no app of the branch makes the record again as a genesis
        // the branch's record does not share (which left Chat's committed history blocked on a
        // restart); the conversation Chat keeps stays behind.
        {
            let state = tempfile::tempdir().unwrap();
            let identity = identity();
            let engine = reopened(&state, &identity);
            let (from, to) = ids();
            pollster::block_on(engine.found(from, "Research")).expect("the source is founded");
            pollster::block_on(engine.found(to, "Research branch")).expect("the branch is founded");
            let keys = key_group::KeyGroup::create(
                collab_types::WorkspaceId::parse(&to.to_string()).unwrap(),
                key_group::MemberKey::generate("owner").unwrap(),
            )
            .unwrap()
            .content_keys()
            .unwrap();
            let emoji = collab_types::workspace_document_id("tilo.workspace.emoji");
            let mut party = record(FROM, "party-parrot");
            write(&engine, &keys, from, (CHAT, &emoji), &mut party);
            let home = tempfile::tempdir().unwrap();
            let workspaces = home.path().join("data/tilo-workspaces");
            // Where an app directory `app` keeps `owner`'s record `record` of `workspace`.
            let kept = |workspace: &str, app: &str, owner: &str, record: &str| {
                workspaces
                    .join(workspace)
                    .join(app)
                    .join(".local/share")
                    .join(owner)
                    .join("workspaces")
                    .join(workspace)
                    .join("documents")
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
            let branched = branch(Some(&engine), places, from, to);
            assert!(branched.missing.is_empty(), "{:?}", branched.missing);

            let mut engines = AutoCommit::load(
                &engine
                    .record(TO, CHAT, &emoji)
                    .unwrap()
                    .expect("the branch holds the emoji"),
            )
            .unwrap();
            for app in [CHAT, TERMINAL] {
                let path = kept(TO, app, CHAT, &emoji);
                assert!(path.exists(), "{app}'s copy of the emoji came along");
                let mut copy = load(&path);
                assert_eq!(body(&copy), "party-parrot");
                assert_eq!(ui::resource::workspace_scope(&copy).as_deref(), Some(TO));
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

        phase an_app_that_first_opens_the_workspaces_emoji_in_the_branch_makes_the_branchs_record;
        // The workspace's emoji as an app makes it, from its kind alone, branched: an app of the
        // branch that opens the emoji before any copy of it reached its own store makes the very
        // record the branch holds, so the two meet as one record holding the source's emoji. Before,
        // the branch's copy kept the source's genesis, the app's was the branch's, and the two met as
        // a record of no identity: Chat's history in the branch stayed blocked after a restart
        // ("record identity does not match its binding").
        {
            use automerge::ReadDoc as _;
            use automerge::transaction::Transactable as _;
            use tilo_automerge::ApplyIncoming as _;
            const EMOJI: &str = "tilo.workspace.emoji";
            let state = tempfile::tempdir().unwrap();
            let identity = identity();
            let engine = reopened(&state, &identity);
            let (from, to) = ids();
            pollster::block_on(engine.found(from, "Research")).expect("the source is founded");
            pollster::block_on(engine.found(to, "Research branch")).expect("the branch is founded");
            let keys = key_group::KeyGroup::create(
                collab_types::WorkspaceId::parse(&to.to_string()).unwrap(),
                key_group::MemberKey::generate("owner").unwrap(),
            )
            .unwrap()
            .content_keys()
            .unwrap();
            // An app's window of `workspace` opening the emoji, in a store of its own.
            let opened = |workspace: &str| {
                ui::resource::workspace::WorkspaceResources::in_workspace(
                    ui::resource::ResourceOwners::with_store(std::rc::Rc::new(
                        durable_store::MemoryStore::new(),
                    )),
                    workspace.to_owned(),
                    ui::permissions::MemberId::person("ada"),
                )
                .open_resource(CHAT, EMOJI, &|doc| {
                    doc.put(automerge::ROOT, "body", "")
                        .map_err(|error| error.to_string())
                })
                .unwrap()
            };
            let source = opened(FROM);
            source
                .edit(true, |doc| {
                    doc.put(automerge::ROOT, "body", "party-parrot")
                        .map_err(|error| error.to_string())
                })
                .unwrap();
            let emoji = collab_types::workspace_document_id(EMOJI);
            let mut held = source.read(AutoCommit::clone).unwrap();
            write(&engine, &keys, from, (CHAT, &emoji), &mut held);

            let branched = branch(Some(&engine), Places::default(), from, to);
            assert!(branched.missing.is_empty(), "{:?}", branched.missing);

            let mut engines = AutoCommit::load(
                &engine
                    .record(TO, CHAT, &emoji)
                    .unwrap()
                    .expect("the branch holds the emoji"),
            )
            .unwrap();
            let mut met = opened(TO).read(AutoCommit::clone).unwrap();
            met.merge_incoming(&mut engines).unwrap();
            assert_eq!(
                ui::resource::identity(&met).map(|checkout| checkout.id.as_str().to_owned()),
                Some(emoji),
                "one record of the branch"
            );
            assert_eq!(ui::resource::workspace_scope(&met).as_deref(), Some(TO));
            assert!(matches!(
                met.get(automerge::ROOT, "body").unwrap(),
                Some((automerge::Value::Scalar(body), _)) if body.as_str() == Some("party-parrot")
            ));
        }

        phase a_branch_that_misses_a_record_names_it_in_its_notice;
        // A branch that could not take a record along says which, and why, in the notice the person
        // gets: never only that it is not complete.
        {
            let branched = Branched {
                missing: vec![
                    "uuid-plan of so.tilo.notes did not reach this device".to_owned(),
                    "the Terminal environments: no runtime here".to_owned(),
                ],
                ..Branched::default()
            };
            let (title, body) = told(&branched, "Question B", "Research");
            assert!(
                title.contains("Question B") && title.contains("not complete"),
                "{title}"
            );
            assert!(
                body.contains("uuid-plan of so.tilo.notes did not reach this device"),
                "{body}"
            );
            assert!(
                body.contains("the Terminal environments: no runtime here"),
                "{body}"
            );
            let (title, _) = told(&Branched::default(), "Question A", "Research");
            assert!(title.contains("is ready"), "{title}");
        }

        phase a_workspace_branched_three_times_in_a_row_gives_each_branch_all_of_it;
        // One workspace branched three times back to back, as the e2e box does (two through the
        // session's action, the third through the CLI): each branch holds all the source holds, its
        // records, what its apps keep and its tree of files with a large file and many small ones,
        // and none is reported not complete. The first branch leaves nothing in the source that the
        // next ones cannot read.
        {
            let state = tempfile::tempdir().unwrap();
            let identity = identity();
            let engine = reopened(&state, &identity);
            let from = ids().0;
            let targets = [
                "7a6b5c4d-0000-4000-8000-000000000003",
                "7a6b5c4d-0000-4000-8000-000000000004",
                "7a6b5c4d-0000-4000-8000-000000000005",
            ]
            .map(|id| WorkspaceId::parse(id).unwrap());
            pollster::block_on(engine.found(from, "Research")).expect("the source is founded");
            let channel = collab_types::WorkspaceId::parse(FROM).unwrap();
            let keys =
                key_group::KeyGroup::create(channel, key_group::MemberKey::generate("owner").unwrap())
                    .unwrap()
                    .content_keys()
                    .unwrap();
            for held in targets {
                engine.hand_keys(held, keys.clone());
            }
            let emoji = collab_types::workspace_document_id("tilo.workspace.emoji");
            write(
                &engine,
                &keys,
                from,
                ("so.tilo.notes", "uuid-plan"),
                &mut record(FROM, "plan"),
            );
            write(
                &engine,
                &keys,
                from,
                (CHAT, &emoji),
                &mut record(FROM, "party-parrot"),
            );

            let home = tempfile::tempdir().unwrap();
            let data = home.path().join("data");
            let files = home.path().join("files");
            let notes = data
                .join("tilo-workspaces")
                .join(FROM)
                .join("so.tilo.notes/.local/share/so.tilo.notes/workspaces")
                .join(FROM)
                .join("documents/uuid-plan");
            std::fs::create_dir_all(&notes).unwrap();
            std::fs::write(notes.join("state.automerge"), record(FROM, "plan").save()).unwrap();
            let tree = files.join(".workspaces").join(FROM);
            std::fs::create_dir_all(tree.join("small")).unwrap();
            std::fs::write(tree.join("bulk.bin"), vec![7_u8; 8 << 20]).unwrap();
            for n in 0..400 {
                std::fs::write(tree.join(format!("small/{n}.txt")), format!("file {n}")).unwrap();
            }
            let places = Places {
                data_home: Some(data),
                files: Some(files.clone()),
                runtime: None,
            };

            for (n, to) in targets.into_iter().enumerate() {
                pollster::block_on(engine.found(to, &format!("Question {n}")))
                    .expect("the branch is founded");
                let branched = branch(Some(&engine), places.clone(), from, to);
                assert!(
                    branched.missing.is_empty(),
                    "branch {n} is not complete: {:?}",
                    branched.missing
                );
                assert_eq!(branched.records, 2, "branch {n}");
                assert_eq!(branched.files.files, 401, "branch {n}");
                let copied = files.join(".workspaces").join(to.to_string());
                assert_eq!(
                    std::fs::metadata(copied.join("bulk.bin")).unwrap().len(),
                    8 << 20
                );
            }
        }

        phase a_branch_holds_the_sources_records_but_its_chat_and_only_the_person;
        // A workspace with notes, the workspace's emoji and a conversation, branched: the branch
        // holds the notes and the emoji with their history, sealed under its own channel and naming
        // it, and not the conversation; its only member is the person, as its owner; the source holds
        // what it held. All of it is as it was after the engine opens again.
        {
            let state = tempfile::tempdir().unwrap();
            let identity = identity();
            let person = hex::encode(identity.device_cert.user_public_key);
            let engine = reopened(&state, &identity);
            let (from, to) = ids();
            pollster::block_on(engine.found(from, "Research")).expect("the source is founded");
            pollster::block_on(engine.found(to, "Research branch")).expect("the branch is founded");
            assert!(engine.holds(from) && engine.holds(to));
            let channel = collab_types::WorkspaceId::parse(&to.to_string()).unwrap();
            let keys =
                key_group::KeyGroup::create(channel, key_group::MemberKey::generate("owner").unwrap())
                    .unwrap()
                    .content_keys()
                    .unwrap();
            let notes = ("so.tilo.notes", "uuid-plan");
            let emoji = collab_types::workspace_document_id("tilo.workspace.emoji");
            let mut plan = record(FROM, "plan");
            write(&engine, &keys, from, notes, &mut plan);
            write(
                &engine,
                &keys,
                from,
                (CHAT, &emoji),
                &mut record(FROM, "party-parrot"),
            );
            write(
                &engine,
                &keys,
                from,
                (CHAT, "messages"),
                &mut record(FROM, "see you at nine"),
            );

            let branched = branch(Some(&engine), Places::default(), from, to);
            assert!(branched.missing.is_empty(), "{:?}", branched.missing);
            assert_eq!((branched.records, branched.left), (2, 1));

            let read = |workspace: &str, route| {
                AutoCommit::load(
                    &engine
                        .record(workspace, notes.0, route)
                        .unwrap()
                        .expect("the record is held"),
                )
                .unwrap()
            };
            let mut copied = read(TO, notes.1);
            assert_eq!(body(&copied), "plan");
            assert_eq!(ui::resource::workspace_scope(&copied).as_deref(), Some(TO));
            for change in plan.get_heads() {
                assert!(
                    copied.has_change(&change),
                    "the branch holds the record's history"
                );
            }
            let routes = engine.documents(TO).unwrap();
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
                people
                    .iter()
                    .all(|member| member.id.person_id() == Some(person.as_str())),
                "nobody but the person: {people:?}"
            );
            assert!(
                engine.reads_whole(to),
                "the person reads all of their branch"
            );

            // The source holds what it held, and the two are documents of their own.
            let mut source = read(FROM, notes.1);
            assert_eq!(ui::resource::workspace_scope(&source).as_deref(), Some(FROM));
            assert_ne!(source.get_heads(), copied.get_heads());
            assert!(
                engine
                    .documents(FROM)
                    .unwrap()
                    .contains(&(CHAT.to_owned(), "messages".to_owned()))
            );

            drop(engine);
            let engine = reopened(&state, &identity);
            assert!(
                engine.holds(to),
                "the branch is this device's after a restart"
            );
            assert_eq!(engine.owner_of(TO), Ok(person));
            assert_eq!(engine.documents(TO).unwrap().len(), 2);
        }
    }

    /// A record document as an app keeps it on this device.
    fn record(workspace: &str, body: &str) -> AutoCommit {
        let mut doc = AutoCommit::new();
        assert!(ui::resource::init_scoped(
            &mut doc,
            &ui::resource::DocumentId::minted(),
            &ui::resource::ResourceType::new("notes.page"),
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

    /// A device with no service open writes no document: what a branch of a session that keeps no
    /// workspaces finds.
    struct NoWrites;

    impl tilo_peer_state::workspace_documents::DeviceWrites for NoWrites {
        fn write(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[u8],
            _: bool,
            _: Option<&key_group::ContentKeys>,
        ) -> tilo_peer_state::workspace_documents::DocumentWrite {
            Box::pin(async {
                Err(tilo_peer_state::owner::WriteRefusal::Refused(
                    "no workspace service is open".to_owned(),
                ))
            })
        }
    }

    /// A device with no service open reads no document: what a branch of a session that keeps no
    /// workspaces finds.
    struct NoDocuments;

    impl tilo_peer_state::workspace_documents::DeviceDocuments for NoDocuments {
        fn read(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[[u8; 32]]>,
        ) -> Result<tilo_peer_state::workspace_documents::DocumentRead, String> {
            Err("no workspace service is open".to_owned())
        }

        fn listing(&self, _: &str) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }

        fn holds(&self, _: &str, _: &str, _: &str) -> bool {
            false
        }

        fn workspaces(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// `from` branched into `to` , with everything this device holds here.
    fn branch(
        held: Option<&Arc<Engine>>,
        places: Places,
        from: WorkspaceId,
        to: WorkspaceId,
    ) -> Branched {
        let reads: Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments> = match held {
            Some(engine) => Arc::new(crate::records::testing::EngineDocuments(Arc::clone(engine))),
            None => Arc::new(NoDocuments),
        };
        let writes: Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites> = match held {
            Some(engine) => Arc::new(crate::records::testing::EngineWrites(Arc::clone(engine))),
            None => Arc::new(NoWrites),
        };
        pollster::block_on(copy(
            held.map(std::convert::AsRef::as_ref),
            &writes,
            &reads,
            places,
            from,
            to,
            Instant::now(),
        ))
    }

    /// A device of a person, certified for their root, that owns workspaces.
    /// This device's engine, opened again under `state` as a session that starts afresh does.
    fn reopened(
        state: &tempfile::TempDir,
        identity: &tilo_identity::identity_store::DeviceIdentity,
    ) -> Arc<Engine> {
        let engine = Arc::new(
            Engine::open(
                identity.clone(),
                Some(state.path()),
                Vec::new(),
                crate::platform::membership_check::of_this_session(),
            )
            .expect("this device's engine opens"),
        );
        crate::founding_fixture::serve(&engine);
        engine
    }

    fn identity() -> tilo_identity::identity_store::DeviceIdentity {
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
        tilo_identity::identity_store::DeviceIdentity::kept(cert, device)
    }

    /// Write `doc` into `workspace` as `record` of `app` , sealed, as an app's first write is.
    fn write(
        engine: &Arc<Engine>,
        keys: &key_group::ContentKeys,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        doc: &mut AutoCommit,
    ) {
        use tilo_peer_state::workspace_documents::DeviceWrites as _;
        // The keys of the source and of the branch, as the key service hands them to a member.
        for held in [ids().0, ids().1] {
            engine.hand_keys(held, keys.clone());
        }
        let writes = crate::records::testing::EngineWrites(Arc::clone(engine));
        pollster::block_on(writes.write(
            &workspace.to_string(),
            app,
            record,
            &doc.save_after(&[]),
            true,
            Some(keys),
        ))
        .expect("the workspace takes the record");
    }
    /// An agent branches the workspace it works in, by any name the session knows it by, and no
    /// other: what it would copy of another is not its to read.
    #[test]
    fn an_agent_branches_its_own_workspace_and_no_other() {
        let workspaces = [
            ["studio-id".to_owned(), "Studio".to_owned()],
            ["garden-id".to_owned(), "Garden".to_owned()],
        ];
        let branch = |source: Option<&str>| tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: tilo_actions::remote::SESSION_ACTIONS_PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    tilo_desktop::verbs::CHROME_RESOURCE,
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
        for own in ["Studio", "studio", "studio-id"] {
            assert!(
                !branches_elsewhere(&branch(Some(own)), &workspaces, "studio-id"),
                "its own workspace, as {own}"
            );
        }
        for other in ["Garden", "garden-id", "nowhere"] {
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
