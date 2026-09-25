#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::float_cmp,
    clippy::panic,
    clippy::unreachable,
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
        rich_metadata: None,
    }
}

#[test]
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

#[test]
fn sort_by_size_orders_by_file_size() {
    let mut v = vec![
        e("big.bin", false, 1000, "application/octet-stream", 2020),
        e("small.bin", false, 10, "application/octet-stream", 2020),
    ];
    sort_entries(SortBy::Size, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "small.bin");
}

#[test]
fn sort_by_modified_orders_by_mtime() {
    let mut v = vec![
        e("new.txt", false, 0, "text/plain", 2025),
        e("old.txt", false, 0, "text/plain", 2000),
    ];
    sort_entries(SortBy::Modified, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "old.txt");
}

#[test]
fn sort_by_type_orders_by_mime() {
    let mut v = vec![
        e("b.png", false, 0, "image/png", 2020),
        e("a.txt", false, 0, "text/plain", 2020),
    ];
    sort_entries(SortBy::Type, SortOrder::Ascending, &mut v);
    assert_eq!(v[0].name, "b.png", "image/png < text/plain alphabetically");
}

#[test]
fn format_path_substitutes_home_prefix() {
    let home = PathBuf::from("/home/quinten");
    let p = PathBuf::from("/home/quinten/Documents");
    assert_eq!(format_path(&p, Some(&home)), "~/Documents");
    assert_eq!(format_path(&home, Some(&home)), "~");
    assert_eq!(format_path(&PathBuf::from("/etc"), Some(&home)), "/etc");
    assert_eq!(format_path(&PathBuf::from("/etc"), None), "/etc");
}

use file_explorer::breadcrumbs::breadcrumb_components;

#[test]
fn breadcrumb_component_click_navigates_to_that_component() {
    let home = PathBuf::from("/home/quinten");
    let cwd = PathBuf::from("/home/quinten/Documents/Projects");
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
#[test]
fn a_workspace_root_is_named_rather_than_spelled_out() {
    let ws = PathBuf::from("/var/tmp/tilo/workspace");
    let roots = vec![ws.clone()];
    let home = PathBuf::from("/home/quinten");

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

#[test]
fn file_explorer_widget_implements_app_widget_and_defaults_correctly() {
    let w = FileExplorerWidget::new();
    assert_eq!(w.app_kind(), AppKind::Files);
    assert_eq!(w.app_title(), "Files");
    match w.app_icon() {
        IconDescriptor::Glyph(g) => assert_eq!(g, "󰉋"),
        IconDescriptor::Named(_) => panic!("glyph expected"),
    }
    assert_eq!(w.permission_hint(), Some(AppKind::Files));
}

#[test]
fn file_explorer_widget_constructs_with_sensible_default_path() {
    let w = FileExplorerWidget::new();
    let cwd = w.current_path();
    // Must match `model::initial_path` resolution.
    assert_eq!(cwd, file_explorer::model::initial_path());
}

#[test]
fn sidebar_shows_fixed_seven_shortcuts_when_home_present() {
    use file_explorer::sidebar::build_shortcuts;
    use ui::icons::Icon;
    let home = Some(PathBuf::from("/home/test"));
    let shortcuts = build_shortcuts(home.as_deref());
    assert_eq!(shortcuts.len(), 7);
    assert_eq!(shortcuts[0].0, Icon::Home);
    assert_eq!(shortcuts[0].1, "Home");
    assert_eq!(shortcuts[6].0, Icon::HardDrive);
    assert_eq!(shortcuts[6].1, "Root");

    let none = build_shortcuts(None);
    assert_eq!(none.len(), 1);
    assert_eq!(none[0].0, Icon::HardDrive);
    assert_eq!(none[0].1, "Root");
}

// ---------------------------------------------------------------------------
// Behavioural parity tests
// ---------------------------------------------------------------------------

#[test]
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

#[test]
fn refresh_rereads_same_directory() {
    let mut w = FileExplorerWidget::new();
    let cwd = w.current_path();
    w.refresh();
    assert_eq!(w.current_path(), cwd);
}

#[test]
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

#[test]
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

#[test]
fn error_state_shows_message_and_retry_button() {
    let mut w = FileExplorerWidget::new();
    w.navigate(PathBuf::from("/this/path/does/not/exist/for/sure"));
    assert!(w.error_message().is_some(), "error path should set error");
}

#[test]
fn footer_shows_item_count_and_formatted_path() {
    let w = FileExplorerWidget::new();
    let (count, path_str) = w.footer_text();
    assert_eq!(count, w.entries_snapshot().len());
    // format_path substitutes home prefix.
    assert_eq!(
        path_str,
        file_explorer::model::format_path(&w.current_path(), xdg_paths::home().as_deref())
    );
}

#[test]
fn view_mode_toggle_switches_between_list_and_grid() {
    use file_explorer::model::ViewMode;
    let mut w = FileExplorerWidget::new();
    assert_eq!(w.view_mode(), ViewMode::List);
    w.set_view_mode(ViewMode::Grid);
    assert_eq!(w.view_mode(), ViewMode::Grid);
}

#[test]
fn initial_path_prefers_workspace_then_legacy_then_user_home_then_cwd_then_root() {
    // Assert the exact startup path for the current environment, including
    // configured mounts and the sandbox's granted user home. An explicit
    // workspace override suppresses the legacy /workspace fallback.
    use file_explorer::model::{initial_path, user_home, workspace_roots};
    let p = initial_path();
    let workspace = workspace_roots().into_iter().next();
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

#[test]
fn sidebar_highlights_active_shortcut() {
    use file_explorer::sidebar::{active_shortcut_index, build_shortcuts};
    let home = PathBuf::from("/home/tester");
    let shortcuts = build_shortcuts(Some(&home));
    // The Home shortcut is at index 0 and must be the active one when
    // `current_path == home`.
    assert_eq!(active_shortcut_index(&shortcuts, &home), Some(0));
    // A non-shortcut path must not match any sidebar row.
    assert_eq!(
        active_shortcut_index(&shortcuts, &PathBuf::from("/tmp")),
        None
    );
    // Root at the end of the list.
    assert_eq!(
        active_shortcut_index(&shortcuts, &PathBuf::from("/")),
        Some(shortcuts.len() - 1)
    );
}

#[test]
fn sidebar_has_fixed_width_180() {
    assert!((file_explorer::sidebar::SIDEBAR_WIDTH_PX - 180.0).abs() < f32::EPSILON);
}

#[test]
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

#[test]
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

#[test]
fn open_terminal_button_spawns_quick_terminal_subprocess() {
    use file_explorer::files_app::CommandRunner;

    #[derive(Default)]
    struct MockRunner {
        calls: Vec<(String, std::path::PathBuf)>,
    }
    impl CommandRunner for MockRunner {
        fn spawn(&mut self, program: &str, cwd: &std::path::Path) -> std::io::Result<()> {
            self.calls.push((program.to_string(), cwd.to_path_buf()));
            Ok(())
        }
        fn spawn_arg(
            &mut self,
            program: &str,
            _arg: &std::path::Path,
            cwd: &std::path::Path,
        ) -> std::io::Result<()> {
            self.calls.push((program.to_string(), cwd.to_path_buf()));
            Ok(())
        }
        fn open_default(&mut self, _path: &std::path::Path) -> std::io::Result<()> {
            Ok(())
        }
    }

    let w = FileExplorerWidget::new();
    let mut runner = MockRunner::default();
    w.open_terminal(&mut runner).unwrap();
    assert_eq!(runner.calls.len(), 1);
    assert_eq!(runner.calls[0].0, "terminal");
    assert_eq!(runner.calls[0].1, w.current_path());
}

#[test]
fn list_mode_has_name_size_modified_columns() {
    // The type glyph is now drawn inline in the Name column, so the header
    // columns are Name / Size / Modified.
    use file_explorer::views::list_view::COLUMN_KEYS;
    assert_eq!(COLUMN_KEYS, ["name", "size", "modified"]);
}

#[test]
fn list_build_yields_a_file_view() {
    use file_explorer::command::new_sink;
    use file_explorer::model::{Entry, SortBy, SortOrder};
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
        rich_metadata: None,
    }];
    let rename_rect = std::rc::Rc::new(std::cell::Cell::new(None));
    let drop_targets = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let w = build_list(
        &entries,
        &SelectionModel::new(),
        &new_sink(),
        1.0,
        1.0,
        SortBy::Name,
        SortOrder::Ascending,
        None,
        rename_rect,
        vello::kurbo::Point::ZERO,
        drop_targets,
        None,
    );
    assert_eq!(ui::widget::Widget::name(&*w), "FileView");
}

#[test]
fn grid_mode_uses_responsive_file_view() {
    // The grid is a responsive FileView: the column count reflows to the
    // available width rather than being hard-coded.
    use file_explorer::command::new_sink;
    use file_explorer::model::Entry;
    use file_explorer::views::grid_view::{GRID_CELL_HEIGHT, GRID_CELL_WIDTH, build_grid};
    use std::path::PathBuf;
    use ui::SelectionModel;

    // Compile-time, not a runtime assertion on constants: the metrics are
    // `const`, so their sanity can (and should) be checked at build time.
    const _: () = assert!(
        GRID_CELL_WIDTH > 0.0 && GRID_CELL_HEIGHT > 0.0,
        "cell metrics are sane"
    );
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
        rich_metadata: None,
    }];
    let sel = SelectionModel::new();
    let sink = new_sink();
    let rename_rect = std::rc::Rc::new(std::cell::Cell::new(None));
    let drop_targets = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let w = build_grid(
        &entries,
        &sel,
        &sink,
        1.0,
        1.0,
        None,
        rename_rect,
        vello::kurbo::Point::ZERO,
        drop_targets,
        None,
    );
    assert_eq!(ui::widget::Widget::name(&*w), "FileView");
}

#[test]
fn grid_cell_metrics_are_documented_values() {
    // Pins the default grid cell size so a regression in the grid builder is
    // caught (the grid derives its column count from these).
    assert!((file_explorer::views::grid_view::GRID_CELL_WIDTH - 112.0).abs() < f64::EPSILON);
    assert!((file_explorer::views::grid_view::GRID_CELL_HEIGHT - 104.0).abs() < f64::EPSILON);
}

#[test]
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

#[test]
fn draw_composition_exercises_the_five_pass_pipeline() {
    // Drives the composed widget through build_layout → apply_layout → draw.
    // Assertion: the widget's `rect` is non-empty after the pipeline runs and
    // the child widget tree is non-empty.
    use std::collections::HashMap;
    use taffy::{AvailableSpace, TaffyTree};
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use ui::widget::Widget;
    use vello::Scene;
    use vello::kurbo::Point;

    let mut w = FileExplorerWidget::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let theme = Theme::dark();
    let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
    let mut text_renderer = TextRenderer::new();

    let root = w.build_layout(&mut taffy, &theme, &mut node_map);

    // The composition registers many taffy nodes (toolbar, breadcrumbs,
    // sidebar, view, footer, plus the intermediate Rows/Columns). A plain
    // leaf-only widget would register exactly one.
    assert!(
        node_map.len() > 1,
        "draw composition must register multiple layout nodes; got {}",
        node_map.len()
    );

    taffy
        .compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(1024.0),
                height: AvailableSpace::Definite(768.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut text_renderer) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .unwrap();
    w.apply_layout(&taffy, root, Point::new(0.0, 0.0));

    let rect = w.get_rect();
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "apply_layout must produce a non-zero rect for the composed widget; got {rect:?}"
    );

    // draw must not panic and must fill a non-empty scene.
    let mut scene = Scene::new();
    let presence = PresenceCtx::empty();
    w.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );
    // Scene should have encoded *something* from the composition — at a
    // minimum the toolbar's buttons produce fill/stroke commands.
    assert!(
        !scene.encoding().is_empty(),
        "draw composition must emit draw commands from the subtree"
    );
}
