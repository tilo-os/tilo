//! The Windows programs the service started that are still open, and the ends of those that
//! stopped.
//!
//! A launch is answered once its program started; the program goes on, watched by a thread of
//! its own, and its end is kept here until each manager of its workspace has been told of it in
//! a list. The service knows every program it started, so it can say which entries are open and end
//! them on request.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::{ExecutionError, ExecutionOutcome, ProgramEnd, StartedProgram};

/// How many ends are kept for managers that have not listed since: older ones are dropped, as a
/// manager that was away that long reads the running state, not each end.
const KEPT_ENDS: usize = 64;

/// A program of the service that is open.
struct Running {
    entry_id: String,
    workspace_id: String,
    /// What ends it; none for a program the service cannot end (a Windows appliance window).
    program: Option<Arc<dyn StartedProgram>>,
}

#[derive(Default)]
struct Inner {
    next: u64,
    running: BTreeMap<u64, Running>,
    /// Each end with its sequence number and workspace, oldest first.
    ended: VecDeque<(u64, String, ProgramEnd)>,
    next_end: u64,
}

/// The programs the service started.
#[derive(Default)]
pub(crate) struct Programs {
    inner: Mutex<Inner>,
    /// Told whenever a program ended.
    ended: Condvar,
}

/// The sequence number of the first end a connection has not read.
#[derive(Default)]
pub(crate) struct EndsRead(u64);

impl Programs {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `program`, of entry `entry_id` in `workspace_id`, is open from now on; [`Self::ended`] is told
    /// of its end under the number this returns.
    pub(crate) fn started(
        &self,
        entry_id: &str,
        workspace_id: &str,
        program: Option<Arc<dyn StartedProgram>>,
    ) -> u64 {
        let mut inner = self.inner();
        let id = inner.next;
        inner.next = inner.next.wrapping_add(1);
        inner.running.insert(
            id,
            Running {
                entry_id: entry_id.to_owned(),
                workspace_id: workspace_id.to_owned(),
                program,
            },
        );
        id
    }

    /// The program `id` ended as `outcome` says; `display_name` names it to the manager.
    pub(crate) fn ended(
        &self,
        id: u64,
        display_name: &str,
        outcome: Result<ExecutionOutcome, String>,
    ) {
        // One step, so a list never finds the program neither open nor ended.
        let mut inner = self.inner();
        let Some(running) = inner.running.remove(&id) else {
            return;
        };
        let end = match outcome {
            Ok(outcome) => ProgramEnd {
                entry_id: running.entry_id,
                display_name: display_name.to_owned(),
                success: outcome.success,
                exit_code: outcome.exit_code,
                error: None,
                said: outcome.said,
            },
            Err(error) => ProgramEnd {
                entry_id: running.entry_id,
                display_name: display_name.to_owned(),
                success: false,
                exit_code: None,
                error: Some(error),
                said: None,
            },
        };
        let sequence = inner.next_end;
        inner.next_end = inner.next_end.wrapping_add(1);
        inner.ended.push_back((sequence, running.workspace_id, end));
        while inner.ended.len() > KEPT_ENDS {
            inner.ended.pop_front();
        }
        drop(inner);
        self.ended.notify_all();
    }

    /// Wait until no program started before the program `id` whose entry `waits_for` names is
    /// open any more: a program that must not be open beside another starts once the ones before it
    /// ended, in the order they were asked for.
    pub(crate) fn wait_for_earlier(&self, id: u64, waits_for: impl Fn(&str) -> bool) {
        let mut inner = self.inner();
        while inner
            .running
            .iter()
            .any(|(other, running)| *other < id && waits_for(&running.entry_id))
        {
            inner = self
                .ended
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
        drop(inner);
    }

    /// The entries with a program that is open, in any workspace.
    pub(crate) fn running(&self) -> BTreeSet<String> {
        self.inner()
            .running
            .values()
            .map(|running| running.entry_id.clone())
            .collect()
    }

    /// The entries with a program in `workspace_id` that is open.
    pub(crate) fn running_in(&self, workspace_id: &str) -> BTreeSet<String> {
        self.inner()
            .running
            .values()
            .filter(|running| running.workspace_id == workspace_id)
            .map(|running| running.entry_id.clone())
            .collect()
    }

    /// The ends in `workspace_id` that `read` has not been told of, oldest first. A connection's
    /// first read is told of every end still kept: a manager opened after its program ended
    /// hears how it ended.
    pub(crate) fn ends_for(&self, workspace_id: &str, read: &mut EndsRead) -> Vec<ProgramEnd> {
        let inner = self.inner();
        let ends = inner
            .ended
            .iter()
            .filter(|(sequence, workspace, _)| workspace == workspace_id && *sequence >= read.0)
            .map(|(_, _, end)| end.clone())
            .collect();
        read.0 = inner.next_end;
        ends
    }

    /// End every program of `entry_id` in `workspace_id`: whether one ran that can be ended.
    pub(crate) fn stop(&self, workspace_id: &str, entry_id: &str) -> bool {
        let programs: Vec<_> = self
            .inner()
            .running
            .values()
            .filter(|running| running.workspace_id == workspace_id && running.entry_id == entry_id)
            .filter_map(|running| running.program.clone())
            .collect();
        for program in &programs {
            program.stop();
        }
        !programs.is_empty()
    }

    /// End every open program of `entries`, in whichever workspace it was started, and wait up
    /// to `patience` for them to go. Answers the entries still open after that: a program the
    /// service cannot end, or one that did not stop, says its capsule must not lose its files
    /// under it.
    pub(crate) fn close_entries(
        &self,
        entries: &BTreeSet<String>,
        patience: std::time::Duration,
    ) -> Vec<String> {
        let (closable, stuck): (Vec<_>, Vec<String>) = {
            let inner = self.inner();
            let mut closable = Vec::new();
            let mut stuck = Vec::new();
            for running in inner
                .running
                .values()
                .filter(|running| entries.contains(&running.entry_id))
            {
                match &running.program {
                    Some(program) => closable.push(Arc::clone(program)),
                    // A program the service holds no handle on cannot be ended from here, so
                    // it is said back at once rather than waited out.
                    None => stuck.push(running.entry_id.clone()),
                }
            }
            drop(inner);
            (closable, stuck)
        };
        if !stuck.is_empty() {
            return stuck;
        }
        for program in &closable {
            program.stop();
        }
        if closable.is_empty() {
            return Vec::new();
        }
        let deadline = std::time::Instant::now().checked_add(patience);
        let mut inner = self.inner();
        loop {
            let open: Vec<String> = inner
                .running
                .values()
                .filter(|running| entries.contains(&running.entry_id))
                .map(|running| running.entry_id.clone())
                .collect();
            if open.is_empty() {
                return Vec::new();
            }
            let Some(left) = deadline
                .and_then(|deadline| deadline.checked_duration_since(std::time::Instant::now()))
            else {
                return open;
            };
            let (next, _) = self
                .ended
                .wait_timeout(inner, left)
                .unwrap_or_else(PoisonError::into_inner);
            inner = next;
        }
    }
}

/// Watch `program` on a thread of its own until it ended: then give back what its launch was
/// lent (`release`) and keep its end in `programs`. A manager may end it only when `stoppable`.
pub(crate) fn watch(
    programs: &Arc<Programs>,
    entry_id: &str,
    workspace_id: &str,
    display_name: &str,
    program: Box<dyn StartedProgram>,
    stoppable: bool,
    release: Box<dyn FnOnce() + Send>,
) -> Result<(), std::io::Error> {
    let program: Arc<dyn StartedProgram> = Arc::from(program);
    let id = programs.started(
        entry_id,
        workspace_id,
        stoppable.then(|| Arc::clone(&program)),
    );
    let (watched, name) = (Arc::clone(programs), display_name.to_owned());
    let stopping = Arc::clone(&program);
    let spawned = std::thread::Builder::new()
        .name("tilo-windows-program".to_owned())
        .spawn(move || {
            let outcome = program.wait();
            release();
            watched.ended(
                id,
                &name,
                outcome.map_err(|error: ExecutionError| error.to_string()),
            );
        });
    if let Err(error) = spawned {
        // Unwatched, it would go on with nobody to report or end it.
        stopping.stop();
        let _ = stopping.wait();
        programs.ended(id, display_name, Err(error.to_string()));
        return Err(error);
    }
    Ok(())
}

/// `program`, telling `finish` how it ended before anyone waiting on it hears: a setup's
/// transaction is closed by the time a manager reads that it ended.
pub(crate) fn finishing(
    program: Box<dyn StartedProgram>,
    finish: impl FnOnce(Result<ExecutionOutcome, String>) + Send + 'static,
) -> Box<dyn StartedProgram> {
    Box::new(Finishing {
        program,
        finish: Mutex::new(Some(Box::new(finish))),
    })
}

type Finish = Box<dyn FnOnce(Result<ExecutionOutcome, String>) + Send>;

struct Finishing {
    program: Box<dyn StartedProgram>,
    finish: Mutex<Option<Finish>>,
}

impl StartedProgram for Finishing {
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError> {
        let outcome = self.program.wait();
        let finish = self
            .finish
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(finish) = finish {
            finish(match &outcome {
                Ok(outcome) => Ok(outcome.clone()),
                Err(error) => Err(error.to_string()),
            });
        }
        outcome
    }

    fn stop(&self) {
        self.program.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_test_env::SpawnUnderEnvLock as _;

    #[test]
    fn each_connection_hears_each_end_of_its_workspace_once() {
        let programs = Programs::default();
        let mut early = EndsRead::default();
        assert!(programs.ends_for("one", &mut early).is_empty());
        let first = programs.started("probe", "one", None);
        let other = programs.started("probe", "two", None);
        assert_eq!(
            programs.running_in("one"),
            BTreeSet::from(["probe".to_owned()])
        );
        programs.ended(
            first,
            "Probe",
            Ok(ExecutionOutcome {
                success: false,
                exit_code: Some(3),
                said: None,
            }),
        );
        programs.ended(other, "Probe", Err("no display".to_owned()));
        assert!(programs.running_in("one").is_empty());
        let heard = programs.ends_for("one", &mut early);
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].exit_code, Some(3));
        assert!(programs.ends_for("one", &mut early).is_empty(), "told once");
        let mut late = EndsRead::default();
        assert_eq!(
            programs.ends_for("two", &mut late)[0].error.as_deref(),
            Some("no display"),
            "a manager opened later hears it"
        );
    }

    /// A program the service started is ended on request: its watcher hears the end, gives back
    /// what the launch was lent, and reports it.
    #[test]
    fn a_stopped_program_ends_and_is_reported() {
        let programs = Arc::new(Programs::default());
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn_locked()
            .expect("a program starts");
        let program = crate::ChildProgram::new(child).expect("its pidfd");
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = Arc::clone(&released);
        watch(
            &programs,
            "probe",
            "one",
            "Probe",
            Box::new(program),
            true,
            Box::new(move || release.store(true, std::sync::atomic::Ordering::Release)),
        )
        .expect("watched");
        assert!(programs.running_in("one").contains("probe"));
        assert!(
            !programs.stop("two", "probe"),
            "another workspace's program is not ended"
        );
        assert!(programs.stop("one", "probe"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !programs.running_in("one").is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the program did not end"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(released.load(std::sync::atomic::Ordering::Acquire));
        let ends = programs.ends_for("one", &mut EndsRead::default());
        assert_eq!(ends.len(), 1);
        assert!(!ends[0].success);
        assert_eq!(ends[0].exit_code, None, "ended by a signal");
    }

    /// A capsule's delete ends its open programs first, in whichever workspace they were
    /// started, and answers only once they are gone: no file disappears under a program still
    /// using it.
    #[test]
    fn a_capsules_programs_are_ended_before_its_files_go() {
        let programs = Arc::new(Programs::default());
        for (workspace, entry) in [("one", "psftp"), ("two", "putty")] {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .spawn_locked()
                .expect("a program starts");
            let program = crate::ChildProgram::new(child).expect("its pidfd");
            watch(
                &programs,
                entry,
                workspace,
                entry,
                Box::new(program),
                true,
                Box::new(|| {}),
            )
            .expect("watched");
        }
        let entries = BTreeSet::from(["psftp".to_owned(), "putty".to_owned()]);
        assert!(
            programs
                .close_entries(&entries, std::time::Duration::from_secs(10))
                .is_empty(),
            "both programs ended"
        );
        assert!(programs.running_in("one").is_empty());
        assert!(programs.running_in("two").is_empty());
    }

    /// A program the service cannot end is named back rather than waited out: the capsule's
    /// files do not go under it.
    #[test]
    fn a_program_that_cannot_be_ended_is_named_back() {
        let programs = Programs::default();
        programs.started("putty", "one", None);
        let entries = BTreeSet::from(["putty".to_owned()]);
        assert_eq!(
            programs.close_entries(&entries, std::time::Duration::from_millis(50)),
            ["putty"]
        );
    }
}
