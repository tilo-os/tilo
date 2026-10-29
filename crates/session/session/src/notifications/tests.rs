#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::{Arc, Mutex};

use tilo_notifications_service::{Caller, Occurrence, Taken};

use super::*;

fn sent(id: u32) -> tilo_notifications_service::Notification {
    let mut notification = tilo_notifications_service::Notification::new(id);
    notification.app_name = "Mail".into();
    notification.summary = "New mail".into();
    notification.body = "From Ada".into();
    notification.expire_timeout = -1;
    notification.urgency = 1;
    notification
}

fn pixels() -> ImageData {
    ImageData {
        width: 2,
        height: 1,
        rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
    }
}

/// What the app hears back: its actions and replies, and its closes.
#[derive(Default)]
struct Heard {
    taken: Mutex<Vec<(u32, String)>>,
}

struct Rig {
    daemon: Notifications,
    desktop: DesktopNotifications,
    bridge: SessionNotifications,
    heard: Arc<Heard>,
}

fn rig() -> Rig {
    let daemon = Notifications::new();
    let heard = Arc::new(Heard::default());
    let tap = Arc::clone(&heard);
    daemon.on_taken(move |id, taken| {
        let said = match taken {
            Taken::Action { key, .. } => format!("action {key}"),
            Taken::Reply { text } => format!("reply {text}"),
        };
        tap.taken.lock().unwrap().push((id, said));
    });
    let desktop = DesktopNotifications::default();
    desktop.show_banners_elsewhere();
    let bridge = SessionNotifications::new(Some(daemon.clone()), desktop.clone(), Link::new());
    Rig {
        daemon,
        desktop,
        bridge,
        heard,
    }
}

impl Rig {
    /// What the person did on the banners app, as it asks it.
    fn ask(&self, ask: Ask) {
        self.bridge.ask(ask);
    }

    /// What the banners were last handed: the desktop's live banners, once
    /// the bridge published them.
    fn feed(&self) -> tilo_desktop_surface::notifications::BannerFeed {
        assert_eq!(
            self.bridge.published,
            Some(self.desktop.revision()),
            "the banners have the latest"
        );
        self.desktop.feed()
    }

    fn heard(&self) -> Vec<(u32, String)> {
        std::mem::take(&mut *self.heard.taken.lock().unwrap())
    }

    fn live(&self, id: u32) -> bool {
        self.daemon.notification(id).is_some()
    }

    fn banner(&self) -> Notification {
        self.desktop.log().active()[0].clone()
    }

    fn history(&self) -> Vec<String> {
        self.desktop
            .log()
            .history()
            .iter()
            .map(|notification| notification.title.clone())
            .collect()
    }
}

#[test]
fn urgency_and_timeout_set_how_long_a_banner_stays() {
    let critical = {
        let mut critical = sent(1);
        critical.urgency = 2;
        critical.expire_timeout = 1000;
        shown(critical, 0)
    };
    assert_eq!(critical.severity, NotificationSeverity::Error);
    assert_eq!(critical.dismiss_after, PERSISTENT_DISMISS);
    for (timeout, stays) in [
        (-1, DEFAULT_DISMISS_AFTER),
        (0, PERSISTENT_DISMISS),
        (2500, Duration::from_millis(2500)),
    ] {
        let mut normal = sent(1);
        normal.expire_timeout = timeout;
        let normal = shown(normal, 0);
        assert_eq!(normal.severity, NotificationSeverity::Info);
        assert_eq!(normal.dismiss_after, stays, "timeout {timeout}");
    }
}

#[test]
fn only_an_attributed_app_is_named_and_a_person_needs_a_name() {
    let unattributed = shown(sent(3), 0);
    assert_eq!(unattributed.sender_id, Some(3));
    assert_eq!(unattributed.app_id, None);
    let claim = unattributed.app_name.expect("the sender's claim");
    assert!(
        claim.contains("Mail") && claim != "Mail",
        "shown as a claim: {claim}"
    );

    let mut message = sent(4);
    message.caller = Some(Caller {
        app_id: "so.tilo.chat".into(),
        workspace: None,
    });
    message.contact_endpoint = Some("mailto:ada@example.org".into());
    message.incoming_call = true;
    let handle_only = shown(message.clone(), 0);
    assert_eq!(handle_only.app_name.as_deref(), Some("so.tilo.chat"));
    assert_eq!(handle_only.app_id.as_deref(), Some("so.tilo.chat"));
    assert_eq!(handle_only.from, None, "a handle alone names no one");
    assert_eq!(handle_only.addressed, None, "a call needs its caller");

    message.sender_name = Some("Ada".into());
    let named = shown(message, 0);
    assert_eq!(
        named.from,
        Some(NotificationPerson {
            name: "Ada".into(),
            identity: "mailto:ada@example.org".into(),
        })
    );
    assert_eq!(named.addressed, Some(Addressed::IncomingCall));
    assert!(!named.bypass_dnd, "an app never pierces Do Not Disturb");
}

#[test]
fn the_senders_pixels_are_the_banners_artwork() {
    let mut with_image = sent(5);
    with_image.image_data = Some(pixels());
    let image = shown(with_image, 7).app_image.expect("artwork");
    assert_eq!(image.key(), "notification:5:7");
    let painted = image.pixels(64).expect("pixels");
    assert_eq!((painted.width, painted.height), (2, 1));
    assert_eq!(painted.data.data(), pixels().rgba.as_slice());
    assert!(shown(sent(5), 7).app_image.is_none());
}

#[test]
fn actions_are_labelled_by_the_desktop_where_it_owns_the_words() {
    let mut alarm = sent(6);
    alarm.actions = vec![
        ("default".into(), "Go".into()),
        ("snooze".into(), "zzz".into()),
        ("archive".into(), "Archive".into()),
    ];
    let plain = shown(alarm.clone(), 0);
    assert_eq!(plain.actions[0].1, notifications::open_action_label());
    assert_eq!(plain.actions[1].1, "zzz", "only an alarm's are relabelled");
    alarm.scheduled = Some(Occurrence {
        room: "local".into(),
        token: "t1".into(),
        occurrence_ms: 1_000,
        calendar_alarm: true,
        uri: "tilo://clock".into(),
    });
    let ringing = shown(alarm, 0);
    let snooze = notifications::alarm_action_labels()
        .into_iter()
        .find(|(key, _)| key == "snooze")
        .unwrap()
        .1;
    assert_eq!(ringing.actions[1].1, snooze);
    assert_eq!(ringing.actions[2].1, "Archive");
    let target = ringing.scheduled_target.expect("its deadline");
    assert_eq!((target.token.as_str(), target.occurrence_ms), ("t1", 1_000));
    assert!(target.calendar_alarm);
}

#[test]
fn an_arrival_with_pixels_reaches_the_banners_and_the_centre() {
    let mut rig = rig();
    let id = rig.daemon.alloc_id();
    let mut notification = sent(id);
    notification.image_data = Some(pixels());
    rig.daemon.push(notification);
    rig.bridge.step();

    let feed = rig.feed();
    assert_eq!(feed.banners.len(), 1);
    let banner = &feed.banners[0].notification;
    assert_eq!(banner.title, "New mail");
    assert!(banner.app_image.is_some(), "its pixels are on the banner");

    // Closed by the person, it stays in the centre with its artwork.
    rig.ask(Ask::Banner(BannerAsk::Dismiss(banner.id)));
    rig.bridge.step();
    assert!(rig.feed().banners.is_empty());
    assert_eq!(rig.history(), ["New mail"]);
    assert!(rig.desktop.log().history()[0].app_image.is_some());
    assert!(!rig.live(id), "the app is told it was dismissed");
}

#[test]
fn a_banners_action_and_reply_reach_the_app() {
    let mut rig = rig();
    let id = rig.daemon.alloc_id();
    let mut notification = sent(id);
    notification.actions = vec![
        ("archive".into(), "Archive".into()),
        (
            tilo_notifications_service::INLINE_REPLY.into(),
            "Reply".into(),
        ),
    ];
    rig.daemon.push(notification.clone());
    rig.bridge.step();
    let banner = rig.banner().id;
    rig.ask(Ask::Banner(BannerAsk::Invoke(banner, "archive".into())));
    rig.bridge.step();
    assert_eq!(rig.heard(), [(id, "action archive".into())]);
    assert!(!rig.live(id), "a chosen banner is closed");

    let id = rig.daemon.alloc_id();
    notification.id = id;
    rig.daemon.push(notification);
    rig.bridge.step();
    let banner = rig.banner().id;
    rig.ask(Ask::Reply {
        id: banner,
        text: "on my way".into(),
    });
    rig.bridge.step();
    assert_eq!(rig.heard(), [(id, "reply on my way".into())]);
    assert!(!rig.live(id));
}

#[test]
fn a_call_is_answered_by_the_callers_own_choice() {
    let mut rig = rig();
    let id = rig.daemon.alloc_id();
    let mut call = sent(id);
    call.actions = vec![
        ("default".into(), "Open".into()),
        ("decline".into(), "Decline".into()),
    ];
    rig.daemon.push(call);
    rig.bridge.step();
    rig.ask(Ask::DeclineCall(rig.banner().id));
    rig.bridge.step();
    assert_eq!(rig.heard(), [(id, "action decline".into())]);
}

#[test]
fn a_replacement_takes_the_banners_place_and_a_withdrawal_takes_it_away() {
    let mut rig = rig();
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.bridge.step();
    let mut replaced = sent(id);
    replaced.summary = "Two new mails".into();
    rig.daemon.push(replaced);
    rig.bridge.step();
    let feed = rig.feed();
    assert_eq!(feed.banners.len(), 1);
    assert_eq!(feed.banners[0].notification.title, "Two new mails");
    assert!(rig.live(id), "a replacement is no close");

    assert!(rig.daemon.request_close(id));
    rig.bridge.step();
    assert!(rig.feed().banners.is_empty());
    assert!(rig.history().is_empty(), "the app took it back");
}

#[test]
fn a_transient_notification_leaves_no_trace_in_the_centre() {
    let mut rig = rig();
    let id = rig.daemon.alloc_id();
    let mut transient = sent(id);
    transient.transient = true;
    rig.daemon.push(transient);
    rig.bridge.step();
    rig.ask(Ask::Banner(BannerAsk::Dismiss(rig.banner().id)));
    rig.bridge.step();
    assert!(rig.history().is_empty());
    assert!(!rig.live(id));
}

#[test]
fn read_in_full_moves_it_to_the_centre() {
    let mut rig = rig();
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.bridge.step();
    rig.ask(Ask::Read(rig.banner().id));
    rig.bridge.step();
    assert!(rig.feed().banners.is_empty());
    assert_eq!(rig.history(), ["New mail"]);
}
