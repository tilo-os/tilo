//! Transient status: the result of a sync, an import or an edit, as a toast
//! in the window's top layer, until it is dismissed, replaced or has been
//! up for a notice's lifetime.

use vello::Scene;
use vello::kurbo::Affine;

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{self, ToastAnchor, ToastCard, ToastKind};

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

impl CalendarApp {
    /// Take the status down once it has been up for a notice's lifetime,
    /// counted from when it first showed; a new status starts its own.
    /// Whether it went.
    pub(super) fn expire_status(&mut self) -> bool {
        let Some(message) = &self.status else {
            self.status_shown = None;
            return false;
        };
        match &self.status_shown {
            Some((shown, at)) if shown == message => {
                if toast::transient_expired(*at, toast::NOTICE_LIFETIME) {
                    self.status = None;
                    self.status_shown = None;
                    return true;
                }
            }
            _ => self.status_shown = Some((message.clone(), ui::time::now())),
        }
        false
    }

    /// When the status shown leaves, for the app's wake deadline. Under the
    /// capture clock it stays.
    pub(super) fn status_deadline(&self) -> Option<ui::time::Instant> {
        if ui::time::wall_frozen() {
            return None;
        }
        let (shown, at) = self.status_shown.as_ref()?;
        (self.status.as_ref() == Some(shown)).then(|| *at + toast::NOTICE_LIFETIME)
    }

    pub(super) fn draw_status(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.expire_status();
        let Some(message) = self.status.clone() else {
            return;
        };
        let card = ToastCard::new(&message, kind_of(&message))
            .closable(true)
            .close_hovered(self.status_close.contains(self.last_mouse));
        // The toast keeps to the window's trailing corner, where it covers
        // the least of the calendar and never sits mid-grid.
        let hit = toast::paint_transient(
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
        use ui::time::testing::with_monotonic_time;
        let mut app = CalendarApp::new_detached();
        let start = ui::time::now();
        let at = |seconds: u64| start + std::time::Duration::from_secs(seconds);
        app.status = Some("Event deleted".into());
        with_monotonic_time(start, || assert!(!app.expire_status()));
        assert_eq!(app.status_deadline(), Some(start + toast::NOTICE_LIFETIME));
        with_monotonic_time(at(2), || {
            app.status = Some("Imported calendar items".into());
            assert!(!app.expire_status());
        });
        with_monotonic_time(at(5), || {
            assert!(!app.expire_status(), "the newer status keeps its time");
        });
        with_monotonic_time(at(7), || assert!(app.expire_status()));
        assert_eq!(app.status, None);
        assert_eq!(app.status_deadline(), None);
    }
}
