//! Transient status: the result of a sync, an import or an edit, as the
//! window bar's notice until it is replaced or has been up for a notice's
//! lifetime (the shared [`ui::widget::toast::Transient`] keeps it).
//!
//! Each message carries its own severity from where it is raised, so what
//! the person reads as a failure is a failure in every language: the words
//! are the catalogue's, and nothing decides a kind by looking for "failed"
//! in them.

use ui::time::Instant;
use ui::widget::toast::{ToastKind, Transient};
use ui::window_toolbar::{ToolbarAction, ToolbarNotice};

use super::CalendarApp;

/// The one message the window's bar says, with the severity its reason
/// carries.
#[derive(Default)]
pub(super) struct Status {
    current: Transient<(String, ToastKind)>,
}

impl Status {
    /// Say `text` of `kind`, replacing anything already up.
    pub(super) fn show(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.current.show((text.into(), kind));
    }

    /// Say `text` as a failure: what the person asked for did not happen.
    pub(super) fn failed(&mut self, text: impl Into<String>) {
        self.show(text, ToastKind::Error);
    }

    /// Say `text` as work under way: it stays while the work runs.
    pub(super) fn show_progress(&mut self, text: impl Into<String>) {
        self.current.show_progress((text.into(), ToastKind::Info));
    }

    /// The message up, if one is.
    pub(super) fn text(&self) -> Option<&str> {
        self.current.get().map(|(text, _)| text.as_str())
    }

    #[cfg(test)]
    pub(super) fn is_shown(&self) -> bool {
        self.current.is_shown()
    }

    pub(super) fn dismiss(&mut self) -> bool {
        self.current.dismiss()
    }

    pub(super) fn expire(&mut self) -> bool {
        self.current.expire()
    }

    pub(super) fn keep_while(&mut self, busy: bool) -> bool {
        self.current.keep_while(busy)
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.current.deadline()
    }

    /// Put the message up in the window's bar, `decorate` adding what the
    /// person may do about it; call on every sync.
    fn publish(&self, decorate: impl FnOnce(ToolbarNotice) -> ToolbarNotice) {
        self.current
            .publish(|(text, kind)| decorate(ToolbarNotice::new(text.clone(), *kind)));
    }
}

/// The bar action id of a failed sync's "Sync again".
pub(super) const SYNC_AGAIN_ACTION: &str = "calendar.notice.sync-again";
/// The bar action id of "Open Settings" beside a status about accounts.
pub(super) const OPEN_SETTINGS_ACTION: &str = "calendar.notice.open-settings";
/// Where calendar accounts are added, changed and removed: Settings'
/// External Accounts, the one place for them.
pub(super) const ACCOUNT_SETTINGS_LINK: &str = "settings://external_accounts";

/// What a status that asks the person to act lets them do from the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusAction {
    /// Sync the calendars again.
    SyncAgain,
    /// Open the accounts in Settings.
    OpenSettings,
}

/// A failed sync: try again, or fix the account first.
pub(super) const SYNC_FAILED_ACTIONS: &[StatusAction] =
    &[StatusAction::SyncAgain, StatusAction::OpenSettings];

/// What a sync that ended as `outcome` lets the person do: nothing once it
/// synced; add an account, or sync again, when it reached no server; sync
/// again or check the account when it failed.
pub(super) fn after_sync(
    outcome: &crate::caldav::SyncOutcome,
    no_accounts: bool,
) -> &'static [StatusAction] {
    use crate::caldav::SyncOutcome;
    match outcome {
        SyncOutcome::Synced { .. } => &[],
        SyncOutcome::Offline if no_accounts => &[StatusAction::OpenSettings],
        SyncOutcome::Offline => &[StatusAction::SyncAgain],
        SyncOutcome::Failed(_) => SYNC_FAILED_ACTIONS,
    }
}

/// The actions of the status up, kept with the status they belong to, so a
/// later status without any shows none.
#[derive(Default)]
pub(super) struct StatusActions {
    shown: Option<(String, Vec<StatusAction>)>,
    /// Links opened from a notice, as the host was asked to open them.
    #[cfg(test)]
    opened_links: Vec<&'static str>,
}

impl StatusAction {
    fn button(self) -> ToolbarAction {
        match self {
            Self::SyncAgain => ToolbarAction::new(SYNC_AGAIN_ACTION, "refresh", "Sync again"),
            Self::OpenSettings => {
                ToolbarAction::new(OPEN_SETTINGS_ACTION, "settings", "Open Settings")
            }
        }
    }
}

impl CalendarApp {
    /// Show `text` of `kind`, which asks the person to act, with `actions`
    /// beside it.
    pub(super) fn show_status_with(
        &mut self,
        text: String,
        kind: ToastKind,
        actions: &[StatusAction],
    ) {
        self.status.show(text.clone(), kind);
        self.status_actions.shown = (!actions.is_empty()).then(|| (text, actions.to_vec()));
        self.dirty = true;
    }

    /// The actions of the status up.
    fn status_actions(&self) -> &[StatusAction] {
        let Some(shown) = self.status.text() else {
            return &[];
        };
        self.status_actions
            .shown
            .as_ref()
            .filter(|(text, _)| text == shown)
            .map_or(&[], |(_, actions)| actions.as_slice())
    }

    /// Run an action pressed beside the status in the bar: sync again, or
    /// open the accounts in Settings.
    pub(super) fn take_status_actions(&mut self) {
        let actions = self.status_actions().to_vec();
        for action in actions {
            let id = match action {
                StatusAction::SyncAgain => SYNC_AGAIN_ACTION,
                StatusAction::OpenSettings => OPEN_SETTINGS_ACTION,
            };
            if !ui::window_toolbar::take_notice_press(id) {
                continue;
            }
            match action {
                StatusAction::SyncAgain => {
                    if !self.syncing {
                        self.sync_requested = true;
                    }
                }
                // To where calendar accounts are kept, through the desktop's
                // `settings://` handler.
                StatusAction::OpenSettings => {
                    #[cfg(test)]
                    self.status_actions.opened_links.push(ACCOUNT_SETTINGS_LINK);
                    #[cfg(not(test))]
                    ui::widget::open_external_url(ACCOUNT_SETTINGS_LINK);
                }
            }
            self.status.dismiss();
            self.dirty = true;
        }
    }

    /// Say `text` of a change just submitted once it is kept: at once on
    /// this device alone, once the room accepts it in a shared calendar,
    /// and never when the room refuses it ([`Self::announce_answers`]).
    pub(super) fn say_when_kept(&mut self, text: impl Into<String>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.say_when_kept(text);
        } else {
            self.status.show(text.into(), ToastKind::Info);
        }
    }

    /// Show what the room answered of this window's changes: a change it
    /// kept is said done ("Added Buy milk"), and one it refused, which the
    /// record withdrew, says why instead.
    pub(super) fn announce_answers(&mut self) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let said = dispatcher.said();
        if let Some(text) = said.with(Clone::clone) {
            said.set(None);
            self.status.show(text, ToastKind::Info);
            self.dirty = true;
        }
        let refused = dispatcher.refused();
        let Some(refusal) = refused.with(Clone::clone) else {
            return;
        };
        refused.set(None);
        self.status.failed(crate::resource::t_args(
            "calendar-change-refused",
            &[("reason", ui::i18n::ArgValue::from(refusal.reason))],
        ));
        self.dirty = true;
    }

    /// Whether a calendar was just shown or hidden, in words.
    pub(super) fn visibility_status(visible: bool) -> String {
        crate::resource::t(if visible {
            "calendar-visible"
        } else {
            "calendar-hidden"
        })
    }

    /// Keep the window bar's notice saying the status while it lasts:
    /// "Syncing calendars…" for as long as the sync runs.
    pub(super) fn publish_status(&mut self) {
        self.status.keep_while(self.syncing);
        self.status.expire();
        let actions = self.status_actions().to_vec();
        self.status.publish(|notice| {
            actions
                .iter()
                .fold(notice, |notice, action| notice.action(action.button()))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure is an error notice in the window's bar because the place
    /// that raised it said so, not because of the words it chose.
    #[test]
    fn a_message_says_the_severity_it_was_raised_with() {
        let mut app = CalendarApp::new_detached();
        app.status.failed("Sync failed. Check the account.");
        app.publish_status();
        let notice = ui::window_toolbar::notice().expect("the bar shows the status");
        assert_eq!(notice.kind, ToastKind::Error);
        assert_eq!(notice.text, "Sync failed. Check the account.");

        // The same words raised as a passing confirmation read as one.
        app.status
            .show("Sync failed. Check the account.", ToastKind::Info);
        app.publish_status();
        let notice = ui::window_toolbar::notice().expect("the bar shows the status");
        assert_eq!(notice.kind, ToastKind::Info);
    }

    /// A status leaves on its own once it has been up for a notice's
    /// lifetime, and the app wakes for it; a newer status starts its own.
    #[test]
    fn a_status_leaves_on_its_own() {
        use ui::Widget;
        use ui::time::testing::with_monotonic_time;
        let mut app = CalendarApp::new_detached();
        let start = ui::time::now();
        let at = |seconds: u64| start + std::time::Duration::from_secs(seconds);
        with_monotonic_time(start, || {
            app.status.show("Event deleted", ToastKind::Info);
        });
        let due = start + ui::widget::toast::NOTICE_LIFETIME;
        assert_eq!(app.status.deadline(), Some(due));
        assert!(
            app.next_wake_deadline().is_some_and(|wake| wake <= due),
            "the app wakes when the status is due to leave"
        );
        with_monotonic_time(at(2), || {
            app.status.show("Imported calendar items", ToastKind::Info);
        });
        with_monotonic_time(at(5), || {
            assert!(!app.status.expire(), "the newer status keeps its time");
            assert_eq!(app.status.text(), Some("Imported calendar items"));
        });
        with_monotonic_time(at(5), || app.publish_status());
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.text),
            Some("Imported calendar items".to_owned()),
            "the window bar says the status"
        );
        with_monotonic_time(at(7), || app.publish_status());
        assert!(!app.status.is_shown());
        assert_eq!(app.status.deadline(), None, "nothing left to wake for");
        assert_eq!(
            ui::window_toolbar::notice(),
            None,
            "the gone status leaves the window bar"
        );
    }

    /// "Syncing…" stays for as long as the sync runs, past a notice's
    /// lifetime, and goes once the sync is over.
    #[test]
    fn syncing_stays_while_the_sync_runs() {
        use ui::time::testing::with_monotonic_time;
        let mut app = CalendarApp::new_detached();
        let start = ui::time::now();
        let at = |seconds: u64| start + std::time::Duration::from_secs(seconds);
        let draw = |app: &mut CalendarApp| {
            app.publish_status();
            ui::window_toolbar::notice().map(|notice| notice.text)
        };
        with_monotonic_time(start, || app.shots_set_sync_state("syncing"));
        for second in [3, 6, 9] {
            with_monotonic_time(at(second), || {
                assert_eq!(
                    draw(&mut app).as_deref(),
                    Some("Syncing calendars…"),
                    "still syncing at {second}s"
                );
            });
        }
        assert_eq!(
            app.status.deadline(),
            Some(at(9) + ui::widget::toast::NOTICE_LIFETIME),
            "the app wakes to look again"
        );
        app.syncing = false;
        with_monotonic_time(at(10), || {
            assert_eq!(draw(&mut app), None, "the sync is over");
        });
    }

    type Window = ui::framework::testing::TestApp<crate::resource::CalendarShell>;

    /// The calendar window with a failed sync's status in its bar.
    fn failed_sync() -> Window {
        let mut window =
            Window::capture_at(Some("month"), (1280, 800)).expect("the calendar opens");
        window.harness().present_in_window_bar();
        window
            .surface_mut::<CalendarApp>()
            .shots_set_sync_state("error");
        window.settle();
        window
    }

    fn press(window: &mut Window, width: f64, index: usize, semantic: bool) {
        let harness = window.harness();
        let pressed = if semantic {
            harness.invoke_window_notice_action(width, index)
        } else {
            harness.click_window_notice_action(width, index)
        };
        pressed.expect("the notice's action is in the bar");
        window.settle();
    }

    const WAYS: [(f64, bool); 4] = [
        (1280.0, false),
        (1280.0, true),
        (420.0, false),
        (420.0, true),
    ];

    /// A failed sync asks to sync again or to check the account, and carries
    /// both, in that order, wide or compact.
    #[test]
    fn a_failed_sync_carries_sync_again_and_open_settings() {
        let mut window = failed_sync();
        for width in [1280.0, 420.0] {
            let names: Vec<String> = window
                .harness()
                .window_bar_notice(width)
                .expect("the failure is up")
                .buttons
                .into_iter()
                .map(|button| button.name)
                .collect();
            assert_eq!(names, ["Sync again", "Open Settings"], "at {width} px");
        }
    }

    /// Sync again, by pointer or through its semantic action, syncs again.
    #[test]
    fn a_failed_syncs_sync_again_syncs() {
        for (width, semantic) in WAYS {
            let mut window = failed_sync();
            press(&mut window, width, 0, semantic);
            let app = window.surface_mut::<CalendarApp>();
            let status = app.status.text().unwrap_or_default().to_owned();
            assert!(
                status.starts_with("Synced"),
                "the calendars synced again at {width} px (semantic: {semantic}): {status}"
            );
            assert!(
                app.status_actions.opened_links.is_empty(),
                "Sync again stays in Calendar"
            );
        }
    }

    /// Open Settings, by pointer or through its semantic action, opens the
    /// accounts in Settings.
    #[test]
    fn a_failed_syncs_open_settings_opens_the_accounts() {
        for (width, semantic) in WAYS {
            let mut window = failed_sync();
            press(&mut window, width, 1, semantic);
            let app = window.surface_mut::<CalendarApp>();
            assert_eq!(
                app.status_actions.opened_links,
                [ACCOUNT_SETTINGS_LINK],
                "at {width} px (semantic: {semantic})"
            );
            assert!(!app.status.is_shown(), "the notice goes once acted on");
        }
    }
}
