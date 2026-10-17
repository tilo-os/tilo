//! The credential vault's work, off the UI thread and one piece at a time.
//!
//! Opening the vault asks the Secret Service for its key and saving it asks
//! again: D-Bus round trips that take seconds on a slow or wedged service.
//! The pane runs each on a worker and takes its result in on the UI thread,
//! where the person's record and the rest of the pane live.
//!
//! An account mutation journals its intent, writes the vault, then the
//! metadata, and clears the journal; loading the stores recovers a journal
//! left behind. So the work the pane queues here runs strictly in order,
//! each piece to its end before the next starts, as it did when all of it
//! ran on the UI thread: two mutations never interleave, and no reload
//! recovers a journal a running mutation still owns.

use std::collections::VecDeque;

use tracing::warn;

use super::AccountsPane;
use crate::accounts::VaultOpener;
use crate::device_read::{Erased, Landed, Landing};

/// What a worker runs with the vault.
type Work = Box<dyn FnOnce(&VaultOpener) -> Erased + Send>;

/// What runs on the UI thread with a worker's result.
type Then = Box<dyn FnOnce(&mut AccountsPane, Erased) -> Step>;

/// A piece of queued work, started on the UI thread.
type Start = Box<dyn FnOnce(&mut AccountsPane) -> Step>;

/// What a piece of work does next.
pub(super) enum Step {
    /// It ended.
    Done,
    /// It goes on once `work` has run with the vault on a worker.
    Vault { work: Work, then: Then },
}

impl Step {
    /// Run `work` with the vault on a worker, then `then` with its result on
    /// the UI thread.
    pub(super) fn vault<T: Send + 'static>(
        work: impl FnOnce(&VaultOpener) -> T + Send + 'static,
        then: impl FnOnce(&mut AccountsPane, T) -> Self + 'static,
    ) -> Self {
        Self::Vault {
            work: Box::new(move |vault: &VaultOpener| Box::new(work(vault)) as Erased),
            then: Box::new(move |pane: &mut AccountsPane, value: Erased| {
                match value.downcast::<T>() {
                    Ok(value) => then(pane, *value),
                    // The worker sends what this step's own work returned.
                    Err(_) => Self::Done,
                }
            }),
        }
    }
}

/// The queue of vault work and the piece a worker runs.
#[derive(Default)]
pub(super) struct VaultLane {
    running: Option<(Landing, Then)>,
    queued: VecDeque<Start>,
    /// Work is being advanced: work queued meanwhile waits its turn.
    advancing: bool,
}

impl VaultLane {
    /// Whether nothing runs or waits.
    pub(super) fn is_idle(&self) -> bool {
        self.running.is_none() && self.queued.is_empty()
    }
}

impl AccountsPane {
    /// Queue `start` behind the work already queued; it starts at once when
    /// there is none.
    pub(super) fn run_in_lane(&mut self, start: impl FnOnce(&mut Self) -> Step + 'static) {
        self.lane.queued.push_back(Box::new(start));
        self.advance_lane(false);
    }

    /// Take in what the worker finished and start what comes next, until a
    /// worker runs or nothing is left. `wait` waits for the worker instead:
    /// only ever off the UI thread.
    pub(super) fn advance_lane(&mut self, wait: bool) {
        if std::mem::replace(&mut self.lane.advancing, true) {
            return;
        }
        loop {
            let step = if let Some((result, _)) = &self.lane.running {
                let landed = if wait {
                    result.wait_erased()
                } else {
                    match result.poll_erased() {
                        Landed::Done(value) => Some(value),
                        Landed::Pending => break,
                        Landed::Lost => None,
                    }
                };
                let Some((_, then)) = self.lane.running.take() else {
                    break;
                };
                let Some(value) = landed else {
                    self.vault_work_lost();
                    continue;
                };
                then(self, value)
            } else if let Some(start) = self.lane.queued.pop_front() {
                start(self)
            } else {
                break;
            };
            if let Step::Vault { work, then } = step {
                let vault = self.account_root.vault_opener();
                let result = Landing::start_erased(Box::new(move || work(&vault)));
                self.lane.running = Some((result, then));
            }
        }
        self.lane.advancing = false;
    }

    /// Run everything queued to its end, waiting for each worker: for a
    /// caller off the UI thread (tests, and [`Self::remove`]).
    pub(super) fn settle_lane(&mut self) {
        crate::device_read::off_ui_thread();
        while !self.lane.is_idle() {
            self.advance_lane(true);
        }
    }

    /// A worker ended without a result (it panicked, or could not start):
    /// what it was part of cannot go on. A mutation it left half done is
    /// rolled back from its journal, as the next load of the stores would.
    fn vault_work_lost(&mut self) {
        warn!(target: "tilo::settings", "the credential vault work stopped unexpectedly");
        self.account_root.recover_mutation_or_warn();
        let stopped = "the credential vault stopped unexpectedly";
        if std::mem::take(&mut self.form_saving) {
            self.form_error = Some(stopped.into());
        }
        if std::mem::take(&mut self.reauth_saving) {
            self.reauth_error = Some(stopped.into());
        }
        if std::mem::take(&mut self.sign_in_saving)
            && let Some(flow) = self.sign_in.as_mut()
        {
            flow.save_failed(stopped);
        }
        self.queue_reload();
        self.changed = true;
    }
}
