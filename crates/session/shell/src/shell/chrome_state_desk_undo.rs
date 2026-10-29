//! Undo for a desk widget the person removed: the toast that offers it,
//! and the widgets an Undo asked back for the desk to restore.

use tilo_desktop::notifications::{Notification, NotificationIconKind, NotificationSeverity};

use super::ChromeState;

impl ChromeState {
    /// Offer Undo for a desk widget the person just removed: the toast
    /// names it and, activated before it lapses, asks the desk to bring
    /// it back where it was ([`Self::take_desk_restores`]).
    pub fn push_desk_undo_toast(
        &mut self,
        widget: settings_store::desk_widgets::WidgetId,
        title: String,
    ) -> u64 {
        use crate::shell::i18n::t;
        let mut toast = Notification::new(title, String::new())
            .with_severity(NotificationSeverity::Info)
            .with_icon_kind(NotificationIconKind::Generic);
        toast.dismiss_after = crate::shell::closed_tabs::UNDO_WINDOW;
        toast.actions = vec![("default".into(), t("desk-widget-undo"))];
        toast.bypass_dnd = true;
        toast.show_on_lock_screen = false;
        let id = self.push_notification_full(toast);
        self.desk_undo_toasts.insert(id, widget);
        id
    }

    /// The widgets an Undo asked back since the last call.
    pub fn take_desk_restores(&mut self) -> Vec<settings_store::desk_widgets::WidgetId> {
        std::mem::take(&mut self.desk_restores)
    }
}
