#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard; scroll offsets are exact"
)]
use tilo_desktop::composition::{DesktopSurface, SurfaceChrome};
use tilo_desktop::resolver::{TileRequest, TileResolution, TileResolver};
use tilo_desktop_model::{
    AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId,
};
use ui::record::RecordStore;

struct Unavailable;
impl TileResolver for Unavailable {
    fn resolve(&mut self, request: TileRequest<'_>) -> TileResolution {
        TileResolution::Unavailable {
            title: request.tab.title.clone(),
            detail: "Service unavailable".into(),
        }
    }
}
fn tree() -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: 3,
            tabs: vec![Tab {
                id: 9_007_199_254_741_001,
                title: "My document".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("so.tilo.word-processor".into()),
                    label: "Renamed".into(),
                },
            }],
            active_tab: 9_007_199_254_741_001,
            width: WindowWidth::Normal,
            maximized: false,
        }],
    }];
    DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    }
}

fn navigation_tree() -> DesktopTree {
    let mut tree = tree();
    let mut second = tree.workspaces[0].strips[0].window_groups[0].clone();
    second.id = 4;
    second.tabs[0].id = 20;
    second.tabs[0].title = "Chat".into();
    second.active_tab = 20;
    tree.workspaces[0].strips[0].window_groups.push(second);
    tree.active_strip.insert(tree.active, StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

#[test]
fn navigation_first_layout_reveals_and_marks_canonical_focus() {
    use ui::widget::Widget;
    let mut tree = navigation_tree();
    tree.active_group.insert(StripId(2), 4);
    let mut surface = Box::new(DesktopSurface::build(
        &tree,
        &RecordStore::in_memory(),
        &mut Unavailable,
        SurfaceChrome::default(),
    ));
    let ptr = std::ptr::from_mut(surface.as_mut());
    let mut frame = ui::headless::FrameHarness::new(ui::state::State::new(), surface, (740, 720));
    frame.poll_frame().unwrap();
    // SAFETY: the harness owns the allocation; these accesses end before each frame call.
    unsafe {
        (&mut *ptr)
            .row_mut(StripId(2))
            .unwrap()
            .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
    }
    frame.relayout();
    let row = unsafe { (&*ptr).row(StripId(2)).unwrap() };
    assert!(
        row.groups[1].get_rect().x1 <= row.get_rect().x1 + 1.0,
        "focused Chat right edge is clipped: {:?} in {:?}",
        row.groups[1].get_rect(),
        row.get_rect()
    );
    assert!(row.groups[1].focused());
    assert!(!row.groups[0].focused());
}

#[test]
fn navigation_clicking_selected_tab_emits_group_focus_without_tab_mutation() {
    use ui::widget::Widget;
    let tree = navigation_tree();
    let mut surface = Box::new(DesktopSurface::build(
        &tree,
        &RecordStore::in_memory(),
        &mut Unavailable,
        SurfaceChrome::default(),
    ));
    let ptr = std::ptr::from_mut(surface.as_mut());
    let mut frame = ui::headless::FrameHarness::new(ui::state::State::new(), surface, (740, 720));
    frame.poll_frame().unwrap();
    let point = unsafe {
        (&*ptr).row(StripId(2)).unwrap().groups[1].children()[0]
            .test_rects()
            .into_iter()
            .find(|(id, _)| id == "tab:Chat")
            .unwrap()
            .1
            .center()
    };
    assert!(point.x < 740.0, "test clicks the real visible tab");
    frame.click_at(point.x, point.y);
    let intents = unsafe { (&mut *ptr).take_intents() };
    assert_eq!(
        intents,
        vec![tilo_desktop::intents::DesktopIntent::Local(
            tilo_desktop::intents::LocalViewIntent::Group {
                strip: StripId(2),
                group: 4
            }
        )]
    );
    assert!(unsafe { (&mut *ptr).take_intents() }.is_empty());
    assert_eq!(
        tree.active_group[&StripId(2)],
        3,
        "focus is only a local intent"
    );
}

struct NavigationFrame {
    frame: ui::headless::FrameHarness,
    surface: *mut DesktopSurface,
    tree: DesktopTree,
    records: RecordStore,
}
impl NavigationFrame {
    fn new(tree: DesktopTree, width: u32) -> Self {
        let records = RecordStore::in_memory();
        let mut surface = Box::new(DesktopSurface::build(
            &tree,
            &records,
            &mut Unavailable,
            SurfaceChrome::default(),
        ));
        let ptr = std::ptr::from_mut(surface.as_mut());
        let frame = ui::headless::FrameHarness::new(ui::state::State::new(), surface, (width, 720));
        Self {
            frame,
            surface: ptr,
            tree,
            records,
        }
    }
    fn surface(&self) -> &DesktopSurface {
        // SAFETY: frame owns the boxed allocation throughout this fixture;
        // fixture methods ensure accesses never overlap a harness mutation.
        unsafe { &*self.surface }
    }
    fn surface_mut(&mut self) -> &mut DesktopSurface {
        unsafe { &mut *self.surface }
    }
    fn reconcile(&mut self) {
        unsafe {
            (&mut *self.surface).reconcile(&self.tree, &self.records, &mut Unavailable);
        }
        self.frame.relayout();
    }
    fn finish_pan(&mut self) {
        for strip in self.surface().strip_ids() {
            self.surface_mut()
                .row_mut(strip)
                .unwrap()
                .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
        }
        self.frame.relayout();
    }
    fn assert_visible(&self, strip: StripId, id: u64) {
        use ui::widget::Widget;
        let row = self.surface().row(strip).unwrap();
        let rect = row.groups.iter().find(|g| g.id == id).unwrap().get_rect();
        assert!(
            rect.x0 >= row.get_rect().x0 - 1.0 && rect.x1 <= row.get_rect().x1 + 1.0,
            "group {id}: {rect:?} in {:?}",
            row.get_rect()
        );
    }
}

#[test]
fn navigation_animation_frames_settle_without_restarting_and_allow_manual_navigation() {
    let mut tree = navigation_tree();
    tree.active_group.insert(StripId(2), 4);
    let mut test = NavigationFrame::new(tree, 740);
    assert!(test.frame.frame_output().needs_animation_frame);
    let start = test.surface().row(StripId(2)).unwrap().scroll_x();
    std::thread::sleep(std::time::Duration::from_millis(35));
    test.frame.poll_frame().unwrap();
    let middle = test.surface().row(StripId(2)).unwrap().scroll_x();
    assert!(
        middle > start,
        "a new host frame advances the existing ease"
    );
    std::thread::sleep(std::time::Duration::from_millis(35));
    test.frame.poll_frame().unwrap();
    assert!(test.surface().row(StripId(2)).unwrap().scroll_x() > middle);
    test.finish_pan();
    test.assert_visible(StripId(2), 4);
    // An explicit viewport navigation away from the selected group must survive
    // unchanged admitted state and frames. This uses GroupRow's actual pan API.
    test.surface_mut()
        .row_mut(StripId(2))
        .unwrap()
        .scroll_group_into_view(3);
    test.frame.relayout();
    let parked = test.surface().row(StripId(2)).unwrap().scroll_x();
    test.reconcile();
    test.finish_pan();
    assert_eq!(test.surface().row(StripId(2)).unwrap().scroll_x(), parked);
}

#[test]
fn navigation_stepping_back_twice_pans_once_and_then_asks_for_no_frame() {
    fn frames_until_idle(test: &mut NavigationFrame) -> usize {
        (0..400)
            .find(|_| {
                if !test.frame.frame_output().needs_animation_frame {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
                test.frame.poll_frame().unwrap();
                false
            })
            .expect("the desktop goes quiet")
    }
    let mut tree = navigation_tree();
    let template = tree.workspaces[0].strips[0].window_groups[1].clone();
    for id in [5_u64, 6] {
        let mut group = template.clone();
        group.id = id;
        group.tabs[0].id = id + 100;
        group.active_tab = id + 100;
        tree.workspaces[0].strips[0].window_groups.push(group);
    }
    tree.active_group.insert(StripId(2), 6);
    let mut test = NavigationFrame::new(tree, 1200);
    frames_until_idle(&mut test);
    test.assert_visible(StripId(2), 6);
    let revealed = test.surface().row(StripId(2)).unwrap().scroll_x();

    test.tree.active_group.insert(StripId(2), 5);
    test.reconcile();
    test.tree.active_group.insert(StripId(2), 4);
    test.reconcile();
    // Widths are read off pan-shifted rects. When they wobble, each layout
    // asks for the same group again. Reproduce that ask on every frame.
    let frames = (0..400)
        .find(|_| {
            if !test.frame.frame_output().needs_animation_frame {
                return true;
            }
            test.surface_mut()
                .row_mut(StripId(2))
                .unwrap()
                .scroll_group_into_view_animated(4, ui::clock::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(16));
            test.frame.poll_frame().unwrap();
            false
        })
        .expect("the desktop goes quiet");
    // One 200 ms ease is about a dozen 16 ms frames. A pan that restarts
    // itself every frame toward the same place runs for seconds.
    assert!(frames <= 30, "{frames} frames to settle two steps back");
    test.assert_visible(StripId(2), 4);
    assert!(test.surface().row(StripId(2)).unwrap().scroll_x() < revealed);
}

#[test]
fn navigation_focus_retries_new_layout_and_tracks_resize_reorder_removal() {
    let mut test = NavigationFrame::new(navigation_tree(), 740);
    let mut group = test.tree.workspaces[0].strips[0].window_groups[1].clone();
    group.id = 5;
    group.tabs[0].id = 21;
    group.active_tab = 21;
    test.tree.workspaces[0].strips[0].window_groups.push(group);
    test.tree.active_group.insert(StripId(2), 5);
    test.reconcile();
    test.finish_pan();
    test.assert_visible(StripId(2), 5);
    test.frame.resize((1280, 720)).unwrap();
    test.finish_pan();
    test.assert_visible(StripId(2), 5);
    test.frame.resize((600, 720)).unwrap();
    test.frame.set_input_scale(2.0).unwrap();
    test.finish_pan();
    test.assert_visible(StripId(2), 5);
    assert_eq!(test.frame.frame_output().device_size, (1200, 1440));
    test.tree.workspaces[0].strips[0].window_groups.swap(0, 2);
    test.reconcile();
    test.finish_pan();
    test.assert_visible(StripId(2), 5);
    // Stale canonical pointer resolves to the surviving leftmost group.
    test.tree.workspaces[0].strips[0].window_groups.remove(0);
    test.reconcile();
    test.finish_pan();
    test.assert_visible(StripId(2), 4);
    let row = test.surface().row(StripId(2)).unwrap();
    assert!(row.groups[0].focused());
    assert!(!row.groups[1].focused());
}

#[test]
fn navigation_focus_fallback_cross_strip_closing_and_lone_group_match_native() {
    let mut tree = navigation_tree();
    let mut other = tree.workspaces[0].strips[0].clone();
    other.id = StripId(9);
    other.window_groups.truncate(1);
    other.window_groups[0].id = 6;
    other.window_groups[0].tabs[0].id = 22;
    other.window_groups[0].active_tab = 22;
    tree.workspaces[0].strips.push(other);
    tree.active_group.remove(&StripId(2));
    let mut test = NavigationFrame::new(tree, 740);
    assert!(test.surface().row(StripId(2)).unwrap().groups[0].focused());
    test.tree.active_strip.insert(test.tree.active, StripId(9));
    test.reconcile();
    assert!(test.surface().row(StripId(9)).unwrap().groups[0].focused());
    assert!(
        test.surface()
            .row(StripId(2))
            .unwrap()
            .groups
            .iter()
            .all(|g| !g.focused())
    );
    test.tree.active_strip.insert(test.tree.active, StripId(2));
    test.surface_mut().row_mut(StripId(2)).unwrap().groups[0].start_closing();
    unsafe {
        (&mut *test.surface).sync_navigation(&test.tree);
    }
    assert!(!test.surface().row(StripId(2)).unwrap().groups[0].focused());
    assert!(test.surface().row(StripId(2)).unwrap().groups[1].focused());
    test.tree.workspaces[0].strips[0].window_groups.remove(0);
    test.tree.workspaces[0].strips[1].window_groups.clear();
    test.reconcile();
    assert!(!test.surface().row(StripId(2)).unwrap().groups[0].focused());
}

#[test]
fn navigation_keyboard_focus_on_same_selected_tab_is_an_edge_not_a_sticky_caret() {
    fn tabbar(node: &ui::headless::InspectionNode) -> Option<String> {
        if node.semantics.as_ref().is_some_and(|s| {
            s.children
                .iter()
                .any(|child| child.name.as_deref() == Some("Chat"))
        }) {
            return Some(node.path.clone());
        }
        node.children.iter().find_map(tabbar)
    }
    let mut test = NavigationFrame::new(navigation_tree(), 740);
    let bar = tabbar(&test.frame.inspection().root).unwrap();
    test.frame
        .perform_semantic_named(&bar, None, &ui::a11y::Action::Focus)
        .unwrap();
    assert_eq!(
        test.surface_mut().take_group_focus_intent(),
        Some(tilo_desktop::intents::LocalViewIntent::Group {
            strip: StripId(2),
            group: 4
        })
    );
    test.tree.active_group.insert(StripId(2), 4);
    test.reconcile();
    test.finish_pan();
    test.assert_visible(StripId(2), 4);
    assert!(test.surface_mut().take_group_focus_intent().is_none());
    // Canonical local shortcut navigation can move away while the app retains
    // its caret; scanning that unchanged caret must not write focus back.
    test.tree.active_group.insert(StripId(2), 3);
    test.reconcile();
    test.finish_pan();
    test.assert_visible(StripId(2), 3);
    assert!(test.surface_mut().take_group_focus_intent().is_none());
}

#[test]
fn navigation_moved_group_and_workspace_rebuild_resolve_current_identity() {
    let mut tree = navigation_tree();
    let mut strip = tree.workspaces[0].strips[0].clone();
    strip.id = StripId(9);
    strip.window_groups.clear();
    tree.workspaces[0].strips.push(strip);
    tree.active_group.insert(StripId(2), 4);
    let mut test = NavigationFrame::new(tree, 740);
    test.finish_pan();
    let moved = test.tree.workspaces[0].strips[0].window_groups.remove(1);
    test.tree.workspaces[0].strips[1].window_groups.push(moved);
    test.tree.active_strip.insert(test.tree.active, StripId(9));
    test.tree.active_group.insert(StripId(9), 4);
    test.reconcile();
    test.finish_pan();
    assert!(test.surface().row(StripId(9)).unwrap().groups[0].focused());
    test.assert_visible(StripId(9), 4);
    assert!(!test.surface().row(StripId(2)).unwrap().groups[0].focused());
    let mut next = navigation_tree().workspaces.remove(0);
    next.id = WorkspaceId(50);
    next.strips[0].id = StripId(51);
    next.strips[0].window_groups[0].id = 52;
    next.strips[0].window_groups[1].id = 53;
    test.tree.active = next.id;
    test.tree.active_group.insert(StripId(51), 53);
    test.tree.workspaces.push(next);
    unsafe {
        (&mut *test.surface).rebuild_retaining(
            &test.tree,
            &test.records,
            &mut Unavailable,
            SurfaceChrome::default(),
        );
    }
    test.frame.relayout();
    test.finish_pan();
    assert!(test.surface().row(StripId(2)).is_none());
    assert!(test.surface().row(StripId(51)).unwrap().groups[1].focused());
    test.assert_visible(StripId(51), 53);
}

#[test]
fn navigation_native_click_policy_keeps_card_under_pointer_until_next_focus_edge() {
    let mut test = NavigationFrame::new(navigation_tree(), 740);
    test.finish_pan();
    let before = test.surface().row(StripId(2)).unwrap().scroll_x();
    test.surface_mut()
        .keep_clicked_group_in_place(StripId(2), 4);
    test.tree.active_group.insert(StripId(2), 4);
    test.reconcile();
    test.finish_pan();
    assert_eq!(test.surface().row(StripId(2)).unwrap().scroll_x(), before);
    assert!(test.surface().row(StripId(2)).unwrap().groups[1].focused());
    test.tree.active_group.insert(StripId(2), 3);
    test.reconcile();
    test.finish_pan();
    test.tree.active_group.insert(StripId(2), 4);
    test.reconcile();
    test.finish_pan();
    test.assert_visible(StripId(2), 4);
}

#[test]
fn navigation_oversized_group_reveals_start_edge_without_oscillating() {
    use ui::widget::Widget;
    let mut tree = navigation_tree();
    tree.workspaces[0].strips[0].window_groups[1].width = WindowWidth::Wide;
    tree.active_group.insert(StripId(2), 4);
    let mut test = NavigationFrame::new(tree, 400);
    test.surface_mut()
        .row_mut(StripId(2))
        .unwrap()
        .set_output_spans(&[ui::widget::OutputSpan {
            x: 0.0,
            width: 740.0,
            usable: 740.0,
        }]);
    test.frame.relayout();
    test.finish_pan();
    let row = test.surface().row(StripId(2)).unwrap();
    assert!(row.groups[1].get_rect().width() > row.get_rect().width());
    assert!(
        (row.groups[1].get_rect().x0 - row.get_rect().x0).abs() < 1.0,
        "an oversized group must start at the viewport edge: {:?}",
        row.groups[1].get_rect()
    );
    let parked = row.scroll_x();
    test.reconcile();
    test.finish_pan();
    assert_eq!(test.surface().row(StripId(2)).unwrap().scroll_x(), parked);
}

#[test]
fn navigation_remote_strip_rebuild_preserves_existing_viewport() {
    let mut test = NavigationFrame::new(navigation_tree(), 740);
    test.finish_pan();
    test.surface_mut()
        .row_mut(StripId(2))
        .unwrap()
        .scroll_group_into_view(4);
    test.frame.relayout();
    let parked = test.surface().row(StripId(2)).unwrap().scroll_x();
    assert!(parked > 0.0);
    test.tree.workspaces[0].strips.push(Strip {
        id: StripId(99),
        name: "Remote strip".into(),
        window_groups: vec![],
    });
    unsafe {
        (&mut *test.surface).rebuild_retaining(
            &test.tree,
            &test.records,
            &mut Unavailable,
            SurfaceChrome::default(),
        );
    }
    test.frame.relayout();
    test.finish_pan();
    assert_eq!(
        test.surface().row(StripId(2)).unwrap().scroll_x(),
        parked,
        "an unrelated admitted strip must not snap an existing peer-local viewport"
    );
}

#[test]
fn navigation_newly_mounted_real_writer_focus_is_not_an_admitted_focus_intent() {
    struct Writer;
    impl TileResolver for Writer {
        fn resolve(&mut self, _: TileRequest<'_>) -> TileResolution {
            TileResolution::Hosted(Box::new(word_processor::WordProcessorWidget::new()))
        }
    }
    let mut test = NavigationFrame::new(navigation_tree(), 740);
    assert!(test.surface_mut().take_group_focus_intent().is_none());
    let mut added = test.tree.workspaces[0].strips[0].window_groups[0].clone();
    added.id = 5;
    added.tabs[0].id = 23;
    added.active_tab = 23;
    test.tree.workspaces[0].strips[0]
        .window_groups
        .insert(0, added);
    let record = tilo_desktop::records::scratch_tab_record(23);
    test.records.create_record(&record).unwrap();
    test.records.with(&record.id, |doc| {
        word_processor::initialize_empty_document(doc).unwrap();
    });
    unsafe {
        (&mut *test.surface).reconcile(&test.tree, &test.records, &mut Writer);
    }
    test.frame.relayout();
    assert!(
        test.frame
            .inspection_json()
            .unwrap()
            .contains("word_processor")
    );
    assert!(
        test.surface_mut().take_group_focus_intent().is_none(),
        "a newly mounted Writer's default focus is not user input"
    );
    assert_eq!(test.tree.active_group[&StripId(2)], 3);
}
