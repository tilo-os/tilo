//! What the tracker's listing says of an issue, and the words its read takes
//! in `where`.
//!
//! The read the issues' chain publishes answers a row for each issue. A row
//! of the tracker says what a person scans a list of issues by ([`LISTED`]),
//! so where a project stands is one read and not one for each issue. Its
//! `where` takes an issue's own fields, and two things as people name them:
//! the project an issue works towards, and the issue it stands under.

use std::rc::Rc;

use serde_json::{Map, Value, json};

use super::{Held, IssuePart};
use crate::model::Tracker;

/// What a row says of its issue when the caller names no fields: its number,
/// where it stands, who has it and who gave it to them since when, what it
/// stands under, when it is wanted, how urgent it is, its labels, the project
/// it works towards, and what its work keeps while it runs long: what it
/// waits for and since when, its budget, and why it stopped.
pub(super) const LISTED: &[&str] = &[
    "number",
    "status",
    "assignee",
    "assigned_by",
    "assigned_ms",
    "parent",
    "due_ms",
    "priority",
    "labels",
    "area",
    "work",
];

/// Turn `project` (a project or an area by its name or id; null for the
/// issues that work towards none) and `parent` (an issue by its number, its
/// name or its id) into what an issue keeps of them.
///
/// # Errors
/// Refuses a project the tracker does not have and a parent that is no
/// issue, each with the ones there are.
pub(super) fn prepare_read(
    filter: &mut Map<String, Value>,
    tracker: &Tracker,
    parts: &[(u64, Rc<IssuePart>)],
) -> Result<(), String> {
    match filter.remove("project") {
        None => {}
        Some(Value::Null) => {
            filter.insert("area".into(), Value::Null);
        }
        Some(Value::String(named)) => {
            let area = tracker
                .area_named(&named)
                .map_err(|refusal| refusal.to_string())?;
            filter.insert("area.resource".into(), json!(area.as_str()));
            filter.insert("area.kind".into(), json!(crate::model::KIND));
        }
        Some(_) => return Err("`project` is a project's or an area's name or id".into()),
    }
    if let Some(parent) = filter.get_mut("parent")
        && let Some(named) = parent.as_str()
    {
        let id = Held(parts).named(named)?;
        *parent = json!(id.as_str());
    }
    Ok(())
}
