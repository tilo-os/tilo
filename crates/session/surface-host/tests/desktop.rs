//! The desktop under the windows and the policy it answers: windows go
//! where the desktop's strips lay their tabs out, and the desktop draws
//! around them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use smithay::utils::{Logical, Rectangle};
use tilo_compositor::{Compositor, OutputConfig, SurfaceHost as _, WindowInfo, WindowPolicy as _};
use tilo_surface_host::desktop;

const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

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
fn the_desktop_draws_under_the_windows_and_covers_the_output() {
    let _settled = ui::motion::testing::settled();
    let (host, policy) = desktop(7, 1.0);
    assert!(host.below_windows());
    let mut compositor = Compositor::headless(Box::new(policy)).unwrap();
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .unwrap();
    compositor.add_host(Box::new(host));
    compositor.render().unwrap();
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let drawn = (0..frame.height)
        .step_by(16)
        .flat_map(|y| (0..frame.width).step_by(16).map(move |x| (x, y)))
        .any(|(x, y)| frame.pixel(x, y) != Some(BACKGROUND));
    assert!(drawn, "the desktop shows on the output");
}
