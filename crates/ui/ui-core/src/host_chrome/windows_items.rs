//! What the Windows menu bar and Jump List show, kept apart from Win32 so the mapping tests on every
//! host. A Jump List task starts a new process of the app with `--host-command` and the task's
//! invocation; that process runs it through the same API as the palette once its window exists.

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionAddress, ControlInvocation};

use super::model::{CommandGroup, HostCommand};
use crate::history_api::HISTORY_RESOURCE;
use crate::widget::search_request::SEARCH_RESOURCE;
use tilo_ui_base::platform::PlatformConventions;

/// The launch option a Jump List task passes its invocation in.
pub const LAUNCH_OPTION: &str = "--host-command";

/// A menu item's text: the label with `&` kept literal (Win32 reads it as the mnemonic marker) and the
/// shortcut right-aligned after a tab.
pub fn menu_text(command: &HostCommand, conventions: PlatformConventions) -> String {
    let label = command.label.replace('&', "&&");
    match &command.shortcut {
        Some(shortcut) => format!("{label}\t{}", shortcut.label(conventions)),
        None => label,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpTask {
    pub title: String,
    /// The whole argument string, quoted for `CommandLineToArgvW`.
    pub arguments: String,
}

/// Search, History and the recent changes, in that order.
pub fn jump_tasks(commands: &[HostCommand]) -> Vec<JumpTask> {
    commands
        .iter()
        .filter(|command| match command.group {
            CommandGroup::Search | CommandGroup::RecentChanges => true,
            CommandGroup::History => command.binding.address().action == "open",
            CommandGroup::Undo | CommandGroup::AppActions | CommandGroup::AppViews => false,
        })
        .filter_map(|command| {
            Some(JumpTask {
                title: command.label.clone(),
                arguments: format!("{LAUNCH_OPTION} {}", quote(&encode(&command.invocation()?))),
            })
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Wire {
    resource: String,
    action: String,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    arguments: serde_json::Map<String, serde_json::Value>,
}

pub fn encode(invocation: &ControlInvocation) -> String {
    serde_json::to_string(&Wire {
        resource: invocation.address.resource.clone(),
        action: invocation.address.action.clone(),
        arguments: invocation.arguments.clone(),
    })
    .unwrap_or_default()
}

/// What a Jump List task may ask of a new process.
const TASK_ACTIONS: [(&str, &str); 3] = [
    (SEARCH_RESOURCE, "open"),
    (HISTORY_RESOURCE, "open"),
    (HISTORY_RESOURCE, "open_at"),
];

/// The invocation a `--host-command` value carries, if it is one a Jump List task could have made.
pub fn decode(value: &str) -> Option<ControlInvocation> {
    let wire: Wire = serde_json::from_str(value).ok()?;
    if !TASK_ACTIONS.contains(&(wire.resource.as_str(), wire.action.as_str())) {
        return None;
    }
    Some(ControlInvocation {
        address: ActionAddress::new(wire.resource, wire.action),
        arguments: wire.arguments,
    })
}

/// One argument quoted so `CommandLineToArgvW` gives it back unchanged: backslashes double before a
/// quote, quotes are escaped.
pub fn quote(value: &str) -> String {
    let mut quoted = String::from('"');
    let mut backslashes = 0;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        let run = if character == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        quoted.extend(std::iter::repeat_n('\\', run));
        quoted.push(character);
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}
