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

/// A workspace's people and its open invitations (`Members`).
#[must_use]
pub fn members(listed: &Value) -> String {
    let members = rows(listed, "members");
    let mut said = String::from("members:");
    for member in members {
        // One place each: an owner is an administrator with a role beneath, named by the first.
        let what = Some(text(member, "place"))
            .filter(|place| !place.is_empty())
            .unwrap_or_else(|| text(member, "role"));
        let _ = write!(said, "\n  {} — {what}", text(member, "name"));
        // A person is named by their key, which `role` and `rm` take.
        if let Some(key) = member.pointer("/member/person").and_then(Value::as_str) {
            let _ = write!(said, "  {key}");
        }
    }
    if members.is_empty() {
        said.push_str("\n  nobody yet");
    }
    let invitations = rows(listed, "invitations");
    if invitations.is_empty() {
        said.push_str("\ninvitations: none open");
        return said;
    }
    said.push_str("\ninvitations:");
    for invitation in invitations {
        // Named by what redeeming it makes them: an owner's or an administrator's invitation
        // joins someone as an Editor and makes them more.
        let what = Some(text(invitation, "place"))
            .filter(|place| !place.is_empty())
            .unwrap_or_else(|| text(invitation, "role"));
        // Who it is for, when the inviter named them, and its id, which cancelling it takes.
        let named = text(invitation, "invitee_name");
        let _ = write!(said, "\n  {what} invitation");
        if !named.is_empty() {
            let _ = write!(said, " for {named}");
        }
        let _ = write!(said, ", {}", text(invitation, "invite"));
    }
    said
}

/// A workspace's wait list (`WaitList`): each address with its entry, and
/// whether it was invited.
#[must_use]
pub fn wait_list(listed: &Value) -> String {
    let entries = rows(listed, "entries");
    if entries.is_empty() {
        return "the wait list is empty".to_owned();
    }
    let mut said = String::from("wait list:");
    for entry in entries {
        let invited = if entry.get("invited_at_ns").is_some_and(|at| !at.is_null()) {
            ", invited"
        } else {
            ""
        };
        let _ = write!(
            said,
            "\n  {}{invited} — {}",
            text(entry, "contact"),
            text(entry, "entry")
        );
    }
    said
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

    #[test]
    fn the_people_of_a_workspace_are_a_line_each() {
        // One place each, as the workspace names it: an owner is an administrator and an editor
        // beneath, and reads as an owner alone.
        let listed = json!({
            "members": [{"name": "Ada", "role": "editor", "owner": true, "administrator": true,
                         "place": "owner"},
                        {"name": "Rosa Ortiz", "role": "editor", "place": "editor"}],
            "invitations": [{"invite": "inv-1", "role": "viewer", "invitee_name": "Bo",
                             "place": "viewer"},
                            {"invite": "inv-2", "role": "editor", "administrator": true,
                             "owner": true, "place": "owner"}],
        });
        assert_eq!(
            members(&listed),
            "members:\n  Ada — owner\n  Rosa Ortiz — editor\n\
             invitations:\n  viewer invitation for Bo, inv-1\n  owner invitation, inv-2"
        );
        let alone = json!({"members": [], "invitations": []});
        assert_eq!(
            members(&alone),
            "members:\n  nobody yet\ninvitations: none open"
        );
    }
}
