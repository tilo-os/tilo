//! A navigation-list sidebar reads by its width: a rail while narrow, a
//! list while wide; the width follows the edge's drag (pointer, finger or
//! arrow keys) one to one and is remembered per app.

use super::*;
use crate::widget::nav_list::AdaptiveNav as _;
use crate::widget::{NavItem, NavList, NavSection};

/// Tube's destinations: a short, flat set of places, each with an icon.
fn places() -> NavList {
    NavList::new(place_sections()).with_id("places")
}

fn place_sections() -> Vec<NavSection> {
    let item = |id: &str, label: &str, icon: Icon| NavItem::new(id, label).icon(icon);
    vec![NavSection::untitled(
        "destinations",
        vec![
            item("home", "Home", Icon::Home),
            item("trending", "Trending", Icon::Flame),
            item("subscriptions", "Subscriptions", Icon::GalleryVerticalEnd),
            item("queue", "Queue", Icon::ListVideo),
            item("history", "History", Icon::History),
            item("settings", "Settings", Icon::Settings),
        ],
    )]
}

/// Mailboxes under headings, with a "New folder" action: a list.
fn mailboxes() -> NavList {
    let item = |id: &str, label: &str, icon: Icon| NavItem::new(id, label).icon(icon);
    NavList::new(vec![
        NavSection::untitled(
            "mailboxes",
            vec![
                item("inbox", "Inbox", Icon::Inbox),
                item("sent", "Sent", Icon::Send),
            ],
        ),
        NavSection::new(
            "folders",
            "Folders",
            vec![item("receipts", "Receipts", Icon::Folder)],
        )
        .trailing_action(crate::widget::NavAction::new(
            "new-folder",
            Icon::Plus,
            "New folder",
        )),
    ])
    .with_id("mailboxes")
}

fn shell_with(sidebar: NavList) -> NavShell {
    NavShell::new()
        .with_id("places-shell")
        .content_kind(ContentKind::Grid)
        .sidebar(Box::new(sidebar))
        .content(Probe::boxed("content").0)
}

fn shows_rail(h: &FrameHarness) -> bool {
    h.root::<NavShell>()
        .sidebar_ref::<NavList>()
        .expect("the sidebar is the list")
        .tree()
        .is_rail()
}

fn adaptive(h: &FrameHarness) -> AdaptiveSidebar {
    h.root::<NavShell>()
        .inputs
        .borrow()
        .adaptive
        .expect("a navigation-list sidebar adapts to its width")
}

fn sidebar_width(h: &FrameHarness) -> f64 {
    geometry(h).sidebar_column.width()
}

const W: u32 = 1400;
const H: u32 = 800;

fn press_edge(h: &mut FrameHarness) -> Point {
    let edge = geometry(h).resize_handle.center();
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(edge.x, edge.y),
    });
    assert!(h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }));
    edge
}

fn move_pointer(h: &mut FrameHarness, x: f64, y: f64) {
    h.dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    });
    h.relayout_at((W, H)).expect("layout");
}

fn release(h: &mut FrameHarness) {
    h.dispatch_to_root(&WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    settle(h, W, H);
}

/// A short, flat set of places opens as a rail at the rail's width, never
/// as rail tiles in a wide sidebar. Dragging the edge out, the edge stays
/// under the pointer at every step, the width only grows, and the rows turn
/// into a list exactly where the width crosses the threshold; dragging back
/// turns them into a rail again.
#[test]
fn the_width_threshold_switches_the_items_between_rail_and_list() {
    let mut h = harness(shell_with(places()), W, H, Theme::dark());
    settle(&mut h, W, H);
    let adaptive = adaptive(&h);
    assert!(
        adaptive.rail_default,
        "a short flat set of places is a rail"
    );
    assert!(shows_rail(&h), "it opens as a rail");
    let start = sidebar_width(&h);
    assert!(
        (start - adaptive.rail).abs() < 0.5,
        "the rail is as wide as a rail ({start} vs {})",
        adaptive.rail
    );
    assert!(
        start < f64::from(Theme::dark().tokens.metrics.sidebar_min_width),
        "never rail tiles in a sidebar's width ({start})"
    );
    assert!(adaptive.list_from > adaptive.rail);

    let edge = press_edge(&mut h);
    let mut last = start;
    let mut switched_at = None;
    for step in 1..=60 {
        let dx = f64::from(step) * 4.0;
        move_pointer(&mut h, edge.x + dx, edge.y);
        let width = sidebar_width(&h);
        assert!(
            (width - (start + dx)).abs() < 0.5,
            "the edge stays under the pointer: {width} after dragging {dx} from {start}"
        );
        assert!(
            width >= last,
            "dragging out never narrows: {width} after {last}"
        );
        last = width;
        let rail = shows_rail(&h);
        assert_eq!(
            rail,
            width < adaptive.list_from - 1e-6,
            "rail below the threshold ({}), list from it, at {width}",
            adaptive.list_from
        );
        if !rail && switched_at.is_none() {
            switched_at = Some(width);
        }
    }
    assert!(switched_at.is_some(), "dragged wide, it became a list");

    // Back toward the rail: a rail again below the threshold, and the
    // narrowest it gets while it shows is the rail's width.
    move_pointer(&mut h, edge.x + 2.0, edge.y);
    assert!(shows_rail(&h), "narrow again, a rail again");
    move_pointer(&mut h, edge.x - 10.0, edge.y);
    assert!(
        (sidebar_width(&h) - adaptive.rail).abs() < 0.5,
        "never narrower than the rail while it shows"
    );
    release(&mut h);
    assert!(h.root::<NavShell>().is_sidebar_open());
}

/// Places under headings with a "New" action open as a list at the
/// sidebar's width; dragged narrow enough, they read as a rail too, and
/// collapsing stays the sidebar toggle's (or a drag past the rail).
#[test]
fn a_list_with_headings_opens_as_a_list_and_narrows_into_a_rail() {
    let theme = Theme::dark();
    let mut h = harness(shell_with(mailboxes()), W, H, theme.clone());
    settle(&mut h, W, H);
    let adaptive = adaptive(&h);
    assert!(
        !adaptive.rail_default,
        "headings and a section action need a list"
    );
    assert!(!shows_rail(&h));
    let start = sidebar_width(&h);
    assert!(
        (start - f64::from(theme.tokens.metrics.sidebar_width)).abs() < 0.5,
        "{start}"
    );
    let edge = press_edge(&mut h);
    move_pointer(&mut h, edge.x - (start - adaptive.rail), edge.y);
    assert!(shows_rail(&h), "narrowed to its rail, it reads as one");
    move_pointer(&mut h, edge.x - start + 10.0, edge.y);
    release(&mut h);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "dragged past the rail, the sidebar hides"
    );
    h.root_mut::<NavShell>().toggle_sidebar();
    settle(&mut h, W, H);
    assert!(
        h.root::<NavShell>().is_sidebar_open(),
        "the toggle shows it again"
    );
}

/// The rail-or-list rule runs once, when the window first shows its
/// sidebar. Places that load afterwards (and would on their own have made
/// a rail) leave the width and the form alone; the person's drag still
/// moves both.
#[test]
fn places_that_arrive_after_the_first_layout_leave_the_form_alone() {
    let theme = Theme::dark();
    let mut h = harness(
        shell_with(NavList::new(Vec::new()).with_id("places")),
        W,
        H,
        theme.clone(),
    );
    settle(&mut h, W, H);
    assert!(!adaptive(&h).rail_default, "nothing loaded yet: a list");
    assert!(!shows_rail(&h));
    let start = sidebar_width(&h);
    assert!(
        (start - f64::from(theme.tokens.metrics.sidebar_width)).abs() < 0.5,
        "{start}"
    );

    h.root_mut::<NavShell>()
        .sidebar_mut::<NavList>()
        .expect("the sidebar is the list")
        .set_sections(place_sections());
    h.root_mut::<NavShell>().invalidate();
    settle(&mut h, W, H);
    assert!(
        NavList::new(place_sections()).rail_by_default(&theme),
        "on their own these places would open as a rail"
    );
    assert!(!adaptive(&h).rail_default, "the rule does not run again");
    assert!(
        !shows_rail(&h),
        "the items arriving do not turn it into a rail"
    );
    assert!(
        (sidebar_width(&h) - start).abs() < 0.5,
        "nor move its edge ({} vs {start})",
        sidebar_width(&h)
    );

    let rail = adaptive(&h).rail;
    let edge = press_edge(&mut h);
    move_pointer(&mut h, edge.x - (start - rail), edge.y);
    release(&mut h);
    assert!(shows_rail(&h), "the person's drag still makes it a rail");
}

/// Remembers where the window's host keeps it, and forgets on drop.
struct Remembering(Option<crate::storage::Settings<memory::NavShellMemory>>);

impl Drop for Remembering {
    fn drop(&mut self) {
        memory::install(self.0.take());
    }
}

/// The width the person drags to is kept in the app's settings on this
/// device when the drag ends, and a window of the app opened later starts
/// at it, rail or list as that width reads.
#[test]
fn the_chosen_width_is_remembered_for_the_next_window() {
    let dir = tempfile::tempdir().expect("a settings directory");
    let settings = crate::storage::Settings::<memory::NavShellMemory>::for_app("so.tilo.tube")
        .in_file(dir.path().join("settings.json"));
    let _restore = Remembering(memory::install(Some(settings.clone())));

    let mut h = harness(shell_with(places()), W, H, Theme::dark());
    settle(&mut h, W, H);
    let start = sidebar_width(&h);
    let edge = press_edge(&mut h);
    move_pointer(&mut h, edge.x + 160.0, edge.y);
    release(&mut h);
    let chosen = sidebar_width(&h);
    assert!((chosen - (start + 160.0)).abs() < 0.5, "{chosen}");
    assert!(!shows_rail(&h), "widened, a list");
    let kept = settings
        .get()
        .nav_sidebar_widths
        .get("places-shell")
        .copied()
        .expect("the width is kept when the drag ends");
    assert!((kept - chosen).abs() < 0.5, "kept {kept}, chose {chosen}");

    let mut again = harness(shell_with(places()), W, H, Theme::dark());
    settle(&mut again, W, H);
    assert!(
        (sidebar_width(&again) - chosen).abs() < 0.5,
        "the next window opens at the chosen width"
    );
    assert!(!shows_rail(&again), "and reads as a list there");
}

fn finger(h: &mut FrameHarness, phase: winit::event::TouchPhase, x: f64, y: f64) -> bool {
    h.dispatch_to_root(&WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(x, y),
        force: None,
        id: 7,
    }))
}

/// A finger landing near the edge takes it (it is the shell's, not a
/// scroll of what is beside it), the width follows the finger one to one
/// and switches the rows at the same threshold, and lifting it ends the
/// drag with the width kept.
#[test]
fn a_finger_drags_the_edge_like_the_pointer() {
    use winit::event::TouchPhase;
    let mut h = harness(shell_with(places()), W, H, Theme::dark());
    settle(&mut h, W, H);
    let start = sidebar_width(&h);
    let edge = geometry(&h).resize_handle.center();
    // Within a pointer target of the edge, not only on its thin grip.
    let landing = Point::new(edge.x + 10.0, edge.y);
    assert!(
        h.root::<NavShell>().claims_raw_touch(landing),
        "a finger near the edge is the edge's"
    );
    assert!(
        !h.root::<NavShell>()
            .claims_raw_touch(Point::new(edge.x + 200.0, edge.y)),
        "a finger on the content is not"
    );
    assert!(finger(&mut h, TouchPhase::Started, landing.x, landing.y));
    let adaptive = adaptive(&h);
    let mut last = start;
    for step in 1..=30 {
        let dx = f64::from(step) * 8.0;
        assert!(finger(&mut h, TouchPhase::Moved, landing.x + dx, landing.y));
        h.relayout_at((W, H)).expect("layout");
        let width = sidebar_width(&h);
        assert!((width - (start + dx)).abs() < 0.5, "{width} after {dx}");
        assert!(width >= last);
        last = width;
        assert_eq!(shows_rail(&h), width < adaptive.list_from - 1e-6);
    }
    assert!(finger(
        &mut h,
        TouchPhase::Ended,
        landing.x + 240.0,
        landing.y
    ));
    settle(&mut h, W, H);
    assert!((sidebar_width(&h) - (start + 240.0)).abs() < 0.5);
    assert!(
        !finger(&mut h, TouchPhase::Moved, landing.x, landing.y),
        "a lifted finger drags nothing"
    );
}

/// The arrow keys on the focused separator step the width as a drag does,
/// across the threshold: the rail becomes a list.
#[test]
fn the_arrow_keys_on_the_separator_widen_a_rail_into_a_list() {
    let mut h = harness(shell_with(places()), W, H, Theme::dark());
    settle(&mut h, W, H);
    assert!(shows_rail(&h));
    let resize = sidebar_resize_control(&h).expect("the rail's edge is a separator");
    h.operate_control(&resize.id, &crate::headless::ControlInput::Focus)
        .expect("focus the edge");
    let mut last = sidebar_width(&h);
    for _ in 0..40 {
        h.key("ArrowRight");
        settle(&mut h, W, H);
        let width = sidebar_width(&h);
        assert!(width >= last, "a step never narrows: {width} after {last}");
        last = width;
        if !shows_rail(&h) {
            break;
        }
    }
    assert!(!shows_rail(&h), "stepped wide, it reads as a list");
}
