//! [`Recents`]: the things used lately, newest first, each once, at most
//! `KEEP` of them. One type for every app's recent files, folders, projects,
//! searches and videos, whether a device-local setting keeps the list or a
//! resource shares it.

use automerge::{ObjId, ScalarValue, Value};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::model::{Doc, Model, ModelError, Slot};

/// What was used lately, newest first, each item once, at most `KEEP`.
///
/// Stored as a plain list. Reading one back drops the items that no longer
/// decode and keeps the newest `KEEP`, so one damaged row costs that row,
/// never the list (and anything but a list reads as an empty one). In a resource the list is one value: two people
/// remembering at once keep one whole list rather than an interleaving.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Recents<T, const KEEP: usize> {
    items: Vec<T>,
}

impl<T, const KEEP: usize> Default for Recents<T, KEEP> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T, const KEEP: usize> Recents<T, KEEP> {
    /// How many items are kept.
    pub const KEEP: usize = KEEP;

    /// The items, newest first.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Put `item` first, dropping any earlier item `same` matches it with and
    /// the oldest beyond `KEEP`.
    pub fn remember_by(&mut self, item: T, same: impl Fn(&T, &T) -> bool) {
        self.items.retain(|kept| !same(kept, &item));
        self.items.insert(0, item);
        self.items.truncate(KEEP);
    }

    /// Keep only the items `keep` accepts. Returns whether any went.
    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) -> bool {
        let before = self.items.len();
        self.items.retain(keep);
        self.items.len() != before
    }

    /// The items to change in place (a moved file's path, a position).
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.items.iter_mut()
    }

    /// Reorder the items by `key`, ascending.
    pub fn sort_by_key<K: Ord>(&mut self, key: impl FnMut(&T) -> K) {
        self.items.sort_by_key(key);
    }

    /// Forget every item.
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

impl<T: PartialEq, const KEEP: usize> Recents<T, KEEP> {
    /// Put `item` first, dropping an equal earlier one and the oldest beyond
    /// `KEEP`.
    pub fn remember(&mut self, item: T) {
        self.remember_by(item, PartialEq::eq);
    }

    /// Forget `item`. Returns whether it was there.
    pub fn forget(&mut self, item: &T) -> bool {
        self.retain(|kept| kept != item)
    }
}

impl<T, const KEEP: usize> std::ops::Deref for Recents<T, KEEP> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<'a, T, const KEEP: usize> IntoIterator for &'a Recents<T, KEEP> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a, T, const KEEP: usize> IntoIterator for &'a mut Recents<T, KEEP> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter_mut()
    }
}

/// Items given newest first; the oldest beyond `KEEP` are dropped.
impl<T, const KEEP: usize> FromIterator<T> for Recents<T, KEEP> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        Self {
            items: items.into_iter().take(KEEP).collect(),
        }
    }
}

impl<'de, T: DeserializeOwned, const KEEP: usize> Deserialize<'de> for Recents<T, KEEP> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Anything but a list reads as an empty one.
        let serde_json::Value::Array(rows) = serde_json::Value::deserialize(deserializer)? else {
            return Ok(Self::default());
        };
        Ok(rows
            .into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect())
    }
}

impl<T: Serialize + DeserializeOwned + 'static, const KEEP: usize> Recents<T, KEEP> {
    fn from_json_value(value: serde_json::Value) -> Result<Self, ModelError> {
        serde_json::from_value(value).map_err(|error| ModelError::invalid(error.to_string()))
    }
}

/// Stored as one JSON value, written whole.
impl<T: Serialize + DeserializeOwned + 'static, const KEEP: usize> Model for Recents<T, KEEP> {
    crate::model::leaf_paths!();
    #[inline(never)]
    fn to_json(&self) -> serde_json::Value {
        crate::__private::json_via_bytes(self).unwrap_or(serde_json::Value::Null)
    }
    #[inline(never)]
    fn from_json(value: &serde_json::Value) -> Result<Self, ModelError> {
        Self::from_json_value(value.clone())
    }
    #[inline(never)]
    fn load(doc: &Doc, value: &Value<'_>, id: &ObjId) -> Result<Self, ModelError> {
        let text = String::load(doc, value, id)?;
        crate::__private::value_of_bytes(text.as_bytes())
            .map_err(|error| ModelError::invalid(error.to_string()))
            .and_then(Self::from_json_value)
    }
    fn missing() -> Result<Self, ModelError> {
        Ok(Self::default())
    }
    #[inline(never)]
    fn store(&self, doc: &mut Doc, slot: Slot<'_>) -> Result<(), ModelError> {
        let text =
            serde_json::to_string(self).map_err(|error| ModelError::invalid(error.to_string()))?;
        slot.put_scalar(doc, ScalarValue::Str(text.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_leads_each_item_once_and_the_oldest_go() {
        let mut recents = Recents::<u32, 3>::default();
        for item in [1, 2, 3, 2, 4] {
            recents.remember(item);
        }
        assert_eq!(recents.items(), [4, 2, 3]);
        assert!(recents.forget(&2));
        assert!(!recents.forget(&2));
        let mut searches = Recents::<String, 3>::default();
        searches.remember("Coffee".into());
        searches.remember_by("coffee".into(), |a, b| a.eq_ignore_ascii_case(b));
        assert_eq!(searches.items(), ["coffee"]);
    }

    #[test]
    fn a_stored_list_drops_damaged_rows_and_keeps_the_newest() {
        let recents: Recents<u32, 2> = serde_json::from_str(r#"[1, "two", 3, 4]"#).unwrap();
        assert_eq!(recents.items(), [1, 3]);
        assert_eq!(serde_json::to_string(&recents).unwrap(), "[1,3]");
        let damaged: Recents<u32, 2> = serde_json::from_str(r#"{"rows": 1}"#).unwrap();
        assert!(damaged.is_empty());
    }
}
