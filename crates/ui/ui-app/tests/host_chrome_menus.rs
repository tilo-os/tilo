//! A native menu holds its groups in the model's order, with separators between them.
use tilo_ui_app::host_chrome_testing::{CommandGroup, Entry, MenuKind, entries};

#[test]
fn each_menu_holds_its_groups_in_model_order_with_separators_between() {
    let mut app = tilo_ui_app::app::testing::native_chrome_app();
    let commands = app.host_commands_for_test(true);
    let group = |entry: &Entry| match entry {
        Entry::Command(index) => Some(commands[*index].group),
        Entry::Separator => None,
    };
    let edit: Vec<_> = entries(&commands, MenuKind::Edit)
        .iter()
        .map(group)
        .collect();
    assert_eq!(
        edit,
        [
            Some(CommandGroup::Undo),
            Some(CommandGroup::Undo),
            None,
            Some(CommandGroup::Search),
        ]
    );
    let history: Vec<_> = entries(&commands, MenuKind::History)
        .iter()
        .map(group)
        .collect();
    assert_eq!(
        history,
        [
            Some(CommandGroup::History),
            None,
            Some(CommandGroup::RecentChanges),
            Some(CommandGroup::RecentChanges),
        ]
    );
    assert!(entries(&commands, MenuKind::View).is_empty());
    let titles: Vec<_> = MenuKind::ALL
        .iter()
        .map(|kind| tilo_ui_base::i18n::I18nManager::lookup_plain(kind.title_key()))
        .collect();
    assert_eq!(titles, ["Edit", "View", "History"]);
}
