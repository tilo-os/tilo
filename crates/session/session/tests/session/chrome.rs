//! The bar's own chrome: what an app's notification shows there, and the
//! clock that Clocks' live activity feeds.

use super::*;

/// Send one notification over the freedesktop interface on `address`, as an
/// app of the session would, and name the id the daemon gave it. The
/// connection is the app's: it stays open until the caller drops it.
async fn notify(address: &str, summary: &str, body: &str, timeout: i32) -> (zbus::Connection, u32) {
    let app = zbus::connection::Builder::address(address)
        .unwrap()
        .build()
        .await
        .unwrap();
    let proxy = zbus::Proxy::new(
        &app,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    .unwrap();
    let reply = proxy
        .call_method(
            "Notify",
            &(
                "Mail",
                0_u32,
                "",
                summary,
                body,
                Vec::<&str>::new(),
                std::collections::HashMap::<String, zbus::zvariant::OwnedValue>::new(),
                timeout,
            ),
        )
        .await
        .unwrap();
    (app, reply.body().deserialize().unwrap())
}

/// An app's notification over the freedesktop interface, from the process
/// whose window the session shows: a banner and a centre entry named after
/// that app, and the bar's bell counting it.
#[test]
fn an_apps_notification_is_shown_named_after_its_app_and_counted_by_the_bell() {
    let _settled = ui::motion::testing::settled();
    let bus = PrivateBus::start();
    let wake = tilo_service::LoopWake::default();
    let (side, link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::Address(bus.address().to_owned()),
                system: Bus::None,
            },
            &wake.handle(),
            std::sync::Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .expect("desktop services"),
    );
    let portal = desktop
        .portal()
        .map(|serving| link.with_captures(serving.input_captures().clone()));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: desktop.services(),
            wake,
            desktop: Some(Rc::clone(&desktop)),
            portal,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");

    // The app the session knows by its window; the sender's process is the
    // one that window's client runs in.
    let mut client = mapped(&mut session, "so.tilo.mail");

    let runtime = desktop.runtime();
    // It waits for the person: nothing here measures the banner running out.
    let (app, id) = runtime.block_on(notify(bus.address(), "New mail", "From Ada", 0));

    pump(&mut session, &mut [&mut client], |session, _| {
        session.notifications().unread() == 1
    });
    let centre = session.notifications();
    let banners = centre.log();
    assert_eq!(banners.active().len(), 1, "the notification is a banner");
    let banner = banners.active()[0].clone();
    drop(banners);
    assert_eq!(banner.title, "New mail");
    assert_eq!(
        banner.app_id.as_deref(),
        Some("so.tilo.mail"),
        "named after the app whose window sent it"
    );
    assert_eq!(banner.sender_id, Some(id));

    // The bar's bell counts what is unread.
    let bell = session.accessible_labels();
    assert!(
        bell.iter().any(|label| label == "Notifications, 1 unread"),
        "the bell says what is unread: {bell:?}"
    );

    // The person closes the banner on its own control: it is kept in the
    // centre, named after the same app.
    let dismiss = until(&mut session, &mut client, |session| {
        session
            .banners()
            .shown()
            .accessibility
            .as_ref()
            .and_then(|node| control(node, "Dismiss"))
    })
    .expect("the banner offers a way out");
    session
        .banners()
        .command(tilo_notification_banners::Command::Activate(dismiss));
    pump(&mut session, &mut [&mut client], |session, _| {
        session.notifications().log().active().is_empty()
    });
    let centre = session.notifications();
    let history = centre.log();
    let kept = history
        .history()
        .iter()
        .find(|shown| shown.title == "New mail")
        .expect("the centre keeps it");
    assert_eq!(kept.app_id.as_deref(), Some("so.tilo.mail"));
    assert_eq!(centre.unread(), 1, "and it stays unread until read");
    drop(app);
}

/// Clocks' live activity is the bar's clock: a running timer shows there,
/// floating over the windows, and a timer coming due is announced once.
#[test]
fn a_running_timer_is_the_bars_clock() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    session.step(Duration::from_millis(1)).unwrap();
    let before = session.accessible_labels();
    session.live_activities().show(
        "so.tilo.clocks",
        Some(serde_json::json!({
            "active": true,
            "activity_id": "timer-1",
            "label": "Tea",
            "remaining_secs": 90,
            "stopwatch": false,
            "overlay": true,
            "notification_id": "due-1",
            "notification_title": "Bread",
            "notification_body": "Out of the oven",
        })),
    );
    session.step(Duration::from_millis(1)).unwrap();
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Tea"),
        "the bar's floating clock names the timer: {labels:?} was {before:?}"
    );

    // Its due time is announced under the app's own words, once.
    let announcements = |session: &Session| {
        session
            .notifications()
            .log()
            .active()
            .iter()
            .map(|notification| (notification.title.clone(), notification.body.clone()))
            .collect::<Vec<_>>()
    };
    let announced = announcements(&session);
    assert_eq!(announced.len(), 1, "one announcement");
    assert_eq!(announced[0].0, "Bread");
    assert_eq!(announced[0].1, "Out of the oven");
    session.step(Duration::from_millis(1)).unwrap();
    assert_eq!(
        announcements(&session).len(),
        1,
        "reading it again announces nothing more"
    );

    // The timer ends: the bar's clock is bare again.
    session.live_activities().show("so.tilo.clocks", None);
    session.step(Duration::from_millis(1)).unwrap();
    let labels = session.accessible_labels();
    assert!(!labels.iter().any(|label| label == "Tea"), "{labels:?}");
}
