//! The requests a workspace's thread holds, driven through a session as the thread meets it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use tilo_server_protocol::WorkspaceId;

use super::{Holder, Waiting, hold, resume, stop_waking_if_idle};

/// The host device's session as the thread meets it ([`super::super::member`]): a change of
/// any workspace hands the thread one wake for each workspace it asked to be woken for, and
/// then nothing more until it asks again.
#[derive(Default)]
struct Session {
    asked: RefCell<HashSet<WorkspaceId>>,
    /// The workspaces whose wakes the thread has not taken yet.
    wakes: VecDeque<WorkspaceId>,
}

impl Session {
    /// Something changed in the session.
    fn change(&mut self) {
        let asked = std::mem::take(&mut *self.asked.borrow_mut());
        self.wakes.extend(asked);
    }
}

/// What a request held for its workspace waits for, and what it does when it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// A wait for the workspace's next change: running answers whoever waited for one.
    Watch,
    /// A write to a record whose keys have not arrived.
    Write,
}

/// A workspace's thread over [`Session`]: what has arrived of one workspace, and what ran.
struct Thread {
    session: Session,
    joining: HashMap<WorkspaceId, VecDeque<Waiting<Request>>>,
    joined: bool,
    keys: bool,
    /// Someone waits for a change of the workspace.
    watched: bool,
    ran: Vec<Request>,
}

impl Thread {
    fn new() -> Self {
        Self {
            session: Session::default(),
            joining: HashMap::new(),
            joined: false,
            keys: false,
            watched: false,
            ran: Vec::new(),
        }
    }

    /// Take the wakes the session handed over, each as the thread's next job.
    fn take_wakes(&mut self) {
        while let Some(workspace_id) = self.session.wakes.pop_front() {
            resume(self, workspace_id);
        }
    }
}

impl Holder for Thread {
    type Job = Request;

    fn joining(&self) -> &HashMap<WorkspaceId, VecDeque<Waiting<Request>>> {
        &self.joining
    }

    fn joining_mut(&mut self) -> &mut HashMap<WorkspaceId, VecDeque<Waiting<Request>>> {
        &mut self.joining
    }

    fn wake_on_change(&self, workspace_id: WorkspaceId, waiting: bool) {
        let mut asked = self.session.asked.borrow_mut();
        if waiting {
            asked.insert(workspace_id);
        } else {
            asked.remove(&workspace_id);
        }
    }

    fn ready_for(&mut self, _: WorkspaceId, job: &Request) -> bool {
        match job {
            Request::Watch => self.joined,
            Request::Write => self.joined && self.keys,
        }
    }

    fn run(&mut self, _: Instant, job: Request) {
        self.ran.push(job);
        if job == Request::Watch {
            // The watch's wait is answered, and nobody else waited: as the service's thread
            // does once everyone who waited for a change was answered.
            self.watched = false;
            stop_waking_if_idle(self, WORKSPACE);
        }
    }

    fn waits_besides(&self, _: WorkspaceId) -> bool {
        self.watched
    }
}

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(7);

fn held(job: Request) -> Waiting<Request> {
    let now = Instant::now();
    Waiting {
        queued: now,
        deadline: now,
        job,
    }
}

/// A watch and a write wait for a workspace being joined. The join lands: the watch runs and
/// answers its wait, which leaves nobody waiting for a change of the workspace but the write,
/// still short of its record's keys. The keys arriving run the write then, not at its bound.
#[test]
fn a_write_waiting_for_keys_runs_when_they_arrive_after_a_watch_beside_it_was_answered() {
    let mut thread = Thread::new();
    thread.watched = true;
    hold(&mut thread, WORKSPACE, held(Request::Watch));
    hold(&mut thread, WORKSPACE, held(Request::Write));
    assert!(thread.ran.is_empty(), "nothing runs before the join");

    thread.joined = true;
    thread.session.change();
    thread.take_wakes();
    assert_eq!(thread.ran, [Request::Watch], "the join runs the watch");

    thread.keys = true;
    thread.session.change();
    thread.take_wakes();
    assert_eq!(
        thread.ran,
        [Request::Watch, Request::Write],
        "the keys arriving run the write"
    );
    assert!(thread.joining.is_empty(), "nothing is held any more");
    assert!(
        thread.session.asked.borrow().is_empty(),
        "with nothing held, the workspace's changes wake the thread no longer"
    );
}
