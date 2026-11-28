//! Telling people outside Tilo what their assistant told them.
//!
//! What a person's assistant tells them is one log on this host
//! ([`Workspaces::tell`]), each thing marked by what kind of thing it is. A
//! device of theirs that runs reads it live and raises its own notice. Here
//! is what reaches a person no device of theirs is looking for: a thing that
//! is to reach them, was not acknowledged, and is a few seconds old is told
//! once through whatever tells outside Tilo ([`Outside`]: the browsers they
//! subscribed, today), and that it was is written down on the thing. A
//! thing that must not be missed is raised again until they acknowledge it,
//! and told outside again each time nothing of theirs is looking.
//! Everything else stays in the thread and in the log, and nothing is told
//! outside for it.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::ResourceWake;
use tilo_invocations::Recorded;
use tilo_person_services::PersonServices;
use tilo_person_services::person_telling::Told;
use tilo_workspace_service::agent_shortcuts::{ANSWER, agent_of};

use super::triggered::{answered, said};

use tilo_lock::MutexRecover as _;

use tilo_workspace_service::workspaces::Workspaces;

/// The longest the teller sleeps when nothing is due: what it would miss is
/// only a log it was not told had moved.
const IDLE: Duration = Duration::from_secs(60);
/// The shortest it sleeps, so a due time in the past never spins it.
const SOONEST: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LivePhase {
    #[default]
    Working,
    Asking,
    Answered,
    Stopped,
    Failed,
    Cancelled,
}

impl LivePhase {
    pub const fn is_over(self) -> bool {
        !matches!(self, Self::Working | Self::Asking)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveTurn {
    pub id: String,
    pub workspace: String,
    pub conversation: String,
    pub agent: String,
    pub asked: String,
    pub phase: LivePhase,
    pub step: u32,
    pub steps: u32,
}

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
    fn live(&self, _person: [u8; 32], _turn: LiveTurn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

pub fn words_outside(told: &Told) -> (String, String) {
    tilo_person_services::person_telling::words_outside(told)
}

fn person_of(record: &Recorded) -> Option<[u8; 32]> {
    let person = |caller: &tilo_actions::remote::Caller| {
        caller
            .acting()
            .person_id()
            .or_else(|| caller.acting_for_person())
            .and_then(|text| {
                tilo_failure::ignored!(
                    "a person's id that does not read names no device to tell",
                    hex::decode(text).ok()
                )
            })
            .and_then(|bytes| bytes.try_into().ok())
            .or_else(|| caller.user_public_key().copied())
    };
    person(&record.caller).or_else(|| person(&record.asked))
}

fn live_of(record: &Recorded) -> Option<LiveTurn> {
    let request = &record.request;
    let target = &request.target.address;
    if target.action != ANSWER {
        return None;
    }
    let (place, event) = answered(request)?;
    let state = record.state.as_ref()?;
    let (phase, step, steps) = match &state.update {
        InvocationUpdate::Progress {
            phase,
            completed,
            total,
            ..
        } => (
            if phase == "asking" {
                LivePhase::Asking
            } else {
                LivePhase::Working
            },
            u32::try_from(completed.unwrap_or(0)).unwrap_or(u32::MAX),
            u32::try_from(total.unwrap_or(0)).unwrap_or(u32::MAX),
        ),
        InvocationUpdate::Accepted { .. } => (LivePhase::Working, 0, 0),
        InvocationUpdate::Succeeded { .. } => (LivePhase::Answered, 0, 0),
        InvocationUpdate::Failed { .. } => (LivePhase::Failed, 0, 0),
        InvocationUpdate::Cancelled { .. } => (LivePhase::Cancelled, 0, 0),
        InvocationUpdate::Item { .. } => return None,
    };
    Some(LiveTurn {
        id: request.invocation.to_string(),
        workspace: record.workspace.clone()?,
        conversation: place.conversation(),
        agent: record
            .asked
            .agent_called()
            .or_else(|| agent_of(&target.resource))
            .map_or_else(|| target.resource.clone(), str::to_owned),
        asked: said(&event),
        phase,
        step,
        steps,
    })
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
        if !PersonServices::of(&self.workspaces).claim_telling_outside() {
            return;
        }
        // What waited while this host was down is still owed.
        let known = PersonServices::of(&self.workspaces).people_told();
        self.waited.lock_recover().extend(known);
        let mut moved = PersonServices::of(&self.workspaces).told_changes();
        loop {
            self.live().await;
            let now_ms = tilo_clock::unix_now_ms();
            let wait = self.step(now_ms).await.map_or(IDLE, |at| {
                Duration::from_millis(at.saturating_sub(now_ms)).clamp(SOONEST, IDLE)
            });
            let waking = {
                let Some(invocations) = PersonServices::of(&self.workspaces).invocations() else {
                    return;
                };
                let described = self.workspaces.described().await;
                let mut waits: Vec<Pin<Box<tilo_actions::remote::Pending<()>>>> = Vec::new();
                for workspace in &described {
                    let (wake, waiting) = ResourceWake::new();
                    invocations.watch(&workspace.id().to_string(), &wake);
                    waits.push(Box::pin(waiting));
                }
                if waits.is_empty() {
                    tokio::time::sleep(IDLE).await;
                    continue;
                }
                async move {
                    drop(futures::future::select_all(waits).await);
                }
            };
            tokio::select! {
                changed = moved.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(wait) => {}
                () = waking => {}
            }
        }
    }

    pub async fn step(&self, now_ms: u64) -> Option<u64> {
        let people: Vec<[u8; 32]> = {
            let mut waited = self.waited.lock_recover();
            waited.extend(PersonServices::of(&self.workspaces).told_changed_people());
            waited.iter().copied().collect()
        };
        let services = PersonServices::of(&self.workspaces);
        let mut next: Option<u64> = None;
        for person in people {
            let mut waits = false;
            // What was told outside Tilo and waits no more.
            let mut over: Vec<Told> = Vec::new();
            for told in services.told_after(&person, 0).told {
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
        if PersonServices::of(&self.workspaces).looking(person, self.pace.watching) {
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
        let services = PersonServices::of(&self.workspaces);
        let raised = match services.raise_told(&person, id, now_ms) {
            Ok(raised) => raised,
            Err(error) => {
                tracing::warn!(%error, "a thing that insists was not raised again");
                return None;
            }
        };
        let next = raised.insists.and_then(|insists| insists.next_at_ms);
        if raised.waits() && !services.looking(&person, self.pace.watching) {
            self.tell(person, raised).await;
        }
        next
    }

    pub async fn live(&self) {
        let Some(invocations) = PersonServices::of(&self.workspaces).invocations() else {
            return;
        };
        for workspace in self.workspaces.described().await {
            for record in invocations.listed(&workspace.id().to_string()) {
                let (Some(person), Some(turn)) = (person_of(&record), live_of(&record)) else {
                    continue;
                };
                for outside in &self.outside {
                    outside.live(person, turn.clone()).await;
                }
            }
        }
    }

    /// Tell `person` the thing `told` outside Tilo, every way this teller
    /// has, and write down on the thing that it was, once for each.
    async fn tell(&self, person: [u8; 32], told: Told) {
        let (id, kind) = (told.id.clone(), told.kind);
        let services = PersonServices::of(&self.workspaces);
        for outside in &self.outside {
            let took = outside.tell(person, told.clone()).await;
            if let Err(error) = services.told_outside(&person, &id, outside.name(), took) {
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

#[cfg(test)]
mod tests;
