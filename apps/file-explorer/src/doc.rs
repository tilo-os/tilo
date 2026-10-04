//! The Files document: the person's bookmarked places, in order.
//!
//! What a window shows — the folder, its listing, the selection, the
//! clipboard, navigation history — is the window's own, and how it shows it
//! (list or grid, sort, item size, hidden files) is the person's preference
//! ([`crate::resource::FilesPreferences`]). The places are stored as
//! [`crate::places`] names them, so they resolve on every device.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{SortBy, SortOrder, ViewMode};

const PLACES: &str = "places";

/// Stateless helpers over the Files document.
pub struct PlacesDoc;

impl PlacesDoc {
    fn existing_list(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, PLACES) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
        }
    }

    fn list(doc: &mut AutoCommit) -> Result<ObjId, automerge::AutomergeError> {
        match Self::existing_list(doc) {
            Some(list) => Ok(list),
            None => doc.put_object(&ROOT, PLACES, ObjType::List),
        }
    }

    /// A new document's places: [`crate::places::DEFAULT_PLACES`].
    ///
    /// # Errors
    /// The document's write error.
    pub fn initialize(doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
        let list = doc.put_object(&ROOT, PLACES, ObjType::List)?;
        for (index, place) in crate::places::DEFAULT_PLACES.iter().enumerate() {
            doc.insert(&list, index, *place)?;
        }
        Ok(())
    }

    /// The bookmarked places, in order.
    pub fn places(doc: &AutoCommit) -> Vec<String> {
        let Some(list) = Self::existing_list(doc) else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| match doc.get(&list, index) {
                Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
                _ => None,
            })
            .collect()
    }

    fn position(doc: &AutoCommit, list: &ObjId, place: &str) -> Option<usize> {
        (0..doc.length(list)).find(|index| {
            matches!(doc.get(list, *index), Ok(Some((Value::Scalar(value), _)))
                if value.as_str() == Some(place))
        })
    }

    /// Bookmark `place` at `at` (the end when `None` or past it), unless it is
    /// bookmarked already.
    ///
    /// # Errors
    /// The document's write error.
    pub fn add(
        doc: &mut AutoCommit,
        place: &str,
        at: Option<usize>,
    ) -> Result<(), automerge::AutomergeError> {
        let list = Self::list(doc)?;
        if Self::position(doc, &list, place).is_some() {
            return Ok(());
        }
        let len = doc.length(&list);
        doc.insert(&list, at.unwrap_or(len).min(len), place)
    }

    /// Forget `place`.
    ///
    /// # Errors
    /// The document's write error.
    pub fn remove(doc: &mut AutoCommit, place: &str) -> Result<(), automerge::AutomergeError> {
        let Some(list) = Self::existing_list(doc) else {
            return Ok(());
        };
        while let Some(index) = Self::position(doc, &list, place) {
            doc.delete(&list, index)?;
        }
        Ok(())
    }

    /// Move `place` to position `to` among the places.
    ///
    /// # Errors
    /// The document's write error.
    pub fn move_to(
        doc: &mut AutoCommit,
        place: &str,
        to: usize,
    ) -> Result<(), automerge::AutomergeError> {
        let Some(list) = Self::existing_list(doc) else {
            return Ok(());
        };
        let Some(from) = Self::position(doc, &list, place) else {
            return Ok(());
        };
        doc.delete(&list, from)?;
        let len = doc.length(&list);
        doc.insert(&list, to.min(len), place)
    }
}

pub fn view_mode_token(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::List => "list",
        ViewMode::Grid => "grid",
    }
}

pub fn parse_view_mode(token: &str) -> ViewMode {
    match token {
        "grid" => ViewMode::Grid,
        _ => ViewMode::List,
    }
}

pub fn sort_key_token(sort: SortBy) -> &'static str {
    match sort {
        SortBy::Name => "name",
        SortBy::Size => "size",
        SortBy::Modified => "modified",
        SortBy::Type => "type",
    }
}

pub fn parse_sort_key(token: &str) -> SortBy {
    match token {
        "size" => SortBy::Size,
        "modified" => SortBy::Modified,
        "type" => SortBy::Type,
        _ => SortBy::Name,
    }
}

pub fn sort_order_token(order: SortOrder) -> &'static str {
    match order {
        SortOrder::Ascending => "ascending",
        SortOrder::Descending => "descending",
    }
}

pub fn parse_sort_order(token: &str) -> SortOrder {
    match token {
        "descending" => SortOrder::Descending,
        _ => SortOrder::Ascending,
    }
}
