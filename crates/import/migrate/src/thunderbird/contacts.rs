//! An address book's cards as vCard text, which Tilo Contacts imports whole.
//!
//! The cards are read from the book as it is on this computer and written back in the one format
//! every address book reads, so the app that keeps them decides what a card means and nothing
//! here copies its shape.

use std::path::Path;

use super::personal_data::{self, Sidecar};
use super::{MAX_SIDECAR_BYTES, find_reviewed, find_sidecar, read_reviewed};

/// The cards of one address book as vCard text.
///
/// `profile` names a profile by its `source_id`, its label or one of its
/// accounts' addresses; `book` names a book by its `source_id` or its name.
///
/// # Errors
///
/// The home holds no such profile or book, the book is not a local file, or it
/// could not be read within its bounds.
pub fn address_book(home: &Path, profile: &str, book: &str) -> Result<String, String> {
    let scan = super::scan(home);
    let reviewed = find_reviewed(&scan.reviewed, profile)
        .ok_or_else(|| format!("no Thunderbird profile {profile:?}"))?;
    let file = find_sidecar(&reviewed.address_books, book)
        .ok_or_else(|| format!("no address book {book:?} in that profile"))?;
    if !file.present || !file.local {
        return Err("that address book is not a local file".into());
    }
    let relative = file
        .relative
        .as_deref()
        .ok_or_else(|| "the address book has no file".to_owned())?;
    let bytes =
        read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES).map_err(|error| error.to_string())?;
    personal_data::canonical(Sidecar::AddressBook, &bytes)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "that address book holds no cards this reader understands".to_owned())
}
