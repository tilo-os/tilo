//! The checklist app several tests drive: one draft and a list of tasks, its
//! rows bounded by a scroll view or shown whole by the test that builds them.

#![allow(dead_code, reason = "each test uses part of the app")]

use tilo_app::i18n::EmbeddedCatalog;
use tilo_app::prelude::*;

/// The app's labels: the field a task's title is typed into, and its box.
pub static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[(
        "en-US",
        "checklist-draft = Next task\nchecklist-done = Completed\n",
    )],
);

/// The label of a row's checkbox.
pub const DONE: &str = "checklist-done";

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// One task.
pub struct Task {
    pub title: String,
    pub done: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
/// The draft and the tasks.
pub struct Checklist {
    pub draft: String,
    pub tasks: Collection<Task>,
}

#[resource(kind = "test.checklist", label = "Checklist")]
impl Checklist {
    /// Set the next task's title.
    #[action]
    pub fn draft(&mut self, title: String) {
        self.draft = title;
    }

    /// Add `count` tasks at the end.
    #[action]
    pub fn seed(&mut self, cx: &mut ActionCx, count: u32) {
        for _ in 0..count {
            let index = self.tasks.len();
            self.tasks.push(
                cx.id(),
                Task {
                    title: format!("Task {index}"),
                    done: false,
                },
            );
        }
    }

    /// Add `count` tasks whose titles wrap in a row, every third done.
    #[action]
    pub fn seed_long(&mut self, cx: &mut ActionCx, count: u32) {
        for index in 0..count {
            self.tasks.push(
                cx.id(),
                Task {
                    title: format!("Task {index}: the quick brown fox jumps over the lazy dog"),
                    done: index.is_multiple_of(3),
                },
            );
        }
    }

    /// Mark a task completed or open.
    ///
    /// # Errors
    /// Refuses a task that no longer exists.
    #[action]
    pub fn set_done(&mut self, task: &Id, done: bool) -> Result<(), &'static str> {
        self.tasks.get_mut(task).ok_or("gone")?.done = done;
        Ok(())
    }
}
