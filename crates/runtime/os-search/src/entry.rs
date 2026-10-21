//! One result the host search holds: a resource and the action that opens it.

use std::fmt;
use std::str::FromStr;

use tilo_actions::remote::{CatalogueRow, RemoteActionTarget};
use tilo_actions::{ConfirmationPolicy, InputSpec, OperationKind};

/// The id a host search holds for one resource. It names the app and the
/// resource without spelling either out, and is all a host hands back on
/// activation: an id this runtime no longer offers opens nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId(u64);

impl EntryId {
    fn of(app_id: &str, resource: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        for part in [app_id, resource] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        let mut bytes = [0; 8];
        hasher.finalize_xof().fill(&mut bytes);
        Self(u64::from_le_bytes(bytes))
    }

    pub(crate) const fn key(self) -> u64 {
        self.0
    }

    pub(crate) const fn from_key(key: u64) -> Self {
        Self(key)
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// A host handed back something `EntryId`'s `Display` never wrote.
#[derive(Debug, PartialEq, Eq)]
pub struct NotAnEntryId;

impl FromStr for EntryId {
    type Err = NotAnEntryId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let exact = text.len() == 16
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !exact {
            return Err(NotAnEntryId);
        }
        u64::from_str_radix(text, 16)
            .map(Self)
            .map_err(|_| NotAnEntryId)
    }
}

/// A resource a host search can find, and how to open it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEntry {
    pub id: EntryId,
    pub app_id: String,
    /// The resource's own label, as the app words it.
    pub title: String,
    /// The app's one-line detail about the resource, when it gives one.
    pub detail: Option<String>,
    /// The opening action, on the publisher that offered it.
    pub target: RemoteActionTarget,
}

impl SearchEntry {
    /// What a host search shows and matches: a change to anything else is
    /// invisible to it.
    pub(crate) fn same_presentation(&self, other: &Self) -> bool {
        self.title == other.title && self.detail == other.detail
    }
}

/// How well a row's action opens its resource; `None` when it does not, or
/// not without a person's input.
///
/// The resource's primary declared view is best, then any declared view,
/// then the conventional `open` view operation.
pub(crate) fn opening(row: &CatalogueRow) -> Option<(u8, SearchEntry)> {
    let action = row.action.as_ref()?;
    let contract = &action.contract;
    let opens_quietly = action.kind == OperationKind::View
        && contract.availability.is_available()
        && contract.advertised_confirmation() == ConfirmationPolicy::None
        && contract.reaches_people()
        && action.input == InputSpec::None
        && action.params.is_empty();
    if !opens_quietly || row.resource.label.trim().is_empty() {
        return None;
    }
    let rank = match &action.view {
        Some(view) if view.primary => 3,
        Some(_) => 2,
        None if action.address.action == OPEN_ACTION => 1,
        None => return None,
    };
    Some((
        rank,
        SearchEntry {
            id: EntryId::of(&row.app_id, &row.resource.id),
            app_id: row.app_id.clone(),
            title: row.resource.label.clone(),
            detail: row
                .resource
                .presentation
                .as_ref()
                .and_then(|presentation| presentation.detail.clone()),
            target: row.target()?,
        },
    ))
}

/// The action id apps give the view operation that opens a resource.
const OPEN_ACTION: &str = "open";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_id_round_trips_only_through_its_exact_form() {
        let id = EntryId::of("so.tilo.notes", "notes:page:1");
        assert_eq!(id.to_string().parse::<EntryId>().unwrap(), id);
        assert_ne!(id, EntryId::of("so.tilo.notes", "notes:page:2"));
        // The app is part of the identity, and the parts cannot run together.
        assert_ne!(
            EntryId::of("a", "bc"),
            EntryId::of("ab", "c"),
            "length-prefixed parts"
        );
        let upper = id.to_string().to_uppercase();
        for foreign in ["", "12", "+000000000000001", "0x00000000000001", &upper] {
            assert!(foreign.parse::<EntryId>().is_err(), "{foreign}");
        }
    }
}
