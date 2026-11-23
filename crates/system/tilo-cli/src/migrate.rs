//! `tilo migrate`: bring the person's things from this computer and the services they use into
//! their workspace, driven by their agent, with no window.
//!
//! The sources are the Tilo OS installer's, read by the same code ([`migration_sources`], the
//! cloud providers Files mounts, the browser readers), listed in one table
//! ([`registry::SOURCES`]): folders on this computer, its browsers and Thunderbird, a mail account,
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

mod browsers;
mod clouds;
mod files;
mod registry;
mod services;
mod state;
mod thunderbird;
mod workspace;

#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod services_tests;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use registry::{Engine, Source};
use state::{Journal, Place, Review, RunState};
use workspace::{Invocation, Workspace};

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
    let answer = match cmd {
        MigrateCmd::Sources => sources(&place, &home),
        MigrateCmd::Connect {
            service,
            url,
            login,
            secret_file,
        } => connect(
            &place,
            registry::named(&service)?,
            url.as_deref(),
            login.as_deref(),
            secret_file.as_deref(),
        )?,
        MigrateCmd::Review { source, profile } => {
            let review = review(&place, &home, &source, profile.as_deref())?;
            place.keep_review(&review)?;
            review_answer(&review, &place.journal(&review.source)?)
        }
        MigrateCmd::Choose {
            source,
            items,
            leave_out,
            only,
        } => {
            let named = registry::named(&source)?;
            let source = named.key.to_owned();
            let mut items = items;
            let mut review = match named.engine {
                // A folder chosen by its path is reviewed with it.
                Engine::Folders => {
                    let mut review = files::review(&home, place.review(&source)?.as_ref());
                    files::add_paths(&home, &mut review, &mut items)?;
                    review
                }
                _ => reviewed_for_start(&place, &home, &source)?,
            };
            review.choose(&items, !leave_out, only)?;
            if named.engine == Engine::Service {
                services::choose(&place, &review)?;
            }
            place.keep_review(&review)?;
            review_answer(&review, &place.journal(&source)?)
        }
        MigrateCmd::Start { source, yes } => start(
            &place,
            &home,
            &source_name(&source)?,
            Asking { json, yes },
            target,
        )?,
        MigrateCmd::Status { source } => status(&place, source.as_deref())?,
        MigrateCmd::Undo { run } => undo(&place, &run, target)?,
        MigrateCmd::Stop { source } => {
            let source = source_name(&source)?;
            let journal = place.journal(&source)?;
            if journal.state != RunState::Running || !alive(journal.pid) {
                json!({"source": source, "said": format!("no {source} migration is running")})
            } else {
                place.ask_stop(&source)?;
                json!({"source": source, "said": format!(
                    "the {source} migration stops after the item it is on; `tilo migrate start {source}` resumes it"
                )})
            }
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&answer)?);
    } else {
        print!("{}", sentences(&answer));
    }
    let failed = answer.get("state").and_then(Value::as_str) == Some("failed");
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The home whose Thunderbird is read: this account's.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("this account has no home directory")
}

fn source_name(source: &str) -> Result<String> {
    Ok(registry::named(source)?.key.to_owned())
}

/// `tilo migrate connect`: sign in to a source that needs it.
fn connect(
    place: &Place,
    source: &'static Source,
    url: Option<&str>,
    login: Option<&str>,
    secret_file: Option<&Path>,
) -> Result<Value> {
    match source.engine {
        Engine::Service => services::connect(place, source.key, url, login, secret_file),
        Engine::Cloud => clouds::connect(place, source, url, login, secret_file),
        Engine::Folders | Engine::Thunderbird | Engine::Browsers => Ok(json!({
            "source": source.key,
            "said": format!(
                "{} is on this computer and needs no connecting; next: tilo migrate review {}",
                source.title, source.key
            ),
        })),
    }
}

fn not_reviewed(source: &str) -> anyhow::Error {
    anyhow!("{source} hasn't been reviewed yet. Run `tilo migrate review {source}` first.")
}

/// The review `start` and `choose` work from: the one the person made, or
/// one made now with every item chosen, so one command brings a source over
/// without a listing step first.
fn reviewed_for_start(place: &Place, home: &Path, source: &str) -> Result<Review> {
    if let Some(review) = place.review(source)? {
        return Ok(review);
    }
    let mut review = review(place, home, source, None)?;
    // A review that suggests some of what it lists (home's familiar folders) brings those;
    // otherwise everything comes.
    let suggested = !review.chosen().is_empty();
    if !suggested {
        let every: Vec<String> = review
            .items
            .iter()
            .filter(|item| item.within.is_none())
            .map(|item| item.key.clone())
            .collect();
        review.choose(&every, true, false)?;
    }
    if registry::named(source)?.engine == Engine::Service {
        services::choose(place, &review)?;
    }
    place.keep_review(&review)?;
    let what = if suggested {
        "what it suggests"
    } else {
        "everything it holds"
    };
    eprintln!(
        "{source} wasn't reviewed yet, so {what} is coming over. `tilo migrate review {source}` \
         lists it item by item, and `tilo migrate choose` changes what comes."
    );
    Ok(review)
}

fn review(place: &Place, home: &Path, source: &str, profile: Option<&str>) -> Result<Review> {
    let named = registry::named(source)?;
    let source = named.key.to_owned();
    match named.engine {
        Engine::Folders => return Ok(files::review(home, place.review(&source)?.as_ref())),
        Engine::Cloud => return clouds::review(place, named),
        Engine::Browsers => {
            let mut review = browsers::review(home);
            // Reviewing again keeps what the person chose.
            if let Some(kept) = place.review(&source)? {
                for item in &mut review.items {
                    item.chosen = kept
                        .items
                        .iter()
                        .any(|old| old.key == item.key && old.chosen);
                }
            }
            return Ok(review);
        }
        Engine::Thunderbird | Engine::Service => {}
    }
    if source == thunderbird::SOURCE {
        let mut review = thunderbird::review(home, profile)?;
        // Reviewing again keeps what the person chose.
        if let Some(kept) = place.review(&source)? {
            for item in &mut review.items {
                item.chosen = kept
                    .items
                    .iter()
                    .any(|old| old.key == item.key && old.chosen);
            }
        }
        return Ok(review);
    }
    services::review(place, &source)
}

fn sources(place: &Place, home: &Path) -> Value {
    let reviewed = place.sources();
    let connected = services::sources();
    let mut found = Vec::new();
    for source in registry::SOURCES {
        let mut row = serde_json::Map::new();
        row.insert("source".to_owned(), json!(source.key));
        row.insert("title".to_owned(), json!(source.title));
        match source.engine {
            Engine::Thunderbird => match migration_sources::thunderbird::discover(home) {
                Ok(profiles) if !profiles.is_empty() => {
                    let accounts: usize =
                        profiles.iter().map(|profile| profile.accounts.len()).sum();
                    row.insert(
                        "here".to_owned(),
                        json!(format!(
                            "{} profile(s), {accounts} account(s)",
                            profiles.len()
                        )),
                    );
                    row.insert("next".to_owned(), json!("tilo migrate review thunderbird"));
                }
                Ok(_) => {
                    row.insert(
                        "unavailable".to_owned(),
                        json!("no Thunderbird profile on this computer"),
                    );
                }
                Err(why) => {
                    row.insert("unavailable".to_owned(), json!(why));
                }
            },
            Engine::Service => {
                let service = connected.iter().find(|service| {
                    service.get("source").and_then(Value::as_str) == Some(source.key)
                });
                match service
                    .and_then(|service| service.get("next"))
                    .and_then(Value::as_str)
                {
                    Some(next) if next.contains("review") => {
                        let here = service
                            .and_then(|service| service.get("title"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        row.insert("here".to_owned(), here);
                        row.insert("next".to_owned(), json!(next));
                    }
                    _ => {
                        row.insert("next".to_owned(), json!(source.connect_command()));
                    }
                }
            }
            Engine::Cloud => {
                let next = if place.path(source.key, "account.json").is_file() {
                    json!(format!("tilo migrate review {}", source.key))
                } else {
                    json!(source.connect_command())
                };
                row.insert("next".to_owned(), next);
            }
            Engine::Folders | Engine::Browsers => {
                row.insert(
                    "next".to_owned(),
                    json!(format!("tilo migrate review {}", source.key)),
                );
            }
        }
        found.push(Value::Object(row));
    }
    json!({"sources": found, "reviewed": reviewed})
}

/// What a review answers: its items, what is chosen, and what already came over.
fn review_answer(review: &Review, journal: &Journal) -> Value {
    let chosen = review.chosen();
    json!({
        "source": review.source,
        "title": review.title,
        "items": review.items,
        "unreadable": review.unreadable,
        "chosen": chosen.iter().map(|item| item.key.as_str()).collect::<Vec<_>>(),
        "already_here": journal.landed.len(),
        "state": journal.state,
        "next": if chosen.is_empty() {
            format!("tilo migrate choose {} <key>…", review.source)
        } else {
            format!("tilo migrate start {}", review.source)
        },
    })
}

/// How a migration talks to the person at the terminal: whether it answers as JSON, and whether they
/// said yes to every account it connects beforehand.
#[derive(Clone, Copy)]
struct Asking {
    json: bool,
    yes: bool,
}

fn start(
    place: &Place,
    home: &Path,
    source: &str,
    asking: Asking,
    target: &Target,
) -> Result<Value> {
    let json = asking.json;
    let review = reviewed_for_start(place, home, source)?;
    if review.chosen().is_empty() {
        bail!(
            "Nothing of {source} is chosen. `tilo migrate review {source}` lists what it holds, \
             and `tilo migrate choose {source} <key>…` picks what comes over."
        );
    }
    let mut journal = place.journal(source)?;
    if journal.state == RunState::Running && alive(journal.pid) {
        bail!(
            "{source} is already being brought over; `tilo migrate status {source}` says how far"
        );
    }
    let _ = place.stop_asked(source, true);
    let named = registry::named(source)?;
    let title = match named.engine {
        Engine::Thunderbird => thunderbird::summary(&review),
        Engine::Service => services::summary(&review),
        Engine::Folders | Engine::Cloud => files::summary(&review, named.title),
        Engine::Browsers => browsers::summary(&review),
    };
    let run = begin(place, source, &mut journal, title)?;
    // Files are written into the workspace's folder here when it has one; only uploads need the
    // workspace's session.
    let sink = match named.engine {
        Engine::Folders => Some(
            target
                .workspace_id()
                .and_then(|id| files::sink(&id, target.is_hosted(), "")),
        ),
        Engine::Cloud => Some(
            target
                .workspace_id()
                .and_then(|id| files::sink(&id, target.is_hosted(), named.title)),
        ),
        _ => None,
    };
    let sink = match sink.transpose() {
        Ok(sink) => sink,
        Err(error) => return failed(place, source, &mut journal, error),
    };
    let connected = match &sink {
        Some(files::Sink::Folder(_)) => Ok(Box::new(workspace::Offline) as Box<dyn Workspace>),
        _ => target.connect(),
    };
    let mut held = match connected {
        Ok(held) => held,
        Err(error) => return failed(place, source, &mut journal, error),
    };
    let mut progress = |line: &str| {
        if !json {
            eprintln!("{line}");
        }
    };
    let mut confirm = |question: &str| confirmed(question, asking);
    let result = {
        let mut lander = Lander {
            workspace: &mut *held,
            opened: std::collections::BTreeSet::new(),
            landing_in: None,
            invocation: &run,
            journal: &mut journal,
            place,
            source,
            progress: &mut progress,
            confirm: &mut confirm,
        };
        match (named.engine, &sink) {
            (Engine::Thunderbird, _) => thunderbird::land(home, &review, &mut lander),
            (Engine::Service, _) => services::land(&review, &mut lander),
            (Engine::Folders, Some(sink)) => files::land(
                &files::Local {
                    home: home.to_path_buf(),
                },
                &review,
                sink,
                place,
                &mut lander,
            ),
            (Engine::Cloud, Some(sink)) => clouds::land(place, named, &review, sink, &mut lander),
            (Engine::Browsers, _) => browsers::land(home, &review, &mut lander),
            (Engine::Folders | Engine::Cloud, None) => Err(anyhow!("files have nowhere to go")),
        }
    };
    journal.pid = None;
    journal.state = match &result {
        Ok(()) => RunState::Complete,
        Err(error) if error.is::<Stopped>() => RunState::Stopped,
        Err(_) => RunState::Failed,
    };
    journal.failure = result
        .as_ref()
        .err()
        .filter(|error| !error.is::<Stopped>())
        .map(|error| format!("{error:#}"));
    place.keep_journal(source, &journal)?;
    let landed = in_workspace(place, source, &journal)?;
    Ok(status_of(source, &review, &journal, landed))
}

/// The invocation `title` names, starting now: the journal says it is going and brings nothing yet.
fn begin(place: &Place, source: &str, journal: &mut Journal, title: String) -> Result<Invocation> {
    let invocation = Invocation {
        id: format!(
            "migrate-{source}-{}",
            hex::encode(rand::random::<[u8; 6]>())
        ),
        title,
    };
    journal.state = RunState::Running;
    journal.pid = Some(std::process::id());
    journal.run = Some(invocation.title.clone());
    journal.run_id = Some(invocation.id.clone());
    journal.brought.clear();
    journal.already_there = 0;
    journal.failure = None;
    place.keep_journal(source, journal)?;
    Ok(invocation)
}

/// Give up before anything is brought: the journal says it failed and why, and the error
/// goes to the caller.
fn failed<T>(
    place: &Place,
    source: &str,
    journal: &mut Journal,
    error: anyhow::Error,
) -> Result<T> {
    journal.state = RunState::Failed;
    journal.pid = None;
    journal.failure = Some(format!("{error:#}"));
    place.keep_journal(source, journal)?;
    Err(error)
}

/// How many of a source's items the workspace has: Thunderbird's journal counts them, a service
/// source's own journal does.
fn in_workspace(place: &Place, source: &str, journal: &Journal) -> Result<usize> {
    match registry::named(source)?.engine {
        Engine::Service => services::landed(place, source),
        // Files are counted as the last run found them: brought, or there already.
        Engine::Folders | Engine::Cloud => Ok(journal
            .brought
            .values()
            .sum::<usize>()
            .saturating_add(journal.already_there)),
        Engine::Thunderbird | Engine::Browsers => Ok(journal.landed.len()),
    }
}

/// The records a source's runs change, where their History entries are taken back.
fn records_of(source: &str) -> &'static [&'static str] {
    registry::named(source).map_or(&[], |source| source.resources)
}

/// `tilo migrate undo`: take the run `named` back in every record it changed.
fn undo(place: &Place, named: &str, target: &Target) -> Result<Value> {
    let (source, run) = run_named(place, named)?;
    let mut journal = place.journal(&source)?;
    let mut held = target.connect()?;
    let mut taken = Vec::new();
    let mut named = false;
    // Files a run wrote into the workspace's folder here go as they came: from the folder.
    if let Some(removed) = files::undo_written(place, &source, &run)? {
        named = true;
        taken.push(json!({"folder": removed}));
    }
    for record in records_of(&source) {
        match undo_in(&mut *held, record, &run)? {
            RunInHistory::InEffect(_) => {
                named = true;
                taken.push(json!({"record": record}));
            }
            RunInHistory::Undone => named = true,
            RunInHistory::Absent => {}
        }
    }
    // A run History does not know is not taken back, whatever this computer kept of it: what it
    // brought stays in the workspace, so this computer goes on counting it.
    if !named {
        bail!(
            "History has no change of run {run} in {}: nothing was taken back",
            records_of(&source).join(", ")
        );
    }
    let forgotten = journal.runs.remove(&run).unwrap_or_default();
    for key in &forgotten {
        journal.landed.remove(key);
    }
    if registry::named(&source)?.engine == Engine::Service {
        services::forget(place, &source, &run, &forgotten)?;
    }
    if journal.run_id.as_deref() == Some(run.as_str()) {
        journal.state = RunState::Reviewed;
        journal.brought.clear();
        journal.already_there = 0;
    }
    place.keep_journal(&source, &journal)?;
    let said = if taken.is_empty() {
        format!("run {run} has nothing left to take back in the workspace")
    } else {
        format!(
            "run {run} is taken back: what it brought left the workspace; `tilo migrate start {source}` brings it again"
        )
    };
    Ok(json!({"source": source, "run_id": run, "undone": taken, "said": said}))
}

/// The source and run id `named` names: a run id a journal knows, or a source's last run.
fn run_named(place: &Place, named: &str) -> Result<(String, String)> {
    if let Ok(source) = source_name(named) {
        let journal = place.journal(&source)?;
        let run = journal
            .run_id
            .ok_or_else(|| anyhow!("{source} has no run to take back"))?;
        return Ok((source, run));
    }
    for source in place.sources() {
        let journal = place.journal(&source)?;
        if journal.run_id.as_deref() == Some(named) || journal.runs.contains_key(named) {
            return Ok((source, named.to_owned()));
        }
    }
    bail!("no run {named:?}: `tilo migrate status` names each source's last run")
}

/// Take the run `run` back in `record`: its own `undo` of the step the run's changes are.
/// What `record`'s History held of the run before.
fn undo_in(workspace: &mut dyn Workspace, record: &str, run: &str) -> Result<RunInHistory> {
    let Ok(answer) = workspace.ask(&format!("{record}#history"), json!({"last": 200})) else {
        // An app the run never opened keeps no history of it.
        return Ok(RunInHistory::Absent);
    };
    let found = run_in_history(answer.result(), run);
    if let RunInHistory::InEffect(change) = &found {
        workspace
            .ask(&format!("{record}#undo"), json!({"step": change}))
            .with_context(|| format!("{record} did not take the run back"))?;
    }
    Ok(found)
}

/// What a record's History holds of one run.
#[derive(Debug, PartialEq, Eq)]
enum RunInHistory {
    /// No change of it is there.
    Absent,
    /// Every change of it is undone already.
    Undone,
    /// It is in effect: its newest change still in effect, which names its step to `undo`.
    InEffect(String),
}

/// What a record's `history` answer holds of the run `run`: History keeps with each change the id
/// of the run its agent said it is a step of (`actor.declared`).
fn run_in_history(history: Option<&Value>, run: &str) -> RunInHistory {
    let entries = history
        .and_then(|value| {
            ["versions", "history"]
                .iter()
                .find_map(|key| value.get(*key).and_then(Value::as_array).cloned())
        })
        .unwrap_or_default();
    let mut of_run = entries
        .iter()
        .rev()
        .filter(|entry| entry.pointer("/actor/declared").and_then(Value::as_str) == Some(run))
        .peekable();
    if of_run.peek().is_none() {
        return RunInHistory::Absent;
    }
    of_run
        .find(|entry| entry.get("undone").and_then(Value::as_bool) != Some(true))
        .and_then(|entry| entry.get("change").and_then(Value::as_str))
        .map_or(RunInHistory::Undone, |change| {
            RunInHistory::InEffect(change.to_owned())
        })
}

fn status(place: &Place, source: Option<&str>) -> Result<Value> {
    let sources = match source {
        Some(source) => vec![source_name(source)?],
        None => place.sources(),
    };
    let mut all = Vec::new();
    for source in sources {
        let review = place
            .review(&source)?
            .ok_or_else(|| not_reviewed(&source))?;
        let mut journal = place.journal(&source)?;
        if journal.state == RunState::Running && !alive(journal.pid) {
            // Its process ended without saying how: what it brought is kept, start resumes.
            journal.state = RunState::Stopped;
        }
        let landed = in_workspace(place, &source, &journal)?;
        all.push(status_of(&source, &review, &journal, landed));
    }
    Ok(match source {
        Some(_) => all.into_iter().next().unwrap_or(Value::Null),
        None => json!({"migrations": all}),
    })
}

/// How `source`'s migration stands; `landed` is how many of its items the workspace has.
fn status_of(source: &str, review: &Review, journal: &Journal, landed: usize) -> Value {
    let next = match journal.state {
        RunState::Reviewed | RunState::Stopped | RunState::Failed => {
            Some(format!("tilo migrate start {source}"))
        }
        RunState::Running => Some(format!("tilo migrate stop {source}")),
        RunState::Complete => None,
    };
    json!({
        "source": source,
        "state": journal.state,
        "run": journal.run,
        "run_id": journal.run_id,
        "chosen": review.chosen().iter().map(|item| item.key.as_str()).collect::<Vec<_>>(),
        "brought": journal.brought,
        "already_there": journal.already_there,
        "in_workspace": landed,
        "failure": journal.failure,
        "next": next,
    })
}

/// Whether the process `pid` still runs.
fn alive(pid: Option<u32>) -> bool {
    pid.and_then(|pid| i32::try_from(pid).ok())
        .is_some_and(|pid| {
            rustix::process::test_kill_process(
                rustix::process::Pid::from_raw(pid).unwrap_or(rustix::process::Pid::INIT),
            )
            .is_ok()
        })
}

/// Whether the person says yes to `question`, asked at this terminal: yes when they said so
/// beforehand (`--yes`). Nobody at a terminal to ask is no answer, and the step is skipped.
///
/// # Errors
/// Nobody can be asked here and nobody said yes beforehand.
fn confirmed(question: &str, asking: Asking) -> Result<bool> {
    use std::io::{BufRead as _, Write as _};
    if asking.yes {
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
    if asking.json && !std::io::stdin().is_terminal() {
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

/// A run the person or their agent stopped.
#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("stopped")
    }
}

impl std::error::Error for Stopped {}

/// How one item went.
pub enum Landed {
    /// The workspace has it now.
    One,
    /// The workspace had it already: another run, or another computer, brought it.
    Already,
}

/// A run landing items in the workspace, each recorded in the journal the moment it is there.
pub struct Lander<'a> {
    workspace: &'a mut dyn Workspace,
    /// The apps this run started.
    opened: std::collections::BTreeSet<String>,
    /// The app the next items land in, started before the first of them does.
    landing_in: Option<String>,
    invocation: &'a Invocation,
    journal: &'a mut Journal,
    place: &'a Place,
    source: &'a str,
    progress: &'a mut dyn FnMut(&str),
    /// Asks the person, at their terminal, whether a step that is theirs alone goes ahead.
    confirm: &'a mut dyn FnMut(&str) -> Result<bool>,
}

impl Lander<'_> {
    /// The next items land in `app`: it is started, once a run, before the first of them that
    /// the workspace lacks; a run that brings nothing starts nothing.
    pub fn open(&mut self, app: &str) {
        self.landing_in = Some(app.to_owned());
    }

    /// Start the app the next item lands in, unless this run did.
    fn started(&mut self) -> Result<()> {
        if let Some(app) = self.landing_in.clone()
            && self.opened.insert(app.clone())
        {
            self.workspace
                .act(self.invocation, &format!("{app}#open"), json!({}))?;
        }
        Ok(())
    }

    fn stop_asked(&self) -> Result<()> {
        if self.place.stop_asked(self.source, true) {
            return Err(Stopped.into());
        }
        Ok(())
    }

    /// Whether the workspace already has `key` from this source.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.journal.landed.contains(key)
    }

    /// Land one item, `key`, of `kind`, unless the workspace has it.
    ///
    /// # Errors
    /// The run was stopped, the workspace refused, or the journal could not be kept.
    pub fn once(
        &mut self,
        key: &str,
        kind: &str,
        land: impl FnOnce(&mut dyn Workspace, &Invocation) -> Result<Landed>,
    ) -> Result<()> {
        if self.has(key) {
            self.journal.already_there = self.journal.already_there.saturating_add(1);
            return Ok(());
        }
        self.stop_asked()?;
        self.started()?;
        match land(&mut *self.workspace, self.invocation)? {
            Landed::One => self.landed([key.to_owned()], kind, 1),
            Landed::Already => {
                self.journal.already_there = self.journal.already_there.saturating_add(1);
                self.landed([key.to_owned()], kind, 0)
            }
        }
    }

    /// Land those of `items` the workspace lacks, together, as `kind`.
    ///
    /// # Errors
    /// The run was stopped, the workspace refused, or the journal could not be kept.
    pub fn batch<T>(
        &mut self,
        items: &[T],
        key: impl Fn(&T) -> String,
        kind: &str,
        land: impl FnOnce(&mut dyn Workspace, &Invocation, &[&T]) -> Result<()>,
    ) -> Result<()> {
        let mut missing = Vec::new();
        let mut keys = Vec::new();
        for item in items {
            let named = key(item);
            if self.has(&named) {
                self.journal.already_there = self.journal.already_there.saturating_add(1);
            } else {
                missing.push(item);
                keys.push(named);
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        self.stop_asked()?;
        self.started()?;
        land(&mut *self.workspace, self.invocation, &missing)?;
        let count = keys.len();
        self.landed(keys, kind, count)
    }

    /// Record `keys` as in the workspace, `count` more of `kind`.
    fn landed(
        &mut self,
        keys: impl IntoIterator<Item = String>,
        kind: &str,
        count: usize,
    ) -> Result<()> {
        let keys: Vec<String> = keys.into_iter().collect();
        self.journal
            .runs
            .entry(self.invocation.id.clone())
            .or_default()
            .extend(keys.iter().cloned());
        self.journal.landed.extend(keys);
        let line = (count > 0).then(|| {
            let total = self.journal.brought.entry(kind.to_owned()).or_default();
            *total = total.saturating_add(count);
            format!("{} brought over", services::counted(*total, kind))
        });
        self.place.keep_journal(self.source, self.journal)?;
        if let Some(line) = line {
            (self.progress)(&line);
        }
        Ok(())
    }

    /// The workspace and run, for a source that keeps its own journal of items.
    ///
    /// # Errors
    /// The app the items land in could not be started.
    pub fn workspace(&mut self) -> Result<(&mut dyn Workspace, &Invocation)> {
        self.started()?;
        Ok((&mut *self.workspace, self.invocation))
    }

    /// Record that `count` more of `kind` came over, as a source with its own journal says.
    ///
    /// # Errors
    /// The journal could not be kept.
    pub fn count(&mut self, kind: &str, count: usize, already_there: usize) -> Result<()> {
        self.journal.already_there = self.journal.already_there.saturating_add(already_there);
        self.landed(std::iter::empty(), kind, count)
    }

    /// Note that this run brought `keys` of a source that keeps its own journal, so taking the
    /// run back forgets them there too.
    ///
    /// # Errors
    /// The journal could not be kept.
    pub fn note(&mut self, keys: impl IntoIterator<Item = String>) -> Result<()> {
        self.journal
            .runs
            .entry(self.invocation.id.clone())
            .or_default()
            .extend(keys);
        self.place.keep_journal(self.source, self.journal)
    }

    /// Whether the run was asked to stop.
    #[must_use]
    pub fn stopping(&self) -> bool {
        self.place.stop_asked(self.source, false)
    }

    /// Tell the person a line of how the run goes.
    pub fn say(&mut self, line: &str) {
        (self.progress)(line);
    }

    /// Whether the person says yes to `question`.
    ///
    /// # Errors
    /// Nobody could be asked.
    pub fn confirm(&mut self, question: &str) -> Result<bool> {
        (self.confirm)(question)
    }

    /// Ask `action` of the person's own `path` on the workspace's server, as the person
    /// ([`Workspace::person`]).
    ///
    /// # Errors
    /// The migration was stopped, or the server refused or could not be reached.
    pub fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        self.stop_asked()?;
        self.workspace.person(path, action, args)
    }

    /// Note that the workspace has `key` now (`count` more of `kind`), or had it already.
    ///
    /// # Errors
    /// The journal could not be kept.
    pub fn noted(&mut self, key: &str, kind: &str, landed: &Landed) -> Result<()> {
        match landed {
            Landed::One => self.landed([key.to_owned()], kind, 1),
            Landed::Already => {
                self.journal.already_there = self.journal.already_there.saturating_add(1);
                self.landed([key.to_owned()], kind, 0)
            }
        }
    }

    /// The source the run brings from.
    #[must_use]
    pub const fn source(&self) -> &str {
        self.source
    }

    /// The run.
    #[must_use]
    pub const fn invocation(&self) -> &Invocation {
        self.invocation
    }
}

/// `answer` as sentences for a person reading the terminal.
fn sentences(answer: &Value) -> String {
    let mut out = String::new();
    if let Some(said) = answer.get("said").and_then(Value::as_str) {
        out.push_str(said);
        out.push('\n');
        return out;
    }
    if let Some(found) = answer.get("sources").and_then(Value::as_array) {
        for source in found {
            let name = source["source"].as_str().unwrap_or_default();
            let title = source["title"].as_str().unwrap_or_default();
            if let Some(why) = source.get("unavailable").and_then(Value::as_str) {
                let _ = writeln!(out, "{name:12} {title}: {why}");
            } else {
                let here = source
                    .get("here")
                    .and_then(Value::as_str)
                    .map(|here| format!(" [{here}]"))
                    .unwrap_or_default();
                let _ = writeln!(out, "{name:12} {title}{here}");
                if let Some(next) = source.get("next").and_then(Value::as_str) {
                    let _ = writeln!(out, "{:12}   next: {next}", "");
                }
            }
        }
        return out;
    }
    if let Some(items) = answer.get("items").and_then(Value::as_array) {
        review_sentences(answer, items, &mut out);
        return out;
    }
    let one = |status: &Value, out: &mut String| {
        let _ = write!(
            out,
            "{}: {}",
            status["source"].as_str().unwrap_or_default(),
            status["state"].as_str().unwrap_or_default()
        );
        if let Some(brought) = status.get("brought").and_then(Value::as_object)
            && !brought.is_empty()
        {
            let parts: Vec<String> = brought
                .iter()
                .map(|(kind, count)| {
                    let count = count
                        .as_u64()
                        .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
                    services::counted(count, kind)
                })
                .collect();
            let _ = write!(out, ", brought {}", parts.join(", "));
        }
        if let Some(already) = status.get("already_there").and_then(Value::as_u64)
            && already > 0
        {
            let _ = write!(out, ", {already} already there");
        }
        if let Some(failure) = status.get("failure").and_then(Value::as_str) {
            let _ = write!(out, ": {failure}");
        }
        out.push('\n');
        if let Some(next) = status.get("next").and_then(Value::as_str) {
            let _ = writeln!(out, "next: {next}");
        }
    };
    if let Some(all) = answer.get("migrations").and_then(Value::as_array) {
        if all.is_empty() {
            out.push_str("nothing reviewed yet: `tilo migrate sources`\n");
        }
        for status in all {
            one(status, &mut out);
        }
        return out;
    }
    if answer.get("state").is_some() {
        one(answer, &mut out);
        return out;
    }
    out.push_str(&answer.to_string());
    out.push('\n');
    out
}

/// A review's `items`, of `answer`, as lines for a person reading the terminal.
fn review_sentences(answer: &Value, items: &[Value], out: &mut String) {
    let _ = writeln!(out, "{}", answer["title"].as_str().unwrap_or_default());
    for item in items {
        let indent = if item.get("within").is_some() {
            "    "
        } else {
            "  "
        };
        let key = item["key"].as_str().unwrap_or_default();
        let title = item["title"].as_str().unwrap_or_default();
        let _ = write!(
            out,
            "{indent}[{}] {key}",
            if item["chosen"] == true { "x" } else { " " },
        );
        // A title that says more than the key does (what choosing it does) is shown.
        if !title.is_empty() && title != key && !key.ends_with(title) {
            let _ = write!(out, "  {title}");
        }
        let counts = item["counts"].as_str().unwrap_or_default();
        if !counts.is_empty() {
            let count = item["count"]
                .as_u64()
                .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
            let _ = write!(out, "  {}", services::counted(count, counts));
        }
        out.push('\n');
        if let Some(note) = item.get("note").and_then(Value::as_str) {
            let _ = writeln!(out, "{indent}    {note}");
        }
    }
    if let Some(problems) = answer.get("unreadable").and_then(Value::as_array) {
        for problem in problems.iter().filter_map(Value::as_str) {
            let _ = writeln!(out, "  incomplete: {problem}");
        }
    }
    if let Some(next) = answer.get("next").and_then(Value::as_str) {
        let _ = writeln!(out, "next: {next}");
    }
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
