//! What tests reach of the sections, as an agent or assistive technology
//! does.

use ui::framework::testing::TestApp;

pub const APPEARANCE: &str = "settings.appearance";
pub const ACCESSIBILITY: &str = "settings.accessibility";
pub const NOTIFICATIONS: &str = "settings.notifications";
pub const POWER: &str = "settings.power";
pub const INPUT: &str = "settings.input";
pub const INPUT_DEVICE: &str = "settings.input-device";
pub const TEXT_ASSISTANCE: &str = "settings.text-assistance";
pub const CONTINUITY: &str = "settings.continuity";

/// How many widgets in the tree under `w` are named `name`.
pub fn count_named(w: &dyn ui::Widget, name: &str) -> usize {
    usize::from(w.name() == name)
        + w.children()
            .into_iter()
            .map(|child| count_named(child, name))
            .sum::<usize>()
}

/// Every action a person can take under `w`, by what it says, in tree
/// order: its buttons, the rows that act when chosen, and the entries
/// of each row's menu (not the More button that opens it).
pub fn actions_of(w: &dyn ui::Widget, actions: &mut Vec<String>) {
    if let Some(menu) = w
        .as_any()
        .and_then(|any| any.downcast_ref::<ui::widget::RowMenu>())
    {
        actions.extend(menu.labels().into_iter().map(str::to_owned));
        return;
    }
    let acts =
        w.name() == "Button" || (w.name() == "ListRow" && (w.is_focusable() || !w.is_enabled()));
    if acts && let Some(label) = w.accessibility().and_then(|node| node.name) {
        actions.push(label);
    }
    for child in w.children() {
        actions_of(child, actions);
    }
}

/// The accessible names of the widgets named `name` under `w`, in tree
/// order, appended to `labels`.
pub fn labels_of(w: &dyn ui::Widget, name: &str, labels: &mut Vec<String>) {
    if w.name() == name
        && let Some(label) = w.accessibility().and_then(|node| node.name)
    {
        labels.push(label);
    }
    for child in w.children() {
        labels_of(child, name, labels);
    }
}

/// Set the field `field` of the section `kind` to `value` through the
/// door, as an agent does.
///
/// # Errors
/// The door's refusal: the section's check, an unknown field.
pub fn edit_field(
    app: &mut TestApp<crate::SettingsShell>,
    kind: &str,
    field: &str,
    value: impl serde::Serialize,
) -> Result<(), String> {
    edit_path(app, kind, &[field], value)
}

/// Invoke the verb `action` of the section `kind` with `arguments`
/// through the door, as an agent does.
///
/// # Errors
/// The door's refusal.
pub fn invoke_section(
    app: &mut TestApp<crate::SettingsShell>,
    kind: &str,
    action: &str,
    arguments: serde_json::Value,
) -> Result<(), String> {
    app.call_on(&format!("settings:{kind}"), action, arguments)
        .map(drop)
        .map_err(|error| format!("{error:?}"))
}

/// Set the field at `path` (field names, outermost first) of the section
/// `kind` to `value` through the door, as an agent does.
///
/// # Errors
/// The door's refusal: the section's check, an unknown field.
pub fn edit_path(
    app: &mut TestApp<crate::SettingsShell>,
    kind: &str,
    path: &[&str],
    value: impl serde::Serialize,
) -> Result<(), String> {
    let path: Vec<serde_json::Value> = path
        .iter()
        .map(|field| serde_json::json!({ "field": field }))
        .collect();
    app.call_on(
        &format!("settings:{kind}"),
        "field-edit",
        serde_json::json!({
            "edit": { "path": path, "op": "set", "value": value }
        }),
    )
    .map(drop)
    .map_err(|error| format!("{error:?}"))
}
