//! The requests a workspace's thread holds until their workspace is ready for them: joined, and
//! holding the keys of what each reads.
//!
//! A held request runs on the workspace's next change that makes it ready, never only at its
//! bound. The session hands the thread a workspace's change once, and only while the thread
//! asked for it ([`super::member`]), so whatever holds a request asks for the next change before
//! it looks whether the request is ready: a change that lands after the look still wakes it.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use tilo_server_protocol::WorkspaceId;

use super::Job;

/// A request held off the service's thread until its workspace is ready for
/// it: joined, and holding the keys of what the request reads.
pub(super) struct Waiting<J = Job> {
    /// When it arrived: the wait it reports, and what its deadline is counted
    /// from.
    pub(super) queued: Instant,
    /// When it runs in any case, and says how far it got.
    pub(super) deadline: Instant,
    pub(super) job: J,
}

/// What holds requests for their workspace: the service's thread, or a test's.
pub(super) trait Holder {
    type Job;

    /// The requests held, oldest first, by workspace.
    fn joining(&self) -> &HashMap<WorkspaceId, VecDeque<Waiting<Self::Job>>>;

    fn joining_mut(&mut self) -> &mut HashMap<WorkspaceId, VecDeque<Waiting<Self::Job>>>;

    /// Ask for `workspace_id`'s next change to wake the thread, or stop asking.
    fn wake_on_change(&self, workspace_id: WorkspaceId, waiting: bool);

    /// Whether `job`, held for `workspace_id`, can run now.
    fn ready_for(&mut self, workspace_id: WorkspaceId, job: &Self::Job) -> bool;

    /// Run `job`, asked at `queued`.
    fn run(&mut self, queued: Instant, job: Self::Job);

    /// Whether anything besides the requests held here waits for a change of `workspace_id`.
    fn waits_besides(&self, workspace_id: WorkspaceId) -> bool;
}

/// Hold `waiting` for `workspace_id` until it is ready, and run it at once if it is by now.
pub(super) fn hold<H: Holder>(holder: &mut H, workspace_id: WorkspaceId, waiting: Waiting<H::Job>) {
    holder
        .joining_mut()
        .entry(workspace_id)
        .or_default()
        .push_back(waiting);
    // A change that landed before the workspace was asked to wake this
    // thread woke nobody; `resume` looks again now that it is.
    resume(holder, workspace_id);
}

/// Run the requests held for `workspace_id`, its change just arrived: those that are ready run,
/// in the order they came, and those still waiting are held again with what they have waited so
/// far.
pub(super) fn resume<H: Holder>(holder: &mut H, workspace_id: WorkspaceId) {
    let Some(waiting) = holder.joining_mut().remove(&workspace_id) else {
        return;
    };
    for Waiting {
        queued,
        deadline,
        job,
    } in waiting
    {
        // The next change is asked for before each look. While the list is out of `joining`, a
        // request run just above that answered who waited for a change finds nothing held here
        // and stops the wake, and a request held again below would then sleep to its bound with
        // what it waits for already arrived.
        holder.wake_on_change(workspace_id, true);
        if !holder.ready_for(workspace_id, &job) {
            holder
                .joining_mut()
                .entry(workspace_id)
                .or_default()
                .push_back(Waiting {
                    queued,
                    deadline,
                    job,
                });
            continue;
        }
        holder.run(queued, job);
    }
    stop_waking_if_idle(holder, workspace_id);
}

/// Stop asking to be woken for `workspace_id` once nothing waits for it: no request held here,
/// and nothing else waiting for a change of it.
pub(super) fn stop_waking_if_idle<H: Holder>(holder: &H, workspace_id: WorkspaceId) {
    if !holder.joining().contains_key(&workspace_id) && !holder.waits_besides(workspace_id) {
        holder.wake_on_change(workspace_id, false);
    }
}

#[cfg(test)]
#[path = "joining_tests.rs"]
mod tests;
