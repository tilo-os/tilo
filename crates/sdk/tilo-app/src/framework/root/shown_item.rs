//! What a window's person may do with the one item it shows, beyond what
//! they may do with its record: an item shared with them to edit.

use super::{LivePermissions, PermissionScope, ResourceRoot};

impl ResourceRoot {
    /// Whether the person may change the item the window shows (its bar's
    /// location anchor, `<kind>/<field>/<item>`) though not the record: an
    /// item shared with them to edit.
    pub(super) fn may_change_shown_item(&self, permissions: &LivePermissions) -> bool {
        let Some(anchor) = ui_core::window_toolbar::declared()
            .and_then(|toolbar| toolbar.location)
            .and_then(|location| location.anchor)
        else {
            return false;
        };
        let Some((collection, item)) = anchor.rsplit_once('/') else {
            return false;
        };
        let Some((kind, _)) = collection.split_once('/') else {
            return false;
        };
        let record = self
            .run
            .launched
            .record
            .as_ref()
            .map_or("", |record| record.route());
        permissions.check(&PermissionScope::Action {
            node: ui_core::permissions::TreePath::root()
                .app(self.dispatch.app_id())
                .kind(kind)
                .resource(record)
                .item(item),
            kind: ui_core::permissions::ActionKind::Undoable,
        })
    }
}
