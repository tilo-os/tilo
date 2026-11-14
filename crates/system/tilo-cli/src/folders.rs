//! `tilo folders`: the workspaces this computer keeps a synced folder of.
//!
//! Each of the person's workspaces has one folder, the same everywhere: on
//! Tilo OS at `/var/workspace`, on any other computer logged in with
//! `tilo login` at `~/<workspace>` by default. The peer `tilo login` leaves
//! running ([`crate::peer`]) mounts them; this command changes which, where,
//! and whether each syncs ([`crate::peer::folders`]), and the peer applies it
//! within seconds.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::peer::folders::Folders;

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
    /// (on unless turned off), and ones joined (off unless turned on).
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
    let dir = crate::login::device_dir()?;
    let home = tilo_paths::home().context("HOME is missing")?;
    match cmd {
        FoldersCmd::List | FoldersCmd::Status => Ok(list(&dir, json)),
        FoldersCmd::Start => {
            let running = start()?;
            if !json {
                println!("{}", running_said(&running));
            }
            Ok(list(&dir, json))
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
        FoldersCmd::Add { workspace, path } => add(&dir, &home, &workspace, path, json),
        FoldersCmd::Remove { folder } => change(&dir, &folder, json, |folder| {
            folder.removed = true;
            format!(
                "Removed the folder at {} from this computer; its files stay in the workspace.",
                folder.path.display()
            )
        }),
        FoldersCmd::Move { folder, to } => {
            let to = std::path::absolute(&to)?;
            change(&dir, &folder, json, |folder| {
                folder.path.clone_from(&to);
                format!("{} is now at {}.", folder.name, to.display())
            })
        }
        FoldersCmd::Pause { folder } => each(&dir, folder.as_deref(), json, true),
        FoldersCmd::Resume { folder } => each(&dir, folder.as_deref(), json, false),
        FoldersCmd::Keep { paths } => residency(&paths, true, json),
        FoldersCmd::Free { paths } => residency(&paths, false, json),
        FoldersCmd::Auto { own, joined } => {
            let mut folders = Folders::load(&dir)?;
            if let Some(own) = own {
                folders.auto_own = matches!(own, Switch::On);
            }
            if let Some(joined) = joined {
                folders.auto_joined = matches!(joined, Switch::On);
            }
            folders.save(&dir)?;
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
            "Your folders are kept in sync by a process of its own until this computer restarts (no user service manager answers here)."
        }
    }
}

/// Start the peer unless it runs.
fn start() -> Result<crate::service::Running> {
    if !crate::login::logged_in() {
        bail!("{}", crate::login::NOT_LOGGED_IN);
    }
    if crate::mcp::ask_peer().is_some() {
        return Ok(crate::service::Running::Service);
    }
    crate::login::trust_test_certificates()?;
    crate::service::start()
}

/// The folders, as the peer shows them and as kept here.
fn list(dir: &Path, json: bool) -> ExitCode {
    let folders = Folders::load(dir).unwrap_or_default();
    let peer = crate::mcp::ask_peer();
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
        println!("Nothing keeps your folders in sync on this computer: run `tilo folders start`.");
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
    if mounted.is_empty() && folders.workspaces.is_empty() {
        println!("No workspace has a folder here yet.");
    }
    for folder in &mounted {
        println!("  {}", crate::peer::describe_folder(folder));
    }
    for (id, folder) in &folders.workspaces {
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
                crate::peer::folders::Folder {
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
    change: impl FnOnce(&mut crate::peer::folders::Folder) -> String,
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
    let dir = crate::login::device_dir()?;
    let home = tilo_paths::home().context("HOME is missing")?;
    let mut folders = Folders::load(&dir)?;
    folders.offer(workspace, name.unwrap_or("Workspace"), true, &home);
    folders.save(&dir)?;
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
