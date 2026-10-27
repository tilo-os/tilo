//! The store's open writes, as the index would hold them: what the store wrote into its
//! mirror and the record does not hold yet, derived field by field like the index
//! ([`super::derive`]), over the index as its last commit left it. Reading through it is
//! reading the index as the writes leave it; the record's next change drops it, as the mirror
//! drops its open transaction.

use std::collections::{BTreeSet, HashMap};

use tilolang_runtime::store::FieldValue;

use super::derive::Sink;
use super::lists::Entry;
use super::{IndexError, Snapshot};

/// What one list became: entries added to and removed from the index's, or (a list the
/// writes made anew) all of them.
#[derive(Clone, Debug)]
pub enum ListState {
    Delta {
        added: BTreeSet<Entry>,
        removed: BTreeSet<Entry>,
    },
    Whole(Vec<Entry>),
}

/// The open writes.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    fields: HashMap<(String, String), FieldValue>,
    texts: HashMap<(String, String), String>,
    /// Resources removed, with everything inside them, unless written again.
    removed: BTreeSet<String>,
    lists: HashMap<String, ListState>,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
            && self.texts.is_empty()
            && self.removed.is_empty()
            && self.lists.is_empty()
    }

    /// Whether `resource`, or one holding it, was removed.
    pub fn removed(&self, resource: &str) -> bool {
        let mut at = resource;
        loop {
            if self.removed.contains(at) {
                return true;
            }
            match at.rsplit_once('/') {
                Some((parent, _)) => at = parent,
                None => return false,
            }
        }
    }

    /// The value written of `field` of `resource`: `Some(None)` when it was removed.
    pub fn field(&self, resource: &str, field: &str) -> Option<Option<FieldValue>> {
        if let Some(value) = self.fields.get(&(resource.to_owned(), field.to_owned())) {
            return Some(Some(value.clone()));
        }
        self.removed(resource).then_some(None)
    }

    /// The type or order key written of `resource`: `Some(None)` when it was removed.
    pub fn text(&self, resource: &str, key: &str) -> Option<Option<String>> {
        if let Some(text) = self.texts.get(&(resource.to_owned(), key.to_owned())) {
            return Some(Some(text.clone()));
        }
        self.removed(resource).then_some(None)
    }

    /// What became of the list `list` of the collection `field` of `resource`: `None` when
    /// it is the index's.
    pub fn list(&self, resource: &str, list: &str) -> Option<ListState> {
        if let Some(state) = self.lists.get(list) {
            return Some(state.clone());
        }
        self.removed(resource).then(|| ListState::Whole(Vec::new()))
    }

    /// The items of the collection `field` of `resource` the writes touched: added, removed or
    /// moved, or one of their fields written.
    pub fn touched_items(&self, resource: &str, field: &str, list: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        match self.lists.get(list) {
            Some(ListState::Delta { added, removed }) => {
                out.extend(added.iter().chain(removed).map(|(_, id)| id.clone()));
            }
            Some(ListState::Whole(entries)) => {
                out.extend(entries.iter().map(|(_, id)| id.clone()));
            }
            None => {}
        }
        let prefix = format!("{resource}/{field}:");
        let direct = |path: &str| -> Option<String> {
            let rest = path.strip_prefix(&prefix)?;
            Some(rest.split('/').next().unwrap_or(rest).to_owned())
        };
        out.extend(self.fields.keys().filter_map(|(path, _)| direct(path)));
        out.extend(self.texts.keys().filter_map(|(path, _)| direct(path)));
        out.extend(self.removed.iter().filter_map(|path| direct(path)));
        out
    }

    fn drop_within(&mut self, resource: &str) {
        let inside = format!("{resource}/");
        let within = |path: &str| path == resource || path.starts_with(&inside);
        self.fields.retain(|(path, _), _| !within(path));
        self.texts.retain(|(path, _), _| !within(path));
        self.removed.retain(|path| !within(path));
        let lists_inside = [
            format!("m\u{1}{resource}\u{1}"),
            format!("m\u{1}{resource}/"),
        ];
        self.lists
            .retain(|list, _| !lists_inside.iter().any(|p| list.starts_with(p.as_str())));
    }
}

/// The open writes over the index they are written against: a [`Sink`].
pub struct OverlaySink<'a> {
    pub overlay: &'a mut Overlay,
    pub snapshot: Option<&'a Snapshot>,
}

impl OverlaySink<'_> {
    fn list_resource(list: &str) -> Option<&str> {
        let rest = list.strip_prefix("m\u{1}")?;
        rest.split_once('\u{1}').map(|(resource, _)| resource)
    }

    fn state(&mut self, list: &str) -> &mut ListState {
        let removed = Self::list_resource(list).is_some_and(|r| self.overlay.removed(r));
        self.overlay
            .lists
            .entry(list.to_owned())
            .or_insert_with(|| {
                if removed {
                    ListState::Whole(Vec::new())
                } else {
                    ListState::Delta {
                        added: BTreeSet::new(),
                        removed: BTreeSet::new(),
                    }
                }
            })
    }
}

impl Sink for OverlaySink<'_> {
    fn field(&mut self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError> {
        if let Some(written) = self.overlay.field(resource, field) {
            return Ok(written);
        }
        match self.snapshot {
            Some(snapshot) => snapshot.field(resource, field),
            None => Ok(None),
        }
    }

    fn text(&mut self, resource: &str, key: &str) -> Result<Option<String>, IndexError> {
        if let Some(written) = self.overlay.text(resource, key) {
            return Ok(written);
        }
        match self.snapshot {
            Some(snapshot) => snapshot.text(resource, key),
            None => Ok(None),
        }
    }

    fn put_field(
        &mut self,
        resource: &str,
        field: &str,
        value: &FieldValue,
    ) -> Result<(), IndexError> {
        self.overlay
            .fields
            .insert((resource.to_owned(), field.to_owned()), value.clone());
        Ok(())
    }

    fn put_text(&mut self, resource: &str, key: &str, text: &str) -> Result<(), IndexError> {
        self.overlay
            .texts
            .insert((resource.to_owned(), key.to_owned()), text.to_owned());
        Ok(())
    }

    fn remove(&mut self, resource: &str) -> Result<(), IndexError> {
        self.overlay.drop_within(resource);
        self.overlay.removed.insert(resource.to_owned());
        Ok(())
    }

    fn doc(&mut self, _doc: &str, _ty: Option<&str>) -> Result<(), IndexError> {
        // The store keeps which records there are.
        Ok(())
    }

    fn insert(&mut self, list: &str, entry: Entry) -> Result<(), IndexError> {
        match self.state(list) {
            ListState::Whole(entries) => {
                if let Err(at) = entries.binary_search(&entry) {
                    entries.insert(at, entry);
                }
            }
            ListState::Delta { added, removed } => {
                if !removed.remove(&entry) {
                    added.insert(entry);
                }
            }
        }
        Ok(())
    }

    fn remove_entry(&mut self, list: &str, entry: &Entry) -> Result<(), IndexError> {
        match self.state(list) {
            ListState::Whole(entries) => entries.retain(|held| held != entry),
            ListState::Delta { added, removed } => {
                if !added.remove(entry) {
                    removed.insert(entry.clone());
                }
            }
        }
        Ok(())
    }

    fn replace(&mut self, list: &str, entries: Vec<Entry>) -> Result<(), IndexError> {
        self.overlay
            .lists
            .insert(list.to_owned(), ListState::Whole(entries));
        Ok(())
    }

    fn entries(&mut self, list: &str) -> Result<Vec<Entry>, IndexError> {
        let resource = Self::list_resource(list).unwrap_or_default().to_owned();
        let state = self.overlay.list(&resource, list);
        let base = || -> Result<Vec<Entry>, IndexError> {
            let Some(snapshot) = self.snapshot else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let dir = super::lists::read_dir(snapshot, list)?;
            for page in &dir.pages {
                out.extend(super::lists::read_page(snapshot, list, page.seq)?);
            }
            Ok(out)
        };
        Ok(match state {
            Some(ListState::Whole(entries)) => entries,
            Some(ListState::Delta { added, removed }) => {
                let mut all: BTreeSet<Entry> = base()?.into_iter().collect();
                for gone in &removed {
                    all.remove(gone);
                }
                all.extend(added);
                all.into_iter().collect()
            }
            None => base()?,
        })
    }

    fn drop_lists(&mut self, _prefix: &str) -> Result<(), IndexError> {
        // Filter columns are the index's: the store reads the open writes' items against them.
        Ok(())
    }

    fn words(
        &mut self,
        _resource: &str,
        _old: Option<&str>,
        _new: Option<&str>,
        _weight: u8,
    ) -> Result<(), IndexError> {
        // Search reads the index once the record holds the writes.
        Ok(())
    }

    fn keeps_columns(&self) -> bool {
        false
    }
}
