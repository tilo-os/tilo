//! The inspector in a narrow window: a drawer over the artwork that holds
//! the brush and layer controls, none of them cut off by the window's edge.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::{ResourceApp, TestApp};

#[test]
fn the_inspector_drawer_holds_its_controls_inside_the_window() {
    // An unselected artwork shows brush controls; the landscape capture
    // deliberately selects the sun and therefore shows its shape controls.
    for (scenario, expected_control) in [
        (None, "canvas-brush-size"),
        (Some("landscape"), "canvas-stroke"),
    ] {
        let mut app = TestApp::<App>::capture_at(scenario, App::shot_viewport(None))
            .expect("the app opens with its scenario");
        let width = 600.0;
        app.harness().resize((600, 800)).expect("resizes");
        let toggle = app
            .harness()
            .targets()
            .into_keys()
            .find(|name| name.ends_with("nav-shell-inspector"))
            .expect("a narrow window offers the inspector toggle");
        app.harness()
            .click_named(&toggle)
            .expect("the toggle opens");
        assert!(
            app.harness()
                .settle_for_capture(std::time::Duration::from_secs(2)),
            "the drawer settles"
        );
        let targets = app.harness().targets();
        let own: Vec<_> = targets
            .iter()
            .filter(|(name, _)| name.starts_with("canvas-") || name.starts_with("add-layer"))
            .collect();
        assert!(
            own.iter().any(|(name, _)| *name == expected_control),
            "{scenario:?}: the open drawer shows {expected_control}: {targets:#?}"
        );
        for (name, rect) in own {
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= width,
                "{name} leaves the window: {rect:?}"
            );
        }
    }
}
