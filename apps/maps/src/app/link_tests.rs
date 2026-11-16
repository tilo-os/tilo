use super::*;

/// A `geo:` link reaches Maps however it arrives: launched on it, or handed to the open window
/// through the `open-link` every app publishes. A point centres the map there; a search searches.
#[test]
fn a_geo_link_shows_its_point_or_its_search() {
    let mut launched =
        ui::framework::testing::TestApp::<crate::MapsShell>::launched(&["geo:52.52,13.405"])
            .unwrap();
    launched.settle();
    let center = launched
        .with_surface(|window: &mut MapWindow| window.app().model.center)
        .unwrap();
    // The point stands in the middle of the map the panel leaves visible, a little off the
    // model's centre.
    assert!(
        (center.lat - 52.52).abs() < 0.05 && (center.lon - 13.405).abs() < 0.05,
        "the launch's point is in view: {center:?}"
    );

    let mut app = ui::framework::testing::TestApp::<crate::MapsShell>::new().unwrap();
    app.with_surface(|window: &mut MapWindow| window.app().online = false)
        .unwrap();
    app.call(
        ui::tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
        serde_json::json!({ "link": "geo:48.137,11.575;u=30" }),
    )
    .unwrap();
    app.settle();
    let center = app
        .with_surface(|window: &mut MapWindow| window.app().model.center)
        .unwrap();
    assert!(
        (center.lat - 48.137).abs() < 0.05 && (center.lon - 11.575).abs() < 0.05,
        "{center:?}"
    );
    app.call(
        ui::tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
        serde_json::json!({ "link": "geo:0,0?q=caf%C3%A9+berlin" }),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.with_surface(|window: &mut MapWindow| window.app().model.query.clone()),
        Some("café berlin".to_owned())
    );
    for refused in ["geo:91,0", "tube:search?q=x", "geo:"] {
        assert!(
            app.call(
                ui::tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
                serde_json::json!({ "link": refused }),
            )
            .is_err(),
            "{refused}"
        );
    }
}
