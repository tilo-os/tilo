//! Shared checklist: what collaboration and undo mean for an app's state.
//!
//! The app has no networking, no locking and no undo stack of its own. Its
//! state is a CRDT the system owns and syncs, so every choice that matters
//! for collaboration is made in the resource type:
//!
//! - A `String` field is collaborative text. Two people typing in the same
//!   title merge character by character.
//! - A `bool` or number is a register. Concurrent writes resolve to one of
//!   them, deterministically on every replica.
//! - A `Collection` gives each item a stable id (`cx.id()`), so concurrent
//!   adds keep both items and an edit follows its item wherever it moves.
//! - Each action is one undo step and one attributed version. Undo takes back
//!   the local participant's last action, not a collaborator's.
//!
//! The tests are the lesson: they fork a second replica, edit both sides
//! concurrently and show what the merge keeps.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.shared-checklist";
/// The window and launcher title.
pub const APP_TITLE: &str = "Shared Checklist";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use ui::i18n::I18nManager;
use ui::prelude::*;

/// One item of the checklist.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    /// Collaborative text.
    pub title: String,
    /// A register: concurrent toggles settle on one value.
    pub done: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Checklist {
    /// Collaborative text, like every `String`.
    pub name: String,
    /// The title of the next item, shared so collaborators see it typed.
    pub draft: String,
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

#[resource(kind = "example.checklist", label = "Checklist")]
impl Checklist {
    /// Rename the checklist.
    #[action]
    pub fn rename(&mut self, #[param("Checklist name")] name: String) {
        self.name = name;
    }

    /// Set the title of the next item.
    #[action]
    pub fn set_draft(&mut self, #[param("Title of the next item")] draft: String) {
        self.draft = draft;
    }

    /// Add the drafted item.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx) -> Result<(), &'static str> {
        let title = self.draft.trim().to_owned();
        if title.is_empty() {
            return Err("an item needs a title");
        }
        self.items.push(cx.id(), Item { title, done: false });
        self.draft.clear();
        Ok(())
    }

    /// Retitle an item.
    #[action]
    pub fn retitle(
        &mut self,
        item: &Id,
        #[param("New title")] title: String,
    ) -> Result<(), &'static str> {
        self.items.get_mut(item).ok_or("no such item")?.title = title;
        Ok(())
    }

    /// Check an item off, or back on.
    #[action]
    pub fn set_done(&mut self, item: &Id, #[param("Done")] done: bool) -> Result<(), &'static str> {
        self.items.get_mut(item).ok_or("no such item")?.done = done;
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
    let Some(id) = row.item().cloned() else {
        return Box::new(Column::new());
    };
    let done_id = id.clone();
    let done = row.checkbox(
        "checklist-done-label",
        |item| item.done,
        Checklist::SET_DONE.map(move |done| (done_id.clone(), done)),
    );
    let title = row
        .input(
            |item| item.title.clone(),
            Checklist::RETITLE.map(move |title| (id.clone(), title)),
        )
        .with_placeholder("checklist-title-placeholder");
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

    fn label(checklist: &Checklist) -> String {
        if checklist.name.is_empty() {
            I18nManager::lookup_plain("checklist-untitled")
        } else {
            checklist.name.clone()
        }
    }

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        install_catalog();
        let name = cx
            .input(|checklist| checklist.name.clone(), Checklist::RENAME)
            .with_id("name-input")
            .with_placeholder("checklist-name-placeholder");
        let draft = cx
            .input(|checklist| checklist.draft.clone(), Checklist::SET_DRAFT)
            .with_id("draft-input")
            .with_placeholder("checklist-draft-placeholder");
        let add = cx.button("checklist-add", Checklist::ADD.invoke());
        let progress = cx.text(progress_text).with_id("progress");
        let rows = cx.each(|checklist| &checklist.items, item_row);
        let clear = cx.button("checklist-clear-done", Checklist::CLEAR_DONE.invoke());
        Box::new(ui::column![
            labeled_field("checklist-name-label", name),
            ui::row![labeled_field("checklist-draft-label", draft), add],
            progress,
            rows,
            clear,
        ])
    }
}

ui::dev_reload_entry!(App);
