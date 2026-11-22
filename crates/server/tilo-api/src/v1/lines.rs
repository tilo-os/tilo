//! The lines a person reads for a listing the API answers: the workspaces,
//! keys, people and wait list, as `tilo` prints them from the typed answer.

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

/// A date the answer holds in milliseconds, or `none` when it holds none.
fn when(row: &Value, field: &str, none: &str) -> String {
    row.get(field)
        .and_then(Value::as_u64)
        .map_or_else(|| none.to_owned(), crate::pages::date)
}

/// One workspace: its name, its id, and where it lives, the place the person
/// has in it, its labels and whether it acts unattended.
#[must_use]
pub fn workspace(row: &Value) -> String {
    let mut line = format!("{} (id {}", text(row, "name"), text(row, "id"));
    if let Some(host) = row.get("host").and_then(Value::as_str) {
        let _ = write!(line, ", on {host}");
    }
    if let Some(role) = row.get("role").and_then(Value::as_str) {
        let _ = write!(line, ", {role}");
    }
    for (name, value) in row
        .get("labels")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let _ = write!(line, ", {name}={}", value.as_str().unwrap_or_default());
    }
    if row.get("unattended").and_then(Value::as_bool) == Some(true) {
        line.push_str(", runs commands unattended");
    }
    line.push(')');
    line
}

/// The person's workspaces (`WorkspaceList`), one line each.
#[must_use]
pub fn workspaces(listed: &Value) -> String {
    let lines: Vec<String> = rows(listed, "workspaces").iter().map(workspace).collect();
    if lines.is_empty() {
        "no workspaces".to_owned()
    } else {
        lines.join("\n")
    }
}

/// One key: its name and grant, what it reaches and what kind of key it is,
/// when it was last used and when it expires.
#[must_use]
pub fn key(row: &Value) -> String {
    let mut line = format!(
        "{} (grant {}, reaches {}",
        text(row, "name"),
        text(row, "grant"),
        text(row, "scope")
    );
    let kind = text(row, "kind");
    if !kind.is_empty() {
        let _ = write!(line, ", {kind}");
    }
    let _ = write!(
        line,
        ", last used {}, expires {})",
        when(row, "last_used_at_ms", "never"),
        when(row, "expires_at_ms", "never"),
    );
    line
}

/// The person's keys (`KeyList`), one line each.
#[must_use]
pub fn keys(listed: &Value) -> String {
    let lines: Vec<String> = rows(listed, "keys").iter().map(key).collect();
    if lines.is_empty() {
        "no keys".to_owned()
    } else {
        lines.join("\n")
    }
}

/// A key just minted: what it is, its MCP address and the key itself, which
/// is shown this once.
#[must_use]
pub fn minted_key(minted: &Value) -> String {
    format!(
        "{} (grant {}, reaches {}, expires {})\n  MCP {}\n  key {} (shown once)",
        text(minted, "name"),
        text(minted, "grant"),
        text(minted, "workspace"),
        when(minted, "expires_at_ms", "never"),
        text(minted, "mcp_url"),
        text(minted, "token"),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_workspace_line_names_its_id_and_where_it_lives() {
        let row = json!({"id": "3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b",
            "name": "Applicant: Jane Doe", "host": "eu1.tilo.sh", "role": "owner"});
        assert_eq!(
            workspace(&row),
            "Applicant: Jane Doe (id 3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b, on eu1.tilo.sh, owner)"
        );
        assert_eq!(workspaces(&json!({"workspaces": []})), "no workspaces");
    }
}
