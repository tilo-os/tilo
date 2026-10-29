//! The bar as a person meets it, headless: where each display's bar, its
//! controls and its menus sit in each scenario, as a capture stages it, and
//! what clicks and the published actions ask of the desktop.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard; geometry is compared exactly"
)]

use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::workspace_menu::WorkspaceMenuChoice;
use tilo_topbar::{
    Ask, BarService, Command, Link, MediaCommand, TopBarApp, TopBarEffect, TopBarSurface, fixture,
};
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget as _;

fn staged(scenario: &str) -> HeadlessApp<TopBarApp> {
    let _settled = ui::motion::testing::settled();
    let viewport = <TopBarApp as ui::framework::ResourceApp>::shot_viewport(Some(scenario));
    HeadlessApp::<TopBarApp>::capture_at(Some(scenario), viewport).unwrap()
}

/// The bar on a desktop that lends it `scenario`'s feed.
fn hosted(scenario: &str) -> (HeadlessApp<TopBarApp>, Link) {
    let link = Link::new();
    let viewport = fixture::viewport(Some(scenario));
    link.publish(fixture::feed(Some(scenario)));
    link.place(fixture::placement(
        Some(scenario),
        (f64::from(viewport.0), f64::from(viewport.1)),
    ));
    let mut app =
        HeadlessApp::<TopBarApp>::for_person_with_lent("person", BarService { link: link.clone() })
            .unwrap();
    app.harness().resize(viewport).unwrap();
    app.settle();
    (app, link)
}

fn named(app: &mut HeadlessApp<TopBarApp>, name: &str) -> Option<Rect> {
    app.surface(|surface: &mut TopBarSurface| surface.test_rects())
        .unwrap()
        .into_iter()
        .find(|(target, _)| target == name)
        .map(|(_, rect)| rect)
}

#[test]
fn the_bar_runs_along_the_top_of_the_display() {
    let mut app = staged("bar");
    let bar = named(&mut app, "topbar").expect("the full bar");
    let (width, _) = fixture::viewport(Some("bar"));
    assert_eq!(bar.y0, 0.0);
    assert_eq!(bar.width(), f64::from(width));
    assert!(
        (bar.height() - f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX)).abs() < 1.0,
        "{bar:?}"
    );
    for control in [
        "topbar_workspace_pill",
        "topbar_tray_pill",
        "topbar_center_clock",
    ] {
        let rect = named(&mut app, control).unwrap_or_else(|| panic!("{control} is drawn"));
        assert!(bar.contains(rect.center()), "{control} {rect:?} in {bar:?}");
    }
}

#[test]
fn the_workspace_menu_hangs_under_the_workspace_name() {
    let mut app = staged("menu");
    let menu = named(&mut app, "workspace_menu").expect("the menu is open");
    let bar = named(&mut app, "topbar").unwrap();
    assert!(menu.y0 >= bar.y1 - 1.0, "{menu:?} under {bar:?}");
    let labels = app
        .surface(|surface: &mut TopBarSurface| surface.workspace_menu_labels())
        .unwrap()
        .expect("rows");
    let labels: Vec<String> = labels.into_iter().flatten().collect();
    assert!(labels.contains(&"Studio".to_owned()), "{labels:?}");
    assert!(labels.contains(&"Home".to_owned()), "{labels:?}");
}

#[test]
fn the_profile_card_opens_under_the_own_avatar() {
    let mut app = staged("profile");
    assert!(
        app.surface(|surface: &mut TopBarSurface| surface.profile_open())
            .unwrap()
    );
    let card = named(&mut app, "profile_card").expect("the card is drawn");
    let bar = named(&mut app, "topbar").unwrap();
    assert!(card.y0 >= bar.y1 - 1.0, "{card:?} under {bar:?}");
}

#[test]
fn every_other_display_carries_a_compact_bar() {
    let mut app = staged("displays");
    let full = named(&mut app, "topbar").expect("the full bar");
    let compact = named(&mut app, "display:2:topbar").expect("the compact bar");
    assert!(full.x1 <= 1280.0 + 0.5, "{full:?} on the first display");
    assert!(compact.x0 >= 1280.0 - 0.5, "{compact:?} on the second");
    assert_eq!(full.y0, compact.y0);
}

#[test]
fn choosing_a_workspace_in_the_menu_asks_the_desktop() {
    let (mut app, link) = hosted("bar");
    app.effect(&TopBarEffect::WorkspaceMenu).unwrap();
    app.settle();
    assert!(link.shown().modal, "the menu takes the next press");
    let chosen = app
        .surface(|surface: &mut TopBarSurface| surface.choose_in_workspace_menu("Home"))
        .unwrap();
    assert!(chosen);
    let asks = link.take_asks();
    assert!(asks.contains(&Ask::Opened), "{asks:?}");
    assert!(
        asks.iter()
            .any(|ask| matches!(ask, Ask::WorkspaceMenu(WorkspaceMenuChoice::Switch(_)))),
        "{asks:?}"
    );
}

#[test]
fn a_click_on_the_status_cluster_opens_quick_settings() {
    let (mut app, link) = hosted("bar");
    let tray = link.shown().tray;
    assert!(!tray.is_zero_area());
    app.harness().click_at(tray.center().x, tray.center().y);
    app.settle();
    assert!(link.take_asks().iter().any(|ask| matches!(
        ask,
        Ask::Chrome(ChromeIntent::ToggleQuickSettings | ChromeIntent::ShowQuickSettings)
    )));
}

#[test]
fn published_actions_reach_the_desktop() {
    let (mut app, link) = hosted("bar");
    app.effect(&TopBarEffect::Notifications).unwrap();
    app.effect(&TopBarEffect::NextTrack).unwrap();
    app.effect(&TopBarEffect::ShowStrips).unwrap();
    assert_eq!(
        link.take_asks(),
        [
            Ask::Notifications,
            Ask::Media(MediaCommand::Next),
            Ask::Chrome(ChromeIntent::ShowStrips),
        ]
    );
    assert!(
        app.effect(&TopBarEffect::ActivateTrayItem {
            item: "nothing".into()
        })
        .is_err(),
        "no tray item by that name"
    );
}

#[test]
fn the_desktop_moves_keyboard_focus_into_the_bar_and_back() {
    let (mut app, link) = hosted("bar");
    link.command(Command::Focus);
    app.settle();
    let shown = link.shown();
    assert!(shown.keyboard, "focus is in the bar");
    assert!(
        shown.holds_shown,
        "a hidden bar stays shown while it has the keyboard"
    );
    link.command(Command::ReleaseKeyboard);
    app.settle();
    assert!(!link.shown().keyboard);
}
