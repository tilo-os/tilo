//! Windows jump tasks and menu text: what each command relaunches with, how it is quoted and
//! decoded, and which commands a jump task can carry at all.
use tilo_ui_app::host_chrome_testing::{CommandGroup, decode, jump_tasks, menu_text, quote};
use tilo_ui_base::{platform::Platform, platform::PlatformConventions};

fn commands() -> Vec<tilo_ui_app::host_chrome_testing::HostCommand> {
    let mut app = tilo_ui_app::app::testing::native_chrome_app();
    app.set_platform_conventions(PlatformConventions::new(Platform::Windows));
    app.host_commands_for_test(true)
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
        .filter_map(tilo_ui_app::host_chrome_testing::HostCommand::invocation)
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
    let mut app = tilo_ui_app::app::testing::native_chrome_app();
    app.set_platform_conventions(PlatformConventions::new(Platform::Windows));
    let commands = app.host_commands_for_test(true);
    let titles: Vec<_> = jump_tasks(&commands)
        .into_iter()
        .map(|task| task.title)
        .collect();
    assert_eq!(titles[..2], ["Search…", "Show History"], "{titles:?}");
    assert_eq!(titles.len(), 4, "and two recent changes: {titles:?}");
    assert!(
        titles[2..].iter().all(|title| title.contains(" by ")),
        "a change names who made it: {titles:?}"
    );
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
        let decoded = decode(&tilo_ui_app::host_chrome_testing::encode(&invocation));
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
