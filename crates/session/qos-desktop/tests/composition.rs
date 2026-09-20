use qos_desktop::composition::{DesktopSurface, SurfaceChrome};
use qos_desktop::resolver::{TileRequest, TileResolution, TileResolver};
use qos_desktop::{
    AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    WorkspaceTree,
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
fn tree() -> WorkspaceTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: 3,
            tabs: vec![Tab {
                id: 9007199254741001,
                title: "My document".into(),
                tile: TileContent::AppPlaceholder {
                    kind: AppKind::Other("dev.qos.word-processor".into()),
                    label: "Renamed".into(),
                },
            }],
            active_tab: 9007199254741001,
            width: WindowWidth::Normal,
            maximized: false,
        }],
    }];
    WorkspaceTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    }
}
#[test]
fn canonical_topology_reconciles_without_replacing_unchanged_hosts() {
    let mut tree = tree();
    let mut surface = DesktopSurface::build(
        &tree,
        &RecordStore::in_memory(),
        &mut Unavailable,
        SurfaceChrome::default(),
    );
    assert_eq!(
        surface.reconcile(&tree, &RecordStore::in_memory(), &mut Unavailable),
        qos_desktop::composition::ReconcileOutcome::Unchanged
    );
    assert_eq!(surface.strip_ids(), vec![StripId(2)]);
    let row = surface.row_mut(StripId(2)).unwrap();
    assert_eq!(row.group_ids(), vec![3]);
    assert_eq!(row.groups[0].tabs()[0].id, 9007199254741001);
    assert_eq!(row.groups[0].tabs()[0].title, "My document");
    let before = row.groups[0].host_for_tab_mut(9007199254741001).unwrap() as *const _;
    tree.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Wide;
    surface.reconcile(&tree, &RecordStore::in_memory(), &mut Unavailable);
    let group = &mut surface.row_mut(StripId(2)).unwrap().groups[0];
    assert_eq!(group.width(), WindowWidth::Wide.to_ui());
    assert_eq!(
        group.host_for_tab_mut(9007199254741001).unwrap() as *const _,
        before
    );
    tree.workspaces[0].strips[0].window_groups.clear();
    surface.reconcile(&tree, &RecordStore::in_memory(), &mut Unavailable);
    assert!(surface.row_mut(StripId(2)).unwrap().groups.is_empty());
}
#[test]
fn stable_records_and_app_identity_do_not_depend_on_titles() {
    let tree = tree();
    let tab = &tree.workspaces[0].strips[0].window_groups[0].tabs[0];
    assert_eq!(
        qos_desktop::resolver::canonical_app_id(&tab.tile),
        Some("dev.qos.word-processor".to_string())
    );
    assert_eq!(
        qos_desktop::records::scratch_tab_record(tab.id)
            .id
            .to_string(),
        "tab:0020000000000009"
    );
    assert_eq!(
        qos_desktop::records::component_record(
            "dev.qos.chat",
            tab.id,
            ui::record::RecordKind::new("chat")
        )
        .id,
        ui::record::RecordId::package_primary()
    );
}
#[test]
fn actual_frame_lays_out_shared_widgets_and_tab_identity() {
    let tree = tree();
    let surface = DesktopSurface::build(
        &tree,
        &RecordStore::in_memory(),
        &mut Unavailable,
        SurfaceChrome::default(),
    );
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 720));
    frame.poll_frame().unwrap();
    let output = frame.inspection_json().unwrap();
    assert!(output.contains("GroupRow"));
    assert!(output.contains("WorkspaceView"));
    assert!(output.contains("StripView"));
    let scene_path_tags = frame.frame_output().scene.encoding().path_tags.len();
    assert!(scene_path_tags > 0);
    if let Some(path) = std::env::var_os("QOS_DESKTOP_LAYOUT_EVIDENCE") {
        let evidence = serde_json::json!({
            "viewport": [1280, 720],
            "scene_path_tags": scene_path_tags,
            "inspection": serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        });
        std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}
#[test]
fn local_selection_emits_intent_without_mutating_canonical_tree() {
    let mut tree = tree();
    let mut second = tree.workspaces[0].strips[0].window_groups[0].tabs[0].clone();
    second.id = 19;
    tree.workspaces[0].strips[0].window_groups[0]
        .tabs
        .push(second);
    let mut surface = DesktopSurface::build(
        &tree,
        &RecordStore::in_memory(),
        &mut Unavailable,
        SurfaceChrome::default(),
    );
    surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab(19);
    assert_eq!(
        surface.take_intents(),
        vec![qos_desktop::intents::DesktopIntent::Local(
            qos_desktop::intents::LocalViewIntent::Tab { group: 3, tab: 19 }
        )]
    );
    assert!(surface.take_intents().is_empty());
    assert_eq!(
        tree.workspaces[0].strips[0].window_groups[0].active_tab,
        9007199254741001
    );
}

// A counted top-layer app makes omission or double painting observable without pixel heuristics.
struct OverlayApp {
    capture: bool,
    leaf: ui::widget::Container,
    draws: std::rc::Rc<std::cell::Cell<usize>>,
}
impl ui::widget::Widget for OverlayApp {
    fn next_wake_deadline(&self) -> Option<ui::clock::Instant> {
        self.capture.then(ui::clock::Instant::now)
    }
    fn modal_capture(&self) -> Option<vello::kurbo::Rect> {
        self.capture
            .then_some(vello::kurbo::Rect::new(0.0, 0.0, 50.0, 50.0))
    }

    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.leaf.apply_layout(tree, node, origin);
    }
    fn name(&self) -> &str {
        "OverlayApp"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.leaf.get_rect()
    }

    fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::Widget>,
    ) -> taffy::NodeId {
        self.leaf.build_layout(tree, theme, map)
    }
    fn draw(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::presence::PresenceCtx,
    ) {
    }
    fn draw_overlay(
        &mut self,
        _: &mut vello::Scene,
        _: vello::kurbo::Affine,
        _: &mut ui::TextRenderer,
        _: &ui::Theme,
        _: &ui::presence::PresenceCtx,
    ) {
        self.draws.set(self.draws.get() + 1);
    }
    fn handle_event(
        &mut self,
        _: &winit::event::WindowEvent,
        _: f64,
        _: &winit::event::Modifiers,
        _: &mut automerge::AutoCommit,
        _: &automerge::ObjId,
        _: &mut bool,
        _: &mut Option<taffy::NodeId>,
        _: taffy::NodeId,
        _: &taffy::TaffyTree<()>,
    ) {
    }
}
impl ui::widget::AppWidget for OverlayApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &str {
        "Overlay app"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Named("overlay".into())
    }
    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
    }
}
struct Overlays(std::collections::HashMap<u64, std::rc::Rc<std::cell::Cell<usize>>>);
impl TileResolver for Overlays {
    fn resolve(&mut self, request: TileRequest<'_>) -> TileResolution {
        TileResolution::Hosted(Box::new(OverlayApp {
            capture: request.tab.id == 19,
            leaf: ui::widget::Container::new(),
            draws: self.0[&request.tab.id].clone(),
        }))
    }
}
#[test]
fn grouped_application_overlay_is_drawn_once_for_the_active_tab_only() {
    let mut tree = tree();
    let group = &mut tree.workspaces[0].strips[0].window_groups[0];
    let first = group.tabs[0].id;
    let mut second = group.tabs[0].clone();
    second.id = 19;
    group.tabs.push(second);
    let active = std::rc::Rc::new(std::cell::Cell::new(0));
    let inactive = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut resolver = Overlays(
        [(first, active.clone()), (19, inactive.clone())]
            .into_iter()
            .collect(),
    );
    let records = RecordStore::in_memory();
    for id in [first, 19] {
        qos_desktop::records::create_scratch_tab_record(&records, id).unwrap();
    }
    let surface = DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
    assert!(
        ui::Widget::modal_capture(&surface).is_none(),
        "inactive modal must not capture the active tab's input"
    );
    assert!(
        ui::Widget::next_wake_deadline(&surface.row(StripId(2)).unwrap().groups[0]).is_none(),
        "inactive presentation deadlines must not keep the active frame awake"
    );
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 720));
    let before = active.get();
    frame.resize((1281, 720)).unwrap();
    assert_eq!(
        active.get() - before,
        1,
        "the top-layer pass must reach the grouped active app once"
    );
    assert_eq!(
        inactive.get(),
        0,
        "inactive tab menus must not paint over the active document"
    );
}

#[test]
fn replacing_the_room_requires_fresh_component_contexts() {
    let mut tree = tree();
    let records = RecordStore::in_memory();
    let mut surface =
        DesktopSurface::build(&tree, &records, &mut Unavailable, SurfaceChrome::default());
    tree.workspaces[0].room = qos_desktop::RoomId::new();
    assert_eq!(
        surface.reconcile(&tree, &records, &mut Unavailable),
        qos_desktop::composition::ReconcileOutcome::RebuildRequired
    );
}

#[test]
fn moving_a_tab_with_changed_application_resolves_the_new_identity() {
    struct Resolved {
        ids: Vec<String>,
        ready: bool,
    }
    impl TileResolver for Resolved {
        fn ready_to_replace(&self, _: &Tab) -> bool {
            self.ready
        }
        fn resolve(&mut self, request: TileRequest<'_>) -> TileResolution {
            self.ids
                .push(qos_desktop::resolver::canonical_app_id(&request.tab.tile).unwrap());
            Unavailable.resolve(request)
        }
    }
    for ready_on_move in [true, false] {
        let mut tree = tree();
        let records = RecordStore::in_memory();
        let mut resolver = Resolved {
            ids: vec![],
            ready: ready_on_move,
        };
        let mut surface =
            DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        group.id = 99;
        group.tabs[0].tile = TileContent::AppPlaceholder {
            kind: AppKind::Other("dev.qos.notes".into()),
            label: "Notes".into(),
        };
        surface.reconcile(&tree, &records, &mut resolver);
        if !ready_on_move {
            assert_eq!(resolver.ids, ["dev.qos.word-processor"]);
            resolver.ready = true;
            surface.reconcile(&tree, &records, &mut resolver);
        }
        assert_eq!(resolver.ids, ["dev.qos.word-processor", "dev.qos.notes"]);
        assert_eq!(surface.row(StripId(2)).unwrap().groups[0].id, 99);
    }
}

fn empty_workspace_chrome() -> SurfaceChrome {
    SurfaceChrome {
        empty_workspace: Some(Box::new(
            ui::widget::Text::new("Empty workspace").with_id("empty-workspace-marker"),
        )),
        ..SurfaceChrome::default()
    }
}
fn has_empty_workspace_marker(widget: &dyn ui::Widget) -> bool {
    widget.test_id() == Some("empty-workspace-marker")
        || widget
            .children()
            .into_iter()
            .any(has_empty_workspace_marker)
}
fn check_empty_workspace_transition(initially_empty: bool) {
    let populated = tree();
    let mut empty = populated.clone();
    empty.workspaces[0].strips[0].window_groups.clear();
    let (before, after) = if initially_empty {
        (&empty, &populated)
    } else {
        (&populated, &empty)
    };
    let records = RecordStore::in_memory();
    let mut surface =
        DesktopSurface::build(before, &records, &mut Unavailable, empty_workspace_chrome());
    assert_eq!(has_empty_workspace_marker(&surface), initially_empty);
    assert_eq!(surface.strip_ids(), vec![StripId(2)]);
    assert_eq!(
        surface.reconcile(after, &records, &mut Unavailable),
        qos_desktop::composition::ReconcileOutcome::RebuildRequired
    );
    // The host replaces only after handling the explicit rebuild outcome.
    surface = DesktopSurface::build(after, &records, &mut Unavailable, empty_workspace_chrome());
    assert_eq!(surface.strip_ids(), vec![StripId(2)]);
    assert_eq!(has_empty_workspace_marker(&surface), !initially_empty);
    assert_eq!(
        surface.reconcile(after, &records, &mut Unavailable),
        qos_desktop::composition::ReconcileOutcome::Unchanged
    );
}
#[test]
fn opening_first_group_rebuilds_empty_workspace_child_with_same_strips() {
    check_empty_workspace_transition(true);
}
#[test]
fn closing_last_group_rebuilds_empty_workspace_child_with_same_strips() {
    check_empty_workspace_transition(false);
}
