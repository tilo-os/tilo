//! Transient status: the result of a sync, an import or an edit, as the
//! window bar's notice until it is replaced or has been up for a notice's
//! lifetime (the shared [`ui::widget::toast::Transient`] keeps it).

use ui::widget::toast::ToastKind;
use ui::window_toolbar::ToolbarNotice;

use super::CalendarApp;

fn kind_of(message: &str) -> ToastKind {
    if message.starts_with("Sync failed") || message.contains("failed") {
        ToastKind::Error
    } else if message.starts_with("Offline") || message.contains("changed on the server") {
        ToastKind::Warning
    } else {
        ToastKind::Info
    }
}

impl CalendarApp {
    /// Keep the window bar's notice saying the status while it lasts:
    /// "Syncing calendars…" for as long as the sync runs.
    pub(super) fn publish_status(&mut self) {
        self.status.keep_while(self.syncing);
        self.status.expire();
        self.status
            .publish(|message| ToolbarNotice::new(message.clone(), kind_of(message)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_takes_the_severity_it_reports() {
        assert_eq!(kind_of("Sync failed. Check the account."), ToastKind::Error);
        assert_eq!(
            kind_of("Offline. Changes sync when you reconnect."),
            ToastKind::Warning
        );
        assert_eq!(kind_of("Event deleted"), ToastKind::Info);
    }

    /// A failed sync is an error notice in the window's bar.
    #[test]
    fn a_failed_sync_is_an_error_in_the_window_bar() {
        let mut app = CalendarApp::new_detached();
        app.status.show("Sync failed. Check the account.".into());
        app.publish_status();
        let notice = ui::window_toolbar::notice().expect("the bar shows the status");
        assert_eq!(notice.kind, ToastKind::Error);
        assert_eq!(notice.text, "Sync failed. Check the account.");
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
        with_monotonic_time(start, || app.status.show("Event deleted".into()));
        let due = start + ui::widget::toast::NOTICE_LIFETIME;
        assert_eq!(app.status.deadline(), Some(due));
        assert!(
            app.next_wake_deadline().is_some_and(|wake| wake <= due),
            "the app wakes when the status is due to leave"
        );
        with_monotonic_time(at(2), || {
            app.status.show("Imported calendar items".into());
        });
        with_monotonic_time(at(5), || {
            assert!(!app.status.expire(), "the newer status keeps its time");
            assert_eq!(app.status.as_deref(), Some("Imported calendar items"));
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
}
