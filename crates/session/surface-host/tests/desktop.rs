//! The desktop under the windows and the policy it answers: windows go
//! where the desktop's strips lay their tabs out, and the desktop draws
//! around them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tilo_compositor::{Compositor, LayoutRect, OutputConfig, WindowInfo, WindowPolicy as _};

use tilo_desktop_model::TileContent;
use tilo_desktop_policy::DesktopPolicy;
use tilo_session_core::surface::SurfaceHost as _;
use tilo_surface_host::{DesktopHost, desktop};

fn output() -> Vec<LayoutRect> {
    vec![LayoutRect::new((0, 0).into(), (1280, 800).into())]
}

fn window(id: u64, app: &str) -> WindowInfo {
    WindowInfo {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..WindowInfo::default()
    }
}

/// Every tab the desk holds, in tree order: its id, what it shows and what it
/// holds.
fn tabs(host: &DesktopHost) -> Vec<(u64, String, TileContent)> {
    host.with_policy(|policy| {
        policy.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .map(|tab| (tab.id, tab.title.clone(), tab.tile.clone()))
                .collect()
        })
    })
}

/// The tab of the window `id`, when the desk holds one.
fn window_tab(host: &DesktopHost, id: u64) -> Option<(u64, String, TileContent)> {
    tabs(host).into_iter().find(
        |(_, _, tile)| matches!(tile, TileContent::Foreign { surface_id, .. } if *surface_id == id),
    )
}

#[test]
fn windows_open_side_by_side_where_their_tabs_are_laid_out() {
    let _settled = ui::motion::testing::settled();
    let (_host, mut policy) = desktop(7, 1.0);
    let outputs = output();
    let first = window(1, "org.gnome.Terminal");
    let size = policy
        .initial_size(&first, &outputs)
        .expect("a tab to fill");
    let at = policy.place(&first, size, &outputs);
    assert!(size.0 > 0 && size.1 > 0);
    assert!(
        at.x >= 0 && at.x + size.0 <= 1280,
        "in view: {at:?} {size:?}"
    );

    let second = window(2, "org.mozilla.firefox");
    let size2 = policy.initial_size(&second, &outputs).unwrap();
    let mapped = [
        (first.clone(), LayoutRect::new(at, size.into())),
        (second.clone(), LayoutRect::new((0, 0).into(), size2.into())),
    ];
    let arranged = policy.arrange(&mapped, &outputs).unwrap();
    assert_eq!(arranged.len(), 2);
    let place = |id: u64| arranged.iter().find(|a| a.id == id).unwrap();
    assert!(
        place(2).location.x >= place(1).location.x,
        "the later window opens right of the first: {arranged:?}"
    );
    assert_eq!(policy.focused(), Some(2), "the window opened takes focus");

    assert!(policy.focus_on_click(&first));
    policy.arrange(&mapped, &outputs).unwrap();
    assert_eq!(policy.focused(), Some(1));

    // The second window went away: its tab closes and the first fills in.
    let (kept, _gone) = mapped
        .split_first()
        .expect("the first window is still there");
    let arranged = policy
        .arrange(std::slice::from_ref(kept), &outputs)
        .unwrap();
    assert_eq!(arranged.len(), 1);
    assert_eq!(policy.focused(), Some(1));
    assert!(policy.take_close_requests().is_empty(), "it closed itself");
}

#[test]
fn a_window_waits_in_its_place_until_it_shows_a_buffer() {
    let _settled = ui::motion::testing::settled();
    let (host, mut policy) = desktop(7, 1.0);
    let outputs = output();
    let first = window(1, "org.gnome.Terminal");
    let size = policy.initial_size(&first, &outputs).unwrap();
    // Laid out again before its first buffer: the compositor lists no
    // window yet, and the place stays.
    policy.arrange(&[], &outputs).unwrap();
    let tabs = || host.with_policy(|policy| policy.layout().shown.len());
    assert_eq!(tabs(), 1, "the pending window keeps its tab");
    let at = policy.place(&first, size, &outputs);
    assert!(at.x >= 0, "placed where it waited: {at:?}");

    // One that goes away before it shows a buffer gives its place up.
    let second = window(2, "org.mozilla.firefox");
    policy.initial_size(&second, &outputs).unwrap();
    policy.withdrawn(&second);
    policy
        .arrange(
            &[(first.clone(), LayoutRect::new(at, size.into()))],
            &outputs,
        )
        .unwrap();
    let layout = host.with_policy(|policy| policy.layout());
    assert!(
        !layout.shown.iter().any(|(id, _)| *id == 2) && !layout.hidden.contains(&2),
        "{layout:?}"
    );
    assert!(policy.take_close_requests().is_empty());
}

#[test]
fn the_desktop_lies_under_the_windows_and_covers_the_output() {
    let _settled = ui::motion::testing::settled();
    let (host, policy) = desktop(7, 1.0);
    assert!(host.below_windows());
    let mut compositor = Compositor::headless(Box::new(policy)).unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .unwrap();
    compositor.add_host(Rc::new(RefCell::new(host.clone())));
    compositor.render().unwrap();
    let layers = host.layers();
    assert_eq!(layers.len(), 1);
    let (layer, _rest) = layers.split_first().expect("the desktop draws one layer");
    assert_eq!(layer.location, (0, 0));
    assert_eq!(layer.size, (640, 480), "the desktop covers the output");
    assert!(
        host.accepts_pointer(320.0, 240.0),
        "and takes the pointer there"
    );
}

#[test]
fn a_window_refused_the_keyboard_waits_on_its_tab_until_the_person_looks() {
    let _settled = ui::motion::testing::settled();
    let (host, mut policy) = desktop(7, 1.0);
    let outputs = output();
    let first = window(1, "org.gnome.Terminal");
    let second = window(2, "org.mozilla.firefox");
    let size = policy.initial_size(&first, &outputs).unwrap();
    let size2 = policy.initial_size(&second, &outputs).unwrap();
    let mapped = [
        (first.clone(), LayoutRect::new((0, 0).into(), size.into())),
        (second.clone(), LayoutRect::new((0, 0).into(), size2.into())),
    ];
    policy.arrange(&mapped, &outputs).unwrap();
    assert_eq!(policy.focused(), Some(2));
    let asks = |id: u64| host.with_policy(|policy| policy.asks_attention(id));

    // A stale activation token: the first window does not take the
    // keyboard from what the person is in, and waits instead.
    assert!(!policy.activate(&first, false));
    policy.arrange(&mapped, &outputs).unwrap();
    assert_eq!(policy.focused(), Some(2));
    assert!(asks(1));
    // The window in front never waits.
    assert!(!policy.activate(&second, false));
    assert!(!asks(2));

    // Looking at it answers it.
    assert!(policy.focus_on_click(&first));
    policy.arrange(&mapped, &outputs).unwrap();
    assert_eq!(policy.focused(), Some(1));
    assert!(!asks(1));

    // A fresh request comes from what the person just did: it is brought
    // forward at once.
    assert!(policy.activate(&second, true));
    policy.arrange(&mapped, &outputs).unwrap();
    assert_eq!(policy.focused(), Some(2));
    assert!(!asks(2));
}

/// A window's own asking to cover the screen is answered by the desktop: the
/// tab fills its strip or covers its display and gives it back, and the
/// compositor is told the desktop took it rather than covering the window on
/// the display itself.
#[test]
fn a_window_that_asks_to_cover_the_screen_fills_its_strip_and_gives_it_back() {
    use tilo_compositor::{ResizeEdges, WindowRequest};

    let _settled = ui::motion::testing::settled();
    let (host, mut policy) = desktop(7, 1.0);
    let outputs = output();
    let first = window(1, "org.gnome.Terminal");
    let size = policy.initial_size(&first, &outputs).unwrap();
    let at = policy.place(&first, size, &outputs);
    let mapped = [(first.clone(), LayoutRect::new(at, size.into()))];
    policy.arrange(&mapped, &outputs).unwrap();
    let maximized = || {
        host.with_policy(|policy| {
            policy.handle().read(|tree| {
                tree.active_workspace()
                    .and_then(|workspace| {
                        workspace
                            .strips
                            .iter()
                            .flat_map(|strip| &strip.window_groups)
                            .next()
                    })
                    .is_some_and(|group| group.maximized)
            })
        })
    };
    let covering = || host.with_policy(|policy| policy.layout().fullscreen);
    assert!(!maximized(), "a window opens at its width");
    assert!(
        !policy.request(&first, &WindowRequest::Maximize(true)),
        "its strip answers the asking"
    );
    assert!(maximized(), "and fills it");
    assert!(
        !policy.request(
            &first,
            &WindowRequest::Fullscreen {
                on: true,
                output: None
            }
        ),
        "the whole display is the desktop's to give too"
    );
    assert_eq!(covering(), Some(1), "the window covers its display");
    assert!(
        !policy.request(
            &first,
            &WindowRequest::Fullscreen {
                on: false,
                output: None
            }
        ),
        "and giving it back is answered here too"
    );
    assert_eq!(covering(), None, "the display is the desktop's again");
    assert!(
        !policy.request(&first, &WindowRequest::Maximize(false)),
        "as is the strip"
    );
    assert!(!maximized(), "the window is its own width again");
    // A tab has no free geometry: a move or a resize asks for nothing.
    let edges = ResizeEdges {
        right: true,
        ..ResizeEdges::default()
    };
    for request in [WindowRequest::Move, WindowRequest::Resize(edges)] {
        assert!(!policy.request(&first, &request));
        assert!(!maximized());
    }
}

/// An X11 client's window is a tab of the strip, named by its class and
/// showing its own title, focused as it opens. Its title reaches the tab as
/// the client changes it, and so does its class: a window renamed to another
/// program is that program's window. Its close goes through the tab's close,
/// which a reopen undoes with the app asked for again.
#[test]
fn an_x11_window_is_a_tab_of_its_class_that_retitles_and_reopens() {
    let _settled = ui::motion::testing::settled();
    let (host, mut policy) = desktop(7, 1.0);
    let outputs = output();
    let mut xterm = WindowInfo {
        id: 9,
        app_id: Some("XTerm".into()),
        title: Some("user@host: ~".into()),
        ..WindowInfo::default()
    };
    let size = policy
        .initial_size(&xterm, &outputs)
        .expect("a tab to fill");
    let at = policy.place(&xterm, size, &outputs);
    let mut mapped = vec![(xterm.clone(), LayoutRect::new(at, size.into()))];
    policy.arrange(&mapped, &outputs).unwrap();

    let (_, title, shown) = window_tab(&host, 9).expect("the window opened a tab");
    assert_eq!(
        shown,
        TileContent::Foreign {
            app_id: "XTerm".into(),
            surface_id: 9
        },
        "a window is known by its class, as any window is by its app"
    );
    assert_eq!(title, "user@host: ~", "and shows its own title");
    assert_eq!(
        policy.focused(),
        Some(9),
        "the window that opens takes focus"
    );

    // The client retitles itself, as a terminal does on every prompt.
    xterm.title = Some("vim ~/notes".into());
    mapped = vec![(xterm.clone(), LayoutRect::new(at, size.into()))];
    policy.arrange(&mapped, &outputs).unwrap();
    let (tab, title, _) = window_tab(&host, 9).unwrap();
    assert_eq!(title, "vim ~/notes", "the title follows the window");

    // The client says it is another program: the tab belongs to that one now.
    xterm.app_id = Some("Thunderbird".into());
    mapped = vec![(xterm.clone(), LayoutRect::new(at, size.into()))];
    policy.arrange(&mapped, &outputs).unwrap();
    let (renamed, _, shown) = window_tab(&host, 9).expect("the same tab, another app");
    assert_eq!(renamed, tab, "renaming a window is no new tab");
    assert!(
        matches!(shown, TileContent::Foreign { ref app_id, .. } if app_id == "Thunderbird"),
        "the tab follows the class the client set: {shown:?}"
    );

    // The client closes its window: the tab goes through the close every
    // close goes through, remembered so a reopen undoes it.
    policy.arrange(&[], &outputs).unwrap();
    assert!(window_tab(&host, 9).is_none(), "the tab went with it");
    assert!(
        tabs(&host).is_empty(),
        "and its group, it was the group's only tab"
    );
    assert!(
        policy.take_close_requests().is_empty(),
        "the window closed itself"
    );

    assert!(
        host.with_policy(DesktopPolicy::reopen_closed),
        "the newest close is undone"
    );
    // The desk carries the command out on its next pass, as it does for the
    // session's loop.
    host.with_widgets(tilo_surface_host::WidgetHost::relayout);
    let (reopened, title, shown) = tabs(&host).pop().expect("the tab is back");
    assert_eq!(reopened, tab, "the window comes back in its own tab");
    assert_eq!(title, "vim ~/notes", "named as the person left it");
    assert!(
        matches!(
            shown,
            TileContent::LocalApp { ref app_id, ref label, .. }
                if app_id == "Thunderbird" && label == "vim ~/notes"
        ),
        "and holds the app that opens it again: {shown:?}"
    );
    let asked = host.take_launches();
    assert_eq!(
        asked.iter().map(|ask| ask.app.as_str()).collect::<Vec<_>>(),
        ["Thunderbird"],
        "the app is asked for again, by the name its window announced"
    );
    assert_eq!(
        asked[0].tab,
        Some(reopened),
        "and in the tab its window had, where the person left it"
    );
    assert!(
        host.take_launches().is_empty(),
        "one reopen asks for it once"
    );
}

/// A window opened where the strip is fuller than the display stands on that display in the frame
/// that maps it: the strip takes the slots its windows are going to and pans to the new one in the
/// same pass, so the place the compositor is given is one it can draw — and the window stays there,
/// rather than sliding onto the screen a few frames later.
#[test]
fn a_window_opened_beyond_the_visible_strip_stands_on_the_output_at_once() {
    // The springs a person's frame shows are the point: a slot still on its way to its width would
    // lay the opening window out past its neighbour's old one, off the display.
    let (host, mut policy) = desktop(7, 1.0);
    let outputs = output();
    let apps = [
        "org.gnome.Terminal",
        "org.mozilla.firefox",
        "so.tilo.notes",
        "so.tilo.chat",
        "so.tilo.maps",
        "so.tilo.sheets",
    ];
    let mut mapped: Vec<(WindowInfo, LayoutRect)> = Vec::new();
    let mut now = ui::time::now();
    let on_output = |placed: &tilo_compositor::Arrangement, app: &str, when: &str| {
        let width = placed.size.unwrap_or_default().0;
        assert!(
            placed.location.x >= 0 && placed.location.x + width <= 1280,
            "{app}: opened off the output at {:?} ({width} wide) {when}; the strip's slots and its \
             pan are not in the frame that opens it",
            placed.location
        );
    };
    for (i, app) in apps.iter().enumerate() {
        let id = u64::try_from(i).expect("a few windows") + 1;
        let opened = window(id, app);
        let size = policy
            .initial_size(&opened, &outputs)
            .expect("the desk holds a place for the window");
        let at = policy.place(&opened, size, &outputs);
        mapped.push((opened, LayoutRect::new(at, size.into())));
        // What the compositor asks once the buffer that maps the window arrived.
        let arranged = policy
            .arrange(&mapped, &outputs)
            .expect("the desk places windows");
        let placed = arranged.iter().find(|a| a.id == id).expect("its place");
        on_output(placed, app, "in the frame that maps it");
        let at = placed.location;
        // Two frames on: the window is where it was put, not sliding onto the screen.
        for _ in 0..2 {
            now = now
                .checked_add(Duration::from_millis(16))
                .expect("a moment");
            ui::time::testing::with_monotonic_time(now, || {
                host.with_widgets(tilo_surface_host::WidgetHost::relayout);
            });
        }
        let arranged = policy
            .arrange(&mapped, &outputs)
            .expect("the desk places windows");
        let placed = arranged.iter().find(|a| a.id == id).expect("its place");
        on_output(placed, app, "two frames on");
        assert_eq!(
            placed.location, at,
            "{app}: the window was placed where it was still moving to"
        );
    }
}
