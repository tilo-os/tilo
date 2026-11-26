use super::*;

/// Run `f` on the toolkit's clock at `ms` after `start`: a frame of a
/// test's own timeline, however long the machine takes.
fn at<T>(start: Instant, ms: u64, f: impl FnOnce() -> T) -> T {
    crate::time::testing::with_monotonic_time(start + Duration::from_millis(ms), f)
}
use crate::testing::Dispatch;
use crate::widget::{AppKind, TabDescriptor, TileContent};

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
    use tilo_ui_base::offscreen::OffscreenRenderer;
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
        group.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
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
    use tilo_ui_base::offscreen::OffscreenRenderer;
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
        group.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
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
    tilo_ui_base::provides!(
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn tilo_ui_base::widget::controls::ActionControls,
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

impl tilo_ui_base::widget::controls::ActionControls for RetainedTraversalProbe {
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

    tilo_ui_base::widget::handle_collab_event(
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
    assert!(tilo_ui_base::widget::controls::has_pending_control_invocation(
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
    from: tilo_ui_base::arriving::Source,
}

impl Widget for Starting {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &tilo_ui_base::theme::Theme,
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
    fn arrives_from(&self) -> tilo_ui_base::arriving::Source {
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

fn group_opening(title: &str, presented: bool, from: tilo_ui_base::arriving::Source) -> WindowGroup {
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
    g.draw(&mut tilo_ui_base::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
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
    let budget = tilo_ui_base::arriving::frame_budget();
    {
        let mut g = crate::time::testing::with_monotonic_time(t0, || {
            group_opening("Files", false, tilo_ui_base::arriving::Source::Local)
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
            group_opening("Notes", false, tilo_ui_base::arriving::Source::Remote)
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
                    from: tilo_ui_base::arriving::Source::Local,
                })),
            ),
            (
                TabDescriptor::new(2, "Notes"),
                TileContent::App(Box::new(Starting {
                    presented: false,
                    from: tilo_ui_base::arriving::Source::Local,
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
        _t: &tilo_ui_base::theme::Theme,
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
        use tilo_ui_base::theme::Theme;
        use std::collections::HashMap;
        use taffy::Style;
        use taffy::prelude::TaffyMaxContent;
        use winit::event::Modifiers;
        use winit::keyboard::{Key, ModifiersState, SmolStr};

        fn key_press(logical_key: Key) -> WindowEvent {
            tilo_ui_base::synthetic_input::keyboard_input(tilo_ui_base::synthetic_input::key_event(
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
            use tilo_ui_base::theme::Theme;
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
            use tilo_ui_base::theme::Theme;
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
