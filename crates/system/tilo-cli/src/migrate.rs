//! `tilo migrate`: bring the person's things from this computer and the services they use into
//! their workspace, driven by their agent, with no window.
//!
//! The sources, and what each one reads, are the Tilo OS installer's migration assistant's
//! ([`migration_sources`]): each Thunderbird account's mail, the address books and calendars,
//! and Outline and Linear. Each step is a command of its own, so an agent can show the person what a
//! source holds before anything moves:
//!
//! ```text
//! tilo migrate sources                         what this computer and its connections offer
//! tilo migrate review thunderbird              its accounts, folders, address books, calendars
//! tilo migrate choose thunderbird ada@work.example
//! tilo migrate start thunderbird               bring the chosen things over
//! tilo migrate status / stop
//! ```
//!
//! What comes over is the agent's run in the workspace (see [`workspace`]): one History entry
//! per run, each record arriving live in the person's tab, the run undone as one step. A run
//! keeps a journal here, item by item, so it resumes after a stop and a second run adds
//! nothing. Keys and passwords never pass through the agent: a service's key is typed by the
//! person at a hidden prompt or read from a file they name, and kept in this computer's vault.

mod services;
mod state;
mod thunderbird;
mod workspace;

#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use state::{Journal, Place, Review, RunState};
use workspace::{Run, Workspace};

#[derive(Subcommand)]
pub enum MigrateCmd {
    /// What can come over: Thunderbird on this computer, and Outline and Linear.
    Sources,
    /// Connect Outline or Linear. Its key is typed at a hidden prompt (or read from
    /// --key-file) and kept in this computer's vault; it is never printed.
    Connect {
        /// outline or linear.
        service: String,
        /// The service's address (Outline's site; Linear's API when not its own).
        #[arg(long)]
        url: Option<String>,
        /// A file holding the key, which the person made.
        #[arg(long, value_name = "FILE")]
        key_file: Option<PathBuf>,
    },
    /// List what a source holds, with counts, and keep the review for `choose` and `start`.
    Review {
        /// thunderbird, outline or linear.
        source: String,
        /// The Thunderbird profile, when this computer has several.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Choose what of a reviewed source comes over (or, with --leave-out, what stays).
    Choose {
        /// thunderbird, outline or linear.
        source: String,
        /// Items by the key the review lists: an account's address, a folder, `contacts/…`,
        /// `calendar/…`, an Outline collection, a Linear team.
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
        /// thunderbird, outline or linear.
        source: String,
    },
    /// How each source's migration stands.
    Status {
        /// One source; all of them when not given.
        source: Option<String>,
    },
    /// Stop a running migration after the item it is on; `start` resumes it.
    Stop {
        /// thunderbird, outline or linear.
        source: String,
    },
}

/// Run `cmd`; `json` answers as JSON.
pub fn run(cmd: MigrateCmd, json: bool) -> Result<ExitCode> {
    let place = place()?;
    let home = home()?;
    let answer = match cmd {
        MigrateCmd::Sources => sources(&place, &home),
        MigrateCmd::Connect {
            service,
            url,
            key_file,
        } => services::connect(&service, url.as_deref(), key_file.as_deref())?,
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
            let source = source_name(&source)?;
            let mut review = place
                .review(&source)?
                .ok_or_else(|| not_reviewed(&source))?;
            review.choose(&items, !leave_out, only)?;
            if source != thunderbird::SOURCE {
                services::choose(&place, &review)?;
            }
            place.keep_review(&review)?;
            review_answer(&review, &place.journal(&source)?)
        }
        MigrateCmd::Start { source } => start(&place, &home, &source_name(&source)?, json)?,
        MigrateCmd::Status { source } => status(&place, source.as_deref())?,
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

fn place() -> Result<Place> {
    let workspace = crate::login::load(&crate::login::device_dir()?)?.map_or_else(
        || "this-computer".to_owned(),
        |(_, device)| device.workspace,
    );
    Place::of(&workspace)
}

/// The home whose Thunderbird is read: this account's.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("this account has no home directory")
}

fn source_name(source: &str) -> Result<String> {
    let source = source.trim().to_ascii_lowercase();
    match source.as_str() {
        thunderbird::SOURCE | services::OUTLINE | services::LINEAR => Ok(source),
        other => bail!(
            "{other:?} is not a source; the sources are thunderbird, outline and linear (`tilo migrate sources`)"
        ),
    }
}

fn not_reviewed(source: &str) -> anyhow::Error {
    anyhow!("{source} is not reviewed yet: `tilo migrate review {source}` first")
}

fn review(place: &Place, home: &Path, source: &str, profile: Option<&str>) -> Result<Review> {
    let source = source_name(source)?;
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
    let mut found = Vec::new();
    match migration_sources::thunderbird::discover(home) {
        Ok(profiles) => {
            for profile in profiles {
                found.push(json!({
                    "source": thunderbird::SOURCE,
                    "title": format!("Thunderbird profile {}", profile.label),
                    "accounts": profile.accounts.len(),
                    "address_books": profile.address_books.len(),
                    "calendars": profile.calendars.len(),
                    "next": "tilo migrate review thunderbird",
                }));
            }
        }
        Err(why) => found.push(json!({"source": thunderbird::SOURCE, "unavailable": why})),
    }
    found.extend(services::sources());
    let reviewed = place.sources();
    json!({"sources": found, "reviewed": reviewed})
}

/// What a review answers: its items, what is chosen, and what already came over.
fn review_answer(review: &Review, journal: &Journal) -> Value {
    let chosen = review.chosen();
    json!({
        "source": review.source,
        "title": review.title,
        "items": review.items,
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

fn start(place: &Place, home: &Path, source: &str, json: bool) -> Result<Value> {
    let review = place.review(source)?.ok_or_else(|| not_reviewed(source))?;
    if review.chosen().is_empty() {
        bail!(
            "nothing of {source} is chosen: `tilo migrate choose {source} <key>…` with keys `tilo migrate review {source}` lists"
        );
    }
    let mut journal = place.journal(source)?;
    if journal.state == RunState::Running && alive(journal.pid) {
        bail!(
            "{source} is already being brought over; `tilo migrate status {source}` says how far"
        );
    }
    let _ = place.stop_asked(source, true);
    let title = if source == thunderbird::SOURCE {
        thunderbird::summary(&review)
    } else {
        services::summary(&review)
    };
    let run = Run {
        id: format!(
            "migrate-{source}-{}",
            hex::encode(rand::random::<[u8; 6]>())
        ),
        title,
    };
    journal.state = RunState::Running;
    journal.pid = Some(std::process::id());
    journal.run = Some(run.title.clone());
    journal.run_id = Some(run.id.clone());
    journal.brought.clear();
    journal.already_there = 0;
    journal.failure = None;
    place.keep_journal(source, &journal)?;
    let mut held = match workspace::Held::connect() {
        Ok(held) => held,
        Err(error) => {
            journal.state = RunState::Failed;
            journal.pid = None;
            journal.failure = Some(format!("{error:#}"));
            place.keep_journal(source, &journal)?;
            return Err(error);
        }
    };
    let mut progress = |line: &str| {
        if !json {
            eprintln!("{line}");
        }
    };
    let result = {
        let mut lander = Lander {
            workspace: &mut held,
            opened: std::collections::BTreeSet::new(),
            landing_in: None,
            run: &run,
            journal: &mut journal,
            place,
            source,
            progress: &mut progress,
        };
        if source == thunderbird::SOURCE {
            thunderbird::land(home, &review, &mut lander)
        } else {
            services::land(&review, &mut lander)
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
    Ok(status_of(source, &review, &journal))
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
        all.push(status_of(&source, &review, &journal));
    }
    Ok(match source {
        Some(_) => all.into_iter().next().unwrap_or(Value::Null),
        None => json!({"migrations": all}),
    })
}

fn status_of(source: &str, review: &Review, journal: &Journal) -> Value {
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
        "in_workspace": journal.landed.len(),
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
    run: &'a Run,
    journal: &'a mut Journal,
    place: &'a Place,
    source: &'a str,
    progress: &'a mut dyn FnMut(&str),
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
                .act(self.run, &format!("{app}#open"), json!({}))?;
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
        land: impl FnOnce(&mut dyn Workspace, &Run) -> Result<Landed>,
    ) -> Result<()> {
        if self.has(key) {
            self.journal.already_there = self.journal.already_there.saturating_add(1);
            return Ok(());
        }
        self.stop_asked()?;
        self.started()?;
        match land(&mut *self.workspace, self.run)? {
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
        land: impl FnOnce(&mut dyn Workspace, &Run, &[&T]) -> Result<()>,
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
        land(&mut *self.workspace, self.run, &missing)?;
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
        self.journal.landed.extend(keys);
        let line = (count > 0).then(|| {
            let total = self.journal.brought.entry(kind.to_owned()).or_default();
            *total = total.saturating_add(count);
            format!("{} {kind} brought over", *total)
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
    pub fn workspace(&mut self) -> Result<(&mut dyn Workspace, &Run)> {
        self.started()?;
        Ok((&mut *self.workspace, self.run))
    }

    /// Record that `count` more of `kind` came over, as a source with its own journal says.
    ///
    /// # Errors
    /// The journal could not be kept.
    pub fn count(&mut self, kind: &str, count: usize, already_there: usize) -> Result<()> {
        self.journal.already_there = self.journal.already_there.saturating_add(already_there);
        self.landed(std::iter::empty(), kind, count)
    }

    /// Whether the run was asked to stop.
    #[must_use]
    pub fn stopping(&self) -> bool {
        self.place.stop_asked(self.source, false)
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
            if let Some(why) = source.get("unavailable").and_then(Value::as_str) {
                let _ = writeln!(out, "{name}: {why}");
            } else {
                let _ = writeln!(
                    out,
                    "{name}: {}{}",
                    source["title"].as_str().unwrap_or_default(),
                    source
                        .get("next")
                        .and_then(Value::as_str)
                        .map(|next| format!("  ({next})"))
                        .unwrap_or_default()
                );
            }
        }
        return out;
    }
    if let Some(items) = answer.get("items").and_then(Value::as_array) {
        let _ = writeln!(out, "{}", answer["title"].as_str().unwrap_or_default());
        for item in items {
            let indent = if item.get("within").is_some() {
                "    "
            } else {
                "  "
            };
            let _ = writeln!(
                out,
                "{indent}[{}] {}  {} {}",
                if item["chosen"] == true { "x" } else { " " },
                item["key"].as_str().unwrap_or_default(),
                item["count"],
                item["counts"].as_str().unwrap_or_default(),
            );
        }
        if let Some(next) = answer.get("next").and_then(Value::as_str) {
            let _ = writeln!(out, "next: {next}");
        }
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
                .map(|(kind, count)| format!("{count} {kind}"))
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
            "the key is typed by the person, never passed by an agent: ask them to run this in their terminal, or to save the key in a file and pass --key-file"
        );
    };
    read_hidden(tty, prompt)
}

fn read_hidden(tty: &mut std::fs::File, prompt: &str) -> Result<zeroize::Zeroizing<String>> {
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
