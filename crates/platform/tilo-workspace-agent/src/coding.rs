//! Coding agents, and what any sub-agent made.
//!
//! A coding agent is one kind of sub-agent, never a resource of its own: a sub-agent whose
//! work a coding harness does in the workspace's machine, not Tilo's own loop. Its job, its
//! role, its conversation, how it stands, a note and a stop are the sub-agent's
//! ([`crate::sub`]). This file is only what the machine adds: one optional field on a
//! sub-agent ([`Coding`]: which harness, where it works, its process), present exactly when
//! the sub-agent works in the machine ([`MACHINE`]).
//!
//! What a sub-agent made is a field of every sub-agent ([`Produced`]): a coder's pull request
//! as a researcher's document, each a thing one can open.
//!
//! Which harness it is, is a word here and nothing more: its name, how it is started and what
//! its output means are its runner's, which writes what it knows through
//! [`WorkspaceAgents::sub_agent_coding`].

use serde::{Deserialize, Serialize};
use tilo_resource::{ActionCx, Id, JsonSchema, Model, Refusal};

use crate::WorkspaceAgents;
use crate::sub::{MAX_LINE, Sub, check_words, one_line, words};

/// Where a sub-agent works when a coding harness does its work in the workspace's machine.
pub const MACHINE: &str = "machine";
/// The longest path or branch a coding agent's item keeps, in bytes.
pub const MAX_PATH: usize = 1024;
/// The longest id of a coding harness, in bytes.
pub const MAX_HARNESS: usize = 64;
/// The longest label of something a sub-agent made, in bytes.
pub const MAX_LABEL: usize = 120;
/// The longest address of something a sub-agent made, in bytes.
pub const MAX_URL: usize = 2048;
/// The most things a sub-agent's item lists as made; the oldest leave first.
pub const MAX_PRODUCED: usize = 32;
/// What a thing a sub-agent made may be.
pub const PRODUCED_KINDS: [&str; 6] = [
    "pull-request",
    "change",
    "branch",
    "file",
    "resource",
    "page",
];
/// How a process in the machine may have ended.
pub const ENDINGS: [&str; 4] = ["finished", "stopped", "failed", "lost"];

/// A coding agent's harness as a process in the machine: one start of it.
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
    /// The Terminal session and command History shows it as; empty when it shows it as none
    #[serde(default)]
    pub command: String,
}

/// A sub-agent's work in the workspace's machine.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Coding {
    /// Which coding harness, by its id; empty until its runner says which one it started.
    pub harness: String,
    /// The harness's name as a person reads it; empty until its runner says it.
    pub harness_name: String,
    /// The folder it was started in: one of the workspace's files, or a path of the machine's
    /// own.
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
    /// Its newest process in the machine; none before its first.
    pub process: Option<CodingProcess>,
    /// What its model calls cost so far, in millionths of the currency.
    pub spent_micros: u64,
}

/// Where a thing a sub-agent made is in Tilo.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProducedAt {
    /// The app that keeps it, by its id
    pub app: String,
    /// The app's document it is in; empty for the app's own document of the workspace
    #[serde(default)]
    pub document: String,
    /// The resource itself, by its id there: an item, a path
    pub resource: String,
}

/// Something a sub-agent made that one can open.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Produced {
    /// Its id among what its sub-agent made.
    pub id: String,
    /// What it is ([`PRODUCED_KINDS`]).
    pub kind: String,
    /// What a person calls it.
    pub label: String,
    /// Its address outside Tilo: it opens in Browser.
    pub url: Option<String>,
    /// Where it is in Tilo.
    pub at: Option<ProducedAt>,
    /// When it was said, in milliseconds since the Unix epoch.
    pub at_ms: i64,
}

/// The coding part of a sub-agent to start.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewCoding {
    /// Which coding harness, by its id; leave it out for the first there is
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// The folder it starts in: one of the workspace's files, or a path of the machine's own
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

/// What a runner says of a coding agent's work in the machine: only what it names changes.
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

/// Refuses a sub-agent whose coding part or list of what it made is none this crate wrote.
pub(crate) fn check(sub: &Sub) -> Result<(), Refusal> {
    if sub.coding.is_some() != (sub.runs == MACHINE) {
        return Err(Refusal::new(
            "a sub-agent works in the machine exactly when it has a coding part",
        ));
    }
    if sub.produced.len() > MAX_PRODUCED {
        return Err(Refusal::new(format!(
            "a sub-agent lists at most {MAX_PRODUCED} things it made"
        )));
    }
    for made in &sub.produced {
        if !PRODUCED_KINDS.contains(&made.kind.as_str()) || made.url.is_some() == made.at.is_some()
        {
            return Err(Refusal::new(
                "a thing a sub-agent made has a kind, and an address or a place",
            ));
        }
        check_words(&made.label, MAX_LABEL, "what a sub-agent made")?;
    }
    if let Some(coding) = &sub.coding {
        check_words(&coding.repository, MAX_PATH, "a coding agent's repository")?;
        check_words(&coding.harness, MAX_HARNESS, "a coding harness's id")?;
        if let Some(process) = &coding.process {
            check_ending(process)?;
        }
    }
    Ok(())
}

impl WorkspaceAgents {
    /// The body of `sub-agent-produced`: the sub-agent `agent` itself, or its runner as it, says
    /// what it made.
    pub(crate) fn produced(
        &mut self,
        cx: &mut ActionCx,
        agent: &Id,
        (kind, label): (&str, &str),
        url: Option<String>,
        at: Option<ProducedAt>,
    ) -> Result<(), Refusal> {
        if !PRODUCED_KINDS.contains(&kind) {
            return Err(Refusal::new(format!(
                "{kind:?} is no kind of thing made: one of {}",
                PRODUCED_KINDS.join(", ")
            )));
        }
        let label = one_line(label, MAX_LABEL);
        let url = words(url, MAX_URL, "an address")?;
        if label.is_empty() || url.is_some() == at.is_some() {
            return Err(Refusal::new(
                "a thing made has a label, and either its address or its place in Tilo",
            ));
        }
        if url
            .as_deref()
            .is_some_and(|url| !url.starts_with("https://"))
        {
            return Err(Refusal::new(
                "an address a person opens starts with https://",
            ));
        }
        if let Some(at) = &at {
            check_words(&at.app, MAX_LABEL, "the app a thing made is in")?;
            check_words(&at.document, MAX_PATH, "the document a thing made is in")?;
            check_words(&at.resource, MAX_PATH, "the resource a thing made is")?;
            if at.app.trim().is_empty() || at.resource.trim().is_empty() {
                return Err(Refusal::new(
                    "a place in Tilo names its app and the resource there",
                ));
            }
        }
        let (id, at_ms) = (cx.id().to_string(), cx.now_ms());
        let sub = self.as_itself(cx, agent)?;
        // The same thing said again replaces what was said of it.
        sub.produced
            .retain(|made| made.kind != kind || made.url != url || made.at != at);
        sub.produced.push(Produced {
            id,
            kind: kind.to_owned(),
            label,
            url,
            at,
            at_ms,
        });
        let over = sub.produced.len().saturating_sub(MAX_PRODUCED);
        sub.produced.drain(..over);
        Ok(())
    }

    /// The body of `sub-agent-coding`: the runner, as the sub-agent `agent` itself, says how
    /// its work in the machine stands.
    pub(crate) fn coding_stands(
        &mut self,
        cx: &ActionCx,
        agent: &Id,
        report: CodingReport,
    ) -> Result<(), Refusal> {
        if let Some(process) = &report.process {
            check_ending(process)?;
            check_words(
                &process.command,
                MAX_PATH,
                "what History shows a process as",
            )?;
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
            .ok_or_else(|| Refusal::new("that sub-agent does not work in the machine"))?;
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
