use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use std::rc::Rc as SharedRc;

/// A pane that fills what it is given and counts the input it receives.
struct Probe {
    id: &'static str,
    rect: Rect,
    events: SharedRc<Cell<usize>>,
}

impl Probe {
    fn boxed(id: &'static str) -> (Element, SharedRc<Cell<usize>>) {
        let events = SharedRc::new(Cell::new(0));
        (
            Box::new(Self {
                id,
                rect: Rect::ZERO,
                events: SharedRc::clone(&events),
            }),
            events,
        )
    }
}

impl Widget for Probe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        self.events.set(self.events.get().saturating_add(1));
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn mail_shell() -> NavShell {
    NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", Vec::new())
        .content_header("Design review", Vec::new())
}

fn harness(shell: NavShell, w: u32, h: u32, theme: Theme) -> FrameHarness {
    FrameHarness::for_root(State::new(), shell, (w, h), theme)
}

fn settle(h: &mut FrameHarness, w: u32, height: u32) {
    h.root_mut::<NavShell>().snap();
    h.relayout_at((w, height)).expect("layout settles");
    h.relayout_at((w, height)).expect("layout settles");
}

fn geometry(h: &FrameHarness) -> NavGeometry {
    h.root::<NavShell>().geometry().clone()
}

fn child_rect(h: &FrameHarness, id: &str) -> Option<Rect> {
    h.root::<NavShell>()
        .children()
        .into_iter()
        .find(|child| child.test_id() == Some(id))
        .map(Widget::get_rect)
}

#[test]
fn the_shell_follows_the_window_and_the_text_scale() {
    for (scale, theme) in [
        (1.0, Theme::dark()),
        (2.0, Theme::dark().with_text_scale(2.0)),
    ] {
        let m = Metrics::resolve(&theme);
        for width in [1280_u32, 800, 560] {
            let mut h = harness(mail_shell(), width, 800, theme.clone());
            settle(&mut h, width, 800);
            let g = geometry(&h);
            let w = f64::from(width);
            let breakpoint = m.sidebar + m.overview + m.detail_min;
            assert!((g.breakpoint - breakpoint).abs() < 1e-6, "{scale}x {width}");
            let expect_docked = w >= breakpoint;
            assert_eq!(
                g.sidebar,
                if expect_docked {
                    SidebarPlacement::Docked
                } else {
                    SidebarPlacement::Drawer
                },
                "{scale}x {width}: the sidebar docks only beside unsqueezed content"
            );
            let room = if expect_docked { w - m.sidebar } else { w };
            assert_eq!(
                g.side_by_side,
                room >= m.overview + m.detail_min,
                "{scale}x {width}: overview and content stand side by side only while both fit"
            );
            assert!(g.header_height >= f64::from(theme.tokens.metrics.header_height));
            if expect_docked {
                assert!((g.sidebar_column.width() - m.sidebar).abs() < 0.5);
                assert_eq!(g.sidebar_column.x0, 0.0);
            } else {
                assert_eq!(g.sidebar_column, Rect::ZERO, "a closed drawer is not shown");
            }
            // Every shown body sits under the one header row.
            for id in ["sidebar", "overview", "content"] {
                if let Some(rect) = child_rect(&h, id) {
                    assert!(
                        (rect.y0 - g.header_height).abs() < 0.5,
                        "{scale}x {width}: {id} starts under the header row ({})",
                        rect.y0
                    );
                }
            }
            // The toggle is always top-leading, one inset in.
            if !g.stack.is_empty() && h.root::<NavShell>().front_pane() == NavPane::Content {
                assert!(g.back.width() > 0.0);
            } else {
                assert!((g.toggle.x0 - m.inset).abs() < 0.5, "{scale}x {width}");
                assert!(g.toggle.y0 > 0.0 && g.toggle.y1 < g.header_height);
            }
        }
    }
}

#[test]
fn an_800_window_never_keeps_a_docked_sidebar_beside_squeezed_columns() {
    let mut h = harness(mail_shell(), 800, 600, Theme::dark());
    settle(&mut h, 800, 600);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Drawer);
    assert!(g.side_by_side);
    let content = child_rect(&h, "content").expect("content laid out");
    assert!(
        content.width() >= f64::from(Theme::dark().tokens.metrics.detail_min_width) - 0.5,
        "the detail keeps its minimum ({})",
        content.width()
    );
}

#[test]
fn the_sidebar_toggle_moves_and_the_choice_is_remembered_per_size_class() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let before = geometry(&h);
    assert!(h.root::<NavShell>().is_sidebar_open());
    assert!(before.toggle.width() > 0.0);

    h.root_mut::<NavShell>().toggle_sidebar();
    settle(&mut h, 1280, 800);
    let hidden = geometry(&h);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(hidden.sidebar_column, Rect::ZERO);
    assert_eq!(hidden.toggle, before.toggle, "the toggle stays top-leading");
    assert_eq!(hidden.toggle_column, Some(Column::Overview));
    assert!(
        child_rect(&h, "sidebar").is_none(),
        "a hidden sidebar takes no input"
    );
    assert_eq!(child_rect(&h, "overview").map(|r| r.x0), Some(0.0));

    // Narrow: a drawer, closed. Open it, then widen again.
    settle(&mut h, 800, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Drawer);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    h.root_mut::<NavShell>().toggle_sidebar();
    settle(&mut h, 800, 800);
    assert!(h.root::<NavShell>().is_sidebar_open());
    settle(&mut h, 1280, 800);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "the wide window remembers the sidebar was hidden"
    );
    settle(&mut h, 800, 800);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "a drawer is transient: it does not reopen by itself"
    );
}

#[test]
fn an_open_drawer_is_modal_and_floats_over_the_content() {
    let mut h = harness(mail_shell(), 800, 600, Theme::dark());
    settle(&mut h, 800, 600);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 800, 600);
    let g = geometry(&h);
    let gap = f64::from(Theme::dark().tokens.spacing.s2);
    assert!(
        (g.sidebar_column.x0 - gap).abs() < 0.5,
        "the drawer floats s2 in"
    );
    assert!(g.scrim.width() > 0.0 && g.scrim_strength > 0.0);
    let ids: Vec<_> = h
        .root::<NavShell>()
        .children()
        .iter()
        .filter_map(|child| child.test_id().map(str::to_owned))
        .collect();
    assert_eq!(
        ids,
        ["sidebar", "nav-shell-sidebar"],
        "only the drawer and its toggle take input and are exposed"
    );

    // A press on the scrim closes it.
    let point = Point::new(g.sidebar_column.x1 + 40.0, 300.0);
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
    let handled = h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    assert!(handled);
    assert!(!h.root::<NavShell>().is_sidebar_open());
}

#[test]
fn narrow_windows_push_the_content_over_the_overview() {
    let mut h = harness(mail_shell(), 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    let g = geometry(&h);
    assert_eq!(g.stack, [NavPane::Overview, NavPane::Content]);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Overview,
        "a narrow window opens on the overview"
    );
    assert_eq!(g.back, Rect::ZERO);
    assert!(
        g.toggle.width() > 0.0,
        "the drawer is summoned from the overview"
    );

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 560, 800);
    let g = geometry(&h);
    assert!(g.back.width() > 0.0, "a pushed screen has a Back button");
    assert_eq!(g.toggle, Rect::ZERO, "one affordance holds the corner");
    assert_eq!(g.content_column.x0, 0.0);
    assert_eq!(g.overview_column, Rect::ZERO, "the overview has slid away");

    // Escape goes back.
    let esc =
        crate::synthetic_input::key_event(&Key::Named(NamedKey::Escape), ElementState::Pressed);
    assert!(h.dispatch_to_root(&esc));
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
}

#[test]
fn push_mode_makes_the_sidebar_the_first_screen() {
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Screen);
    assert_eq!(g.stack, [NavPane::Sidebar, NavPane::Content]);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert_eq!(g.toggle, Rect::ZERO, "a sidebar screen has no toggle");
    assert_eq!(child_rect(&h, "sidebar").map(|r| r.width()), Some(560.0));

    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 560, 800);
    assert!(geometry(&h).back.width() > 0.0);
    assert!(h.root_mut::<NavShell>().back());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);

    // Wide, it docks like any sidebar.
    settle(&mut h, 1280, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
}

#[test]
fn choosing_a_destination_in_a_narrow_window_moves_on() {
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![
            crate::widget::NavItem::new("inbox", "Inbox"),
            crate::widget::NavItem::new("sent", "Sent"),
        ],
    )]);
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    let g = geometry(&h);
    let row = crate::chrome::nav_row_height(&Theme::dark());
    let point = Point::new(60.0, g.body(Column::Sidebar).y0 + row * 1.5);
    let device = winit::event::DeviceId::dummy();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        h.dispatch_to_root(&WindowEvent::MouseInput {
            device_id: device,
            state,
            button: MouseButton::Left,
        });
    }
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "choosing Sent pushes the content"
    );
}

#[test]
fn the_system_shortcut_toggles_the_sidebar() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>()
        .set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
    settle(&mut h, 1280, 800);
    let key = crate::synthetic_input::key_event(&Key::Character("L".into()), ElementState::Pressed);
    let modifiers = winit::event::Modifiers::from(
        winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT,
    );
    assert!(h.dispatch_to_root_with(&key, &modifiers));
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert!(h.dispatch_to_root_with(&key, &modifiers));
    assert!(h.root::<NavShell>().is_sidebar_open());
}

#[test]
fn the_toggle_names_what_it_does_for_the_palette_and_agents() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let name = |h: &FrameHarness| {
        h.root::<NavShell>()
            .toggle
            .accessibility()
            .and_then(|node| node.name)
    };
    assert_eq!(name(&h).as_deref(), Some("Hide sidebar"));
    h.root_mut::<NavShell>().toggle_sidebar();
    settle(&mut h, 1280, 800);
    assert_eq!(name(&h).as_deref(), Some("Show sidebar"));
    let doc = automerge::AutoCommit::new();
    let resources = crate::resources::resources(&h.root::<NavShell>().toggle, &doc);
    assert!(
        resources
            .iter()
            .any(|resource| resource.label == "Show sidebar"),
        "the toggle publishes its action"
    );
}

#[test]
fn dragging_the_trailing_edge_resizes_and_below_its_minimum_hides() {
    let mut h = harness(mail_shell(), 1400, 800, Theme::dark());
    settle(&mut h, 1400, 800);
    let g = geometry(&h);
    let edge = g.resize_handle.center();
    let device = winit::event::DeviceId::dummy();
    let move_to = |h: &mut FrameHarness, x: f64| {
        h.dispatch_to_root(&WindowEvent::CursorMoved {
            device_id: device,
            position: winit::dpi::PhysicalPosition::new(x, edge.y),
        })
    };
    move_to(&mut h, edge.x);
    assert!(h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }));
    assert!(move_to(&mut h, edge.x + 60.0));
    settle(&mut h, 1400, 800);
    let wider = geometry(&h).sidebar_column.width();
    assert!(
        (wider - (g.sidebar_column.width() + 60.0)).abs() < 1.0,
        "{wider}"
    );

    move_to(&mut h, 20.0);
    h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    settle(&mut h, 1400, 800);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "dragged below its minimum, the sidebar hides"
    );
}

#[test]
fn the_inspector_docks_beside_the_content_and_floats_when_narrow() {
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(Probe::boxed("inspector").0);
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert_eq!(g.inspector, InspectorPlacement::Docked);
    assert_eq!(g.inspector_column.x1, 1280.0);
    assert!(g.inspector_toggle.width() > 0.0);
    assert!(
        child_rect(&h, "content").is_some_and(|r| r.x1 <= g.inspector_column.x0 + 0.5),
        "the content ends where the inspector starts"
    );

    settle(&mut h, 600, 800);
    let g = geometry(&h);
    assert_eq!(g.inspector, InspectorPlacement::Drawer);
    assert_eq!(
        g.inspector_column,
        Rect::ZERO,
        "closed by default when it floats"
    );
    h.root_mut::<NavShell>().toggle_inspector();
    settle(&mut h, 600, 800);
    assert!(geometry(&h).inspector_column.width() > 0.0);
}

#[test]
fn a_column_without_a_shell_has_no_header_row() {
    let shell = NavShell::new().content(Probe::boxed("content").0);
    let mut h = harness(shell, 1024, 700, Theme::dark());
    settle(&mut h, 1024, 700);
    let g = geometry(&h);
    assert_eq!(g.header_height, 0.0);
    assert_eq!(
        child_rect(&h, "content"),
        Some(Rect::new(0.0, 0.0, 1024.0, 700.0))
    );
}

#[test]
fn history_is_one_glyph_in_the_window_top_trailing_corner() {
    // Case `only_where_the_host_gives_the_shell_history`.
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        assert_eq!(geometry(&h).history, Rect::ZERO);
        assert!(child_rect(&h, "nav-shell-history").is_none());
    }

    // Case `closed_it_stands_at_the_trailing_edge_of_the_row`.
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(g.history.width() > 0.0);
        assert!(g.history.x1 > 1280.0 - 24.0 && g.history.x1 <= 1280.0);
        assert!(g.history.y1 <= g.header_height);
        let button = h
            .root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| child.test_id() == Some("nav-shell-history"))
            .and_then(Widget::accessibility)
            .unwrap();
        assert_eq!(button.state.checked, Some(false));
        assert!(
            g.actions_band(Column::Content).x1 <= g.history.x0,
            "the app's actions end before it"
        );
    }

    // Case `open_it_reads_as_on`.
    {
        let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Open));
        settle(&mut h, 1280, 800);
        let button = h
            .root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| child.test_id() == Some("nav-shell-history"))
            .and_then(Widget::accessibility)
            .unwrap();
        assert_eq!(button.state.checked, Some(true));
    }

    // Case `the_inspector_toggle_stands_just_before_it`.
    {
        let shell = NavShell::new()
            .content(Probe::boxed("content").0)
            .inspector(Probe::boxed("inspector").0);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 1280, 800);
        let g = geometry(&h);
        assert!(g.inspector_toggle.width() > 0.0);
        assert!(g.inspector_toggle.x1 < g.history.x0, "{g:?}");
        assert!((g.inspector_toggle.y0 - g.history.y0).abs() < 0.5);
    }

    // Case `a_shell_without_headers_grows_a_row_for_it`.
    {
        let shell = NavShell::new().content(Probe::boxed("content").0);
        let mut h = harness(shell, 1024, 700, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 1024, 700);
        let g = geometry(&h);
        assert!(g.header_height > 0.0);
        assert!(g.history.x1 > 1024.0 - 24.0);
    }

    // Case `a_narrow_push_stack_keeps_it_on_the_front_screen`.
    {
        let mut h = harness(mail_shell(), 560, 800, Theme::dark());
        h.root_mut::<NavShell>()
            .set_history_control(Some(HistoryControl::Closed));
        settle(&mut h, 560, 800);
        let g = geometry(&h);
        assert!(g.history.width() > 0.0 && g.history.x1 <= 560.0, "{g:?}");
    }
}

#[test]
fn slides_take_the_motion_duration_and_reduced_motion_jumps() {
    let theme = Theme::dark();
    let mut h = harness(mail_shell(), 1280, 800, theme.clone());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().toggle_sidebar();
    h.relayout_at((1280, 800)).expect("layout");
    assert!(
        h.root::<NavShell>().needs_animation_frame(),
        "hiding the sidebar animates"
    );

    let mut calm = Theme::dark();
    calm.reduce_motion = true;
    let mut h = harness(mail_shell(), 1280, 800, calm);
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().toggle_sidebar();
    h.relayout_at((1280, 800)).expect("layout");
    h.relayout_at((1280, 800)).expect("layout");
    assert_eq!(
        geometry(&h).sidebar_column,
        Rect::ZERO,
        "reduced motion does not slide"
    );
}

#[test]
fn pressing_the_toggle_hides_and_shows_the_sidebar() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let toggle = geometry(&h).toggle.center();
    let press = |h: &mut FrameHarness| {
        h.click_at(toggle.x, toggle.y);
        h.relayout_at((1280, 800)).expect("layout");
    };
    press(&mut h);
    settle(&mut h, 1280, 800);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(
        geometry(&h).toggle.center(),
        toggle,
        "the toggle did not move"
    );
    press(&mut h);
    settle(&mut h, 1280, 800);
    assert!(h.root::<NavShell>().is_sidebar_open());
}
