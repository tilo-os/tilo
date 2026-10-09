//! The header row as a capability, Back, drawers and typed access.

use super::*;
use crate::widget::{FilterCount, Filterable};

fn labelled(id: &'static str, label: &str, icon: Icon) -> Element {
    Box::new(Button::new(id, label).with_icon(icon).on_view(|| {}))
}

fn icon(id: &'static str, icon: Icon, name: &str) -> Element {
    Box::new(Button::icon_only(id, icon, name).on_view(|| {}))
}

fn part_rect(h: &FrameHarness, part: Part) -> Rect {
    h.root::<NavShell>()
        .part(part)
        .map_or(Rect::ZERO, Widget::get_rect)
}

fn exposed_ids(h: &FrameHarness) -> Vec<String> {
    h.root::<NavShell>()
        .children()
        .iter()
        .filter_map(|child| child.test_id().map(str::to_owned))
        .collect()
}

fn titled_content(actions: Vec<Element>) -> NavShell {
    NavShell::new()
        .content(Probe::boxed("content").0)
        .content_header("Quarterly planning: first draft", actions)
}

/// Swapping a column while the host holds the tree waits for the next
/// build: the event routed in between still reaches the widget the tree
/// was built with, never a dropped one.
#[test]
fn a_swap_waits_for_the_next_build() {
    let (old, old_events) = Probe::boxed("content");
    let shell = NavShell::new()
        .content(old)
        .content_header("Draft", Vec::new());
    let mut h = harness(shell, 1024, 700, Theme::dark());
    settle(&mut h, 1024, 700);
    let (new, new_events) = Probe::boxed("content-next");
    h.root_mut::<NavShell>().set_content(new);
    h.root_mut::<NavShell>()
        .set_content_header("Final", vec![icon("share", Icon::Send, "Share")]);
    assert_eq!(
        exposed_ids(&h),
        ["content"],
        "the old content stays until the host builds again"
    );
    let device = winit::event::DeviceId::dummy();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(400.0, 400.0),
    });
    assert!(old_events.get() > 0, "the event reached the built content");
    assert_eq!(new_events.get(), 0);
    assert!(
        h.root_mut::<NavShell>()
            .content_mut::<Probe>()
            .is_some_and(|p| p.id == "content-next"),
        "typed access sees the content waiting to go in"
    );
    settle(&mut h, 1024, 700);
    assert_eq!(exposed_ids(&h), ["content-next", "share"]);
    assert_eq!(
        h.root::<NavShell>()
            .part(Part::Title(Column::Content))
            .and_then(Widget::accessibility)
            .and_then(|node| node.name)
            .as_deref(),
        Some("Final")
    );
}

#[test]
fn every_column_title_changes_in_place_and_a_bound_title_follows_its_state() {
    let title = LocalState::new("Inbox".to_owned());
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .inspector(Probe::boxed("inspector").0)
        .content_header_bound(&title, Vec::new())
        .overview_count(3);
    let mut h = harness(shell, 1400, 800, Theme::dark());
    settle(&mut h, 1400, 800);
    let heading = |h: &FrameHarness, column: Column| {
        h.root::<NavShell>()
            .part(Part::Title(column))
            .and_then(Widget::accessibility)
            .filter(|_| {
                h.root::<NavShell>()
                    .host_parts
                    .contains(&Part::Title(column))
            })
            .and_then(|node| node.name)
    };
    assert_eq!(heading(&h, Column::Content).as_deref(), Some("Inbox"));
    h.root_mut::<NavShell>().set_sidebar_title("Mail");
    h.root_mut::<NavShell>().set_inspector_title("Details");
    h.root_mut::<NavShell>().set_content_count(Some(12));
    title.set("Archive".to_owned());
    let doc = automerge::AutoCommit::new();
    h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
    settle(&mut h, 1400, 800);
    assert_eq!(heading(&h, Column::Sidebar).as_deref(), Some("Mail"));
    assert_eq!(heading(&h, Column::Inspector).as_deref(), Some("Details"));
    assert_eq!(heading(&h, Column::Content).as_deref(), Some("Archive"));
    let count = h
        .root::<NavShell>()
        .part(Part::Title(Column::Content))
        .and_then(Widget::accessibility)
        .and_then(|node| node.value);
    assert_eq!(
        count.as_deref(),
        Some("12"),
        "the count reads after the title"
    );
}

/// The actions band starts after the leading buttons and the title's
/// measured width; the title starts at the content gutter.
#[test]
fn the_actions_start_after_the_title_and_the_title_at_the_gutter() {
    let theme = Theme::dark();
    let m = Metrics::resolve(&theme);
    let shell = titled_content(vec![
        icon("reply", Icon::Reply, "Reply"),
        icon("delete", Icon::Trash2, "Delete message"),
    ]);
    let mut h = harness(shell, 1024, 700, theme.clone());
    settle(&mut h, 1024, 700);
    let title = part_rect(&h, Part::Title(Column::Content));
    assert!(
        (title.x0 - f64::from(theme.tokens.spacing.rhythm.group)).abs() < 0.5,
        "the title starts at the content gutter ({})",
        title.x0
    );
    let reply = part_rect(&h, Part::Action(Column::Content, 0));
    assert!(
        reply.x0 >= title.x1 + m.gap - 0.5,
        "{reply:?} after {title:?}"
    );
    assert!(reply.x1 < 1024.0 && reply.width() > 0.0);

    // With the sidebar hidden the toggle leads, and the title follows it.
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .content_header(
            "Quarterly planning",
            vec![icon("reply", Icon::Reply, "Reply")],
        )
        .with_sidebar_hidden();
    let mut h = harness(shell, 1024, 700, theme);
    settle(&mut h, 1024, 700);
    let g = geometry(&h);
    let title = part_rect(&h, Part::Title(Column::Content));
    assert!(title.x0 >= g.toggle.x1 + m.gap - 0.5);
    assert!(part_rect(&h, Part::Action(Column::Content, 0)).x0 >= title.x1);
}

/// Labelled buttons collapse to their icons when the row is short; then
/// the rest move into the More menu, and choosing one there runs it.
#[test]
fn actions_collapse_to_icons_then_overflow_into_more() {
    let deleted = SharedRc::new(Cell::new(false));
    let actions = || {
        let deleted = SharedRc::clone(&deleted);
        vec![
            labelled("reply", "Reply to everyone", Icon::Reply),
            labelled("forward", "Forward", Icon::Send),
            labelled("archive", "Archive", Icon::Archive),
            Box::new(
                Button::new("delete", "Delete message")
                    .with_icon(Icon::Trash2)
                    .on_view(move || deleted.set(true)),
            ) as Element,
        ]
    };
    // Wide: labels.
    let mut h = harness(titled_content(actions()), 1400, 700, Theme::dark());
    settle(&mut h, 1400, 700);
    let wide = part_rect(&h, Part::Action(Column::Content, 0));
    assert!(wide.width() > wide.height() * 1.5, "a labelled button");
    assert!(part_rect(&h, Part::More(Column::Content)).width() <= 0.0);

    // Narrower: icons.
    let mut h = harness(titled_content(actions()), 640, 700, Theme::dark());
    settle(&mut h, 640, 700);
    let compact = part_rect(&h, Part::Action(Column::Content, 0));
    assert!(
        (compact.width() - compact.height()).abs() < 1.0,
        "an icon-only square ({compact:?})"
    );
    let reply = h
        .root::<NavShell>()
        .part(Part::Action(Column::Content, 0))
        .and_then(Widget::accessibility)
        .and_then(|node| node.name);
    assert_eq!(
        reply.as_deref(),
        Some("Reply to everyone"),
        "the label stays its name"
    );

    // Narrow: the rest go into More.
    let mut h = harness(titled_content(actions()), 220, 700, Theme::dark());
    settle(&mut h, 220, 700);
    let shown = h.root::<NavShell>().shown_actions(Column::Content);
    assert!(shown < 4, "{shown}");
    let more = part_rect(&h, Part::More(Column::Content));
    assert!(more.width() > 0.0 && more.x1 <= 220.0);
    assert!(
        h.root::<NavShell>()
            .part_hidden(Part::Action(Column::Content, 3)),
        "an action in the menu is not in the row"
    );
    assert!(
        h.root::<NavShell>()
            .exposed_parts()
            .contains(&Part::More(Column::Content))
    );
    // Press More: the menu lists what did not fit.
    h.click_at(more.center().x, more.center().y);
    settle(&mut h, 220, 700);
    let doc = automerge::AutoCommit::new();
    h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
    let labels = at(&h.root::<NavShell>().headers, Column::Content)
        .more
        .menu_labels();
    assert_eq!(labels.last().map(String::as_str), Some("Delete message"));
    // Choose it: the action runs through its door.
    let row = labels.len() - 1;
    at_mut(&mut h.root_mut::<NavShell>().headers, Column::Content)
        .more
        .choose(row);
    // The choice runs with the event that made it.
    h.send(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(10.0, 300.0),
    });
    assert!(deleted.get(), "the chosen action ran through its door");
}

#[test]
fn a_leading_fill_stands_between_the_toggle_and_the_actions() {
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .content_header("", vec![icon("new", Icon::Plus, "New folder")])
        .content_header_leading(Box::new(Button::new("path", "Home › Documents › Taxes")));
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    let leading = part_rect(&h, Part::Leading(Column::Content));
    let action = part_rect(&h, Part::Action(Column::Content, 0));
    assert!(leading.width() > 0.0);
    assert!(leading.x0 >= g.content_column.x0, "in the content's header");
    assert!(leading.x1 <= action.x0, "{leading:?} before {action:?}");
    assert!(leading.y0 >= 0.0 && leading.y1 <= g.header_height);
}

/// A switch states its size and measures nothing; in a header it still
/// stands at that size, beside a labelled button.
#[test]
fn a_switch_in_the_header_keeps_its_own_width() {
    let switch = crate::widget::Toggle::new("power", "Power");
    let intrinsic = crate::widget::ToggleFace::size(&Theme::dark()).0;
    let shell = titled_content(vec![
        Box::new(switch),
        labelled("apply", "Apply", Icon::Check),
    ]);
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let switch = part_rect(&h, Part::Action(Column::Content, 0));
    let apply = part_rect(&h, Part::Action(Column::Content, 1));
    assert!(
        switch.width() >= intrinsic,
        "{switch:?} is at least the switch's own {intrinsic} wide"
    );
    assert!(switch.x1 <= apply.x0, "{switch:?} before {apply:?}");
}

#[test]
fn the_window_group_is_keyed_by_the_shell_id() {
    let mut h = harness(mail_shell().with_id("mail.shell"), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let node = h.root::<NavShell>().accessibility().expect("a group");
    assert_eq!(node.semantic_key.as_deref(), Some("mail.shell.window"));
    let audit = h.accessibility_audit();
    assert!(audit.issues.is_empty(), "{:?}", audit.issues);
}

/// The content's Back follows the app's page stack; the shell draws the
/// chip, and a press pops the stack.
#[test]
fn back_follows_the_apps_page_stack() {
    let pages = LocalState::new(crate::NavStack::new("dashboard"));
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .content_header("Dashboard", Vec::new())
        .with_back(&pages);
    let mut h = harness(shell, 1024, 700, Theme::dark());
    settle(&mut h, 1024, 700);
    assert_eq!(geometry(&h).back, Rect::ZERO, "the root has no Back");

    pages.update(|stack| stack.push("queue"));
    let doc = automerge::AutoCommit::new();
    h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
    settle(&mut h, 1024, 700);
    let g = geometry(&h);
    assert!(g.back.width() > 0.0, "a page pushed: Back shows");
    assert_eq!(g.back_column, Some(Column::Content));
    assert!(part_rect(&h, Part::Title(Column::Content)).x0 >= g.back.x1);

    assert!(h.root_mut::<NavShell>().back());
    assert_eq!(pages.with(|stack| *stack.current()), "dashboard");
    settle(&mut h, 1024, 700);
    assert_eq!(geometry(&h).back, Rect::ZERO);

    // Beside a hidden sidebar's toggle, Back comes second.
    let pages = LocalState::new(crate::NavStack::new("watch"));
    pages.update(|stack| stack.push("creator"));
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0)
        .with_back(&pages)
        .with_sidebar_hidden();
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert!(g.toggle.width() > 0.0 && g.back.x0 > g.toggle.x1);
}

/// Behind an open drawer the other columns stay laid out and painted,
/// inert; the drawer opens under the header row, which stays live.
#[test]
fn an_open_drawer_keeps_the_window_under_its_scrim() {
    let mut h = harness(mail_shell(), 800, 600, Theme::dark());
    h.root_mut::<NavShell>()
        .set_history_control(Some(HistoryControl::Closed));
    settle(&mut h, 800, 600);
    let before = child_rect(&h, "content").expect("content laid out");
    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 800, 600);
    let g = geometry(&h);
    assert!(
        g.sidebar_column.y0 >= g.header_height,
        "under the header row"
    );
    assert!(g.scrim.y0 >= g.header_height - 0.5, "the row is not dimmed");
    let shell = h.root::<NavShell>();
    assert!(shell.host_parts.contains(&Part::Body(Column::Content)));
    assert_eq!(
        shell
            .part(Part::Body(Column::Content))
            .map(Widget::get_rect),
        Some(before),
        "the content keeps its place under the scrim"
    );
    let ids = exposed_ids(&h);
    assert!(!ids.contains(&"content".to_owned()), "inert: {ids:?}");
    assert!(
        ids.contains(&"nav-shell-history".to_owned()),
        "History stays reachable"
    );
    assert!(ids.contains(&"sidebar".to_owned()));
}

#[test]
fn enlarged_drawers_leave_a_pointer_target_on_the_dismissible_scrim() {
    for theme in [Theme::light(), Theme::dark().with_text_scale(2.0)] {
        let band = f64::from(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s4);
        for inspector in [false, true] {
            let shell = NavShell::new()
                .sidebar(Probe::boxed("sidebar").0)
                .content(Probe::boxed("content").0)
                .inspector(Probe::boxed("inspector").0);
            let mut h = harness(shell, 480, 800, theme.clone());
            if inspector {
                h.root_mut::<NavShell>().open_inspector();
            } else {
                h.root_mut::<NavShell>().open_sidebar();
            }
            settle(&mut h, 480, 800);
            let g = geometry(&h);
            let x = if inspector {
                assert!(g.inspector_column.x0 >= band - 0.5);
                g.inspector_column.x0 / 2.0
            } else {
                assert!(480.0 - g.sidebar_column.x1 >= band - 0.5);
                (480.0 + g.sidebar_column.x1) / 2.0
            };
            h.click_at(x, g.scrim.center().y);
            settle(&mut h, 480, 800);
            assert_eq!(
                geometry(&h).scrim,
                Rect::ZERO,
                "scrim press closes the drawer"
            );
        }
    }
}

#[test]
fn a_bound_state_opens_a_drawer_and_hears_the_persons_choice() {
    let open = LocalState::new(true);
    let shell = mail_shell().with_sidebar_state(&open);
    let mut h = harness(shell, 800, 600, Theme::dark());
    settle(&mut h, 800, 600);
    assert!(
        !open.with(|v| *v),
        "the shell wrote where it stands: a closed drawer"
    );
    open.set(true);
    let doc = automerge::AutoCommit::new();
    h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
    settle(&mut h, 800, 600);
    assert!(
        h.root::<NavShell>().is_sidebar_open(),
        "the state opened the drawer"
    );
    h.root_mut::<NavShell>().close_sidebar();
    assert!(!open.with(|v| *v), "the shell wrote the choice back");

    let inspector = LocalState::new(false);
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(Probe::boxed("inspector").0)
        .with_inspector_state(&inspector);
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    assert!(inspector.with(|v| *v), "docked: open");
    inspector.set(false);
    h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
    settle(&mut h, 1280, 800);
    assert!(
        !h.root::<NavShell>().is_inspector_open(),
        "docked, but closed by state"
    );
}

/// The inspector keeps the choice made for its size class while the app
/// takes it away and gives it back.
#[test]
fn the_inspector_remembers_its_choice_across_removal() {
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(Probe::boxed("inspector").0);
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().close_inspector();
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().set_inspector(None);
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>()
        .set_inspector(Some(Probe::boxed("inspector").0));
    settle(&mut h, 1280, 800);
    assert!(!h.root::<NavShell>().is_inspector_open());
}

/// A widget whose overlay holds the pointer (an open popover).
struct Capturing {
    rect: Rect,
    capture: Rect,
    events: SharedRc<Cell<usize>>,
}

impl Widget for Capturing {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::length(32.0),
                height: Dimension::length(32.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::MouseInput { .. }) {
            self.events.set(self.events.get().saturating_add(1));
            *ctx.handled = true;
        }
    }

    fn modal_capture(&self) -> Option<Rect> {
        Some(self.capture)
    }

    fn name(&self) -> &'static str {
        "Capturing"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[test]
fn a_popover_owned_by_a_header_button_hears_the_pointer_first() {
    let events = SharedRc::new(Cell::new(0));
    let (content, content_events) = Probe::boxed("content");
    let shell = NavShell::new().content(content).content_header(
        "Forecast",
        vec![Box::new(Capturing {
            rect: Rect::ZERO,
            capture: Rect::new(600.0, 60.0, 900.0, 400.0),
            events: SharedRc::clone(&events),
        })],
    );
    let mut h = harness(shell, 1024, 700, Theme::dark());
    settle(&mut h, 1024, 700);
    let device = winit::event::DeviceId::dummy();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: device,
        position: winit::dpi::PhysicalPosition::new(700.0, 200.0),
    });
    let before = content_events.get();
    h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: device,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    assert_eq!(events.get(), 1, "the popover's owner heard the press");
    assert_eq!(content_events.get(), before, "the column under it did not");
}

/// A pane that can be filtered and reports its count.
struct Filtered {
    query: SharedRc<RefCell<String>>,
    inner: Probe,
}

impl Filterable for Filtered {
    fn set_filter(&mut self, query: &str) {
        *self.query.borrow_mut() = query.to_owned();
    }

    fn filter_count(&self) -> Option<FilterCount> {
        Some(FilterCount::new(2, 9))
    }
}

impl Widget for Filtered {
    crate::provides!(dyn Filterable);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
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

    fn name(&self) -> &'static str {
        "Filtered"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.inner.id)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.inner.rect
    }
}

#[test]
fn typed_access_controls_and_the_filter_reach_the_apps_widgets() {
    let query = SharedRc::new(RefCell::new(String::new()));
    let list = Filtered {
        query: SharedRc::clone(&query),
        inner: Probe {
            id: "overview",
            deadline: None,
            animating: false,
            rect: Rect::ZERO,
            events: SharedRc::new(Cell::new(0)),
        },
    };
    let shell = NavShell::new()
        .overview(Box::new(list))
        .content(Probe::boxed("content").0)
        .overview_header("Contacts", Vec::new())
        .overview_controls(Box::new(Button::new("sort", "Sort by name").on_view(|| {})))
        .with_filter(SearchField::filter());
    let mut h = harness(shell, 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    let g = geometry(&h);
    assert!(g.filter.width() > 0.0 && g.controls.width() > 0.0);
    assert!(
        g.controls.y0 >= g.filter.y1,
        "the controls stand under the filter"
    );
    let sort = part_rect(&h, Part::Controls);
    assert!(
        (sort.x0 - (g.overview_column.x0 + Metrics::resolve(&Theme::dark()).inset)).abs() < 0.5
    );
    let overview = child_rect(&h, "overview").expect("overview laid out");
    assert!(overview.y0 >= g.controls.y1);

    // A fluid control (a dropdown) stands at the band's height.
    let order = LocalState::new("Name".to_owned());
    let shell = NavShell::new()
        .overview(Probe::boxed("list").0)
        .content(Probe::boxed("detail").0)
        .overview_header("Contacts", Vec::new())
        .overview_controls(Box::new(
            crate::widget::Dropdown::from_state(vec!["Name".into(), "Organization".into()], &order)
                .with_accessible_name("Sort contacts"),
        ));
    let mut d = harness(shell, 1280, 800, Theme::dark());
    settle(&mut d, 1280, 800);
    let sort = part_rect(&d, Part::Controls);
    assert!(sort.width() > 0.0 && sort.height() > 0.0, "{sort:?}");

    assert!(
        h.root_mut::<NavShell>()
            .overview_mut::<Filtered>()
            .is_some()
    );
    assert!(h.root_mut::<NavShell>().content_mut::<Probe>().is_some());
    assert!(h.root_mut::<NavShell>().sidebar_mut::<Probe>().is_none());

    h.root_mut::<NavShell>()
        .filter_mut()
        .expect("the filter")
        .set_query("kai");
    let doc = automerge::AutoCommit::new();
    h.root_mut::<NavShell>().sync(&doc, &automerge::ROOT);
    assert_eq!(query.borrow().as_str(), "kai");
    assert_eq!(
        h.root::<NavShell>().filter().and_then(SearchField::count),
        Some(FilterCount::new(2, 9))
    );
}

#[test]
fn the_overview_collapses_into_its_rail_before_it_pushes() {
    let rail = crate::widget::NavList::new(vec![crate::widget::NavSection::untitled(
        "places",
        vec![
            crate::widget::NavItem::new("oslo", "Oslo").icon(Icon::Pin),
            crate::widget::NavItem::new("lima", "Lima").icon(Icon::Pin),
        ],
    )])
    .rail();
    let shell = NavShell::new()
        .overview(Probe::boxed("overview").0)
        .overview_rail(Box::new(rail))
        .content(Probe::boxed("content").0);
    let theme = Theme::dark();
    let m = Metrics::resolve(&theme);
    let mut h = harness(shell, 1280, 800, theme.clone());
    settle(&mut h, 1280, 800);
    assert_eq!(geometry(&h).overview, OverviewPlacement::List);
    // Wider than the rail and the detail, narrower than the list and it.
    assert!(m.rail + m.detail_min < 600.0 && 600.0 < m.overview + m.detail_min);
    settle(&mut h, 600, 800);
    let g = geometry(&h);
    assert_eq!(g.overview, OverviewPlacement::Rail, "{g:?}");
    assert!(g.stack.is_empty(), "no push while the rail fits");
    assert!((g.rail.width() - m.rail).abs() < 0.5);
    assert!(g.content_column.x0 >= g.rail.x1 - 0.5);
    settle(&mut h, 300, 800);
    assert_eq!(geometry(&h).stack, [NavPane::Overview, NavPane::Content]);
}

#[test]
fn a_shell_with_overflowing_actions_passes_the_native_audits() {
    let shell = titled_content(vec![
        labelled("reply", "Reply to everyone", Icon::Reply),
        labelled("forward", "Forward", Icon::Send),
        labelled("archive", "Archive", Icon::Archive),
        labelled("delete", "Delete message", Icon::Trash2),
    ])
    .with_id("audit.shell");
    let mut h = harness(shell, 220, 700, Theme::dark());
    settle(&mut h, 220, 700);
    let audit = h.accessibility_audit();
    assert!(audit.issues.is_empty(), "{:?}", audit.issues);
    let native = h.native_app_audit();
    assert!(native.issues.is_empty(), "{:?}", native.issues);
}
