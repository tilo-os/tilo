//! A live window opens on its pane without reading every other area of
//! Settings first (TIL-712), and loses nothing for it: the pane shown
//! accepts what it accepted, a row of an area not yet read is still
//! reached, and a few frames after the first the window publishes exactly
//! what reading every area up front published.
//!
//! Each case compares a window that puts its reads off, as one reaching
//! the device does, with one that reads every area before its first frame,
//! as a screenshot or a test does, both over the same stand-ins.

use std::collections::BTreeSet;

use crate::SettingsShell;
use crate::categories::Category;
use crate::effects::{LIVE_AREAS, defer_area_reads_in_tests, pane_row_id};
use crate::settings_app::SettingsApp;
use ui::framework::testing::TestApp;

/// A window that reads every area before its first frame.
fn reading_first() -> TestApp<SettingsShell> {
    TestApp::<SettingsShell>::new().expect("settings opens headlessly")
}

/// A window that reads its areas as one reaching the device does.
fn reading_after() -> TestApp<SettingsShell> {
    defer_area_reads_in_tests(true);
    let app = TestApp::<SettingsShell>::new();
    defer_area_reads_in_tests(false);
    app.expect("settings opens headlessly")
}

/// The areas that have read, by profiling name.
fn read(app: &mut TestApp<SettingsShell>) -> Vec<&'static str> {
    app.surface::<SettingsApp, _>(|settings| {
        LIVE_AREAS
            .iter()
            .filter(|area| (area.is_open)(settings))
            .map(|area| area.name)
            .collect()
    })
    .expect("the settings surface is mounted")
}

/// `resource` as inert data, children included: what agents, the palette
/// and accessibility read.
fn outline(resource: &tilo_actions::Resource) -> serde_json::Value {
    let mut own = resource.clone();
    own.children.clear();
    serde_json::json!({
        "id": resource.id.as_str(),
        "kind": resource.kind,
        "label": resource.label,
        "detail": resource.presentation.as_ref().and_then(|presentation| presentation.detail.clone()),
        "actions": tilo_actions::door::survey(&[own]),
        "views": resource.views.len(),
        "children": resource.children.iter().map(outline).collect::<Vec<_>>(),
    })
}

/// What the window publishes: its resource tree and every action on it.
fn published(app: &mut TestApp<SettingsShell>) -> (serde_json::Value, serde_json::Value) {
    let resources = app.harness().resources().iter().map(outline).collect();
    (resources, app.agent_surface())
}

/// Every node a screen reader is told about: its role, its name and
/// whether it accepts input.
fn controls(app: &mut TestApp<SettingsShell>) -> Vec<(String, Option<String>, bool)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .map(|(_, node)| {
            (
                format!("{:?}", node.role()),
                node.label().map(str::to_owned),
                node.is_disabled(),
            )
        })
        .collect()
}

/// Every resource id the live window publishes before its first frame:
/// what its first control-availability refresh built.
fn before_the_first_frame(deferred: bool) -> BTreeSet<String> {
    fn collect(resources: &[tilo_actions::Resource], ids: &mut BTreeSet<String>) {
        for resource in resources {
            ids.insert(resource.id.as_str().to_owned());
            collect(&resource.children, ids);
        }
    }
    defer_area_reads_in_tests(deferred);
    let ids = ui::framework::testing::inspect_live_window::<SettingsShell, _>(|app| {
        let mut ids = BTreeSet::new();
        collect(&app.collect_resources(), &mut ids);
        ids
    });
    defer_area_reads_in_tests(false);
    ids.expect("settings opens as a live window")
}

#[test]
fn the_first_refresh_reads_only_the_pane_shown() {
    let whole = before_the_first_frame(false);
    let first = before_the_first_frame(true);
    assert!(
        first.is_subset(&whole),
        "nothing is published that reading first does not: {:?}",
        first.difference(&whole).collect::<Vec<_>>()
    );
    for category in Category::ALL {
        assert!(
            first.contains(&pane_row_id(*category)),
            "{} opens from the first frame",
            category.slug()
        );
    }
    let put_off = whole.difference(&first).collect::<Vec<_>>();
    assert!(
        put_off.contains(&&"settings.display-output:eDP-1".to_owned()),
        "an area the pane shown does not read is put off: {put_off:?}"
    );
    // The first pane is Network: every row it shows is published at once.
    assert!(
        put_off
            .iter()
            .all(|id| !id.starts_with("settings.network") && !id.starts_with("settings.vpn")),
        "the pane shown publishes its rows from the first frame: {put_off:?}"
    );
}

#[test]
fn the_pane_shown_accepts_what_it_accepts_when_every_area_read_first() {
    let mut first = reading_first();
    let mut window = reading_after();
    assert_eq!(
        read(&mut first).len(),
        LIVE_AREAS.len(),
        "reading first reads every area before the first frame"
    );
    let shown = controls(&mut window);
    assert!(
        shown.iter().any(|(_, _, disabled)| !disabled),
        "the pane shown has controls that accept input"
    );
    assert_eq!(
        shown,
        controls(&mut first),
        "every control of the pane shown accepts what it accepts when every area read first"
    );
}

#[test]
fn a_window_publishes_every_area_a_few_frames_after_its_first() {
    let mut first = reading_first();
    first.settle();
    let whole = published(&mut first);
    let mut window = reading_after();
    assert_ne!(
        published(&mut window).0,
        whole.0,
        "the first frame publishes only the areas that have read"
    );
    let mut frames = 0;
    while read(&mut window).len() < LIVE_AREAS.len() {
        assert!(
            window.harness().poll_frame().expect("a frame lays out"),
            "a drawn window asks for its next area's read: {:?}",
            read(&mut window)
        );
        frames += 1;
        assert!(
            frames <= 2 * LIVE_AREAS.len(),
            "an area reads with every frame: {:?}",
            read(&mut window)
        );
    }
    window.settle();
    let settled = published(&mut window);
    assert_eq!(
        settled.0, whole.0,
        "the resource tree is the one reading first published"
    );
    assert_eq!(
        settled.1, whole.1,
        "every published action is the one reading first published"
    );
}

#[test]
fn an_action_on_a_row_of_an_area_not_yet_read_reaches_it() {
    let mut window = reading_after();
    assert!(!read(&mut window).contains(&"settings.rows.displays"));
    window
        .call_on(
            "settings.display-output:eDP-1",
            "set-output-scale",
            serde_json::json!({"scale": "200%"}),
        )
        .expect("an address an agent kept reaches the display before its area is published");
    window.settle();
    let staged = window
        .call("inspect-display-configuration", serde_json::json!({}))
        .expect("display observation is declared")
        .expect("observation returned");
    assert_eq!(
        staged["staged_configuration"]["outputs"]["eDP-1"]["scale"],
        2.0
    );
    assert!(read(&mut window).contains(&"settings.rows.displays"));
}
