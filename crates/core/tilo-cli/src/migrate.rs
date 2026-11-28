//! `tilo migrate`: bring the person's things from this computer and the services they use into
//! their workspace, with no window: the terminal's view of this device's migrations, the actions
//! of provider `so.tilo.migrate` on `devices/<this device>/migrate`
//! ([`tilo_migrate::provider`]), which the device's daemon runs.
//!
//! The sources are listed in one table ([`tilo_migrate::engine::registry::SOURCES`]): folders on
//! this computer, its browsers and Thunderbird, a mail account, Google Drive, `OneDrive`, Dropbox,
//! iCloud Drive, Nextcloud and `WebDAV`, and Outline, Linear and Forgejo. Each step is a command
//! of its own, so an agent can show the person what a source holds before anything moves:
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
//! Each command is one invocation of the step's action, as the person whose account runs it,
//! through the daemon's native endpoint; the Migrations window and the person's agents invoke the
//! same actions, so what one chooses or starts the others see. A run is the invocation of
//! `start`: this command follows it and prints what it brings as it goes. Keys and passwords
//! never pass through the agent: a step that asks for one fails saying so, and the person types it
//! here at a hidden prompt (or names a file holding it), after which the step runs again with it.
//!
//! Connecting a mail account is a decision the person makes, so the action that keeps the account
//! asks them first, as its asked invocation record. Only they answer it, where Tilo asks them on
//! their screen: this command is a program, so it says the question and waits for their answer.
//!
//! `-w` names the workspace the migration lands in: the device's own by its id or name, or by its
//! id another the device is a member of, where the run lands as the migration's agent through the
//! device's daemon. A workspace the device is not in is refused, naming `tilo login`.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Map, Value, json};
use tilo_action_line::LineClient;
use tilo_actions::ActionFailure;
use tilo_actions::remote::Caller;
use tilo_migrate::engine;
use tilo_migrate::provider::SECRET_NEEDED;
use tilo_migrate::provider::caller::{Answering, Steps};

/// How long a command waits for this computer's daemon to answer, its service started.
const REACHING: Duration = Duration::from_secs(30);

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
        /// The Thunderbird profile: one of this computer's by its name, when it has several, or
        /// any profile by the path of its folder or its prefs.js.
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

/// Run `cmd` on this computer's device; `json` answers as JSON. `workspace` names the workspace
/// it lands in with `-w`, which must be one this computer's device is a member of.
pub fn run(cmd: MigrateCmd, json: bool, workspace: Option<&str>) -> Result<ExitCode> {
    let mut device = reach()?;
    if let Some(named) = workspace {
        device.in_workspace(named)?;
    }
    // The command's own runtime, on the process's main thread: a step awaits the device's
    // answers, and this is where the command waits for its steps.
    let runtime = crate::login::runtime()?;
    let call = |action: &str, args: Map<String, Value>| {
        step(&runtime, &device, action, args, json)?.map_err(failed)
    };
    let answer = match cmd {
        MigrateCmd::Sources => listed_sources(call("sources", Map::new())?)?,
        MigrateCmd::Connect {
            service,
            url,
            login,
            secret_file,
        } => {
            let mut args = Map::new();
            args.insert("source".into(), json!(service));
            if let Some(url) = url {
                args.insert("url".into(), json!(url));
            }
            if let Some(login) = login {
                args.insert("login".into(), json!(login));
            }
            if let Some(file) = &secret_file {
                args.insert("secret".into(), json!(*secret("", Some(file))?));
            }
            match step(&runtime, &device, "connect", args.clone(), json)? {
                Ok(answer) => answer,
                Err(failure) => match asked_secret(&failure) {
                    // The person types what it asks for, and it runs again with it.
                    Some(asks) if secret_file.is_none() => {
                        args.insert("secret".into(), json!(*secret(&asks, None)?));
                        call("connect", args)?
                    }
                    _ => return Err(failed(failure)),
                },
            }
        }
        MigrateCmd::Review { source, profile } => {
            let mut args = sourced(&source);
            if let Some(profile) = profile {
                args.insert("profile".into(), json!(handed_profile(&profile)?));
            }
            call("review", args)?
        }
        MigrateCmd::Choose {
            source,
            items,
            leave_out,
            only,
        } => {
            let mut args = sourced(&source);
            args.insert("items".into(), json!(items));
            args.insert("leave_out".into(), json!(leave_out));
            args.insert("only".into(), json!(only));
            call("choose", args)?
        }
        MigrateCmd::Start { source } => {
            let args = sourced(&source);
            if !json {
                eprintln!(
                    "Bringing {source} over; `tilo migrate stop {source}` stops it after the item \
                     it is on."
                );
            }
            call("start", args)?
        }
        MigrateCmd::Status { source } => {
            let args = source.as_deref().map(sourced).unwrap_or_default();
            call("status", args)?
        }
        MigrateCmd::Stop { source } => call("stop", sourced(&source))?,
        MigrateCmd::Undo { run } => {
            let mut args = Map::new();
            args.insert("run".into(), json!(run));
            call("undo", args)?
        }
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

/// The sources the device answers, as the list a terminal prints: the action answers them as the
/// JSON text the app that lists them decodes.
///
/// # Errors
/// The device answered no JSON where it answers its sources.
fn listed_sources(answered: Value) -> Result<Value> {
    match answered.as_str() {
        Some(text) => serde_json::from_str(text)
            .with_context(|| format!("the device's sources are no JSON: {text}")),
        None => Ok(answered),
    }
}

/// The arguments naming `source`.
fn sourced(source: &str) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("source".into(), json!(source));
    args
}

/// The Thunderbird profile `profile` as the device reads it: one of its own by name, or a
/// profile folder or prefs.js the person names by a path here, which the device reads from
/// where it is.
fn handed_profile(profile: &str) -> Result<String> {
    let path = Path::new(profile);
    if !path.exists() {
        return Ok(profile.to_owned());
    }
    let path =
        std::fs::canonicalize(path).with_context(|| format!("{} is unreadable", path.display()))?;
    Ok(path.display().to_string())
}

/// What a step that failed `failure` asks the person to type, when it failed for a key or
/// password it was not given.
fn asked_secret(failure: &ActionFailure) -> Option<String> {
    if failure.code != SECRET_NEEDED {
        return None;
    }
    failure
        .details
        .as_ref()?
        .get("asks")?
        .as_str()
        .map(str::to_owned)
}

/// A step's failure, as the person reads it.
fn failed(failure: ActionFailure) -> anyhow::Error {
    anyhow::Error::msg(failure.message)
}

/// This computer's device, whose migrations each command steps, through its daemon's native
/// endpoint, the service started when none runs: the person whose account connects is who the
/// daemon decides each step as.
///
/// # Errors
/// This computer is not logged in, or its daemon could not be reached.
fn reach() -> Result<Steps> {
    let login = crate::login::Place::here(crate::identity::paths())?;
    let device = crate::login::served_device_key(login.keys())
        .ok_or_else(|| anyhow!(crate::login::NOT_LOGGED_IN))?;
    let home = crate::login::home();
    // A daemon that is there is waited for, not replaced: it may be busy with a change.
    let stream = tilo_api::daemon::this_computer()
        .and_then(|place| place.reach_native(REACHING))
        .map_err(anyhow::Error::msg)?;
    Ok(Steps::new(
        Arc::new(LineClient::of_stream(stream)?),
        // The daemon decides the step as whoever connects; the caller here names nobody.
        Caller::local_person(""),
        &device,
        home.as_ref()
            .map(|home| home.workspace.clone())
            .unwrap_or_default(),
        home.and_then(|home| home.workspace_name),
    ))
}

/// Invoke `action` with `args` on `device` and follow it to its end, saying its progress on
/// stderr unless `json`, with the questions it asks: its answer, or the failure it ran into.
fn step(
    runtime: &tokio::runtime::Runtime,
    device: &Steps,
    action: &str,
    args: Map<String, Value>,
    json: bool,
) -> Result<std::result::Result<Value, ActionFailure>> {
    let mut say = |line: &str| {
        if !json {
            eprintln!("{line}");
        }
    };
    runtime.block_on(device.step(action, args, &mut Answering { say: &mut say }))
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
