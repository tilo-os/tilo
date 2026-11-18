#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use file_explorer::model::{Entry, SortBy, SortOrder, format_path, sort_entries};
use std::path::PathBuf;

fn e(name: &str, is_dir: bool, size: u64, mime: &str, year: i16) -> Entry {
    Entry {
        name: name.into(),
        path: PathBuf::from(name),
        is_dir,
        size,
        modified: Some(
            jiff::civil::date(year, 1, 1)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap()
                .timestamp(),
        ),
        mime: mime.into(),
        sync_state: None,
        content_hash: None,
        document_id: None,
        availability: None,
        app: None,
        rich_metadata: None,
        observed: None,
    }
}

#[test]
fn sort_entries_orders_by_each_key_and_keeps_directories_first() {
    sort_entries_always_places_directories_first();
    sort_by_size_orders_by_file_size();
    sort_by_modified_orders_by_mtime();
    sort_by_type_orders_by_mime();
    sort_by_name_places_alpha_order_with_dirs_first();
    sort_order_toggle_reverses_non_dir_group_but_keeps_dirs_first();
    sort_by_name_reads_numbers_by_value();
    entries_alike_by_the_key_read_in_name_order();
}

fn names(v: &[Entry]) -> Vec<&str> {
    v.iter().map(|entry| entry.name.as_str()).collect()
}

fn sort_by_name_reads_numbers_by_value() {
    let mut v = vec![
        e("IMG_10.jpg", false, 0, "image/jpeg", 2020),
        e("img_9.jpg", false, 0, "image/jpeg", 2020),
        e("IMG_100.jpg", false, 0, "image/jpeg", 2020),
        e("IMG_09b.jpg", false, 0, "image/jpeg", 2020),
        e("Chapter 2", true, 0, "inode/directory", 2020),
        e("chapter 11", true, 0, "inode/directory", 2020),
        e("Éclair.txt", false, 0, "text/plain", 2020),
        e("éclair 2.txt", false, 0, "text/plain", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    assert_eq!(
        names(&v),
        [
            "Chapter 2",
            "chapter 11",
            "img_9.jpg",
            "IMG_09b.jpg",
            "IMG_10.jpg",
            "IMG_100.jpg",
            "éclair 2.txt",
            "Éclair.txt",
        ]
    );
    sort_entries(SortBy::Name, SortOrder::Descending, &mut v);
    assert_eq!(
        names(&v),
        [
            "chapter 11",
            "Chapter 2",
            "Éclair.txt",
            "éclair 2.txt",
            "IMG_100.jpg",
            "IMG_10.jpg",
            "IMG_09b.jpg",
            "img_9.jpg",
        ]
    );
    let mut v = vec![
        e("b", false, 0, "text/plain", 2020),
        e("B", false, 0, "text/plain", 2020),
        e("page007", false, 0, "text/plain", 2020),
        e("page7", false, 0, "text/plain", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    assert_eq!(names(&v), ["B", "b", "page007", "page7"]);
}

fn entries_alike_by_the_key_read_in_name_order() {
    let mut v = vec![
        e("report-10.txt", false, 1, "text/plain", 2020),
        e("notes.png", false, 1, "image/png", 2020),
        e("report-2.txt", false, 1, "text/plain", 2020),
        e("a.txt", false, 1, "text/plain", 2020),
    ];
    for sort_by in [SortBy::Size, SortBy::Modified] {
        sort_entries(sort_by, SortOrder::Ascending, &mut v);
        assert_eq!(
            names(&v),
            ["a.txt", "notes.png", "report-2.txt", "report-10.txt"],
            "{sort_by:?}"
        );
        sort_entries(sort_by, SortOrder::Descending, &mut v);
        assert_eq!(
            names(&v),
            ["a.txt", "notes.png", "report-2.txt", "report-10.txt"],
            "{sort_by:?} descending reverses the key, not the names within it"
        );
    }
    sort_entries(SortBy::Type, SortOrder::Ascending, &mut v);
    assert_eq!(
        names(&v),
        ["notes.png", "a.txt", "report-2.txt", "report-10.txt"]
    );
}

fn sort_entries_always_places_directories_first() {
    let mut v = vec![
        e("z_file.txt", false, 10, "text/plain", 2020),
        e("a_dir", true, 0, "inode/directory", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "a_dir");
    sort_entries(SortBy::Name, SortOrder::Descending, &mut v);
    assert_eq!(v[0].name, "a_dir", "descending still keeps dirs first");
}

fn sort_by_size_orders_by_file_size() {
    let mut v = vec![
        e("big.bin", false, 1000, "application/octet-stream", 2020),
        e("small.bin", false, 10, "application/octet-stream", 2020),
    ];
    sort_entries(SortBy::Size, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "small.bin");
}

fn sort_by_modified_orders_by_mtime() {
    let mut v = vec![
        e("new.txt", false, 0, "text/plain", 2025),
        e("old.txt", false, 0, "text/plain", 2000),
    ];
    sort_entries(SortBy::Modified, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "old.txt");
}

fn sort_by_type_orders_by_mime() {
    let mut v = vec![
        e("b.png", false, 0, "image/png", 2020),
        e("a.txt", false, 0, "text/plain", 2020),
    ];
    sort_entries(SortBy::Type, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "b.png", "image/png < text/plain alphabetically");
}

#[test]
fn paths_and_breadcrumbs_name_home_workspaces_and_root() {
    format_path_substitutes_home_prefix();
    breadcrumb_component_click_navigates_to_that_component();
    a_workspace_root_is_named_rather_than_spelled_out();
}

fn format_path_substitutes_home_prefix() {
    let home = PathBuf::from("/home/user");
    let p = PathBuf::from("/home/user/Documents");
    assert_eq!(format_path(&p, Some(&home)), "~/Documents");
    assert_eq!(format_path(&home, Some(&home)), "~");
    assert_eq!(format_path(&PathBuf::from("/etc"), Some(&home)), "/etc");
    assert_eq!(format_path(&PathBuf::from("/etc"), None), "/etc");
}

use file_explorer::breadcrumbs::breadcrumb_components;

fn breadcrumb_component_click_navigates_to_that_component() {
    crate::common::pin_locale();
    let home = PathBuf::from("/home/user");
    let cwd = PathBuf::from("/home/user/Documents/Projects");
    let comps = breadcrumb_components(&cwd, Some(&home), &[]);
    let labels: Vec<&str> = comps.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["Home", "Documents", "Projects"]);
    assert_eq!(comps.last().unwrap().1, cwd);

    let comps2 = breadcrumb_components(&PathBuf::from("/etc/hosts"), Some(&home), &[]);
    let labels2: Vec<&str> = comps2.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels2, vec!["/", "etc", "hosts"]);
}

fn a_workspace_root_is_named_rather_than_spelled_out() {
    let ws = PathBuf::from("/var/tmp/tilo/workspace");
    let roots = vec![ws.clone()];
    let home = PathBuf::from("/home/user");

    let comps = breadcrumb_components(&ws, Some(&home), &roots);
    let labels: Vec<&str> = comps.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["workspace"]);
    assert_eq!(comps[0].1, ws, "the root segment navigates to the mount");

    let deep = ws.join("Designs").join("v2");
    let labels: Vec<String> = breadcrumb_components(&deep, Some(&home), &roots)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert_eq!(labels, vec!["workspace", "Designs", "v2"]);

    assert_eq!(file_explorer::model::workspace_label(&ws), "workspace");

    let in_home = home.join("projects");
    let roots_in_home = vec![in_home.clone()];
    let labels: Vec<String> =
        breadcrumb_components(&in_home.join("notes"), Some(&home), &roots_in_home)
            .into_iter()
            .map(|(l, _)| l)
            .collect();
    assert_eq!(labels, vec!["projects", "notes"]);

    let labels: Vec<String> =
        breadcrumb_components(&PathBuf::from("/etc/hosts"), Some(&home), &roots)
            .into_iter()
            .map(|(l, _)| l)
            .collect();
    assert_eq!(labels, vec!["/", "etc", "hosts"]);
}

use file_explorer::FileExplorerWidget;
#[test]
fn the_footer_counts_items_and_names_the_folder_with_home_shortened() {
    footer_shows_item_count_and_formatted_path();
}

#[test]
fn navigation_selection_and_terminal_commands() {
    back_button_moves_to_parent_and_noops_at_root();
    double_click_dir_navigates_into_it();
    error_state_shows_message_and_retry_button();
    open_terminal_asks_the_shell_for_terminal_in_this_directory();
}

fn back_button_moves_to_parent_and_noops_at_root() {
    let w = FileExplorerWidget::new();
    let start_parent = w
        .state()
        .current_path()
        .parent()
        .map(std::path::Path::to_path_buf);
    w.state().back();
    let now = w.state().current_path();
    assert_eq!(Some(now.clone()), start_parent.or(Some(now)));

    let w2 = FileExplorerWidget::new();
    w2.state().navigate(PathBuf::from("/"));
    let before = w2.state().current_path();
    w2.state().back();
    assert_eq!(w2.state().current_path(), before, "back at / is a no-op");
}

fn double_click_dir_navigates_into_it() {
    let w = FileExplorerWidget::new();
    let dir_idx = w_first_dir_index(&w);
    if let Some(idx) = dir_idx {
        let path = w_entry_path(&w, idx);
        let _ = w.state().select(Some(idx)); // first click
        let triggered = w.state().select(Some(idx)); // second click within 500 ms
        assert_eq!(triggered, Some(path.clone()));
        w.state().navigate(path.clone());
        assert_eq!(w.state().current_path(), path);
    }
}

fn w_first_dir_index(w: &FileExplorerWidget) -> Option<usize> {
    w.state().entries_snapshot().iter().position(|e| e.is_dir)
}
fn w_entry_path(w: &FileExplorerWidget, i: usize) -> PathBuf {
    w.state().entries_snapshot()[i].path.clone()
}

fn error_state_shows_message_and_retry_button() {
    let w = FileExplorerWidget::new();
    w.state()
        .navigate(PathBuf::from("/this/path/does/not/exist/for/sure"));
    assert!(
        w.state().error_message().is_some(),
        "error path should set error"
    );
}

fn footer_shows_item_count_and_formatted_path() {
    let w = FileExplorerWidget::new();
    let (count, path_str) = w.state().footer_text();
    assert_eq!(count, w.state().entries_snapshot().len());
    assert_eq!(
        path_str,
        file_explorer::model::format_path(&w.state().current_path(), tilo_paths::home().as_deref())
    );
}

fn sort_by_name_places_alpha_order_with_dirs_first() {
    use file_explorer::model::{SortBy, SortOrder, sort_entries};
    let mut v = vec![
        e("zeta.txt", false, 0, "text/plain", 2020),
        e("alpha.txt", false, 0, "text/plain", 2020),
        e("beta_dir", true, 0, "inode/directory", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "beta_dir", "dirs come first regardless of name");
    assert_eq!(v[1].name, "alpha.txt");
    assert_eq!(v[2].name, "zeta.txt");
}

fn sort_order_toggle_reverses_non_dir_group_but_keeps_dirs_first() {
    use file_explorer::model::{SortBy, SortOrder, sort_entries};
    let mut v = vec![
        e("alpha.txt", false, 10, "text/plain", 2020),
        e("zeta.txt", false, 100, "text/plain", 2020),
        e("dir_a", true, 0, "inode/directory", 2020),
        e("dir_z", true, 0, "inode/directory", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    let asc_names: Vec<String> = v.iter().map(|e| e.name.clone()).collect();
    assert_eq!(asc_names, vec!["dir_a", "dir_z", "alpha.txt", "zeta.txt"]);

    sort_entries(SortBy::Name, SortOrder::Descending, &mut v);
    let desc_names: Vec<String> = v.iter().map(|e| e.name.clone()).collect();
    assert_eq!(desc_names, vec!["dir_z", "dir_a", "zeta.txt", "alpha.txt"]);
}

fn open_terminal_asks_the_shell_for_terminal_in_this_directory() {
    let w = FileExplorerWidget::new();
    let dir = std::path::PathBuf::from("/var/workspace/Projects");
    w.state().navigate(dir.clone());
    assert_eq!(
        w.state().terminal_intent(),
        runtime_portal::tools::ToolIntent::Terminal {
            directory: Some(dir.to_string_lossy().into_owned())
        }
    );
    w.state()
        .navigate(std::path::PathBuf::from("/tmp/line\nbreak"));
    assert_eq!(
        w.state().terminal_intent(),
        runtime_portal::tools::ToolIntent::Terminal { directory: None }
    );
}
