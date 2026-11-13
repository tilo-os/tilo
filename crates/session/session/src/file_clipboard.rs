//! Files' clipboard: what a person copied or cut in Files, and pasting it into
//! a folder.
//!
//! The app never names a path the session trusts: an entry arrives as a
//! descriptor the app handed over on its connection, and what it named is
//! checked against the object the window showed. A copy keeps those objects
//! for as long as the offer lives; a paste hands them, with the folder it
//! takes them into, to the same engine the desktop's own file paste runs
//! ([`crate::clipboard::paste`]): staged beside their names, synced,
//! written down in a receipt, and only then renamed into place, so a paste
//! that is killed leaves every file whole and its source where it was.
//!
//! A copy also goes on the desktop's clipboard, as a file manager's does, so
//! the person can paste it into any other app; what they pasted there is that
//! app's own business.
//!
//! Commands run on the broker's threads, never on the session's loop: they
//! reach the loop only to say a copy is ready to be offered
//! ([`NativeFileClipboard::take_published`]).

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use runtime_collab::file_clipboard::{
    Command, Conflict as Asked, NamedEntry, ObjectIdentity, Reply, SelectedDescriptor,
};
use tilo_action_broker::services::{FileClipboardContext, FileClipboardServices};
use tilo_actions::ActionKind;
use tilo_lock::MutexRecover as _;
use ui::dnd::CopiedFiles;

use crate::clipboard::paste::{self, Conflict};

/// The most entries one copy holds, as one command may capture.
const MAX_ENTRIES: usize = runtime_collab::file_clipboard::MAX_ENTRIES;
/// How long a copied offer can be pasted.
const OFFER_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// How long a captured object is kept for the command that consumes it.
const TICKET_LIFETIME: Duration = Duration::from_secs(2 * 60);
/// The most transfers whose receipt this keeps: one per flash of work, not a history.
const MAX_OPERATIONS: usize = 256;
/// The most clashing names a [`Reply::Conflicts`] lists (it also counts).
const MAX_CONFLICT_NAMES: usize = 8;
/// How long the filesystem names an agent as the cause of the writes to the paths it was told: long
/// enough for a transfer's writes to land, short enough that a later write of the person's own
/// stays theirs.
const WRITE_ATTRIBUTION_TTL: Duration = Duration::from_secs(60);

/// Files' clipboard, as the broker's seam.
pub(crate) struct NativeFileClipboard {
    state: Mutex<State>,
    /// Copies Files made, waiting for the loop that has the compositor to put them on the desktop's
    /// clipboard.
    published: Mutex<Vec<CopiedFiles>>,
    wake: tilo_service::Wake,
}

/// The offers waiting to be pasted, the objects commands hold, and the transfers whose receipt the
/// windows ask after.
#[derive(Default)]
struct State {
    offers: HashMap<String, Offer>,
    tickets: HashMap<String, Ticket>,
    operations: HashMap<String, Running>,
}

/// A copy a person made: the objects Files showed, and whether it is a cut.
struct Offer {
    id: String,
    cut: bool,
    entries: Vec<Held>,
    expires: Instant,
}

impl Offer {
    fn public(&self) -> runtime_collab::file_clipboard::Offer {
        runtime_collab::file_clipboard::Offer {
            id: self.id.clone(),
            count: self.entries.len(),
            cut: self.cut,
        }
    }
}

/// An object a copy names, as it was when the copy was made: a file replaced under the same name
/// since is not the one the person copied, and a cut must not take it away.
#[derive(Clone)]
struct Held {
    path: PathBuf,
    object: ObjectIdentity,
}

impl Held {
    fn still_there(&self) -> bool {
        std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| identity(&metadata) == self.object)
    }
}

/// The object a command captured, kept for the Publish or Paste that consumes it, for the window
/// that captured it.
struct Ticket {
    peer: u64,
    window: String,
    workspace: String,
    paths: Vec<PathBuf>,
    /// Whether it names the folder a paste takes the files into.
    destination: bool,
    expires: Instant,
}

impl Ticket {
    fn belongs_to(&self, context: &FileClipboardContext) -> bool {
        self.peer == context.peer()
            && self.window == context.window()
            && self.workspace == context.workspace()
    }
}

/// One paste, on its own thread, and what it has to say.
struct Running {
    peer: u64,
    window: String,
    workspace: String,
    offer: String,
    destination: String,
    paste: paste::Paste,
    /// The answer once the worker finished, so a reply lost on the way is answered again from here
    /// instead of starting a second paste.
    settled: Option<(usize, Option<String>)>,
}

impl Running {
    /// What the transfer is doing, or what it did.
    fn answer(&self, operation: &str) -> Reply {
        match &self.settled {
            Some((completed, error)) => Reply::Settled {
                operation: operation.to_owned(),
                completed: *completed,
                error: error.clone(),
                undone: false,
            },
            None => Reply::Running {
                operation: operation.to_owned(),
            },
        }
    }

    fn same_paste(&self, context: &FileClipboardContext, offer: &str, destination: &str) -> bool {
        self.peer == context.peer()
            && self.window == context.window()
            && self.workspace == context.workspace()
            && self.offer == offer
            && self.destination == destination
    }
}

impl NativeFileClipboard {
    /// Files' clipboard, woken by `wake` when a copy waits for the loop.
    pub(crate) fn new(wake: tilo_service::Wake) -> Self {
        Self {
            state: Mutex::new(State::default()),
            published: Mutex::new(Vec::new()),
            wake,
        }
    }

    /// The copies Files made since the last pass, for the loop to offer on the desktop's clipboard
    /// so the person can paste them into another app.
    pub(crate) fn take_published(&self) -> Vec<CopiedFiles> {
        std::mem::take(&mut *self.published.lock_recover())
    }

    /// Name `agent`, an `agent:<id>` principal the broker took from its own
    /// custody of a live Files invocation, as the cause of this device's next
    /// writes to `paths`: each version the filesystem then makes of them
    /// records the agent, for the person who signs it, in the agent's presence
    /// colour. It grants nothing.
    ///
    /// # Errors
    /// Refuses a principal that is not an agent, and what the filesystem says.
    pub(crate) fn attribute(agent: &str, paths: Vec<String>) -> Result<(), String> {
        let label = ui::history_source::agent_principal_name(agent)
            .ok_or("only an agent is named as the cause of a write")?;
        let (red, green, blue) = ui::presence::user_color(agent);
        blobs::with_session_blobs(|client| {
            client.attribute_writes(runtime_fs::blobs::WriteAttribution {
                paths,
                actor: agent.to_owned(),
                label,
                color: [red, green, blue],
                ttl: WRITE_ATTRIBUTION_TTL,
            })
        })
    }

    /// Keep `paths` under a ticket of `context` 's window until the command that consumes it: what
    /// Publish takes a copy from, or the folder a Paste takes the files into.
    fn hold(
        &self,
        context: &FileClipboardContext,
        paths: Vec<PathBuf>,
        destination: bool,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        let mut state = self.state.lock_recover();
        state.tickets.retain(|_, ticket| now < ticket.expires);
        state.tickets.insert(
            id.clone(),
            Ticket {
                peer: context.peer(),
                window: context.window().to_owned(),
                workspace: context.workspace().to_owned(),
                paths,
                destination,
                expires: now.checked_add(TICKET_LIFETIME).unwrap_or(now),
            },
        );
        id
    }

    /// What `id` holds, for a command of the same window in the same workspace.
    fn held(
        &self,
        context: &FileClipboardContext,
        id: &str,
        destination: bool,
    ) -> Result<(Vec<PathBuf>, String), String> {
        let now = Instant::now();
        let mut state = self.state.lock_recover();
        state.tickets.retain(|_, ticket| now < ticket.expires);
        let paths = state
            .tickets
            .get(id)
            .filter(|ticket| ticket.belongs_to(context) && ticket.destination == destination)
            .map(|ticket| ticket.paths.clone())
            .ok_or("the selected object is no longer held")?;
        drop(state);
        Ok((paths, id.to_owned()))
    }

    /// Capture the selected descriptors as tickets: what Publish takes a copy from, and Paste takes
    /// the folder it pastes into.
    fn capture(
        &self,
        context: &FileClipboardContext,
        sources: &[SelectedDescriptor],
        destination: Option<&SelectedDescriptor>,
    ) -> Result<Reply, String> {
        context.authorize(ActionKind::Read)?;
        let mut tickets = Vec::with_capacity(sources.len());
        for selected in sources {
            let held = select(context, selected)?;
            tickets.push(self.hold(context, vec![held.path], false));
        }
        let destination = match destination {
            Some(selected) => {
                let held = select(context, selected)?;
                if !held.path.is_dir() {
                    return Err("a paste needs a folder to go into".into());
                }
                Some(self.hold(context, vec![held.path], true))
            }
            None => None,
        };
        Ok(Reply::Captured {
            sources: tickets,
            destination,
        })
    }

    /// Capture the entries of a shown folder by name: one descriptor holds the folder, and each
    /// entry names the object the window showed under it.
    fn capture_named(
        &self,
        context: &FileClipboardContext,
        folder: &SelectedDescriptor,
        entries: &[NamedEntry],
    ) -> Result<Reply, String> {
        context.authorize(ActionKind::Read)?;
        let held = select(context, folder)?;
        if !held.path.is_dir() {
            return Err("a named selection needs the folder it is shown in".into());
        }
        let mut paths = Vec::with_capacity(entries.len());
        for entry in entries {
            let path = held.path.join(OsString::from_vec(entry.name.clone()));
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| "a selected entry is no longer in its folder")?;
            if identity(&metadata) != entry.expected {
                return Err("a selected entry was replaced".into());
            }
            paths.push(path);
        }
        let count = paths.len();
        let ticket = self.hold(context, paths, false);
        Ok(Reply::CapturedNamed { ticket, count })
    }

    /// Release what `tickets` hold. A ticket another window captured is never released here.
    fn release(&self, context: &FileClipboardContext, tickets: &[String]) -> Reply {
        let mut state = self.state.lock_recover();
        for id in tickets {
            if state
                .tickets
                .get(id)
                .is_some_and(|ticket| ticket.belongs_to(context))
            {
                state.tickets.remove(id);
            }
        }
        Reply::Released
    }

    /// Take a copy of what `sources` hold, for pasting later.
    fn publish(
        &self,
        context: &FileClipboardContext,
        sources: &[String],
        cut: bool,
    ) -> Result<Reply, String> {
        context.authorize(if cut {
            ActionKind::Undoable
        } else {
            ActionKind::Read
        })?;
        let mut entries: Vec<Held> = Vec::new();
        let mut tickets = Vec::with_capacity(sources.len());
        for id in sources {
            let (paths, id) = self.held(context, id, false)?;
            for path in paths {
                if entries.iter().any(|held| held.path == path) {
                    return Err("the clipboard selection names an entry twice".into());
                }
                let metadata = std::fs::symlink_metadata(&path)
                    .map_err(|_| "a selected entry is no longer there")?;
                entries.push(Held {
                    path,
                    object: identity(&metadata),
                });
            }
            tickets.push(id);
        }
        if entries.is_empty() {
            return Err("there is nothing to copy".into());
        }
        if entries.len() > MAX_ENTRIES {
            return Err("the copy holds more entries than a clipboard may".into());
        }
        let public = runtime_collab::file_clipboard::Offer {
            id: uuid::Uuid::new_v4().to_string(),
            count: entries.len(),
            cut,
        };
        let now = Instant::now();
        let mut state = self.state.lock_recover();
        state.offers.retain(|_, offer| now < offer.expires);
        // The objects are the offer's now: its tickets took them here.
        for id in &tickets {
            state.tickets.remove(id);
        }
        state.offers.insert(
            context.workspace().to_owned(),
            Offer {
                id: public.id.clone(),
                cut,
                entries: entries.clone(),
                expires: now.checked_add(OFFER_LIFETIME).unwrap_or(now),
            },
        );
        drop(state);
        // What a file manager puts on the desktop's clipboard is the copy itself: pasting it into
        // any app is that app's business.
        self.published.lock_recover().push(CopiedFiles {
            paths: entries.into_iter().map(|held| held.path).collect(),
            cut,
        });
        (self.wake)();
        Ok(Reply::Published { offer: public })
    }

    /// The copy waiting in this workspace, if it is still good.
    fn snapshot(&self, context: &FileClipboardContext) -> Reply {
        let now = Instant::now();
        let mut state = self.state.lock_recover();
        state.offers.retain(|_, offer| now < offer.expires);
        let offer = state
            .offers
            .get(context.workspace())
            // A copy whose files were moved away or replaced since cannot be pasted, and a window
            // asking for the clipboard must not be told it can.
            .filter(|offer| offer.entries.iter().all(Held::still_there))
            .map(Offer::public);
        drop(state);
        Reply::Snapshot { offer }
    }

    /// Paste the offer `offer` names into the folder `destination` holds.
    fn paste(
        &self,
        context: &FileClipboardContext,
        offer: &str,
        destination: &str,
        operation: &str,
        asked: Asked,
    ) -> Result<Reply, String> {
        context.authorize(ActionKind::Undoable)?;
        {
            let state = self.state.lock_recover();
            if let Some(prior) = state.operations.get(operation) {
                if !prior.same_paste(context, offer, destination) {
                    return Err("the operation already names another transfer".into());
                }
                // The same paste, asked again after a reply went missing: answer what it is doing,
                // never start a second copy of it.
                return Ok(prior.answer(operation));
            }
        }
        let (cut, entries) = {
            let now = Instant::now();
            let mut state = self.state.lock_recover();
            state.offers.retain(|_, offer| now < offer.expires);
            let held = state
                .offers
                .get(context.workspace())
                .filter(|held| held.id == offer)
                .map(|held| (held.cut, held.entries.clone()))
                .ok_or("the clipboard offer is no longer there")?;
            drop(state);
            held
        };
        let (folder, _ticket) = self.held(context, destination, true)?;
        let folder = folder
            .first()
            .cloned()
            .ok_or("the paste has no folder to go into")?;
        if entries.iter().any(|held| !held.still_there()) {
            return Err("a copied entry was moved or replaced".into());
        }
        if let Some(ask) = asked_conflicts(&entries, &folder, asked) {
            return Ok(ask);
        }
        let conflict = match asked {
            Asked::Skip => Conflict::Skip,
            Asked::Replace => Conflict::Replace,
            Asked::Ask | Asked::KeepBoth => Conflict::KeepBoth,
        };
        // An agent acting for this person asked for the transfer: what it writes is recorded as
        // that agent's, for its person. A refusal leaves the writes the person's own and never
        // stops the transfer.
        if let Some(agent) = context.attributed_agent() {
            let mut paths = vec![folder.to_string_lossy().into_owned()];
            if cut {
                paths.extend(
                    entries
                        .iter()
                        .map(|held| held.path.to_string_lossy().into_owned()),
                );
            }
            if let Err(error) = Self::attribute(agent, paths) {
                tilo_failure::ignored!(
                    "the transfer stays its person's when it cannot be attributed",
                    tracing::debug!(%error, "a clipboard transfer stays its person's in file history")
                );
            }
        }
        let files = CopiedFiles {
            paths: entries.iter().map(|held| held.path.clone()).collect(),
            cut,
        };
        let Some(paste) = paste::Paste::start(
            &files,
            &folder,
            conflict,
            ui::time::unix_now_ms(),
            &self.wake,
        ) else {
            return Err("another paste of files is still running".into());
        };
        let mut state = self.state.lock_recover();
        if state.operations.len() >= MAX_OPERATIONS {
            state
                .operations
                .retain(|_, running| running.settled.is_none());
            if state.operations.len() >= MAX_OPERATIONS {
                state.operations.clear();
            }
        }
        state.operations.insert(
            operation.to_owned(),
            Running {
                peer: context.peer(),
                window: context.window().to_owned(),
                workspace: context.workspace().to_owned(),
                offer: offer.to_owned(),
                destination: destination.to_owned(),
                paste,
                settled: None,
            },
        );
        drop(state);
        Ok(Reply::Running {
            operation: operation.to_owned(),
        })
    }

    /// What the transfer `operation` names has done.
    fn observe(&self, context: &FileClipboardContext, operation: &str) -> Result<Reply, String> {
        context.authorize(ActionKind::Read)?;
        let mut state = self.state.lock_recover();
        let running = state
            .operations
            .get_mut(operation)
            .filter(|running| running.peer == context.peer())
            .ok_or("no such transfer")?;
        if running.settled.is_none() && running.paste.finished() {
            let outcome = running.paste.outcome();
            running.settled = Some((outcome.landed, pasting_error(&outcome)));
        }
        let answer = running.answer(operation);
        drop(state);
        Ok(answer)
    }
}

impl FileClipboardServices for NativeFileClipboard {
    fn command(&self, context: &FileClipboardContext, command: Command) -> Result<Reply, String> {
        if !command.valid() {
            return Err("invalid clipboard command".into());
        }
        match command {
            Command::Snapshot => {
                context.authorize(ActionKind::Read)?;
                Ok(self.snapshot(context))
            }
            Command::Capture {
                sources,
                destination,
            } => self.capture(context, &sources, destination.as_ref()),
            Command::CaptureNamed { folder, entries } => {
                self.capture_named(context, &folder, &entries)
            }
            Command::Release { tickets } => {
                context.authorize(ActionKind::Read)?;
                Ok(self.release(context, &tickets))
            }
            Command::Publish { sources, cut } => self.publish(context, &sources, cut),
            Command::Paste {
                offer,
                destination,
                operation,
                conflict,
            } => self.paste(context, &offer, &destination, &operation, conflict),
            Command::Observe { operation } => self.observe(context, &operation),
            // A paste is not undone from here: the files it wrote stand until the person removes
            // them, and a paste a session stopped in the middle of is undone as the session starts.
            Command::Undo { .. } | Command::Redo { .. } => Ok(Reply::Rejected {
                reason: "this session keeps no history of Files' transfers to undo".into(),
            }),
            Command::RestoreTrash { .. } => Ok(Reply::Rejected {
                reason: "this session keeps no Trash record of Files' transfers".into(),
            }),
            Command::Recover { .. } => Ok(Reply::Recovered {
                operations: Vec::new(),
                completed: 0,
                error: None,
            }),
            // A paste is staged and renamed into place file by file, so stopping it would leave the
            // person guessing which of them landed. It is answered as what it is: not stoppable
            // here.
            Command::Cancel { .. } => {
                context.authorize(ActionKind::Undoable)?;
                Ok(Reply::Rejected {
                    reason: "a paste of files cannot be stopped once it has started".into(),
                })
            }
        }
    }

    fn disconnect(&self, peer: u64) {
        let mut state = self.state.lock_recover();
        state.tickets.retain(|_, ticket| ticket.peer != peer);
        state.operations.retain(|_, running| running.peer != peer);
    }

    fn attribute_writes(&self, agent: &str, paths: Vec<String>) -> Result<(), String> {
        Self::attribute(agent, paths)
    }
}

/// What a finished paste has to tell the window of what did not land.
fn pasting_error(outcome: &paste::Outcome) -> Option<String> {
    use paste::Refused;
    if let Some(refusal) = &outcome.refusal {
        return Some(match refusal {
            Refused::Nothing => "the folder already has every one of them".to_owned(),
            Refused::NotAFolder(path) => {
                format!("{} is not a folder a paste can go into", path.display())
            }
            Refused::Stopped(reason) => reason.clone(),
        });
    }
    let names: Vec<&str> = outcome
        .failed
        .iter()
        .map(|(name, _)| name.as_str())
        .take(MAX_CONFLICT_NAMES)
        .collect();
    (!outcome.failed.is_empty()).then(|| {
        format!(
            "{} of {} did not paste: {}",
            outcome.failed.len(),
            outcome.total,
            names.join(", ")
        )
    })
}

/// What a paste asks the person to decide, when the app wanted to be told and a name it takes is
/// already used: the clashing names, or how many there.
fn asked_conflicts(entries: &[Held], folder: &Path, asked: Asked) -> Option<Reply> {
    if asked != Asked::Ask {
        return None;
    }
    let clashing: Vec<String> = entries
        .iter()
        .filter(|held| clashes(held, folder))
        .map(|held| {
            held.path.file_name().map_or_else(
                || held.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        })
        .collect();
    (!clashing.is_empty()).then(|| Reply::Conflicts {
        count: clashing.len(),
        names: clashing.into_iter().take(MAX_CONFLICT_NAMES).collect(),
    })
}

/// Whether the folder `into` has a different object than `held` under the name the entry takes.
fn clashes(held: &Held, into: &Path) -> bool {
    let Some(name) = held.path.file_name() else {
        return false;
    };
    let there = into.join(name);
    match std::fs::symlink_metadata(&there) {
        Ok(metadata) => identity(&metadata) != held.object,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %there.display(),
                "a paste destination could not be examined; the paste keeps both"
            );
            true
        }
    }
}

/// The path this session sees for the absolute path `named` of a window of `context` 's app.
/// An app reads its own private home where the launcher mounted it, at
/// [`launcher::appdir::SANDBOX_APP_DIR`]; this session holds that same directory as the workspace's
/// directory for the app. Every other mount an app browses keeps its place, so those paths are the
/// same in both.
fn session_path(context: &FileClipboardContext, named: &[u8]) -> PathBuf {
    let named = PathBuf::from(OsString::from_vec(named.to_vec()));
    let Ok(rest) = named.strip_prefix(launcher::appdir::SANDBOX_APP_DIR) else {
        return named;
    };
    let Some(data_home) = launcher::appdir::host_data_home() else {
        return named;
    };
    launcher::appdir::host_app_dir(&data_home, context.workspace(), context.app_id()).join(rest)
}

/// The object a selected descriptor names, once the object the window showed under it is what is
/// actually there: the descriptor the app handed over and the object the path names are the same,
/// or nothing is taken.
fn select(context: &FileClipboardContext, selected: &SelectedDescriptor) -> Result<Held, String> {
    if selected.path.first() != Some(&b'/') {
        return Err("a selected object is named by its absolute path".into());
    }
    let handed = context.duplicate_selected(selected.descriptor)?;
    let shown = handed
        .metadata()
        .map_err(|_| "the selected object cannot be read")?;
    if identity(&shown) != selected.expected {
        return Err("the selected object changed before it was taken".into());
    }
    let path = session_path(context, &selected.path);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "the selected object was moved: {} ({error})",
            path.display()
        )
    })?;
    if identity(&metadata) != selected.expected {
        return Err("the selected object was replaced or moved".into());
    }
    Ok(Held {
        path,
        object: selected.expected,
    })
}

/// The object a metadata describes, told apart from whatever else takes its name.
fn identity(metadata: &std::fs::Metadata) -> ObjectIdentity {
    ObjectIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

/// The filesystem daemon's content store, as this session reaches it: a connection of its own, made
/// on first use and again after it went away.
mod blobs {
    use runtime_fs::blobs::{BlobClient, BlobError};
    use tilo_lock::MutexRecover as _;

    static SESSION_BLOBS: std::sync::Mutex<Option<BlobClient>> = std::sync::Mutex::new(None);

    /// Run `call` against the filesystem daemon as this desktop session.
    ///
    /// # Errors
    /// Returns why the daemon could not be reached, or `call`'s refusal.
    pub(super) fn with_session_blobs<T>(
        call: impl FnOnce(&BlobClient) -> Result<T, BlobError>,
    ) -> Result<T, String> {
        let socket = launcher::services::blob_socket();
        let mut session = SESSION_BLOBS.lock_recover();
        let client = if let Some(client) = session.as_ref() {
            client.clone()
        } else {
            let client = BlobClient::connect_session(&socket).map_err(|error| error.to_string())?;
            *session = Some(client.clone());
            client
        };
        drop(session);
        let result = call(&client);
        if matches!(result, Err(BlobError::Disconnected(_))) {
            // Reconnect on the next call rather than keep a dead session.
            *SESSION_BLOBS.lock_recover() = None;
        }
        result.map_err(|error| error.to_string())
    }
}
