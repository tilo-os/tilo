//! The invocations of the system's dispatch an instance's views show, and the tilolang lines the
//! person runs from them.
//!
//! A view reads an invocation by its id (`invoked(id)`): an agent's turn a reply keeps, each step
//! it made, a question it waits on. Nothing of it is stored in the app's record: the instance
//! reads the invocation's record from the workspace's invocations (`tilo.invocations`) through
//! the record API, on its own connection, as the caller it runs for, and
//! reads it again while it runs or waits, so the view moves as the invocation does. What a
//! person does to one (stop it, run it again, answer what it asks) is an action on its record
//! (`host.cancel`, `host.retry`, `host.resolve`), sent through the dispatch like any call.
//!
//! A view reads an engine job by its path (`engine.job(path)`, `devices/<device>/jobs/<job>`) the
//! same way: its record is the device's, read through the record API as the caller the instance
//! runs for, held open until the job moves again, and read no more once it ended
//! ([`Invocations::follow_job`]). A job this process ran with no device to hand it to is read
//! where it runs.
//!
//! A line the person runs (`run_line(r, line)`) is their own step: the session's program host
//! runs it as them (`session:programs#run-line`), every call it makes is theirs, and what it
//! answers is shown where they ran it (`lines_run(r)`) while the window is open. What it changed
//! is in the records it changed, credited to them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use collaboration::{CollabEvent, CollabRequest};
use serde_json::{Value, json};
use tilo_action_services::workspace_invocations::{self, Invocation};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
    ResourceReadRequest,
};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::platform::call_place;
use tilolang_tilo::runtime::value::ResourceRef;
use tilolang_tilo::runtime::{Invoked, LineRun};
use ui_core::time::{Duration, Instant};

/// How often the record of an invocation that runs or waits is read again.
const READ_AGAIN: Duration = Duration::from_millis(1000);
/// How long a read may go unanswered before it is asked again.
const READ_LOST: Duration = Duration::from_secs(10);

/// What the instance knows of the invocations its views read, and of the lines the person ran.
pub struct Invocations {
    /// The workspace the instance's record is in: where the invocations are read, and what their
    /// records are addressed by.
    workspace: String,
    /// Each invocation a view read, by the id it read it by.
    followed: BTreeMap<String, Followed>,
    /// The lines in flight, by the invocation they went out as.
    asked: HashMap<InvocationId, Asked>,
    /// The reads in flight, by their correlation: the invocation a view reads that each asks for.
    reads: BTreeMap<u64, String>,
    /// The next correlation a read is asked under.
    next_correlation: u64,
    /// The lines the person ran, by the place they ran them from, oldest first.
    lines: BTreeMap<String, Vec<LineRun>>,
    /// Places whose lines moved since the views were told.
    lines_moved: BTreeSet<String>,
    /// What waits to be sent.
    queued: Vec<RemoteActionMessage>,
    /// Each engine job a view read, by its path.
    jobs: BTreeMap<String, FollowedJob>,
    /// The job reads in flight, by their correlation: the path of the job each asks for.
    job_reads: BTreeMap<u64, String>,
}

/// How long a read of a job that has not ended waits for its next move.
const JOB_WAIT: Duration = Duration::from_secs(10);

/// One engine job a view reads.
#[derive(Default)]
struct FollowedJob {
    /// Where its record is (`devices/<device>`, and its id there); `None` for a job this process
    /// runs with no device, read where it runs.
    address: Option<(String, String)>,
    /// Its record as last read, and the revision it was read at.
    record: Option<Value>,
    revision: Option<String>,
    /// When it was last asked for, and whether an answer is still awaited.
    asked_at: Option<Instant>,
    waiting: bool,
    /// The views were told what was read last.
    told: bool,
}

impl FollowedJob {
    /// Whether the job it reads has ended: it moves no more.
    fn ended(&self) -> bool {
        self.record
            .as_ref()
            .and_then(|record| record.get("state"))
            .and_then(Value::as_str)
            .is_some_and(|state| matches!(state, "done" | "failed" | "cancelled"))
    }
}

/// One invocation a view reads.
#[derive(Default)]
struct Followed {
    /// Its id on the dispatch, as the id the view read it by names it; `None` for one that names
    /// no invocation, which reads as nothing.
    id: Option<InvocationId>,
    /// Its record as last read; `None` until it is, and for one the workspace holds none of.
    read: Option<Invoked>,
    /// When it was last asked for, and whether an answer is still awaited.
    asked_at: Option<Instant>,
    waiting: bool,
    /// The views were told what was read last.
    told: bool,
}

/// What a call in flight was for.
enum Asked {
    /// The line run from the place, the `at`th of its lines.
    Line { place: String, at: usize },
}

impl Invocations {
    /// The invocations an instance of `workspace` reads.
    #[must_use]
    pub fn new(workspace: &str) -> Self {
        Self {
            workspace: workspace.to_owned(),
            followed: BTreeMap::new(),
            asked: HashMap::new(),
            reads: BTreeMap::new(),
            next_correlation: 1,
            lines: BTreeMap::new(),
            lines_moved: BTreeSet::new(),
            queued: Vec::new(),
            jobs: BTreeMap::new(),
            job_reads: BTreeMap::new(),
        }
    }

    /// A view reads the engine job `path`: its record is read from now on, as it moves, until it
    /// ends. A job of this device's is read through the record API; one this process runs with no
    /// device, where it runs.
    pub fn follow_job(&mut self, path: &str) {
        if path.is_empty() {
            // A view that holds no job yet reads nothing.
            return;
        }
        let address = crate::engines::recorded()
            .then(|| tilo_actions::remote::parse_job_path(path))
            .flatten()
            .map(|(device, id)| (tilo_actions::remote::device_record_space(&device), id));
        self.jobs
            .entry(path.to_owned())
            .or_insert_with(|| FollowedJob {
                address,
                ..FollowedJob::default()
            });
    }

    /// Ask for each job record that is due: one never read, and one that has not ended once its
    /// last read answered. A read of one that was read waits for its next move.
    fn follow_jobs(&mut self) {
        let now = ui_core::time::now();
        let mut due = Vec::new();
        for (path, followed) in &mut self.jobs {
            let Some((space, id)) = followed.address.clone() else {
                // Read where it runs: told whenever it moved since.
                let record = tilolang_tilo::engine::job(path);
                if record != followed.record {
                    followed.record = record;
                    followed.told = false;
                }
                continue;
            };
            if followed.ended() {
                continue;
            }
            let lost = followed.asked_at.is_some_and(|at| {
                now.saturating_duration_since(at) >= JOB_WAIT.saturating_add(READ_LOST)
            });
            if followed.waiting && !lost {
                continue;
            }
            followed.asked_at = Some(now);
            followed.waiting = true;
            due.push((path.clone(), space, id, followed.revision.clone()));
        }
        for (path, workspace, record, after) in due {
            let correlation = self.next_correlation;
            self.next_correlation = self.next_correlation.saturating_add(1);
            self.job_reads.insert(correlation, path);
            let wait_ms = after
                .is_some()
                .then(|| u32::try_from(JOB_WAIT.as_millis()).unwrap_or(u32::MAX));
            self.queued.push(RemoteActionMessage::ResourceRead {
                correlation,
                on_behalf_of: None,
                read: ResourceReadRequest {
                    workspace,
                    app: tilo_actions::remote::DEVICE_JOBS.to_owned(),
                    record,
                    joined: false,
                    after,
                    wait_ms,
                    answer: tilo_actions::remote::ReadAnswer::Reading,
                },
            });
        }
    }

    /// Take the reading `result` of the job `correlation` asked for.
    fn read_job(&mut self, correlation: u64, result: Option<&String>) -> bool {
        let Some(path) = self.job_reads.remove(&correlation) else {
            return false;
        };
        let Some(followed) = self.jobs.get_mut(&path) else {
            return true;
        };
        followed.waiting = false;
        let Some(text) = result else {
            // A read the device refused says nothing of the job: it is read again.
            tracing::debug!(%path, "a job's record was not read");
            return true;
        };
        let readings = match serde_json::from_str::<Value>(text) {
            Ok(readings) => readings,
            Err(why) => {
                tracing::warn!(%path, %why, "a job's reading is not JSON: it is read again");
                return true;
            }
        };
        followed.revision = readings
            .get("revision")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let record = readings.pointer("/records/0/value").cloned();
        if let Some(record) = &record {
            crate::engines::read_record(&path, record);
        }
        if record.is_some() && record != followed.record {
            followed.record = record;
            followed.told = false;
        }
        true
    }

    /// How long until a record that runs or waits is due to be read again: `None` when no view
    /// reads one.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        let now = ui_core::time::now();
        self.followed
            .values()
            .filter(|followed| followed.id.is_some())
            .filter(|followed| {
                followed
                    .read
                    .as_ref()
                    .is_none_or(|read| matches!(read.state.as_str(), "running" | "waiting"))
            })
            .filter_map(|followed| {
                let after = if followed.waiting {
                    READ_LOST
                } else {
                    READ_AGAIN
                };
                followed.asked_at?.checked_add(after)
            })
            .min()
            .map(|due| due.saturating_duration_since(now))
    }

    /// Run `line`, which the person typed in `target`, as their own step.
    pub fn run_line(&mut self, target: &ResourceRef, line: &str) {
        let place = call_place(target);
        let lines = self.lines.entry(place.clone()).or_default();
        let at = lines.len();
        lines.push(LineRun {
            line: line.to_owned(),
            running: true,
            said: String::new(),
            failed: false,
        });
        self.lines_moved.insert(place.clone());
        let request = request(
            tilo_action_services::session_actions::run_line_target(),
            &json!({ "line": line }),
        );
        self.asked
            .insert(request.invocation, Asked::Line { place, at });
        self.queued.push(RemoteActionMessage::Invoke(request));
    }

    /// Follow every invocation `instance`'s views read now, and ask for the records that are due:
    /// one never read, and one that runs or waits once [`READ_AGAIN`] passed.
    pub fn follow(&mut self, instance: &Instance) {
        self.follow_jobs();
        for read in instance.invoked_read() {
            self.followed
                .entry(read.clone())
                .or_insert_with(|| Followed {
                    id: invocation_in(&read),
                    ..Followed::default()
                });
        }
        let now = ui_core::time::now();
        let mut due = Vec::new();
        for (read, followed) in &mut self.followed {
            let Some(id) = followed.id else {
                continue;
            };
            let since = followed
                .asked_at
                .map(|at| now.saturating_duration_since(at));
            let settled = followed
                .read
                .as_ref()
                .is_some_and(|read| !matches!(read.state.as_str(), "running" | "waiting"));
            let ask = match since {
                None => true,
                Some(_) if settled => false,
                Some(since) if followed.waiting => since >= READ_LOST,
                Some(since) => since >= READ_AGAIN,
            };
            if ask {
                followed.asked_at = Some(now);
                followed.waiting = true;
                due.push((read.clone(), id));
            }
        }
        for (read, id) in due {
            let correlation = self.next_correlation;
            self.next_correlation = self.next_correlation.saturating_add(1);
            self.reads.insert(correlation, read);
            self.queued.push(RemoteActionMessage::ResourceRead {
                correlation,
                on_behalf_of: None,
                read: ResourceReadRequest {
                    workspace: self.workspace.clone(),
                    app: workspace_invocations::PROVIDER.to_owned(),
                    record: id.0.to_string(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                    answer: tilo_actions::remote::ReadAnswer::Reading,
                },
            });
        }
    }

    /// Send what is queued with `send`. A request the connection does not take is asked again on
    /// a later frame: a read when it is due again, a line never (it says it did not run).
    pub fn say(&mut self, mut send: impl FnMut(CollabRequest) -> bool) {
        for message in std::mem::take(&mut self.queued) {
            let (invocation, correlation) = match &message {
                RemoteActionMessage::Invoke(request) => (Some(request.invocation), None),
                RemoteActionMessage::ResourceRead { correlation, .. } => (None, Some(*correlation)),
                _ => (None, None),
            };
            if send(CollabRequest::RemoteAction {
                message,
                press: None,
            }) {
                continue;
            }
            if let Some(invocation) = invocation {
                if let Some(Asked::Line { place, at }) = self.asked.remove(&invocation) {
                    self.said(&place, at, "The line could not be sent: try again.", true);
                }
            } else if let Some(correlation) = correlation {
                if let Some(read) = self.reads.remove(&correlation)
                    && let Some(followed) = self.followed.get_mut(&read)
                {
                    followed.waiting = false;
                }
                if let Some(path) = self.job_reads.remove(&correlation)
                    && let Some(followed) = self.jobs.get_mut(&path)
                {
                    followed.waiting = false;
                }
            }
        }
    }

    /// Take `event` when it answers one of this instance's reads or lines. Whether it did.
    pub fn take(&mut self, event: &CollabEvent) -> bool {
        let CollabEvent::RemoteAction { message } = event else {
            return false;
        };
        if let RemoteActionMessage::ResourceReading {
            correlation,
            result,
        } = message
        {
            if !self.read_job(*correlation, result.as_ref().ok()) {
                self.read(*correlation, result.as_ref().ok());
            }
            return true;
        }
        let receipt = match message {
            RemoteActionMessage::Receipt(receipt) => receipt.clone(),
            RemoteActionMessage::Update {
                invocation,
                state: Some(state),
                ..
            } => InvocationReceipt {
                invocation: *invocation,
                status: state.receipt().status,
            },
            _ => return false,
        };
        if !self.asked.contains_key(&receipt.invocation) {
            return false;
        }
        // Admitted and running: its answer comes when it ends.
        if matches!(receipt.status, ReceiptStatus::Accepted) {
            return true;
        }
        match self.asked.remove(&receipt.invocation) {
            Some(Asked::Line { place, at }) => {
                let (said, failed) = line_answer(&receipt);
                self.said(&place, at, &said, failed);
            }
            None => {}
        }
        true
    }

    /// Tell `instance`'s views what moved: each invocation read since, and each place's lines.
    pub fn show(&mut self, instance: &mut Instance) {
        if let Some(host) = instance.host() {
            for (path, followed) in &mut self.jobs {
                if followed.told {
                    continue;
                }
                if let Some(record) = &followed.record {
                    host.runtime().set_job(path, record);
                }
                followed.told = true;
            }
        }
        for (read, followed) in &mut self.followed {
            if followed.told {
                continue;
            }
            instance.set_invoked(read, followed.read.as_ref());
            followed.told = true;
        }
        for place in std::mem::take(&mut self.lines_moved) {
            if let Some(lines) = self.lines.get(&place) {
                instance.set_lines(&place, lines);
            }
        }
    }

    /// Take the reading `result` of the invocation views read as `correlation` asked for.
    fn read(&mut self, correlation: u64, result: Option<&String>) {
        let Some(read) = self.reads.remove(&correlation) else {
            return;
        };
        let Some(followed) = self.followed.get_mut(&read) else {
            return;
        };
        followed.waiting = false;
        let Some(text) = result else {
            // A read the dispatch refused says nothing of the invocation: it is read again when
            // it is due.
            tracing::debug!(%read, "an invocation's record was not read");
            return;
        };
        let readings = match serde_json::from_str::<Value>(text) {
            Ok(readings) => readings,
            Err(why) => {
                tracing::debug!(%read, %why, "an invocation's reading is not JSON");
                return;
            }
        };
        let Some(value) = readings.pointer("/records/0/value").cloned() else {
            return;
        };
        let item = match serde_json::from_value::<Invocation>(value) {
            Ok(item) => item,
            Err(why) => {
                tracing::debug!(%read, %why, "an invocation's reading is not one");
                return;
            }
        };
        let id = followed.id.map(|id| id.0.to_string()).unwrap_or_default();
        let now = invoked(&self.workspace, &id, &item);
        if followed.read.as_ref() != Some(&now) {
            followed.read = Some(now);
            followed.told = false;
        }
    }

    /// The `at`th line of `place` answered `said`.
    fn said(&mut self, place: &str, at: usize, said: &str, failed: bool) {
        if let Some(line) = self
            .lines
            .get_mut(place)
            .and_then(|lines| lines.get_mut(at))
        {
            line.running = false;
            said.clone_into(&mut line.said);
            line.failed = failed;
            self.lines_moved.insert(place.to_owned());
        }
    }
}

/// An invocation of `target` with `argument`, as the instance's own call.
fn request(target: tilo_actions::remote::RemoteActionTarget, argument: &Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(argument.as_object().cloned().unwrap_or_default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The invocation the text `read` names: its id, alone or as the last step of the address it
/// is kept at (`<workspace>/<id>`, `tilo.invocations:<id>`, `…#answer:<id>`).
fn invocation_in(read: &str) -> Option<InvocationId> {
    read.rsplit(['/', ':', '#'])
        .find_map(|part| match part.trim().parse::<InvocationId>() {
            Ok(id) => Some(id),
            Err(why) => {
                tracing::trace!(%why, part, "a word of a read names no invocation");
                None
            }
        })
}

/// `item`, the record of the invocation `id`, as a view reads it.
fn invoked(workspace: &str, id: &str, item: &Invocation) -> Invoked {
    let state = serde_json::to_value(item.status)
        .ok()
        .and_then(|status| status.as_str().map(str::to_owned))
        .unwrap_or_default();
    Invoked {
        workspace: workspace.to_owned(),
        id: id.to_owned(),
        state,
        action: item.action.clone().unwrap_or_default(),
        title: item.title.clone().unwrap_or_default(),
        summary: item.summary.clone().unwrap_or_default(),
        failure: item.failure.clone().unwrap_or_default(),
        // The invocations it made, its steps: its own calls of the same id are not steps of it.
        calls: item
            .calls
            .iter()
            .filter(|call| call.as_str() != id)
            .cloned()
            .collect(),
        // TODO(agent-engines): the workspace's invocation record names no model call yet; once
        // `workspace_invocations::Invocation` carries the step's `provider`, `model` and
        // `cost_usd`, read them here and a reply's cost line sums them over its steps.
        provider: String::new(),
        model: String::new(),
        cost: 0.0,
    }
}

/// What a line's run answered, in the person's words, and whether it failed.
fn line_answer(receipt: &InvocationReceipt) -> (String, bool) {
    match &receipt.status {
        ReceiptStatus::Completed { output } => {
            let value = output.value.clone().unwrap_or_default();
            let text = |key: &str| {
                value
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            };
            let said = Some(text("person_text"))
                .filter(|said| !said.is_empty())
                .unwrap_or_else(|| text("text"));
            let failed = value.get("failed").and_then(Value::as_bool) == Some(true);
            (said, failed)
        }
        ReceiptStatus::Failed { error } => (error.message.clone(), true),
        ReceiptStatus::Rejected { error } => (error.to_string(), true),
        ReceiptStatus::Cancelled { .. } => ("Stopped.".to_owned(), true),
        ReceiptStatus::Accepted => (String::new(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invocation_is_found_in_the_address_it_is_kept_at() {
        let id = InvocationId::new();
        let text = id.0.to_string();
        for kept in [
            text.clone(),
            format!("w1/{text}"),
            format!("tilo.invocations:{text}"),
            format!("agents.agent:a1#answer:{text}"),
        ] {
            assert_eq!(invocation_in(&kept), Some(id), "{kept}");
        }
        assert_eq!(invocation_in("not an invocation"), None);
        assert_eq!(invocation_in(""), None);
    }
}
