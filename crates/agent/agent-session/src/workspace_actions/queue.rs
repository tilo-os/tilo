//! The order a workspace's thread takes its requests in: a person's first,
//! an agent's after, and on each side what a caller waits on to read before
//! writes ([`Job::urgency`]).
//!
//! Every request of a workspace is served on one thread. Taken in the order
//! they came, an agent bringing in a tracker of two thousand issues, list after
//! list from several workers at once, put the person's next click behind all of
//! them: opening the tracker waited for every list sent before it and timed
//! out. A person's request (and the service's own work, such as a workspace
//! joined) is taken before any agent's that waits, so it waits at most for the
//! one request already running. An agent's requests fill what is left. On each
//! side the most urgent is taken first and each kind in the order it came, so
//! one caller's writes stay in their order.
//!
//! What a person does is often several requests one after another: `tilo ls`
//! lists a few levels, an open lists and then acts. Each of them came after
//! the last was answered, and the agent's next request had started in
//! between, so every one of them waited for an agent's whole request. For
//! [`PERSON_PAUSE`] after a person's request was served, an agent's request is
//! not started while nothing of the person's waits: their next request finds
//! the thread free. An agent works at full speed again once the person is
//! quiet.
//!
//! The same holds among an agent's own requests: a listing is several reads one after another,
//! and an import's next write started between two of them held the next page for the whole
//! write, so `tilo ls` during an import waited for one write per page. For [`READ_PAUSE`] after
//! an agent's read was served, no agent's write is started while nothing else waits: the
//! listing's next page finds the thread free, and waits for no write that began after its
//! first page. A write that waited [`AGENT_TURN`] goes in any case, so reads that keep coming
//! never hold writes back for good.
//!
//! An agent is never stopped for good: a person whose page asks again and
//! again would otherwise hold every agent off for as long as it stays open.
//! Once an agent's request has waited [`AGENT_TURN`] since an agent last had a
//! turn, the next request served is an agent's, so a person waits for at most
//! one agent request in each [`AGENT_TURN`] and the agents always go on.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tilo_actions::remote::Caller;

use super::Job;
use super::invocation_log::InvocationLog;
use super::job::Urgency;

/// How long after serving a person's request the thread starts no agent's: about the time a
/// person's next request of the same command takes to arrive, a round trip and the client's
/// own work on the answer.
pub(super) const PERSON_PAUSE: Duration = Duration::from_millis(300);

/// The longest agents wait for a turn while a person keeps asking.
pub(super) const AGENT_TURN: Duration = Duration::from_secs(2);

/// How long after serving an agent's read the thread starts no agent's write: about the time
/// the next read of the same listing takes to arrive, a round trip and the client's own work on
/// the page it was answered.
pub(super) const READ_PAUSE: Duration = Duration::from_millis(150);

/// Requests sent to the thread and not served yet.
#[derive(Default)]
pub(super) struct Queue {
    /// A person's requests and the service's own work, with whether serving it starts the
    /// pause ([`PERSON_PAUSE`]).
    person: VecDeque<(Instant, Job, bool)>,
    agents: VecDeque<(Instant, Job)>,
    /// When the last request a person asked was taken ([`PERSON_PAUSE`]).
    person_asked: Option<Instant>,
    /// Since when the agents wait for a turn: when an agent's request came while none waited,
    /// or an agent's was last served ([`AGENT_TURN`]).
    agents_waiting: Option<Instant>,
    /// When an agent's read was last answered ([`READ_PAUSE`]).
    agent_read: Option<Instant>,
    /// The request last taken is an agent's read: answered once the thread asks for the next.
    reading: bool,
    /// Nobody sends any more requests: the thread ends once these are served.
    closed: bool,
}

/// Who a request is served for, which decides when it is taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Asker {
    /// A person asked it: it goes first, and an agent's waits a moment after it.
    Person,
    /// The service's own work, or a page's wait for a change: it goes first, and is no ask of
    /// a person's that another follows.
    Service,
    /// An agent asked it: it fills what is left.
    Agent,
}

impl Queue {
    /// Take every request already sent.
    pub(super) fn take_sent(
        &mut self,
        jobs: &mpsc::Receiver<(Instant, Job)>,
        log: &std::sync::Mutex<InvocationLog>,
    ) {
        loop {
            match jobs.try_recv() {
                Ok(job) => self.put(job, log),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    /// Put `job` behind the others of whoever asked it.
    pub(super) fn put(&mut self, (at, job): (Instant, Job), log: &std::sync::Mutex<InvocationLog>) {
        match asker(&job, log) {
            Asker::Agent => {
                if self.agents.is_empty() {
                    self.agents_waiting = Some(Instant::now());
                }
                self.agents.push_back((at, job));
            }
            Asker::Person => self.person.push_back((at, job, true)),
            Asker::Service => self.person.push_back((at, job, false)),
        }
    }

    /// The next request to serve: a person's while one waits, else an agent's once the person
    /// has paused ([`Self::paused_until`]); an agent's first once the agents waited their longest
    /// ([`AGENT_TURN`]).
    pub(super) fn next(&mut self) -> Option<(Instant, Job)> {
        if std::mem::take(&mut self.reading) {
            self.agent_read = Some(Instant::now());
        }
        if !self.agents_due()
            && let Some(at) = most_urgent(self.person.iter().map(|(_, job, _)| job))
            && let Some((queued, job, pauses)) = self.person.remove(at)
        {
            if pauses {
                self.person_asked = Some(Instant::now());
            }
            return Some((queued, job));
        }
        if self.paused_until().is_some() {
            return None;
        }
        let at = most_urgent(self.agents.iter().map(|(_, job)| job))?;
        if self.reading_until(at).is_some() {
            return None;
        }
        let (queued, job) = self.agents.remove(at)?;
        self.reading = job.urgency() == Urgency::Read;
        self.agents_waiting = (!self.agents.is_empty()).then(Instant::now);
        Some((queued, job))
    }

    /// Until when the agent's write at `at` waits for the next read of a listing whose last
    /// page was just served ([`READ_PAUSE`]): never a read, nor a write that waited its longest
    /// ([`AGENT_TURN`]).
    fn reading_until(&self, at: usize) -> Option<Instant> {
        let (queued, job) = self.agents.get(at)?;
        if job.urgency() == Urgency::Read || queued.elapsed() >= AGENT_TURN || self.closed {
            return None;
        }
        self.agent_read?
            .checked_add(READ_PAUSE)
            .filter(|until| *until > Instant::now())
    }

    /// Whether the agents waited their longest for a turn ([`AGENT_TURN`]).
    fn agents_due(&self) -> bool {
        !self.agents.is_empty()
            && self
                .agents_waiting
                .is_some_and(|since| since.elapsed() >= AGENT_TURN)
    }

    /// Until when an agent's request waits for the person's next, or an agent's write for the
    /// next read of a listing, while one waits.
    pub(super) fn paused_until(&self) -> Option<Instant> {
        if self.agents.is_empty() || self.closed {
            return None;
        }
        let person = (!self.agents_due())
            .then(|| self.person_asked?.checked_add(PERSON_PAUSE))
            .flatten()
            .filter(|until| *until > Instant::now());
        person.or_else(|| {
            most_urgent(self.agents.iter().map(|(_, job)| job))
                .and_then(|at| self.reading_until(at))
        })
    }

    /// Nobody sends any more requests, and every one sent was served.
    pub(super) fn closed(&self) -> bool {
        self.closed && self.person.is_empty() && self.agents.is_empty()
    }

    /// Nobody sends any more requests.
    pub(super) const fn close(&mut self) {
        self.closed = true;
    }
}

/// Where among `jobs` the most urgent is, the oldest of its kind.
fn most_urgent<'a>(jobs: impl Iterator<Item = &'a Job> + Clone) -> Option<usize> {
    let most = jobs.clone().map(Job::urgency).min()?;
    jobs.into_iter().position(|job| job.urgency() == most)
}

/// Whether `caller` was admitted as an agent.
fn is_agent(caller: &Caller) -> bool {
    caller.acting().is_agent()
}

/// Who `job` is served for: an agent's is served after every person's request that waits. Who
/// asked decides, never what is asked: a listing an agent asks for waits like its writes do. An
/// invocation is an agent's too where it says it is a step of an agent's session: a `tilo mcp`
/// on the person's computer asks through that computer's own session, as the person's device.
/// The order only decides who waits; it grants nothing. The service's own bookkeeping (a
/// workspace joined, a deleted one let go, a command that ended) and a page's wait for a change
/// go first without holding the agents back; what the service does after a write, reading an
/// item's name again for its app's index, goes with the agents' work.
fn asker(job: &Job, log: &std::sync::Mutex<InvocationLog>) -> Asker {
    let by = |agent: bool| if agent { Asker::Agent } else { Asker::Person };
    match job {
        Job::Invoke { invocation, .. } => {
            let log = tilo_lock::MutexRecover::lock_recover(log);
            by(log.entries.get(invocation).is_some_and(|entry| {
                is_agent(&entry.caller)
                    || entry
                        .request
                        .context
                        .as_ref()
                        .and_then(|context| context.group.as_ref())
                        .is_some_and(|group| {
                            group.kind == tilo_actions::context::AGENT_SESSION_GROUP
                        })
            }))
        }
        Job::Feedback { caller, .. }
        | Job::Catalogue { caller, .. }
        | Job::Content { caller, .. }
        | Job::ContentGet { caller, .. }
        | Job::ContentLink { caller, .. }
        | Job::ReadDocument { caller, .. }
        | Job::Search { caller, .. } => by(is_agent(caller)),
        Job::Changes { caller, .. }
        | Job::Invocations { caller, .. }
        | Job::Changed { caller, .. }
        | Job::Arrived { caller, .. } => {
            if is_agent(caller) {
                Asker::Agent
            } else {
                Asker::Service
            }
        }
        Job::Rename(..) => Asker::Agent,
        Job::Ran(_) | Job::Joined(_) | Job::Forget(_) => Asker::Service,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_peer_state::document_frame::DocumentRoute;
    use tilo_server_protocol::WorkspaceId;

    /// The service's own work that came after a write, and the names the writes left to read,
    /// are taken in order of urgency; writes keep their order among themselves.
    #[test]
    fn what_a_caller_waits_on_goes_before_queued_writes() {
        let workspace = WorkspaceId::parse("6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b").unwrap();
        let other = WorkspaceId::parse("0b5e7c1d-2e3f-4a5b-8c6d-7e8f9a0b1c2d").unwrap();
        let route = DocumentRoute {
            app_id: "tasks".into(),
            cluster_id: workspace.to_string(),
            document: "tracker".into(),
        };
        let log = std::sync::Mutex::default();
        let now = Instant::now();
        let mut queue = Queue::default();
        for job in [
            Job::Rename(workspace, route),
            Job::Forget(workspace),
            Job::Forget(other),
            Job::Joined(workspace),
        ] {
            queue.put((now, job), &log);
        }
        let taken: Vec<String> = std::iter::from_fn(|| queue.next())
            .map(|(_, job)| match job {
                Job::Forget(id) => format!("forget {id}"),
                job => job.kind().to_owned(),
            })
            .collect();
        assert_eq!(
            taken,
            [
                "joined".to_owned(),
                format!("forget {workspace}"),
                format!("forget {other}"),
                "rename".to_owned(),
            ]
        );
        assert!(queue.next().is_none());
    }
}
