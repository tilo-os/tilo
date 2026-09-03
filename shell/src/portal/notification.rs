//! `org.freedesktop.impl.portal.Notification` backend.
//!
//! The shell already owns the notification chrome and the freedesktop
//! notification service.  This adapter lets sandboxed applications reach the
//! same chrome through `xdg-desktop-portal`, preserving the portal contract's
//! per-application string ids while the shell continues to use compact numeric
//! ids internally.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::OwnedValue;

use crate::notifications::IncomingNotification;

use super::{FileChooserBackend, PortalContext};

type NotificationKey = (String, String);

/// Backend instance registered alongside the other portal interfaces.
pub struct NotificationBackend {
    ctx: PortalContext,
    ids: Arc<Mutex<HashMap<NotificationKey, u32>>>,
}

impl NotificationBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self {
            ctx,
            ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn string_field(notification: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
        notification
            .get(key)
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| String::try_from(value).ok())
    }

    fn urgency(notification: &HashMap<String, OwnedValue>) -> u8 {
        match Self::string_field(notification, "priority").as_deref() {
            Some("low") => 0,
            Some("high" | "urgent") => 2,
            _ => 1,
        }
    }

    fn id_for(&self, app_id: &str, id: &str) -> u32 {
        let key = (app_id.to_string(), id.to_string());
        if let Ok(mut ids) = self.ids.lock() {
            *ids.entry(key)
                .or_insert_with(|| self.ctx.notifications.alloc_id())
        } else {
            self.ctx.notifications.alloc_id()
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Notification")]
impl NotificationBackend {
    /// Add or replace a notification. Unknown vardict entries are ignored;
    /// malformed known entries fall back to safe, empty/default values.
    fn add_notification(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_id: String,
        id: String,
        notification: HashMap<String, OwnedValue>,
    ) {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let dbus_id = self.id_for(&app_id, &id);
        self.ctx.notifications.push_incoming(IncomingNotification {
            dbus_id,
            app_name: app_id,
            summary: Self::string_field(&notification, "title").unwrap_or_default(),
            body: Self::string_field(&notification, "body").unwrap_or_default(),
            // Portal actions need activation-token-aware routing. Until that
            // path is wired, advertise and render no inert buttons.
            actions: Vec::new(),
            expire_timeout: -1,
            urgency: Self::urgency(&notification),
            caller: Some(caller),
            badge_count: None,
            contact_endpoint: None,
        });
    }

    /// Withdraw the notification identified by this app-local string id.
    fn remove_notification(&self, app_id: String, id: String) {
        let numeric = self
            .ids
            .lock()
            .ok()
            .and_then(|mut ids| ids.remove(&(app_id, id)));
        if let Some(numeric) = numeric {
            self.ctx.notifications.request_close(numeric);
        }
    }

    /// No icon or sound transport option is advertised until the shell
    /// validates and consumes those payloads.
    #[allow(
        clippy::unused_self,
        reason = "zbus exposes D-Bus properties as instance methods even when their advertised value is fixed"
    )]
    #[zbus(property)]
    fn supported_options(&self) -> HashMap<String, OwnedValue> {
        HashMap::new()
    }

    #[allow(
        clippy::unused_self,
        reason = "zbus exposes D-Bus properties as instance methods even when their advertised value is fixed"
    )]
    #[zbus(property)]
    const fn version(&self) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_maps_to_shell_urgency_and_bad_values_fail_safe() {
        for (priority, expected) in [("low", 0), ("normal", 1), ("high", 2), ("urgent", 2)] {
            let mut values = HashMap::new();
            values.insert(
                "priority".into(),
                OwnedValue::from(zbus::zvariant::Str::from(priority)),
            );
            assert_eq!(NotificationBackend::urgency(&values), expected);
        }
        assert_eq!(NotificationBackend::urgency(&HashMap::new()), 1);

        let mut malformed = HashMap::new();
        malformed.insert("priority".into(), OwnedValue::from(7_u32));
        malformed.insert("title".into(), OwnedValue::from(false));
        assert_eq!(NotificationBackend::urgency(&malformed), 1);
        assert_eq!(NotificationBackend::string_field(&malformed, "title"), None);
    }
}
