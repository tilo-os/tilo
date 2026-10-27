//! Shared checklist: what collaboration and undo mean for an app's state.
//!
//! The app has no networking, no locking and no undo stack of its own. Its
//! state is a CRDT the system owns and syncs, so every choice that matters
//! for collaboration is made in the model's field types:
//!
//! - A `String` field is collaborative text. Two people typing in the same
//!   title merge character by character.
//! - A `bool` or number is a register. Concurrent writes resolve to one of
//!   them, deterministically on every replica.
//! - A `Collection` gives each item a stable id (`cx.id()`), so concurrent
//!   adds keep both items and an edit follows its item wherever it moves.
//! - Each edit of a field and each verb is one undo step and one attributed
//!   version. Undo takes back the local participant's own, not a
//!   collaborator's.
//!
//! The next item's title is typed into a form: local to this window, never
//! synced or undone, and submitted as one invocation of `add`.
//!
//! The tests are the lesson: they fork a second replica, edit both sides
//! concurrently and show what the merge keeps.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.shared-checklist";
/// The window and launcher title.
pub const APP_TITLE: &str = "Shared Checklist";

pub mod locales;

use ui::i18n::I18nManager;
use ui::prelude::*;

/// One item of the checklist.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    /// Collaborative text.
    #[model(label)]
    pub title: String,
    /// A register: concurrent toggles settle on one value.
    pub done: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Checklist {
    /// Collaborative text, like every `String`.
    pub name: String,
    pub items: Collection<Item>,
}

impl Checklist {
    /// How many items are done, of how many.
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        let done = self.items.values().filter(|item| item.done).count();
        (done, self.items.len())
    }
}

/// The name, every title and every done flag are edited at their paths; the
/// verbs are what a field edit cannot say.
#[resource(kind = "example.checklist", label = "Checklist")]
impl Checklist {
    /// Add an item.
    #[action]
    pub fn add(
        &mut self,
        cx: &mut ActionCx,
        #[param("Title of the new item")] title: String,
    ) -> Result<(), &'static str> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err("an item needs a title");
        }
        self.items.push(cx.id(), Item { title, done: false });
        Ok(())
    }

    /// Remove every checked item, as one undo step.
    #[action(destructive)]
    pub fn clear_done(&mut self) -> Result<(), &'static str> {
        let done = self
            .items
            .ids()
            .filter(|id| self.items.get(id).is_some_and(|item| item.done))
            .cloned()
            .collect::<Vec<_>>();
        if done.is_empty() {
            return Err("nothing is checked");
        }
        for id in done {
            self.items.remove(&id);
        }
        Ok(())
    }
}

fn item_row(row: &mut ViewCx<Checklist, Item>) -> Element {
    let p = row.paths();
    let done = row.checkbox(crate::locales::keys::CHECKLIST_DONE_LABEL, p.done);
    let title = row
        .input(p.title)
        .with_placeholder(crate::locales::keys::CHECKLIST_TITLE_PLACEHOLDER);
    Box::new(ui::row![done, title])
}

fn progress_text(checklist: &Checklist) -> String {
    let (done, total) = checklist.progress();
    if total == 0 {
        return I18nManager::lookup_plain("checklist-empty");
    }
    I18nManager::format(
        "checklist-progress",
        &[("done", done.into()), ("total", total.into())],
    )
}

pub struct App;

impl ResourceApp for App {
    type Resource = Checklist;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn label(checklist: &Checklist) -> String {
        if checklist.name.is_empty() {
            I18nManager::lookup_plain("checklist-untitled")
        } else {
            checklist.name.clone()
        }
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        let p = cx.paths();
        let name = cx
            .input(p.name)
            .with_id("name-input")
            .with_placeholder(crate::locales::keys::CHECKLIST_NAME_PLACEHOLDER);
        // The next item's title is this window's own until it is added.
        let next = cx.form::<checklist::Add>();
        let draft = cx
            .input(next.paths().title)
            .with_id("draft-input")
            .with_placeholder(crate::locales::keys::CHECKLIST_DRAFT_PLACEHOLDER);
        let add = cx.button(crate::locales::keys::CHECKLIST_ADD, next.submit());
        let progress = cx.text(progress_text).with_id("progress");
        let rows = cx.each(|checklist| &checklist.items, item_row);
        let clear = cx.button(
            crate::locales::keys::CHECKLIST_CLEAR_DONE,
            Checklist::CLEAR_DONE.invoke(),
        );
        Box::new(ui::column![
            labeled_field(crate::locales::keys::CHECKLIST_NAME_LABEL, name),
            ui::row![
                labeled_field(crate::locales::keys::CHECKLIST_DRAFT_LABEL, draft),
                add
            ],
            progress,
            rows,
            clear,
        ])
    }
}

ui::dev_reload_entry!(App);
