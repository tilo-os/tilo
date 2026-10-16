//! Transient status: the result of a sync, an import or an edit, as a toast
//! in the window's top layer, until it is dismissed or replaced.

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
    pub(super) fn draw_status(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
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
}
