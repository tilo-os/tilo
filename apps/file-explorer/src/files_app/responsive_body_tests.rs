//! Tests of the window's body at every width: the docked sidebar, the narrow
//! drawer over the files, and the panes each lays out.
#![allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
use super::*;
use taffy::prelude::*;
use ui::framework::HostDelivery as _;
use ui::theme::Theme;
use ui::widget::DrawChild;

include!("../native_proof_tests.rs");

struct H {
    w: FileExplorerWidget,
    taffy: TaffyTree<()>,
    node: NodeId,
    state: ui::State,
    tr: ui::text_render::TextRenderer,
}

impl H {
    fn new(dir: Option<&std::path::Path>, width: f32, height: f32) -> Self {
        let w = FileExplorerWidget::with_persistence(None, None);
        if let Some(d) = dir {
            w.state().navigate(d.to_path_buf());
            w.state().wait_for_archive();
        }
        Self::from_widget(w, width, height)
    }

    fn from_widget(w: FileExplorerWidget, width: f32, height: f32) -> Self {
        let mut h = H {
            w,
            taffy: TaffyTree::new(),
            node: NodeId::new(0),
            state: ui::State::new(),
            tr: ui::text_render::TextRenderer::new(),
        };
        h.lay_out(width, height);
        if h.w.take_structure_dirty() {
            h.lay_out(width, height);
        }
        h
    }

    fn lay_out(&mut self, width: f32, height: f32) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        self.w.sync(&self.state.doc, &self.state.root);
        let node = self.w.build_layout(&mut taffy, &theme, &mut nm);
        let frame = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(width, height),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
        // SAFETY: this fixture exclusively owns the exact widget/layout
        // tree and keeps both stable through admission and measurement.
        unsafe {
            bindings.rebuild(&self.w, &taffy, frame, &nm);
            ui::widget::compute_widget_layout(
                &mut taffy,
                frame,
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::Definite(height),
                },
                &nm,
                &bindings,
                &mut self.tr,
            )
            .unwrap();
        }
        self.w.apply_layout(&taffy, node, Point::ORIGIN);
        self.taffy = taffy;
        self.node = node;
    }

    fn dispatch(&mut self, ev: &winit::event::WindowEvent) {
        ui::testing::Dispatch::new()
            .doc(&mut self.state.doc)
            .path(&self.state.root)
            .layout(&self.taffy, self.node)
            .send(&mut self.w, ev);
    }

    fn cursor(&mut self, x: f64, y: f64) {
        self.dispatch(&ui::synthetic_input::cursor_moved(x, y));
    }

    fn right_press(&mut self) {
        self.dispatch(&ui::synthetic_input::mouse_input(
            winit::event::ElementState::Pressed,
            winit::event::MouseButton::Right,
        ));
    }

    fn left_release(&mut self) {
        self.dispatch(&ui::synthetic_input::mouse_release());
    }

    fn left_press(&mut self) {
        self.dispatch(&ui::synthetic_input::mouse_press());
    }

    fn open_menu_labels(&mut self) -> Vec<String> {
        let chrome = self.w.chrome();
        if !chrome.context_menu.is_open() {
            return Vec::new();
        }
        chrome
            .context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(str::to_string))
            .collect()
    }

    fn rect_of(&self, name: &str) -> Option<vello::kurbo::Rect> {
        fn find(w: &dyn Widget, name: &str) -> Option<vello::kurbo::Rect> {
            if w.name() == name {
                return Some(w.get_rect());
            }
            for c in w.children() {
                if let Some(r) = find(c, name) {
                    return Some(r);
                }
            }
            None
        }
        find(&self.w, name)
    }

    fn empty_state(&self) -> Option<(ui::a11y::AccessNode, vello::kurbo::Rect)> {
        fn find(w: &dyn Widget) -> Option<(ui::a11y::AccessNode, vello::kurbo::Rect)> {
            if w.name() == "EmptyState" {
                let action = w.synthetic_accessibility_rect(&[0]).unwrap_or_default();
                return w.accessibility().map(|node| (node, action));
            }
            w.children().into_iter().find_map(find)
        }
        find(&self.w)
    }

    fn draw(&mut self) {
        let theme = Theme::dark();
        let presence = ui::presence::PresenceCtx::empty();
        let mut scene = vello::Scene::new();
        self.w.draw_child(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut self.tr,
            &theme,
            &presence,
        );
    }

    fn press_named(&mut self, key: winit::keyboard::NamedKey) {
        let ev = ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(key),
            None,
            winit::event::ElementState::Pressed,
        );
        self.dispatch(&ui::synthetic_input::keyboard_input(ev));
    }

    fn type_char(&mut self, s: &str) {
        let ev = ui::synthetic_input::key_event(
            winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s)),
            Some(winit::keyboard::SmolStr::new(s)),
            winit::event::ElementState::Pressed,
        );
        self.dispatch(&ui::synthetic_input::keyboard_input(ev));
    }

    fn ctrl_char(&mut self, s: &str) -> bool {
        let ev = ui::synthetic_input::key_event(
            winit::keyboard::Key::Character(winit::keyboard::SmolStr::new(s)),
            Some(winit::keyboard::SmolStr::new(s)),
            winit::event::ElementState::Pressed,
        );
        ui::testing::Dispatch::new()
            .doc(&mut self.state.doc)
            .path(&self.state.root)
            .layout(&self.taffy, self.node)
            .modifiers(winit::keyboard::ModifiersState::CONTROL)
            .send(&mut self.w, &ui::synthetic_input::keyboard_input(ev))
            .handled
    }

    fn draw_overlay(&mut self) {
        let theme = Theme::dark();
        let presence = ui::presence::PresenceCtx::empty();
        let mut scene = vello::Scene::new();
        self.w.draw_overlay(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut self.tr,
            &theme,
            &presence,
        );
    }
}

#[test]
fn changes_announce_once_with_the_windows_undo() {
    use crate::file_ops::Change;
    let _serial = crate::files_app::serial();
    ui::attention::clear();
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
    h.draw_overlay();
    h.w.state().status = None;
    h.w.state().select(Some(1));
    h.draw_overlay();
    assert!(
        !h.w.state().status_notice.is_shown(),
        "selecting a file is not news"
    );

    assert!(
        !h.ctrl_char("z"),
        "Ctrl+Z falls through to the window's Undo"
    );

    ui::undo::UndoControl::set_undo_status(&mut h.w, &mut || ui::UndoStatus {
        can_undo: true,
        ..ui::UndoStatus::default()
    });
    let gone = dir.path().join("a.txt").display().to_string();
    h.w.state().settle_op(
        OpFollowUp::Trash { count: 1 },
        &ui::tilo_resource::Settled::Done(vec![Change::Trashed {
            from: gone,
            entry: Some("/trash/files/a.txt".into()),
        }]),
    );
    let told = h.w.state().notice_line().expect("a move to Trash is said");
    assert_eq!(
        told,
        t_args("files-moved-to-trash", &[arg("count", 1)]),
        "one plain line, no meta text"
    );
    h.lay_out(1000.0, 720.0);
    let waiting = ui::attention::items()
        .into_iter()
        .find(|item| item.owner().app == "so.tilo.files")
        .expect("the move waits in Needs you");
    assert_eq!(waiting.title(), told);
    assert_eq!(waiting.action().label, t("files-notice-undo"));
    h.w.state()
        .needs
        .pressed(ui::attention::Press::Act(waiting.id()));
    h.lay_out(1000.0, 720.0);
    assert_eq!(
        ui::undo::UndoControl::take_undo_request(&mut h.w),
        Some(ui::UndoRequest::Undo),
        "Undo asks the host, the same door as Ctrl+Z"
    );
    assert!(
        h.w.state().notice_line().is_none(),
        "acting on the notice takes it down"
    );

    h.w.state().settle_op(
        OpFollowUp::Rename,
        &ui::tilo_resource::Settled::Done(vec![Change::Moved {
            from: dir.path().join("b.txt").display().to_string(),
            to: dir.path().join("d.txt").display().to_string(),
        }]),
    );
    assert!(h.w.state().notice_line().is_none());
    assert!(h.w.state().status_line().is_none());

    h.w.state().settle_op(
        OpFollowUp::Rename,
        &ui::tilo_resource::Settled::Failed {
            reason: std::io::Error::from_raw_os_error(13).to_string(),
            partial: vec![],
        },
    );
    let told = h.w.state().notice_line().expect("a failed rename is said");
    assert!(told.contains(&t("files-error-permission")), "{told}");
    assert!(!told.contains("os error"), "{told}");
}

#[test]
fn a_move_to_trash_keeps_its_undo_where_the_person_can_take_it() {
    use crate::file_ops::Change;
    let _serial = crate::files_app::serial();
    ui::attention::clear();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let files = FileExplorerWidget::with_persistence(None, None);
    files.state().navigate(dir.path().to_path_buf());
    files.state().wait_for_archive();
    let mut harness =
        ui::headless::FrameHarness::for_app(ui::State::new(), files, (1280, 720), Theme::dark());
    harness.present_in_window_bar();
    let gone = dir.path().join("a.txt").display().to_string();
    let mut w = harness.root_mut::<FileExplorerWidget>();
    ui::undo::UndoControl::set_undo_status(&mut *w, &mut || ui::UndoStatus {
        can_undo: true,
        ..ui::UndoStatus::default()
    });
    w.state().settle_op(
        OpFollowUp::Trash { count: 1 },
        &ui::tilo_resource::Settled::Done(vec![Change::Trashed {
            from: gone,
            entry: Some("/trash/files/a.txt".into()),
        }]),
    );
    drop(w);
    harness.recompose().unwrap();
    let waiting = ui::attention::items()
        .into_iter()
        .find(|item| item.owner().app == "so.tilo.files")
        .expect("the move waits in Needs you");
    assert_eq!(waiting.action().label, t("files-notice-undo"));
    assert_eq!(waiting.owner().name, "Files");
    let harness = harness;
    drop(harness);
}

#[test]
fn keys_go_back_forward_up_and_type_to_a_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("alpha")).unwrap();
    for name in ["beta.txt", "bravo.txt", "charlie.txt"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
    let selected = |h: &H| {
        let state = h.w.state();
        state
            .selection
            .primary()
            .and_then(|i| state.displayed_entries().get(i).map(|e| e.name.clone()))
    };
    h.type_char("b");
    assert_eq!(selected(&h).as_deref(), Some("beta.txt"));
    h.type_char("r");
    assert_eq!(selected(&h).as_deref(), Some("bravo.txt"));

    h.w.state().navigate(dir.path().join("alpha"));
    h.lay_out(1000.0, 720.0);
    let alt = |h: &mut H, key| {
        let ev = ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(key),
            None,
            winit::event::ElementState::Pressed,
        );
        ui::testing::Dispatch::new()
            .doc(&mut h.state.doc)
            .path(&h.state.root)
            .layout(&h.taffy, h.node)
            .modifiers(winit::keyboard::ModifiersState::ALT)
            .send(&mut h.w, &ui::synthetic_input::keyboard_input(ev))
            .handled
    };
    assert!(alt(&mut h, winit::keyboard::NamedKey::ArrowLeft));
    assert_eq!(h.w.state().current_path(), dir.path());
    assert!(alt(&mut h, winit::keyboard::NamedKey::ArrowRight));
    assert_eq!(h.w.state().current_path(), dir.path().join("alpha"));
    assert!(alt(&mut h, winit::keyboard::NamedKey::ArrowUp));
    assert_eq!(h.w.state().current_path(), dir.path());
}

#[test]
fn a_drag_resting_on_a_folder_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let w = FileExplorerWidget::with_persistence(None, None);
    w.state().navigate(dir.path().to_path_buf());
    let now = ui::time::now();
    w.state().spring = Some((inner.clone(), now));
    assert!(!w.state().spring_open_if_due(now), "no drag, nothing opens");
    w.state().pin_drag = Some(dir.path().join("a.txt"));
    assert!(
        !w.state().spring_open_if_due(now),
        "not rested long enough yet"
    );
    assert!(w.state().spring_open_if_due(now + SPRING_OPEN));
    assert_eq!(w.state().current_path(), inner);
    assert!(
        w.state().pin_drag.is_some(),
        "the drag goes on in the opened folder"
    );
}

#[test]
fn rename_selects_the_name_but_not_its_extension() {
    assert_eq!(rename_stem_end("report.final.pdf", false), Some(12));
    assert_eq!(rename_stem_end("notes", false), None);
    assert_eq!(rename_stem_end(".bashrc", false), None);
    assert_eq!(rename_stem_end("photos.2024", true), None);
}

#[test]
fn places_list_docks_wide_and_takes_turns_with_the_files_when_narrow() {
    body_docks_wide_and_reflows_to_push_when_narrowed();
    wide_docks_the_places_list_beside_the_files();
    narrow_shows_the_files_not_the_places_list();
    narrow_places_are_a_screen_of_their_own();
    narrow_places_screen_takes_input_the_files_do_not();
    narrow_navigation_returns_to_the_files();
}

fn body_docks_wide_and_reflows_to_push_when_narrowed() {
    let mut h = H::new(None, 1200.0, 720.0);
    assert_eq!(
        h.w.debug_body_mode(),
        SidebarPlacement::Docked,
        "a wide window docks the places list beside the file view"
    );
    h.lay_out(480.0, 720.0);
    assert_eq!(
        h.w.debug_body_mode(),
        SidebarPlacement::Screen,
        "a narrow window shows one pane at a time"
    );
    h.lay_out(1200.0, 720.0);
    assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Docked);
}

#[test]
fn the_empty_state_is_centred_in_the_content_pane() {
    const WIDTHS: [f32; 19] = [
        1200.0, 620.0, 560.0, 530.0, 520.0, 505.0, 480.0, 420.0, 360.0, 300.0, 260.0, 300.0, 420.0,
        520.0, 700.0, 1200.0, 900.0, 500.0, 1200.0,
    ];
    for mode in [ViewMode::List, ViewMode::Grid] {
        for places_open in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
            h.w.state().set_view_mode(mode);
            if places_open {
                h.w.debug_show_places();
            }
            for width in WIDTHS {
                for _ in 0..2 {
                    h.lay_out(width, 720.0);
                    h.draw();
                    let window = h.w.get_rect();
                    let content = h.w.debug_content_rect();
                    let sidebar = h.w.debug_sidebar_rect();
                    let empty = h
                        .rect_of("EmptyState")
                        .expect("an empty folder shows its empty state");
                    assert!(
                        (empty.x0 - content.x0).abs() <= 1.0
                            && (empty.x1 - content.x1).abs() <= 1.0,
                        "{mode:?} open={places_open} at {width}: the empty state is the \
                             file pane, not the window: empty={empty:?} content={content:?} \
                             window={window:?} places={sidebar:?}"
                    );
                    assert!(
                        (empty.center().x - content.center().x).abs() <= 1.0,
                        "{mode:?} open={places_open} at {width}: the message centres in \
                             the file pane: empty={empty:?} content={content:?}"
                    );
                    assert!(
                        content.x1 <= window.x1 + 1.0,
                        "{mode:?} open={places_open} at {width}: the file pane stays \
                             inside the window: content={content:?} window={window:?}"
                    );
                    assert!(
                        sidebar.width() <= 0.0 || sidebar.x1 <= empty.x0 + 1.0,
                        "{mode:?} open={places_open} at {width}: the places list stands \
                             beside the file pane, never over its message: \
                             places={sidebar:?} empty={empty:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn toolbar_adapts_to_width_and_its_menus_open_through_every_door() {
    toolbar_reprioritizes_before_desktop_controls_can_overlap_on_phone_width();
    toolbar_more_menu_tracks_its_trigger_and_flips_inside_the_viewport();
    shared_toolbar_resource_action_opens_view_options();
    accesskit_view_options_activation_updates_the_exported_menu_tree();
    archive_extraction_stays_directly_reachable_on_phone_and_desktop();
}

fn toolbar_reprioritizes_before_desktop_controls_can_overlap_on_phone_width() {
    let mut h = H::new(None, 390.0, 760.0);
    assert!(
        h.w.state().toolbar.is_compact(),
        "the first narrow frame keeps the path and navigation clear"
    );
    h.w.state().forward.push(PathBuf::from("/later"));
    h.lay_out(390.0, 760.0);
    h.w.state().open_view_menu();
    assert!(
        h.open_menu_labels().iter().any(|label| label == "Forward"),
        "compact overflow retains history navigation rather than removing it"
    );

    h.lay_out(900.0, 760.0);
    h.lay_out(900.0, 760.0);
    assert!(
        !h.w.state().toolbar.is_compact(),
        "a 560 px content column still fits the direct controls; only a phone width moves them into the overflow"
    );
    h.lay_out(390.0, 760.0);
    h.lay_out(390.0, 760.0);
    assert!(h.w.state().toolbar.is_compact(), "back to phone width");

    h.lay_out(1200.0, 720.0);
    assert!(
        h.w.take_structure_dirty(),
        "crossing the breakpoint requests a structural toolbar rebuild"
    );
    h.lay_out(1200.0, 720.0);
    assert!(
        !h.w.state().toolbar.is_compact(),
        "wide windows restore direct view and sort controls"
    );
}

fn toolbar_more_menu_tracks_its_trigger_and_flips_inside_the_viewport() {
    let mut h = H::new(None, 900.0, 720.0);
    h.w.state().open_view_menu();
    let trigger =
        h.w.state()
            .toolbar
            .view_menu_rect()
            .expect("More trigger is laid out");
    let menu = h.w.chrome().context_menu.get_rect();
    assert!(
        (menu.x1 - trigger.x1).abs() <= 1.0 && menu.y0 >= trigger.y1,
        "toolbar menu must sit directly below and right-aligned to More: trigger={trigger:?} menu={menu:?}"
    );

    h.w.state().open_menu_right_aligned(
        vec![
            ("One".into(), None, None),
            ("Two".into(), None, None),
            ("Three".into(), None, None),
            ("Four".into(), None, None),
        ],
        Point::new(890.0, 710.0),
        680.0,
    );
    let flipped = h.w.chrome().context_menu.get_rect();
    let window = h.w.state().rect;
    assert!(
        flipped.x0 >= window.x0
            && flipped.x1 <= window.x1
            && flipped.y0 >= window.y0
            && flipped.y1 <= 681.0,
        "a constrained menu must flip above its trigger and remain on-screen: {flipped:?}"
    );
}

#[test]
fn keyboard_and_pointer_events_drive_rename_menus_and_retry() {
    keyboard_context_menu_reaches_selection_actions_and_executes_them();
    a_click_runs_a_menu_row_and_send_to_opens_the_box_beside_the_menu();
    rename_is_edited_in_place_over_the_row();
    typing_in_the_in_place_editor_renames_on_enter();
    narrow_error_retry_uses_the_shared_button_and_recovers();
}

fn narrow_error_retry_uses_the_shared_button_and_recovers() {
    let mut h = H::new(None, 390.0, 780.0);
    h.w.state().debug_show_error_state();
    h.lay_out(390.0, 780.0);
    h.draw();

    let (_, retry) = h
        .empty_state()
        .expect("the failure is the pane's empty state");
    assert!(
        retry.width() >= 120.0 && retry.y0 > 0.0,
        "retry is a usable target: {retry:?}"
    );
    h.cursor(retry.center().x, retry.center().y);
    h.left_press();
    h.left_release();
    let intent = ui::widget::controls::take_control_invocation(&mut h.w)
        .expect("the shared button emits a resource action");
    assert_eq!(intent.address.resource, "files.retry");
    assert_eq!(intent.address.action, "invoke");
    assert!(
        crate::files_app::published::invoke(
            &mut h.w,
            &mut h.state.doc,
            &intent.address.resource,
            &intent.address.action,
            &serde_json::Value::Object(intent.arguments),
        )
        .unwrap()
        .is_some(),
        "the retry row refreshes the folder"
    );
    assert!(
        h.w.state().error().is_none(),
        "the shared button dispatches the normal refresh command"
    );
}

fn shared_toolbar_resource_action_opens_view_options() {
    let mut h = H::new(None, 1200.0, 720.0);
    assert!(
        h.w.start_encoded(
            tilo_actions::OperationKind::Effect,
            "tilo.ui.control.invoke",
            br#""files.view_menu""#,
            &mut h.state.doc,
            &h.state.root
        )
        .unwrap()
        .is_some(),
        "the same resource action an AT-SPI client discovers is handled"
    );
    h.lay_out(1200.0, 720.0);
    assert!(
        h.w.chrome().context_menu.is_open(),
        "resource/AT-SPI activation opens the real View options menu"
    );
    assert!(
        h.w.chrome()
            .menu_commands
            .iter()
            .any(|command| { matches!(command, Some(FileCommand::NewFolder)) }),
        "the real menu owns the New folder follow-up; this is not a hidden state toggle"
    );
}

fn accesskit_view_options_activation_updates_the_exported_menu_tree() {
    let new_folder_item = t("files-menu-new-folder");
    let exports_new_folder_item = |update: &ui::accesskit::TreeUpdate| {
        update.nodes.iter().any(|(_, node)| {
            node.role() == ui::accesskit::Role::MenuItem
                && node.label() == Some(new_folder_item.as_str())
                && !node.is_hidden()
        })
    };
    let mut app = ui::App::new(
        ui::State::new(),
        Box::new(FileExplorerWidget::with_persistence(None, None)),
    );
    app.init_layout();
    let before = app.build_a11y_tree_update(1.0);
    assert!(
        !exports_new_folder_item(&before),
        "the closed View options menu exports no New Folder item"
    );
    let view_options = before
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::Button && node.label() == Some("View options")
        })
        .map(|(id, _)| *id)
        .expect("the retained View options button is exported");

    assert_eq!(
        app.dispatch_a11y_action(view_options, &ui::a11y::Action::Activate),
        None
    );
    let after = app.build_a11y_tree_update(1.0);
    assert!(
        exports_new_folder_item(&after),
        "AT-SPI activation must publish the actual View options menu, not only acknowledge Click"
    );
}

fn archive_extraction_stays_directly_reachable_on_phone_and_desktop() {
    fn names(widget: &dyn Widget, name: &str) -> bool {
        widget
            .accessibility()
            .is_some_and(|node| node.name.as_deref() == Some(name))
            || widget
                .children()
                .into_iter()
                .any(|child| names(child, name))
    }
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("note.txt");
    std::fs::write(&source, b"hello").unwrap();
    let archive = temp.path().join("notes.zip");
    crate::archive::create_zip(&[source], &archive).unwrap();

    let mut h = H::new(Some(&archive), 390.0, 760.0);
    let extract = t("files-archive-extract");
    let has_extract = |w: &FileExplorerWidget| names(&w.chrome.nav, extract.as_str());
    assert!(has_extract(&h.w), "phone toolbar exposes Extract");
    assert!(
        h.w.state().toolbar.view_menu_rect().is_none(),
        "archive toolbar must not expose a one-item duplicate menu"
    );
    assert_eq!(h.w.state().footer_label(), t("files-archive-read-only"));
    assert!(
        h.w.state().status_notice.get().cloned() == Some(t("files-archive-read-only"))
            || ui::attention::entries()
                .iter()
                .any(|entry| entry.what() == t("files-archive-read-only")),
        "entering an archive says it is read-only, and the record keeps it"
    );

    h.lay_out(1200.0, 760.0);
    if h.w.take_structure_dirty() {
        h.lay_out(1200.0, 760.0);
    }
    assert!(has_extract(&h.w), "desktop toolbar exposes Extract");
    assert!(h.w.state().toolbar.view_menu_rect().is_none());
}

/// The regression: at ~600 px the window showed the breadcrumb and the
/// FAVOURITES / SYSTEM list and **no files at all**, while the footer
/// underneath read "27 items". The places list had been declared a content
/// overview, so the narrow layout promoted it to the full-screen page and
/// pushed the file view off the window.
fn narrow_shows_the_files_not_the_places_list() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
    }
    let h = H::new(Some(dir.path()), 480.0, 720.0);
    assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Screen);

    assert!(
        !h.w.debug_places_open(),
        "a narrow window opens on the folder, so the files are what you see",
    );
    let files = h
        .rect_of("FileView")
        .expect("the file view must be in the tree at a narrow width");
    assert!(
        files.width() > 0.0 && files.height() > 0.0,
        "the file view must be laid out at a real size; got {files:?}",
    );
    assert!(
        files.x0 >= 0.0 && files.x1 <= 480.0,
        "and inside the window rather than pushed off it: {files:?}",
    );
    assert!(
        (h.w.debug_content_rect().width() - 480.0).abs() <= f64::EPSILON,
        "the content pane spans the whole narrow window",
    );
}

fn keyboard_context_menu_reaches_selection_actions_and_executes_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("report.txt"), b"draft").unwrap();
    let mut h = H::new(Some(dir.path()), 900.0, 720.0);

    h.press_named(winit::keyboard::NamedKey::ArrowDown);
    h.press_named(winit::keyboard::NamedKey::ContextMenu);
    assert!(h.w.chrome().context_menu.is_open());
    let labels = h.open_menu_labels();
    assert!(labels.iter().any(|label| label == "Open"));
    assert!(labels.iter().any(|label| label == "Rename"));

    let rows: Vec<&String> = labels.iter().collect();
    let steps = rows
        .iter()
        .position(|label| label.as_str() == "Rename")
        .expect("Rename is offered")
        + 1;
    for _ in 0..steps {
        h.press_named(winit::keyboard::NamedKey::ArrowDown);
    }
    h.press_named(winit::keyboard::NamedKey::Enter);
    assert!(!h.w.chrome().context_menu.is_open());
    assert!(
        h.w.state().renaming.is_some(),
        "keyboard menu executes Rename"
    );
}

fn a_click_runs_a_menu_row_and_send_to_opens_the_box_beside_the_menu() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("budget.txt"), b"40 EUR").unwrap();
    let mut h = H::new(Some(dir.path()), 900.0, 720.0);
    ui::shared_selection::set_send_host_available(true);
    let open_menu = |h: &mut H| {
        let key = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ContextMenu),
            None,
            winit::event::ElementState::Pressed,
        ));
        ui::widget::context_menu::note_input(&key);
        h.dispatch(&key);
        assert!(h.w.chrome().context_menu.is_open());
    };
    let click_row = |h: &mut H, index: usize| {
        let row =
            h.w.chrome()
                .context_menu
                .item_rect(index)
                .expect("the row is drawn")
                .center();
        h.cursor(row.x, row.y);
        h.left_press();
        h.left_release();
    };
    h.press_named(winit::keyboard::NamedKey::ArrowDown);
    open_menu(&mut h);
    let send = h.w.chrome().context_menu.items().len() - 1;
    assert_eq!(
        h.open_menu_labels().last().map(String::as_str),
        Some("Send to…"),
        "the file's menu ends in Send to…"
    );
    click_row(&mut h, send);
    ui::shared_selection::set_send_host_available(false);
    let menu = &h.w.chrome().context_menu;
    assert!(menu.is_open(), "the menu stays beside its box");
    assert!(menu.send_to().is_open(), "the box opened");
    assert!(
        menu.send_to().rect().x0 >= menu.get_rect().x1
            || menu.send_to().rect().x1 <= menu.get_rect().x0,
        "beside the menu: {:?} by {:?}",
        menu.send_to().rect(),
        menu.get_rect()
    );
    h.press_named(winit::keyboard::NamedKey::Escape);
    h.press_named(winit::keyboard::NamedKey::Escape);
    assert!(!h.w.chrome().context_menu.is_open());

    open_menu(&mut h);
    let rename =
        h.w.chrome()
            .context_menu
            .items()
            .iter()
            .position(|item| item.label() == Some("Rename"))
            .expect("Rename is offered");
    click_row(&mut h, rename);
    assert!(!h.w.chrome().context_menu.is_open());
    assert!(h.w.state().renaming.is_some(), "a click runs Rename");
}

fn narrow_places_are_a_screen_of_their_own() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut h = H::new(Some(dir.path()), 480.0, 720.0);
    assert!(
        !h.w.debug_places_open(),
        "a narrow window opens on the folder, so the files are what you see"
    );
    assert!(h.rect_of("FileView").is_some());
    assert!(
        !h.w.chrome().nav.back(),
        "the files are the place the window opened on, so Back leads nowhere"
    );
    let toggle = h.w.chrome().nav.geometry().toggle;
    assert!(
        toggle.width() > 0.0 && toggle.height() > 0.0,
        "the bar holds the sidebar's button at this width: {toggle:?}"
    );
    h.w.chrome().nav.toggle_sidebar();
    h.w.chrome().nav.snap();
    h.lay_out(480.0, 720.0);
    assert!(h.w.debug_places_open(), "the places screen is in front");
    assert_eq!(h.w.debug_sidebar_rect().width(), 480.0);
    assert!(
        h.rect_of("FileView").is_none(),
        "one column at a time: the files are not under the places"
    );
    h.w.chrome().nav.show(ui::widget::NavPane::Content);
    h.w.chrome().nav.snap();
    h.lay_out(480.0, 720.0);
    assert!(!h.w.debug_places_open());
    assert!(h.rect_of("FileView").is_some());
}

fn narrow_places_screen_takes_input_the_files_do_not() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("behind-the-places.txt"), b"x").unwrap();
    let mut h = H::new(Some(dir.path()), 480.0, 720.0);
    h.w.debug_show_places();
    h.lay_out(480.0, 720.0);
    let places = h.w.debug_sidebar_rect();
    h.cursor(places.center().x, places.y0 + 300.0);
    h.right_press();
    assert!(
        !h.w.chrome().context_menu.is_open() && h.w.state().selected_indices().is_empty(),
        "a press on the places screen cannot select or open a menu for a file"
    );
    let semantic_children = h.w.chrome().nav.children();
    assert!(
        semantic_children
            .iter()
            .any(|child| child.name() == "NavList"),
        "the places screen exposes the places list"
    );
}

fn narrow_navigation_returns_to_the_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let mut h = H::new(Some(dir.path()), 480.0, 720.0);
    assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Screen);
    h.w.debug_show_places();
    h.lay_out(480.0, 720.0);
    assert!(h.w.debug_places_open());

    crate::command::push(
        &h.w.state().cmds,
        FileCommand::Navigate(dir.path().join("sub")),
    );
    h.cursor(10.0, 10.0);
    assert_eq!(
        h.w.state().current_path(),
        dir.path().join("sub"),
        "the navigation applied"
    );
    assert!(
        !h.w.debug_places_open(),
        "navigating leaves the places for the files",
    );
    h.w.chrome().nav.snap();
    h.lay_out(480.0, 720.0);
    assert!(
        (h.w.debug_content_rect().width() - 480.0).abs() <= f64::EPSILON,
        "and the file view owns the whole window",
    );
}

fn rename_is_edited_in_place_over_the_row() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.txt"), b"x").unwrap();
    let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
    let idx =
        h.w.state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "old.txt")
            .unwrap();
    h.w.state().select(Some(idx));
    assert!(h.w.state().apply_command(FileCommand::Rename));
    assert!(h.w.state().is_renaming());

    h.lay_out(1000.0, 720.0);
    h.draw();

    let field =
        h.w.state()
            .rename_rect
            .get()
            .expect("the in-place editor is placed over the row");
    let files = h.rect_of("FileView").expect("the file view is present");
    assert!(
        field.y0 >= files.y0 - 1.0 && field.y1 <= files.y1 + 1.0,
        "the rename field sits within the file rows (in place), not a bottom bar: \
             field={field:?} files={files:?}",
    );
    assert!(
        field.width() > 40.0,
        "the field spans the name column: {field:?}"
    );

    h.w.state().cancel_rename();
    h.lay_out(1000.0, 720.0);
    h.draw();
    assert!(
        h.w.state().rename_rect.get().is_none(),
        "cancel removes the in-place editor"
    );
}

fn typing_in_the_in_place_editor_renames_on_enter() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut h = H::new(Some(dir.path()), 1000.0, 720.0);
    let idx =
        h.w.state()
            .displayed_entries()
            .iter()
            .position(|e| e.name == "a.txt")
            .unwrap();
    h.w.state().select(Some(idx));

    h.press_named(winit::keyboard::NamedKey::F2);
    assert!(h.w.state().is_renaming(), "F2 starts an inline rename");
    h.lay_out(1000.0, 720.0);
    h.draw();

    h.type_char("b");
    h.press_named(winit::keyboard::NamedKey::Enter);

    assert!(
        !h.w.state().is_renaming(),
        "Enter commits and closes the editor"
    );
    assert!(
        dir.path().join("b.txt").exists(),
        "the typed name renamed the file on disk"
    );
    assert!(!dir.path().join("a.txt").exists(), "the old name is gone");
}

#[test]
fn row_drags_and_sidebar_favourites_respond_to_real_pointer_events() {
    dropping_a_folder_on_the_sidebar_pins_it();
    releasing_a_pin_drag_off_the_sidebar_pins_nothing();
    a_row_press_still_becomes_a_drag_after_the_view_is_rebuilt();
}

fn dropping_a_folder_on_the_sidebar_pins_it() {
    let cfg = tempfile::tempdir().unwrap();
    let w =
        FileExplorerWidget::with_persistence(Some(cfg.path()), Some(PathBuf::from("/home/tester")));
    let mut h = H::from_widget(w, 1200.0, 720.0);
    assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Docked);
    let before = h.w.state().favorites().len();
    let folder = PathBuf::from("/home/tester/Projects/tilo");

    assert!(h.w.state().apply_command(FileCommand::BeginDrag {
        path: folder.clone(),
        is_dir: true
    }));
    assert!(h.w.state().debug_pin_dragging(), "the drag is in flight");

    let sb = h.w.debug_sidebar_rect();
    h.cursor(sb.center().x, sb.center().y);
    h.left_release();
    assert!(!h.w.state().debug_pin_dragging(), "the drop ends the drag");
    assert_eq!(
        h.w.state().favorites().len(),
        before + 1,
        "the folder was pinned"
    );
    assert!(
        h.w.state().favorites().iter().any(|f| f.path == folder),
        "and it is the dropped folder"
    );
}

fn releasing_a_pin_drag_off_the_sidebar_pins_nothing() {
    let cfg = tempfile::tempdir().unwrap();
    let w =
        FileExplorerWidget::with_persistence(Some(cfg.path()), Some(PathBuf::from("/home/tester")));
    let mut h = H::from_widget(w, 1200.0, 720.0);
    let before = h.w.state().favorites().len();
    h.w.state().apply_command(FileCommand::BeginDrag {
        path: PathBuf::from("/home/tester/Downloads/tmp"),
        is_dir: true,
    });
    let content = h.w.debug_content_rect();
    h.cursor(content.center().x, content.center().y);
    h.left_release();
    assert!(!h.w.state().debug_pin_dragging());
    assert_eq!(
        h.w.state().favorites().len(),
        before,
        "a drop off the sidebar pins nothing"
    );
}

fn a_row_press_still_becomes_a_drag_after_the_view_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("photo.png"), b"png").unwrap();
    let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
    let view = h.rect_of("FileView").expect("the file view is laid out");
    let row = (view.x0 + 60.0, view.y0 + 30.0 + 17.0);
    h.cursor(row.0, row.1);
    h.left_press();
    assert!(
        !h.w.state().debug_pin_dragging(),
        "a press alone is not a drag"
    );
    h.lay_out(1200.0, 720.0);
    h.cursor(row.0 + 40.0, row.1 + 20.0);
    assert!(
        h.w.state().debug_pin_dragging(),
        "the armed press must start the drag after the rebuild"
    );
    assert_eq!(
        crate::external_drag::unclaimed().map(|d| d.paths),
        Some(vec![dir.path().join("photo.png")]),
        "the host is told what is being carried"
    );
    h.left_release();
    assert!(!h.w.state().debug_pin_dragging());
    assert_eq!(crate::external_drag::unclaimed(), None);
}

#[test]
fn right_clicking_a_favourite_opens_its_menu() {
    fn row(w: &dyn Widget, label: &str) -> Option<vello::kurbo::Point> {
        fn node(n: &ui::a11y::AccessNode, label: &str) -> Option<vello::kurbo::Point> {
            if n.name.as_deref() == Some(label) && n.role == ui::a11y::Role::TreeItem {
                return n.bounds.map(|bounds| {
                    vello::kurbo::Point::new(
                        f64::midpoint(bounds.x0, bounds.x1),
                        f64::midpoint(bounds.y0, bounds.y1),
                    )
                });
            }
            n.children.iter().find_map(|child| node(child, label))
        }
        w.accessibility()
            .as_ref()
            .and_then(|n| node(n, label))
            .or_else(|| w.children().iter().find_map(|child| row(*child, label)))
    }
    let cfg = tempfile::tempdir().unwrap();
    let w =
        FileExplorerWidget::with_persistence(Some(cfg.path()), Some(PathBuf::from("/home/tester")));
    let mut h = H::from_widget(w, 1200.0, 720.0);
    assert_eq!(
        h.w.debug_body_mode(),
        SidebarPlacement::Docked,
        "wide docks the sidebar"
    );

    let favorites = h.w.state().favorites_store().borrow().favorites.clone();
    let removable = row(&h.w, &favorites[1].label).expect("the removable favorite is laid out");
    let home_row = row(&h.w, &favorites[0].label).expect("the Home favorite is laid out");
    let (x, y) = (removable.x, removable.y);
    h.cursor(x, y);
    h.right_press();
    let labels = h.open_menu_labels();
    assert!(
        labels
            .iter()
            .any(|l| l == &crate::i18n::t("files-menu-remove-favourite")),
        "the favourite's own menu is shown: {labels:?}",
    );
    assert!(
        labels.iter().any(|l| l == "Open"),
        "with the Open folder action: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "New Folder"),
        "not the file menu: {labels:?}"
    );

    let (hx, hy) = (home_row.x, home_row.y);
    h.cursor(hx, hy);
    h.right_press();
    let home = h.open_menu_labels();
    assert!(
        !home.iter().any(|l| l == "Remove from Favorites"),
        "Home is pinned and cannot be removed: {home:?}",
    );
}

fn wide_docks_the_places_list_beside_the_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let mut h = H::new(Some(dir.path()), 1200.0, 720.0);
    assert_eq!(h.w.debug_body_mode(), SidebarPlacement::Docked);
    assert!(h.w.debug_places_open(), "wide docks the places list open");

    let content = h.w.debug_content_rect();
    assert!(
        (content.x1 - 1200.0).abs() <= f64::EPSILON,
        "the file view runs to the window's right edge",
    );
    assert!(
        content.x0 > 0.0,
        "and starts after the docked places list: {content:?}",
    );

    crate::command::push(
        &h.w.state().cmds,
        FileCommand::Navigate(dir.path().join("sub")),
    );
    h.cursor(10.0, 10.0);
    assert!(
        h.w.debug_places_open(),
        "wide navigation leaves the docked list in place — nothing to dismiss",
    );
}

#[test]
fn the_toolbar_starts_after_the_shells_toggle_when_the_places_list_is_away() {
    let mut h = H::new(None, 1200.0, 720.0);
    h.w.chrome().nav.close_sidebar();
    for _ in 0..2 {
        h.w.chrome().nav.snap();
        h.w.take_structure_dirty();
        h.lay_out(1200.0, 720.0);
    }
    let toggle = h.w.chrome().nav.geometry().toggle;
    assert!(toggle.width() > 0.0, "the shell shows its toggle");
    let up =
        h.w.state()
            .toolbar
            .up_button_rect()
            .expect("the toolbar is laid out");
    assert!(
        up.x0 > toggle.x1,
        "the toolbar's controls clear the toggle: toggle={toggle:?} up={up:?}"
    );

    h.w.chrome().nav.open_sidebar();
    h.w.chrome().nav.snap();
    h.lay_out(1200.0, 720.0);
    h.w.take_structure_dirty();
    h.lay_out(1200.0, 720.0);
    let docked = h.w.chrome().nav.geometry();
    assert!(docked.toggle.x1 <= docked.content_column.x0 + 1.0);
}

#[test]
fn menu_rows_show_the_sort_and_hidden_state_as_checks() {
    let w = FileExplorerWidget::with_persistence(None, None);
    assert_eq!(
        w.state()
            .command_checked(Some(&FileCommand::SetSort(SortBy::Name))),
        Some(true)
    );
    assert_eq!(
        w.state()
            .command_checked(Some(&FileCommand::SetSort(SortBy::Size))),
        Some(false)
    );
    assert_eq!(
        w.state().command_checked(Some(&FileCommand::ToggleHidden)),
        Some(false)
    );
    w.state().show_hidden = true;
    assert_eq!(
        w.state().command_checked(Some(&FileCommand::ToggleHidden)),
        Some(true)
    );
    assert_eq!(
        w.state().command_checked(Some(&FileCommand::NewFolder)),
        None
    );
    assert_eq!(w.state().command_checked(None), None);
}

#[test]
fn the_item_count_is_withheld_while_a_folder_is_loading_or_failed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let w = FileExplorerWidget::with_persistence(None, None);
    w.state().navigate(dir.path().to_path_buf());
    assert!(!w.state().footer_label().is_empty());
    w.state().debug_show_error_state();
    assert!(
        w.state().footer_label().is_empty(),
        "no count over an error"
    );
    w.state().debug_show_loading_state();
    assert!(
        w.state().footer_label().is_empty(),
        "no count while opening"
    );
}
