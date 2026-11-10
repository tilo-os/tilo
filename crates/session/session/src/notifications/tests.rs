#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::{Arc, Mutex};

use settings_store::notification_policy::AppNotificationRule;
use settings_store::system_sounds::{SystemSoundEvent, SystemSoundPrefs, should_play};
use tilo_desktop_surface::notifications::{
    DEFAULT_DISMISS_AFTER, NotificationSeverity, PERSISTENT_DISMISS,
};
use tilo_notifications_service::{Caller, ImageData, Occurrence, Taken};
use tilo_power_mgmt::inhibit::INHIBIT_IDLE;
use tilo_session_power::PowerLink;

use super::*;
use crate::platform::notice::OPEN_ACTION;

/// What an app with a launcher-verified identity sent.
fn from_app(
    mut notification: tilo_notifications_service::Notification,
    app_id: &str,
) -> tilo_notifications_service::Notification {
    notification.caller = Some(Caller {
        app_id: app_id.into(),
        workspace: None,
    });
    notification
}

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
    tokens: Mutex<Vec<Option<String>>>,
}

struct Rig {
    daemon: Notifications,
    desktop: DesktopNotifications,
    bridge: Rc<RefCell<SessionNotifications>>,
    policy: Arc<Mutex<NotificationPolicy>>,
    signals: Rc<NoticeSignals>,
    power: PowerLink,
    heard: Arc<Heard>,
}

fn rig() -> Rig {
    rig_at(None)
}

/// A rig whose centre is kept at `history` , so a next session can open it.
fn rig_at(history: Option<std::path::PathBuf>) -> Rig {
    let daemon = Notifications::new();
    let heard = Arc::new(Heard::default());
    let tap = Arc::clone(&heard);
    daemon.on_taken(move |id, taken| {
        let said = match taken {
            Taken::Action {
                key,
                activation_token,
            } => {
                tap.tokens
                    .lock()
                    .unwrap()
                    .push(activation_token.map(str::to_owned));
                format!("action {key}")
            }
            Taken::Reply { text } => {
                tap.tokens.lock().unwrap().push(None);
                format!("reply {text}")
            }
        };
        tap.taken.lock().unwrap().push((id, said));
    });
    let desktop = DesktopNotifications::default();
    desktop.show_banners_elsewhere();
    let policy = Arc::new(Mutex::new(NotificationPolicy::default()));
    let signals = Rc::new(NoticeSignals::default());
    let power = PowerLink::default();
    let bridge = Rc::new(RefCell::new(SessionNotifications::new(
        daemon.clone(),
        desktop.clone(),
        Link::new(),
        Arc::clone(&policy),
        Rc::clone(&signals),
        Some(power.clone()),
        history,
    )));
    Rig {
        daemon,
        desktop,
        bridge,
        policy,
        signals,
        power,
        heard,
    }
}

impl Rig {
    /// A pass of the notifications, with no compositor to mint tokens.
    fn step(&self) {
        self.bridge.borrow_mut().step(&mut || None);
    }

    /// A pass of the notifications, minting tokens as the compositor would.
    fn step_minting(&self, mut mint: impl FnMut() -> Option<String>) {
        self.bridge.borrow_mut().step(&mut mint);
    }

    /// What the person did on the banners app, as it asks it.
    fn ask(&self, ask: Ask) {
        self.bridge.borrow().ask(ask);
    }

    /// What the banners were last handed: the desktop's live banners, once the bridge published
    /// them.
    fn feed(&self) -> tilo_desktop_surface::notifications::BannerFeed {
        assert_eq!(
            self.bridge.borrow().published,
            Some(self.desktop.revision()),
            "the banners have the latest"
        );
        self.desktop.feed()
    }

    fn heard(&self) -> Vec<(u32, String)> {
        std::mem::take(&mut *self.heard.taken.lock().unwrap())
    }

    /// The activation tokens the apps were handed, in their order.
    fn tokens(&self) -> Vec<Option<String>> {
        std::mem::take(&mut *self.heard.tokens.lock().unwrap())
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
        shown(critical, 0, None)
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
        let normal = shown(normal, 0, None);
        assert_eq!(normal.severity, NotificationSeverity::Info);
        assert_eq!(normal.dismiss_after, stays, "timeout {timeout}");
    }
}

/// The desktop's own notice is the system speaking, not a sender: it keeps the name it carries and
/// leads with the system's mark. A program nobody attributed is still shown as making a claim,
/// never as who it is.
#[test]
fn the_desktops_own_notice_is_the_systems_never_a_claim() {
    let mut notice = sent(8);
    notice.app_name = "Updates".into();
    notice.system = true;
    let banner = shown(notice, 0, None);
    assert_eq!(banner.app_id, None, "no app is behind it");
    assert_eq!(
        banner.app_name.as_deref(),
        Some("Updates"),
        "the system raises it in its own name"
    );
    assert_eq!(
        banner.mark().icon,
        Some(ui::icons::Icon::Bell),
        "the system's mark, not a generic window"
    );
    assert_eq!(
        shown(sent(9), 0, None).app_name,
        notifications::sender_claim("Mail"),
        "a program nobody attributed still says what it claims"
    );
}

#[test]
fn only_an_attributed_app_is_named_and_a_person_needs_a_name() {
    let unattributed = shown(sent(3), 0, None);
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
    let handle_only = shown(message.clone(), 0, None);
    assert_eq!(
        handle_only.app_name.as_deref(),
        Some("Chat"),
        "an attributed app is named by what the device calls it, never its id"
    );
    assert_eq!(handle_only.app_id.as_deref(), Some("so.tilo.chat"));
    assert_eq!(handle_only.from, None, "a handle alone names no one");
    assert!(!handle_only.rings, "a call needs its caller");

    message.sender_name = Some("Ada".into());
    let named = shown(message, 0, None);
    assert_eq!(
        named.from,
        Some(NotificationPerson {
            name: "Ada".into(),
            identity: "mailto:ada@example.org".into(),
        })
    );
    assert!(named.rings, "a call rings");
}

/// A program the session proved with no installed app behind it is named by the name it gives
/// itself, and by its file's words when it gives none: a banner and the centre never show the path
/// it runs from.
#[test]
fn a_program_is_named_by_itself_never_by_its_path() {
    let mut program = sent(6);
    program.app_name = "/usr/bin/notify-send".into();
    let named = shown(from_app(program.clone(), "/usr/bin/notify-send"), 0, None);
    assert_eq!(
        named.app_name.as_deref(),
        Some("Notify Send"),
        "a program that names itself by its path is named by its file"
    );

    program.app_name = "Notes".into();
    let says = shown(from_app(program, "/usr/bin/notify-send"), 0, None);
    assert_eq!(
        says.app_name.as_deref(),
        Some("Notes"),
        "the program's own name is what the person reads"
    );
}

#[test]
fn the_senders_pixels_are_the_banners_artwork() {
    let mut with_image = sent(5);
    with_image.image_data = Some(pixels());
    let image = shown(with_image, 7, None).app_image.expect("artwork");
    assert_eq!(image.key(), "notification:5:7");
    let painted = image.pixels(64).expect("pixels");
    assert_eq!((painted.width, painted.height), (2, 1));
    assert_eq!(painted.data.data(), pixels().rgba.as_slice());
    assert!(shown(sent(5), 7, None).app_image.is_none());
}

#[test]
fn actions_are_labelled_by_the_desktop_where_it_owns_the_words() {
    let mut alarm = sent(6);
    alarm.actions = vec![
        ("default".into(), "Go".into()),
        ("snooze".into(), "zzz".into()),
        ("archive".into(), "Archive".into()),
    ];
    let plain = shown(alarm.clone(), 0, None);
    assert_eq!(plain.actions[0].1, notifications::open_action_label());
    assert_eq!(plain.actions[1].1, "zzz", "only an alarm's are relabelled");
    alarm.scheduled = Some(Occurrence {
        workspace: "local".into(),
        token: "t1".into(),
        occurrence_ms: 1_000,
        calendar_alarm: true,
        uri: "tilo://clock".into(),
    });
    let ringing = shown(alarm, 0, None);
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
    let rig = rig();
    let id = rig.daemon.alloc_id();
    let mut notification = sent(id);
    notification.image_data = Some(pixels());
    rig.daemon.push(notification);
    rig.step();

    let feed = rig.feed();
    assert_eq!(feed.banners.len(), 1);
    let banner = &feed.banners[0].notification;
    assert_eq!(banner.title, "New mail");
    assert!(banner.app_image.is_some(), "its pixels are on the banner");

    // Closed by the person, it stays in the centre with its artwork.
    rig.ask(Ask::Banner(BannerAsk::Dismiss(banner.id)));
    rig.step();
    assert!(rig.feed().banners.is_empty());
    assert_eq!(rig.history(), ["New mail"]);
    assert!(rig.desktop.log().history()[0].app_image.is_some());
    assert!(!rig.live(id), "the app is told it was dismissed");
}

#[test]
fn a_banners_action_and_reply_reach_the_app() {
    let rig = rig();
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
    rig.step();
    let banner = rig.banner().id;
    rig.ask(Ask::Banner(BannerAsk::Invoke(banner, "archive".into())));
    rig.step();
    assert_eq!(rig.heard(), [(id, "action archive".into())]);
    assert!(!rig.live(id), "a chosen banner is closed");

    let id = rig.daemon.alloc_id();
    notification.id = id;
    rig.daemon.push(notification);
    rig.step();
    let banner = rig.banner().id;
    rig.ask(Ask::Reply {
        id: banner,
        text: "on my way".into(),
    });
    rig.step();
    assert_eq!(rig.heard(), [(id, "reply on my way".into())]);
    assert!(!rig.live(id));
}

#[test]
fn a_call_is_answered_by_the_callers_own_choice() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    let mut call = sent(id);
    call.actions = vec![
        ("default".into(), "Open".into()),
        ("decline".into(), "Decline".into()),
    ];
    rig.daemon.push(call);
    rig.step();
    rig.ask(Ask::DeclineCall(rig.banner().id));
    rig.step();
    assert_eq!(rig.heard(), [(id, "action decline".into())]);
}

#[test]
fn a_replacement_takes_the_banners_place_and_a_withdrawal_takes_it_away() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    let mut replaced = sent(id);
    replaced.summary = "Two new mails".into();
    rig.daemon.push(replaced);
    rig.step();
    let feed = rig.feed();
    assert_eq!(feed.banners.len(), 1);
    assert_eq!(feed.banners[0].notification.title, "Two new mails");
    assert!(rig.live(id), "a replacement is no close");

    assert!(rig.daemon.request_close(id));
    rig.step();
    assert!(rig.feed().banners.is_empty());
    assert!(rig.history().is_empty(), "the app took it back");
}

/// A role's notice and the machine's own news are told the same way: the producer's words, urgency,
/// choices and timing are what the banner shows, and choosing a notice that opens something hands
/// its place to the role that told it.
#[test]
fn a_notice_is_told_as_its_producer_wrote_it_wherever_it_came_from() {
    let rig = rig();
    let role = RoleNotices::over(Rc::clone(&rig.bridge));
    let place = Place {
        app: "so.tilo.notes".into(),
        record: "tab:00000000000000a1".into(),
        resource: "notes.notebook/pages/p1".into(),
    };
    role.show(
        &Notice::opens(
            "Notes",
            "Ada mentioned you",
            "Ask about the date",
            place.clone(),
        )
        .tagged(place.tag("u-bea"))
        .choosing(vec![(OPEN_ACTION.to_owned(), "Open".to_owned())])
        .staying(Some(Duration::from_secs(8))),
    );
    rig.step();
    let banner = rig.banner();
    assert_eq!(banner.title, "Ada mentioned you");
    assert_eq!(banner.app_name.as_deref(), Some("Notes"));
    assert_eq!(
        banner.actions,
        [(OPEN_ACTION.to_owned(), "Open".to_owned())]
    );
    assert_eq!(banner.dismiss_after, Duration::from_secs(8));

    // The person chooses what it names: the place reaches the role, and the banner goes.
    rig.desktop.invoke_action(banner.id, OPEN_ACTION);
    rig.step();
    assert_eq!(
        role.take_chosen(),
        vec![place],
        "the place the notice named"
    );
    assert!(rig.feed().banners.is_empty(), "a chosen notice leaves");

    // The machine's own news keeps its own name and mark, and its own urgency decides the banner.
    role.show(&crate::notices::battery_notice(4, true, true));
    rig.step();
    let news = rig.banner();
    assert_eq!(news.app_name.as_deref(), Some("Power"));
    assert_eq!(news.severity, NotificationSeverity::Error);
    assert!(news.machine_wide, "the whole machine's news");
}

#[test]
fn a_transient_notification_leaves_no_trace_in_the_centre() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    let mut transient = sent(id);
    transient.transient = true;
    rig.daemon.push(transient);
    rig.step();
    rig.ask(Ask::Banner(BannerAsk::Dismiss(rig.banner().id)));
    rig.step();
    assert!(rig.history().is_empty());
    assert!(!rig.live(id));
}

#[test]
fn read_in_full_moves_it_to_the_centre() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    rig.ask(Ask::Read(rig.banner().id));
    rig.step();
    assert!(rig.feed().banners.is_empty());
    assert_eq!(rig.history(), ["New mail"]);
}

/// Opening the centre is the person reading it: the bar's bell stops counting, and what the centre
/// keeps stays kept.
#[test]
fn opening_the_centre_reads_what_it_keeps() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert_eq!(rig.signals.unread(), 1, "the bell counts it unread");

    rig.desktop.mark_read();
    rig.step();
    assert_eq!(rig.signals.unread(), 0, "opening the centre reads it");
    assert_eq!(
        rig.feed().banners.len(),
        1,
        "reading it does not take it off the screen"
    );
}

/// A banner over an open centre would take the presses meant for its rows: an arrival while the
/// centre is open goes straight into it, and banners come back once it closes.
#[test]
fn an_open_centre_holds_new_banners_until_it_closes() {
    let rig = rig();
    rig.bridge.borrow().desktop().set_centre_open(true);
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert!(
        rig.feed().banners.is_empty(),
        "no banner over the centre's rows"
    );
    assert_eq!(
        rig.history(),
        ["New mail"],
        "it waits in the centre instead"
    );

    rig.bridge.borrow().desktop().set_centre_open(false);
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert_eq!(rig.feed().banners.len(), 1, "and shows again after");
}

/// What the centre kept is written where this person's state lives, and the next session opens the
/// centre the last one left.
#[test]
fn the_centre_opens_with_what_the_last_session_kept() {
    let path = tilo_paths::memory_temp_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("tilo-notifications-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let rig = rig_at(Some(path.clone()));
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    rig.ask(Ask::Read(rig.banner().id));
    rig.step();
    assert!(path.is_file(), "the centre was written");

    let reopened = DesktopNotifications::default();
    super::restore_kept(&reopened, &path);
    let next = SessionNotifications::new(
        Notifications::new(),
        reopened,
        Link::new(),
        Arc::clone(&rig.policy),
        Rc::new(NoticeSignals::default()),
        None,
        Some(path.clone()),
    );
    let centre = next.centre();
    assert_eq!(centre.count, 1, "the next session opens the centre it had");
    let _ = std::fs::remove_file(&path);
}

/// The centre's own resource is what the desk reads and invokes: how many it keeps, and the verbs
/// that dismiss, run, clear and bring back. What people said to this person is personal, so only a
/// caller the workspace grants reads it.
#[test]
fn the_centre_publishes_what_it_keeps_and_the_verbs_that_change_it() {
    let rig = rig();
    let second = rig.daemon.alloc_id();
    rig.daemon.push(sent(second));
    let first = rig.daemon.alloc_id();
    let mut acting = from_app(sent(first), "so.tilo.mail");
    acting.actions = vec![("open".into(), "Open".into())];
    rig.daemon.push(acting);
    rig.step();
    let up: Vec<u64> = rig
        .feed()
        .banners
        .iter()
        .map(|banner| banner.notification.id)
        .collect();
    for id in up {
        rig.ask(Ask::Read(id));
        rig.step();
    }
    assert_eq!(rig.history().len(), 2, "both are kept");

    let mut served = Served::new(Rc::clone(&rig.bridge));
    let resource = served.resources().remove(0);
    assert_eq!(
        resource.readers,
        tilo_actions::Readers::Granted,
        "what people said to this person is personal"
    );
    assert!(served.poll(), "what the centre keeps is a reading");
    assert_eq!(centre_of(&served).count, 2, "the centre counts them");
    let newest = rig
        .desktop
        .log()
        .history()
        .iter()
        .find(|notification| notification.app_id.as_deref() == Some("so.tilo.mail"))
        .map(|notification| notification.id)
        .expect("the mail is kept");

    // The person runs the notification's own action from its row.
    invoke(
        &mut served,
        "act",
        serde_json::json!({ "notification": newest, "action": "open" }),
    );
    rig.step();
    assert_eq!(
        rig.heard.taken.lock().unwrap().as_slice(),
        [(first, "action open".to_owned())],
        "the action reached the app that sent it"
    );

    // Dismissing it takes it out of the centre.
    invoke(
        &mut served,
        "dismiss",
        serde_json::json!({ "notification": newest }),
    );
    assert!(served.poll(), "the centre moved");
    assert_eq!(centre_of(&served).count, 1, "it is no longer kept");

    // Clearing the rest can be brought back.
    invoke(&mut served, "clear", serde_json::json!({}));
    assert!(served.poll());
    assert_eq!(centre_of(&served).count, 0, "the centre is empty");
    invoke(&mut served, "undo-clear", serde_json::json!({}));
    assert!(served.poll());
    assert_eq!(centre_of(&served).count, 1, "what was cleared is back");
}

/// The centre as the service publishes it now.
fn centre_of(served: &Served) -> NotificationCentre {
    serde_json::from_value(served.read(RESOURCE).expect("the centre is published"))
        .expect("a reading")
}

/// Invoke one of the centre's verbs with `arguments` , as the panel does.
fn invoke(served: &mut Served, action: &str, arguments: serde_json::Value) {
    let arguments: Map<String, Value> =
        serde_json::from_value(arguments).expect("the verb's arguments");
    served
        .invoke(RESOURCE, action, &arguments)
        .expect("the centre answers its own verbs");
}

#[test]
fn do_not_disturb_holds_the_banner_and_the_bar_says_why() {
    let rig = rig();
    rig.policy.lock().unwrap().do_not_disturb = true;
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert!(rig.feed().banners.is_empty(), "quiet holds the banner");
    assert_eq!(rig.history(), ["New mail"], "it goes to the centre");
    assert!(rig.signals.quiet(), "the bar shows a silenced bell");
    assert_eq!(rig.signals.unread(), 1, "and counts it unread");
    assert!(rig.desktop.quiet());

    rig.policy.lock().unwrap().do_not_disturb = false;
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert_eq!(rig.feed().banners.len(), 1, "and shows it again after");
    assert!(!rig.signals.quiet());
}

/// The switch the panel shows is a verb of the centre: turning it on holds the next banner back,
/// and the centre says why it is held.
#[test]
fn the_do_not_disturb_switch_is_a_verb_of_the_centre() {
    let rig = rig();
    let mut served = Served::new(Rc::clone(&rig.bridge));

    invoke(&mut served, "set-quiet", serde_json::json!({ "on": true }));
    assert!(served.poll(), "the switch changed the centre's reading");
    assert_eq!(
        rig.bridge.borrow().quiet_status().as_deref(),
        Some(notifications::quiet_label(&QuietReason::DoNotDisturb).as_str()),
        "the centre's header says why banners are held"
    );

    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert!(rig.feed().banners.is_empty(), "quiet holds the banner");
    assert_eq!(rig.history(), ["New mail"], "it goes to the centre");

    invoke(&mut served, "set-quiet", serde_json::json!({ "on": false }));
    assert!(served.poll(), "the switch changed the centre's reading");
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert_eq!(rig.feed().banners.len(), 1, "banners come back");
    assert!(rig.bridge.borrow().quiet_status().is_none());
}

/// While an app presents it holds the machine from going idle, which is how the desk knows the
/// screen is being watched by others: a notification arriving then makes no sound, its banner is
/// held, and the bar and the centre say why without the person's own Do Not Disturb switch moving.
#[test]
fn presenting_holds_the_banner_and_silences_it_without_touching_do_not_disturb() {
    let rig = rig();
    let presenting = rig.power.registry().acquire_for("Presenting", INHIBIT_IDLE);
    assert!(rig.power.idle_held());
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert!(rig.feed().banners.is_empty(), "quiet holds the banner");
    assert_eq!(rig.history(), ["New mail"], "it goes to the centre");
    assert!(rig.signals.quiet(), "the bar shows a silenced bell");
    assert_eq!(rig.signals.unread(), 1, "and counts it unread");
    assert!(
        !rig.policy.lock().unwrap().do_not_disturb,
        "presenting never rewrites the person's switch"
    );
    let label = notifications::quiet_label(&QuietReason::Presenting);
    assert_eq!(
        rig.bridge.borrow().quiet_status().as_deref(),
        Some(label.as_str()),
        "the bar and the centre say the person is presenting"
    );
    assert!(
        !should_play(
            SystemSoundEvent::Notification,
            &SystemSoundPrefs::default(),
            rig.bridge.borrow().quiet_state(),
            true,
        ),
        "an arrival while presenting makes no sound"
    );

    drop(presenting);
    assert!(!rig.power.idle_held());
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    assert_eq!(
        rig.feed().banners.len(),
        1,
        "it shows again once the hold is gone"
    );
    assert!(!rig.signals.quiet());
    assert!(rig.bridge.borrow().quiet_status().is_none());
    assert!(
        should_play(
            SystemSoundEvent::Notification,
            &SystemSoundPrefs::default(),
            rig.bridge.borrow().quiet_state(),
            true,
        ),
        "and sounds again"
    );
}

/// An app the person silenced on screen still tells them what arrived: the banner is held, the
/// centre keeps it and the bar counts it unread.
#[test]
fn an_app_with_its_banners_off_still_reaches_the_centre() {
    let rig = rig();
    rig.policy.lock().unwrap().per_app.insert(
        "so.tilo.mail".into(),
        AppNotificationRule {
            banners: Some(false),
            ..AppNotificationRule::default()
        },
    );
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    assert!(rig.feed().banners.is_empty(), "nothing is put on screen");
    assert_eq!(rig.history(), ["New mail"], "the centre keeps it");
    assert_eq!(rig.signals.unread(), 1, "the bar counts it unread");

    // An app with no rule of its own keeps its banners.
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.chat"));
    rig.step();
    assert_eq!(
        rig.feed().banners.len(),
        1,
        "only the silenced app is quiet"
    );
    assert_eq!(rig.signals.unread(), 2);
}

/// An app whose previews are off shows its banner without the body, and the centre still has what
/// was said.
#[test]
fn an_app_with_previews_off_shows_no_body_on_its_banner() {
    let rig = rig();
    rig.policy.lock().unwrap().per_app.insert(
        "so.tilo.mail".into(),
        AppNotificationRule {
            previews: Some(false),
            ..AppNotificationRule::default()
        },
    );
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    let banner = rig.banner();
    assert!(!banner.show_preview, "the body stays hidden");
    assert_eq!(banner.body, "From Ada", "but it is not lost");
    assert_eq!(banner.title, "New mail", "the title still says it arrived");

    rig.ask(Ask::Read(banner.id));
    rig.step();
    assert_eq!(
        rig.desktop.log().history()[0].body,
        "From Ada",
        "the centre lists it in full"
    );
}

/// An app the person keeps off the lock screen is marked so, while an app following the global
/// preference is shown there.
#[test]
fn an_app_kept_off_the_lock_screen_says_so_on_its_notification() {
    let rig = rig();
    rig.policy.lock().unwrap().show_on_lock_screen = true;
    rig.policy.lock().unwrap().per_app.insert(
        "so.tilo.mail".into(),
        AppNotificationRule {
            lock_screen: Some(false),
            ..AppNotificationRule::default()
        },
    );
    for app_id in ["so.tilo.mail", "so.tilo.chat"] {
        let id = rig.daemon.alloc_id();
        rig.daemon.push(from_app(sent(id), app_id));
    }
    rig.step();
    let feed = rig.feed();
    assert_eq!(feed.banners.len(), 2);
    let allowed = |app_id: &str| {
        feed.banners
            .iter()
            .find(|banner| banner.notification.app_id.as_deref() == Some(app_id))
            .map(|banner| banner.notification.show_on_lock_screen)
            .expect("its banner")
    };
    assert!(!allowed("so.tilo.mail"), "kept off the lock screen");
    assert!(
        allowed("so.tilo.chat"),
        "shown there as the global rule says"
    );
}

#[test]
fn an_apps_badge_follows_its_count_until_this_person_hides_it() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    let mut message = sent(id);
    message.caller = Some(Caller {
        app_id: "so.tilo.chat".into(),
        workspace: None,
    });
    message.badge_count = Some(7);
    rig.daemon.push(message);
    rig.step();
    assert_eq!(
        rig.signals
            .badges()
            .get("so.tilo.chat")
            .copied()
            .unwrap_or(0),
        7
    );
    assert_eq!(
        rig.signals
            .badges()
            .get("so.tilo.mail")
            .copied()
            .unwrap_or(0),
        0,
        "nothing for others"
    );

    rig.policy.lock().unwrap().show_badges = false;
    rig.step();
    assert_eq!(
        rig.signals
            .badges()
            .get("so.tilo.chat")
            .copied()
            .unwrap_or(0),
        0,
        "hidden, not lost"
    );

    rig.policy.lock().unwrap().show_badges = true;
    rig.step();
    assert_eq!(
        rig.signals
            .badges()
            .get("so.tilo.chat")
            .copied()
            .unwrap_or(0),
        7,
        "shown again as it is"
    );

    let id = rig.daemon.alloc_id();
    let mut cleared = sent(id);
    cleared.caller = Some(Caller {
        app_id: "so.tilo.chat".into(),
        workspace: None,
    });
    cleared.badge_count = Some(0);
    rig.daemon.push(cleared);
    rig.step();
    assert_eq!(
        rig.signals
            .badges()
            .get("so.tilo.chat")
            .copied()
            .unwrap_or(0),
        0,
        "the app says none"
    );
}

#[test]
fn choosing_a_scheduled_notification_names_the_target_it_rings_for() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    let mut ringing = sent(id);
    ringing.scheduled = Some(Occurrence {
        workspace: "local".into(),
        token: "0123456789abcdef0123456789abcdef".into(),
        occurrence_ms: 1_000,
        calendar_alarm: true,
        uri: "tilo://clock/timer-1".into(),
    });
    rig.daemon.push(ringing);
    rig.step();
    rig.ask(Ask::Banner(BannerAsk::Activate(rig.banner().id)));
    rig.step();
    assert_eq!(
        rig.bridge.borrow().take_opened(),
        [("local".to_owned(), "tilo://clock/timer-1".to_owned())],
        "the session opens it in the app that takes its scheme"
    );
    assert!(rig.bridge.borrow().take_opened().is_empty(), "taken once");
}

#[test]
fn a_chosen_banner_hands_its_app_a_token_to_raise_its_window() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    let mut notification = sent(id);
    notification.actions = vec![("default".into(), "Go".into())];
    rig.daemon.push(notification);
    rig.step();
    rig.ask(Ask::Banner(BannerAsk::Activate(rig.banner().id)));
    let mut minted = 0;
    rig.step_minting(|| {
        minted += 1;
        Some(format!("token-{minted}"))
    });
    assert_eq!(rig.heard(), [(id, "action default".into())]);
    assert_eq!(rig.tokens(), [Some("token-1".into())]);
    assert!(!rig.live(id), "and the banner closes behind it");
}

#[test]
fn a_critical_arrival_gets_the_error_sound_and_a_silenced_one_none() {
    let mut critical = sent(1);
    critical.urgency = 2;
    assert_eq!(
        sound_event(&critical),
        Some(SystemSoundEvent::Error),
        "a failed job gets the error voice, which plays through quiet modes"
    );

    let mut device = sent(2);
    device.category = Some("device.added".into());
    assert_eq!(
        sound_event(&device),
        Some(SystemSoundEvent::DeviceConnected),
        "an ordinary arrival keeps its category's sound"
    );

    let mut silenced = sent(3);
    silenced.urgency = 2;
    silenced.suppress_sound = true;
    assert_eq!(sound_event(&silenced), None, "a sender may ask for none");
}

// ---- Notifications that continue on the person's other devices ----

/// What this session asked the person's other devices to show, and what it told them.
struct FakeFabric {
    local: DeviceId,
    online: BTreeSet<DeviceId>,
    permitted: std::sync::atomic::AtomicBool,
    sent: Mutex<Vec<(DeviceId, PersonalFabricAction)>>,
}

impl FakeFabric {
    /// This desktop, with `laptop` as the person's other device online.
    fn new(permitted: bool) -> Arc<Self> {
        let Ok(local) = DeviceId::new("desk".to_owned()) else {
            panic!("desk is a device id");
        };
        let Ok(laptop) = DeviceId::new("laptop".to_owned()) else {
            panic!("laptop is a device id");
        };
        Arc::new(Self {
            online: BTreeSet::from([local.clone(), laptop]),
            local,
            permitted: std::sync::atomic::AtomicBool::new(permitted),
            sent: Mutex::default(),
        })
    }

    /// The actions it was asked to send since the last look.
    fn sent(&self) -> Vec<(DeviceId, PersonalFabricAction)> {
        std::mem::take(&mut *self.sent.lock().unwrap())
    }

    /// What it projected since the last look.
    fn projected(&self) -> Vec<ContinuityNotificationProjection> {
        self.sent()
            .into_iter()
            .filter_map(|(_, action)| match action {
                PersonalFabricAction::NotificationProject {
                    notification: projection,
                } => Some(projection),
                _ => None,
            })
            .collect()
    }
}

impl crate::platform::fabric::FabricLink for FakeFabric {
    fn device_fabric(&self) -> crate::platform::fabric::DeviceFabric {
        crate::platform::fabric::DeviceFabric::default()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.online.clone()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        Some((self.local.clone(), "Desk".to_owned()))
    }

    fn try_send_personal_fabric_action(
        &self,
        target: &DeviceId,
        action: PersonalFabricAction,
    ) -> Result<[u8; 16], crate::platform::fabric::PersonalFabricSendError> {
        self.sent.lock().unwrap().push((target.clone(), action));
        Ok([7; 16])
    }

    fn require_media(&self) -> Result<(), String> {
        Ok(())
    }

    fn notifications_permitted(&self) -> bool {
        self.permitted.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// A rig on the person's fabric, with `laptop` as the other device online.
fn rig_on_fabric(permitted: bool) -> (Rig, Arc<FakeFabric>) {
    let rig = rig();
    let fabric = FakeFabric::new(permitted);
    let link: crate::platform::fabric::SharedLink = fabric.clone();
    rig.bridge.borrow().attach_fabric(link);
    (rig, fabric)
}

/// What another of the person's devices projected: a mail to Ada, with its archive action.
fn projection() -> ContinuityNotificationProjection {
    ContinuityNotificationProjection {
        id: ContinuityNotificationId {
            epoch: [1; 16],
            token: [2; 16],
        },
        app_id: "so.tilo.mail".into(),
        app_name: "Mail".into(),
        title: "New mail".into(),
        body: "From Ada".into(),
        body_redacted: false,
        actions: vec![("archive".into(), "Archive".into())],
        urgency: 0,
        expires_unix_ms: tilo_clock::unix_now_ms().saturating_add(60_000),
    }
}

fn laptop() -> DeviceId {
    let Ok(laptop) = DeviceId::new("laptop".to_owned()) else {
        panic!("laptop is a device id");
    };
    laptop
}

/// The desktop's own notice is about the machine that raises it: it is not an app's message to
/// continue on the person's other devices.
#[test]
fn the_desktops_own_notice_stays_on_this_machine() {
    let (rig, fabric) = rig_on_fabric(true);
    let mismatch = crate::network::VersionMismatch {
        endpoint: "zpeerexample".into(),
        theirs: 91,
        ours: 90,
    };
    rig.bridge
        .borrow_mut()
        .tell(&crate::notices::version_mismatch_notice(
            &mismatch,
            Some("Framework 16"),
        ));
    rig.step();
    assert!(
        fabric.projected().is_empty(),
        "this machine's own news is no other device's notification"
    );
}

#[test]
fn an_apps_notification_continues_on_the_persons_other_devices() {
    let (rig, fabric) = rig_on_fabric(true);
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();

    let projections = fabric.projected();
    assert_eq!(projections.len(), 1, "sent to the laptop alone");
    let projected = &projections[0];
    assert_eq!(projected.title, "New mail");
    assert_eq!(projected.body, "From Ada");
    assert_eq!(projected.app_id, "so.tilo.mail");
    assert_eq!(projected.app_name, "Mail");
    assert_eq!(projected.urgency, 0, "nothing asks for alarm treatment");
    assert!(projected.actions.is_empty(), "the app offered none");
    assert!(
        projected.expires_unix_ms > tilo_clock::unix_now_ms(),
        "it has a window to be shown in"
    );
}

#[test]
fn with_previews_off_the_text_stays_on_the_device_it_was_told() {
    let (rig, fabric) = rig_on_fabric(true);
    rig.policy.lock().unwrap().show_previews = false;
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();

    let projected = fabric.projected();
    assert_eq!(projected.len(), 1);
    assert!(projected[0].body_redacted);
    assert!(projected[0].body.is_empty(), "the text never leaves here");
}

#[test]
fn notifications_stay_where_they_are_when_the_person_turns_this_off() {
    let (rig, fabric) = rig_on_fabric(false);
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    assert!(fabric.projected().is_empty());

    let (rig, fabric) = rig_on_fabric(true);
    rig.policy.lock().unwrap().cross_device = false;
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    assert!(
        fabric.projected().is_empty(),
        "the person's own switch holds them here"
    );
}

#[test]
fn a_notification_from_the_persons_other_device_is_named_after_it() {
    let rig = rig();
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &projection());

    let shown = rig.banner();
    assert_eq!(shown.title, "New mail");
    assert_eq!(shown.body, "From Ada");
    assert_eq!(shown.app_name.as_deref(), Some("Mail from Laptop"));
    assert_eq!(shown.app_id.as_deref(), Some("so.tilo.mail"));
    assert_eq!(
        shown.actions,
        [("archive".to_owned(), "Archive".to_owned())]
    );
    assert_eq!(
        shown.remote_route,
        Some(RemoteNotificationRoute {
            source_device: "laptop".into(),
            source_label: "Laptop".into(),
            notification_id: projection().id,
        })
    );
    assert!(
        !shown.show_on_lock_screen,
        "the laptop's text stays off this lock screen"
    );
}

/// The banner a person meets leads with the app's own mark, whichever device told it: the
/// projection carries the app's id, so the other device's Mail draws Mail's mark here rather than a
/// generic window.
#[test]
fn another_devices_notification_leads_with_the_same_mark_as_here() {
    let here = {
        let rig = rig();
        let id = rig.daemon.alloc_id();
        rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
        rig.step();
        rig.banner()
    };
    let there = {
        let rig = rig();
        rig.bridge
            .borrow()
            .remote_project(&laptop(), "Laptop", &projection());
        rig.banner()
    };
    assert_eq!(
        there.mark().icon,
        here.mark().icon,
        "the same app leads with the same mark on both devices"
    );
    assert_eq!(
        there.mark().icon,
        Some(ui::icons::Icon::Mail),
        "Mail's own mark, not a generic window"
    );
    assert!(
        there.mark().image.is_none(),
        "the other device's banner carries no artwork"
    );
}

#[test]
fn without_previews_the_other_devices_text_arrives_redacted() {
    let rig = rig();
    let mut redacted = projection();
    redacted.body_redacted = true;
    redacted.body = String::new();
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &redacted);
    assert!(rig.banner().body.is_empty());
    assert!(!rig.banner().show_preview);
}

#[test]
fn the_same_projection_updates_its_row_in_place() {
    let rig = rig();
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &projection());
    let first = rig.banner().id;
    let mut update = projection();
    update.title = "Two new mails".into();
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &update);
    assert_eq!(rig.desktop.log().active().len(), 1, "one row, not two");
    assert_eq!(rig.banner().id, first, "the same row shows the update");
    assert_eq!(rig.banner().title, "Two new mails");
}

#[test]
fn dismissing_the_other_devices_notification_answers_it_there() {
    let (rig, fabric) = rig_on_fabric(true);
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &projection());
    let id = rig.banner().id;
    rig.desktop.dismiss_history(id);
    assert_eq!(
        fabric.sent(),
        [(
            laptop(),
            PersonalFabricAction::NotificationDismiss {
                id: projection().id
            }
        )]
    );
}

#[test]
fn choosing_an_action_answers_the_device_it_came_from() {
    let (rig, fabric) = rig_on_fabric(true);
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &projection());
    let id = rig.banner().id;
    rig.desktop.invoke_action(id, "archive");
    assert_eq!(
        fabric.sent(),
        [(
            laptop(),
            PersonalFabricAction::NotificationInvoke {
                id: projection().id,
                action_key: "archive".into(),
            }
        )]
    );
}

#[test]
fn an_action_the_app_never_offered_is_not_run() {
    let (rig, fabric) = rig_on_fabric(true);
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    let projected = fabric.projected();
    assert_eq!(projected.len(), 1);
    // An action it never offered, under the id it did project: refused.
    let asked = PersonalFabricAction::NotificationInvoke {
        id: projected[0].id,
        action_key: "wipe".into(),
    };
    assert!(!rig.bridge.borrow_mut().answered(&asked));
    assert!(rig.live(id), "the app's notification is untouched");
    assert!(fabric.sent().is_empty());
}

#[test]
fn a_choice_made_elsewhere_closes_the_apps_notification_here() {
    let (rig, fabric) = rig_on_fabric(true);
    let id = rig.daemon.alloc_id();
    let mut notification = from_app(sent(id), "so.tilo.mail");
    notification.actions = vec![("archive".into(), "Archive".into())];
    rig.daemon.push(notification);
    rig.step();
    let projected = fabric.projected();
    assert_eq!(projected.len(), 1);
    assert!(!rig.feed().banners.is_empty());

    let chosen = PersonalFabricAction::NotificationInvoke {
        id: projected[0].id,
        action_key: "archive".into(),
    };
    assert!(rig.bridge.borrow_mut().answered(&chosen));
    assert_eq!(rig.heard(), [(id, "action archive".into())]);
    assert!(!rig.live(id), "the app is told it was dismissed");
    assert!(rig.desktop.log().active().is_empty());
    rig.step();
    assert!(rig.feed().banners.is_empty());
    assert_eq!(
        fabric.sent(),
        [(
            laptop(),
            PersonalFabricAction::NotificationRetire {
                id: projected[0].id
            }
        )],
        "every other copy goes with it"
    );
}

#[test]
fn dismissing_it_here_takes_it_away_on_the_other_devices() {
    let (rig, fabric) = rig_on_fabric(true);
    let id = rig.daemon.alloc_id();
    rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
    rig.step();
    let projected = fabric.projected();
    assert_eq!(projected.len(), 1);

    // The person here is done with it: it leaves the centre.
    let shown = rig.banner().id;
    rig.ask(Ask::Banner(BannerAsk::Dismiss(shown)));
    rig.step();
    rig.desktop.dismiss_history(shown);
    rig.step();
    assert_eq!(
        fabric.sent(),
        [(
            laptop(),
            PersonalFabricAction::NotificationRetire {
                id: projected[0].id
            }
        )]
    );
}

#[test]
fn a_retire_from_the_other_device_takes_its_copy_away() {
    let rig = rig();
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &projection());
    assert_eq!(rig.desktop.log().active().len(), 1);
    rig.bridge
        .borrow()
        .remote_retire(&laptop(), projection().id);
    assert!(rig.desktop.log().active().is_empty());
    assert!(
        rig.desktop.log().history().is_empty(),
        "the centre keeps no copy either"
    );
}

#[test]
fn an_expired_projection_is_never_shown() {
    let rig = rig();
    let mut stale = projection();
    stale.expires_unix_ms = tilo_clock::unix_now_ms().saturating_sub(1);
    rig.bridge
        .borrow()
        .remote_project(&laptop(), "Laptop", &stale);
    assert!(rig.desktop.log().active().is_empty());
}

/// A collaborator's device at another protocol version reaches the desk as an ordinary banner
/// naming it: the person reads what to update, not an operator's log line.
#[test]
fn a_device_on_another_version_is_named_on_the_desk() {
    let rig = rig();
    let mismatch = crate::network::VersionMismatch {
        endpoint: "zpeerexample".into(),
        theirs: 91,
        ours: 90,
    };
    rig.bridge
        .borrow_mut()
        .tell(&crate::notices::version_mismatch_notice(
            &mismatch,
            Some("Framework 16"),
        ));
    rig.step();
    let feed = rig.feed();
    assert_eq!(feed.banners.len(), 1, "it is a banner over the desk");
    let banner = &feed.banners[0].notification;
    assert!(
        banner.title.contains("Framework 16") && banner.title.contains("newer Tilo"),
        "the device names the title: {}",
        banner.title
    );
    assert!(
        banner.body.contains("restarts"),
        "the body says what happens here: {}",
        banner.body
    );
    assert_eq!(
        banner.app_name.as_deref(),
        Some("Updates"),
        "the system raises it in its own name, never a sender's claim"
    );
    assert_eq!(
        banner.mark().icon,
        Some(ui::icons::Icon::Bell),
        "the system's mark, not a generic window"
    );
    assert_eq!(
        rig.desktop.log().active().len(),
        1,
        "and it waits in the centre"
    );
}
