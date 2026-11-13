//! What tests reach of the sections, as an agent or assistive technology does.

use ui::framework::testing::TestApp;

pub const APPEARANCE: &str = "settings.appearance";
pub const IDENTITY: &str = "settings.identity";
pub const ACCESSIBILITY: &str = "settings.accessibility";
pub const NOTIFICATIONS: &str = "settings.notifications";
pub const POWER: &str = "settings.power";
pub const INPUT: &str = "settings.input";
pub const INPUT_DEVICE: &str = "settings.input-device";
pub const TEXT_ASSISTANCE: &str = "settings.text-assistance";
pub const CONTINUITY: &str = "settings.continuity";

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

/// The row caption `key` names, as the pane shows it and a screen reader
/// reads it. A key no catalogue has answers with itself, and an id is never
/// a word to read aloud.
pub fn caption(key: &str) -> String {
    let caption = crate::i18n::t(key);
    assert_ne!(caption, key, "the catalogue has no word for {key}");
    caption
}

pub fn shown_values(app: &mut TestApp<crate::SettingsShell>, caption: &str) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.label() == Some(caption))
        .filter_map(|(_, node)| node.value().map(str::to_owned))
        .collect()
}

pub fn edit_field(
    app: &mut TestApp<crate::SettingsShell>,
    kind: &str,
    field: &str,
    value: impl serde::Serialize,
) -> Result<(), String> {
    edit_path(app, kind, &[field], value)
}

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

pub fn switch_named(app: &mut TestApp<crate::SettingsShell>, name: &str) -> ui::accesskit::Node {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.role() == ui::accesskit::Role::Switch && node.label() == Some(name))
                .then_some(node)
        })
        .unwrap_or_else(|| panic!("no switch named {name}"))
}
