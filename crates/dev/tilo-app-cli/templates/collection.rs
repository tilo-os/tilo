use ui::i18n::I18nManager;
use ui::prelude::*;

/// One item. Its title is collaborative text.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    pub title: String,
    pub done: bool,
}

/// The app's state: the items, in the order they were added, and the title of
/// the next one. The draft is shared too, so collaborators see it being typed.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Items {
    pub draft: String,
    pub items: Collection<Item>,
}

/// Actions that take an item's `&Id` are item actions: the palette and agents
/// see each item as a resource of its own with these actions on it, and a
/// row's context menu (right-click, the Menu key, Shift+F10) offers them.
#[resource(kind = "generated.items", label = "Items")]
impl Items {
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

    /// Rename an item.
    #[action]
    pub fn rename(
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

    /// Remove an item. Undo brings it back.
    #[action]
    pub fn remove(&mut self, item: &Id) -> Result<(), &'static str> {
        self.items.remove(item).map(drop).ok_or("no such item")
    }
}

/// One row: built once per item and kept while the item exists, so focus and
/// the caret survive other items changing.
fn item_row(row: &mut ViewCx<Items, Item>) -> Element {
    let Some(id) = row.item().cloned() else {
        return Box::new(Column::new());
    };
    let done_id = id.clone();
    let done = row.checkbox(
        "collection-done-label",
        |item| item.done,
        Items::SET_DONE.map(move |done| (done_id.clone(), done)),
    );
    let title_id = id.clone();
    let title = row
        .input(
            |item| item.title.clone(),
            Items::RENAME.map(move |title| (title_id.clone(), title)),
        )
        .with_placeholder("collection-title-placeholder");
    let remove = row.button("collection-remove", Items::REMOVE.with(id));
    Box::new(ui::row![done, title, remove])
}

pub struct App;

impl ResourceApp for App {
    type Resource = Items;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn view(cx: &mut ViewCx<Items>) -> Element {
        crate::install_catalog();
        let draft = cx
            .input(|items| items.draft.clone(), Items::SET_DRAFT)
            .with_id("draft-input")
            .with_placeholder("collection-draft-placeholder");
        let add = cx.button("collection-add", Items::ADD.invoke());
        // Apps start empty; say what to do instead of showing a blank list.
        let empty = cx.value(|items| items.items.is_empty());
        let hint = ModelView::from_state(&empty, |empty| -> Element {
            if *empty {
                Box::new(
                    EmptyState::new(I18nManager::lookup_plain("collection-empty-title"))
                        .with_message(I18nManager::lookup_plain("collection-empty-message")),
                )
            } else {
                Box::new(Column::new())
            }
        });
        let rows = cx.each(|items| &items.items, item_row);
        Box::new(ui::column![
            ui::row![labeled_field("collection-draft-label", draft), add],
            hint,
            rows,
        ])
    }
}
