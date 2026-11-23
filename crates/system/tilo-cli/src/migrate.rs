//! `tilo migrate`: bring the person's things from this computer and the services they use into
//! their workspace, driven by their agent, with no window: the terminal's view of the one
//! migration engine ([`tilo_migrate::engine`]).
//!
//! The sources are listed in one table ([`engine::registry::SOURCES`]): folders on this computer, its browsers and Thunderbird, a mail account,
//! Google Drive, `OneDrive`, Dropbox, iCloud Drive, Nextcloud and `WebDAV`, and Outline, Linear and
//! Forgejo. Each step is a command of its own, so an agent can show the person what a source
//! holds before anything moves:
//!
//! ```text
//! tilo migrate sources                         every source, and how to connect each
//! tilo migrate connect gdrive                  sign in (in the browser) where a source needs it
//! tilo migrate review thunderbird              what it holds: accounts, folders, …
//! tilo migrate choose thunderbird ada@work.example
//! tilo migrate start thunderbird               bring the chosen things over
//! tilo migrate status / stop
//! tilo migrate undo <run>                      take a run back
//! ```
//!
//! Files (folders here, cloud drives) are copied into the workspace's folder on this computer
//! when it has one, as fast as the disk or network goes ([`files`]).
//!
//! A run lands in the workspace this computer's device is in, or, with `-w <workspace>`, in a
//! hosted workspace through Tilo's API with the key this computer holds: what comes over is the
//! agent's run in the workspace (see [`workspace`]), one History entry per run, each record
//! arriving live in the person's tab, the run undone as one step. A run keeps a journal here,
//! item by item, so it resumes after a stop and a second run adds nothing. Keys and passwords
//! never pass through the agent: a service's key is typed by the person at a hidden prompt or
//! read from a file they name, and kept in this computer's vault.

mod workspace;

use std::collections::BTreeMap;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use serde_json::Value;

use tilo_migrate::engine::{self, state::Place, writer::Workspace};

#[derive(Subcommand)]
pub enum MigrateCmd {
    /// Every source, what it holds here, and how to connect the ones that need it.
    Sources,
    /// Connect a source that needs signing in: Google Drive, `OneDrive` and Dropbox in the browser
    /// (the link is opened or printed, and this finishes by itself); iCloud, Nextcloud, `WebDAV`,
    /// a mail account (IMAP), Outline, Linear and Forgejo with a password or key typed at a hidden
    /// prompt (or read from --secret-file). It is kept in this computer's vault and never printed.
    Connect {
        /// gdrive, onedrive, dropbox, icloud, nextcloud, webdav, mail, outline, linear or forgejo.
        service: String,
        /// The address: a Nextcloud or `WebDAV` server, Outline's site, Linear's API when not its
        /// own, the Forgejo repository or owner whose issues come over.
        #[arg(long)]
        url: Option<String>,
        /// The account: a mail address, an Apple ID, a Nextcloud or `WebDAV` user name.
        #[arg(long)]
        login: Option<String>,
        /// A file holding the key or password, which the person made (not `--key-file`, which
        /// names the key a hosted workspace is reached with).
        #[arg(long, value_name = "FILE")]
        secret_file: Option<PathBuf>,
    },
    /// List what a source holds, with counts, and keep the review for `choose` and `start`.
    Review {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
        /// The Thunderbird profile, when this computer has several.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Choose what of a reviewed source comes over (or, with --leave-out, what stays).
    Choose {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
        /// Items by the key the review lists: a folder, an account's address, `contacts/…`,
        /// `calendar/…`, an Outline collection, a Linear team, a Forgejo repository; for home,
        /// also any folder by its path (`~/code/site`).
        #[arg(required = true)]
        items: Vec<String>,
        /// Leave these out instead.
        #[arg(long)]
        leave_out: bool,
        /// Choose only these: everything else is left out.
        #[arg(long, conflicts_with = "leave_out")]
        only: bool,
    },
    /// Bring the chosen things over, as one run in the workspace; resumes a stopped run.
    Start {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
        /// Connect the chosen mail accounts without asking first. Without it, each account is
        /// shown here and connected once you say yes.
        #[arg(long)]
        yes: bool,
    },
    /// How each source's migration stands.
    Status {
        /// One source; all of them when not given.
        source: Option<String>,
    },
    /// Stop a running migration after the item it is on; `start` resumes it.
    Stop {
        /// A source, as `tilo migrate sources` names it (home, gdrive, thunderbird, …).
        source: String,
    },
    /// Take a run back: everything it brought leaves the workspace, as undoing its History
    /// entry does, and a later `start` brings it again.
    Undo {
        /// The run, by the id `status` names (`migrate-linear-…`), or a source for its last run.
        run: String,
    },
}

/// Where a run lands.
pub enum Target {
    /// The workspace this computer's device is in, through its held session.
    Device,
    /// A hosted workspace, through Tilo's API with the key this computer holds.
    Hosted {
        /// The API server, as `--api` names it.
        api: Option<String>,
        /// A key file the person named, as `--key-file` does.
        key_file: Option<PathBuf>,
        /// The workspace, by name or id.
        workspace: String,
    },
}

impl Target {
    /// The API `reach` this target's key is opened with.
    fn reach(&self) -> crate::api::Reach<'_> {
        match self {
            Self::Device => crate::api::Reach {
                api: None,
                key_file: None,
                json: false,
            },
            Self::Hosted { api, key_file, .. } => crate::api::Reach {
                api: api.as_deref(),
                key_file: key_file.as_deref(),
                json: false,
            },
        }
    }

    /// The place a run's journal is kept in: named by the workspace it lands in.
    fn place(&self) -> Result<Place> {
        Place::of(&self.workspace_id()?)
    }

    /// The id of the workspace a run lands in.
    fn workspace_id(&self) -> Result<String> {
        match self {
            Self::Hosted { workspace, .. } => {
                workspace::Hosted::workspace_id(self.reach(), workspace)
            }
            Self::Device => Ok(crate::api::entered_workspace(crate::api::Reach {
                api: None,
                key_file: None,
                json: false,
            })?
            .map_or_else(|| "this-computer".to_owned(), |(workspace, _)| workspace)),
        }
    }

    /// Whether a run lands through Tilo's API rather than this computer's session.
    const fn is_hosted(&self) -> bool {
        matches!(self, Self::Hosted { .. })
    }

    /// The workspace a run lands in.
    fn connect(&self) -> Result<Box<dyn Workspace>> {
        Ok(match self {
            Self::Device => Box::new(workspace::Held::connect()?),
            Self::Hosted { workspace, .. } => {
                Box::new(workspace::Hosted::connect(self.reach(), workspace)?)
            }
        })
    }
}

/// Run `cmd` into this computer's workspace; `json` answers as JSON.
pub fn run(cmd: MigrateCmd, json: bool) -> Result<ExitCode> {
    run_into(cmd, json, &Target::Device)
}

/// Run `cmd` into `target`'s workspace; `json` answers as JSON.
pub fn run_into(cmd: MigrateCmd, json: bool, target: &Target) -> Result<ExitCode> {
    let place = target.place()?;
    let home = home()?;
    let folders = folders();
    let open = || -> Result<Box<dyn Workspace>> { Ok(Box::new(workspace::Held::connect()?)) };
    let cx = engine::Cx {
        place: &place,
        home: &home,
        folders: &folders,
        open: &open,
        secret: &secret,
    };
    let answer = match cmd {
        MigrateCmd::Sources => engine::sources(&cx),
        MigrateCmd::Connect {
            service,
            url,
            login,
            secret_file,
        } => engine::connect(
            &cx,
            &service,
            engine::Connecting {
                url: url.as_deref(),
                login: login.as_deref(),
                secret_file: secret_file.as_deref(),
            },
        )?,
        MigrateCmd::Review { source, profile } => engine::review(&cx, &source, profile.as_deref())?,
        MigrateCmd::Choose {
            source,
            items,
            leave_out,
            only,
        } => engine::choose(&cx, &source, &items, leave_out, only)?,
        MigrateCmd::Start { source, yes } => {
            let mut progress = |line: &str| {
                if !json {
                    eprintln!("{line}");
                }
            };
            let mut confirm = |question: &str| confirmed(question, json, yes);
            let workspace = target.workspace_id()?;
            engine::start(
                &cx,
                &source,
                engine::Asking {
                    progress: &mut progress,
                    confirm: &mut confirm,
                    workspace: &workspace,
                    hosted: target.is_hosted(),
                },
                &|| target.connect(),
            )?
        }
        MigrateCmd::Status { source } => engine::status(&cx, source.as_deref())?,
        MigrateCmd::Undo { run } => engine::undo(&cx, &run, &mut *target.connect()?)?,
        MigrateCmd::Stop { source } => engine::stop(&cx, &source)?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&answer)?);
    } else {
        print!("{}", engine::sentences(&answer));
    }
    let failed = answer.get("state").and_then(Value::as_str) == Some("failed");
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The home whose folders, browsers and Thunderbird are read: this account's.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("this account has no home directory")
}

/// The workspace folders this computer keeps, by workspace id.
fn folders() -> BTreeMap<String, PathBuf> {
    crate::login::Place::here(crate::identity::paths())
        .ok()
        .and_then(|login| tilo_api::daemon::folders::Folders::load(login.dir()).ok())
        .map(|folders| {
            folders
                .workspaces
                .into_iter()
                .filter(|(_, folder)| !folder.removed)
                .map(|(id, folder)| (id, folder.path))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the person says yes to `question`, asked at this terminal: yes when they said so
/// beforehand (`--yes`). Nobody at a terminal to ask is no answer, and the step is skipped.
///
/// # Errors
/// Nobody can be asked here and nobody said yes beforehand.
fn confirmed(question: &str, json: bool, yes: bool) -> Result<bool> {
    use std::io::{BufRead as _, Write as _};
    if yes {
        return Ok(true);
    }
    let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    else {
        bail!(
            "{question}\nNobody can be asked at a terminal here: start it in yours, or pass --yes \
             to `tilo migrate start` to say yes beforehand"
        );
    };
    if json && !std::io::stdin().is_terminal() {
        bail!("{question}\nPass --yes to `tilo migrate start` to say yes beforehand");
    }
    let mut writer = tty.try_clone()?;
    write!(writer, "{question} [Y/n] ")?;
    writer.flush()?;
    let mut said = String::new();
    std::io::BufReader::new(tty).read_line(&mut said)?;
    let said = said.trim().to_ascii_lowercase();
    Ok(said.is_empty() || said == "y" || said == "yes")
}

/// A secret the person types, unseen, or reads from a file they name; never an argument.
fn secret(prompt: &str, file: Option<&Path>) -> Result<zeroize::Zeroizing<String>> {
    if let Some(file) = file {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("{} is unreadable", file.display()))?;
        return Ok(zeroize::Zeroizing::new(text.trim().to_owned()));
    }
    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()
        .filter(|_| std::io::stdin().is_terminal() || std::env::var_os("TILO_ASK_TTY").is_some());
    let Some(tty) = tty.as_mut() else {
        bail!(
            "the key is typed by the person, never passed by an agent: ask them to run this in their terminal, or to save the key in a file and pass --secret-file"
        );
    };
    read_hidden(tty, prompt)
}

/// One line the person types on the terminal `tty` after `prompt`, not shown as it is typed.
pub(crate) fn read_hidden(
    tty: &mut std::fs::File,
    prompt: &str,
) -> Result<zeroize::Zeroizing<String>> {
    use rustix::termios;
    use std::io::{BufRead as _, Write as _};
    write!(tty, "{prompt}")?;
    tty.flush()?;
    let before = termios::tcgetattr(&*tty).ok();
    if let Some(before) = &before {
        let mut hidden = before.clone();
        hidden.local_modes.remove(termios::LocalModes::ECHO);
        let _ = termios::tcsetattr(&*tty, termios::OptionalActions::Now, &hidden);
    }
    let mut line = zeroize::Zeroizing::new(String::new());
    let read = std::io::BufReader::new(&*tty).read_line(&mut line);
    if let Some(before) = &before {
        let _ = termios::tcsetattr(&*tty, termios::OptionalActions::Now, before);
    }
    writeln!(tty)?;
    read?;
    Ok(zeroize::Zeroizing::new(line.trim().to_owned()))
}
