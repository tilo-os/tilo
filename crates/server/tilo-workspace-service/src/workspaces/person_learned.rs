//! What the agents on this server learned about a person, kept where the server reads and writes
//! it with every device of theirs off.
//!
//! A person's own resource in a workspace (Chat's `chat.learned`) is written by their devices
//! alone: a server is refused it like every other member, so an agent on a workspace's server cannot keep a
//! fact there. What such an agent learns about its keeper is kept here instead, in the person's
//! own store beside their vault and their model providers: the same resource
//! ([`tilo_workspace_agent::learned::Learned`]), changed through its own three verbs, one
//! document for each workspace the agent works in, as their own resource is one for each
//! workspace. It is one person's apart from every other person's, readable on disk by the
//! server's own user alone, and changed one change at a time.
//!
//! Two callers reach it. Code linked into the server, which answers the turns of a person's agents and decides
//! first whose turn it is ([`Workspaces::learned_about`], [`Workspaces::change_learned`]). And
//! the person themselves on a device of theirs, through the provider [`PROVIDER`]: they read the
//! list, correct a fact, forget one, or write one down. Nobody else is answered, the person's
//! own agents included: unknown and forbidden are the same answer.
//!
//! Nothing a fact says is written to a log.

pub mod dates;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use dates::{
    BoundaryWake, DateWindow, IsoDate, WindowBoundary, WindowPhase, boundary_wakes,
    next_boundary_wake, render_relative_window, upcoming_boundary_wakes,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::ActionCallError::{self, UnknownAction};
use tilo_actions::remote::{Caller, InvokeRequest, RemoteActionTarget};
use tilo_lock::MutexRecover as _;
use tilo_resource::{Id, NonBlank, Replica};
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::learned::{Fact, Learned, learned};

use super::Workspaces;
use super::person_settings::{answer, argument};

/// The provider a person reads and changes what this server's agents learned about them by.
pub const PROVIDER: &str = super::actions::LEARNED;
/// Its one resource: the facts, in the workspace the request comes through.
pub const RESOURCE: &str = "facts";
/// Read the facts: answers [`Facts`].
pub const LIST: &str = "list";
/// Write a fact down oneself, `{ text, source, startsOn?, endsOn? }`: answers [`Facts`].
pub const LEARN: &str = "learn";
/// Correct a fact, `{ fact, text, source?, startsOn?, endsOn? }`: answers [`Facts`].
pub const CORRECT: &str = "correct";
/// Confirm an existing fact (and optionally update its dates), `{ fact, startsOn?, endsOn? }`:
/// answers [`Facts`].
pub const CONFIRM: &str = "confirm";
/// Forget a fact, `{ fact }`: answers [`Facts`].
pub const FORGET: &str = "forget";

/// The app whose resource this is, as every change of it names it.
const CHAT: (&str, &str) = ("so.tilo.chat", "Chat");

/// The folder of a person's store their learned resources are kept in, one file a workspace.
const FOLDER: &str = "learned";

/// Suffix for the sidecar file keeping optional date windows and confirmation authority beside a
/// workspace's learned automerge document.
const DETAILS_SUFFIX: &str = "-details.json";

/// The most bytes one kept resource holds. A few hundred short facts with their history stay far
/// below it, and a file past it is neither read nor written.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// One change of a kept resource at a time on this server: a change reads the resource, applies
/// itself and writes the resource back whole.
static CHANGING: Mutex<()> = Mutex::new(());

/// The provider's `action`, as a caller addresses it.
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress {
            resource: RESOURCE.to_owned(),
            action: action.to_owned(),
        },
    }
}

/// Who vouches for a learned fact, strictly ordered from weakest to strongest:
/// `DecisionModel < LargeModel < Person`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum FactAuthority {
    /// Inferred by the fast decision model (weakest).
    DecisionModel,
    /// Written or inferred by a workspace agent / large model.
    #[default]
    LargeModel,
    /// Written, corrected, or explicitly confirmed by the person (strongest).
    Person,
}

impl FactAuthority {
    /// Whether an update carrying `self` authority may overwrite or forget a fact held at
    /// `existing`.
    #[must_use]
    pub const fn may_overwrite(self, existing: Self) -> bool {
        self.rank() >= existing.rank()
    }

    /// Whether this authority represents the person's own word or explicit confirmation.
    #[must_use]
    pub const fn is_person(self) -> bool {
        matches!(self, Self::Person)
    }

    const fn rank(self) -> u8 {
        match self {
            Self::DecisionModel => 0,
            Self::LargeModel => 1,
            Self::Person => 2,
        }
    }
}

/// What the provider answers: the facts this server keeps about the person in one workspace,
/// newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    /// The workspace they were learned in.
    pub workspace: String,
    /// What the resource stands at: the same until a fact is kept, corrected, confirmed or
    /// forgotten, and empty while nothing was learned. A reader that holds a list reads again
    /// when it differs.
    pub revision: String,
    /// The facts, newest first.
    pub facts: Vec<FactView>,
}

/// One fact, as its person reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactView {
    /// Its id: what `correct`, `confirm` and `forget` name it by.
    pub id: String,
    /// What was learned.
    pub text: String,
    /// Where it came from: the agent's words, closed by the message of the turn that kept it
    /// where an agent on this server did.
    pub source: String,
    /// When it was learned or last corrected, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// The agent that wrote it down, by its id; absent where the person wrote it themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Concrete start date of a time-relative fact (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_on: Option<String>,
    /// Concrete end date of a time-relative fact (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_on: Option<String>,
    /// Whether the person themselves set or confirmed this fact (their word outranks model
    /// inference).
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirmed: bool,
    /// Provenance authority level (`DecisionModel < LargeModel < Person`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<FactAuthority>,
}

impl FactView {
    /// The effective authority holding this fact, accounting for legacy views without explicit
    /// authority metadata.
    #[must_use]
    pub fn effective_authority(&self) -> FactAuthority {
        if self.confirmed || self.by.is_none() {
            return FactAuthority::Person;
        }
        self.authority.unwrap_or(FactAuthority::LargeModel)
    }

    /// Parse this fact's optional start/end dates into a validated [`DateWindow`].
    pub fn date_window(&self) -> Result<DateWindow, String> {
        DateWindow::parse(self.starts_on.as_deref(), self.ends_on.as_deref())
    }

    /// Render fresh relative phrasing for this fact's date window on `today`, or `None` when
    /// undated.
    #[must_use]
    pub fn render_relative(&self, today: IsoDate) -> Option<String> {
        self.date_window().ok()?.render_relative(today)
    }

    /// Render the fact's text alongside fresh relative phrasing and concrete dates for `today`.
    #[must_use]
    pub fn render_for_date(&self, today: IsoDate) -> String {
        let Ok(window) = self.date_window() else {
            return self.text.clone();
        };
        let Some(relative) = window.render_relative(today) else {
            return self.text.clone();
        };
        match window.format_iso_range() {
            Some(range) => format!("{} [{relative}; {range}]", self.text),
            None => format!("{} [{relative}]", self.text),
        }
    }

    /// Compute upcoming boundary wakes on or after `today` for this fact's date window.
    #[must_use]
    pub fn upcoming_boundary_wakes(&self, today: IsoDate) -> Vec<BoundaryWake> {
        self.date_window().map_or_else(
            |_| Vec::new(),
            |window| window.upcoming_boundary_wakes(today),
        )
    }
}

/// One upcoming confirmation wake for a dated learned fact, ready for the deterministic
/// scheduler.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactBoundaryWake {
    /// The learned fact's id.
    pub fact_id: String,
    /// What the fact says.
    pub text: String,
    /// Calendar date on which the scheduler wakes (`YYYY-MM-DD`).
    pub on: IsoDate,
    /// Which window boundary this wake corresponds to.
    pub boundary: WindowBoundary,
    /// Whether the person has already confirmed this fact.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirmed: bool,
}

/// Structured input for keeping a learned fact with optional dates and provenance authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactUpdate {
    /// What was learned, in a sentence or two.
    pub text: String,
    /// Where it came from.
    pub source: String,
    /// Optional concrete start date (`YYYY-MM-DD`).
    pub starts_on: Option<String>,
    /// Optional concrete end date (`YYYY-MM-DD`).
    pub ends_on: Option<String>,
    /// Who vouches for this write (`DecisionModel < LargeModel < Person`).
    pub authority: FactAuthority,
}

/// Structured correction for an existing learned fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCorrection {
    /// What the fact says now.
    pub text: String,
    /// Replacement source, or `None` to keep the existing source.
    pub source: Option<String>,
    /// Change to `starts_on`: `None` keeps existing, `Some(None)` clears, `Some(Some(date))` sets.
    pub starts_on: Option<Option<String>>,
    /// Change to `ends_on`: `None` keeps existing, `Some(None)` clears, `Some(Some(date))` sets.
    pub ends_on: Option<Option<String>>,
    /// Who vouches for this correction (`DecisionModel < LargeModel < Person`).
    pub authority: FactAuthority,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FactDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    starts_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ends_on: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority: Option<FactAuthority>,
}

impl FactDetails {
    const fn is_empty(&self) -> bool {
        self.starts_on.is_none()
            && self.ends_on.is_none()
            && !self.confirmed
            && self.authority.is_none()
    }

    fn set_authority(&mut self, authority: FactAuthority) {
        self.confirmed = authority.is_person();
        self.authority = Some(authority);
    }

    fn set_window(&mut self, window: DateWindow) {
        self.starts_on = window.starts_on.map(|date| date.to_string());
        self.ends_on = window.ends_on.map(|date| date.to_string());
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Learn {
    text: String,
    source: String,
    #[serde(default, alias = "starts_on")]
    starts_on: Option<String>,
    #[serde(default, alias = "ends_on")]
    ends_on: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Correct {
    fact: String,
    text: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default, alias = "starts_on")]
    starts_on: Option<String>,
    #[serde(default, alias = "ends_on")]
    ends_on: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Confirm {
    fact: String,
    #[serde(default, alias = "starts_on")]
    starts_on: Option<String>,
    #[serde(default, alias = "ends_on")]
    ends_on: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forget {
    fact: String,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires `fn(&T) -> bool`"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

impl Workspaces {
    /// Where `person`'s learned resource of `workspace` is kept.
    fn learned_at(&self, person: &[u8; 32], workspace: &str) -> Result<PathBuf, String> {
        // The id as the workspace spells it, never the caller's text: the name of a file.
        let workspace = collab_types::WorkspaceId::parse(workspace)
            .ok_or_else(|| "not a workspace id".to_owned())?;
        Ok(self
            .person_settings_root(person)
            .join(FOLDER)
            .join(format!("{workspace}.automerge")))
    }

    /// Where `person`'s structured date/authority metadata for `workspace` is kept beside the
    /// automerge resource.
    fn learned_details_at(&self, person: &[u8; 32], workspace: &str) -> Result<PathBuf, String> {
        let workspace = collab_types::WorkspaceId::parse(workspace)
            .ok_or_else(|| "not a workspace id".to_owned())?;
        Ok(self
            .person_settings_root(person)
            .join(FOLDER)
            .join(format!("{workspace}{DETAILS_SUFFIX}")))
    }

    /// The resource this server keeps about `person` in `workspace`, its sidecar metadata, and
    /// what it stands at; none where nothing was learned yet, and none where it does not read.
    fn learned_kept(
        &self,
        person: &[u8; 32],
        workspace: &str,
    ) -> Option<(Learned, BTreeMap<String, FactDetails>, String)> {
        let path = match self.learned_at(person, workspace) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%workspace, %error, "what was learned about a person is not read");
                return None;
            }
        };
        let bytes = match read(&path) {
            Ok(bytes) => bytes?,
            Err(error) => {
                tracing::warn!(%workspace, %error, "what was learned about a person is not read");
                return None;
            }
        };
        let details_bytes = self
            .learned_details_at(person, workspace)
            .ok()
            .and_then(|details_path| read(&details_path).ok().flatten());
        let revision = compute_revision(&bytes, details_bytes.as_deref());
        let details = details_bytes
            .as_deref()
            .map(decode_details)
            .unwrap_or_default();
        let read_state = opened(Some(bytes))
            .and_then(|resource| state(&resource))
            .ok();
        if read_state.is_none() {
            // Why is left unsaid: a resource's own error may quote what it holds.
            tracing::warn!(%workspace, "what was learned about a person does not read as its resource");
        }
        read_state.map(|learned| (learned, details, revision))
    }

    /// What this server's agents learned about `person` in `workspace`: none where nothing was
    /// learned yet. For the code that answers the turns of the person's agents; it decides whose
    /// turn is told it.
    #[must_use]
    pub fn learned_about(&self, person: &[u8; 32], workspace: &str) -> Option<Learned> {
        self.learned_kept(person, workspace)
            .map(|(learned, _, _)| learned)
    }

    /// The facts this server keeps about `person` in `workspace`, with their structured date
    /// windows and confirmation state, newest first.
    #[must_use]
    pub fn learned_facts_for(&self, person: &[u8; 32], workspace: &str) -> Facts {
        self.learned_facts(person, workspace)
    }

    /// All upcoming deterministic boundary wakes on or after `today` across `person`'s dated
    /// learned facts in `workspace`, sorted chronologically.
    #[must_use]
    pub fn upcoming_fact_wakes(
        &self,
        person: &[u8; 32],
        workspace: &str,
        today: IsoDate,
    ) -> Vec<FactBoundaryWake> {
        let listed = self.learned_facts(person, workspace);
        let mut wakes = Vec::new();
        for fact in listed.facts {
            for wake in fact.upcoming_boundary_wakes(today) {
                wakes.push(FactBoundaryWake {
                    fact_id: fact.id.clone(),
                    text: fact.text.clone(),
                    on: wake.on,
                    boundary: wake.boundary,
                    confirmed: fact.confirmed,
                });
            }
        }
        wakes.sort_by(|left, right| {
            (left.on, left.boundary, left.fact_id.as_str()).cmp(&(
                right.on,
                right.boundary,
                right.fact_id.as_str(),
            ))
        });
        wakes
    }

    /// Change what this server keeps about `person` in `workspace` through the resource's own
    /// verbs: `change` applies them, as the agent `by` acting for the person, or as the person
    /// themselves where `by` is none, stamped with the time now. One change is made at a time,
    /// the first makes the resource, and one that fails keeps the resource as it was.
    ///
    /// Authority order is enforced: an agent write (`by.is_some()`) cannot silently overwrite or
    /// forget a fact set or confirmed by the person.
    pub fn change_learned<R>(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        change: impl FnOnce(&mut Replica<Learned>) -> Result<R, String>,
    ) -> Result<R, String> {
        let authority = if by.is_some() {
            FactAuthority::LargeModel
        } else {
            FactAuthority::Person
        };
        self.change_learned_with_authority(person, workspace, by, authority, change)
    }

    /// Like [`Self::change_learned`], specifying the provenance [`FactAuthority`] of the writer
    /// (`DecisionModel < LargeModel < Person`). Any modification or deletion of a fact held at a
    /// strictly higher authority is refused without changing disk.
    pub fn change_learned_with_authority<R>(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        authority: FactAuthority,
        change: impl FnOnce(&mut Replica<Learned>) -> Result<R, String>,
    ) -> Result<R, String> {
        let path = self.learned_at(person, workspace)?;
        let details_path = self.learned_details_at(person, workspace)?;
        let keeper = hex::encode(person);
        let one_at_a_time = CHANGING.lock_recover();
        let resource = opened(read(&path)?)?;
        let before = state(&resource)?;
        let mut details = read_details(&details_path);
        let mut resource = match by {
            Some(agent) => resource.as_agent_for(agent, &keeper),
            None => resource.as_author(&keeper),
        }
        .in_app(CHAT.0, CHAT.1);
        resource.set_now_ms(tilo_clock::unix_now_ms_i64());
        let answer = change(&mut resource)?;
        let after = state(&resource)?;
        enforce_authority(&before, &after, &details, authority)?;
        let details_changed = sync_details_after_change(&before, &after, &mut details, authority);
        let bytes = resource.save();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            return Err("the list of what was learned is full".to_owned());
        }
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        if details_changed {
            write_details(&details_path, &details)?;
        }
        drop(one_at_a_time);
        Ok(answer)
    }

    /// Keep `text` as a fact about `person` in `workspace`, with where it came from in words,
    /// as server code that read it from the workspace's resources: `by` is the id it writes under,
    /// one no agent has. A model's words never come this way: a turn keeps what it learns
    /// through its own tools, which decide first whose turn it is.
    pub fn keep_learned_fact(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: &str,
        (text, source): (&str, &str),
    ) -> Result<(), String> {
        let learn = learned::Learn {
            text: said(text)?,
            source: said(source)?,
        };
        self.change_learned(person, workspace, Some(by), |resource| {
            applied(resource.apply(Learned::LEARN.with(learn)))
        })
    }

    /// Keep a fact with optional concrete start/end dates and explicit [`FactAuthority`],
    /// returning the kept fact's id.
    ///
    /// Enforces authority order: if a matching fact is held at a higher authority (for example,
    /// confirmed by the person), a lower-authority update is refused without writing to disk.
    pub fn keep_dated_fact(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        update: FactUpdate,
    ) -> Result<String, String> {
        let window = DateWindow::parse(update.starts_on.as_deref(), update.ends_on.as_deref())?;
        let text = said(&update.text)?;
        let source = said(&update.source)?;
        let trimmed_text = text.as_str().trim().to_owned();
        let path = self.learned_at(person, workspace)?;
        let details_path = self.learned_details_at(person, workspace)?;
        let keeper = hex::encode(person);
        let one_at_a_time = CHANGING.lock_recover();
        let resource = opened(read(&path)?)?;
        let before = state(&resource)?;
        let mut details = read_details(&details_path);

        if let Some((existing_id, existing_fact)) = before
            .facts
            .iter()
            .find(|(_, fact)| fact.text.eq_ignore_ascii_case(&trimmed_text))
        {
            let existing_auth =
                effective_fact_authority(existing_fact, details.get(existing_id.as_str()));
            if !update.authority.may_overwrite(existing_auth) {
                return Err(authority_refusal(existing_auth, update.authority));
            }
        }

        let mut resource = match by {
            Some(agent) => resource.as_agent_for(agent, &keeper),
            None => resource.as_author(&keeper),
        }
        .in_app(CHAT.0, CHAT.1);
        resource.set_now_ms(tilo_clock::unix_now_ms_i64());
        applied(resource.apply(Learned::LEARN.with(learned::Learn { text, source })))?;
        let after = state(&resource)?;
        let fact_id = after
            .facts
            .iter()
            .find(|(_, fact)| fact.text.eq_ignore_ascii_case(&trimmed_text))
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| "the kept fact was not found after applying learn".to_owned())?;

        let entry = details.entry(fact_id.clone()).or_default();
        entry.set_authority(update.authority);
        if !window.is_empty() {
            entry.set_window(window);
        }
        if entry.is_empty() {
            details.remove(&fact_id);
        }
        prune_details(&after, &mut details);

        let bytes = resource.save();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            return Err("the list of what was learned is full".to_owned());
        }
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        write_details(&details_path, &details)?;
        drop(one_at_a_time);
        Ok(fact_id)
    }

    /// Correct an existing fact with optional date updates and explicit [`FactAuthority`].
    ///
    /// Refuses if the fact does not exist, if `update.authority` cannot overwrite the fact's
    /// current authority, or if the resulting date window is invalid (`starts_on > ends_on`).
    pub fn correct_dated_fact(
        &self,
        person: &[u8; 32],
        workspace: &str,
        by: Option<&str>,
        fact_id: &str,
        update: FactCorrection,
    ) -> Result<(), String> {
        let text = said(&update.text)?;
        let path = self.learned_at(person, workspace)?;
        let details_path = self.learned_details_at(person, workspace)?;
        let keeper = hex::encode(person);
        let one_at_a_time = CHANGING.lock_recover();
        let resource = opened(read(&path)?)?;
        let before = state(&resource)?;
        let mut details = read_details(&details_path);

        let id = Id::from_raw(fact_id.to_owned());
        let existing_fact = before
            .facts
            .get(&id)
            .ok_or_else(|| format!("there is no fact {fact_id}"))?;
        let existing_details = details.get(fact_id).cloned().unwrap_or_default();
        let existing_auth = effective_fact_authority(existing_fact, Some(&existing_details));
        if !update.authority.may_overwrite(existing_auth) {
            return Err(authority_refusal(existing_auth, update.authority));
        }

        let next_start = match update.starts_on {
            None => existing_details.starts_on,
            Some(replacement) => replacement,
        };
        let next_end = match update.ends_on {
            None => existing_details.ends_on,
            Some(replacement) => replacement,
        };
        let window = DateWindow::parse(next_start.as_deref(), next_end.as_deref())?;

        let mut resource = match by {
            Some(agent) => resource.as_agent_for(agent, &keeper),
            None => resource.as_author(&keeper),
        }
        .in_app(CHAT.0, CHAT.1);
        resource.set_now_ms(tilo_clock::unix_now_ms_i64());
        applied(resource.apply(Learned::CORRECT.with(learned::Correct {
            fact: id,
            text,
            source: update.source,
        })))?;
        let after = state(&resource)?;

        let entry = details.entry(fact_id.to_owned()).or_default();
        entry.set_authority(update.authority);
        entry.set_window(window);
        prune_details(&after, &mut details);

        let bytes = resource.save();
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            return Err("the list of what was learned is full".to_owned());
        }
        tilo_files::native::atomic::write_creating_parent_with_mode(&path, &bytes, 0o600)
            .map_err(|error| error.to_string())?;
        write_details(&details_path, &details)?;
        drop(one_at_a_time);
        Ok(())
    }

    /// Confirm an existing fact as the person (locking it at [`FactAuthority::Person`] against
    /// silent model overwrites) and optionally replace its date window.
    pub fn confirm_learned_fact(
        &self,
        person: &[u8; 32],
        workspace: &str,
        fact_id: &str,
        dates: Option<DateWindow>,
    ) -> Result<(), String> {
        let patch = dates.map(|window| {
            (
                Some(window.starts_on.map(|date| date.to_string())),
                Some(window.ends_on.map(|date| date.to_string())),
            )
        });
        let (starts_patch, ends_patch) = patch.unwrap_or((None, None));
        self.confirm_learned_fact_patch(person, workspace, fact_id, starts_patch, ends_patch)
    }

    fn confirm_learned_fact_patch(
        &self,
        person: &[u8; 32],
        workspace: &str,
        fact_id: &str,
        starts_on: Option<Option<String>>,
        ends_on: Option<Option<String>>,
    ) -> Result<(), String> {
        let path = self.learned_at(person, workspace)?;
        let details_path = self.learned_details_at(person, workspace)?;
        let one_at_a_time = CHANGING.lock_recover();
        let resource = opened(read(&path)?)?;
        let kept = state(&resource)?;
        let id = Id::from_raw(fact_id.to_owned());
        if kept.facts.get(&id).is_none() {
            return Err(format!("there is no fact {fact_id}"));
        }
        let mut details = read_details(&details_path);
        let existing = details.get(fact_id).cloned().unwrap_or_default();
        let next_start = match starts_on {
            None => existing.starts_on,
            Some(replacement) => replacement,
        };
        let next_end = match ends_on {
            None => existing.ends_on,
            Some(replacement) => replacement,
        };
        let window = DateWindow::parse(next_start.as_deref(), next_end.as_deref())?;
        let entry = details.entry(fact_id.to_owned()).or_default();
        entry.set_authority(FactAuthority::Person);
        entry.set_window(window);
        prune_details(&kept, &mut details);
        write_details(&details_path, &details)?;
        drop(one_at_a_time);
        Ok(())
    }

    /// The facts this server keeps about `person` in `workspace`, as the provider answers them.
    fn learned_facts(&self, person: &[u8; 32], workspace: &str) -> Facts {
        let (learned, details, revision) = self.learned_kept(person, workspace).unwrap_or_default();
        let mut facts: Vec<FactView> = learned
            .facts
            .iter()
            .map(|(id, fact)| {
                let extra = details.get(id.as_str());
                let authority = effective_fact_authority(fact, extra);
                FactView {
                    id: id.to_string(),
                    text: fact.text.clone(),
                    source: fact.source.clone(),
                    at_ms: fact.at,
                    by: fact.by.clone(),
                    starts_on: extra.and_then(|item| item.starts_on.clone()),
                    ends_on: extra.and_then(|item| item.ends_on.clone()),
                    confirmed: authority.is_person(),
                    authority: Some(authority),
                }
            })
            .collect();
        facts.sort_by_key(|fact| std::cmp::Reverse(fact.at_ms));
        Facts {
            workspace: workspace.to_owned(),
            revision,
            facts,
        }
    }

    /// Answer a [`PROVIDER`] request: the person's own list in the workspace the request comes
    /// through, read or changed by them on a device of theirs and by nobody else.
    pub(super) fn person_learned_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let workspace = workspace_id.ok_or(UnknownAction)?.to_string();
        if request.target.address.resource != RESOURCE {
            return Err(UnknownAction);
        }
        let changed = match request.target.address.action.as_str() {
            LIST => Ok(()),
            LEARN => {
                let Learn {
                    text,
                    source,
                    starts_on,
                    ends_on,
                } = argument(request, "a fact with where it came from")?;
                self.keep_dated_fact(
                    &person,
                    &workspace,
                    None,
                    FactUpdate {
                        text,
                        source,
                        starts_on,
                        ends_on,
                        authority: FactAuthority::Person,
                    },
                )
                .map(drop)
            }
            CORRECT => {
                let Correct {
                    fact,
                    text,
                    source,
                    starts_on,
                    ends_on,
                } = argument(request, "the fact to correct and what it says now")?;
                let parsed = (parse_date_patch(starts_on), parse_date_patch(ends_on));
                match parsed {
                    (Ok(starts_on), Ok(ends_on)) => self.correct_dated_fact(
                        &person,
                        &workspace,
                        None,
                        &fact,
                        FactCorrection {
                            text,
                            source,
                            starts_on,
                            ends_on,
                            authority: FactAuthority::Person,
                        },
                    ),
                    (Err(error), _) | (_, Err(error)) => Err(error),
                }
            }
            CONFIRM => {
                let Confirm {
                    fact,
                    starts_on,
                    ends_on,
                } = argument(request, "the fact to confirm")?;
                let parsed = (parse_date_patch(starts_on), parse_date_patch(ends_on));
                match parsed {
                    (Ok(starts_on), Ok(ends_on)) => self
                        .confirm_learned_fact_patch(&person, &workspace, &fact, starts_on, ends_on),
                    (Err(error), _) | (_, Err(error)) => Err(error),
                }
            }
            FORGET => {
                let Forget { fact } = argument(request, "the fact to forget")?;
                self.change_learned(&person, &workspace, None, |resource| {
                    let forget = learned::Forget {
                        fact: Id::from_raw(fact),
                    };
                    applied(resource.apply(Learned::FORGET.with(forget)))
                })
            }
            _ => return Err(UnknownAction),
        };
        answer(&changed.map(|()| self.learned_facts(&person, &workspace)))
    }
}

/// Turn an optional provider date argument into a patch:
/// - `None`: leave the existing date unchanged.
/// - `Some("")` (or whitespace): clear the date.
/// - `Some(date)`: validate as `YYYY-MM-DD` and set.
fn parse_date_patch(raw: Option<String>) -> Result<Option<Option<String>>, String> {
    match raw {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Ok(Some(None)),
        Some(value) => IsoDate::parse(&value).map(|date| Some(Some(date.to_string()))),
    }
}

fn effective_fact_authority(fact: &Fact, details: Option<&FactDetails>) -> FactAuthority {
    if fact.by.is_none() || details.is_some_and(|item| item.confirmed) {
        return FactAuthority::Person;
    }
    details
        .and_then(|item| item.authority)
        .unwrap_or(FactAuthority::LargeModel)
}

fn authority_refusal(existing: FactAuthority, incoming: FactAuthority) -> String {
    match (existing, incoming) {
        (FactAuthority::Person, _) => {
            "this fact was set or confirmed by the person and requires explicit confirmation to change"
                .to_owned()
        }
        (FactAuthority::LargeModel, FactAuthority::DecisionModel) => {
            "a decision-model inference cannot overwrite a fact held at higher authority".to_owned()
        }
        _ => "insufficient authority to overwrite this fact".to_owned(),
    }
}

/// Refuse any modification or removal of a fact whose existing authority outranks `authority`.
fn enforce_authority(
    before: &Learned,
    after: &Learned,
    details: &BTreeMap<String, FactDetails>,
    authority: FactAuthority,
) -> Result<(), String> {
    for (id, before_fact) in before.facts.iter() {
        let existing_auth = effective_fact_authority(before_fact, details.get(id.as_str()));
        if authority.may_overwrite(existing_auth) {
            continue;
        }
        let changed_or_removed = after
            .facts
            .get(id)
            .is_none_or(|after_fact| after_fact != before_fact);
        if changed_or_removed {
            return Err(authority_refusal(existing_auth, authority));
        }
    }
    Ok(())
}

/// Synchronize sidecar metadata for facts added, modified, or removed by a change.
fn sync_details_after_change(
    before: &Learned,
    after: &Learned,
    details: &mut BTreeMap<String, FactDetails>,
    authority: FactAuthority,
) -> bool {
    let mut dirty = false;
    for (id, after_fact) in after.facts.iter() {
        let changed = before
            .facts
            .get(id)
            .is_none_or(|before_fact| before_fact != after_fact);
        if changed {
            let entry = details.entry(id.to_string()).or_default();
            if entry.authority != Some(authority) || entry.confirmed != authority.is_person() {
                entry.set_authority(authority);
                dirty = true;
            }
        }
    }
    if prune_details(after, details) {
        dirty = true;
    }
    dirty
}

/// Remove sidecar metadata entries whose fact ids are no longer present in `kept`.
fn prune_details(kept: &Learned, details: &mut BTreeMap<String, FactDetails>) -> bool {
    let before_len = details.len();
    details.retain(|id, entry| {
        !entry.is_empty() && kept.facts.get(&Id::from_raw(id.clone())).is_some()
    });
    details.len() != before_len
}

fn compute_revision(doc_bytes: &[u8], details_bytes: Option<&[u8]>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(doc_bytes);
    if let Some(extra) = details_bytes {
        hasher.update(b"\0");
        hasher.update(extra);
    }
    hasher
        .finalize()
        .to_hex()
        .get(..16)
        .unwrap_or_default()
        .to_owned()
}

fn read_details(path: &Path) -> BTreeMap<String, FactDetails> {
    let bytes = match read(path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(%error, "learned fact details are not read");
            return BTreeMap::new();
        }
    };
    decode_details(&bytes)
}

fn decode_details(bytes: &[u8]) -> BTreeMap<String, FactDetails> {
    match serde_json::from_slice(bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            // Where it fails is logged and not why: the error's own words may quote the file.
            let (line, column) = (error.line(), error.column());
            tracing::warn!(line, column, "learned fact details do not read as kept");
            BTreeMap::new()
        }
    }
}

fn write_details(path: &Path, details: &BTreeMap<String, FactDetails>) -> Result<(), String> {
    if details.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        };
    }
    let bytes = serde_json::to_vec(details).map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err("the list of what was learned is full".to_owned());
    }
    tilo_files::native::atomic::write_creating_parent_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// `words` as a verb takes them: something, on one line.
fn said(words: &str) -> Result<NonBlank, String> {
    NonBlank::parse(words).ok_or_else(|| "a fact says something and where it came from".to_owned())
}

/// How a verb of the resource ended, in words when it refused.
fn applied<T>(ended: Result<T, tilo_actions::ApiError>) -> Result<(), String> {
    ended.map(drop).map_err(|error| error.to_string())
}

/// What `resource` holds now.
fn state(resource: &Replica<Learned>) -> Result<Learned, String> {
    resource.state().map_err(|error| error.to_string())
}

/// The resource `bytes` hold, or a new one where nothing was learned yet.
fn opened(bytes: Option<Vec<u8>>) -> Result<Replica<Learned>, String> {
    let Some(bytes) = bytes else {
        return Ok(Replica::new());
    };
    let mut doc = automerge::AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
    tilo_resource::evolution::migrate::<Learned>(&mut doc).map_err(|error| error.to_string())?;
    Ok(Replica::from_doc(doc))
}

/// The resource kept at `path`, or none where there is none yet.
fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(kept) if kept.len() > MAX_BYTES => {
            return Err("the list of what was learned is larger than one is kept".to_owned());
        }
        Ok(_) => {}
    }
    std::fs::read(path)
        .map(Some)
        .map_err(|error| error.to_string())
}
