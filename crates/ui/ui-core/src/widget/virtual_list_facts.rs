//! What a [`super::DescribedList`] reads of an item beside building its row.

use std::collections::HashMap;

use crate::widget::ListRowProps;
use tilo_ui_base::widget::Tone;
use tilo_ui_base::{
    component::Component as _, component::Event, component::Moment, component::Person,
    component::ResourceId, component::Source,
};

use super::{DescribedRows, Held, ItemRow, ListItemProps};

#[derive(Clone, Default)]
pub(super) struct ItemFacts {
    pub(super) item: Option<ResourceId>,
    pub(super) value: Option<String>,
    /// What the item's row does when it is activated: run for a row that stands for a value.
    pub(super) activate: Event,
    pub(super) author: Option<Person>,
    pub(super) time: Option<Moment>,
    pub(super) read_by: Vec<Person>,
    pub(super) change: bool,
    pub(super) group: Option<String>,
    pub(super) group_tone: Option<Tone>,
    pub(super) tail: bool,
    pub(super) done: bool,
    pub(super) removed: bool,
    pub(super) parent: Option<ResourceId>,
    pub(super) opens: bool,
    pub(super) detail: bool,
    pub(super) title: Option<String>,
}

fn prop_of<T: tilo_ui_base::component::PropType + Default>(
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
        let mut activate = Event::None;
        if matches!(source.part_kinds("row").as_slice(), [Some(kind)] if kind == ListRowProps::NAME)
        {
            source.components("row", &mut |row| {
                title = Some(prop_of::<String>(row, "title")?);
                activate = row.event("activate");
                Ok(())
            })?;
        }
        Ok(Self {
            item: prop_of(source, "item")?,
            value: prop_of(source, "value")?,
            author: prop_of(source, "author")?,
            time: prop_of(source, "time")?,
            read_by: prop_of(source, "read_by")?,
            change: source.holds("change"),
            group: prop_of(source, "group")?,
            group_tone: prop_of(source, "group_tone")?,
            tail: prop_of(source, "tail")?,
            done: prop_of(source, "done")?,
            removed: prop_of(source, "removed")?,
            parent: prop_of(source, "parent")?,
            opens: prop_of(source, "opens")?,
            detail: source.holds("detail"),
            title,
            activate,
        })
    }

    pub(super) fn of(item: &ListItemProps) -> Self {
        Self {
            item: item.item.clone(),
            value: item.value.clone(),
            author: item.author.clone(),
            time: item.time,
            read_by: item.read_by.clone(),
            change: item.change.is_some(),
            group: item.group.clone(),
            group_tone: item.group_tone,
            tail: item.tail,
            done: item.done,
            removed: item.removed,
            parent: item.parent.clone(),
            opens: item.opens,
            detail: !matches!(item.detail, ItemRow::None),
            title: match &item.row {
                ItemRow::Row(row) => Some(row.title.clone()),
                ItemRow::View(_) | ItemRow::None => None,
            },
            activate: match &item.row {
                ItemRow::Row(row) => row.activate.clone(),
                ItemRow::View(_) | ItemRow::None => Event::None,
            },
        }
    }
}

/// What a list read of its rendered rows, each kept while its source says the row stands as it
/// was read ([`tilo_ui_base::component::RowSource::revision`]): the list asks it of every row it holds
/// on every pass, and a row's item is decoded again only when the row changed.
#[derive(Default)]
pub(super) struct KeptReads {
    facts: HashMap<String, (u64, ItemFacts)>,
    menus: HashMap<String, (u64, bool)>,
}

impl KeptReads {
    /// Forget what was read of rows that changed since, or are no longer rendered.
    pub(super) fn retain(&mut self, revision: impl Fn(&str) -> Option<u64>) {
        self.facts.retain(|key, (at, _)| revision(key) == Some(*at));
        self.menus.retain(|key, (at, _)| revision(key) == Some(*at));
    }

    pub(super) fn clear(&mut self) {
        self.facts.clear();
        self.menus.clear();
    }
}

impl DescribedRows {
    /// The rendered row `key` as it stands, when its source numbers it: `None` for a row the
    /// list builds itself or is given whole, which costs nothing to read again.
    fn revision(&self, key: &str) -> Option<u64> {
        if self.is_removed(key) {
            return None;
        }
        match &self.held {
            Held::Rendered(rows) => rows.try_borrow().ok()?.revision(key),
            Held::Nothing | Held::Given(_) => None,
        }
    }

    /// Whether the row of `key` offers a menu: what its item offers on one, or the view put on
    /// it. Read from the row's built item once per change of the row.
    pub(super) fn offers_menu(&self, key: &str) -> bool {
        let revision = self.revision(key);
        if let Some(revision) = revision {
            let kept = self.kept.borrow().menus.get(key).copied();
            if let Some((at, offers)) = kept
                && at == revision
            {
                return offers;
            }
        }
        let offers = self
            .with_item(key, |item| {
                item.offers_menu || !item.menu.is_empty() || !item.attached.menu.is_empty()
            })
            .unwrap_or(false);
        if let Some(revision) = revision {
            self.kept
                .borrow_mut()
                .menus
                .insert(key.to_owned(), (revision, offers));
        }
        offers
    }

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
                // Numbered before it is read: a read that changes the row (a bound control shown
                // anew) leaves what it read kept under the number before, read again next time.
                let revision = rows.try_borrow().ok()?.revision(key);
                if let Some(revision) = revision {
                    let kept = self.kept.borrow().facts.get(key).cloned();
                    if let Some((at, facts)) = kept
                        && at == revision
                    {
                        return Some(read(facts));
                    }
                }
                let mut found = None;
                let read_one = &mut |source: &mut dyn Source| {
                    found = Some(ItemFacts::read(source)?);
                    Ok(())
                };
                let shown = rows.try_borrow_mut().ok()?.read_props(key, read_one);
                match shown {
                    Ok(true) => {
                        let facts = found?;
                        if let Some(revision) = revision {
                            self.kept
                                .borrow_mut()
                                .facts
                                .insert(key.to_owned(), (revision, facts.clone()));
                        }
                        Some(read(facts))
                    }
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
