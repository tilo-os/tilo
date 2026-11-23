//! Telling people outside Tilo what their assistant told them.
//!
//! What a person's assistant tells them is one log on this host
//! ([`Workspaces::tell`]), each thing marked by the person's own rule. A
//! device of theirs that runs reads it live and raises its own notice. Here
//! is what reaches a person no device of theirs is looking for: a thing that
//! is to reach them, was not acknowledged, and is a few seconds old is told
//! once through whatever tells outside Tilo ([`Outside`]: the browsers they
//! subscribed, today), and that it was is written down on the thing. A
//! thing that must not be missed is raised again by its person's own
//! rhythm until they acknowledge it, and told outside again each time
//! nothing of theirs is looking. Everything else stays in the thread and in
//! the log, and nothing is told outside for it.
//!
//! This is also where a judgement made for a person finds whose day it is
//! counted on: the assistant that person keeps in the workspace
//! ([`count_judgements`]), and where how a turn that a cause started for
//! them ended joins their log by itself.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::person_telling::{Kind, Told};
use tilo_lock::MutexRecover as _;
use tilo_workspace_agent::{Agent, WorkspaceAgents};

use crate::workspaces::person_model::Spender;
use crate::workspaces::{Spenders, WeakWorkspaces, Workspaces};

mod caused;
pub(crate) use caused::turn_ended;

/// The longest the teller sleeps when nothing is due: what it would miss is
/// only a log it was not told had moved.
const IDLE: Duration = Duration::from_secs(60);
/// The shortest it sleeps, so a due time in the past never spins it.
const SOONEST: Duration = Duration::from_millis(250);

/// What tells a person outside every Tilo of theirs.
pub trait Outside: Send + Sync {
    /// What it is called where a telling is written down on the thing told.
    fn name(&self) -> &'static str;
    /// Tell `person` the thing `told`. Answers how many of their devices
    /// took it.
    fn tell(&self, person: [u8; 32], told: Told) -> Pin<Box<dyn Future<Output = u32> + Send>>;
    /// The things `ids`, which were told `person` here, wait for them no
    /// more: what showed one on a device of theirs may take it back.
    /// Nothing is taken back unless whatever tells says how.
    fn taken_back(
        &self,
        _person: [u8; 32],
        ids: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        drop(ids);
        Box::pin(async {})
    }
}

/// What a telling outside Tilo says of `told`: its title and its body. The
/// words are the thing's own where they may leave the host; where a person
/// keeps them on the host, the body says only where to look.
#[must_use]
pub fn words_outside(told: &Told) -> (String, String) {
    let name = told
        .from
        .as_ref()
        .map(|from| from.name.trim())
        .filter(|name| !name.is_empty());
    let title = match (told.kind, name) {
        (Kind::Question, Some(name)) => format!("{name} needs you"),
        (Kind::Question, None) => "An agent needs you".to_owned(),
        (Kind::Mention, Some(name)) => format!("{name} mentioned you"),
        (_, Some(name)) => name.to_owned(),
        (_, None) => "Tilo".to_owned(),
    };
    let body = if told.outside {
        told.words.clone()
    } else {
        "Open Tilo to see it.".to_owned()
    };
    (title, body)
}

/// When a thing is told outside Tilo.
#[derive(Clone, Copy, Debug)]
pub struct Pace {
    /// How long a thing waits first: a device that is open hears of it
    /// within its next look.
    pub after: Duration,
    /// How lately a device of the person must have looked for what they
    /// were told to count as looking: a page asks every few seconds, and a
    /// device that waits for news counts for as long as it waits.
    pub watching: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            after: Duration::from_secs(4),
            watching: Duration::from_secs(8),
        }
    }
}

/// When one thing is due to be told outside Tilo.
enum Due {
    /// Not any more: it was told there.
    Never,
    /// At this time, in milliseconds since the epoch, or it is looked at
    /// again then.
    At(u64),
    Now,
    /// Now, again: it insists and its time has come.
    Again,
}

fn millis(time: Duration) -> u64 {
    u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
}

/// Tells people outside Tilo what waits for them in the logs a host keeps.
pub struct Teller {
    workspaces: Workspaces,
    outside: Vec<Arc<dyn Outside>>,
    pace: Pace,
    /// The people something still waits for.
    waited: Mutex<BTreeSet<[u8; 32]>>,
}

impl Teller {
    /// A teller of what `workspaces` keeps, through `outside`.
    #[must_use]
    pub fn new(workspaces: Workspaces, outside: Arc<dyn Outside>) -> Self {
        Self {
            workspaces,
            outside: vec![outside],
            pace: Pace::default(),
            waited: Mutex::default(),
        }
    }

    /// This teller, telling through `outside` as well: a thing that is
    /// due is told every way the teller has.
    #[must_use]
    pub fn also(mut self, outside: Arc<dyn Outside>) -> Self {
        self.outside.push(outside);
        self
    }

    /// This teller, telling at `pace`.
    #[must_use]
    pub const fn pacing(mut self, pace: Pace) -> Self {
        self.pace = pace;
        self
    }

    /// Tell what is due now, then again whenever a log moves or the next
    /// thing is due, until the host stops. One teller tells for a host:
    /// a second one started for the same workspaces ends at once.
    pub async fn run(self) {
        if !self.workspaces.claim_telling_outside() {
            return;
        }
        // What waited while this host was down is still owed.
        let known = self.workspaces.people_told();
        self.waited.lock_recover().extend(known);
        let mut moved = self.workspaces.told_changes();
        loop {
            let now_ms = tilo_clock::unix_now_ms();
            let wait = self.step(now_ms).await.map_or(IDLE, |at| {
                Duration::from_millis(at.saturating_sub(now_ms)).clamp(SOONEST, IDLE)
            });
            tokio::select! {
                changed = moved.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Tell everything that is due at `now_ms`, in milliseconds since the
    /// epoch, and answer when the next thing is: none while nothing waits
    /// to be told.
    pub async fn step(&self, now_ms: u64) -> Option<u64> {
        let people: Vec<[u8; 32]> = {
            let mut waited = self.waited.lock_recover();
            waited.extend(self.workspaces.told_changed_people());
            waited.iter().copied().collect()
        };
        let mut next: Option<u64> = None;
        for person in people {
            let mut waits = false;
            // What was told outside Tilo and waits no more.
            let mut over: Vec<Told> = Vec::new();
            for told in self.workspaces.told_after(&person, 0).told {
                if !told.waits() {
                    if !told.pushed.is_empty() {
                        over.push(told);
                    }
                    continue;
                }
                match self.due(&person, &told, now_ms) {
                    Due::Never => {}
                    Due::At(at) => {
                        waits = true;
                        next = Some(next.map_or(at, |soonest| soonest.min(at)));
                    }
                    Due::Now => {
                        // What insists comes round again.
                        if let Some(at) = told.insists.and_then(|insists| insists.next_at_ms) {
                            waits = true;
                            next = Some(next.map_or(at, |soonest| soonest.min(at)));
                        }
                        self.tell(person, told).await;
                    }
                    Due::Again => {
                        if let Some(at) = self.raise(person, &told.id, now_ms).await {
                            waits = true;
                            next = Some(next.map_or(at, |soonest| soonest.min(at)));
                        }
                    }
                }
            }
            self.take_back(person, &over).await;
            if !waits {
                self.waited.lock_recover().remove(&person);
            }
        }
        next
    }

    /// When `told`, which waits for `person`, is due.
    fn due(&self, person: &[u8; 32], told: &Told, now_ms: u64) -> Due {
        let again = told.insists.and_then(|insists| insists.next_at_ms);
        if again.is_some_and(|at| at <= now_ms) {
            return Due::Again;
        }
        let first = self.first_due(person, told, now_ms);
        match (first, again) {
            (Due::Never, Some(at)) => Due::At(at),
            (Due::At(first), Some(at)) => Due::At(first.min(at)),
            (first, _) => first,
        }
    }

    /// When `told` is due to be told outside Tilo the first time.
    fn first_due(&self, person: &[u8; 32], told: &Told, now_ms: u64) -> Due {
        if !told.pushed.is_empty() {
            return Due::Never;
        }
        let at = told.at_ms.saturating_add(millis(self.pace.after));
        if now_ms < at {
            return Due::At(at);
        }
        if self.workspaces.looking(person, self.pace.watching) {
            // A device of theirs that runs shows it. It is looked at again
            // once that device may have stopped looking.
            return Due::At(now_ms.saturating_add(millis(self.pace.watching)));
        }
        Due::Now
    }

    /// Raise the thing `id`, which insists, again for `person`: a device of
    /// theirs that runs raises its notice again by itself, and with none
    /// looking it is told outside again. Answers when it comes round next.
    async fn raise(&self, person: [u8; 32], id: &str, now_ms: u64) -> Option<u64> {
        let raised = match self.workspaces.raise_told(&person, id, now_ms) {
            Ok(raised) => raised,
            Err(error) => {
                tracing::warn!(%error, "a thing that insists was not raised again");
                return None;
            }
        };
        let next = raised.insists.and_then(|insists| insists.next_at_ms);
        if raised.waits() && !self.workspaces.looking(&person, self.pace.watching) {
            self.tell(person, raised).await;
        }
        next
    }

    /// Tell `person` the thing `told` outside Tilo, every way this teller
    /// has, and write down on the thing that it was, once for each.
    async fn tell(&self, person: [u8; 32], told: Told) {
        let (id, kind) = (told.id.clone(), told.kind);
        for outside in &self.outside {
            let took = outside.tell(person, told.clone()).await;
            if let Err(error) = self
                .workspaces
                .told_outside(&person, &id, outside.name(), took)
            {
                tracing::warn!(%error, "what a person was told outside Tilo is not written down");
            }
            tracing::info!(
                took,
                ?kind,
                by = outside.name(),
                "a person with no Tilo open was told what waits for them"
            );
        }
    }

    /// The things `over` were told `person` outside Tilo and wait for them
    /// no more: whatever told each may take it back.
    async fn take_back(&self, person: [u8; 32], over: &[Told]) {
        for outside in &self.outside {
            let ids: Vec<String> = over
                .iter()
                .filter(|told| told.pushed.iter().any(|pushed| pushed.by == outside.name()))
                .map(|told| told.id.clone())
                .collect();
            if !ids.is_empty() {
                outside.taken_back(person, ids).await;
            }
        }
    }
}

/// Whose day the judgements made for a person are counted on: the agents
/// that person keeps, read from the workspace's own record of its agents.
struct Assistants {
    workspaces: WeakWorkspaces,
}

/// The person `agent` acts for, by their root.
fn person_of(agent: &Agent) -> Option<[u8; 32]> {
    tilo_failure::ignored!(
        "an agent kept by nobody's person has nobody to tell",
        tilo_action_policy::PersonId::parse(&agent.acts_for).ok()
    )
    .and_then(|acts_for| acts_for.root())
}

/// The agents `workspace` keeps, or none where its record does not read.
async fn agents_of(workspaces: &Workspaces, workspace: &str) -> Option<Vec<Agent>> {
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let bytes = workspaces
        .resource_document(workspace, tilo_workspace_agent::OWNER, &record)
        .await
        .ok()??;
    let mut doc = automerge::AutoCommit::load(&bytes).ok()?;
    let agents = crate::agents::opened::<WorkspaceAgents>(&mut doc).ok()?;
    Some(agents.agents.iter().cloned().collect())
}

/// The agent among `agents` whose day a judgement for `person` is counted
/// on: the agent `told_by` that told the thing, where that person keeps it
/// and it still works; else the assistant they keep, which is the first
/// made of their working agents that no MCP client connects as. None where
/// they keep no such agent: somebody else's agent is never theirs.
fn counted_on<'a>(
    agents: &'a [Agent],
    person: &[u8; 32],
    told_by: Option<&str>,
) -> Option<&'a Agent> {
    let kept = |agent: &&Agent| agent.is_active() && person_of(agent) == Some(*person);
    told_by
        .and_then(|id| agents.iter().filter(kept).find(|agent| agent.id.0 == id))
        .or_else(|| {
            agents
                .iter()
                .filter(kept)
                .filter(|agent| agent.mcp_client.is_none())
                .min_by_key(|agent| agent.created_at)
        })
}

impl Spenders for Assistants {
    fn spender<'a>(
        &'a self,
        person: &'a [u8; 32],
        workspace: &'a str,
        agent: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<Spender>> + Send + 'a>> {
        Box::pin(async move {
            let workspaces = self.workspaces.upgrade()?;
            let agents = agents_of(&workspaces, workspace).await?;
            let assistant = counted_on(&agents, person, agent)?;
            Some(Spender {
                workspace: workspace.to_owned(),
                agent: assistant.id.0.clone(),
                name: assistant.name.clone(),
                // The day as the agents' own runtime counts it.
                day: tilo_clock::local::today().to_string(),
                limit_micros: assistant.spend_limit_micros,
            })
        })
    }
}

/// Count the judgements `workspaces` makes for its people on the day of
/// the assistant each of them keeps, within the limit they set it. Until
/// this is called no judgement is made: no model is asked for a call that
/// no day counts.
pub fn count_judgements(workspaces: &Workspaces) {
    workspaces.count_judgements_with(Arc::new(Assistants {
        workspaces: workspaces.downgrade(),
    }));
}

/// Start what a host does for what its people are told, beside telling
/// them outside Tilo: count their judgements on their assistants' days.
pub fn start(workspaces: &Workspaces) {
    count_judgements(workspaces);
}

#[cfg(test)]
mod tests;
