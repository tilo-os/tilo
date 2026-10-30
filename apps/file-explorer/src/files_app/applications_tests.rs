use super::*;

/// What the window asked the shell for, across the threads it asks on.
static ASKED: Mutex<Vec<ToolIntent>> = Mutex::new(Vec::new());

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the shell's tool service, which can refuse"
)]
fn record(intent: ToolIntent, _: Option<[u8; 16]>) -> Result<(), String> {
    ASKED.lock().unwrap().push(intent);
    Ok(())
}

fn asked(intent: &ToolIntent) -> bool {
    ASKED.lock().unwrap().contains(intent)
}

/// Wait for a request that runs off the UI thread to reach the shell.
fn settle(files: &FileExplorerWidget, intent: &ToolIntent) {
    tilo_test_env::wait_until(format!("the shell to be asked for {intent:?}"), || {
        files.state().poll_applications(ui::time::now());
        asked(intent)
    });
}

/// The Applications place, showing one app of each kind.
fn place() -> FileExplorerWidget {
    let files = FileExplorerWidget::new();
    files.state().debug_show_applications(sample_apps());
    files.state().navigate(crate::applications::location());
    files.state().set_app_request(record);
    files
}

fn select(files: &FileExplorerWidget, name: &str) {
    let index = files
        .state()
        .displayed_entries()
        .iter()
        .position(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} is listed"));
    files.state().selection.select_only(index);
}

fn menu(files: &mut FileExplorerWidget) -> Vec<String> {
    files.state().open_context_menu(Point::new(5.0, 5.0));
    files
        .chrome()
        .context_menu
        .items()
        .iter()
        .filter_map(|item| item.label().map(str::to_string))
        .collect()
}

#[test]
fn every_kind_of_app_is_an_item_of_the_applications_place() {
    let files = place();
    let names: Vec<String> = files
        .state()
        .displayed_entries()
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    // An added Tilo app, a Flatpak, an AppImage and a menu entry of the OS.
    for name in ["Notes", "Chess", "Krita", "Text Editor"] {
        assert!(
            names.contains(&name.to_owned()),
            "{name} is missing from {names:?}"
        );
    }
    let places = {
        let state = files.state();
        places_sidebar::Places::new(&state.favs, state.current_path.clone())
    };
    assert!(
        places
            .sections()
            .iter()
            .any(|section| section.items.iter().any(|item| {
                item.id == format!("loc:{}", crate::applications::location().display())
            })),
        "the place is in the places list"
    );
}

#[test]
fn an_apps_menu_has_uninstall_and_not_move_to_trash() {
    let mut files = place();
    for name in ["Notes", "Chess", "Krita"] {
        select(&files, name);
        let labels = menu(&mut files);
        assert!(
            labels.contains(&t("files-menu-uninstall")),
            "{name}: {labels:?}"
        );
        assert!(
            !labels.contains(&t("files-menu-move-trash")),
            "{name}: {labels:?}"
        );
        assert!(!labels.contains(&t("files-menu-rename")), "{labels:?}");
    }
}

#[test]
fn an_os_entry_offers_only_remove_from_apps() {
    let mut files = place();
    select(&files, "Text Editor");
    let labels = menu(&mut files);
    assert!(
        labels.contains(&t("files-menu-remove-from-apps")),
        "{labels:?}"
    );
    assert!(!labels.contains(&t("files-menu-uninstall")), "{labels:?}");
    assert!(!labels.contains(&t("files-menu-move-trash")), "{labels:?}");
}

#[test]
fn uninstall_delete_and_a_drop_on_the_trash_all_ask_the_shell() {
    let files = place();
    select(&files, "Chess");
    assert!(files.state().apply_command(FileCommand::Uninstall));
    settle(
        &files,
        &ToolIntent::UninstallApplication {
            app: "org.gnome.Chess".into(),
        },
    );
    select(&files, "Notes");
    assert!(files.state().apply_command(FileCommand::Delete));
    settle(
        &files,
        &ToolIntent::UninstallApplication {
            app: "so.tilo.notes".into(),
        },
    );
    files
        .state()
        .uninstall_apps(&[crate::applications::app_path("tilo-downloaded-krita")]);
    settle(
        &files,
        &ToolIntent::UninstallApplication {
            app: "tilo-downloaded-krita".into(),
        },
    );
}

#[test]
fn opening_an_app_asks_the_shell_to_open_it() {
    let files = place();
    select(&files, "Krita");
    let index = files.state().selection.primary().unwrap();
    assert!(files.state().apply_command(FileCommand::Activate(index)));
    settle(
        &files,
        &ToolIntent::OpenApplication {
            app: "tilo-downloaded-krita".into(),
        },
    );
    assert!(crate::applications::is_location(
        &files.state().current_path
    ));
}

#[test]
fn an_app_is_not_a_file_to_copy_or_paste_into() {
    let files = place();
    select(&files, "Chess");
    assert!(files.state().apply_command(FileCommand::Copy));
    assert_eq!(
        files.state().status.as_deref(),
        Some(t("files-applications-not-a-file").as_str())
    );
    assert!(files.state().apply_command(FileCommand::NewFolder));
    assert_eq!(
        files.state().status.as_deref(),
        Some(t("files-applications-not-a-folder").as_str())
    );
}

#[test]
fn the_applications_folder_in_home_opens_the_place() {
    let home = tempfile::tempdir().unwrap();
    let files = place();
    files.state().home_path = Some(home.path().to_path_buf());
    files.state().navigate(home.path().to_path_buf());
    files.state().navigate(home.path().join("Applications"));
    assert!(crate::applications::is_location(
        &files.state().current_path
    ));
}

#[test]
fn an_apps_row_offers_open_and_its_way_out_to_agents() {
    let files = place();
    let action_ids = |name: &str| {
        let entry = files
            .state()
            .displayed_entries()
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
            .unwrap();
        files
            .state()
            .entry_resource(&entry)
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(action_ids("Chess"), ["read", "open", "uninstall"]);
    assert_eq!(
        action_ids("Text Editor"),
        ["read", "open", "remove-from-apps"]
    );
}

/// The list shows facts the applications model has: a size where it is
/// known and nothing where it is not, never a placeholder.
#[test]
fn an_apps_size_shows_only_where_it_is_known() {
    let files = place();
    let size_of = |name: &str| {
        let entry = files
            .state()
            .displayed_entries()
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
            .unwrap();
        crate::model::human_size(&entry)
    };
    assert_eq!(size_of("Krita"), crate::i18n::format_bytes(312_000_000));
    assert_eq!(size_of("Text Editor"), "");
}

/// The Applications place as it stands after the frame loop has taken the
/// shell's answer in: shown, and no read under way.
fn settled(lister: AppLister) -> FileExplorerWidget {
    let files = FileExplorerWidget::new();
    files.state().debug_show_applications(sample_apps());
    files.state().navigate(crate::applications::location());
    files.state().set_app_request(record);
    files.state().set_app_lister(lister);
    assert!(
        files.state().applications.wanted,
        "the place is where the window shows"
    );
    files.state().applications.next_poll = None;
    files.state().applications.stale = false;
    files
}

#[test]
fn a_settled_app_list_asks_nothing_on_a_timer() {
    let files = settled(one_app);

    assert_eq!(
        files.state().applications.next_wake(),
        None,
        "nothing is due while the list is current"
    );
    assert_eq!(
        files.state().applications.next_poll,
        None,
        "the place schedules no read of its own"
    );
}

#[test]
fn the_shells_word_makes_the_place_ask_again() {
    let files = settled(one_app);
    files
        .state()
        .take_apps_event(&ui::collaboration::CollabEvent::ApplicationsChanged);

    assert!(
        files.state().applications.next_wake().is_some(),
        "a change the shell reported is asked about at once"
    );
    assert_eq!(
        files.state().applications.next_poll,
        None,
        "on no clock of its own"
    );
}

#[test]
fn only_the_app_lists_own_word_wakes_the_place() {
    let files = settled(one_app);
    files
        .state()
        .take_apps_event(&ui::collaboration::CollabEvent::DrivesChanged);

    assert_eq!(
        files.state().applications.next_wake(),
        None,
        "the place is kept current by the app list's word alone"
    );
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "it stands in for the shell's tool service, which can refuse"
)]
fn one_app() -> Result<Vec<AppEntry>, String> {
    Ok(sample_apps().into_iter().take(1).collect())
}
