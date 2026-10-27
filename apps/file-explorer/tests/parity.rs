#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use chrono::{Local, TimeZone};
use file_explorer::model::{Entry, SortBy, SortOrder, format_path, sort_entries};
use std::path::PathBuf;

fn e(name: &str, is_dir: bool, size: u64, mime: &str, year: i32) -> Entry {
    Entry {
        name: name.into(),
        path: PathBuf::from(name),
        is_dir,
        size,
        modified: Some(Local.with_ymd_and_hms(year, 1, 1, 0, 0, 0).unwrap()),
        mime: mime.into(),
        sync_state: None,
        content_hash: None,
        record_id: None,
        availability: None,
        share: None,
        history: None,
        app: None,
        rich_metadata: None,
        observed: None,
    }
}

/// Every sort key and both orders, each on its own entry fixture.
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

/// A camera's or a scanner's numbered files list in the order they were
/// made, as a person reads the numbers: `IMG_9` before `IMG_10`.
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
    // Names that differ only in case or leading zeros still have one order.
    let mut v = vec![
        e("b", false, 0, "text/plain", 2020),
        e("B", false, 0, "text/plain", 2020),
        e("page007", false, 0, "text/plain", 2020),
        e("page7", false, 0, "text/plain", 2020),
    ];
    sort_entries(SortBy::Name, SortOrder::Ascending, &mut v);
    assert_eq!(names(&v), ["B", "b", "page007", "page7"]);
}

/// Sorting by size, date or type orders the files that tie on it by name,
/// not by the order the disk happened to list them in.
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
    // Clean text labels — the leading root is drawn as a themed vector icon
    // by the breadcrumb bar, not a font glyph.
    assert_eq!(labels, vec!["Home", "Documents", "Projects"]);
    // The last component path equals the cwd itself.
    assert_eq!(comps.last().unwrap().1, cwd);

    // Non-home path falls through to root-based components.
    let comps2 = breadcrumb_components(&PathBuf::from("/etc/hosts"), Some(&home), &[]);
    let labels2: Vec<&str> = comps2.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels2, vec!["/", "etc", "hosts"]);
}

/// A workspace is named, not spelled out.
///
/// The room's folder is mounted wherever the driver put it, which in a
/// demo is under `/var/tmp`. Spelling that out tells a reader they are
/// looking at scratch space on somebody's laptop, which is the opposite of
/// what a workspace is - and the app already knew better, because its own
/// sidebar lists the same directory by name under System.
fn a_workspace_root_is_named_rather_than_spelled_out() {
    let ws = PathBuf::from("/var/tmp/tilo/workspace");
    let roots = vec![ws.clone()];
    let home = PathBuf::from("/home/user");

    // The root itself: one segment, the workspace's name.
    let comps = breadcrumb_components(&ws, Some(&home), &roots);
    let labels: Vec<&str> = comps.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["workspace"]);
    assert_eq!(comps[0].1, ws, "the root segment navigates to the mount");

    // Below it, relative to the workspace - no `var`, no `tmp`, no `tilo`.
    let deep = ws.join("Designs").join("v2");
    let labels: Vec<String> = breadcrumb_components(&deep, Some(&home), &roots)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert_eq!(labels, vec!["workspace", "Designs", "v2"]);

    // And the same label the places sidebar puts on the same location.
    assert_eq!(file_explorer::model::workspace_label(&ws), "workspace");

    // A workspace outranks home when the mount happens to be inside it.
    let in_home = home.join("room");
    let roots_in_home = vec![in_home.clone()];
    let labels: Vec<String> =
        breadcrumb_components(&in_home.join("notes"), Some(&home), &roots_in_home)
            .into_iter()
            .map(|(l, _)| l)
            .collect();
    assert_eq!(labels, vec!["room", "notes"]);

    // Nothing else moves: a path in no workspace is the trail it always was.
    let labels: Vec<String> =
        breadcrumb_components(&PathBuf::from("/etc/hosts"), Some(&home), &roots)
            .into_iter()
            .map(|(l, _)| l)
            .collect();
    assert_eq!(labels, vec!["/", "etc", "hosts"]);
}

use file_explorer::FileExplorerWidget;
use ui::{AppKind, AppWidget, IconDescriptor};

/// App identity, the startup path, footer, refresh and view-mode defaults,
/// each on a freshly constructed widget.
#[test]
fn widget_identity_defaults_and_initial_path() {
    file_explorer_widget_implements_app_widget_and_defaults_correctly();
    file_explorer_widget_constructs_with_sensible_default_path();
    initial_path_prefers_workspace_then_legacy_then_user_home_then_cwd_then_root();
    footer_shows_item_count_and_formatted_path();
    refresh_rereads_same_directory();
    view_mode_toggle_switches_between_list_and_grid();
}

fn file_explorer_widget_implements_app_widget_and_defaults_correctly() {
    crate::common::pin_locale();
    let w = FileExplorerWidget::new();
    assert_eq!(w.app_kind(), AppKind::Files);
    assert_eq!(w.app_title(), "Files");
    match w.app_icon() {
        IconDescriptor::Glyph(g) => assert_eq!(g, "󰉋"),
        IconDescriptor::Named(_) => panic!("glyph expected"),
    }
    assert_eq!(w.permission_hint(), Some(AppKind::Files));
}

fn file_explorer_widget_constructs_with_sensible_default_path() {
    let w = FileExplorerWidget::new();
    let cwd = w.current_path();
    // Must match `model::initial_path` resolution.
    assert_eq!(cwd, file_explorer::model::initial_path());
}

// ---------------------------------------------------------------------------
// Behavioural parity tests
// ---------------------------------------------------------------------------

/// Back, click/double-click, error and loading states and Open Terminal,
/// each on a freshly constructed widget.
#[test]
fn navigation_selection_loading_and_terminal_commands() {
    back_button_moves_to_parent_and_noops_at_root();
    double_click_dir_navigates_into_it();
    single_click_selects_single_entry();
    error_state_shows_message_and_retry_button();
    loading_state_shown_during_navigation();
    open_terminal_asks_the_shell_for_terminal_in_this_directory();
}

fn back_button_moves_to_parent_and_noops_at_root() {
    let mut w = FileExplorerWidget::new();
    let start_parent = w.current_path().parent().map(std::path::Path::to_path_buf);
    w.back();
    assert_eq!(
        Some(w.current_path()),
        start_parent.or_else(|| Some(w.current_path()))
    );

    // At root, back is a no-op.
    let mut w2 = FileExplorerWidget::new();
    w2.navigate(PathBuf::from("/"));
    let before = w2.current_path();
    w2.back();
    assert_eq!(w2.current_path(), before, "back at / is a no-op");
}

fn refresh_rereads_same_directory() {
    let mut w = FileExplorerWidget::new();
    let cwd = w.current_path();
    w.refresh();
    assert_eq!(w.current_path(), cwd);
}

fn double_click_dir_navigates_into_it() {
    let mut w = FileExplorerWidget::new();
    // Find a directory in current entries, if any. If no dirs, skip (test env-dep).
    let dir_idx = w_first_dir_index(&w);
    if let Some(idx) = dir_idx {
        let path = w_entry_path(&w, idx);
        let _ = w.select(Some(idx)); // first click
        let triggered = w.select(Some(idx)); // second click within 500 ms
        assert_eq!(triggered, Some(path.clone()));
        // Caller would call navigate(path); simulate:
        w.navigate(path.clone());
        assert_eq!(w.current_path(), path);
    }
}

fn w_first_dir_index(w: &FileExplorerWidget) -> Option<usize> {
    w.entries_snapshot().iter().position(|e| e.is_dir)
}
fn w_entry_path(w: &FileExplorerWidget, i: usize) -> PathBuf {
    w.entries_snapshot()[i].path.clone()
}

fn single_click_selects_single_entry() {
    let mut w = FileExplorerWidget::new();
    if w.entries_snapshot().len() >= 2 {
        let triggered = w.select(Some(0));
        assert!(triggered.is_none());
        assert_eq!(w.selected_index(), Some(0));
        // Re-select another clears the double-click window for the first.
        let _ = w.select(Some(1));
        assert_eq!(w.selected_index(), Some(1));
    }
}

fn error_state_shows_message_and_retry_button() {
    let mut w = FileExplorerWidget::new();
    w.navigate(PathBuf::from("/this/path/does/not/exist/for/sure"));
    assert!(w.error_message().is_some(), "error path should set error");
}

fn footer_shows_item_count_and_formatted_path() {
    let w = FileExplorerWidget::new();
    let (count, path_str) = w.footer_text();
    assert_eq!(count, w.entries_snapshot().len());
    // format_path substitutes home prefix.
    assert_eq!(
        path_str,
        file_explorer::model::format_path(&w.current_path(), tilo_paths::home().as_deref())
    );
}

fn view_mode_toggle_switches_between_list_and_grid() {
    use file_explorer::model::ViewMode;
    let mut w = FileExplorerWidget::new();
    assert_eq!(w.view_mode(), ViewMode::List);
    w.set_view_mode(ViewMode::Grid);
    assert_eq!(w.view_mode(), ViewMode::Grid);
}

fn initial_path_prefers_workspace_then_legacy_then_user_home_then_cwd_then_root() {
    // Assert the exact startup path for the current environment, including
    // configured mounts and the sandbox's granted user home. An explicit
    // workspace override suppresses the legacy /workspace fallback.
    use file_explorer::model::{initial_path, user_home, workspace_record_authority_roots};
    let p = initial_path();
    // A configured mount, not `~/Shared`: received shares are a sidebar
    // place, and an empty one is no window to open on.
    let workspace = workspace_record_authority_roots().into_iter().next();
    let legacy = PathBuf::from("/workspace");
    let home = user_home();
    let cwd = std::env::current_dir().ok();

    if let Some(workspace) = workspace {
        assert_eq!(p, workspace, "workspace wins when present");
    } else if std::env::var_os("TILO_WORKSPACE_ROOTS").is_none() && legacy.exists() {
        assert_eq!(p, legacy, "legacy workspace wins without an override");
    } else if let Some(h) = home {
        assert_eq!(p, h, "home is the next fallback when workspace absent");
    } else if let Some(c) = cwd {
        assert_eq!(p, c, "cwd wins when workspace and home are absent");
    } else {
        assert_eq!(p, PathBuf::from("/"), "root is the final fallback");
    }
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
    // Dirs still first. Among dirs and among files the order is reversed.
    assert_eq!(desc_names, vec!["dir_z", "dir_a", "zeta.txt", "alpha.txt"]);
}

fn open_terminal_asks_the_shell_for_terminal_in_this_directory() {
    let mut w = FileExplorerWidget::new();
    let dir = std::path::PathBuf::from("/var/workspace/Projects");
    w.navigate(dir.clone());
    assert_eq!(
        w.terminal_intent(),
        runtime_portal::tools::ToolIntent::Terminal {
            directory: Some(dir.to_string_lossy().into_owned())
        }
    );
    // A path the request cannot carry still opens Terminal, just not there.
    w.navigate(std::path::PathBuf::from("/tmp/line\nbreak"));
    assert_eq!(
        w.terminal_intent(),
        runtime_portal::tools::ToolIntent::Terminal { directory: None }
    );
}

#[test]
fn list_and_grid_builders_compose_real_file_views() {
    list_build_yields_a_file_view();
    grid_mode_uses_responsive_file_view();
    draw_composition_exercises_the_five_pass_pipeline();
}

fn list_build_yields_a_file_view() {
    use file_explorer::command::new_sink;
    use file_explorer::model::{Entry, SortBy, SortOrder};
    use file_explorer::views::file_view::FileView;
    use file_explorer::views::list_view::build_list;
    use ui::SelectionModel;
    let entries = vec![Entry {
        name: "a.txt".into(),
        path: PathBuf::from("/a.txt"),
        is_dir: false,
        size: 0,
        modified: None,
        mime: String::new(),
        sync_state: None,
        content_hash: None,
        record_id: None,
        availability: None,
        share: None,
        history: None,
        app: None,
        rich_metadata: None,
        observed: None,
    }];
    let rename_rect = std::rc::Rc::new(std::cell::Cell::new(None));
    let drop_targets = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let view = FileView::new(&entries, &SelectionModel::new(), &new_sink())
        .with_rename(None, rename_rect)
        .with_cursor(vello::kurbo::Point::ZERO)
        .with_drop_targets(drop_targets)
        .with_thumbs(None);
    let w = build_list(view, SortBy::Name, SortOrder::Ascending);
    assert_eq!(ui::widget::Widget::name(&*w), "FileView");
}

fn grid_mode_uses_responsive_file_view() {
    // The grid is a responsive FileView: the column count reflows to the
    // available width rather than being hard-coded.
    use file_explorer::command::new_sink;
    use file_explorer::model::Entry;
    use file_explorer::views::file_view::FileView;
    use file_explorer::views::grid_view::build_grid;
    use std::path::PathBuf;
    use ui::SelectionModel;

    let entries = vec![Entry {
        name: "a.txt".into(),
        path: PathBuf::from("/a.txt"),
        is_dir: false,
        size: 0,
        modified: None,
        mime: String::new(),
        sync_state: None,
        content_hash: None,
        record_id: None,
        availability: None,
        share: None,
        history: None,
        app: None,
        rich_metadata: None,
        observed: None,
    }];
    let sel = SelectionModel::new();
    let sink = new_sink();
    let rename_rect = std::rc::Rc::new(std::cell::Cell::new(None));
    let drop_targets = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let view = FileView::new(&entries, &sel, &sink)
        .with_rename(None, rename_rect)
        .with_cursor(vello::kurbo::Point::ZERO)
        .with_drop_targets(drop_targets)
        .with_thumbs(None);
    let w = build_grid(view);
    assert_eq!(ui::widget::Widget::name(&*w), "FileView");
}

fn loading_state_shown_during_navigation() {
    // Navigation is synchronous, so every return path must clear the flag.
    let mut w = FileExplorerWidget::new();
    assert!(!w.is_loading(), "fresh widget not loading");
    w.navigate(PathBuf::from("/definitely/not/here"));
    assert!(!w.is_loading(), "after error navigate loading flag clears");
    assert!(w.error_message().is_some(), "and an error is surfaced");
    // Successful refresh on a real directory also clears the flag.
    w.navigate(file_explorer::model::initial_path());
    assert!(
        !w.is_loading(),
        "after successful navigate loading is clear"
    );
}

fn draw_composition_exercises_the_five_pass_pipeline() {
    // Drives the composed widget through the host's full frame
    // (sync → build_layout → compute → apply_layout → draw).
    // Assertion: the widget's `rect` is non-empty after the pipeline runs and
    // the child widget tree is non-empty.
    use ui::headless::Framed;
    use ui::theme::Theme;
    use ui::widget::Widget;

    let w = Framed::new(
        ui::State::new(),
        FileExplorerWidget::new(),
        (1024, 768),
        Theme::dark(),
    );

    // The composition registers many taffy nodes (toolbar, breadcrumbs,
    // sidebar, view, footer, plus the intermediate Rows/Columns). A plain
    // leaf-only widget would register exactly one.
    let nodes = Framed::harness(&w).layout_node_ids().len();
    assert!(
        nodes > 1,
        "draw composition must register multiple layout nodes; got {nodes}"
    );

    let rect = w.get_rect();
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "apply_layout must produce a non-zero rect for the composed widget; got {rect:?}"
    );

    // draw must not panic and must fill a non-empty scene.
    // Scene should have encoded *something* from the composition — at a
    // minimum the toolbar's buttons produce fill/stroke commands.
    assert!(
        !Framed::harness(&w).scene().encoding().is_empty(),
        "draw composition must emit draw commands from the subtree"
    );
}
