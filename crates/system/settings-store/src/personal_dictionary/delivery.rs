//! The person's dictionary as an app's settings are served: the snapshot in the app's view, and
//! an edit as a command in its own device-local entry, which is answered but never kept there.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{Edit, snapshot_of};
use crate::SettingsSource;

pub const VIEW_KEY: &str = "__personal_dictionary";
pub const EDIT_KEY: &str = "__personal_dictionary_edit";

/// One edit an app asks for, at the revision it read.
#[derive(Serialize, Deserialize)]
pub struct Command {
    pub revision: u64,
    pub edit: Edit,
}

/// Put the person's dictionary at `VIEW_KEY` in `app`'s device-local entry of `view`.
pub fn project(document: &Map<String, Value>, app: &str, view: &mut Map<String, Value>) {
    let Ok(delivery) = serde_json::to_value(snapshot_of(document)) else {
        return;
    };
    let section = view
        .entry(crate::placement::APP_DEVICE_LOCAL_SECTION.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(section) = section.as_object_mut() else {
        return;
    };
    let entry = section
        .entry(app.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(entry) = entry.as_object_mut() {
        entry.insert(VIEW_KEY.to_owned(), delivery);
    }
}

/// Run the dictionary command a write to `section` carries, if it carries one. Returns whether it
/// was a command: `false` leaves the write to the app's ordinary settings.
///
/// # Errors
///
/// [`io::ErrorKind::PermissionDenied`] when the write names the dictionary's view, mixes the
/// command with other values, or the app may not make the edit; [`io::ErrorKind::InvalidData`]
/// when the command does not decode or is not a valid edit.
pub fn command(
    source: &dyn SettingsSource,
    app: &str,
    section: &str,
    values: &Map<String, Value>,
) -> io::Result<bool> {
    if app.is_empty() {
        return Err(denied());
    }
    if section != crate::placement::APP_DEVICE_LOCAL_SECTION {
        return Ok(false);
    }
    let Some(own) = values.get(app).and_then(Value::as_object) else {
        return Ok(false);
    };
    if own.contains_key(VIEW_KEY) {
        return Err(denied());
    }
    let Some(command) = own.get(EDIT_KEY) else {
        return Ok(false);
    };
    if values.len() != 1 || own.len() != 1 {
        return Err(denied());
    }
    let command: Command = serde_json::from_value(command.clone()).map_err(io::Error::other)?;
    if !matches!(command.edit, Edit::Add(_))
        && !matches!(app, "so.tilo.settings" | "so.tilo.word-processor")
    {
        return Err(denied());
    }
    super::edit_in(source, command.revision, command.edit)?;
    Ok(true)
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "personal dictionary authority is unavailable",
    )
}
