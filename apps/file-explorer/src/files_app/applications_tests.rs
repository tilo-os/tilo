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
fn settle(files: &mut FileExplorerWidget, intent: &ToolIntent) {
    let deadline = tilo_test_env::hang_deadline();
    while !asked(intent) {
        assert!(
            std::time::Instant::now() < deadline,
            "the shell was never asked for {intent:?}"
        );
        files.poll_applications(ui::time::now());
        std::thread::yield_now();
    }
}

/// The Applications place, showing one app of each kind.
fn place() -> FileExplorerWidget {
    let mut files = FileExplorerWidget::new();
    files.debug_show_applications(sample_apps());
    files.navigate(crate::applications::location());
    files.set_app_request(record);
    files
}

fn select(files: &mut FileExplorerWidget, name: &str) {
    let index = files
        .displayed_entries()
        .iter()
        .position(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} is listed"));
    files.selection.select_only(index);
}

fn menu(files: &mut FileExplorerWidget) -> Vec<String> {
    files.open_context_menu(Point::new(5.0, 5.0));
    files
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
    let places = places_sidebar::Places::new(&files.favs, files.current_path.clone());
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
        select(&mut files, name);
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
    select(&mut files, "Text Editor");
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
    let mut files = place();
    select(&mut files, "Chess");
    assert!(files.apply_command(FileCommand::Uninstall));
    settle(
        &mut files,
        &ToolIntent::UninstallApplication {
            app: "org.gnome.Chess".into(),
        },
    );
    select(&mut files, "Notes");
    assert!(files.apply_command(FileCommand::Delete));
    settle(
        &mut files,
        &ToolIntent::UninstallApplication {
            app: "so.tilo.notes".into(),
        },
    );
    files.uninstall_apps(&[crate::applications::app_path("tilo-downloaded-krita")]);
    settle(
        &mut files,
        &ToolIntent::UninstallApplication {
            app: "tilo-downloaded-krita".into(),
        },
    );
}

#[test]
fn opening_an_app_asks_the_shell_to_open_it() {
    let mut files = place();
    select(&mut files, "Krita");
    let index = files.selection.primary().unwrap();
    assert!(files.apply_command(FileCommand::Activate(index)));
    settle(
        &mut files,
        &ToolIntent::OpenApplication {
            app: "tilo-downloaded-krita".into(),
        },
    );
    assert!(crate::applications::is_location(&files.current_path));
}

#[test]
fn an_app_is_not_a_file_to_copy_or_paste_into() {
    let mut files = place();
    select(&mut files, "Chess");
    assert!(files.apply_command(FileCommand::Copy));
    assert_eq!(
        files.status.as_deref(),
        Some(t("files-applications-not-a-file").as_str())
    );
    assert!(files.apply_command(FileCommand::NewFolder));
    assert_eq!(
        files.status.as_deref(),
        Some(t("files-applications-not-a-folder").as_str())
    );
}

#[test]
fn the_applications_folder_in_home_opens_the_place() {
    let home = tempfile::tempdir().unwrap();
    let mut files = place();
    files.home_path = Some(home.path().to_path_buf());
    files.navigate(home.path().to_path_buf());
    files.navigate(home.path().join("Applications"));
    assert!(crate::applications::is_location(&files.current_path));
}

#[test]
fn an_apps_row_offers_open_and_its_way_out_to_agents() {
    let files = place();
    let action_ids = |name: &str| {
        let entry = files
            .displayed_entries()
            .iter()
            .find(|entry| entry.name == name)
            .cloned()
            .unwrap();
        files
            .entry_resource(&entry)
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(action_ids("Chess"), ["open", "uninstall"]);
    assert_eq!(action_ids("Text Editor"), ["open", "remove-from-apps"]);
}

/// The list shows facts the applications model has: a size where it is
/// known and nothing where it is not, never a placeholder.
#[test]
fn an_apps_size_shows_only_where_it_is_known() {
    let files = place();
    let size_of = |name: &str| {
        let entry = files
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
    let mut files = FileExplorerWidget::new();
    files.debug_show_applications(sample_apps());
    files.navigate(crate::applications::location());
    files.set_app_request(record);
    files.set_app_lister(lister);
    assert!(
        files.applications.wanted,
        "the place is where the window shows"
    );
    files.applications.next_poll = None;
    files.applications.stale = false;
    files
}

#[test]
fn a_settled_app_list_asks_nothing_on_a_timer() {
    let files = settled(one_app);

    assert_eq!(
        files.applications.next_wake(),
        None,
        "nothing is due while the list is current"
    );
    assert_eq!(
        files.applications.next_poll, None,
        "the place schedules no read of its own"
    );
}

#[test]
fn the_shells_word_makes_the_place_ask_again() {
    let mut files = settled(one_app);
    files.take_apps_event(&ui::collaboration::CollabEvent::ApplicationsChanged);

    assert!(
        files.applications.next_wake().is_some(),
        "a change the shell reported is asked about at once"
    );
    assert_eq!(files.applications.next_poll, None, "on no clock of its own");
}

#[test]
fn only_the_app_lists_own_word_wakes_the_place() {
    let mut files = settled(one_app);
    files.take_apps_event(&ui::collaboration::CollabEvent::DrivesChanged);

    assert_eq!(
        files.applications.next_wake(),
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
