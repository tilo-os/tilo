//! Coding agents: sub-agents whose work a coding harness does.
//!
//! A coding agent is one kind of sub-agent, never a resource of its own: a sub-agent whose
//! work a coding harness (Claude Code) does in a repository, not Tilo's own loop. Its job, its
//! role, its conversation, how it stands, a note and a stop are the sub-agent's
//! ([`crate::sub`]). This file is only what the harness adds: one optional field on a
//! sub-agent ([`Coding`]: which harness, where it works, its process).
//!
//! Which harness it is, is a word here and nothing more: its name, how it is started and what
//! its output means are its runner's, which writes what it knows through
//! [`WorkspaceAgents::sub_agent_coding`].

use serde::{Deserialize, Serialize};
use tilo_resource::{ActionCx, Id, JsonSchema, Model, Refusal};

use crate::WorkspaceAgents;
use crate::produced::{MAX_LABEL, MAX_PATH};
use crate::sub::{MAX_LINE, Sub, check_words, one_line, words};

/// The longest id of a coding harness, in bytes.
pub const MAX_HARNESS: usize = 64;
/// How a harness's process may have ended.
pub const ENDINGS: [&str; 4] = ["finished", "stopped", "failed", "lost"];

/// A coding agent's harness as a process: one start of it.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodingProcess {
    /// When it started, in milliseconds since the Unix epoch
    pub started_ms: i64,
    /// When it ended; none while it lives
    #[serde(default)]
    pub ended_ms: Option<i64>,
    /// How it ended: `finished`, `stopped`, `failed` or `lost`; empty while it lives
    #[serde(default)]
    pub ended: String,
    /// The harness's exit status, when it ran to one
    #[serde(default)]
    pub exit: Option<i32>,
    /// One plain line of why it ended so; empty for one that finished
    #[serde(default)]
    pub words: String,
    /// Who stopped it, as a member (`person:<id>` or `agent:<id>`); empty for one nobody
    /// stopped
    #[serde(default)]
    pub stopped_by: String,
}

/// A sub-agent's work by a coding harness.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Coding {
    /// Which coding harness, by its id; empty until its runner says which one it started.
    pub harness: String,
    /// The harness's name as a person reads it; empty until its runner says it.
    pub harness_name: String,
    /// The repository it was started on: a folder of the computer that works it.
    pub repository: String,
    /// Its own working copy.
    pub directory: String,
    /// Its own branch.
    pub branch: String,
    /// What its branch started from; empty for the repository's commit at the time.
    pub base: String,
    /// The harness's own id for its conversation, by which work that ended is taken up where
    /// it stood; empty until it has one.
    pub session: String,
    /// Its newest process; none before its first.
    pub process: Option<CodingProcess>,
    /// What its model calls cost so far, in millionths of the currency.
    pub spent_micros: u64,
}

/// The coding part of a sub-agent to start.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewCoding {
    /// Which coding harness, by its id; leave it out for the first there is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The repository it works on: a folder of the computer that works it
    pub repository: String,
    /// Its branch; leave it out for one named for the sub-agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// What a new branch starts from; leave it out for the repository's current commit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

/// A coding harness as its runner names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodingHarness {
    /// Its id
    pub id: String,
    /// Its name as a person reads it
    pub name: String,
}

/// What a runner says of a coding agent's work: only what it names changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodingReport {
    /// Its newest process.
    pub process: Option<CodingProcess>,
    /// The harness's own id for its conversation.
    pub session: Option<String>,
    /// What its model calls cost so far.
    pub spent_micros: Option<u64>,
    /// Which harness it started.
    pub harness: Option<CodingHarness>,
}

fn path(given: &str, what: &str) -> Result<String, Refusal> {
    let given = given.trim();
    if given.is_empty() {
        return Err(Refusal::new(format!("{what} names a folder")));
    }
    check_words(given, MAX_PATH, what)?;
    if given.split('/').any(|part| part == "..") {
        return Err(Refusal::new(format!("{what} goes up through no folder")));
    }
    let kept = given.trim_end_matches('/');
    Ok(if kept.is_empty() { "/" } else { kept }.to_owned())
}

fn check_ending(process: &CodingProcess) -> Result<(), Refusal> {
    if process.ended.is_empty() || ENDINGS.contains(&process.ended.as_str()) {
        return Ok(());
    }
    Err(Refusal::new(format!(
        "a process ends as one of: {}",
        ENDINGS.join(", ")
    )))
}

/// The coding part of the sub-agent `id` as it is kept when it starts: a working copy beside
/// its repository and a branch named for it, unless it was given others.
pub(crate) fn started(new: NewCoding, id: &str) -> Result<Coding, Refusal> {
    let repository = path(&new.repository, "a coding agent's repository")?;
    Ok(Coding {
        harness: words(new.harness, MAX_HARNESS, "a coding harness's id")?.unwrap_or_default(),
        directory: format!("{}.agents/{id}", repository.trim_end_matches('/')),
        branch: words(new.branch, MAX_PATH, "a branch")?.unwrap_or_else(|| format!("agent/{id}")),
        base: words(new.base, MAX_PATH, "what a branch starts from")?.unwrap_or_default(),
        repository,
        ..Coding::default()
    })
}

/// The sub-agent was stopped by the member `by`: a process of its harness that lives says so,
/// before whatever works it has ended it.
pub(crate) fn stopped(sub: &mut Sub, by: String) {
    if let Some(process) = sub
        .coding
        .as_mut()
        .and_then(|coding| coding.process.as_mut())
        .filter(|process| process.ended.is_empty())
    {
        process.stopped_by = by;
    }
}

/// Refuses a coding part none this crate wrote.
pub(crate) fn check(sub: &Sub) -> Result<(), Refusal> {
    let Some(coding) = &sub.coding else {
        return Ok(());
    };
    check_words(&coding.repository, MAX_PATH, "a coding agent's repository")?;
    check_words(&coding.directory, MAX_PATH, "a coding agent's working copy")?;
    check_words(&coding.branch, MAX_PATH, "a branch")?;
    check_words(&coding.harness, MAX_HARNESS, "a coding harness's id")?;
    if let Some(process) = &coding.process {
        check_ending(process)?;
    }
    Ok(())
}

impl WorkspaceAgents {
    /// The body of `sub-agent-coding`: the runner, as the sub-agent `agent` itself, says how
    /// its work stands.
    pub(crate) fn coding_stands(
        &mut self,
        cx: &ActionCx,
        agent: &Id,
        report: CodingReport,
    ) -> Result<(), Refusal> {
        if let Some(process) = &report.process {
            check_ending(process)?;
            check_words(&process.stopped_by, MAX_LABEL, "who stopped a process")?;
        }
        let session = words(report.session, MAX_PATH, "a harness's own id")?;
        let harness = report
            .harness
            .map(|harness| {
                let id = words(Some(harness.id), MAX_HARNESS, "a coding harness's id")?;
                let name = words(Some(harness.name), MAX_LABEL, "a coding harness's name")?;
                id.zip(name)
                    .ok_or_else(|| Refusal::new("a coding harness has an id and a name"))
            })
            .transpose()?;
        let sub = self.as_itself(cx, agent)?;
        let coding = sub
            .coding
            .as_mut()
            .ok_or_else(|| Refusal::new("that sub-agent is no coding agent"))?;
        if let Some((id, name)) = harness {
            coding.harness = id;
            coding.harness_name = name;
        }
        if let Some(session) = session {
            coding.session = session;
        }
        if let Some(mut process) = report.process {
            process.words = one_line(&process.words, MAX_LINE);
            // Who stopped it was said when it was stopped: its end keeps that.
            if process.stopped_by.is_empty()
                && let Some(known) = coding
                    .process
                    .as_ref()
                    .filter(|known| known.started_ms == process.started_ms)
            {
                process.stopped_by.clone_from(&known.stopped_by);
            }
            coding.process = Some(process);
        }
        if let Some(spent) = report.spent_micros {
            // What was spent only grows: a late report never takes money back.
            coding.spent_micros = coding.spent_micros.max(spent);
        }
        Ok(())
    }
}
