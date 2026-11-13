//! A navigation-list sidebar reads by its width: a rail while narrow, a
//! list while wide; the width follows the edge's drag (pointer, finger or
//! arrow keys) one to one and is remembered per app.

use super::*;
use crate::widget::{NavItem, NavList, NavSection};

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

/// A short, flat set of places opens as a labeled list at the sidebar's width: the rail holds
/// short captions, and a window with room for the labels gives them.
#[test]
fn a_sidebar_opens_as_a_list_and_the_width_switches_it_to_a_rail() {
    let theme = Theme::dark();
    let mut h = harness(shell_with(places()), W, H, theme.clone());
    settle(&mut h, W, H);
    let adaptive = adaptive(&h);
    assert!(!shows_rail(&h), "places open as a labeled list");
    let start = sidebar_width(&h);
    assert!(
        start >= f64::from(theme.tokens.metrics.sidebar_width) - 0.5,
        "at the sidebar's width: {start}"
    );
    assert_eq!(
        shows_rail(&h),
        start < adaptive.list_from - 1e-6,
        "the width alone decides ({start} against {})",
        adaptive.list_from
    );

    let edge = press_edge(&mut h);
    let mut last = start;
    for step in 1..=20 {
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
        assert!(!shows_rail(&h), "still a list at {width}");
    }

    move_pointer(&mut h, edge.x - (start - adaptive.list_from) - 20.0, edge.y);
    let narrow = sidebar_width(&h);
    assert!(
        narrow < adaptive.list_from,
        "narrower than the labels need ({narrow})"
    );
    assert!(shows_rail(&h), "a rail at {narrow}");
    move_pointer(&mut h, edge.x - (start + 10.0), edge.y);
    release(&mut h);
    assert!(
        !h.root::<NavShell>().is_sidebar_open(),
        "dragged past the rail, the sidebar hides"
    );
}

/// Places under headings with a "New" action open as a list at the
/// sidebar's width; dragged narrow enough, they read as a rail too, and
/// collapsing stays the sidebar toggle's (or a drag past the rail).
#[test]
fn a_list_with_headings_narrows_into_a_rail() {
    let theme = Theme::dark();
    let mut h = harness(shell_with(mailboxes()), W, H, theme.clone());
    settle(&mut h, W, H);
    let adaptive = adaptive(&h);
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

/// The width decides, so places that arrive after the first layout read as
/// the list they now are: nothing is settled about their form before they
/// are there, and their labels widen the sidebar to fit, up to its maximum.
#[test]
fn places_that_arrive_after_the_first_layout_set_the_width() {
    let theme = Theme::dark();
    let mut h = harness(
        shell_with(NavList::new(Vec::new()).with_id("places")),
        W,
        H,
        theme.clone(),
    );
    settle(&mut h, W, H);
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
    let adaptive = adaptive(&h);
    assert!(
        !shows_rail(&h),
        "the items arriving read as the labels they are"
    );
    let width = sidebar_width(&h);
    assert!(
        width >= start - 0.5 && width <= adaptive.default + 0.5,
        "the list's labels set its width ({width} from {start}, at most {})",
        adaptive.default
    );

    let rail = adaptive.rail;
    let edge = press_edge(&mut h);
    move_pointer(&mut h, edge.x - (width - rail), edge.y);
    release(&mut h);
    assert!(shows_rail(&h), "the person's drag still makes it a rail");
}
