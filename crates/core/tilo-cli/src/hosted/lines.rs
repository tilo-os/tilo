//! The lines a person reads for what a workspace answers of a path: a listing and a read, as
//! `tilo` prints them from the typed answer.

use std::fmt::Write as _;

use serde_json::Value;

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

fn rows<'a>(listed: &'a Value, name: &str) -> &'a [Value] {
    listed
        .get(name)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// What a path holds (`Listing`): the path and its type, a row each with its type (`/` after
/// one that holds more), the resource's own actions, and the cursor for the rest.
#[must_use]
pub fn listing(listed: &Value) -> String {
    let mut said = text(listed, "path").to_owned();
    let kind = text(listed, "kind");
    if !kind.is_empty() {
        let _ = write!(said, "  ({kind})");
    }
    for row in rows(listed, "rows") {
        let more = if row.get("has_children") == Some(&Value::Bool(true)) {
            "/"
        } else {
            ""
        };
        let _ = write!(said, "\n{}{more}  {}", text(row, "name"), text(row, "kind"));
    }
    for action in rows(listed, "actions") {
        let _ = write!(said, "\n#{}  {}", text(action, "id"), text(action, "label"));
    }
    match listed.get("next_cursor").and_then(Value::as_str) {
        Some(next) => {
            let _ = write!(said, "\n… more: cursor \"{next}\"");
        }
        None if rows(listed, "rows").is_empty() && rows(listed, "actions").is_empty() => {
            said.push_str("\n(nothing here)");
        }
        None => {}
    }
    said
}

/// One resource read (`Resource`): its markdown when it is written in it, else what it holds
/// a line each, else its fields; an app's surface as it is published.
#[must_use]
pub fn resource(read: &Value) -> String {
    let fields = read.get("fields").unwrap_or(&Value::Null);
    if let Some(markdown) = fields.get("markdown").and_then(Value::as_str) {
        return match fields.get("title").and_then(Value::as_str) {
            Some(title) => format!("# {title}\n{markdown}"),
            None => markdown.to_owned(),
        };
    }
    let children = rows(read, "children");
    if !children.is_empty() {
        let mut said = String::new();
        for child in children {
            let _ = writeln!(said, "{}  ({})", text(child, "name"), text(child, "path"));
        }
        return said.trim_end().to_owned();
    }
    let shown = if read.get("surface").is_some() {
        read
    } else {
        fields
    };
    serde_json::to_string_pretty(shown).unwrap_or_default()
}
