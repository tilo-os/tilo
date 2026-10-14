use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::widget::Text;
use std::rc::Rc as SharedRc;

mod capabilities;

/// A pane that fills what it is given and counts the input it receives.
struct Probe {
    id: &'static str,
    deadline: Option<Instant>,
    animating: bool,
    rect: Rect,
    events: SharedRc<Cell<usize>>,
}

impl Probe {
    fn boxed(id: &'static str) -> (Element, SharedRc<Cell<usize>>) {
        let events = SharedRc::new(Cell::new(0));
        (
            Box::new(Self {
                id,
                deadline: None,
                animating: false,
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
        taffy.fill_leaf(node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.animating = false;
    }

    fn handle_event(&mut self, _event: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
        self.events.set(self.events.get().saturating_add(1));
    }

    fn needs_animation_frame(&self) -> bool {
        self.animating
    }

    fn needs_redraw(&self) -> bool {
        self.animating
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn is_layout_dirty(&self) -> bool {
        // This fixture fills a static rectangle and has no intrinsic state.
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.id)
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

fn notes_shell() -> NavShell {
    NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Notes", Vec::new())
        .content_header("", Vec::new())
}

/// The sidebar docks only while the content beside it keeps the room it
/// needs (a readable column); an 800 × 600 window would leave a document
/// 520 px, so its sidebar becomes the drawer and the content gets the
/// window.
#[test]
fn the_sidebar_collapses_when_the_content_would_be_squeezed() {
    let theme = Theme::dark();
    let m = Metrics::resolve(&theme);
    for (width, docked) in [
        (1280_u32, true),
        (1024, true),
        (840, true),
        (800, false),
        (640, false),
    ] {
        let mut h = harness(notes_shell(), width, 600, theme.clone());
        settle(&mut h, width, 600);
        let g = geometry(&h);
        assert!(
            (g.breakpoint - (m.sidebar + m.content_min)).abs() < 1e-6,
            "{width}: the breakpoint is the sidebar plus the content's need"
        );
        assert_eq!(
            g.sidebar == SidebarPlacement::Docked,
            docked,
            "{width}: {:?}",
            g.sidebar
        );
        let content = child_rect(&h, "content").expect("the content is laid out");
        assert!(
            content.width() >= m.content_min - 1e-6,
            "{width}: the content keeps a readable column ({})",
            content.width()
        );
    }
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
fn a_toolbar_in_a_header_behind_the_drawer_settles() {
    // The headers under an open drawer are laid out hidden; a toolbar among
    // their actions takes that placement instead of holding its own height
    // against it, so the layout settles.
    let toolbar = || -> Element {
        Box::new(
            crate::widget::Toolbar::new(crate::widget::toolbar::Orientation::Horizontal)
                .push_icon(crate::icons::Icon::Search, "Search", || {})
                .push_icon(crate::icons::Icon::Refresh, "Refresh", || {}),
        )
    };
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Mailboxes", Vec::new())
        .overview_header("Inbox", vec![toolbar()])
        .content_header("", vec![toolbar()]);
    let mut h = harness(shell, 560, 860, Theme::dark());
    settle(&mut h, 560, 860);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 560, 860);
    assert!(h.root::<NavShell>().is_sidebar_open());
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
    assert!(g.scrim.width() > 0.0 && g.scrim_openness > 0.0);
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
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
    let handled = h.dispatch_to_root(&crate::synthetic_input::mouse_press());
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
        crate::synthetic_input::key_input(&Key::Named(NamedKey::Escape), ElementState::Pressed);
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
    let key = crate::synthetic_input::key_input(&Key::Character("L".into()), ElementState::Pressed);
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
fn sync_status_reads_as_quiet_text_before_the_rows_trailing_controls() {
    const OFFLINE: &str = "Working offline. Changes will sync when you're back.";
    let status = |h: &FrameHarness| {
        h.root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| child.name() == "HeaderStatus")
            .map(|child| (child.get_rect(), child.accessibility()))
    };
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    assert!(h.root_mut::<NavShell>().set_sync_status(Some(OFFLINE)));
    h.root_mut::<NavShell>()
        .set_history_control(Some(HistoryControl::Closed));
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    let (rect, node) = status(&h).unwrap();
    let node = node.unwrap();
    assert_eq!(node.name.as_deref(), Some(OFFLINE));
    assert_eq!(node.live, Some(crate::a11y::Politeness::Polite));
    assert!(rect.width() > 0.0, "painted in the row");
    assert!(
        rect.y1 <= g.header_height,
        "inside the header row, not a band"
    );
    assert!(
        rect.x1 <= g.history.x0,
        "{rect:?} ends before History {g:?}"
    );
    assert!(
        rect.x0 >= g.content_column.x0,
        "stays in the content column's row"
    );
    let slot = h.root::<NavShell>().header_status_slot();
    assert!(slot.width() > 0.0, "room is left for a save status");
    assert!(
        slot.x1 < rect.x0 && slot.y1 <= g.header_height,
        "{slot:?} sits in the row before the sync status {rect:?}"
    );

    assert!(h.root_mut::<NavShell>().set_sync_status(None));
    settle(&mut h, 1280, 800);
    assert!(
        status(&h).and_then(|(_, node)| node).is_none(),
        "nothing to announce"
    );
    let slot = h.root::<NavShell>().header_status_slot();
    assert!(
        slot.x1 <= g.history.x0 && slot.x1 > rect.x0,
        "without a sync status the save status takes its place: {slot:?}"
    );
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

#[test]
fn an_unbuilt_shell_reports_structure_changes_once() {
    let mut shell = notes_shell();
    shell.take_structure_dirty();
    for frame in 0..3 {
        assert!(
            !shell.take_structure_dirty(),
            "unbuilt frame {frame} must settle"
        );
    }
    shell.set_content(Probe::boxed("replacement").0);
    assert!(shell.take_structure_dirty());
    assert!(!shell.take_structure_dirty());
}

#[test]
fn the_modal_drawer_keeps_the_backdrops_wake_deadline() {
    let deadline = crate::time::now() + crate::time::Duration::from_millis(900);
    let (mut content, _) = Probe::boxed("content");
    content
        .as_any_mut()
        .unwrap()
        .downcast_mut::<Probe>()
        .unwrap()
        .deadline = Some(deadline);
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(content);
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 560, 800);
    assert!(geometry(&h).sidebar_modal());
    assert_eq!(h.root::<NavShell>().next_wake_deadline(), Some(deadline));
}

#[test]
fn modal_control_inventory_hides_backdrop_action_emitters() {
    let resource = tilo_actions::Resource::new("test-pane", "test", "Test").with_action(
        tilo_actions::ActionSpec::operation(
            "go",
            "Go",
            tilo_actions::OperationKind::View,
            "test.go",
            &(),
        )
        .unwrap(),
    );
    let button = Button::new("go", "Go")
        .on_action(tilo_actions::ActionBinding::unit_for(&resource, "go").unwrap());
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Box::new(button));
    let mut h = harness(shell, 560, 800, Theme::dark());
    for open in [false, true, false] {
        if open {
            h.root_mut::<NavShell>().open_sidebar();
        } else {
            h.root_mut::<NavShell>().close_sidebar();
        }
        settle(&mut h, 560, 800);
        let bindings = crate::widget::controls::control_action_bindings(h.root::<NavShell>());
        let inventory = bindings
            .iter()
            .any(|binding| matches!(binding.kind, crate::widget::ControlBindingKind::Action(_)));
        let backdrop = bindings.iter().any(|binding| matches!(&binding.kind,
            crate::widget::ControlBindingKind::Action(address) if address.resource == "test-pane" && address.action == "go"));
        assert_eq!(
            crate::widget::controls::has_action_control_bindings(h.root::<NavShell>()),
            inventory
        );
        assert_eq!(
            backdrop, !open,
            "modal drawer hides the backdrop action emitter"
        );
    }
}

#[test]
fn narrowing_keeps_the_explicit_or_last_interacted_content_pane() {
    for clicked in [false, true] {
        let mut h = harness(mail_shell(), 1600, 800, Theme::dark());
        settle(&mut h, 1600, 800);
        assert!(geometry(&h).overview_column.width() > 0.0);
        if clicked {
            let point = geometry(&h).overview_column.center();
            h.dispatch_to_root(&crate::synthetic_input::cursor_moved(point.x, point.y));
            h.dispatch_to_root(&crate::synthetic_input::mouse_press());
        } else {
            h.root_mut::<NavShell>().show(NavPane::Overview);
        }
        settle(&mut h, 560, 800);
        assert_eq!(
            h.root::<NavShell>().front_pane(),
            NavPane::Overview,
            "clicked: {clicked}"
        );
        assert!(geometry(&h).overview_column.width() > 0.0);
    }
}

#[test]
fn scrolling_reuses_measured_panes_without_reloading_fonts() {
    let content = crate::widget::Scrollable::new_vertical().with_child(Box::new(Text::new(
        &"file row with a measured label\n".repeat(100),
    )));
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Box::new(content));
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().mark_drawn();
    h.relayout_at((1280, 800)).expect("draw state republished");
    let text_rect = |h: &FrameHarness| {
        h.root::<NavShell>()
            .children()
            .into_iter()
            .find(|child| {
                child.concrete_type_name() == std::any::type_name::<crate::widget::Scrollable>()
            })
            .expect("retained scroll pane")
            .children()[0]
            .get_rect()
    };
    let before = text_rect(&h);
    let center = h.root::<NavShell>().geometry().content_column.center();
    h.dispatch_to_root(&crate::synthetic_input::cursor_moved(center.x, center.y));
    crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
    for _ in 0..4 {
        h.dispatch_to_root(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                0.0, -20.0,
            )),
            phase: winit::event::TouchPhase::Moved,
        });
        assert!(h.root::<NavShell>().is_layout_dirty());
        assert!(
            !h.root::<NavShell>().layout_needs_measure(),
            "scroll preserves intrinsic sizes: shell_pending={} dirty_parts={:?} font_constructions={}",
            h.root::<NavShell>().invalidation.is_pending(),
            h.root::<NavShell>()
                .host_parts
                .iter()
                .filter_map(|part| {
                    let widget = h.root::<NavShell>().part(*part)?;
                    widget.layout_needs_measure().then(|| {
                        (
                            *part,
                            widget.name(),
                            widget.get_rect(),
                            widget.is_layout_dirty(),
                        )
                    })
                })
                .collect::<Vec<_>>(),
            crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(Cell::get),
        );
        h.relayout_at((1280, 800)).expect("scroll repositioned");
    }
    let after = text_rect(&h);
    assert_eq!(after.x0, before.x0);
    assert_eq!(after.y0, before.y0 - 80.0);
    assert_eq!(
        crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(Cell::get),
        0,
        "translation-only layout must not reload system fonts"
    );
}

#[test]
fn retained_panes_remeasure_theme_resize_replacement_and_modal_backdrop_edits() {
    use automerge::transaction::Transactable;
    fn make(theme: Theme, viewport: (u32, u32), label: &str) -> FrameHarness {
        let mut state = State::new();
        state.doc.put(automerge::ROOT, "label", label).unwrap();
        let content = crate::widget::Scrollable::new_vertical()
            .with_child(Box::new(Text::from_document_string("label")));
        FrameHarness::for_root(
            state,
            NavShell::new()
                .sidebar(Probe::boxed("sidebar").0)
                .content(Box::new(content)),
            viewport,
            theme,
        )
    }
    fn text_rect(h: &mut FrameHarness) -> Rect {
        h.root_mut::<NavShell>()
            .content_mut::<crate::widget::Scrollable>()
            .unwrap()
            .children()[0]
            .get_rect()
    }
    let mut h = make(Theme::dark(), (1280, 800), "first line");
    settle(&mut h, 1280, 800);
    let original = text_rect(&mut h);
    let scaled = Theme::light().with_text_scale(1.5);
    crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(|count| count.set(0));
    h.relayout_themed((1400, 900), scaled.clone())
        .expect("theme and resize measured");
    assert!(text_rect(&mut h).height() > original.height());
    assert_eq!(
        crate::text_render::FONT_SYSTEM_CONSTRUCTIONS.with(Cell::get),
        0,
        "panes reuse the host renderer rather than constructing font systems"
    );
    let mut fresh = make(scaled.clone(), (1400, 900), "first line");
    settle(&mut fresh, 1400, 900);
    assert_eq!(text_rect(&mut h), text_rect(&mut fresh));

    h.relayout_at((560, 800)).expect("narrow");
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 560, 800);
    assert!(geometry(&h).sidebar_modal());
    let before = text_rect(&mut h);
    h.state_mut()
        .doc
        .put(
            automerge::ROOT,
            "label",
            "first line\nsecond line\nthird line",
        )
        .unwrap();
    h.relayout_at((560, 800))
        .expect("modal backdrop edit measured");
    let changed = text_rect(&mut h);
    assert!(
        changed.height() > before.height(),
        "the live backdrop gets changed text geometry"
    );
    h.recompose().expect("authoritative full measurement");
    assert_eq!(text_rect(&mut h), changed);

    // A replacement already clean at the same scale still invalidates the
    // retained composition, even though its own intrinsic sizes are known.
    let mut replacement = make(
        scaled,
        (560, 800),
        "replacement\nsecond line\nthird line\nfourth line",
    );
    settle(&mut replacement, 560, 800);
    replacement.root_mut::<NavShell>().set_content(Box::new(
        crate::widget::Scrollable::new_vertical().with_child(Box::new(Text::new(
            "replacement\nsecond line\nthird line\nfourth line",
        ))),
    ));
    settle(&mut replacement, 560, 800);
    replacement.root_mut::<NavShell>().mark_drawn();
    let content = std::mem::replace(
        &mut replacement.root_mut::<NavShell>().content,
        Probe::boxed("placeholder").0,
    );
    assert!(
        !content.layout_needs_measure(),
        "fixture: replacement is already measured and clean"
    );
    h.root_mut::<NavShell>().set_content(content);
    h.relayout_at((560, 800))
        .expect("clean replacement measured");
    assert!(text_rect(&mut h).height() > changed.height());
}

#[test]
fn the_navigation_list_keeps_its_typed_identity_and_selected_activation_opens_content() {
    let changes = SharedRc::new(Cell::new(0));
    let seen = SharedRc::clone(&changes);
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![crate::widget::NavItem::new("inbox", "Inbox")],
    )])
    .selected("inbox")
    .on_select(move |_| seen.set(seen.get() + 1));
    assert!(
        Widget::as_any(&list)
            .unwrap()
            .is::<crate::widget::NavList>()
    );
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    assert_eq!(
        h.root_mut::<NavShell>()
            .sidebar_mut::<crate::widget::NavList>()
            .expect("typed NavList access through the shared decorator")
            .selected_id()
            .as_deref(),
        Some("inbox")
    );
    // Mutable typed access retires the host's raw layout maps, even when
    // used only to inspect a value. Publish a fresh admitted frame for AT.
    h.relayout_at((560, 800))
        .expect("typed inspection republished");
    let inbox = h
        .a11y_node_named("Inbox")
        .expect("published navigation row");
    h.perform_a11y_action(inbox, &crate::a11y::Action::Activate)
        .expect("activating the already-selected destination");
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    assert_eq!(
        changes.get(),
        0,
        "activation does not invent a selection change"
    );
}

#[test]
fn push_navigation_keeps_back_operable_before_the_incoming_screen_is_visible() {
    let list = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![crate::widget::NavItem::new("inbox", "Inbox")],
    )])
    .selected("inbox");
    let shell = NavShell::new()
        .mode(NavMode::Push)
        .sidebar(Box::new(list))
        .content(Probe::boxed("content").0);
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    let outgoing = h.a11y_node_named("Inbox").expect("initial navigation row");
    {
        let shell = h.root_mut::<NavShell>();
        shell.show(NavPane::Content);
        // The first animation sample is still wholly on the outgoing page.
        shell.inputs.borrow_mut().push_pos = Some(0.0);
    }
    h.relayout_at((560, 800)).expect("incoming page frame");
    assert!(geometry(&h).side_by_side, "no overview splits the content");
    assert!(
        h.a11y_node_named("Inbox").is_none(),
        "outgoing navigation rows are inert even while their pixels remain visible"
    );
    assert!(
        h.perform_a11y_action(outgoing, &crate::a11y::Action::Activate)
            .is_err(),
        "a stale AT target cannot activate the outgoing screen"
    );
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
    let back = h
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some("Back")
                && node.role() == crate::accesskit::Role::Button
                && node.supports_action(crate::accesskit::Action::Click)
        })
        .expect("Back remains exported during the transition");
    assert!(geometry(&h).back.x0 >= 0.0 && geometry(&h).back.x1 <= 560.0);
    h.perform_a11y_action(back.0, &crate::a11y::Action::Activate)
        .expect("Back cancels the in-flight navigation through AT");
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
}

#[test]
fn a_hidden_inspector_animation_waits_until_the_drawer_is_painted() {
    let (mut inspector, _) = Probe::boxed("inspector");
    inspector
        .as_any_mut()
        .unwrap()
        .downcast_mut::<Probe>()
        .unwrap()
        .animating = true;
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(inspector);
    let mut h = harness(shell, 800, 534, Theme::dark());
    assert!(h.settle_for_capture(crate::time::Duration::from_secs(1)));
    let shell = h.root::<NavShell>();
    assert!(!shell.is_inspector_open());
    assert!(!shell.needs_animation_frame());
    assert!(!shell.needs_redraw());
    assert!(
        shell.inspector.as_ref().unwrap().needs_animation_frame(),
        "the hidden animation is retained"
    );
    h.root_mut::<NavShell>().open_inspector();
    assert!(h.settle_for_capture(crate::time::Duration::from_secs(1)));
    let shell = h.root::<NavShell>();
    assert!(shell.is_inspector_open());
    assert!(
        !shell.inspector.as_ref().unwrap().needs_animation_frame(),
        "opening the drawer paints the retained animation"
    );
}

#[test]
fn a_toast_floats_over_the_bottom_corner_and_a_click_dismisses_it() {
    let mut h = harness(mail_shell(), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().mark_drawn();
    h.root_mut::<NavShell>().toast("Saved", ToastKind::Success);
    assert!(h.root::<NavShell>().needs_redraw(), "a new notice repaints");
    settle(&mut h, 1280, 800);
    let card = h.root::<NavShell>().toasts.get_rect();
    assert!(
        card.width() > 0.0 && card.x1 < 1280.0 && card.y1 < 800.0 && card.y0 > 400.0,
        "the notice sits inside the window's bottom trailing corner: {card:?}"
    );
    assert_eq!(h.root::<NavShell>().toast_text(), Some("Saved"));
    let announced = h
        .root::<NavShell>()
        .children()
        .last()
        .and_then(|child| child.accessibility())
        .and_then(|node| node.value);
    assert_eq!(
        announced.as_deref(),
        Some("Saved"),
        "a live region announces it"
    );
    h.click_at(card.center().x, card.center().y);
    assert_eq!(h.root::<NavShell>().toast_text(), None);
}

/// One frame of the shell as a host presents it at `scale`: its widgets and
/// overlays painted magnified, and the damage it reports, in the host's
/// scene pixels as a host repaints it (rounded out by a pixel).
fn present_shell(
    shell: &mut NavShell,
    text: &mut TextRenderer,
    theme: &Theme,
    scale: f64,
) -> (Scene, Vec<Rect>) {
    let mut scene = Scene::new();
    let presence = PresenceCtx::empty();
    let transform = Affine::scale(scale);
    shell.draw(&mut scene, transform, text, theme, &presence);
    shell.draw_overlay(&mut scene, transform, text, theme, &presence);
    let mut damage = Vec::new();
    shell.collect_damage(&mut damage);
    shell.mark_drawn();
    let damage = damage
        .into_iter()
        .map(|rect| transform.transform_rect_bbox(rect).inflate(1.0, 1.0))
        .collect();
    (scene, damage)
}

/// Repaint `after` over `before` only where `damage` says, and require the
/// pixels of a full render of `after`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the test's window is a few megapixels at scale 2"
)]
fn assert_sparse_equals_full(
    (before, after): (&Scene, &Scene),
    damage: &[Rect],
    theme: &Theme,
    (size, label): ((f64, f64), &str),
) {
    let (width, height) = (size.0.round() as u32, size.1.round() as u32);
    let background = theme.tokens.colors.surfaces.content;
    let mut retained = crate::offscreen::OffscreenRenderer::new(width, height);
    let mut full = crate::offscreen::OffscreenRenderer::new(width, height);
    let _ = retained.render(before, background);
    let rasterized = retained.render_damage_regions_gpu_wait(after, background, damage);
    assert!(
        rasterized < u64::from(width).saturating_mul(u64::from(height)),
        "{label}: the damage must exercise a sparse render: {damage:?}"
    );
    let expected = full.render(after, background);
    let differing = retained
        .readback_current()
        .chunks_exact(4)
        .zip(expected.chunks_exact(4))
        .filter(|(actual, expected)| {
            actual
                .iter()
                .zip(*expected)
                .any(|(a, e)| a.abs_diff(*e) > 1)
        })
        .count();
    assert_eq!(
        differing, 0,
        "{label}: the sparse repaint must equal a full render; damage {damage:?}"
    );
}

/// A notice that comes and goes repaints the card and its shadow where it
/// was and is, nothing else: the card's real painted bounds, not the
/// window, and never short of its shadow. Every theme, at 100 % and 200 %.
#[test]
fn a_toast_damages_its_card_and_shadow_where_it_was_and_is() {
    let (w, h_px) = (1280, 800);
    for scale in [1.0, 2.0] {
        for (name, theme) in [
            ("dark", Theme::dark()),
            ("light", Theme::light()),
            ("high contrast", Theme::high_contrast()),
        ] {
            let label = format!("{name} at {scale}x");
            let mut h = harness(mail_shell(), w, h_px, theme.clone());
            settle(&mut h, w, h_px);
            let mut text = crate::font_catalog::fixture_renderer();
            let shell = h.root_mut::<NavShell>();
            let size = (f64::from(w) * scale, f64::from(h_px) * scale);
            // The first frame at this scale and renderer; then one with no
            // change, which must damage nothing, or this observes nothing.
            let _ = present_shell(shell, &mut text, &theme, scale);
            let (before, quiet) = present_shell(shell, &mut text, &theme, scale);
            assert!(
                quiet.is_empty(),
                "{label}: a settled shell damages nothing: {quiet:?}"
            );

            shell.toast("Saved", ToastKind::Success);
            let (shown, damage) = present_shell(shell, &mut text, &theme, scale);
            let card = shell.toasts.get_rect();
            let painted = crate::widget::floating_surface::painted_bounds(card, &theme);
            assert_eq!(
                damage,
                [Affine::scale(scale)
                    .transform_rect_bbox(painted)
                    .inflate(1.0, 1.0)],
                "{label}: a new notice damages its card and shadow ({card:?})"
            );
            assert_sparse_equals_full(
                (&before, &shown),
                &damage,
                &theme,
                (size, &format!("{label}: showing")),
            );

            shell.dismiss_toast();
            let (gone, damage) = present_shell(shell, &mut text, &theme, scale);
            assert_eq!(
                damage,
                [Affine::scale(scale)
                    .transform_rect_bbox(painted)
                    .inflate(1.0, 1.0)],
                "{label}: a dismissed notice damages where it was"
            );
            assert_sparse_equals_full(
                (&shown, &gone),
                &damage,
                &theme,
                (size, &format!("{label}: dismissing")),
            );
        }
    }
}
