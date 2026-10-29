//! The overview as a person, a screen reader and an agent meet it: where
//! the panel and the picture stand on each display, what keys and presses
//! ask the shell for, and what its actions do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_desktop_model::WorkspaceId;
use tilo_overview::{
    OverviewApp, OverviewEffect, OverviewPick, OverviewSeat, OverviewSurface, fixture,
};
use tilo_sheets::Seat as _;
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget as _;

fn staged(scenario: &str) -> HeadlessApp<OverviewApp> {
    let viewport = <OverviewApp as ui::framework::ResourceApp>::shot_viewport(Some(scenario));
    HeadlessApp::<OverviewApp>::capture_at(Some(scenario), viewport).unwrap()
}

fn targets(app: &mut HeadlessApp<OverviewApp>) -> Vec<(String, Rect)> {
    app.surface(|surface: &mut OverviewSurface| surface.test_rects())
        .unwrap()
}

fn target(app: &mut HeadlessApp<OverviewApp>, name: &str) -> Rect {
    targets(app)
        .into_iter()
        .find(|(shown, _)| shown == name)
        .map_or_else(|| panic!("no target named {name}"), |(_, rect)| rect)
}

fn has(app: &mut HeadlessApp<OverviewApp>, name: &str) -> bool {
    targets(app).iter().any(|(shown, _)| shown == name)
}

/// An overview the shell put up, lent the seat it reports to.
fn seated() -> (HeadlessApp<OverviewApp>, OverviewSeat) {
    let seat = OverviewSeat::default();
    seat.set_desk(fixture::desk());
    seat.presentation().open(true);
    let mut app = HeadlessApp::<OverviewApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize((1440, 900)).unwrap();
    app.settle();
    (app, seat)
}

fn key(app: &mut HeadlessApp<OverviewApp>, key: &str) {
    app.harness().key(key);
    app.settle();
}

#[test]
fn on_a_wide_display_the_picture_stands_beside_the_panel() {
    let mut app = staged("overview");
    let panel = target(&mut app, "overview");
    let review = target(&mut app, "overview-window:101");
    let terminal = target(&mut app, "overview-window:104");
    assert!(
        review.x1 <= panel.x0,
        "the picture is beside: {review:?} {panel:?}"
    );
    assert!(
        review.x1 < terminal.x0,
        "a strip's windows stand left to right"
    );
    let writing = target(&mut app, "overview-strip:11");
    let reading = target(&mut app, "overview-strip:12");
    assert!(
        writing.y1 <= review.y0 && review.y1 <= reading.y0,
        "strips stack under their names"
    );
    // The tabs of one window stand together, narrower beside its front one.
    let roadmap = target(&mut app, "overview-window:102");
    assert!(roadmap.width() < review.width());
    for name in [
        "overview_card[1]",
        "overview_card[3]",
        "add_workspace",
        "join_workspace",
    ] {
        let row = target(&mut app, name);
        assert!(panel.contains(row.center()), "{name} is in the panel");
    }
    assert!(
        has(&mut app, "overview-leave:3"),
        "a site's workspace can be left"
    );
}

#[test]
fn on_a_narrow_display_the_panel_holds_the_picture() {
    let mut app = staged("narrow");
    let panel = target(&mut app, "overview");
    let here = target(&mut app, "overview_card[0]");
    let review = target(&mut app, "overview-window:101");
    assert!(panel.contains(review.center()) && here.contains(review.center()));
    assert!(panel.x1 <= 900.0 + 0.5, "{panel:?}");
}

#[test]
fn on_a_small_display_the_overview_is_a_page() {
    let mut app = staged("page");
    let panel = target(&mut app, "overview");
    assert_eq!(panel, Rect::new(0.0, 0.0, 480.0, 640.0));
}

#[test]
fn a_crowded_desktop_scrolls_its_list_to_the_keyboard() {
    let mut app = staged("crowded");
    let panel = target(&mut app, "overview");
    let join = target(&mut app, "join_workspace");
    assert!(
        panel.contains(join.center()),
        "the last row was brought into view"
    );
    assert!(panel.height() <= 900.0);
}

#[test]
fn the_keyboard_walks_the_windows_and_enter_goes_to_one() {
    let (mut app, seat) = seated();
    key(&mut app, "ArrowRight");
    key(&mut app, "ArrowRight");
    key(&mut app, "Enter");
    assert_eq!(seat.take_asked(), vec![OverviewPick::Window(102)]);
    assert!(
        !seat.presentation().is_open(),
        "going somewhere leaves the overview"
    );
}

#[test]
fn escape_leaves_the_overview_where_it_was() {
    let (mut app, seat) = seated();
    key(&mut app, "Escape");
    assert!(seat.take_asked().is_empty());
    assert!(!seat.presentation().is_open());
}

#[test]
fn every_action_reaches_the_shell_as_what_it_asks() {
    let (mut app, seat) = seated();
    app.effect(&OverviewEffect::MoveWindow {
        window: "Week 44".into(),
        workspace: "Research".into(),
    })
    .unwrap();
    app.effect(&OverviewEffect::RenameStrip {
        strip: "Reading".into(),
        name: "Later".into(),
    })
    .unwrap();
    app.effect(&OverviewEffect::CloseWindow {
        window: "~/src/tilo".into(),
    })
    .unwrap();
    assert_eq!(
        seat.take_asked(),
        vec![
            OverviewPick::MoveWindow {
                tab: 106,
                to: WorkspaceId(2)
            },
            OverviewPick::RenameStrip {
                strip: fixture::READING,
                name: "Later".into()
            },
            OverviewPick::CloseWindow(104),
        ]
    );
    assert!(
        seat.presentation().is_open(),
        "editing the desktop keeps the overview up"
    );
    app.effect(&OverviewEffect::SwitchWorkspace {
        workspace: "research".into(),
    })
    .unwrap();
    assert_eq!(
        seat.take_asked(),
        vec![OverviewPick::Workspace(WorkspaceId(2))]
    );
    assert!(!seat.presentation().is_open());
    assert!(
        app.effect(&OverviewEffect::FocusWindow {
            window: "Atlantis".into()
        })
        .is_err(),
        "a window that is not there"
    );
    app.effect(&OverviewEffect::Open).unwrap();
    assert!(seat.take_open_asked());
}

#[test]
fn the_overview_names_every_window_it_pictures_for_the_compositor() {
    let (mut app, seat) = seated();
    app.settle();
    let named: Vec<u64> = seat.flight().pictures().iter().map(|p| p.tab).collect();
    assert_eq!(named, [101, 102, 103, 104, 105, 106]);
}

#[test]
fn a_screen_reader_hears_the_rooms_the_windows_and_the_strips() {
    let mut app = staged("overview");
    let names: Vec<String> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    for name in [
        "Research",
        "Design review.md",
        "Writing",
        "New workspace",
        "Join a workspace…",
    ] {
        assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
    }
}

#[test]
fn staged_moments_show_what_the_person_is_doing() {
    let mut app = staged("naming");
    assert!(
        app.surface(|s: &mut OverviewSurface| s.panel().is_naming())
            .unwrap()
    );
    let mut app = staged("keyboard");
    assert_eq!(
        app.surface(|s: &mut OverviewSurface| s.panel().window_selection())
            .unwrap(),
        Some(102)
    );
    let mut app = staged("carrying");
    let research = target(&mut app, "overview_card[1]");
    assert!(research.area() > 0.0);
}
