//! `tilo folders`: the workspaces this computer keeps a synced folder of.
//!
//! Each of the person's workspaces has one folder, the same everywhere: on
//! Tilo OS at `/var/workspace`, on any other computer logged in with
//! `tilo login` at `~/<workspace>` by default. The daemon `tilo login` leaves
//! running (`tilo-runtime`) mounts them; this command changes which, where,
//! and whether each syncs ([`tilo_api::daemon::folders`]), and the daemon applies it
//! within seconds.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use tilo_api::daemon::folders::{self, Folders};
use tilo_fs::files_sync::{Refusal, Waiting};

/// How long a command waits for the peer to apply what it changed.
const APPLYING: Duration = Duration::from_secs(20);

#[derive(Subcommand)]
pub enum FoldersCmd {
    /// Each folder: where it is, whether it is in sync, and how much of it is
    /// on this computer.
    #[command(visible_alias = "ls")]
    List,
    /// Whether the peer runs, and each folder's state.
    Status,
    /// Give a workspace a folder here (again), at `~/<workspace>` unless a
    /// path is given.
    Add {
        /// The workspace, by name or id.
        workspace: String,
        /// Where its folder goes.
        path: Option<PathBuf>,
    },
    /// Remove a workspace's folder from this computer; its files stay in the
    /// workspace and on the person's other devices.
    #[command(visible_alias = "rm")]
    Remove {
        /// The folder, or its workspace by name or id.
        folder: String,
    },
    /// Show a workspace's folder somewhere else.
    #[command(visible_alias = "mv")]
    Move {
        /// The folder, or its workspace by name or id.
        folder: String,
        /// Where it goes.
        to: PathBuf,
    },
    /// Stop syncing a folder: changes made here wait, and none arrive.
    Pause {
        /// The folder, or its workspace; every folder when not given.
        folder: Option<String>,
    },
    /// Sync a paused folder again.
    Resume {
        /// The folder, or its workspace; every folder when not given.
        folder: Option<String>,
    },
    /// Keep files or folders on this computer, to open offline.
    Keep {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Free the space files take on this computer; they stay in the folder
    /// and come back when opened.
    Free {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Whether workspaces get a folder here by themselves: one's own
    /// and ones joined (both on unless turned off).
    Auto {
        #[arg(long, value_name = "on|off")]
        own: Option<Switch>,
        #[arg(long, value_name = "on|off")]
        joined: Option<Switch>,
    },
    /// Stop the peer: unmount every folder, and start nothing at the next
    /// login (`tilo folders start` starts it again).
    Stop,
    /// Start the peer, as a user service from now on.
    Start,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Switch {
    On,
    Off,
}

/// Run `cmd`, answering in JSON when `json`.
///
/// # Errors
/// This computer is not logged in, or the change could not be kept or applied.
pub fn run(cmd: FoldersCmd, json: bool) -> Result<ExitCode> {
    if !crate::login::logged_in() {
        bail!("{}", crate::login::NOT_LOGGED_IN);
    }
    let place = crate::login::Place::here(crate::identity::paths())?;
    let dir = place.dir();
    let home = tilo_paths::home().context("HOME is missing")?;
    match cmd {
        FoldersCmd::List | FoldersCmd::Status => Ok(list(dir, json)),
        FoldersCmd::Start => {
            let running = start()?;
            if !json {
                println!("{}", running_said(&running));
            }
            Ok(list(dir, json))
        }
        FoldersCmd::Stop => {
            crate::service::stop()?;
            say(
                json,
                &json!({"stopped": true}),
                "Stopped: no folder is kept in sync here, and nothing starts at your next login (`tilo folders start` starts it again).",
            );
            Ok(ExitCode::SUCCESS)
        }
        FoldersCmd::Add { workspace, path } => add(dir, &home, &workspace, path, json),
        FoldersCmd::Remove { folder } => change(dir, &folder, json, |folder| {
            folder.removed = true;
            format!(
                "Removed the folder at {} from this computer; its files stay in the workspace.",
                folder.path.display()
            )
        }),
        FoldersCmd::Move { folder, to } => {
            let to = std::path::absolute(&to)?;
            change(dir, &folder, json, |folder| {
                folder.path.clone_from(&to);
                format!("{} is now at {}.", folder.name, to.display())
            })
        }
        FoldersCmd::Pause { folder } => each(dir, folder.as_deref(), json, true),
        FoldersCmd::Resume { folder } => each(dir, folder.as_deref(), json, false),
        FoldersCmd::Keep { paths } => residency(&paths, true, json),
        FoldersCmd::Free { paths } => residency(&paths, false, json),
        FoldersCmd::Auto { own, joined } => {
            let mut folders = Folders::load(dir)?;
            if let Some(own) = own {
                folders.auto_own = matches!(own, Switch::On);
            }
            if let Some(joined) = joined {
                folders.auto_joined = matches!(joined, Switch::On);
            }
            folders.save(dir)?;
            let said = format!(
                "Workspaces of your own get a folder here by themselves: {}. Workspaces you joined: {}.",
                if folders.auto_own { "yes" } else { "no" },
                if folders.auto_joined { "yes" } else { "no" },
            );
            say(
                json,
                &json!({"own": folders.auto_own, "joined": folders.auto_joined}),
                &said,
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn say(json: bool, value: &Value, text: &str) {
    if json {
        println!("{value}");
    } else {
        println!("{text}");
    }
}

fn running_said(running: &crate::service::Running) -> &'static str {
    match running {
        crate::service::Running::Service => {
            "Your folders are kept in sync by a user service, started again at every login."
        }
        crate::service::Running::Process => {
            "Your folders are kept in sync by a process of its own: no user service keeps them here, so nothing starts it again after this computer restarts or after the program that started it ends."
        }
    }
}

/// Start the peer when none is there; one that is there is waited for, never replaced for being
/// slow.
fn start() -> Result<crate::service::Running> {
    if !crate::login::logged_in() {
        bail!("{}", crate::login::NOT_LOGGED_IN);
    }
    crate::login::trust_test_certificates()?;
    crate::service::start()
}

/// What keeps the folders in sync, started when nothing does: `tilo status`
/// and `tilo folders` start it themselves and say how it stands, rather
/// than asking the person to.
///
/// # Errors
/// This computer is not logged in, or its background sync did not start or
/// did not answer.
pub(crate) fn sync() -> Result<Value> {
    if let Some(status) = crate::mcp::ask_peer() {
        return Ok(status);
    }
    start()?;
    crate::mcp::ask_peer().ok_or_else(|| {
        anyhow!(
            "Tilo's background sync started but isn't answering yet: run `tilo status` again in \
             a minute."
        )
    })
}

/// The folders, as the peer shows them and as kept here.
fn list(dir: &Path, json: bool) -> ExitCode {
    let folders = Folders::load(dir).unwrap_or_default();
    // Nothing running is started here: a person reading their folders gets
    // them kept in sync, not a command to run first.
    let peer = match sync() {
        Ok(status) => Some(status),
        Err(error) => {
            if !json {
                eprintln!("{error:#}");
            }
            None
        }
    };
    if json {
        println!(
            "{}",
            json!({
                "running": peer.is_some(),
                "peer": peer,
                "folders": folders,
            })
        );
        return ExitCode::SUCCESS;
    }
    let Some(peer) = peer else {
        println!("Your folders aren't in sync on this computer right now.");
        for (id, folder) in folders
            .workspaces
            .iter()
            .filter(|(_, folder)| !folder.removed)
        {
            println!(
                "  {}: {} (not running) [{id}]",
                folder.name,
                folder.path.display()
            );
        }
        return ExitCode::from(1);
    };
    let mounted = peer
        .get("mounted")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // The workspaces the person's list still has, when the host answered it:
    // one it no longer has was deleted, and its folder's place goes with it.
    let listed = peer.get("workspaces").and_then(Value::as_array);
    let complete = peer
        .get("workspaces_complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let gone = |id: &str| {
        complete
            && listed.is_some_and(|listed| {
                !listed
                    .iter()
                    .any(|entry| entry.get("workspace").and_then(Value::as_str) == Some(id))
            })
    };
    say_answered_late(&peer);
    if mounted.is_empty() && folders.workspaces.is_empty() {
        println!("No workspace has a folder here yet.");
    }
    for folder in &mounted {
        println!("  {}", describe_folder(folder));
    }
    for (id, folder) in &folders.workspaces {
        if gone(id) {
            continue;
        }
        let shown = mounted
            .iter()
            .any(|m| m.get("workspace").and_then(Value::as_str) == Some(id));
        if folder.removed {
            println!(
                "  {}: no folder here (removed; `tilo folders add {}` brings it back)",
                folder.name, folder.name
            );
        } else if !shown {
            println!("  {}: {} (starting)", folder.name, folder.path.display());
        }
    }
    for failed in peer
        .get("failed")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        println!(
            "  {} has no folder here: {}",
            failed
                .get("workspace")
                .and_then(Value::as_str)
                .unwrap_or("?"),
            failed.get("why").and_then(Value::as_str).unwrap_or("?"),
        );
    }
    let auto = |on: bool| if on { "on" } else { "off" };
    println!(
        "New workspaces get a folder by themselves: your own {}, joined {} (`tilo folders auto`).",
        auto(folders.auto_own),
        auto(folders.auto_joined)
    );
    ExitCode::SUCCESS
}

/// The workspace `named` (an id or name) of the person's, from the peer's list.
fn listed(named: &str) -> Option<(String, String)> {
    let peer = crate::mcp::ask_peer()?;
    peer.get("workspaces")?
        .as_array()?
        .iter()
        .find_map(|entry| {
            let id = entry.get("workspace")?.as_str()?;
            let name = entry.get("name")?.as_str()?;
            (id == named || name == named).then(|| (id.to_owned(), name.to_owned()))
        })
}

fn add(
    dir: &Path,
    home: &Path,
    named: &str,
    path: Option<PathBuf>,
    json: bool,
) -> Result<ExitCode> {
    start()?;
    let mut folders = Folders::load(dir)?;
    let id = if let Some(id) = folders.find(named) {
        id
    } else {
        {
            let (id, name) = listed(named).with_context(|| {
                format!("none of your workspaces is called {named}; `tilo folders` lists them")
            })?;
            folders.workspaces.insert(
                id.clone(),
                folders::Folder {
                    path: folders.free_path(home, &name),
                    name,
                    paused: false,
                    removed: false,
                },
            );
            id
        }
    };
    let folder = folders
        .workspaces
        .get_mut(&id)
        .context("the folder just added")?;
    folder.removed = false;
    if let Some(path) = path {
        folder.path = std::path::absolute(path)?;
    }
    let at = folder.path.clone();
    folders.save(dir)?;
    applied(&id, &Expect::At(at.clone(), folder_paused(&folders, &id)));
    say(
        json,
        &json!({"workspace": id, "path": at}),
        &format!(
            "The workspace's files are at {}, kept in sync.",
            at.display()
        ),
    );
    Ok(ExitCode::SUCCESS)
}

fn folder_paused(folders: &Folders, id: &str) -> bool {
    folders
        .workspaces
        .get(id)
        .is_some_and(|folder| folder.paused)
}

/// Change the folder `named` (its path, workspace id or name) with `change`,
/// which says what it did.
fn change(
    dir: &Path,
    named: &str,
    json: bool,
    change: impl FnOnce(&mut folders::Folder) -> String,
) -> Result<ExitCode> {
    let mut folders = Folders::load(dir)?;
    let id = folders
        .find(named)
        .with_context(|| format!("no folder here is {named}; `tilo folders` lists them"))?;
    let folder = folders.workspaces.get_mut(&id).context("the folder")?;
    let said = change(folder);
    let wanted = folder.clone();
    folders.save(dir)?;
    applied(
        &id,
        &if wanted.removed {
            Expect::Gone
        } else {
            Expect::At(wanted.path.clone(), wanted.paused)
        },
    );
    say(json, &json!({"workspace": id, "folder": wanted}), &said);
    Ok(ExitCode::SUCCESS)
}

/// Pause or resume one folder, or all.
fn each(dir: &Path, named: Option<&str>, json: bool, paused: bool) -> Result<ExitCode> {
    let word = if paused { "Paused" } else { "Syncing again" };
    if let Some(named) = named {
        return change(dir, named, json, |folder| {
            folder.paused = paused;
            format!("{word}: {}.", folder.path.display())
        });
    }
    let mut folders = Folders::load(dir)?;
    for folder in folders.workspaces.values_mut() {
        folder.paused = paused;
    }
    folders.save(dir)?;
    for id in folders.workspaces.keys() {
        applied(id, &Expect::Paused(paused));
    }
    say(
        json,
        &json!({"paused": paused}),
        &format!("{word}: every folder."),
    );
    Ok(ExitCode::SUCCESS)
}

/// What a change looks like once the peer applied it.
enum Expect {
    /// Shown at this path, paused or not.
    At(PathBuf, bool),
    /// Shown, paused or not, wherever it is.
    Paused(bool),
    /// Not shown.
    Gone,
}

impl Expect {
    fn met(&self, shown: Option<&Value>) -> bool {
        let paused = |shown: &Value| {
            shown
                .get("paused")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        match (self, shown) {
            (Self::Gone, None) => true,
            (Self::At(at, wanted), Some(shown)) => {
                shown.get("at").and_then(Value::as_str) == Some(&*at.to_string_lossy())
                    && paused(shown) == *wanted
            }
            (Self::Paused(wanted), Some(shown)) => paused(shown) == *wanted,
            _ => false,
        }
    }
}

/// Wait until the peer shows `workspace` as `expect` says, for at most
/// [`APPLYING`].
fn applied(workspace: &str, expect: &Expect) {
    let Some(until) = Instant::now().checked_add(APPLYING) else {
        return;
    };
    while Instant::now() < until {
        let Some(peer) = crate::mcp::ask_peer() else {
            return;
        };
        let mounted = peer
            .get("mounted")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let shown = mounted
            .iter()
            .find(|m| m.get("workspace").and_then(Value::as_str) == Some(workspace));
        if expect.met(shown) {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Keep `paths` on this computer, or free the space they take: the same
/// residency Tilo OS's folder has, asked of the filesystem itself.
fn residency(paths: &[PathBuf], keep: bool, json: bool) -> Result<ExitCode> {
    use fs_protocol::projection::{AVAILABILITY_CONTROL_XATTR, PIN_XATTR};
    let mut done = Vec::new();
    for path in paths {
        let pin: &[u8] = if keep { b"pinned" } else { b"automatic" };
        rustix::fs::setxattr(path, PIN_XATTR, pin, rustix::fs::XattrFlags::empty())
            .with_context(|| format!("{} is not in a synced folder", path.display()))?;
        if !keep {
            for file in files_in(path) {
                // A file held nowhere else stays: freeing never loses it.
                if let Err(error) = rustix::fs::setxattr(
                    &file,
                    AVAILABILITY_CONTROL_XATTR,
                    b"evict",
                    rustix::fs::XattrFlags::empty(),
                ) {
                    eprintln!("{} stays on this computer: {error}", file.display());
                }
            }
        }
        done.push(path.display().to_string());
    }
    let text = if keep {
        "Kept on this computer, to open offline."
    } else {
        "Freed: the files stay in the folder and come back when opened."
    };
    say(json, &json!({"kept": keep, "paths": done}), text);
    Ok(ExitCode::SUCCESS)
}

/// `path` and every file below it.
fn files_in(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut todo = vec![path.to_path_buf()];
    while let Some(at) = todo.pop() {
        match std::fs::read_dir(&at) {
            Ok(entries) => todo.extend(entries.flatten().map(|entry| entry.path())),
            Err(_) => files.push(at),
        }
    }
    files
}

/// What `tilo login` does last: give the workspace it entered a folder here
/// and start the peer that keeps it. The folder's path, for its closing words.
///
/// # Errors
/// The folder could not be kept or the peer could not start.
pub(crate) fn after_login(workspace: &str, name: Option<&str>) -> Result<PathBuf> {
    let place = crate::login::Place::here(crate::identity::paths())?;
    let dir = place.dir();
    let home = tilo_paths::home().context("HOME is missing")?;
    let mut folders = Folders::load(dir)?;
    folders.offer(workspace, name.unwrap_or("Workspace"), true, &home);
    folders.save(dir)?;
    let path = folders
        .workspaces
        .get(workspace)
        .filter(|folder| !folder.removed)
        .map(|folder| folder.path.clone())
        .context(
            "this computer keeps no folder of the workspace (`tilo folders add` gives it one)",
        )?;
    start()?;
    Ok(path)
}

/// Say that the peer's workers did not answer in time, when its answer is what it said last: a
/// peer that stays that way is replaced by a fresh one.
fn say_answered_late(peer: &Value) {
    let Some(ago) = peer.get("answered_ago_s").and_then(Value::as_u64) else {
        return;
    };
    let restarts = peer
        .get("restarts_after_s")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!(
        "Tilo's background sync is busy and hasn't answered for {ago}s, so this is how it stood \
         then. If it stays busy for {restarts}s, it starts again by itself."
    );
}

/// `n` bytes as a person reads them: one decimal in the largest unit that
/// holds a whole one.
pub(crate) fn size(n: u64) -> String {
    let units = [
        ("TB", 1_000_000_000_000_u64),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("KB", 1_000),
    ];
    units.iter().find(|(_, scale)| n >= *scale).map_or_else(
        || format!("{n} B"),
        |(unit, scale)| {
            let tenths = n.saturating_mul(10).checked_div(*scale).unwrap_or(0);
            format!("{}.{} {unit}", tenths / 10, tenths % 10)
        },
    )
}

/// What `tilo status` prints of the peer's answer to its status request.
pub(crate) fn describe(status: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let text = |key: &str| {
        status
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    };
    let mut said = format!(
        "This computer is \"{}\" in Tilo, one of the person's computers, through {}.\n",
        text("device"),
        text("host"),
    );
    if let Some(why) = status.get("not_in").and_then(serde_json::Value::as_str) {
        let _ = writeln!(said, "{why}");
    }
    let mounted = status
        .get("mounted")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if mounted.is_empty() {
        said.push_str("No workspace has a folder here yet.\n");
    }
    for workspace in &mounted {
        let _ = writeln!(said, "  {}", describe_folder(workspace));
    }
    for failed in status
        .get("failed")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let _ = writeln!(
            said,
            "  {} has no folder here: {}",
            failed
                .get("workspace")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            failed
                .get("why")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
        );
    }
    said
}

/// Why a folder's changes wait, in a person's words.
fn waiting(why: &Waiting) -> String {
    match why {
        Waiting::Unopened
        | Waiting::RetiredUnanswered
        | Waiting::RetiredUnarrived
        | Waiting::RetiredOrdered
        | Waiting::RetiredUnsent
        | Waiting::RetiredLacking => {
            "the workspace hasn't said which files it holds yet, so nothing changed here goes out"
                .to_owned()
        }
        Waiting::Refused(Refusal::NotPermitted) => {
            "the workspace's rules don't let you make these \
                                                     changes; ask an owner of the workspace"
                .to_owned()
        }
        Waiting::Refused(Refusal::Unanswered) => {
            "the workspace isn't answering; the change is sent again shortly".to_owned()
        }
        Waiting::Refused(Refusal::Other(why)) => {
            format!("the workspace didn't take the change ({why}); it is sent again shortly")
        }
    }
}

/// One folder in a sentence: where, what state, and how much is here.
pub(crate) fn describe_folder(workspace: &serde_json::Value) -> String {
    let field = |key: &str| {
        workspace
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let number = |key: &str| {
        workspace
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let at = workspace
        .get("at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let name = workspace
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    // A peer that does not say is one that answers for its session itself.
    let connected = workspace
        .get("connected")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let stuck = workspace
        .get("stuck")
        .and_then(|why| serde_json::from_value::<Vec<Waiting>>(why.clone()).ok())
        .and_then(|why| why.into_iter().next())
        .map(|why| format!("changes made here wait: {}", waiting(&why)));
    // The folder says what it is doing as one state; only whether this computer reaches the
    // workspace at all is the daemon's own word beside it.
    let said = workspace
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("opening");
    let state = match said {
        "paused" => "paused",
        "waiting-for-workspace" => "waiting for the workspace to reach this computer",
        _ if !connected => "not connected to the workspace; changes made here wait to go out",
        "opening" => "waiting for the workspace's files",
        "stuck" => stuck.as_deref().unwrap_or("sending changes"),
        "content-going-out" => "some files aren't on the workspace's server yet",
        "in-sync" => "in sync",
        _ => "sending changes",
    };
    let read_only = if field("read_only") {
        ", read-only"
    } else {
        ""
    };
    format!(
        "{name}: {at} ({state}{read_only}; {} files, {} of {} on this computer)",
        number("files"),
        size(number("local_bytes")),
        size(number("bytes")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `tilo folders` says of a folder whose sync stands as `status`, on a computer that
    /// reaches the workspace or not.
    fn said(mut status: tilo_fs::files_sync::Status, connected: bool) -> String {
        status.files = 1;
        status.bytes = 1;
        status.state = status.folder_state();
        let mut folder = serde_json::to_value(&status).unwrap();
        folder["name"] = serde_json::json!("Tilo");
        folder["at"] = serde_json::json!("/home/ada/Tilo");
        folder["connected"] = serde_json::json!(connected);
        describe_folder(&folder)
    }

    /// A folder whose every change went out and whose content a holder keeps whole.
    fn whole() -> tilo_fs::files_sync::Status {
        tilo_fs::files_sync::Status {
            opened: true,
            sent: true,
            durable: true,
            host_lacks: Some(0),
            ..tilo_fs::files_sync::Status::default()
        }
    }

    /// A folder whose device has no live session with the workspace is never "in sync": what it
    /// last sent says nothing of what is written there since.
    #[test]
    fn a_folder_without_a_session_is_not_in_sync() {
        assert!(said(whole(), true).contains("(in sync;"));
        let offline = said(whole(), false);
        assert!(!offline.contains("in sync"), "{offline}");
        assert!(offline.contains("not connected"), "{offline}");
    }

    /// A folder whose changes all went out is not "in sync" while the workspace's server still
    /// lacks some of its files' content: wiping this computer then could lose them.
    #[test]
    fn a_folder_the_server_lacks_files_of_is_not_in_sync() {
        let lacking = said(
            tilo_fs::files_sync::Status {
                host_lacks: Some(3),
                ..whole()
            },
            true,
        );
        assert!(
            lacking.contains("(some files aren't on the workspace's server yet;"),
            "{lacking}"
        );
    }

    /// A folder whose workspace is not on this computer says it waits for it, rather than
    /// reading as sending changes forever.
    #[test]
    fn a_folder_waiting_for_its_workspace_says_so() {
        let waiting = said(
            tilo_fs::files_sync::Status {
                opened: false,
                waiting_for_workspace: true,
                ..whole()
            },
            true,
        );
        assert!(
            waiting.contains("(waiting for the workspace to reach this computer;"),
            "{waiting}"
        );
        assert!(said(whole(), true).contains("(in sync;"));
    }

    /// A folder whose changes wait for part of the workspace's file list that has not arrived
    /// says so and why, rather than that it is sending changes for as long as that lasts.
    #[test]
    fn a_folder_whose_changes_wait_says_why() {
        let sending = tilo_fs::files_sync::Status {
            sent: false,
            ..whole()
        };
        assert!(said(sending.clone(), true).contains("(sending changes;"));
        let waiting = said(
            tilo_fs::files_sync::Status {
                stuck: vec![tilo_fs::files_sync::Waiting::Refused(
                    tilo_fs::files_sync::Refusal::Unanswered,
                )],
                ..sending
            },
            true,
        );
        assert!(
            waiting.contains("(changes made here wait: the workspace isn't answering;"),
            "{waiting}"
        );
    }
}
