//! What a [`super::DescribedList`] reads of an item beside building its row.

use crate::component::{Component as _, Moment, Person, ResourceId, Source};
use crate::widget::{ListRowProps, Tone};

use super::{DescribedRows, Held, ItemRow, ListItemProps};

#[derive(Default)]
pub(super) struct ItemFacts {
    pub(super) item: Option<ResourceId>,
    pub(super) author: Option<Person>,
    pub(super) time: Option<Moment>,
    pub(super) read_by: Vec<Person>,
    pub(super) change: bool,
    pub(super) group: Option<String>,
    pub(super) group_tone: Option<Tone>,
    pub(super) tail: bool,
    pub(super) removed: bool,
    pub(super) parent: Option<ResourceId>,
    pub(super) opens: bool,
    pub(super) detail: bool,
    pub(super) title: Option<String>,
}

fn prop_of<T: crate::component::PropType + Default>(
    source: &mut dyn Source,
    name: &str,
) -> Result<T, String> {
    match source.prop(name) {
        Some(value) => T::read(&value, source).map_err(|message| format!("{name} {message}")),
        None => Ok(T::default()),
    }
}

impl ItemFacts {
    pub(super) fn read(source: &mut dyn Source) -> Result<Self, String> {
        let mut title = None;
        if matches!(source.slot_kinds("row").as_slice(), [Some(kind)] if kind == ListRowProps::NAME)
        {
            source.components("row", &mut |row| {
                title = Some(prop_of::<String>(row, "title")?);
                Ok(())
            })?;
        }
        Ok(Self {
            item: prop_of(source, "item")?,
            author: prop_of(source, "author")?,
            time: prop_of(source, "time")?,
            read_by: prop_of(source, "read_by")?,
            change: source.holds("change"),
            group: prop_of(source, "group")?,
            group_tone: prop_of(source, "group_tone")?,
            tail: prop_of(source, "tail")?,
            removed: prop_of(source, "removed")?,
            parent: prop_of(source, "parent")?,
            opens: prop_of(source, "opens")?,
            detail: source.holds("detail"),
            title,
        })
    }

    pub(super) fn of(item: &ListItemProps) -> Self {
        Self {
            item: item.item.clone(),
            author: item.author.clone(),
            time: item.time,
            read_by: item.read_by.clone(),
            change: item.change.is_some(),
            group: item.group.clone(),
            group_tone: item.group_tone,
            tail: item.tail,
            removed: item.removed,
            parent: item.parent.clone(),
            opens: item.opens,
            detail: !matches!(item.detail, ItemRow::None),
            title: match &item.row {
                ItemRow::Row(row) => Some(row.title.clone()),
                ItemRow::View(_) | ItemRow::None => None,
            },
        }
    }
}

impl DescribedRows {
    /// Read what the list knows of the item `key` beside its row with `read`: `None` when there
    /// is no such item, or it is not rendered. Nothing of its row is built: the list asks this of
    /// every row it holds whenever it places them.
    pub(super) fn with_facts<R>(&self, key: &str, read: impl FnOnce(ItemFacts) -> R) -> Option<R> {
        if let Some(item) = self.removed.iter().find(|item| item.key == key) {
            return Some(read(ItemFacts::of(item)));
        }
        match &self.held {
            Held::Nothing => None,
            Held::Given(items) => {
                let item = items.iter().find(|item| item.key == key)?;
                Some(read(ItemFacts::of(item)))
            }
            Held::Rendered(rows) => {
                let mut found = None;
                let read_one = &mut |source: &mut dyn Source| {
                    found = Some(ItemFacts::read(source)?);
                    Ok(())
                };
                let shown = rows.try_borrow_mut().ok()?.read_props(key, read_one);
                match shown {
                    Ok(true) => found.map(read),
                    Ok(false) => None,
                    Err(error) => {
                        tracing::warn!(%error, key, "an item of the list did not read");
                        None
                    }
                }
            }
        }
    }
}
