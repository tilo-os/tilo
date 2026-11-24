use super::*;

/// Run `f` on the toolkit's clock at `ms` after `start`: a frame of a
/// test's own timeline, however long the machine takes.
fn at<T>(start: Instant, ms: u64, f: impl FnOnce() -> T) -> T {
    crate::time::testing::with_monotonic_time(start + Duration::from_millis(ms), f)
}
use crate::testing::Dispatch;
use crate::widget::{AppKind, TabDescriptor, TileContent};
use winit::event::MouseButton;

#[test]
fn external_navigation_omits_bar_from_layout_and_semantics_but_retains_tabs() {
    let mut group = make_group();
    assert_eq!(group.children().len(), 4);
    group.set_external_navigation(true);
    let mut tree = TaffyTree::new();
    let node = group.build_layout(&mut tree, &Theme::default(), &mut Default::default());
    assert_eq!(tree.children(node).unwrap().len(), 1);
    assert_eq!(group.children().len(), 1);
    assert_eq!(group.tabs().len(), 2);
}
#[test]
fn external_windows_paint_to_their_border_without_spilling_into_neighbors() {
    use crate::offscreen::OffscreenRenderer;
    use taffy::prelude::TaffyMaxContent;
    let theme = Theme::dark();
    let mut renderer = OffscreenRenderer::new(400, 300);
    for focused in [false, true] {
        let mut group = make_group();
        group.set_external_navigation(true);
        group.set_focused(focused);
        let mut tree = TaffyTree::new();
        let node = group.build_layout(&mut tree, &theme, &mut Default::default());
        let mut style = tree.style(node).unwrap().clone();
        style.size.width = Dimension::length(200.0);
        tree.set_style(node, style).unwrap();
        let root = tree.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(200.0),
                    height: Dimension::length(200.0),
                },
                ..Default::default()
            },
            &[node],
        );
        tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
        group.apply_layout(&tree, node, Point::new(100.0, 50.0));
        assert_eq!(group.rect, Rect::new(100.0, 50.0, 300.0, 250.0));
        let mut scene = Scene::new();
        group.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        let rgba = renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 255));
        for x in [96, 97, 98, 99, 300, 301, 302, 303] {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 400, x, 150);
            assert_eq!(
                (r, g, b),
                (0, 0, 255),
                "window spilled into neighbor at x={x}, focused={focused}"
            );
        }
        for x in [100, 299] {
            let (r, g, b, _) = OffscreenRenderer::sample(&rgba, 400, x, 150);
            assert_ne!(
                (r, g, b),
                (0, 0, 255),
                "gap before window border at x={x}, focused={focused}"
            );
        }
    }
}

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sample points are small positive pixel coordinates"
)]
fn a_resting_pointer_lights_the_whole_edge_that_will_change() {
    use crate::offscreen::OffscreenRenderer;
    use taffy::prelude::TaffyMaxContent;
    let theme = Theme::dark();
    let mut renderer = OffscreenRenderer::new(400, 300);
    let mut render = |group: &mut WindowGroup| {
        let mut tree = TaffyTree::new();
        let node = group.build_layout(&mut tree, &theme, &mut Default::default());
        let mut style = tree.style(node).unwrap().clone();
        style.size.width = Dimension::length(200.0);
        tree.set_style(node, style).unwrap();
        let root = tree.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(200.0),
                    height: Dimension::length(240.0),
                },
                ..Default::default()
            },
            &[node],
        );
        tree.compute_layout(root, Size::MAX_CONTENT).unwrap();
        group.apply_layout(&tree, node, Point::new(100.0, 30.0));
        let mut scene = Scene::new();
        group.draw(&mut crate::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            crate::presence::PresenceCtx::empty().draw_env(),
        ));
        renderer.render(&scene, vello::peniko::Color::from_rgb8(0, 0, 255))
    };
    let start = crate::time::now();
    let later = start.checked_add(EDGE_HOVER_DWELL).unwrap();
    use crate::time::testing::with_monotonic_time as at;
    let mut group = make_group();
    let idle = at(start, || render(&mut group));
    let rect = group.rect;
    let px = |rgba: &[u8], x: f64, y: f64| {
        let (r, g, b, _) = OffscreenRenderer::sample(rgba, 400, x as u32, y as u32);
        (r, g, b)
    };
    let trailing = rect.x1 - EDGE_LIT_PX / 2.0;
    let leading = rect.x0 + EDGE_LIT_PX / 2.0;
    let card = group.card_rect();
    let heights = [card.y0 + 24.0, card.center().y, card.y1 - 24.0];

    let passing = at(start, || {
        group.set_edge_highlight(Some(WindowEdge::Trailing));
        render(&mut group)
    });
    for y in heights {
        assert_eq!(
            px(&passing, trailing, y),
            px(&idle, trailing, y),
            "passing over an edge lights nothing (y = {y})"
        );
    }
    assert!(
        at(later, || group.needs_redraw()
            || group.needs_animation_frame()),
        "the dwell's end is a frame of its own"
    );
    assert_eq!(group.next_wake_deadline(), Some(later));

    let rested = at(later, || render(&mut group));
    for y in heights {
        assert_ne!(
            px(&rested, trailing, y),
            px(&idle, trailing, y),
            "a resting pointer lights the edge a press would resize (y = {y})"
        );
        assert_eq!(
            px(&rested, leading, y),
            px(&idle, leading, y),
            "and only that edge"
        );
    }
    assert_eq!(
        group.edge_reveal_at(),
        None,
        "lit, it waits for nothing more"
    );

    let dragging = at(start, || {
        group.set_edge_highlight(None);
        group.begin_edge_drag(WindowEdge::Leading);
        render(&mut group)
    });
    let y = card.center().y;
    assert_ne!(
        px(&dragging, leading, y),
        px(&idle, leading, y),
        "a drag lights its edge at once"
    );
    let beside = (12..60)
        .map(|dx| rect.x0 + f64::from(dx))
        .any(|x| px(&dragging, x, y) != px(&idle, x, y));
    assert!(beside, "the window's size shows beside the dragged edge");
    group.end_edge_drag(true);
    assert!(
        group.needs_animation_frame(),
        "the size fades out once let go"
    );
}

#[test]
fn deferred_close_retains_host_pill_order_and_focus() {
    let mut group = make_group();
    assert!(group.request_close_tab(1));
    assert!(group.has_closing_host(1));
    assert!(group.cancel_tab_close(1));
    group.sweep_expired_closing_hosts();
    assert_eq!(
        group
            .bar
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(group.active_tab_id(), 1);
    assert!(group.has_host(1));
    assert!(!group.has_closing_host(1));
    assert!(!group.bar.is_closing(1));
    assert!(
        !group.cancel_tab_close(1),
        "restoring twice must not duplicate the pill"
    );
    assert_eq!(group.tab_count(), 2);
}

#[test]
fn a_host_keeps_its_address_while_other_tabs_come_and_go() {
    // The frame's layout pointer map names the shown host by address
    // until the next structural rebuild; another tab opening, moving or
    // leaving must not move it.
    let mut group = make_group();
    let shown: *const TileHost = group.host_for_tab_mut(1).expect("tab 1");
    for id in 3..40 {
        group.insert_detached_tab(
            0,
            TabDescriptor::new(id, format!("T{id}")),
            TileHost::new(TileContent::Stub {
                kind: AppKind::Settings,
                label: format!("t{id}"),
            }),
        );
    }
    assert!(group.reorder_tab(0, 5));
    assert!(group.detach_tab(2).is_some());
    let now: *const TileHost = group.host_for_tab_mut(1).expect("tab 1");
    assert_eq!(shown, now);
}

fn group_of(tabs: u64) -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = (1..=tabs)
        .map(|id| {
            (
                TabDescriptor::new(id, format!("Tab {id}")),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: format!("s{id}"),
                },
            )
        })
        .collect();
    WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
}

/// `TILO_WINDOW_BAR_SHOTS=<dir>` writes the bar's every state — a
/// window of two tabs with its active tab, the same window unfocused,
/// a window of one tab, which shows its one tab as the window's title,
/// and the same title in a window that is not the one in use — in the
/// dark and light themes.
#[test]
fn window_bar_shots_of_every_state() {
    let Ok(directory) = std::env::var("TILO_WINDOW_BAR_SHOTS") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    for (scheme, theme) in [
        ("dark", crate::Theme::dark()),
        ("light", crate::Theme::light()),
    ] {
        for (state, tabs, focused) in [
            ("two-tabs", 2_u64, true),
            ("two-tabs-unfocused", 2, false),
            ("one-tab", 1, true),
            ("one-tab-unfocused", 1, false),
        ] {
            let mut group = group_of(tabs);
            group.set_focused(focused);
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                group,
                (900, 260),
                theme.clone(),
            );
            harness.relayout();
            harness.render_png(&directory.join(format!("window-bar-{state}-{scheme}.png")));
        }
    }
}

pub(super) fn make_group() -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        (
            TabDescriptor::new(1, "A"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "a".into(),
            },
        ),
        (
            TabDescriptor::new(2, "B"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "b".into(),
            },
        ),
    ];
    WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
}

#[derive(Default)]
struct RetainedTraversalHits {
    unsaved: std::cell::Cell<u32>,
    checkpoint: std::cell::Cell<u32>,
    finish: std::cell::Cell<u32>,
    history: std::cell::Cell<u32>,
    history_mut: std::cell::Cell<u32>,
}

struct RetainedTraversalProbe {
    hits: std::rc::Rc<RetainedTraversalHits>,
    pending_control: bool,
}

impl Widget for RetainedTraversalProbe {
    crate::provides!(
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn crate::widget::controls::ActionControls,
        dyn crate::component_lifecycle::ComponentLifecycle
    );

    crate::testing::inert_widget_methods!(name = "RetainedTraversalProbe");
    fn has_pending_visual_work(&self) -> bool {
        self.pending_control
    }
    fn has_transient_feedback(&self) -> bool {
        self.pending_control
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(Style::default())
    }
}

impl crate::component_lifecycle::ComponentLifecycle for RetainedTraversalProbe {
    fn unsaved_documents(&self, _: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        self.hits.unsaved.set(self.hits.unsaved.get() + 1);
        vec![]
    }
    fn checkpoint_components(&mut self) -> Vec<String> {
        self.hits.checkpoint.set(self.hits.checkpoint.get() + 1);
        vec![]
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.hits.finish.set(self.hits.finish.get() + 1);
        vec![]
    }
}

impl crate::widget::controls::ActionControls for RetainedTraversalProbe {
    fn has_pending_control_invocation(&self) -> bool {
        self.pending_control
    }
}

impl crate::history_mode::capability::FrameworkHistoryTopology for RetainedTraversalProbe {
    fn visit_framework_history(
        &self,
        _: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        self.hits.history.set(self.hits.history.get() + 1);
    }
    fn visit_framework_history_mut(
        &mut self,
        _: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        self.hits.history_mut.set(self.hits.history_mut.get() + 1);
    }
}

crate::testing::test_app_widget!(
    RetainedTraversalProbe,
    "Retained traversal probe",
    icon = crate::widget::app_widget::IconDescriptor::Glyph("R"),
    kind = AppKind::Settings
);

#[test]
fn inactive_tabs_retain_session_collaboration_delivery() {
    let active = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "active-session",
        "fixture.active-session",
    );
    let inactive = crate::history_mode::isolation_tests::native_effect_probe_parts(
        "inactive-session",
        "fixture.inactive-session",
    );
    let active_events = std::rc::Rc::clone(&active.collab_events);
    let inactive_events = std::rc::Rc::clone(&inactive.collab_events);
    let tabs = vec![
        (
            TabDescriptor::new(1, "Active"),
            TileContent::App(active.root),
        ),
        (
            TabDescriptor::new(2, "Inactive"),
            TileContent::App(inactive.root),
        ),
    ];
    let mut group = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

    crate::widget::handle_collab_event(
        &mut group,
        &::collaboration::CollabEvent::FollowChanged { peer_id: None },
    );
    assert_eq!(active_events.get(), 1);
    assert_eq!(inactive_events.get(), 1);
}

#[test]
fn inactive_tabs_remain_in_lifecycle_history_and_resource_traversal() {
    let active = std::rc::Rc::new(RetainedTraversalHits::default());
    let inactive = std::rc::Rc::new(RetainedTraversalHits::default());
    let tabs = vec![
        (
            TabDescriptor::new(1, "Active"),
            TileContent::App(Box::new(RetainedTraversalProbe {
                hits: active.clone(),
                pending_control: false,
            })),
        ),
        (
            TabDescriptor::new(2, "Inactive"),
            TileContent::App(Box::new(RetainedTraversalProbe {
                hits: inactive.clone(),
                pending_control: true,
            })),
        ),
    ];
    let mut group = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
    let doc = AutoCommit::new();

    crate::component_lifecycle::unsaved_documents(&group, &doc);
    crate::component_lifecycle::checkpoint_components(&mut group);
    crate::component_lifecycle::finish_admitted_close(&mut group);
    crate::history_mode::capability::visit_framework_history(&group, &mut |_, _| {});
    crate::history_mode::capability::visit_framework_history_mut(&mut group, &mut |_, _| {});
    assert!(crate::widget::controls::has_pending_control_invocation(
        &group
    ));

    assert!(
        !group.has_pending_visual_work(),
        "inactive work must not hold the active scene unsettled"
    );
    assert!(
        !group.has_transient_feedback(),
        "inactive feedback is not part of the presented scene"
    );
    group.bar.set_active(2);
    assert!(
        group.has_pending_visual_work(),
        "the same work counts when its tab becomes visible"
    );
    assert!(
        group.has_transient_feedback(),
        "the same feedback counts when its tab becomes visible"
    );

    for hits in [active, inactive] {
        assert_eq!(hits.unsaved.get(), 1);
        assert_eq!(hits.checkpoint.get(), 1);
        assert_eq!(hits.finish.get(), 1);
        assert_eq!(hits.history.get(), 1);
        assert_eq!(hits.history_mut.get(), 1);
    }
}

struct Starting {
    presented: bool,
    from: crate::arriving::Source,
}

impl Widget for Starting {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &crate::theme::Theme,
        _m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }
    fn name(&self) -> &'static str {
        "Starting"
    }
    fn presented(&self) -> bool {
        self.presented
    }
    fn arrives_from(&self) -> crate::arriving::Source {
        self.from.clone()
    }
}

impl crate::widget::app_widget::AppWidget for Starting {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Starting"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("S")
    }
}

struct Restructuring {
    drained: std::rc::Rc<std::cell::Cell<u32>>,
}

impl Widget for Restructuring {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &crate::theme::Theme,
        _m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }
    fn name(&self) -> &'static str {
        "Restructuring"
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.drained.set(self.drained.get() + 1);
        true
    }
}

impl crate::widget::app_widget::AppWidget for Restructuring {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Restructuring"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("R")
    }
}

#[test]
fn an_inactive_tab_does_not_rebuild_the_group_layout() {
    let drained = std::rc::Rc::new(std::cell::Cell::new(0));
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        (
            TabDescriptor::new(1, "Shown"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "shown".into(),
            },
        ),
        (
            TabDescriptor::new(2, "Behind"),
            TileContent::App(Box::new(Restructuring {
                drained: drained.clone(),
            })),
        ),
    ];
    let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let _ = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    for frame in 0..3 {
        assert!(
            !g.take_structure_dirty(),
            "frame {frame}: a tab with no layout nodes rebuilt the group's layout"
        );
    }
    assert_eq!(drained.get(), 3, "the inactive tab's flag is still drained");

    assert!(g.set_active_tab(2));
    assert!(
        g.take_structure_dirty(),
        "selecting the tab builds its layout"
    );
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let _ = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    assert!(
        g.take_structure_dirty(),
        "the active tab's own requests still reach the host"
    );
}

fn group_opening(title: &str, presented: bool, from: crate::arriving::Source) -> WindowGroup {
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
        TabDescriptor::new(1, title),
        TileContent::App(Box::new(Starting { presented, from })),
    )];
    let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
    g.rect = Rect::new(0.0, 0.0, 400.0, 300.0);
    g
}

fn painted_anything(g: &mut WindowGroup) -> bool {
    use taffy::prelude::TaffyMaxContent;
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = g.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.parent_node(Style::default(), &[node]);
    taffy
        .compute_layout(root, taffy::Size::MAX_CONTENT)
        .unwrap();
    g.apply_layout(&taffy, node, Point::ZERO);

    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    g.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        crate::presence::PresenceCtx::empty().draw_env(),
    ));
    !scene.encoding().path_data.is_empty()
}

#[test]
fn a_window_holds_its_first_frame_only_for_its_budget_then_reserves_its_place() {
    {
        let scene = Scene::new();
        assert!(
            scene.encoding().path_data.is_empty(),
            "an untouched scene must read as nothing painted, or the \
         held case passes for free",
        );
    }

    let t0 = crate::time::now();
    let budget = crate::arriving::frame_budget();
    {
        let mut g = crate::time::testing::with_monotonic_time(t0, || {
            group_opening("Files", false, crate::arriving::Source::Local)
        });
        crate::time::testing::with_monotonic_time(t0, || {
            assert_eq!(
                Widget::holds_frame_until(&g, t0),
                Some(t0 + budget),
                "a first frame from this device holds the window's for one frame"
            );
            assert!(!g.stands());
            assert!(
                !painted_anything(&mut g),
                "a window frame went up before its app's first frame had its \
             budget, which is the window appearing empty and then filled",
            );
        });
        crate::time::testing::with_monotonic_time(t0 + budget, || {
            assert_eq!(Widget::holds_frame_until(&g, t0 + budget), None);
            assert!(g.stands(), "past the budget its place is reserved");
            assert!(
                painted_anything(&mut g),
                "the shell stayed empty for an app the person asked for and \
             is still waiting on",
            );
            assert!(g.any_tile_arriving());
        });
    }

    {
        // Another device's frame never holds this one's: the place is
        // reserved from the first frame.
        let mut g = crate::time::testing::with_monotonic_time(t0, || {
            group_opening("Notes", false, crate::arriving::Source::Remote)
        });
        crate::time::testing::with_monotonic_time(t0, || {
            assert_eq!(Widget::holds_frame_until(&g, t0), None);
            assert!(g.stands());
            assert!(painted_anything(&mut g));
        });
    }

    {
        let tabs: Vec<(TabDescriptor, TileContent)> = vec![
            (
                TabDescriptor::new(1, "Files"),
                TileContent::App(Box::new(Starting {
                    presented: true,
                    from: crate::arriving::Source::Local,
                })),
            ),
            (
                TabDescriptor::new(2, "Notes"),
                TileContent::App(Box::new(Starting {
                    presented: false,
                    from: crate::arriving::Source::Local,
                })),
            ),
        ];
        let g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);
        assert!(
            Widget::presented(&g),
            "one of its tabs is a window on screen"
        );
        assert_eq!(
            Widget::holds_frame_until(&g, crate::time::now()),
            None,
            "a group holding a window the person is using never holds a \
         frame for the tab beside it",
        );
        assert!(g.stands());
    }
}

struct Latching {
    drawn: bool,
    latched: std::rc::Rc<std::cell::Cell<bool>>,
    asked: std::rc::Rc<std::cell::Cell<u32>>,
}

impl Latching {
    fn new(
        drawn: bool,
    ) -> (
        Self,
        std::rc::Rc<std::cell::Cell<bool>>,
        std::rc::Rc<std::cell::Cell<u32>>,
    ) {
        let latched = std::rc::Rc::new(std::cell::Cell::new(false));
        let asked = std::rc::Rc::new(std::cell::Cell::new(0));
        (
            Self {
                drawn,
                latched: latched.clone(),
                asked: asked.clone(),
            },
            latched,
            asked,
        )
    }
}

impl Widget for Latching {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &crate::theme::Theme,
        _m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }
    fn name(&self) -> &'static str {
        "Latching"
    }
    fn presented(&self) -> bool {
        self.asked.set(self.asked.get().saturating_add(1));
        if self.drawn {
            self.latched.set(true);
        }
        self.latched.get()
    }
}

impl crate::widget::app_widget::AppWidget for Latching {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Latching"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("L")
    }
}

fn group_of_two(
    first_drawn: bool,
    second_drawn: bool,
) -> (
    WindowGroup,
    std::rc::Rc<std::cell::Cell<bool>>,
    std::rc::Rc<std::cell::Cell<u32>>,
) {
    let (a, _, _) = Latching::new(first_drawn);
    let (b, latched, asked) = Latching::new(second_drawn);
    let tabs: Vec<(TabDescriptor, TileContent)> = vec![
        (
            TabDescriptor::new(1, "Notes"),
            TileContent::App(Box::new(a)),
        ),
        (
            TabDescriptor::new(2, "Files"),
            TileContent::App(Box::new(b)),
        ),
    ];
    (
        WindowGroup::new(10, tabs, 2, WindowWidth::Normal),
        latched,
        asked,
    )
}

#[test]
fn the_placeholder_retires_when_its_tabs_window_has_drawn() {
    {
        let (g, latched, asked) = group_of_two(true, true);

        assert!(Widget::presented(&g), "both of its tabs have drawn");
        assert!(
            asked.get() > 0,
            "the second tab was never asked whether its window had drawn, so \
         nothing recorded that it had",
        );
        assert!(
            latched.get(),
            "a window that has drawn must end up marked as having drawn",
        );
        assert!(
            !g.any_tile_arriving(),
            "and must not still be there for its own place",
        );
    }

    {
        let (g, _latched, _asked) = group_of_two(true, true);
        let _ = Widget::presented(&g);
        let host = g
            .hosts
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, h)| h)
            .expect("the group holds the tab it was built with");
        assert!(
            !host.is_arriving(),
            "the placeholder in this window's place must be gone: the window \
         is drawn, populated, and the one on screen",
        );
    }

    {
        let (g, latched, _asked) = group_of_two(true, false);
        let _ = Widget::presented(&g);
        assert!(
            !latched.get(),
            "asking is not answering — a window that has not drawn must not be \
         recorded as having drawn",
        );
        let host = g
            .hosts
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, h)| h)
            .expect("the group holds the tab it was built with");
        assert!(host.is_arriving(), "its place is still reserved");
    }
}

fn dispatch_at(
    g: &mut WindowGroup,
    taffy: &TaffyTree<()>,
    node: NodeId,
    event: &WindowEvent,
) -> bool {
    Dispatch::new().layout(taffy, node).send(g, event).handled
}

#[allow(
    clippy::type_complexity,
    reason = "test fixture bundling everything one maximize-click test needs; a named struct would only move the same fields, not reduce them"
)]
fn open_maximize_chip() -> (WindowGroup, TaffyTree<()>, NodeId, WindowEvent, WindowEvent) {
    use crate::theme::Theme;
    use std::collections::HashMap;
    use taffy::Style;
    use taffy::prelude::TaffyMaxContent;
    use winit::event::{ElementState, MouseButton};

    let mut g = make_group();
    assert!(!g.maximized, "fresh group must start non-maximized");

    // `max_rects` must be populated by `apply_layout` before the
    // click; reduce-motion so the pill's reveal lands in one pass.
    let theme = Theme::dark().with_reduce_motion(true);
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.parent_node(Style::default(), &[group_node]);
    taffy
        .compute_layout(root, taffy::Size::MAX_CONTENT)
        .unwrap();
    g.apply_layout(&taffy, group_node, Point::ZERO);

    let click_pt = g.bar.window_controls().maximize.center();

    let moved = WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(click_pt.x, click_pt.y),
    };
    let press = WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    };

    (g, taffy, group_node, moved, press)
}

#[test]
fn maximize_toggles_rebuild_layout_and_drain_once() {
    {
        let (mut g, taffy, group_node, moved, press) = open_maximize_chip();

        let release = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        dispatch_at(&mut g, &taffy, group_node, &moved);
        let handled = dispatch_at(&mut g, &taffy, group_node, &press);
        assert!(handled, "press on the maximize control must be claimed");
        dispatch_at(&mut g, &taffy, group_node, &release);
        assert!(
            g.maximized,
            "a click on the window's maximize control maximizes it",
        );

        dispatch_at(&mut g, &taffy, group_node, &moved);
        dispatch_at(&mut g, &taffy, group_node, &press);
        dispatch_at(&mut g, &taffy, group_node, &release);
        assert!(!g.maximized, "a second click restores it");
    }

    {
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        assert!(!g.take_structure_dirty(), "fresh group is not dirty");

        g.bar.pending_max = Some(1);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        dispatch_at(&mut g, &taffy, group_node, &moved);

        assert!(g.maximized, "maximize drain must flip the flag");
        assert!(
            g.take_structure_dirty(),
            "maximize toggle must report structure dirty so App re-runs build_layout",
        );
        assert!(!g.take_structure_dirty(), "structure_dirty drains on poll");
    }

    {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        assert!(!g.take_structure_dirty(), "fresh group is not dirty");

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);
        assert!(
            !g.take_structure_dirty(),
            "a built group with no tab switch stays clean",
        );

        assert!(g.set_active_tab(2), "the second tab is a live target");
        assert!(
            g.take_structure_dirty(),
            "active-tab switch must report structure dirty so the new tile gets a node",
        );

        let mut taffy2 = TaffyTree::<()>::new();
        let mut node_map2: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = g.build_layout(&mut taffy2, &theme, &mut node_map2);
        assert!(
            !g.take_structure_dirty(),
            "structure_dirty drains once the host rebuilds for the new active tab",
        );
    }

    {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let mut g = make_group();
        assert_eq!(
            g.take_pending_maximize_change(),
            None,
            "no pending change before any click",
        );

        g.bar.pending_max = Some(1);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        let moved = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        dispatch_at(&mut g, &taffy, group_node, &moved);

        assert!(g.maximized, "drain must flip maximized true");
        assert_eq!(
            g.take_pending_maximize_change(),
            Some(true),
            "first poll after the click reports the new value",
        );
        assert_eq!(
            g.take_pending_maximize_change(),
            None,
            "take-pattern: drained on first poll, second poll is None",
        );

        g.bar.pending_max = Some(1);
        dispatch_at(&mut g, &taffy, group_node, &moved);
        assert!(!g.maximized, "second drain must flip back to false");
        assert_eq!(
            g.take_pending_maximize_change(),
            Some(false),
            "the second click reports the new (false) value once",
        );
    }
}

#[test]
fn tab_bar_requests_reorder_search_and_pin_keep_hosts_aligned() {
    {
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        let theme = Theme::dark();
        let mut g = make_group();
        assert_eq!(
            g.take_pending_tab_reorder(),
            None,
            "nothing pending initially"
        );

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();

        g.bar_mut_for_tests().pending_reorder = Some((0, 1));
        let moved = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
        };
        dispatch_at(&mut g, &taffy, node, &moved);

        assert_eq!(
            g.take_pending_tab_reorder(),
            Some((0, 1)),
            "a harvested reorder publishes (from, to) for the shell exactly once",
        );
        assert_eq!(
            g.take_pending_tab_reorder(),
            None,
            "take-pattern: drained on the first poll",
        );
    }

    // Case `reorder_keeps_hosts_and_bar_tabs_aligned`.
    // Parallel-vec invariant: drift between `bar.tabs()` and `hosts`
    // would render tab pill `i`'s content from a different host.
    {
        use crate::widget::{AppKind, TabDescriptor, TileContent};
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;

        fn make_group_4() -> WindowGroup {
            let tabs: Vec<(TabDescriptor, TileContent)> = (1..=4u64)
                .map(|i| {
                    (
                        TabDescriptor::new(i, format!("T{i}")),
                        TileContent::Stub {
                            kind: AppKind::Settings,
                            label: format!("s{i}"),
                        },
                    )
                })
                .collect();
            WindowGroup::new(10, tabs, 1, WindowWidth::Normal)
        }

        let cases: &[(usize, usize)] = &[(0, 2), (3, 1), (1, 2), (2, 2), (0, 3)];

        for &(from, to) in cases {
            let mut g = make_group_4();

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "precondition: tab[{i}] id must equal hosts[{i}].0",
                );
            }

            g.bar.pending_reorder = Some((from, to));

            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(9000.0, 9000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);

            assert_eq!(
                g.bar.tabs().len(),
                g.hosts.len(),
                "case ({from}, {to}): bar.tabs().len() must equal hosts.len() after reorder",
            );
            for (i, t) in g.bar.tabs().iter().enumerate() {
                assert_eq!(
                    t.id, g.hosts[i].0,
                    "case ({from}, {to}): bar.tabs()[{i}].id must equal hosts[{i}].0 after reorder",
                );
            }
        }
    }

    {
        let mut g = make_group();
        assert!(!g.search_active, "fresh group must not be searching");
        g.bar.pending_search = Some(1);
        if let Some(_id) = g.bar.take_search() {
            g.search_active = !g.search_active;
        }
        assert!(g.search_active, "drain should flip search_active on");
        g.bar.pending_search = Some(1);
        if let Some(_id) = g.bar.take_search() {
            g.search_active = !g.search_active;
        }
        assert!(
            !g.search_active,
            "second drain should flip search_active off"
        );
    }

    {
        use crate::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::Modifiers;
        use winit::keyboard::{Key, ModifiersState, SmolStr};

        fn key_press(logical_key: Key) -> WindowEvent {
            crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
                logical_key,
                None,
                ElementState::Pressed,
            ))
        }

        let mut g = make_group();
        let active = g.bar.active_id();
        assert!(
            !g.bar.is_pinned(active),
            "fresh group: active tab is unpinned"
        );

        let mods = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
        let event = key_press(Key::Character(SmolStr::new_inline("p")));

        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[group_node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, group_node, Point::ZERO);

        let mut ctrl_shift = Dispatch::new().layout(&taffy, group_node).modifiers(mods);
        let handled = ctrl_shift.send(&mut g, &event).handled;
        assert!(handled, "Ctrl+Shift+P press must be claimed");
        assert!(
            g.bar.is_pinned(active),
            "first Ctrl+Shift+P must pin the active tab",
        );

        let handled = ctrl_shift.send(&mut g, &event).handled;
        assert!(handled, "second Ctrl+Shift+P press is also claimed");
        assert!(
            !g.bar.is_pinned(active),
            "second Ctrl+Shift+P must unpin the active tab",
        );

        dispatch_at(&mut g, &taffy, group_node, &event);
        assert!(
            !g.bar.is_pinned(active),
            "plain `P` (no Ctrl+Shift) must not toggle pin state",
        );
    }
}

pub(super) fn g_normal_width() -> WindowWidth {
    WindowWidth::Normal
}

#[test]
fn maximize_restore_springs_the_drawn_rect_from_the_tiled_part() {
    use std::collections::HashMap;
    use taffy::Style;
    use taffy::prelude::TaffyMaxContent;

    fn layout(g: &mut WindowGroup, theme: &Theme, w: f32, h: f32) -> Rect {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = g.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        g.apply_layout(&taffy, node, Point::ZERO);
        g.get_rect()
    }

    {
        let theme = Theme::dark();
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        let t0 = crate::time::now();
        let tiled = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
        assert!(
            tiled.width() > 100.0 && tiled.width() < 1600.0,
            "tiled part must be narrower than the work area, got {}",
            tiled.width(),
        );
        at(t0, 0, || g.toggle_maximized());
        assert!(
            g.needs_animation_frame(),
            "a maximize spring must pump frames",
        );
        let dest = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
        assert!(
            dest.width() > tiled.width() + 50.0,
            "layout must jump to the work area, tiled {} dest {}",
            tiled.width(),
            dest.width(),
        );
        let drawn = g.drawn_rect();
        assert!(
            (drawn.width() - tiled.width()).abs() < 1.0,
            "the first animated frame must still be the tiled rect, got {} want {}",
            drawn.width(),
            tiled.width(),
        );
        at(t0, 0, || g.step_rect_spring_for_tests());
        assert!(
            (g.drawn_rect().width() - tiled.width()).abs() < 1.0,
            "the arming frame does not move, so there is no jump at the start",
        );
        at(t0, 16, || g.step_rect_spring_for_tests());
        let mid = g.drawn_rect().width();
        assert!(
            mid > tiled.width() + 1.0,
            "the drawn width must grow toward the work area, {mid} from tiled {}",
            tiled.width(),
        );
    }

    {
        let theme = Theme::dark();
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        let t0 = crate::time::now();
        let _ = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
        at(t0, 0, || g.toggle_maximized());
        let _ = at(t0, 0, || layout(&mut g, &theme, 1600.0, 900.0));
        at(t0, 0, || g.step_rect_spring_for_tests());
        at(t0, 48, || g.step_rect_spring_for_tests());
        let vel_before = g
            .rect_spring_width_velocity()
            .expect("maximize spring in flight");
        assert!(
            vel_before > 0.0,
            "opening maximize must grow, vel={vel_before}"
        );
        let mid = g.drawn_rect();
        at(t0, 48, || g.toggle_maximized());
        let _ = at(t0, 48, || layout(&mut g, &theme, 1600.0, 900.0));
        let vel_after = g
            .rect_spring_width_velocity()
            .expect("restore retargets the same spring");
        assert_eq!(
            vel_after, vel_before,
            "a new Super+M must keep velocity so the motion is interruptible",
        );
        assert!(
            (g.drawn_rect().width() - mid.width()).abs() < 0.5,
            "retargeting must not jump the drawn rect",
        );
    }

    {
        let theme = Theme::dark();
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        let _ = layout(&mut g, &theme, 1600.0, 900.0);
        g.reduce_motion = true;
        g.toggle_maximized();
        let dest = layout(&mut g, &theme, 1600.0, 900.0);
        assert!(
            g.rect_spring.is_none(),
            "reduced motion must not leave a spring ticking",
        );
        assert_eq!(
            g.drawn_rect(),
            dest,
            "reduced motion paints at the work-area rect immediately",
        );
        assert!(
            !g.needs_animation_frame(),
            "a snapped maximize must not pump frames",
        );
    }
}

#[test]
fn width_spring_opens_closes_and_resizes_from_the_current_value() {
    {
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        let target = g.resolved_width_px();
        assert!(target > 100.0, "fixture must have a real part to grow into");
        let t0 = crate::time::now();
        at(t0, 0, || g.start_spawn_animation());
        assert!(
            g.layout_width_px() < 1.0,
            "an opening group reserves no width until the spring moves, got {}",
            g.layout_width_px(),
        );
        assert!(
            g.needs_animation_frame(),
            "an in-flight width spring must pump frames",
        );
        let mut last = g.layout_width_px();
        for frame in 1..=8 {
            // Frames 16 ms apart on the spring's clock.
            at(t0, 16 * frame, || {
                g.next_frame();
                g.step_width_spring();
            });
            let now = g.layout_width_px();
            assert!(now >= last, "open must grow the part, {last} → {now}");
            last = now;
        }
        assert!(
            last > 1.0 && last < target,
            "after a few frames the part is open but not yet at the target ({last} of {target})",
        );
    }

    {
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        let t0 = crate::time::now();
        at(t0, 0, || g.start_spawn_animation());
        for frame in 1..=10 {
            at(t0, 16 * frame, || {
                g.next_frame();
                g.step_width_spring();
            });
        }
        let mid = g.layout_width_px();
        let vel_before = g
            .width_spring
            .as_ref()
            .map(FrameSpring::velocity)
            .expect("open spring still in flight");
        assert!(
            vel_before > 0.0,
            "opening must still be moving outward, vel={vel_before}",
        );
        at(t0, 160, || g.set_width(WindowWidth::Narrow));
        let vel_after = g
            .width_spring
            .as_ref()
            .map(FrameSpring::velocity)
            .expect("resize retargets the same spring");
        assert_eq!(
            vel_after, vel_before,
            "set_target must keep the current velocity so a new input is interruptible",
        );
        assert!(
            (g.layout_width_px() - mid).abs() < 0.5,
            "retargeting must not jump the part, was {mid} now {}",
            g.layout_width_px(),
        );
        assert!(
            g.width_spring.as_ref().is_some_and(|s| s.target() < mid),
            "Narrow on a 1200 px strip is smaller than the mid-open width",
        );
    }

    {
        // The row steps the part spring only while it rebuilds the
        // layout, and rebuilds only when a group asks for it. Frames at
        // 60 and at 20 a second must both end the motion.
        for frame_ms in [16, 50] {
            let mut g = group_opening("Files", true, crate::arriving::Source::Local);
            g.set_strip_usable_px(1200.0);
            let t0 = crate::time::now();
            at(t0, 0, || g.start_spawn_animation());
            let mut rebuilds = 0;
            while std::mem::take(&mut g.style_dirty) && rebuilds < 200 {
                rebuilds += 1;
                at(t0, frame_ms * rebuilds, || {
                    g.next_frame();
                    g.step_width_spring();
                });
            }
            assert!(
                g.width_spring.is_none(),
                "{frame_ms} ms frames: the part spring stalled at {:?} after {rebuilds} rebuilds",
                g.width_spring
                    .as_ref()
                    .map(|s| (s.value(), s.target(), s.velocity())),
            );
            assert!(
                (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
                "{frame_ms} ms frames: the part ends at its width",
            );
        }
    }

    {
        // The host's frame: rebuild when a widget asks (the row steps the spring in its build,
        // a frame may take two passes), then a painted frame turns the token. A tick that
        // rebuilds while the presenter is still busy paints nothing, and the step the next
        // rebuild skips must not leave the part stopped part-way, asking for frames forever.
        let mut g = group_opening("Notes", true, crate::arriving::Source::Local);
        g.set_strip_usable_px(1200.0);
        let t0 = crate::time::now();
        at(t0, 0, || g.start_spawn_animation());
        let mut frames = 0;
        while g.width_spring.is_some() && frames < 400 {
            at(t0, 16 * (frames + 1), || {
                if Widget::take_structure_dirty(&mut g) {
                    g.step_width_spring();
                }
                if frames % 7 != 3 {
                    g.frame_presented();
                }
            });
            frames += 1;
        }
        assert!(
            g.width_spring.is_none(),
            "the part spring froze at {:?} after {frames} frames",
            g.width_spring
                .as_ref()
                .map(|s| (s.value(), s.target(), s.velocity())),
        );
        assert!(
            (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
            "the part ends at its width"
        );
    }

    {
        let t0 = crate::time::now();
        let mut g = at(t0, 0, || {
            group_opening("Files", false, crate::arriving::Source::Local)
        });
        g.set_strip_usable_px(1200.0);
        assert!(
            !at(t0, 0, || painted_anything(&mut g)),
            "precondition: a group holding its first frame paints nothing",
        );
        let from = g.layout_width_px();
        at(t0, 0, || g.start_closing());
        assert!(
            (g.layout_width_px() - from).abs() < 0.5,
            "close must start from the last reserved width, not jump to zero",
        );
        assert!(
            at(t0, 0, || painted_anything(&mut g)),
            "a closing group is drawn from its last frame while the part collapses",
        );
        let mut last = g.layout_width_px();
        for frame in 1..=8 {
            // Frames 16 ms apart on the spring's clock.
            at(t0, 16 * frame, || {
                g.next_frame();
                g.step_width_spring();
            });
            let now = g.layout_width_px();
            assert!(now <= last, "close must collapse the part, {last} → {now}");
            last = now;
        }
        assert!(
            last < from,
            "the part must have started to collapse ({last} from {from})",
        );
    }

    {
        let mut g = make_group();
        g.set_strip_usable_px(1200.0);
        g.set_reduce_motion(true);
        g.start_spawn_animation();
        assert!(
            (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
            "reduced motion opens at the target width, no spring frames",
        );
        assert!(
            g.width_spring.is_none(),
            "reduced motion must not leave a spring ticking",
        );
        g.set_reduce_motion(false);
        g.rect = Rect::new(0.0, 0.0, g.resolved_width_px(), 400.0);
        g.set_width(WindowWidth::Narrow);
        assert!(
            g.width_spring.is_some(),
            "a live resize starts a spring when motion is allowed",
        );
        g.set_reduce_motion(true);
        g.next_frame();
        g.step_width_spring();
        assert!(
            (g.layout_width_px() - g.resolved_width_px()).abs() < 0.5,
            "reduced motion snaps a running resize to the target",
        );
    }
}

/// A host that lays out at ~12 fps (a headless session rendering in
/// software) steps the opening part 50 ms at a time. The part must
/// still settle at its width; it used to grow until the client was
/// configured `i32::MAX` wide.
#[test]
fn a_slow_host_opens_the_part_to_its_width() {
    let mut g = make_group();
    g.set_strip_usable_px(996.0);
    let target = g.resolved_width_px();
    let t0 = crate::time::now();
    at(t0, 0, || g.start_spawn_animation());
    for frame in 0..40 {
        at(t0, 80 * (frame + 1), || {
            g.next_frame();
            g.step_width_spring();
        });
        let width = g.layout_width_px();
        assert!(
            width <= target * 1.05,
            "frame {frame}: part at {width} past its {target} px width",
        );
    }
    assert!(
        g.width_spring.is_none() && (g.layout_width_px() - target).abs() < 0.5,
        "the part settles at {target}, got {}",
        g.layout_width_px(),
    );
}

#[test]
fn refused_final_tab_close_restores_live_group_input() {
    let mut group = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "solo"),
            TileContent::Stub {
                kind: AppKind::Settings,
                label: "a".into(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    assert!(group.request_close_tab(1));
    assert!(group.is_closing());
    assert_eq!(crate::bound_dispatch::keyboard_input_scope(&group), None);
    assert!(group.cancel_tab_close(1));
    assert!(!group.is_closing());
    assert!(!group.has_closing_host(1));
    assert_eq!(crate::bound_dispatch::keyboard_input_scope(&group), Some(1));
    assert!(group.has_host(1));
}

#[test]
fn closing_tab_hosts_fade_out_and_the_last_one_closes_the_group() {
    // One instant for the whole test: each case backdates its own
    // timestamps, so a descheduled runner cannot move them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        // Case `detach_tab_clears_closing_state_for_moved_tab`.
        // Its id sits in `closing_hosts` — a stale entry whose timer never
        // expires once the host is moved — so `needs_animation_frame` would
        // otherwise keep pumping redraws for a host that's gone.
        {
            let mut g = make_group();
            g.closing_hosts.insert(1, crate::time::now());
            assert!(
                g.closing_hosts.contains_key(&1),
                "precondition: closing_hosts carries the moved tab id",
            );

            let detached = g.detach_tab(1);
            assert!(detached.is_some(), "detach_tab returns the moved tab");

            assert!(
                !g.closing_hosts.contains_key(&1),
                "detach_tab must clear closing_hosts for the moved tab so \
             needs_animation_frame stops pumping for a host that's gone",
            );
            assert!(
                !g.bar.closing_tab_ids().contains(&1),
                "detach_tab must call bar.finalize_close so any pill-side \
             fade state for the moved id is also dropped",
            );
        }

        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut g = make_group();
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            g.bar_mut().start_closing(2);

            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);

            assert!(
                g.has_closing_host(2),
                "drain must latch the host-level close fade",
            );
            assert!(
                g.needs_animation_frame(),
                "in-flight close fade must pump redraws",
            );
            assert!(g.has_host(2), "host stays in `hosts` for the fade duration");
            let host_count_during = g.host_count();

            g.force_expire_closing(2);
            let moved2 = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved2);
            assert!(
                !g.has_host(2),
                "sweep drops the host after the fade expires",
            );
            assert_eq!(
                g.host_count(),
                host_count_during - 1,
                "exactly one host removed",
            );
            assert!(
                !g.has_closing_host(2),
                "sweep clears the closing_hosts entry",
            );
        }

        // Case `closing_last_tab_kicks_off_group_close`.
        // `GroupRow` relies on `is_closing()` → `close_complete()` to
        // sequence the prune, so if `closing_at` never flips the group
        // stays around as an empty shell forever.
        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let tabs: Vec<(TabDescriptor, TileContent)> = vec![(
                TabDescriptor::new(1, "solo"),
                TileContent::Stub {
                    kind: AppKind::Settings,
                    label: "a".into(),
                },
            )];
            let mut g = WindowGroup::new(10, tabs, 1, WindowWidth::Normal);

            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);

            g.bar_mut().start_closing(1);

            let moved = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved);
            assert!(
                g.has_closing_host(1),
                "precondition: host-level close latched",
            );
            assert!(
                g.is_closing(),
                "the last tab and group close together, before canonical reconciliation",
            );

            // A later event must not restart or prolong the already-owned group close.
            g.force_expire_closing(1);
            let moved2 = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(1000.0, 1000.0),
            };
            dispatch_at(&mut g, &taffy, group_node, &moved2);
            assert!(
                g.is_closing(),
                "the group close remains latched without a second fade",
            );
        }
    });
}

#[test]
fn group_close_fade_paints_pumps_frames_and_completes() {
    // One instant for the whole test: each case backdates its own
    // timestamps, so a descheduled runner cannot move them.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        {
            let mut g = make_group();
            g.set_closing_at_for_tests(
                crate::time::now()
                    .checked_sub(g.close_duration.saturating_add(Duration::from_millis(20)))
                    .expect("representable close deadline"),
            );
            assert!(
                g.close_complete(),
                "close_complete must flip true after the fade window elapses",
            );
            g.set_closing_at_for_tests(crate::time::now());
            assert!(
                !g.close_complete(),
                "a just-latched close must not report complete",
            );
        }

        // Case `group_still_paints_during_close_fade`.
        // Guards against a panic or a collapsed rect mid-fade.
        {
            use crate::theme::Theme;
            use std::collections::HashMap;
            use taffy::Style;
            use taffy::prelude::TaffyMaxContent;

            let mut g = make_group();
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let group_node = g.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy.parent_node(Style::default(), &[group_node]);
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            g.apply_layout(&taffy, group_node, Point::ZERO);
            let pre_rect = g.get_rect();

            g.start_closing();

            // The fade scales about the centre via an Affine; the logical
            // `self.rect` used for hit-tests stays put so drop-targets and
            // focus bookkeeping don't shift under the user mid-animation.
            let mut scene = Scene::new();
            let presence = crate::presence::PresenceCtx::default();
            let mut tr = TextRenderer::new();
            g.draw(&mut crate::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                presence.draw_env(),
            ));

            assert_eq!(
                g.get_rect(),
                pre_rect,
                "close fade paints over the existing rect; hit-tests must stay stable",
            );
            assert!(
                g.is_closing(),
                "close flag stays latched through the draw pass",
            );
        }

        {
            let mut g = make_group();
            assert!(
                !g.needs_animation_frame(),
                "precondition: idle group is quiet"
            );
            g.start_closing();
            assert!(
                g.needs_animation_frame(),
                "group-level close fade must pump frames until close_complete",
            );
            let later = crate::time::now() + Duration::from_secs(2);
            crate::time::testing::with_monotonic_time(later, || {
                g.step_width_spring();
                g.set_closing_at_for_tests(
                    crate::time::now()
                        .checked_sub(g.close_duration.saturating_add(Duration::from_millis(20)))
                        .expect("representable close deadline"),
                );
                assert!(
                    !g.needs_animation_frame(),
                    "after the fade window elapses, frames are no longer required",
                );
            });
        }

        {
            let mut g = make_group();
            assert!(
                !g.needs_animation_frame(),
                "precondition: idle group doesn't demand frames",
            );
            g.closing_hosts.insert(1, crate::time::now());
            assert!(
                g.needs_animation_frame(),
                "a fresh closing host must pump the run loop",
            );
            g.closing_hosts.insert(
                1,
                crate::time::now()
                    .checked_sub(group_close_fade() + Duration::from_millis(20))
                    .unwrap(),
            );
            assert!(
                !g.needs_animation_frame(),
                "after the fade window elapses the group goes idle again",
            );
        }
    });
}
