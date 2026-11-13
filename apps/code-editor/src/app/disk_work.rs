//! The editor's disk work, done where the window may wait for it.
//!

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ui::lock::MutexRecover as _;
use ui::widget::toast::ToastKind;

use super::CodeApp;

type Apply = Box<dyn FnOnce(&mut CodeApp, &mut ui::widget::Edits<'_>, Box<dyn Any + Send>)>;

type Finished = Arc<Mutex<Vec<(u64, Box<dyn Any + Send>)>>>;

#[derive(Default)]
pub(crate) struct DiskWork {
    next: u64,
    waiting: HashMap<u64, Apply>,
    done: Finished,
    pub(crate) seeding: std::collections::HashSet<super::TreeId>,
}

impl CodeApp {
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
            Self::note(
                format!("Could not reach the disk: {error}"),
                ToastKind::Error,
            );
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
