//! The editor's disk work, done where the window may wait for it.
//!
//! Reading a file into a tab, writing it back, noticing it changed, reading a
//! folder and opening a project all touch the disk. Where the window's thread
//! may wait (its own process, a test) that happens at once, as a direct call.
//! On the compositor's frame loop ([`ui::storage::waiting`]), which never
//! waits on the disk, the same work runs on a background thread; the window
//! is woken when it is done and folds the outcome in at its next settlement,
//! exactly as the direct call would have.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ui::lock::MutexRecover as _;

use super::CodeApp;

/// What to do with a finished piece of work, given the editor and the live
/// document.
type Apply = Box<dyn FnOnce(&mut CodeApp, &mut ui::widget::Edits<'_>, Box<dyn Any + Send>)>;

/// Work finished on a background thread, by the id it was queued under.
type Finished = Arc<Mutex<Vec<(u64, Box<dyn Any + Send>)>>>;

/// Work under way on a background thread, and what came back.
#[derive(Default)]
pub(crate) struct DiskWork {
    next: u64,
    waiting: HashMap<u64, Apply>,
    done: Finished,
    /// Files being read into their tabs.
    pub(crate) seeding: std::collections::HashSet<super::TreeId>,
}

impl CodeApp {
    /// Do `work`, then `apply` its outcome: at once where this thread may
    /// wait on the disk, else on a background thread named `name`, applying
    /// at the settlement after it is done.
    pub(crate) fn on_disk<T: Send + 'static>(
        &mut self,
        name: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
        apply: impl FnOnce(&mut Self, T) + 'static,
    ) {
        self.on_disk_waking(name, |_| true, work, apply);
    }

    /// [`Self::on_disk`], waking the window only for an outcome `wakes`
    /// picks: work whose outcome usually changes nothing (a file found
    /// unchanged) leaves an idle window asleep, and is folded in when
    /// something else brings the window round.
    pub(crate) fn on_disk_waking<T: Send + 'static>(
        &mut self,
        name: &'static str,
        wakes: fn(&T) -> bool,
        work: impl FnOnce() -> T + Send + 'static,
        apply: impl FnOnce(&mut Self, T) + 'static,
    ) {
        if !ui::storage::waiting::forbidden() {
            apply(self, work());
            return;
        }
        self.in_background(name, wakes, work, move |app, _, outcome| {
            apply(app, outcome);
        });
    }

    /// [`Self::on_disk`] for an outcome applied to the live document.
    pub(crate) fn on_disk_editing<T: Send + 'static>(
        &mut self,
        name: &'static str,
        doc: &mut ui::widget::Edits<'_>,
        work: impl FnOnce() -> T + Send + 'static,
        apply: impl FnOnce(&mut Self, &mut ui::widget::Edits<'_>, T) + 'static,
    ) {
        if !ui::storage::waiting::forbidden() {
            apply(self, doc, work());
            return;
        }
        self.in_background(name, |_| true, work, apply);
    }

    /// [`Self::on_disk`] for work a file effect waits on: its answer at once
    /// where this thread may wait, else `None`, and the effect waiting
    /// ([`CodeApp::finish_file_effect`]) is answered once the work is done.
    pub(crate) fn on_disk_answering<T: Send + 'static>(
        &mut self,
        name: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
        apply: impl FnOnce(&mut Self, T) -> Result<(), String> + 'static,
    ) -> Option<Result<(), String>> {
        if !ui::storage::waiting::forbidden() {
            return Some(apply(self, work()));
        }
        self.in_background(
            name,
            |_| true,
            work,
            move |app, _, outcome| {
                let answer = apply(app, outcome);
                app.finish_file_effect(answer);
            },
        );
        None
    }

    fn in_background<T: Send + 'static>(
        &mut self,
        name: &'static str,
        wakes: fn(&T) -> bool,
        work: impl FnOnce() -> T + Send + 'static,
        apply: impl FnOnce(&mut Self, &mut ui::widget::Edits<'_>, T) + 'static,
    ) {
        let id = self.queue_apply(apply);
        let done = Arc::clone(&self.disk_work.done);
        let window = ui::WakeHandle::new();
        let spawned = ui::background::Background::current().spawn(name, move || {
            let outcome = work();
            let wake = wakes(&outcome);
            done.lock_recover().push((id, Box::new(outcome)));
            if wake {
                window.wake();
            }
        });
        if let Err(error) = spawned {
            self.disk_work.waiting.remove(&id);
            self.note(format!("Could not reach the disk: {error}"), true);
        }
    }

    fn queue_apply<T: Send + 'static>(
        &mut self,
        apply: impl FnOnce(&mut Self, &mut ui::widget::Edits<'_>, T) + 'static,
    ) -> u64 {
        let id = self.disk_work.next;
        self.disk_work.next = self.disk_work.next.wrapping_add(1);
        self.disk_work.waiting.insert(
            id,
            Box::new(move |app, doc, outcome| {
                if let Ok(outcome) = outcome.downcast::<T>() {
                    apply(app, doc, *outcome);
                }
            }),
        );
        id
    }

    /// Fold in the work finished since the last settlement.
    pub(crate) fn apply_disk_work(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let finished = std::mem::take(&mut *self.disk_work.done.lock_recover());
        for (id, outcome) in finished {
            if let Some(apply) = self.disk_work.waiting.remove(&id) {
                apply(self, &mut doc.reborrow(), outcome);
            }
        }
    }
}

#[cfg(test)]
#[path = "disk_work_tests.rs"]
mod tests;
