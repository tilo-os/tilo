//! How often a person used each emoji, in the picker's frequent row: kept in the person's
//! settings, so every device of theirs opens the picker with the same ones.

use std::io;

use serde_json::{Map, Value};

use crate::ChangeOrigin;

/// The section of the person's settings the picker's recents are kept in.
pub const SECTION: &str = "emoji";

/// The field holding them, as JSON: each emoji and how many times.
const USED: &str = "used";

/// The counts the settings hold, as JSON text; none when the section says nothing.
#[must_use]
pub fn used(doc: &Map<String, Value>) -> Option<String> {
    doc.get(SECTION)
        .and_then(|section| section.get(USED))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Keep `used` (JSON: each emoji and how many times) as the emoji's counts, in place of what the
/// section held, as one change `origin` made.
///
/// # Errors
///
/// Why the settings could not be written: this program installed none.
pub fn keep(used: &str, origin: &ChangeOrigin) -> io::Result<()> {
    let section = Map::from_iter([(USED.to_owned(), Value::String(used.to_owned()))]);
    crate::update_section_by(SECTION, section, origin)
}
