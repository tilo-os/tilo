//! What the Windows menu bar and Jump List show, kept apart from Win32 so
//! the mapping tests on every host.
//!
//! A Jump List task starts a new process of the app with `--host-command`
//! and the task's invocation; that process runs it through the same door as
//! the palette once its window exists.

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionAddress, ControlInvocation};

use super::model::{CommandGroup, HostCommand};
use crate::platform::PlatformConventions;
use crate::standalone_history_host::{HISTORY_RESOURCE, SEARCH_RESOURCE};

/// The launch option a Jump List task passes its invocation in.
pub(crate) const LAUNCH_OPTION: &str = "--host-command";

/// A menu item's text: the label with `&` kept literal (Win32 reads it as
/// the mnemonic marker) and the shortcut right-aligned after a tab.
pub(crate) fn menu_text(command: &HostCommand, conventions: PlatformConventions) -> String {
    let label = command.label.replace('&', "&&");
    match &command.shortcut {
        Some(shortcut) => format!("{label}\t{}", shortcut.label(conventions)),
        None => label,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JumpTask {
    pub title: String,
    /// The whole argument string, quoted for `CommandLineToArgvW`.
    pub arguments: String,
}

/// Search, History and the recent changes, in that order.
pub(crate) fn jump_tasks(commands: &[HostCommand]) -> Vec<JumpTask> {
    commands
        .iter()
        .filter(|command| match command.group {
            CommandGroup::Search | CommandGroup::RecentChanges => true,
            // A new process has no History open to hide or compare in.
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

fn encode(invocation: &ControlInvocation) -> String {
    serde_json::to_string(&Wire {
        resource: invocation.address.resource.clone(),
        action: invocation.address.action.clone(),
        arguments: invocation.arguments.clone(),
    })
    .unwrap_or_default()
}

/// What a Jump List task may ask of a new process. Anything that can start
/// the app can pass `--host-command`, so it reaches these and nothing else.
const TASK_ACTIONS: [(&str, &str); 3] = [
    (SEARCH_RESOURCE, "open"),
    (HISTORY_RESOURCE, "open"),
    (HISTORY_RESOURCE, "open_at"),
];

/// The invocation a `--host-command` value carries, if it is one a Jump
/// List task could have made.
pub(crate) fn decode(value: &str) -> Option<ControlInvocation> {
    let wire: Wire = serde_json::from_str(value).ok()?;
    if !TASK_ACTIONS.contains(&(wire.resource.as_str(), wire.action.as_str())) {
        return None;
    }
    Some(ControlInvocation {
        address: ActionAddress::new(wire.resource, wire.action),
        arguments: wire.arguments,
    })
}

/// One argument quoted so `CommandLineToArgvW` gives it back unchanged:
/// backslashes double before a quote, quotes are escaped.
fn quote(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use automerge::ROOT;
    use automerge::transaction::Transactable;

    use super::{decode, jump_tasks, menu_text, quote};
    use crate::host_chrome::model::CommandGroup;
    use crate::platform::{Platform, PlatformConventions};

    fn commands() -> Vec<crate::host_chrome::model::HostCommand> {
        let mut state = crate::State::new();
        for title in ["first", "second"] {
            state.doc.put(ROOT, "title", title).unwrap();
            state.commit_action(None, None);
        }
        let mut app = crate::app::App::new_with_controlled_inputs(
            state,
            Box::new(crate::widget::Text::new("Document")),
        );
        app.set_platform_conventions(PlatformConventions::new(Platform::Windows));
        app.enable_document_history_chrome();
        app.init_layout();
        app.host_commands(true)
    }

    #[test]
    fn menu_text_keeps_ampersands_and_shows_the_windows_shortcut() {
        let commands = commands();
        let windows = PlatformConventions::new(Platform::Windows);
        let text = |group| {
            commands
                .iter()
                .find(|command| command.group == group)
                .map(|command| menu_text(command, windows))
                .unwrap()
        };
        assert_eq!(text(CommandGroup::History), "Show History\tCtrl+Shift+H");
        assert_eq!(text(CommandGroup::Search), "Search…\tCtrl+K");
        let mut ampersand = commands[0].clone();
        ampersand.label = "Undo Cut & Paste".into();
        ampersand.shortcut = None;
        assert_eq!(menu_text(&ampersand, windows), "Undo Cut && Paste");
    }

    #[test]
    fn jump_tasks_relaunch_with_the_exact_invocation_their_command_makes() {
        let commands = commands();
        let tasks = jump_tasks(&commands);
        let titles: Vec<_> = tasks.iter().map(|task| task.title.as_str()).collect();
        assert_eq!(titles[..2], ["Search…", "Show History"]);
        assert_eq!(tasks.len(), 4, "Search, History and two recent changes");
        let expected: Vec<_> = commands
            .iter()
            .filter(|command| {
                matches!(
                    command.group,
                    CommandGroup::Search | CommandGroup::History | CommandGroup::RecentChanges
                ) && command.binding.address().action != "compare"
            })
            .filter_map(crate::host_chrome::model::HostCommand::invocation)
            .collect();
        for (task, invocation) in tasks.iter().zip(&expected) {
            let value = task.arguments.strip_prefix("--host-command ").unwrap();
            let unquoted = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap()
                .replace("\\\"", "\"");
            assert_eq!(decode(&unquoted).as_ref(), Some(invocation));
        }
    }

    #[test]
    fn framework_history_apps_get_search_history_and_recent_changes_as_jump_tasks() {
        let mut app = crate::app::framework_history_tests::native_chrome_app();
        app.set_platform_conventions(PlatformConventions::new(Platform::Windows));
        let commands = app.host_commands(true);
        let titles: Vec<_> = jump_tasks(&commands)
            .into_iter()
            .map(|task| task.title)
            .collect();
        assert_eq!(titles[..2], ["Search…", "Show History"], "{titles:?}");
        assert_eq!(titles.len(), 4, "and two recent changes: {titles:?}");
        assert!(titles[2..].iter().all(|title| title.contains(" · ")));
        let windows = PlatformConventions::new(Platform::Windows);
        let search = commands
            .iter()
            .find(|command| command.group == CommandGroup::Search)
            .expect("the Edit menu offers Search");
        assert_eq!(menu_text(search, windows), "Search…\tCtrl+K");
    }

    #[test]
    fn quoting_follows_command_line_to_argv() {
        assert_eq!(quote(r#"{"a":"b"}"#), r#""{\"a\":\"b\"}""#);
        assert_eq!(quote(r"C:\dir\"), r#""C:\dir\\""#);
        assert_eq!(quote(r#"\"x"#), r#""\\\"x""#);
        assert_eq!(decode("not json"), None);
    }

    #[test]
    fn a_launch_argument_reaches_only_what_a_jump_task_can() {
        for command in commands() {
            let Some(invocation) = command.invocation() else {
                continue;
            };
            let from_task = jump_tasks(std::slice::from_ref(&command)).len() == 1;
            let decoded = decode(&super::encode(&invocation));
            assert_eq!(
                decoded.as_ref(),
                from_task.then_some(&invocation),
                "{:?}",
                command.binding.address()
            );
        }
        assert_eq!(
            decode(r#"{"resource":"app:document","action":"delete"}"#),
            None
        );
    }
}
