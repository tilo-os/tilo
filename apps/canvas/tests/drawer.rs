//! The inspector in a narrow window: a drawer over the artwork that holds
//! the brush and layer controls, none of them cut off by the window's edge.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::TestApp;

#[test]
fn the_inspector_drawer_holds_its_controls_inside_the_window() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.stage(Some("landscape")).expect("the scenario stages");
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
        own.iter().any(|(name, _)| *name == "canvas-brush-size"),
        "the open drawer shows the brush control: {targets:#?}"
    );
    for (name, rect) in own {
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= width,
            "{name} leaves the window: {rect:?}"
        );
    }
}
