//! The run a store holds open: from an action's first change to its end, everything it and the
//! actions it called write belongs to the run, and a run that fails anywhere takes all of it back
//! out ([`tilolang_runtime::Store::abort_run`]).

use super::Inner;

/// Where a run began: the record's local changes and the actions queued when it started.
#[derive(Clone, Copy, Debug)]
pub(super) struct RunBegin {
    pub(super) local: usize,
    pub(super) ticket: u64,
}

impl Inner {
    /// A run began: what it changes from here, and what it queues, is the run's to keep or lose.
    pub(super) fn begin_run(&mut self, run: &str) {
        let begin = RunBegin {
            local: self.local.len(),
            ticket: self.next_ticket,
        };
        self.runs.insert(run.to_owned(), begin);
    }

    pub(super) fn end_run(&mut self, run: &str) {
        self.runs.remove(run);
    }

    /// The run failed somewhere: nothing of it is queued to be applied, and nothing of it stays in
    /// the record.
    pub(super) fn abort_run(&mut self, run: &str) {
        let Some(begin) = self.runs.remove(run) else {
            return;
        };
        let dropped: Vec<tilo_actions::InvocationId> = self
            .committed
            .iter()
            .filter(|committed| committed.ticket >= begin.ticket)
            .filter_map(|committed| committed.action.authorship.invocation.as_ref())
            .map(|run| crate::action::invocation_id(run))
            .collect();
        for run in &dropped {
            self.own_dropped(run);
        }
        self.committed
            .retain(|committed| committed.ticket < begin.ticket);
        if self.local.len() > begin.local {
            self.drop_local_from(begin.local);
            self.reapply();
            self.rebuild_overlay();
            self.changed();
        }
    }
}
