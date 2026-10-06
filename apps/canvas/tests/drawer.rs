//! The inspector in a narrow window: a drawer over the artwork that holds
//! its controls and the layers, none of them cut off by the window's edge.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use ui::prelude::TestApp;

#[test]
fn the_inspector_drawer_holds_its_controls_inside_the_window() {
    let mut app = TestApp::<App>::capture(Some("landscape")).expect("the scenario stages");
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
        own.iter().any(|(name, _)| *name == "canvas-dock"),
        "the open drawer shows the layers and history: {targets:#?}"
    );
    for (name, rect) in own {
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= width,
            "{name} leaves the window: {rect:?}"
        );
    }
}

#[test]
fn blank_phone_welcome_and_header_controls_stay_inside_the_window() {
    let mut app = TestApp::<App>::capture_at(None, (390, 844)).expect("blank artwork");
    assert!(
        app.harness()
            .settle_for_capture(std::time::Duration::from_secs(2))
    );
    let targets = app.harness().targets();
    let welcome = [
        "canvas-welcome-new",
        "canvas-welcome-open",
        "canvas-welcome-close",
    ];
    for name in welcome {
        let rect = targets.get(name).expect("the actual welcome control");
        assert!(
            rect.width() > 0.0 && rect.x0 >= 0.0 && rect.x1 <= 390.0,
            "{name} leaves the phone viewport: {rect:?}"
        );
    }
    let header: Vec<_> = targets
        .iter()
        .filter(|(name, rect)| {
            (name.starts_with("canvas-zoom-")
                || name.as_str() == "canvas-add-picture"
                || name.as_str() == "canvas-menu"
                || name.ends_with("nav-shell-inspector")
                || name.as_str() == "standalone-history-open")
                && rect.height() > 0.0
        })
        .collect();
    assert!(
        !header.is_empty(),
        "the actual header exposes its mounted controls"
    );
    for (index, (name, rect)) in header.iter().enumerate() {
        assert!(rect.x0 >= 0.0 && rect.x1 <= 390.0, "{name}: {rect:?}");
        assert!(
            rect.y1 < targets["canvas-welcome-close"].y0,
            "header and welcome overlap: {name} {rect:?}"
        );
        for (other, other_rect) in &header[index + 1..] {
            assert!(
                rect.intersect(**other_rect).area().total_cmp(&0.0).is_eq(),
                "header controls overlap: {name} {rect:?}; {other} {other_rect:?}"
            );
        }
    }
}
