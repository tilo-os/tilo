//! The reminders of the workspace, as the platform's notifications.
//!
//! A type that reminds says so by its fields' roles, and [`tilolang_tilo::reminders`] reads what
//! the workspace's records hold of them for one person. This is what a live window does with
//! that: it shows the reminders whose instant has come as notifications, withdraws the ones that
//! are no longer due, and keeps the instant the next one comes, so the window wakes then and
//! nothing here polls.
//!
//! A notification is the item's own: its title names the item, a click opens it where its app
//! shows it, and the one-step actions its type declares are the notification's buttons, run by
//! the window that shows the app. What the person clicked comes back to that window as an
//! [`Asked`], on whichever thread the platform reported it.
//!
//! One reminder tells one person once, on one of their devices: whichever window shows it keeps a
//! mark in the workspace's record of each person's own state ([`tilolang_tilo::personal`]), and
//! the person's other devices read the mark before they would show it. A reminder whose instant
//! passed while none of their devices ran the app comes when one does, saying it is late. So does
//! a reminder whose device was asleep: the notification names when the item is due, which is
//! still the fact.
//!
//! Reminders are delivered while the app's window runs on one of the person's devices. A device
//! that shows no window of the app delivers nothing until it does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use chrono::{DateTime, Local, Utc};
use tilolang_tilo::reminders::{Alarm, Look};
use ui::notify::{MAX_BUTTONS, Notification, Notifier};

/// The scheme of a reminder's activation: a click opens the item, and a button runs its action.
pub(crate) const SCHEME: &str = "tilo-tilolang";

/// How long past its instant a reminder still reads as coming, rather than as late: a window
/// wakes a moment after the instant it asked for.
const GRACE_SECS: i64 = 60;

/// What the person asked of a reminder's notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Show what a reminder is about.
    Open { app: String, resource: String },
    /// Run an action the notification offered on what a reminder is about.
    Run {
        app: String,
        resource: String,
        action: String,
    },
}

impl Asked {
    /// The package whose resource the ask is about.
    pub(crate) fn app(&self) -> &str {
        match self {
            Self::Open { app, .. } | Self::Run { app, .. } => app,
        }
    }
}

/// Every ask a reminder's notification reported that no window took yet.
fn asked() -> &'static Mutex<Vec<Asked>> {
    static ASKED: OnceLock<Mutex<Vec<Asked>>> = OnceLock::new();
    ASKED.get_or_init(Mutex::default)
}

/// The windows that wake when a reminder is clicked.
fn waking() -> &'static Mutex<Vec<ui::WakeHandle>> {
    static WAKING: OnceLock<Mutex<Vec<ui::WakeHandle>>> = OnceLock::new();
    WAKING.get_or_init(Mutex::default)
}

/// Take the platform's reminder clicks, once per process: each goes to the windows, which take
/// the ones of their own app.
fn listen() {
    static LISTENING: OnceLock<()> = OnceLock::new();
    LISTENING.get_or_init(|| {
        ui::notify::set_activation_handler(SCHEME, |uri| {
            let Some(ask) = asked_of(uri) else {
                return;
            };
            asked()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(ask);
            for wake in waking()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
            {
                wake.wake();
            }
        });
    });
}

/// The ask the activation URI `uri` names: what a click opens, or the action a button runs.
fn asked_of(uri: &str) -> Option<Asked> {
    let rest = uri.strip_prefix(SCHEME)?.strip_prefix(':')?;
    let (verb, rest) = rest.split_once('/')?;
    let (app, rest) = rest.split_once('/')?;
    let app = ui::uri::decode(app);
    match verb {
        "open" => Some(Asked::Open {
            app,
            resource: ui::uri::decode(rest),
        }),
        "run" => {
            let (resource, action) = rest.rsplit_once('/')?;
            Some(Asked::Run {
                app,
                resource: ui::uri::decode(resource),
                action: ui::uri::decode(action),
            })
        }
        _ => None,
    }
}

/// The window's half of the reminders: the notifications it showed, and what the person asked of
/// them.
pub(crate) struct Ringer {
    /// The package whose reminders this window rings: a click of another app's notification is
    /// that app's window's.
    app: String,
    notifier: Arc<dyn Notifier>,
    /// The notifications shown and not withdrawn, by the key of the reminder each tells of.
    shown: BTreeMap<String, String>,
    /// The instant the next reminder of this person's comes, when one is coming.
    next: Option<DateTime<Utc>>,
}

impl Ringer {
    /// The reminders of the package `app`, shown through `notifier`; a click wakes the window
    /// through `wake`.
    pub(crate) fn new(app: &str, notifier: Arc<dyn Notifier>, wake: ui::WakeHandle) -> Self {
        listen();
        let mut waking = waking().lock().unwrap_or_else(PoisonError::into_inner);
        // A window that closed wakes nobody: its handle goes with it.
        waking.retain(ui::WakeHandle::is_active);
        waking.push(wake);
        drop(waking);
        Self {
            app: app.to_owned(),
            notifier,
            shown: BTreeMap::new(),
            next: None,
        }
    }

    /// The instant the next reminder of this person's comes, when one is coming.
    pub(crate) fn next(&self) -> Option<DateTime<Utc>> {
        self.next
    }

    /// How long until the next reminder comes, for a window that wakes then.
    pub(crate) fn wakes_in(&self, now: DateTime<Utc>) -> Option<std::time::Duration> {
        let wait = self
            .next?
            .timestamp()
            .saturating_sub(now.timestamp())
            .max(0);
        Some(std::time::Duration::from_secs(
            u64::try_from(wait).unwrap_or_default(),
        ))
    }

    /// Show what `look` says is due and not shown yet, withdraw the notifications of reminders
    /// that are no longer due, and keep when the next one comes. Answers the reminders shown,
    /// which the window keeps as told so no device of the person's shows them again.
    pub(crate) fn ring(&mut self, look: &Look, now: DateTime<Utc>) -> Vec<Alarm> {
        self.next = look.next;
        let due: BTreeSet<&str> = look.due.iter().map(|alarm| alarm.key.as_str()).collect();
        let gone: Vec<String> = self
            .shown
            .keys()
            .filter(|key| !due.contains(key.as_str()))
            .cloned()
            .collect();
        for key in gone {
            let Some(tag) = self.shown.remove(&key) else {
                continue;
            };
            if let Err(error) = self.notifier.withdraw(&tag) {
                tracing::debug!(%error, "a reminder's notification stays");
            }
        }
        let mut rung = Vec::new();
        for alarm in &look.due {
            if self.shown.contains_key(&alarm.key) {
                continue;
            }
            if let Err(error) = self.notifier.show(&self.notification(alarm, now)) {
                tracing::warn!(%error, app = self.app, "a reminder was not shown");
                continue;
            }
            self.shown.insert(alarm.key.clone(), alarm.tag());
            rung.push(alarm.clone());
        }
        rung
    }

    /// What the person asked of this window's notifications since it last looked.
    pub(crate) fn take(&self) -> Vec<Asked> {
        let mut queue = asked().lock().unwrap_or_else(PoisonError::into_inner);
        let (mine, others) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|asked| asked.app() == self.app);
        *queue = others;
        mine
    }

    /// The notification of `alarm`: what it is about, when it is due, which app it is from, and
    /// what the person can do about it there.
    fn notification(&self, alarm: &Alarm, now: DateTime<Utc>) -> Notification {
        let mut notification =
            Notification::new(alarm.tag(), alarm.title.clone(), Self::body(alarm, now))
                .with_activation(self.activation("open", &alarm.resource(), None))
                .with_action(tilo_ui_host::i18n::t("tilolang-reminder-open"));
        for offer in alarm.actions.iter().take(MAX_BUTTONS) {
            notification = notification.with_button(
                offer.label.clone(),
                self.activation("run", &alarm.resource(), Some(&offer.id)),
            );
        }
        notification
    }

    /// What the notification says under the title, one fact a line: when the item is due or —
    /// for a reminder that came after its instant, a device that was off past it — that it is
    /// late and the time it was due, then the app it is from.
    fn body(alarm: &Alarm, now: DateTime<Utc>) -> String {
        let late = now
            .timestamp()
            .checked_sub(alarm.at.timestamp())
            .is_some_and(|past| past > GRACE_SECS);
        let when = if late {
            ui::clock::time_of_day(&alarm.deadline.with_timezone(&Local))
        } else {
            let ahead = alarm.deadline.timestamp().saturating_sub(now.timestamp());
            ui::format::relative_ahead(ahead)
        };
        let mut lines = Vec::new();
        if late {
            lines.push(tilo_ui_host::i18n::t("tilolang-reminder-late"));
        }
        lines.push(when);
        lines.push(alarm.app_title.clone());
        lines.join("\n")
    }

    /// The activation URI that asks `verb` of `resource`, with `action` where it runs one.
    fn activation(&self, verb: &str, resource: &str, action: Option<&str>) -> String {
        let app = ui::uri::encode_component(&self.app);
        let resource = ui::uri::encode_component(resource);
        match action {
            Some(action) => format!(
                "{SCHEME}:{verb}/{app}/{resource}/{}",
                ui::uri::encode_component(action)
            ),
            None => format!("{SCHEME}:{verb}/{app}/{resource}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilolang_tilo::reminders::Offer;
    use ui::notify::conformance::Recording;

    const APP: &str = "t.remind";

    fn alarm(key: &str, at: DateTime<Utc>) -> Alarm {
        Alarm {
            app: APP.to_owned(),
            app_title: "Remind".to_owned(),
            path: "board/1".to_owned(),
            ty: "t.remind.Task".to_owned(),
            person: "bo".to_owned(),
            at,
            deadline: at
                .checked_add_signed(chrono::TimeDelta::minutes(30))
                .unwrap(),
            title: "Call the landlord".to_owned(),
            actions: vec![
                Offer {
                    id: "finish".to_owned(),
                    label: "Done".to_owned(),
                },
                Offer {
                    id: "snooze".to_owned(),
                    label: "Snooze".to_owned(),
                },
            ],
            key: key.to_owned(),
        }
    }

    fn due(alarm: Alarm) -> Look {
        Look {
            due: vec![alarm],
            next: None,
        }
    }

    fn instant(hour: u32, minute: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 27)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_utc()
    }

    fn ringing(app: &str) -> (Ringer, Arc<Recording>) {
        let notes = Arc::new(Recording::default());
        let ringer = Ringer::new(
            app,
            Arc::clone(&notes) as Arc<dyn Notifier>,
            ui::WakeHandle::new(),
        );
        (ringer, notes)
    }

    #[test]
    fn a_reminder_shows_once_with_what_it_is_about_and_what_the_item_offers() {
        let (mut ringer, notes) = ringing(APP);
        let reminder = alarm("board/1@1793080800", instant(9, 30));
        let rung = ringer.ring(&due(reminder.clone()), instant(9, 30));
        assert_eq!(rung.len(), 1, "shown once");
        let shown = notes.shown();
        assert_eq!(shown.len(), 1, "{shown:#?}");
        assert_eq!(shown[0].tag, reminder.tag());
        assert_eq!(shown[0].title, "Call the landlord");
        assert_eq!(
            shown[0].body.lines().last(),
            Some("Remind"),
            "what it is from, on a line of its own: {}",
            shown[0].body
        );
        assert_eq!(
            shown[0].body.lines().count(),
            2,
            "in 30 min, then the app: {}",
            shown[0].body
        );
        assert_eq!(
            shown[0].activation.as_deref(),
            Some(
                format!(
                    "{SCHEME}:open/t.remind/{}",
                    ui::uri::encode_component(&reminder.resource())
                )
                .as_str()
            )
        );
        let labels: Vec<&str> = shown[0]
            .buttons
            .iter()
            .map(|button| button.label.as_str())
            .collect();
        assert_eq!(labels, vec!["Done", "Snooze"], "the item's own actions");
        assert!(
            shown[0].buttons[0].activation.contains("/finish"),
            "{}",
            shown[0].buttons[0].activation
        );
        // The second look over the same reminder shows nothing new.
        assert!(ringer.ring(&due(reminder), instant(9, 31)).is_empty());
        assert_eq!(notes.shown().len(), 1);
    }

    #[test]
    fn a_reminder_that_is_no_longer_due_has_its_notification_taken_back() {
        let (mut ringer, notes) = ringing(APP);
        let reminder = alarm("board/1@1793080800", instant(9, 30));
        ringer.ring(&due(reminder.clone()), instant(9, 30));
        let empty = Look::default();
        assert!(ringer.ring(&empty, instant(9, 31)).is_empty());
        assert_eq!(notes.withdrawn(), vec![reminder.tag()]);
    }

    #[test]
    fn a_reminder_that_comes_after_its_instant_says_it_is_late() {
        let (mut ringer, notes) = ringing(APP);
        let reminder = alarm("board/1@1793080800", instant(9, 30));
        ringer.ring(&due(reminder), instant(11, 0));
        let body = notes.shown()[0].body.clone();
        assert!(
            body.contains("Late"),
            "the person is told it is late: {body}"
        );
        assert!(body.contains("10:00"), "and when the item was due: {body}");
    }

    #[test]
    fn a_click_opens_the_item_and_a_button_runs_its_action_in_the_window_of_its_app() {
        let (ringer, _notes) = ringing(APP);
        let reminder = alarm("board/1@1793080800", instant(9, 30));
        let open = format!(
            "{SCHEME}:open/t.remind/{}",
            ui::uri::encode_component(&reminder.resource())
        );
        let run = format!(
            "{SCHEME}:run/t.remind/{}/finish",
            ui::uri::encode_component(&reminder.resource())
        );
        ui::notify::report_activation(&open);
        ui::notify::report_activation(&run);
        let taken = ringer.take();
        assert_eq!(
            taken,
            vec![
                Asked::Open {
                    app: APP.to_owned(),
                    resource: reminder.resource(),
                },
                Asked::Run {
                    app: APP.to_owned(),
                    resource: reminder.resource(),
                    action: "finish".to_owned(),
                },
            ]
        );
        assert!(ringer.take().is_empty(), "an ask is taken once");
        // Another app's window takes none of this app's clicks.
        let (other, _notes) = ringing("t.other");
        ui::notify::report_activation(&open);
        assert!(other.take().is_empty());
        assert_eq!(ringer.take().len(), 1);
    }

    #[test]
    fn the_window_wakes_at_the_next_reminder_before_it_comes() {
        let (mut ringer, _notes) = ringing(APP);
        let later = alarm("board/1@1793080800", instant(9, 30));
        ringer.ring(
            &Look {
                due: Vec::new(),
                next: Some(later.at),
            },
            instant(9, 0),
        );
        assert_eq!(
            ringer.wakes_in(instant(9, 28)),
            Some(std::time::Duration::from_secs(120))
        );
        assert_eq!(
            ringer.wakes_in(instant(9, 40)),
            Some(std::time::Duration::ZERO),
            "an instant that came wakes it now"
        );
    }
}
