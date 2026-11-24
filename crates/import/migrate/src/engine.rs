//! The one migration engine: every source a person brings things from, reviewed, chosen, brought
//! into the workspace through its apps' ordinary actions, followed while it runs, and taken back.
//!
//! The sources are the ones the readers of this crate open, listed in one table
//! ([`registry::SOURCES`]): folders on this computer, its home's settings and hidden files, the
//! folders Windows keeps, the accounts it is signed in to, its Flatpak apps, its browsers and
//! Thunderbird, a mail account, Google Drive, `OneDrive`, Dropbox, iCloud Drive, Nextcloud and
//! `WebDAV`, and Outline, Linear and Forgejo. An old computer is migrated from by running these on
//! it, Tilo's installer among the programs that do, before anything is installed over it. Each is one [`Source`]: how it is connected, what it holds ([`Review`]), and how what
//! the person chose lands ([`Lander`]). The steps are the same for each, so an agent can show the
//! person what a source holds before anything moves:
//!
//! ```text
//! sources        every source, and how to connect each
//! connect        sign in where a source needs it
//! review         what it holds: accounts, folders, …
//! choose         what comes over
//! start          bring the chosen things over, as one run in the workspace
//! status         how each source's migration stands
//! undo           take a run back
//! ```
//!
//! Files (folders here, cloud drives) are copied into the workspace's folder on this computer
//! when it has one, as fast as the disk or network goes ([`files`]).
//!
//! A run lands in a [`Workspace`] its caller reaches: what comes over is the agent's run in the
//! workspace, one History entry per run, each record arriving live in the person's tab, the run
//! undone as one step. A run keeps a journal, item by item, where its caller keeps the
//! migration ([`state::Place`]), so it resumes after a stop and a second run adds nothing. Whoever
//! runs it says whether a run is going and when one is asked to stop ([`Cx::running`],
//! [`Asking::stopping`]). Keys and passwords never pass through the agent: a
//! service's key is typed by the person ([`Cx::secret`]) and kept in this computer's vault.

pub mod accounts;
pub mod browsers;
pub mod clouds;
pub mod files;
pub mod flatpaks;
pub mod mcp;
pub mod registry;
pub mod services;
pub mod settings;
pub mod state;
pub mod text;
pub mod thunderbird;
pub mod windows;
pub mod writer;

#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod services_tests;
#[cfg(test)]
pub(crate) mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use registry::{Engine, Row};
use state::{Journal, Place, Review, RunState};
use writer::{Answer, Invocation, Workspace};

pub use text::sentences;

/// What a migration runs with: where its journals are, the home its local sources are read from,
/// and the ways its caller reaches the workspace and the person.
pub struct Cx<'a> {
    /// Where the journals of the workspace the migration lands in are kept.
    pub place: &'a Place,
    /// Whether a run of a source is going now: its caller knows the runs it started.
    pub running: &'a dyn Fn(&str) -> bool,
    /// A folder of this computer a run may stage what it fetches in, for a while.
    pub scratch: &'a Path,
    /// The home this computer's folders, browsers and Thunderbird are read from.
    pub home: &'a Path,
    /// The workspace folders this computer keeps, by workspace id: what a home review leaves out,
    /// and where a run's files are copied to.
    pub folders: &'a BTreeMap<String, PathBuf>,
    /// The workspace the migration lands in, reached for the person's own steps (connecting an
    /// account, reading the accounts record).
    pub open: &'a dyn Fn() -> Result<Box<dyn Workspace>>,
    /// A secret the person types, unseen (after the prompt it is given), or reads from the file
    /// they name: never an argument an agent passes.
    pub secret: &'a AsksSecret<'a>,
}

/// What asks for a key or password: after the words it is asked with, or from the file the
/// person named holding it.
pub type AsksSecret<'a> = dyn Fn(&str, Option<&Path>) -> Result<Zeroizing<String>> + 'a;

impl Cx<'_> {
    /// The workspace, reached for one of the person's own steps.
    ///
    /// # Errors
    /// It could not be reached.
    pub fn open(&self) -> Result<Box<dyn Workspace>> {
        (self.open)()
    }

    /// A secret the person types after `prompt`, or reads from `file`.
    ///
    /// # Errors
    /// Nobody can type it here and no file was named.
    pub fn secret(&self, prompt: &str, file: Option<&Path>) -> Result<Zeroizing<String>> {
        (self.secret)(prompt, file)
    }
}

/// How a source is connected: the address, the account, and a file holding its secret.
#[derive(Clone, Copy, Debug, Default)]
pub struct Connecting<'a> {
    pub url: Option<&'a str>,
    pub login: Option<&'a str>,
    pub secret_file: Option<&'a Path>,
}

/// How a source stands on this computer, as `sources` lists it.
#[derive(Debug, Default)]
pub struct Here {
    /// What it holds here, briefly (`1 profile(s), 2 account(s)`).
    pub here: Option<Value>,
    /// The step to take next.
    pub next: Option<String>,
    /// Why it cannot come over from here.
    pub unavailable: Option<String>,
}

/// One kind of source: how it is connected, what it holds, and how what the person chose lands.
/// Every source is one of these and every step of a migration goes through it, so the steps are
/// the same for each.
pub trait Source: Sync {
    /// How `row` stands on this computer.
    fn here(&self, cx: &Cx<'_>, row: &'static Row) -> Here;

    /// Connect `row` as `how` says: sign in, and keep its secret in this computer's vault.
    ///
    /// # Errors
    /// The sign-in failed; the error says what to do.
    fn connect(&self, cx: &Cx<'_>, row: &'static Row, how: Connecting<'_>) -> Result<Value> {
        let _ = (cx, how);
        Ok(json!({
            "source": row.key,
            "said": format!(
                "{} is on this computer and needs no connecting; next: tilo migrate review {}",
                row.title, row.key
            ),
        }))
    }

    /// What `row` holds now, keeping what the person chose before.
    ///
    /// # Errors
    /// It could not be read; the error says why.
    fn review(&self, cx: &Cx<'_>, row: &'static Row, profile: Option<&str>) -> Result<Review>;

    /// The person changed what of `review` comes over.
    ///
    /// # Errors
    /// The source's own journal could not be kept.
    fn chose(&self, cx: &Cx<'_>, review: &Review) -> Result<()> {
        let _ = (cx, review);
        Ok(())
    }

    /// What a run of the chosen items of `review` brings, for its History entry.
    fn summary(&self, row: &'static Row, review: &Review) -> String;

    /// The folder of the workspace's files a run's files go under, for a source of files.
    fn files_under(&self, row: &'static Row) -> Option<&'static str> {
        let _ = row;
        None
    }

    /// Bring the chosen items of `review` over through `lander`; files go to `sink`.
    ///
    /// # Errors
    /// The run was stopped, the source could not be read, or the workspace refused.
    fn land(
        &self,
        cx: &Cx<'_>,
        row: &'static Row,
        review: &Review,
        sink: Option<&files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()>;

    /// How many of `row`'s items the workspace has.
    ///
    /// # Errors
    /// The source's own journal could not be read.
    fn landed(&self, cx: &Cx<'_>, row: &'static Row, journal: &Journal) -> Result<usize> {
        let _ = (cx, row);
        Ok(journal.landed.len())
    }

    /// Forget the items (`keys`) the run `run` brought, which the workspace took back.
    ///
    /// # Errors
    /// The source's own journal could not be kept.
    fn forget(
        &self,
        cx: &Cx<'_>,
        row: &'static Row,
        run: &str,
        keys: &std::collections::BTreeSet<String>,
    ) -> Result<()> {
        let _ = (cx, row, run, keys);
        Ok(())
    }
}

/// The source that reads `engine`.
#[must_use]
pub fn source_of(engine: Engine) -> &'static dyn Source {
    match engine {
        Engine::Folders => &files::Folders,
        Engine::Cloud => &clouds::Clouds,
        Engine::Service => &services::Services,
        Engine::Thunderbird => &thunderbird::Thunderbird,
        Engine::Browsers => &browsers::Browsers,
        Engine::Accounts => &accounts::SignedIn,
        Engine::Settings => &settings::Settings,
        Engine::Windows => &windows::Windows,
        Engine::Flatpaks => &flatpaks::Flatpaks,
    }
}

/// The source `row` is.
fn of(row: &'static Row) -> &'static dyn Source {
    source_of(row.engine)
}

/// How a run talks to the person: a line of how it goes, and a question only they answer.
pub struct Asking<'a> {
    pub progress: &'a mut dyn FnMut(&str),
    /// Whether the run was asked to stop: it stops after the item it is on.
    pub stopping: &'a dyn Fn() -> bool,
    /// Whether the person says yes to the question; `Err` when nobody can be asked.
    pub confirm: &'a mut dyn FnMut(&str) -> Result<bool>,
    /// Where the files of the workspace the run lands in go: the folder this computer keeps of
    /// it, or uploads when it has none here but is reached through Tilo's API.
    pub workspace: &'a str,
    pub hosted: bool,
}

/// `sources`: every source, how it stands here and what to do next.
#[must_use]
pub fn sources(cx: &Cx<'_>) -> Value {
    let reviewed = cx.place.sources();
    let mut found = Vec::new();
    for row in registry::SOURCES {
        let here = of(row).here(cx, row);
        let mut answer = serde_json::Map::new();
        answer.insert("source".to_owned(), json!(row.key));
        answer.insert("title".to_owned(), json!(row.title));
        if let Some(why) = here.unavailable {
            answer.insert("unavailable".to_owned(), json!(why));
        } else {
            if let Some(here) = here.here {
                answer.insert("here".to_owned(), here);
            }
            answer.insert("next".to_owned(), json!(here.next));
        }
        found.push(Value::Object(answer));
    }
    json!({"sources": found, "reviewed": reviewed})
}

/// `connect`: sign in to `source`.
///
/// # Errors
/// No such source, or the sign-in failed.
pub fn connect(cx: &Cx<'_>, source: &str, how: Connecting<'_>) -> Result<Value> {
    let row = registry::named(source)?;
    of(row).connect(cx, row, how)
}

/// `review`: what `source` holds, kept for `choose` and `start`.
///
/// # Errors
/// No such source, or it could not be read.
pub fn review(cx: &Cx<'_>, source: &str, profile: Option<&str>) -> Result<Value> {
    let review = reviewed(cx, source, profile)?;
    cx.place.keep_review(&review)?;
    Ok(review_answer(&review, &cx.place.journal(&review.source)?))
}

fn reviewed(cx: &Cx<'_>, source: &str, profile: Option<&str>) -> Result<Review> {
    let row = registry::named(source)?;
    of(row).review(cx, row, profile)
}

/// `choose`: what of `source` comes over: `items` by their keys, or, with `leave_out`, what stays;
/// `only` leaves everything else out.
///
/// # Errors
/// No such source, an item it does not hold, or the review could not be kept.
pub fn choose(
    cx: &Cx<'_>,
    source: &str,
    items: &[String],
    leave_out: bool,
    only: bool,
) -> Result<Value> {
    let row = registry::named(source)?;
    let mut items = items.to_vec();
    let mut review = if row.engine == Engine::Folders {
        // A folder chosen by its path is reviewed with it.
        let mut review = of(row).review(cx, row, None)?;
        files::add_paths(cx.home, &mut review, &mut items)?;
        review
    } else {
        reviewed_for_start(cx, row.key)?
    };
    review.choose(&items, !leave_out, only)?;
    of(row).chose(cx, &review)?;
    cx.place.keep_review(&review)?;
    Ok(review_answer(&review, &cx.place.journal(row.key)?))
}

/// Why `source` cannot be chosen from or started yet.
pub(crate) fn not_reviewed(source: &str) -> anyhow::Error {
    anyhow!("{source} hasn't been reviewed yet. Run `tilo migrate review {source}` first.")
}

/// The review `start` and `choose` work from: the one the person made, or one made now with every
/// item chosen, so one command brings a source over without a listing step first.
pub(crate) fn reviewed_for_start(cx: &Cx<'_>, source: &str) -> Result<Review> {
    if let Some(review) = cx.place.review(source)? {
        return Ok(review);
    }
    let row = registry::named(source)?;
    let mut review = of(row).review(cx, row, None)?;
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
    of(row).chose(cx, &review)?;
    cx.place.keep_review(&review)?;
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

/// Keep what the person chose in `kept` in the fresh review `review`.
pub(crate) fn keep_chosen(review: &mut Review, kept: Option<&Review>) {
    if let Some(kept) = kept {
        for item in &mut review.items {
            item.chosen = kept
                .items
                .iter()
                .any(|old| old.key == item.key && old.chosen);
        }
    }
}

/// What a review answers: its items, what is chosen, and what already came over.
#[must_use]
pub fn review_answer(review: &Review, journal: &Journal) -> Value {
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

/// `start`: bring the chosen things of `source` over, as one run into the workspace `connect`
/// reaches; resumes a stopped run.
///
/// # Errors
/// Nothing is chosen, a run is going already, or the run failed before it began.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the run is the one user of what it is asked with, which lends its callbacks to it"
)]
pub fn start(
    cx: &Cx<'_>,
    source: &str,
    asking: Asking<'_>,
    connect: &dyn Fn() -> Result<Box<dyn Workspace>>,
) -> Result<Value> {
    let row = registry::named(source)?;
    let source = row.key;
    let review = reviewed_for_start(cx, source)?;
    if review.chosen().is_empty() {
        bail!(
            "Nothing of {source} is chosen. `tilo migrate review {source}` lists what it holds, \
             and `tilo migrate choose {source} <key>…` picks what comes over."
        );
    }
    let place = cx.place;
    let mut journal = place.journal(source)?;
    if journal.state == RunState::Running && (cx.running)(source) {
        bail!(
            "{source} is already being brought over; `tilo migrate status {source}` says how far"
        );
    }
    let engine = of(row);
    let run = begin(place, source, &mut journal, engine.summary(row, &review))?;
    // Files are written into the workspace's folder here when it has one; only uploads need the
    // workspace's session.
    let sink = engine
        .files_under(row)
        .map(|under| files::sink(cx.folders, asking.workspace, asking.hosted, under))
        .transpose();
    let sink = match sink {
        Ok(sink) => sink,
        Err(error) => return failed(place, source, &mut journal, error),
    };
    let connected = match &sink {
        Some(files::Sink::Folder(_)) => Ok(Box::new(writer::Offline) as Box<dyn Workspace>),
        _ => connect(),
    };
    let mut held = match connected {
        Ok(held) => held,
        Err(error) => return failed(place, source, &mut journal, error),
    };
    let result = {
        let mut lander = Lander {
            workspace: &mut *held,
            opened: std::collections::BTreeSet::new(),
            landing_in: None,
            invocation: &run,
            journal: &mut journal,
            place,
            source,
            progress: asking.progress,
            stopping: asking.stopping,
            scratch: cx.scratch,
            confirm: asking.confirm,
        };
        engine.land(cx, row, &review, sink.as_ref(), &mut lander)
    };
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
    let landed = engine.landed(cx, row, &journal)?;
    Ok(status_of(source, &review, &journal, landed))
}

/// The invocation `title` names, starting now: the journal says it is going. A run that was
/// stopped, or ended without saying how, goes on as the same run, so what it brought before the
/// stop and after it is one History entry, and undoing it takes all of it back. Any other starts
/// afresh and brings nothing yet.
fn begin(place: &Place, source: &str, journal: &mut Journal, title: String) -> Result<Invocation> {
    let resumed = journal
        .run_id
        .clone()
        .filter(|_| matches!(journal.state, RunState::Stopped | RunState::Running));
    let invocation = Invocation {
        id: resumed.clone().unwrap_or_else(|| {
            format!(
                "migrate-{source}-{}",
                hex::encode(rand::random::<[u8; 6]>())
            )
        }),
        title,
    };
    journal.state = RunState::Running;
    journal.run = Some(invocation.title.clone());
    journal.run_id = Some(invocation.id.clone());
    if resumed.is_none() {
        journal.brought.clear();
        journal.already_there = 0;
    }
    journal.failure = None;
    journal.failed.clear();
    journal.left_out.clear();
    place.keep_journal(source, journal)?;
    Ok(invocation)
}

/// Give up before anything is brought: the journal says it failed and why, and the error goes to
/// the caller.
fn failed<T>(
    place: &Place,
    source: &str,
    journal: &mut Journal,
    error: anyhow::Error,
) -> Result<T> {
    journal.state = RunState::Failed;
    journal.failure = Some(format!("{error:#}"));
    place.keep_journal(source, journal)?;
    Err(error)
}

/// `undo`: take the run `named` (a run's id, or a source for its last run) back in every record it
/// changed, in the workspace `workspace`.
///
/// # Errors
/// No such run, or History holds none of it.
pub fn undo(cx: &Cx<'_>, named: &str, workspace: &mut dyn Workspace) -> Result<Value> {
    let place = cx.place;
    let (source, run) = run_named(place, named)?;
    let row = registry::named(&source)?;
    let mut journal = place.journal(&source)?;
    let mut taken = Vec::new();
    let mut found = false;
    // Files a run wrote into the workspace's folder here go as they came: from the folder.
    if let Some(removed) = files::undo_written(place, &source, &run)? {
        found = true;
        taken.push(json!({"folder": removed}));
    }
    for record in row.resources {
        match undo_in(workspace, record, &run)? {
            RunInHistory::InEffect(_) => {
                found = true;
                taken.push(json!({"record": record}));
            }
            RunInHistory::Undone => found = true,
            RunInHistory::Absent => {}
        }
    }
    // A run History does not know is not taken back, whatever this computer kept of it: what it
    // brought stays in the workspace, so this computer goes on counting it.
    if !found {
        bail!(
            "History has no change of run {run} in {}: nothing was taken back",
            row.resources.join(", ")
        );
    }
    let forgotten = journal.runs.remove(&run).unwrap_or_default();
    for key in &forgotten {
        journal.landed.remove(key);
    }
    of(row).forget(cx, row, &run, &forgotten)?;
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
    if let Ok(row) = registry::named(named) {
        let journal = place.journal(row.key)?;
        let run = journal
            .run_id
            .ok_or_else(|| anyhow!("{} has no run to take back", row.key))?;
        return Ok((row.key.to_owned(), run));
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
///
/// # Errors
/// The record refused to take it back.
pub fn undo_in(workspace: &mut dyn Workspace, record: &str, run: &str) -> Result<RunInHistory> {
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
pub enum RunInHistory {
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

/// `status`: how `source`'s migration stands, or every reviewed source's.
///
/// # Errors
/// No such source, or it was never reviewed.
pub fn status(cx: &Cx<'_>, source: Option<&str>) -> Result<Value> {
    let place = cx.place;
    let sources = match source {
        Some(source) => vec![registry::named(source)?.key.to_owned()],
        None => place.sources(),
    };
    let mut all = Vec::new();
    for source in sources {
        let row = registry::named(&source)?;
        let review = place
            .review(&source)?
            .ok_or_else(|| not_reviewed(&source))?;
        let mut journal = place.journal(&source)?;
        if journal.state == RunState::Running && !(cx.running)(&source) {
            // Its run ended without saying how: what it brought is kept, start resumes.
            journal.state = RunState::Stopped;
        }
        let landed = of(row).landed(cx, row, &journal)?;
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
        RunState::Running | RunState::Complete => None,
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
        "failed": journal.failed,
        "left_out": journal.left_out,
        "destination": journal.destination,
        "next": next,
    })
}

/// `n` bytes as a person reads them: one decimal in the largest unit that holds a whole one.
#[must_use]
pub fn size(n: u64) -> String {
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

/// `count` of `plural` as a person reads it (`1 document`, `3 documents`).
#[must_use]
pub fn counted(count: usize, plural: &str) -> String {
    services::counted(count, plural)
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
    stopping: &'a dyn Fn() -> bool,
    /// Where the run may stage what it fetches.
    scratch: &'a Path,
    /// Asks the person whether a step that is theirs alone goes ahead.
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
        if (self.stopping)() {
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
        self.once_of(key, kind, 1, land)
    }

    /// Land one item, `key`, that holds `count` things of `kind` (an address book's contacts),
    /// unless the workspace has it.
    ///
    /// # Errors
    /// The run was stopped, the workspace refused, or the journal could not be kept.
    pub fn once_of(
        &mut self,
        key: &str,
        kind: &str,
        count: usize,
        land: impl FnOnce(&mut dyn Workspace, &Invocation) -> Result<Landed>,
    ) -> Result<()> {
        if self.has(key) {
            self.journal.already_there = self.journal.already_there.saturating_add(count);
            return Ok(());
        }
        self.stop_asked()?;
        self.started()?;
        match land(&mut *self.workspace, self.invocation)? {
            Landed::One => self.landed([key.to_owned()], kind, count),
            Landed::Already => {
                self.journal.already_there = self.journal.already_there.saturating_add(count);
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
            format!("{} brought over", counted(*total, kind))
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
        (self.stopping)()
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

    /// Ask `action` of `path` (`workspace/mail-accounts#add`) as the run's own agent, a step of
    /// this run: the workspace's policy asks its person where it says so, so the call waits in
    /// their asked record until they answer it.
    ///
    /// # Errors
    /// The migration was stopped, or the server refused or could not be reached.
    pub fn act(&mut self, path: &str, action: &str, args: Value) -> Result<Answer> {
        self.stop_asked()?;
        self.workspace
            .act(self.invocation, &format!("{path}#{action}"), args)
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

    /// Where the run may stage what it fetches, for a while.
    #[must_use]
    pub const fn scratch(&self) -> &Path {
        self.scratch
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
