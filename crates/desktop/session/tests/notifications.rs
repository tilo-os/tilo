//! An app's notification reaches the desktop through the session, on the socket the launcher
//! opens for it, and what the person does with it comes back to the app.
//!
//! The app here is the production client ([`tilo_notify::service::ServiceNotifier`]) on a socket
//! the real session bound, named as the launcher names an app; the person is the shell's own
//! banner stack ([`NotificationHost::invoke_action`]), the same call a click on the banner makes.
//! Nothing is faked between them: what the app sends is a banner in the session's notification
//! log, and the click is answered into the app's own activation handler.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use tilo_compositor::OutputConfig;
use tilo_notify::Notification as AppNotification;
use tilo_notify::Notifier as _;
use tilo_notify::service::ServiceNotifier;
use tilo_session::{Options, Session};
use tilo_session_core::desktop::notifications::NotificationHost as _;

/// The app, as the launcher names it on its connection.
const APP: &str = "so.tilo.mail";
/// The URI its notification opens.
const ACTIVATION: &str = "tilo-notify-session-test:thread/1";

/// A headless session serving its own services, and the app's connection to its notifications.
fn session() -> (Session, ServiceNotifier, tilo_test_env::TestHome) {
    let home = tilo_test_env::TestHome::new().expect("a home of this test's own");
    let session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
        Options::default(),
    );
    let path = launcher::services::socket_for(launcher::services::NOTIFY_SERVICE);
    assert!(
        path.exists(),
        "the session serves notifications at {}",
        path.display()
    );
    let mut stream = tilo_socket::connect(&path).expect("the notification socket");
    runtime_portal::session_services::write_app_id(&mut stream, APP)
        .expect("the launcher's hand-off");
    let app = ServiceNotifier::connect(stream).expect("the app's end of the connection");
    (session, app, home)
}

#[test]
fn an_apps_notification_is_a_banner_and_the_persons_choice_comes_back() {
    let _settled = ui::motion::testing::settled();
    let (mut session, app, _home) = session();
    let (chosen, answers) = std::sync::mpsc::channel();
    tilo_notify::set_activation_handler("tilo-notify-session-test", move |uri| {
        let _ = chosen.send(uri.to_owned());
    });

    app.show(
        &AppNotification::new("new-mail", "Ada wrote", "See you at nine")
            .with_activation(ACTIVATION),
    )
    .expect("the session took it");
    let banner = |session: &mut Session| {
        let notifications = session.notifications();
        let log = notifications.log.log.borrow();
        log.active()
            .iter()
            .chain(log.history())
            .find(|banner| banner.title == "Ada wrote")
            .map(|banner| banner.id)
    };
    let mut id = None;
    crate::common::until(&mut session, "the notification's banner", |session| {
        id = banner(session);
        id.is_some()
    });
    let id = id.expect("the banner's id");

    let (gone, closings) = std::sync::mpsc::channel();
    tilo_notify::set_close_handler("new-mail", move |tag, reason| {
        let _ = gone.send((tag.to_owned(), reason));
    });

    // The person clicks it: the shell's own answer, which the session hands to the app that
    // showed it. The session's own passes carry it there.
    session.notifications().log.invoke_action(id, "default");
    let answered = crate::common::wait_for(&mut session, "the app hearing its choice", |_| {
        answers.try_recv().ok()
    });
    assert_eq!(
        answered, ACTIVATION,
        "the app is opened at what it showed the notification with"
    );
    // And the app hears that it is gone, as the session closes what the person dealt with.
    let closed = crate::common::wait_for(&mut session, "the app hearing it is gone", |_| {
        closings.try_recv().ok()
    });
    assert_eq!(
        closed,
        ("new-mail".to_owned(), tilo_notify::CloseReason::Dismissed)
    );
}
