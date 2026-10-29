//! What a row of an app's action wears: the action's own icon, as the app
//! declared it, else the app's icon and picture, live or retained alike.

use super::LiveAppActionsProvider;

impl LiveAppActionsProvider {
    /// What `read` says of the published action behind row `id`, when the
    /// retained snapshot holds it.
    pub(super) fn published_entry<T>(
        &self,
        id: &str,
        read: impl FnOnce(&super::PublishedAction) -> T,
    ) -> Option<T> {
        let snapshot = self.surface_snapshot.borrow();
        snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.entry(id))
            .map(|(_, action)| read(action))
    }

    /// The icon and picture of row `id`.
    pub(super) fn row_picture(
        &self,
        id: &str,
    ) -> (Option<ui::icons::Icon>, Option<ui::icons::AppImage>) {
        let declared = {
            let snapshot = self.surface_snapshot.borrow();
            snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.entry(id))
                .and_then(|(_, action)| action.icon.as_deref())
                .and_then(ui::icons::Icon::from_name)
        };
        if declared.is_some() {
            return (declared, None);
        }
        let Some(parts) = Self::result_parts(id) else {
            return (None, None);
        };
        (
            Some(ui::icons::app_glyph(parts.app, None)),
            crate::app_icons::for_window_app_id(parts.app),
        )
    }
}
