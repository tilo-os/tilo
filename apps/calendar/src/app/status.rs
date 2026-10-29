//! Transient status: the result of a sync, an import or an edit, as a toast
//! in the window's top layer, until it is dismissed, replaced or has been
//! up for a notice's lifetime (the shared [`ui::widget::toast::Transient`] keeps it).

use vello::Scene;
use vello::kurbo::Affine;

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind};

use super::{Action, CalendarApp};

fn kind_of(message: &str) -> ToastKind {
    if message.starts_with("Sync failed") || message.contains("failed") {
        ToastKind::Error
    } else if message.starts_with("Offline") || message.contains("changed on the server") {
        ToastKind::Warning
    } else {
        ToastKind::Info
    }
}

/// What a finished export says it saved: counts in words, each noun
/// agreeing with its count.
pub(super) fn exported_summary(events: usize, tasks: usize) -> String {
    let counted =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match (events, tasks) {
        (_, 0) => format!("Exported {}", counted(events, "event", "events")),
        (0, _) => format!("Exported {}", counted(tasks, "task", "tasks")),
        _ => format!(
            "Exported {} and {}",
            counted(events, "event", "events"),
            counted(tasks, "task", "tasks")
        ),
    }
}

impl CalendarApp {
    pub(super) fn draw_status(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.status.keep_while(self.syncing);
        self.status.expire();
        let Some(message) = self.status.get() else {
            return;
        };
        let card = ToastCard::new(message, kind_of(message))
            .closable(true)
            .close_hovered(self.status_close.contains(self.last_mouse));
        // The toast keeps to the window's trailing corner, where it covers
        // the least of the calendar and never sits mid-grid.
        let hit = self.status.paint(
            scene,
            t,
            text,
            theme,
            self.rect,
            ToastAnchor::BottomRight,
            &card,
        );
        self.status_close = hit.close_rect;
        if hit.card_rect.area() > 0.0 {
            self.hits.push((hit.card_rect, Action::Noop));
        }
        if hit.close_rect.area() > 0.0 {
            self.hits.push((hit.close_rect, Action::DismissStatus));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_says_what_it_saved_in_words() {
        assert_eq!(exported_summary(3, 2), "Exported 3 events and 2 tasks");
        assert_eq!(exported_summary(1, 1), "Exported 1 event and 1 task");
        assert_eq!(exported_summary(4, 0), "Exported 4 events");
        assert_eq!(exported_summary(0, 1), "Exported 1 task");
    }

    #[test]
    fn a_message_takes_the_severity_it_reports() {
        assert_eq!(kind_of("Sync failed. Check the account."), ToastKind::Error);
        assert_eq!(
            kind_of("Offline. Changes sync when you reconnect."),
            ToastKind::Warning
        );
        assert_eq!(kind_of("Event deleted"), ToastKind::Info);
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
        with_monotonic_time(at(7), || {
            app.draw_status(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::default(),
            );
        });
        assert!(!app.status.is_shown());
        assert_eq!(app.status.deadline(), None, "nothing left to wake for");
        assert!(
            !app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::DismissStatus)),
            "the gone status leaves no close button behind"
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
            app.hits.clear();
            app.draw_status(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::default(),
            );
            app.status.as_deref().map(str::to_owned)
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
