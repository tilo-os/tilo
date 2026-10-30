//! The banners as a person meets them, headless: where the stack, its
//! banners, the machine-wide banners and a call's edge sit in each scenario,
//! as a capture stages it, and what the published actions ask of the
//! desktop.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_desktop::notifications::BannerAsk;
use tilo_notification_banners::{
    Ask, BannerEffect, BannerService, BannerSurface, Command, Link, NotificationBanners, fixture,
};
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;
use ui::widget::Widget as _;

fn staged(scenario: &str) -> HeadlessApp<NotificationBanners> {
    let _settled = ui::motion::testing::settled();
    let viewport =
        <NotificationBanners as ui::framework::ResourceApp>::shot_viewport(Some(scenario));
    HeadlessApp::<NotificationBanners>::capture_at(Some(scenario), viewport).unwrap()
}

/// The banners on a desktop that lends them `scenario`'s feed.
fn hosted(scenario: &str) -> (HeadlessApp<NotificationBanners>, Link) {
    let link = Link::new();
    let viewport = fixture::viewport(Some(scenario));
    link.publish(fixture::feed(Some(scenario)));
    link.place(fixture::placement(
        Some(scenario),
        (f64::from(viewport.0), f64::from(viewport.1)),
    ));
    let mut app = HeadlessApp::<NotificationBanners>::for_person_with_lent(
        "person",
        BannerService { link: link.clone() },
    )
    .unwrap();
    app.harness().resize(viewport).unwrap();
    app.settle();
    (app, link)
}

fn targets(app: &mut HeadlessApp<NotificationBanners>) -> Vec<(String, Rect)> {
    let mut targets = app
        .surface(|surface: &mut BannerSurface| surface.test_rects())
        .unwrap();
    targets.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    targets
}

fn named(app: &mut HeadlessApp<NotificationBanners>, prefix: &str) -> Vec<Rect> {
    targets(app)
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(_, rect)| rect)
        .collect()
}

#[test]
fn banners_stack_in_the_top_right_corner_newest_first() {
    let mut app = staged("banners");
    let banners = named(&mut app, "banner:");
    assert_eq!(banners.len(), 3, "{banners:?}");
    let (width, _) = fixture::viewport(Some("banners"));
    for banner in &banners {
        assert!(banner.x1 <= f64::from(width) - 20.0, "{banner:?} inset");
        assert!(
            banner.x0 > f64::from(width) / 2.0,
            "{banner:?} on the right"
        );
    }
    assert!(banners[0].y0 >= 32.0, "under the top bar");
    let stack = named(&mut app, "banners-stack")[0];
    assert!(banners.iter().all(|banner| stack.contains(banner.center())));
}

#[test]
fn a_window_bar_under_the_corner_pushes_the_stack_below_it() {
    let mut app = staged("bars");
    let first = named(&mut app, "banner:")[0];
    let placement = fixture::placement(Some("bars"), (1280.0, 800.0));
    for bar in &placement.window_bars {
        assert!(
            bar.intersect(first).area() <= 0.0,
            "{first:?} covers {bar:?}"
        );
    }
}

#[test]
fn banners_that_do_not_fit_fold_into_a_card() {
    let mut app = staged("more");
    let shown = named(&mut app, "banner:").len();
    assert!(shown > 0 && shown < 6, "{shown} of 6 fit");
}

#[test]
fn a_locked_session_shows_a_count_and_no_banner() {
    let mut app = staged("locked");
    assert!(named(&mut app, "banner:").is_empty());
    assert_eq!(named(&mut app, "banners-stack").len(), 1, "the count card");
}

#[test]
fn the_machine_repeats_its_banners_and_a_ringing_call_edges_the_other_display() {
    let mut app = staged("machine");
    let machine = named(&mut app, "machine-banner:");
    assert_eq!(machine.len(), 1, "the battery banner repeats");
    assert!(machine[0].x0 >= 1280.0, "on the other display");
    let edges = named(&mut app, "call-edge:");
    assert_eq!(edges, [Rect::new(1280.0, 0.0, 2560.0, 800.0)]);
    assert!(
        named(&mut app, "banner:")
            .iter()
            .all(|banner| banner.x1 <= 1280.0),
        "the stack stays on the display in use"
    );
}

#[test]
fn nothing_shows_with_no_banner() {
    let mut app = staged("empty");
    assert!(targets(&mut app).is_empty());
}

#[test]
fn a_click_on_a_banner_opens_it_on_the_desktop() {
    let (mut app, link) = hosted("banners");
    let banner = link.shown().banners[0];
    app.harness()
        .click_at(banner.1.center().x, banner.1.center().y);
    app.settle();
    assert_eq!(
        link.take_asks()
            .into_iter()
            .filter(|ask| !matches!(ask, Ask::Banner(BannerAsk::Present { .. })))
            .collect::<Vec<_>>(),
        [Ask::Banner(BannerAsk::Activate(banner.0))]
    );
}

#[test]
fn the_chord_moves_focus_into_the_newest_banner_and_escape_closes_it() {
    let (mut app, link) = hosted("banners");
    let newest = link.shown().banners[0].0;
    assert!(link.shown().focusable);
    link.command(Command::FocusNewest);
    app.settle();
    assert!(link.shown().keyboard, "focus is in the banners");
    link.command(Command::Key {
        key: ui::winit::keyboard::Key::Named(ui::winit::keyboard::NamedKey::Escape),
        shift: false,
    });
    app.settle();
    assert!(!link.shown().keyboard, "acting gives focus back");
    assert!(
        link.take_asks()
            .contains(&Ask::Banner(BannerAsk::Dismiss(newest))),
        "Escape closes the focused banner"
    );
}

#[test]
fn published_actions_reach_the_desktop() {
    let (mut app, link) = hosted("call");
    let call = link.shown().banners[0].0;
    app.effect(&BannerEffect::AnswerCall { banner: None })
        .unwrap();
    assert!(link.take_asks().contains(&Ask::AnswerCall(call)));
    assert!(
        app.effect(&BannerEffect::Snooze { banner: None }).is_err(),
        "nothing on screen snoozes"
    );
}

#[test]
fn assistive_technology_reads_a_live_region_with_the_newest_banner() {
    let (_app, link) = hosted("banners");
    let node = link.shown().accessibility.expect("a live region");
    assert!(node.live.is_some());
    assert_eq!(
        node.value.as_deref(),
        Some("Ada Lovelace. Can you look at the review?")
    );
}
