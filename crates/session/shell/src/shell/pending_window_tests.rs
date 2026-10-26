//! The place a window holds before it maps: nothing of its own, the
//! framework's arrival of the window's first frame in the tab it holds.

use super::*;
use ui::arriving::{GRACE, NAME_SOURCE_AFTER};
use ui::time::{Duration, Instant};
use ui::widget::{TileContent, TileHost};

const PACKAGE: &str = "so.tilo.test.pending-window";

fn offered_by_registry() {
    catalogue::replace_for_test(vec![catalogue::CatalogueApp {
        package_id: PACKAGE.into(),
        display_name: "Slides".into(),
        download_url: "https://apps.tilo.coffee/v1/so.tilo.slides/latest.tilo".into(),
        ..catalogue::CatalogueApp::default()
    }]);
}

fn installing() -> PendingWindow {
    PendingWindow::new(
        "Slides",
        Wait::Install {
            package_id: PACKAGE.into(),
        },
    )
}

#[test]
fn a_start_is_a_window_of_this_device_that_never_presents_and_draws_nothing() {
    let mut place = PendingWindow::new("Notes", Wait::Start);
    place.rect = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut scene = Scene::new();
    place.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::default(),
    );
    assert!(
        scene.encoding().is_empty(),
        "the arrival stands here, not a card"
    );
    assert!(!place.presented());
    assert_eq!(place.arrives_from(), Source::Local);
    assert_eq!(place.arrival_progress(), None);
    assert!(!place.needs_animation_frame());
    assert_eq!(place.next_wake_deadline(), None);
}

#[test]
fn an_install_comes_from_its_registry_and_says_how_much_of_it_arrived() {
    offered_by_registry();
    let place = installing();
    assert_eq!(place.arrives_from(), Source::host("apps.tilo.coffee"));
    assert_eq!(place.arrival_progress(), None, "nothing measured yet");

    catalogue::set_install_progress(
        PACKAGE,
        Some(InstallProgress::Downloading {
            received: 4_200_000,
            total: Some(18_000_000),
        }),
    );
    assert_eq!(
        place.arrival_progress(),
        Some(Progress::new(4_200_000, Some(18_000_000)))
    );
    catalogue::set_install_progress(PACKAGE, Some(InstallProgress::Installing));
    assert_eq!(
        place.arrival_progress(),
        Some(Progress::new(18_000_000, Some(18_000_000))),
        "the bytes are in: it stands full while the app installs"
    );
    catalogue::set_install_progress(PACKAGE, None);
    assert_eq!(
        place.arrival_progress(),
        Some(Progress::new(18_000_000, Some(18_000_000))),
        "and stays until the window takes the tab"
    );
}

#[test]
fn an_app_updated_before_it_starts_comes_from_its_registry_too() {
    offered_by_registry();
    let place = PendingWindow::new("Slides", Wait::Start).for_package(PACKAGE);
    assert_eq!(place.arrives_from(), Source::Local);
    crate::shell::app_versions::mark_updating_for_test(PACKAGE, true);
    assert_eq!(place.arrives_from(), Source::host("apps.tilo.coffee"));
    crate::shell::app_versions::mark_updating_for_test(PACKAGE, false);
}

#[test]
fn its_tab_shows_the_arrival_naming_the_registry_and_the_amount() {
    offered_by_registry();
    catalogue::set_install_progress(
        PACKAGE,
        Some(InstallProgress::Downloading {
            received: 4_200_000,
            total: Some(18_000_000),
        }),
    );
    let t0 = Instant::now();
    let at = |after: Duration, f: &mut dyn FnMut()| {
        ui::time::testing::with_monotonic_time(t0 + after, f);
    };
    let mut host = None;
    at(Duration::ZERO, &mut || {
        host = Some(TileHost::new(TileContent::App(Box::new(installing()))));
    });
    let host = host.expect("the tab's host");
    at(GRACE / 2, &mut || {
        assert!(host.is_arriving(), "the place is reserved");
        assert!(
            host.holds_first_frame_until(ui::time::now()).is_none(),
            "a window from elsewhere reserves its place at once"
        );
    });
    at(NAME_SOURCE_AFTER + GRACE, &mut || {
        let node = host.accessibility().expect("the arrival is announced");
        assert!(node.state.busy);
        let said = node.description.unwrap_or_default();
        assert!(
            said.contains("apps.tilo.coffee"),
            "names the registry: {said}"
        );
        assert!(said.contains("4.2 MB"), "says how much came: {said}");
    });
    catalogue::set_install_progress(PACKAGE, None);
}
