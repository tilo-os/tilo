use std::sync::{Arc, Mutex};
use tilo_session_core::desktop::state::banners::Banners as _;

use settings_store::notification_policy::AppNotificationRule;
use settings_store::system_sounds::{SystemSoundEvent, SystemSoundPrefs, should_play};
use tilo_notifications_service::{Caller, ImageData, Occurrence, Taken};
use tilo_power_mgmt::inhibit::INHIBIT_IDLE;
use tilo_session_core::desktop::notifications::{
    DEFAULT_DISMISS_AFTER, NotificationSeverity, PERSISTENT_DISMISS,
};
use tilo_session_power::PowerLink;

use super::*;
use crate::platform::notice::OPEN_ACTION;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

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
fn rig_at(history: Option<super::KeptCentre>) -> Rig {
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

    /// What the person did on a banner the shell shows, as the session hears it.
    fn ask(&self, ask: Ask) {
        self.bridge.borrow().ask(ask);
    }

    /// The banners the shell reads now: the desktop's live banners.
    fn feed(&self) -> tilo_session_core::desktop::state::banners::BannerFeed {
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

    /// The live banner saying `title` .
    fn banner_titled(&self, title: &str) -> Notification {
        self.desktop
            .log()
            .active()
            .iter()
            .find(|notification| notification.title == title)
            .cloned()
            .expect("it is up")
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

tilo_test_env::scenario! {
    fn a_banner_names_its_sender_and_shows_what_it_sent;

    phase urgency_and_timeout_set_how_long_a_banner_stays;
    {
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

    phase the_desktops_own_notice_is_the_systems_never_a_claim;
    // The desktop's own notice is the system speaking, not a sender: it keeps the name it carries and
    // leads with the system's mark. A program nobody attributed is still shown as making a claim,
    // never as who it is.
    {
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

    phase only_an_attributed_app_is_named_and_a_person_needs_a_name;
    {
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
                agent: false,
            })
        );
        assert!(named.rings, "a call rings");
    }

    phase a_program_is_named_by_itself_never_by_its_path;
    // A program the session proved with no installed app behind it is named by the name it gives
    // itself, and by its file's words when it gives none: a banner and the centre never show the path
    // it runs from.
    {
        let mut program = sent(6);
        program.app_name = "/usr/bin/notify-send".into();
        let named = shown(from_app(program.clone(), "/usr/bin/notify-send"), 0, None);
        assert_eq!(
            named.app_name.as_deref(),
            Some("Notify Send"),
            "a program that names itself by its path is named by its file"
        );

        program.app_name = "Notes".into();
        let says = shown(from_app(program.clone(), "/usr/bin/notify-send"), 0, None);
        assert_eq!(
            says.app_name.as_deref(),
            Some("Notes"),
            "the program's own name is what the person reads"
        );

        // A command line program names itself by its file: the person reads where it came from, with
        // the terminal's mark, never the program's file name beside a generic window.
        program.app_name = "notify-send".into();
        let command = shown(from_app(program, "/usr/bin/notify-send"), 0, None);
        assert_eq!(
            command.app_name.as_deref(),
            Some("Terminal"),
            "a command line sender is the terminal"
        );
        assert_eq!(
            command.mark().icon,
            Some(ui::icons::Icon::SquareTerminal),
            "and leads with the terminal's mark"
        );
    }

    phase the_senders_pixels_are_the_banners_artwork;
    {
        let mut with_image = sent(5);
        with_image.image_data = Some(pixels());
        let image = shown(with_image, 7, None).app_image.expect("artwork");
        assert_eq!(image.key(), "notification:5:7");
        let painted = image.pixels(64).expect("pixels");
        assert_eq!((painted.width, painted.height), (2, 1));
        assert_eq!(painted.data.data(), pixels().rgba.as_slice());
        assert!(shown(sent(5), 7, None).app_image.is_none());
    }

    phase actions_are_labelled_by_the_desktop_where_it_owns_the_words;
    {
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

    phase a_notice_is_told_as_its_producer_wrote_it_wherever_it_came_from;
    // A role's notice and the machine's own news are told the same way: the producer's words, urgency,
    // choices and timing are what the banner shows, and choosing a notice that opens something hands
    // its place to the role that told it.
    {
        let rig = rig();
        let role = RoleNotices::over(Rc::clone(&rig.bridge));
        let place = Place {
            app: "so.tilo.notes".into(),
            record: "tab:00000000000000a1".into(),
            resource: "notes.notebook".into(),
            anchor: "notes.notebook/pages/p1".into(),
        };
        role.show(
            &Notice::opens(
                "Notes",
                "Ada mentioned you",
                "Ask about the date",
                place.clone(),
            )
            .tagged(place.tag(&ui::MemberId::person("u-bea")))
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
            vec![place.clone()],
            "the place the notice named"
        );
        assert_eq!(
            role.take_chosen_actions(),
            [(place.tag(&ui::MemberId::person("u-bea")), OPEN_ACTION.to_owned())],
            "the action it offered, by the notice's tag"
        );
        assert!(rig.feed().banners.is_empty(), "a chosen notice leaves");

        // The machine's own news keeps its own name and mark, and its own urgency decides the banner.
        role.show(&crate::notices::battery_notice(4, true, true));
        rig.step();
        let news = rig.banner();
        assert_eq!(news.app_name.as_deref(), Some("Power"));
        assert_eq!(news.severity, NotificationSeverity::Error);
        assert!(news.machine_wide, "the whole machine's news");

        // A notice says what it is about, and the shell leads with that glyph.
        role.show(
            &Notice::new("", 1, "You joined Book club", "").about(NotificationIconKind::Workspace),
        );
        rig.step();
        assert_eq!(
            rig.banner_titled("You joined Book club").mark().icon,
            Some(ui::icons::Icon::LayoutGrid),
            "the workspace's own mark"
        );

        // An ask offers its answer and "Not now", under the part of the desktop that asks, and the
        // answer comes back by the notice's tag: how a window's move is answered.
        let action = tilo_session_core::desktop::state::HostAction {
            id: "keep-identity".into(),
            title: "Keep with passkey".into(),
            label: None,
            subtitle: None,
            icon: None,
            destructive: false,
            unasked: false,
            source: Some(tilo_session_core::desktop::state::HostActionSource::Devices),
        };
        let ask = action.ask("Pair “Phone”?", "").tagged("ask");
        role.show(&ask);
        rig.step();
        let banner = rig.banner_titled("Pair “Phone”?");
        assert_eq!(banner.icon_kind, NotificationIconKind::Permission);
        assert_eq!(
            banner.dismiss_after, PERSISTENT_DISMISS,
            "until the person answers"
        );
        rig.desktop.invoke_action(banner.id, "not-now");
        rig.step();
        assert!(
            role.take_chosen_actions().is_empty(),
            "put off, not answered"
        );

        role.show(&action.ask("Pair “Phone”?", "").tagged("pairing"));
        rig.step();
        let key = tilo_session_core::desktop::state::host_action_key("keep-identity");
        rig.desktop
            .invoke_action(rig.banner_titled("Pair “Phone”?").id, &key);
        rig.step();
        assert_eq!(
            role.take_chosen_actions(),
            [("pairing".to_owned(), key)],
            "the answer names the notice that asked"
        );
    }
}

tilo_test_env::scenario! {
    fn a_banner_arrives_is_answered_and_goes;

    phase an_arrival_with_pixels_reaches_the_banners_and_the_centre;
    {
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

    phase a_banners_action_and_reply_reach_the_app;
    {
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

    phase a_call_is_answered_by_the_callers_own_choice;
    {
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

    phase a_replacement_takes_the_banners_place_and_a_withdrawal_takes_it_away;
    {
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

    phase choosing_a_scheduled_notification_names_the_target_it_rings_for;
    {
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

    phase a_chosen_banner_hands_its_app_a_token_to_raise_its_window;
    {
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

    phase a_critical_arrival_gets_the_error_sound_and_a_silenced_one_none;
    {
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
}

tilo_test_env::scenario! {
    fn the_centre_keeps_what_was_read_and_publishes_it;

    phase a_transient_notification_leaves_no_trace_in_the_centre;
    {
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

    phase read_in_full_moves_it_to_the_centre;
    {
        let rig = rig();
        let id = rig.daemon.alloc_id();
        rig.daemon.push(sent(id));
        rig.step();
        rig.ask(Ask::Read(rig.banner().id));
        rig.step();
        assert!(rig.feed().banners.is_empty());
        assert_eq!(rig.history(), ["New mail"]);
    }

    phase the_centre_the_panel_reads_counts_what_the_bell_counts;
    // The centre the panel reads counts what the bar's bell counts: a fresh notification is one the
    // person has not read, before it is kept, so the panel's row is never "None" while the bell shows
    // one.
    {
        let rig = rig();
        let id = rig.daemon.alloc_id();
        rig.daemon.push(sent(id));
        rig.step();
        assert_eq!(rig.signals.unread(), 1, "the bell counts it unread");
        assert_eq!(
            rig.bridge.borrow().centre().count,
            1,
            "the row that opens the centre counts it too, while it is still a banner"
        );

        rig.desktop.mark_read();
        rig.step();
        assert_eq!(
            rig.bridge.borrow().centre().count,
            0,
            "and reads nothing once the person has opened the centre"
        );
    }

    phase opening_the_centre_reads_what_it_keeps;
    // Opening the centre is the person reading it: the bar's bell stops counting, and what the centre
    // keeps stays kept.
    {
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

    phase an_open_centre_holds_new_banners_until_it_closes;
    // A banner over an open centre would take the presses meant for its rows: an arrival while the
    // centre is open goes straight into it, and banners come back once it closes.
    {
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

    phase the_centre_opens_with_what_the_last_session_kept;
    // What the centre kept is written to the device's records, and the next session opens the
    // centre the last one left.
    {
        let records = tilo_session_client::device_records::DeviceRecords::held();
        let rig = rig_at(Some(super::KeptCentre {
            record: records.record(super::KEPT),
            arrived: tilo_session_client::device_records::Arrived::default(),
        }));
        let id = rig.daemon.alloc_id();
        rig.daemon.push(from_app(sent(id), "so.tilo.mail"));
        rig.step();
        rig.ask(Ask::Read(rig.banner().id));
        rig.step();
        assert!(
            records.record(super::KEPT).read().unwrap().is_some(),
            "the centre was written"
        );

        let reopened = DesktopNotifications::default();
        let options = crate::Options {
            records: Some(records.clone()),
            ..crate::Options::default()
        };
        let kept = restore_history(&reopened, &options);
        let next = SessionNotifications::new(
            Notifications::new(),
            reopened,
                Arc::clone(&rig.policy),
            Rc::new(NoticeSignals::default()),
            None,
            kept,
        );
        let centre = next.centre();
        assert_eq!(centre.count, 1, "the next session opens the centre it had");

        // The centre is followed while the session runs: one Settings or an agent cleared on
        // this device is the one the person sees, with no restart.
        let mut next = next;
        records.write_elsewhere(super::KEPT, serde_json::json!({ "notifications": [] }));
        next.step(&mut || None);
        assert_eq!(next.centre().count, 0, "the centre cleared elsewhere shows cleared");
    }

    phase the_centre_publishes_what_it_keeps_and_the_verbs_that_change_it;
    // The centre's own resource is what the shell reads and invokes: how many it keeps, and the verbs
    // that dismiss, run, clear and bring back. What people said to this person is personal, so only a
    // caller the workspace grants reads it.
    {
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
        let _resource = served.resources().remove(0);
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

tilo_test_env::scenario! {
    fn do_not_disturb_and_presenting_hold_the_banner;

    phase do_not_disturb_holds_the_banner_and_the_bar_says_why;
    {
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

    phase the_do_not_disturb_switch_is_a_verb_of_the_centre;
    // The switch the panel shows is a verb of the centre: turning it on holds the next banner back,
    // and the centre says why it is held.
    {
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

    phase presenting_holds_the_banner_and_silences_it_without_touching_do_not_disturb;
    // While an app presents it holds the machine from going idle, which is how the shell knows the
    // screen is being watched by others: a notification arriving then makes no sound, its banner is
    // held, and the bar and the centre say why without the person's own Do Not Disturb switch moving.
    {
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
}

tilo_test_env::scenario! {
    fn an_apps_notification_rule_decides_what_it_shows;

    phase an_app_with_its_banners_off_still_reaches_the_centre;
    // An app the person silenced on screen still tells them what arrived: the banner is held, the
    // centre keeps it and the bar counts it unread.
    {
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

    phase an_app_with_previews_off_shows_no_body_on_its_banner;
    // An app whose previews are off shows its banner without the body, and the centre still has what
    // was said.
    {
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

    phase an_app_kept_off_the_lock_screen_says_so_on_its_notification;
    // An app the person keeps off the lock screen is marked so, while an app following the global
    // preference is shown there.
    {
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

    phase an_apps_badge_follows_its_count_until_this_person_hides_it;
    {
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
        assert_eq!(rig.signals.badge("so.tilo.chat"), 7);
        assert_eq!(rig.signals.badge("so.tilo.mail"), 0, "nothing for others");

        rig.policy.lock().unwrap().show_badges = false;
        rig.step();
        assert_eq!(rig.signals.badge("so.tilo.chat"), 0, "hidden, not lost");

        rig.policy.lock().unwrap().show_badges = true;
        rig.step();
        assert_eq!(rig.signals.badge("so.tilo.chat"), 7, "shown again as it is");

        let id = rig.daemon.alloc_id();
        let mut cleared = sent(id);
        cleared.caller = Some(Caller {
            app_id: "so.tilo.chat".into(),
            workspace: None,
        });
        cleared.badge_count = Some(0);
        rig.daemon.push(cleared);
        rig.step();
        assert_eq!(rig.signals.badge("so.tilo.chat"), 0, "the app says none");
    }
}

// ---- Notifications that continue on the person's other devices ----

/// The person's own workspace as their devices reach it: one store each of them writes to as
/// itself, which tells every one of them what moved.
#[derive(Default)]
struct PersonsWorkspace {
    docs: Mutex<BTreeMap<(String, String), automerge::AutoCommit>>,
    told: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<tilo_workspace_service::workspaces::Moved>>>,
}

/// One device of the person, reaching their own workspace as itself.
struct AsDevice {
    workspace: Arc<PersonsWorkspace>,
    cert: tilo_user_identity::DeviceCert,
}

impl tilo_workspace_service::workspaces::PersonStore for AsDevice {
    fn certificate(&self) -> tilo_user_identity::DeviceCert {
        self.cert.clone()
    }

    fn held(&self) -> tilo_workspace_service::workspaces::Answer<'_, bool> {
        Box::pin(async { true })
    }

    fn routes(
        &self,
    ) -> tilo_workspace_service::workspaces::Answer<'_, Result<Vec<(String, String)>, String>> {
        let routes = self
            .workspace
            .docs
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        Box::pin(async move { Ok(routes) })
    }

    fn document<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
    ) -> tilo_workspace_service::workspaces::Answer<'a, Result<Option<Vec<u8>>, String>> {
        let saved = self
            .workspace
            .docs
            .lock()
            .unwrap()
            .get_mut(&(app.to_owned(), name.to_owned()))
            .map(automerge::AutoCommit::save);
        Box::pin(async move { Ok(saved) })
    }

    fn author(
        &self,
        app: String,
        name: String,
        authoring: tilo_workspace_service::workspaces::Authoring,
    ) -> tilo_workspace_service::workspaces::Answer<'_, Result<(), String>> {
        let made = {
            let mut docs = self.workspace.docs.lock().unwrap();
            let key = (app.clone(), name.clone());
            if !docs.contains_key(&key) {
                let genesis = authoring
                    .genesis
                    .ok_or_else(|| "a first write founds the resource".to_owned());
                match genesis {
                    Ok(genesis) => {
                        docs.insert(key.clone(), *genesis);
                    }
                    Err(error) => return Box::pin(async move { Err(error) }),
                }
            }
            let doc = docs.get_mut(&key).expect("founded above");
            (authoring.edit)(doc)
        };
        if made.is_ok() {
            self.workspace.told.lock().unwrap().retain(|told| {
                told.send(tilo_workspace_service::workspaces::Moved::Resource(
                    app.clone(),
                    name.clone(),
                ))
                .is_ok()
            });
        }
        Box::pin(async move { made })
    }

    fn moves(
        &self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<tilo_workspace_service::workspaces::Moved> {
        let (tell, told) = tokio::sync::mpsc::unbounded_channel();
        self.workspace.told.lock().unwrap().push(tell);
        told
    }
}

/// Where the person's workspace writes run: one thread, so each device's writes land in the order
/// it made them.
fn test_runtime() -> tokio::runtime::Handle {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime");
            let handle = runtime.handle().clone();
            std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));
            handle
        })
        .clone()
}

/// A device of the person whose root is `root`, called `label`, its key drawn from `seed`.
fn device_of(
    root: &ed25519_dalek::SigningKey,
    label: &str,
    seed: u8,
) -> tilo_user_identity::DeviceCert {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root signs");
    cert
}

/// Continuity as the person set it: on, or `off` in Settings.
fn continuity(on: bool) -> crate::platform::fabric::Continuity {
    let continuity = crate::platform::fabric::Continuity::default();
    let document = if on {
        Map::new()
    } else {
        serde_json::json!({ "continuity": { "enabled": false } })
            .as_object()
            .cloned()
            .unwrap_or_default()
    };
    continuity.read(&document);
    continuity
}

/// Two devices of one person, a desktop and a laptop, each a session over the one record of their
/// devices.
struct TwoDevices {
    desktop: Rig,
    laptop: Rig,
    desktop_cert: tilo_user_identity::DeviceCert,
    /// The desktop's switch for notifications continuing.
    desktop_continuity: crate::platform::fabric::Continuity,
    workspace: Arc<PersonsWorkspace>,
}

fn two_devices() -> TwoDevices {
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let workspace = Arc::new(PersonsWorkspace::default());
    let desktop_cert = device_of(&root, "Desktop", 2);
    let laptop_cert = device_of(&root, "Laptop", 3);
    let desktop_continuity = continuity(true);
    let attached = |cert: &tilo_user_identity::DeviceCert,
                    continuity: crate::platform::fabric::Continuity| {
        let resources = crate::PersonResources::default();
        resources.attach(
            Arc::new(AsDevice {
                workspace: Arc::clone(&workspace),
                cert: cert.clone(),
            }),
            &test_runtime(),
        );
        let kept = cert.clone();
        resources.apply(DEVICES, move || {
            Devices::keeping(&kept).expect("a certificate the record keeps")
        });
        let rig = rig();
        rig.bridge
            .borrow()
            .attach_record(resources, continuity, Arc::new(|| {}));
        rig
    };
    let desktop = attached(&desktop_cert, desktop_continuity.clone());
    let laptop = attached(&laptop_cert, continuity(true));
    TwoDevices {
        desktop,
        laptop,
        desktop_cert,
        desktop_continuity,
        workspace,
    }
}

impl TwoDevices {
    /// Run both devices' passes until `done` holds.
    fn until(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.desktop.step();
            self.laptop.step();
            if done(self) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what} never happened: the desktop shows {:?}, the laptop {:?}",
                titles(&self.desktop),
                titles(&self.laptop),
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The notifications the record of the person's devices shows now.
    fn in_record(&self) -> usize {
        let docs = self.workspace.docs.lock().unwrap();
        let Some(doc) = docs.get(&(DEVICES.0.to_owned(), DEVICES.1.to_owned())) else {
            return 0;
        };
        tilo_resource::Replica::<Devices>::from_doc(doc.clone())
            .state()
            .map_or(0, |devices| devices.notices.len())
    }
}

/// What `rig` shows: its live banners' and its centre's titles.
fn titles(rig: &Rig) -> Vec<String> {
    let log = rig.desktop.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .map(|notification| notification.title.clone())
        .collect()
}

/// What an app sent on a device: a mail under `id`, titled `title`.
fn mail(rig: &Rig, title: &str) -> u32 {
    let id = rig.daemon.alloc_id();
    let mut notification = from_app(sent(id), "so.tilo.mail");
    notification.summary = title.into();
    notification.actions = vec![("archive".into(), "Archive".into())];
    rig.daemon.push(notification);
    id
}

tilo_test_env::scenario! {
    fn what_is_told_here_is_shown_on_the_persons_other_devices;

    phase an_apps_notification_is_shown_on_the_laptop_named_after_this_device;
    {
        let devices = two_devices();
        mail(&devices.desktop, "New mail");
        devices.until("the laptop showing the desktop's mail", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        let shown = devices.laptop.banner();
        assert_eq!(shown.title, "New mail");
        assert_eq!(shown.body, "From Ada");
        assert_eq!(shown.app_name.as_deref(), Some("Mail from Desktop"));
        assert_eq!(shown.app_id.as_deref(), Some("so.tilo.mail"));
        assert_eq!(shown.actions, [("archive".to_owned(), "Archive".to_owned())]);
        let route = shown.remote_route.clone().expect("a way back to the desktop");
        assert_eq!(route.source_device, hex::encode(devices.desktop_cert.device_public_key));
        assert_eq!(route.source_label, "Desktop");
        assert!(!shown.show_on_lock_screen, "the desktop's text stays off this lock screen");
        // The banner leads with the app's own mark, whichever device told it.
        assert_eq!(shown.mark().icon, devices.desktop.banner().mark().icon);
        assert_eq!(shown.mark().icon, Some(ui::icons::Icon::Mail));
        assert!(shown.mark().image.is_none(), "the other device's banner carries no artwork");
        assert!(
            devices.desktop.desktop.log().active()[0].remote_route.is_none(),
            "the desktop shows its own mail as its own"
        );
    }

    phase with_previews_off_the_text_stays_on_the_device_it_was_told;
    {
        let devices = two_devices();
        devices.desktop.policy.lock().unwrap().show_previews = false;
        mail(&devices.desktop, "New mail");
        devices.until("the laptop showing the mail", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        assert!(devices.laptop.banner().body.is_empty(), "the text never leaves the desktop");
        assert!(!devices.laptop.banner().show_preview);
    }

    phase the_desktops_own_notice_and_what_the_person_holds_back_stay_here;
    // The desktop's own notice is about the machine that raises it; with continuity off, or the
    // person's own switch off, an app's notification stays where it was sent too. Each comes
    // before a mail that does cross, which the laptop then shows alone.
    {
        let devices = two_devices();
        let mismatch = crate::network::VersionMismatch {
            endpoint: "zpeerexample".into(),
            theirs: 91,
            ours: 90,
        };
        devices
            .desktop
            .bridge
            .borrow_mut()
            .tell(&crate::notices::version_mismatch_notice(
                &mismatch,
                Some("Framework 16"),
            ));
        devices.desktop.step();
        devices.desktop_continuity.read(
            serde_json::json!({ "continuity": { "enabled": false } })
                .as_object()
                .expect("a document"),
        );
        mail(&devices.desktop, "With continuity off");
        devices.desktop.step();
        devices.desktop_continuity.read(&Map::new());
        devices.desktop.policy.lock().unwrap().cross_device = false;
        mail(&devices.desktop, "With the switch off");
        devices.desktop.step();
        devices.desktop.policy.lock().unwrap().cross_device = true;
        mail(&devices.desktop, "Crosses");
        devices.until("the laptop showing the mail that crosses", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        assert_eq!(titles(&devices.laptop), ["Crosses"]);
        assert_eq!(devices.in_record(), 1, "the record keeps only the mail that crosses");
    }

    phase a_notification_replaced_on_the_desktop_updates_the_laptops_row;
    {
        let devices = two_devices();
        let id = mail(&devices.desktop, "New mail");
        devices.until("the laptop showing the mail", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        let first = devices.laptop.banner().id;
        let mut again = from_app(sent(id), "so.tilo.mail");
        again.summary = "Two new mails".into();
        devices.desktop.daemon.push(again);
        devices.until("the laptop showing the update", |devices| {
            devices.laptop.banner().title == "Two new mails"
        });
        assert_eq!(devices.laptop.desktop.log().active().len(), 1, "one row, not two");
        assert_eq!(devices.laptop.banner().id, first, "the same row shows the update");
    }
}

tilo_test_env::scenario! {
    fn what_the_person_does_with_it_on_one_device_holds_on_every_one;

    phase dismissed_on_the_laptop_it_is_gone_on_the_desktop;
    {
        let devices = two_devices();
        let id = mail(&devices.desktop, "New mail");
        devices.until("the laptop showing the mail", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        let shown = devices.laptop.banner().id;
        devices.laptop.desktop.dismiss_history(shown);
        devices.until("the desktop's mail closing", |devices| !devices.desktop.live(id));
        assert!(devices.desktop.heard().is_empty(), "dismissing it runs none of its actions");
        devices.until("every copy going", |devices| {
            titles(&devices.desktop).is_empty()
                && titles(&devices.laptop).is_empty()
                && devices.in_record() == 0
        });
    }

    phase choosing_an_action_on_the_laptop_runs_it_on_the_desktop;
    {
        let devices = two_devices();
        let id = mail(&devices.desktop, "New mail");
        devices.until("the laptop showing the mail", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        let shown = devices.laptop.banner().id;
        devices.laptop.desktop.invoke_action(shown, "archive");
        devices.until("the desktop's mail closing", |devices| !devices.desktop.live(id));
        assert_eq!(devices.desktop.heard(), [(id, "action archive".into())]);
        devices.until("every copy going", |devices| {
            titles(&devices.desktop).is_empty()
                && titles(&devices.laptop).is_empty()
                && devices.in_record() == 0
        });
    }

    phase dismissed_on_the_desktop_it_leaves_the_laptop;
    {
        let devices = two_devices();
        mail(&devices.desktop, "New mail");
        devices.until("the laptop showing the mail", |devices| {
            !devices.laptop.desktop.log().active().is_empty()
        });
        let shown = devices.desktop.banner().id;
        devices.desktop.ask(Ask::Banner(BannerAsk::Dismiss(shown)));
        devices.desktop.step();
        devices.desktop.desktop.dismiss_history(shown);
        devices.until("the laptop's copy going", |devices| {
            titles(&devices.laptop).is_empty() && devices.in_record() == 0
        });
        assert!(
            devices.laptop.desktop.log().history().is_empty(),
            "the laptop's centre keeps no copy either"
        );
    }
}

/// A member's device at another protocol version reaches the shell as an ordinary banner
/// naming it: the person reads what to update, not an operator's log line.
#[test]
fn a_device_on_another_version_is_named_on_the_shell() {
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
    assert_eq!(feed.banners.len(), 1, "it is a banner over the session");
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

/// The centre's reading says how long ago each notice arrived and whether its body may show, so a
/// view can write "3 min ago" and a banner can keep a secret: the rest of the reading is the
/// centre's own, which a reader that knows those fields reads as before.
#[test]
fn the_reading_says_how_long_ago_each_notice_arrived_and_whether_it_may_show() {
    let rig = rig();
    let id = rig.daemon.alloc_id();
    rig.daemon.push(sent(id));
    rig.step();
    let mut served = Served::new(Rc::clone(&rig.bridge));
    assert!(served.poll(), "what the centre keeps is a reading");
    let reading = served.read(RESOURCE).expect("the centre is published");
    let notice = reading
        .get("notices")
        .and_then(Value::as_array)
        .and_then(|notices| notices.first())
        .expect("the notice it keeps");
    assert!(
        notice.get("age_seconds").and_then(Value::as_u64).is_some(),
        "the notice says how long ago it arrived: {notice}"
    );
    assert_eq!(
        notice.get("preview").and_then(Value::as_bool),
        Some(true),
        "a sender that asked for a preview says so: {notice}"
    );
    // What the panel reads is unchanged: it knows neither field.
    let centre: NotificationCentre =
        serde_json::from_value(reading).expect("the centre still reads");
    assert_eq!(centre.count, 1);
}

/// Invoke one of the banners' verbs with `arguments`, as the shell's banner does.
fn invoke_banner(served: &mut Served, action: &str, arguments: serde_json::Value) {
    let arguments: Map<String, Value> =
        serde_json::from_value(arguments).expect("the verb's arguments");
    served
        .invoke(BANNERS, action, &arguments)
        .expect("the banners answer their own verbs");
}

/// A banner on screen offering `actions`, and the service publishing it.
fn banner_rig(actions: &[(&str, &str)]) -> (Rig, Served, u64) {
    let rig = rig();
    let mut note = sent(rig.daemon.alloc_id());
    note.actions = actions
        .iter()
        .map(|(key, label)| ((*key).to_owned(), (*label).to_owned()))
        .collect();
    rig.daemon.push(note);
    rig.step();
    let mut served = Served::new(Rc::clone(&rig.bridge));
    assert!(served.poll(), "what the banners show is a reading");
    let id = rig
        .feed()
        .banners
        .last()
        .expect("the notification is on screen")
        .notification
        .id;
    (rig, served, id)
}

/// The banner closes and opens through its published actions, each naming the row it read.
#[test]
fn a_banner_dismisses_and_opens_through_its_published_actions() {
    let (rig, mut served, id) = banner_rig(&[("open", "Open")]);
    invoke_banner(
        &mut served,
        "dismiss",
        serde_json::json!({ "banner": id.to_string() }),
    );
    rig.step();
    assert!(
        rig.feed().banners.is_empty(),
        "the banner left when it was dismissed"
    );

    let (rig, mut served, id) = banner_rig(&[("open", "Open")]);
    invoke_banner(
        &mut served,
        "open-banner",
        serde_json::json!({ "banner": id.to_string() }),
    );
    rig.step();
    let taken = rig.heard.taken.lock().unwrap().clone();
    assert!(
        taken.iter().any(|(_, said)| said == "action open"),
        "opening ran the action the sender offered: {taken:?}"
    );
}

/// A choice is named by its label as a person reads it, and a reply reaches the sender.
#[test]
fn a_banner_chooses_by_label_and_replies() {
    let (rig, mut served, id) = banner_rig(&[
        ("snooze", "Snooze"),
        (
            tilo_session_core::desktop::notifications::INLINE_REPLY,
            "Reply",
        ),
    ]);
    invoke_banner(
        &mut served,
        "act",
        serde_json::json!({ "banner": id.to_string(), "choice": "Snooze" }),
    );
    rig.step();
    let taken = rig.heard.taken.lock().unwrap().clone();
    assert!(
        taken.iter().any(|(_, said)| said == "action snooze"),
        "the label chose the key it names: {taken:?}"
    );

    let (rig, mut served, id) = banner_rig(&[(
        tilo_session_core::desktop::notifications::INLINE_REPLY,
        "Reply",
    )]);
    invoke_banner(
        &mut served,
        "reply",
        serde_json::json!({ "banner": id.to_string(), "text": "On it" }),
    );
    rig.step();
    let taken = rig.heard.taken.lock().unwrap().clone();
    assert!(
        taken.iter().any(|(_, said)| said == "reply On it"),
        "the reply reached the sender: {taken:?}"
    );
}

/// Snoozing runs the sender's snooze, reading takes the banner to the centre, and focusing says
/// which banner the keys act on.
#[test]
fn a_banner_snoozes_reads_and_focuses() {
    let (rig, mut served, id) = banner_rig(&[("snooze", "Snooze")]);
    invoke_banner(
        &mut served,
        "snooze",
        serde_json::json!({ "banner": id.to_string() }),
    );
    rig.step();
    let taken = rig.heard.taken.lock().unwrap().clone();
    assert!(
        taken.iter().any(|(_, said)| said == "action snooze"),
        "the snooze ran: {taken:?}"
    );

    let (_rig, mut served, id) = banner_rig(&[("open", "Open")]);
    invoke_banner(&mut served, "focus-newest", serde_json::json!({}));
    let reading = served.read(RESOURCE).expect("the centre is published");
    assert_eq!(
        reading.get("focused").and_then(Value::as_u64),
        Some(id),
        "the newest banner took the keyboard: {reading}"
    );

    let (rig, mut served, id) = banner_rig(&[("open", "Open")]);
    invoke_banner(
        &mut served,
        "read",
        serde_json::json!({ "banner": id.to_string() }),
    );
    rig.step();
    assert!(
        rig.feed().banners.is_empty(),
        "reading it took it off the screen into the centre"
    );
}

/// A ringing call is answered or declined from its banner: the key the caller offered runs.
/// A refusal — what the session posts when an action could not run — is a notice of its own kind,
/// and the centre's published dismiss takes it away like any other.
#[test]
fn a_refusal_is_dismissed_through_the_centres_published_action() {
    let rig = rig();
    rig.bridge.borrow().post(
        "Settings did not open",
        "It was not installed",
        "Session",
        "error",
    );
    rig.step();
    let mut served = Served::new(Rc::clone(&rig.bridge));
    assert!(served.poll());
    let reading = served.read(RESOURCE).expect("the centre is published");
    let notice = reading
        .get("notices")
        .and_then(Value::as_array)
        .and_then(|notices| notices.first())
        .expect("the refusal is kept");
    assert_eq!(
        notice.get("kind").and_then(Value::as_str),
        Some("error"),
        "it reads as a failure: {notice}"
    );
    let number = notice
        .get("number")
        .and_then(Value::as_u64)
        .expect("the refusal has a number");
    invoke(
        &mut served,
        "dismiss",
        serde_json::json!({ "notification": number }),
    );
    rig.step();
    assert!(served.poll(), "the centre changed");
    let kept = served.read(RESOURCE).expect("the centre is published");
    assert!(
        kept.get("notices")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "the dismissal took it away: {kept}"
    );
}

#[test]
fn a_call_is_answered_and_declined_from_its_banner() {
    for (verb, key) in [("answer-call", "answer"), ("decline-call", "decline")] {
        let rig = rig();
        let mut call = sent(rig.daemon.alloc_id());
        call.incoming_call = true;
        call.sender_name = Some("Mia".to_owned());
        call.contact_endpoint = Some("mia".to_owned());
        call.actions = [("answer", "Answer"), ("decline", "Decline")]
            .iter()
            .map(|(key, label)| ((*key).to_owned(), (*label).to_owned()))
            .collect();
        rig.daemon.push(call);
        rig.step();
        let mut served = Served::new(Rc::clone(&rig.bridge));
        assert!(served.poll());
        let id = rig
            .feed()
            .banners
            .last()
            .expect("the call rings on screen")
            .notification
            .id;
        invoke_banner(
            &mut served,
            verb,
            serde_json::json!({ "banner": id.to_string() }),
        );
        rig.step();
        let taken = rig.heard.taken.lock().unwrap().clone();
        assert!(
            taken
                .iter()
                .any(|(_, said)| said == &format!("action {key}")),
            "{verb} ran the caller's {key}: {taken:?}"
        );
    }
}
