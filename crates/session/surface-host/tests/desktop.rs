//! The desktop under the windows and the policy it answers: windows go
//! where the desktop's strips lay their tabs out, and the desktop draws
//! around them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use smithay::utils::{Logical, Rectangle};
use tilo_compositor::{Compositor, OutputConfig, SurfaceHost as _, WindowInfo, WindowPolicy as _};
use tilo_surface_host::desktop;

fn output() -> Vec<Rectangle<i32, Logical>> {
    vec![Rectangle::new((0, 0).into(), (1280, 800).into())]
}

fn window(id: u64, app: &str) -> WindowInfo {
    WindowInfo {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
    }
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
        (first.clone(), Rectangle::new(at, size.into())),
        (second.clone(), Rectangle::new((0, 0).into(), size2.into())),
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
    let arranged = policy.arrange(&mapped[..1], &outputs).unwrap();
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
            &[(first.clone(), Rectangle::new(at, size.into()))],
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
    compositor.add_host(Box::new(host.clone()));
    compositor.render().unwrap();
    let layers = host.layers();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].location, (0, 0));
    assert_eq!(layers[0].size, (640, 480), "the desktop covers the output");
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
        (first.clone(), Rectangle::new((0, 0).into(), size.into())),
        (second.clone(), Rectangle::new((0, 0).into(), size2.into())),
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
