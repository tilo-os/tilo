//! The credential vault's work, off the UI thread and one piece at a time.

use std::collections::VecDeque;

use tracing::warn;

use super::AccountsPane;
use crate::accounts::VaultOpener;
use crate::device_read::{Erased, Landed, Landing};

type Work = Box<dyn FnOnce(&VaultOpener) -> Erased + Send>;

type Then = Box<dyn FnOnce(&mut AccountsPane, Erased) -> Step>;

type Start = Box<dyn FnOnce(&mut AccountsPane) -> Step>;

pub(super) enum Step {
    Done,
    Vault { work: Work, then: Then },
}

impl Step {
    pub(super) fn vault<T: Send + 'static>(
        work: impl FnOnce(&VaultOpener) -> T + Send + 'static,
        then: impl FnOnce(&mut AccountsPane, T) -> Self + 'static,
    ) -> Self {
        Self::Vault {
            work: Box::new(move |vault: &VaultOpener| Box::new(work(vault)) as Erased),
            then: Box::new(move |pane: &mut AccountsPane, value: Erased| {
                match value.downcast::<T>() {
                    Ok(value) => then(pane, *value),
                    Err(_) => Self::Done,
                }
            }),
        }
    }
}

#[derive(Default)]
pub(super) struct VaultLane {
    running: Option<(Landing, Then)>,
    queued: VecDeque<Start>,
    advancing: bool,
}

impl VaultLane {
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
