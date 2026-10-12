use ui::i18n::I18nManager;
use ui::prelude::*;

/// One item. Its title is collaborative text.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    /// Names the item in history, search and the palette.
    #[model(label)]
    pub title: String,
    pub done: bool,
}

/// The app's state: the items, in the order they were added.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Items {
    pub items: Collection<Item>,
}

/// An item's title and done flag are edited at their paths
/// (`Items::paths().items.item(&id).done`). Verbs that take an item's `&Id`
/// are item verbs: the palette and agents see each item as a resource of its
/// own with them on it, and a row's context menu (right-click, the Menu key,
/// Shift+F10) offers them.
#[resource(kind = "generated.items", label = "Items")]
impl Items {
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

    /// Remove an item. Undo brings it back.
    #[action]
    pub fn remove(&mut self, item: &Id) -> Result<(), &'static str> {
        self.items.remove(item).map(drop).ok_or("no such item")
    }
}

/// One row: built once per item and kept while the item exists, so focus and
/// the caret survive other items changing. Its controls are bound to the
/// item's own paths.
fn item_row(row: &mut ViewCx<Items, Item>) -> Element {
    let Some(id) = row.item().cloned() else {
        return Box::new(Column::new());
    };
    let p = row.paths();
    let done = row.checkbox("collection-done-label", p.done);
    let title = row
        .input(p.title)
        .with_placeholder("collection-title-placeholder");
    let remove = row.button("collection-remove", Items::REMOVE.with(id));
    Box::new(ui::row![done, title, remove])
}

pub struct App;

impl ResourceApp for App {
    type Resource = Items;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn view(cx: &mut ViewCx<Items>) -> Element {
        // The next item's title is a form: this window's own, never synced or
        // undone, until Add submits it as one invocation of `add`.
        let next = cx.form::<items::Add>();
        let draft = cx
            .input(next.paths().title)
            .with_id("draft-input")
            .with_placeholder("collection-draft-placeholder");
        let add = cx.button("collection-add", next.submit());
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
