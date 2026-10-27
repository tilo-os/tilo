//! `org.freedesktop.impl.portal.Notification` backend.
//!
//! Portal string ids are mapped to the shell's numeric toast ids, with a
//! bounded reverse map retained for action routing. The mapping handle is
//! intentionally public: compositor chrome can turn a numeric toast click
//! back into the portal `ActionInvoked(app_id, id, action, parameters)`
//! signal without trusting labels rendered on screen.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

#[cfg(target_os = "linux")]
use crate::notifications::IncomingNotification;

#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};

const MAX_ACTIVE_PORTAL_NOTIFICATIONS: usize = 256;
#[cfg(target_os = "linux")]
const MAX_TITLE_CHARS: usize = 512;
#[cfg(target_os = "linux")]
const MAX_BODY_CHARS: usize = 16 * 1024;
#[cfg(target_os = "linux")]
const MAX_BUTTONS: usize = 3;

type NotificationKey = (String, String);

struct ActionSpec {
    target: Option<OwnedValue>,
}

struct NotificationRecord {
    numeric_id: u32,
    actions: HashMap<String, ActionSpec>,
}

#[derive(Default)]
struct NotificationState {
    records: HashMap<NotificationKey, NotificationRecord>,
    order: VecDeque<NotificationKey>,
}

/// Source-aware action ready to be emitted on the backend interface.
pub struct PortalNotificationAction {
    pub app_id: String,
    pub id: String,
    pub action: String,
    pub parameter: Vec<OwnedValue>,
}

/// Cloneable compositor-side hook for action routing and deterministic
/// cleanup. Persistence code may use [`Self::active_keys`] to reconcile its
/// durable records after a compositor restart without persisting numeric ids.
#[derive(Clone, Default)]
pub struct NotificationPortalHandle {
    state: Arc<Mutex<NotificationState>>,
    pending_actions: Arc<Mutex<VecDeque<(u32, String)>>>,
    wake: crate::idle_wake::IdleWake,
}

impl NotificationPortalHandle {
    /// Subscribe this portal backend to compositor toast actions without
    /// stealing them from the standard freedesktop notification service.
    pub fn install_on_bridge(&self, bridge: &crate::notifications::NotificationBridge) {
        let portal = self.clone();
        bridge.add_action_tap(Arc::new(move |numeric_id, action| {
            portal.note_numeric_action(numeric_id, action);
        }));
    }

    fn note_numeric_action(&self, numeric_id: u32, action: &str) {
        if self.action_for(numeric_id, action).is_none() {
            return;
        }
        if let Ok(mut actions) = self.pending_actions.lock() {
            if actions.len() >= MAX_ACTIVE_PORTAL_NOTIFICATIONS {
                actions.pop_front();
            }
            actions.push_back((numeric_id, action.into()));
            self.wake.poke();
        }
    }

    #[cfg(target_os = "linux")]
    fn drain_actions(&self) -> Vec<PortalNotificationAction> {
        let actions = self
            .pending_actions
            .lock()
            .map(|mut actions| actions.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        actions
            .into_iter()
            .filter_map(|(numeric_id, action)| self.action_for(numeric_id, &action))
            .collect()
    }

    pub fn action_for(&self, numeric_id: u32, action: &str) -> Option<PortalNotificationAction> {
        let state = self.state.lock().ok()?;
        let ((app_id, id), record) = state
            .records
            .iter()
            .find(|(_, record)| record.numeric_id == numeric_id)?;
        let spec = record.actions.get(action)?;
        let mut parameter = Vec::new();
        if let Some(target) = spec
            .target
            .as_ref()
            .and_then(|target| target.try_clone().ok())
        {
            parameter.push(target);
        }
        // platform-data is always present. Tilo does not claim an activation
        // token until xdg-activation is actually implemented.
        let platform_data: HashMap<String, OwnedValue> = HashMap::new();
        if let Ok(value) = OwnedValue::try_from(Value::new(platform_data)) {
            parameter.push(value);
        }
        Some(PortalNotificationAction {
            app_id: app_id.clone(),
            id: id.clone(),
            action: action.to_string(),
            parameter,
        })
    }

    /// Remove the reverse mapping when chrome expires or dismisses a toast.
    pub fn close_numeric(&self, numeric_id: u32) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let key = state
            .records
            .iter()
            .find_map(|(key, record)| (record.numeric_id == numeric_id).then(|| key.clone()));
        if let Some(key) = key {
            state.records.remove(&key);
            state.order.retain(|candidate| candidate != &key);
        }
    }

    /// Stable application/string ids suitable for persistence reconciliation.
    /// Numeric toast ids are deliberately excluded because they are scoped to
    /// one bridge lifetime and must be reallocated on restore.
    pub fn active_keys(&self) -> Vec<(String, String)> {
        self.state
            .lock()
            .map(|state| state.order.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn upsert(
        &self,
        key: NotificationKey,
        allocate: impl FnOnce() -> u32,
        actions: HashMap<String, ActionSpec>,
    ) -> (u32, Option<u32>) {
        let Ok(mut state) = self.state.lock() else {
            return (allocate(), None);
        };
        if let Some(record) = state.records.get_mut(&key) {
            record.actions = actions;
            return (record.numeric_id, None);
        }
        let evicted = if state.records.len() >= MAX_ACTIVE_PORTAL_NOTIFICATIONS {
            state
                .order
                .pop_front()
                .and_then(|oldest| state.records.remove(&oldest))
                .map(|record| record.numeric_id)
        } else {
            None
        };
        let numeric_id = allocate();
        state.order.push_back(key.clone());
        state.records.insert(
            key,
            NotificationRecord {
                numeric_id,
                actions,
            },
        );
        (numeric_id, evicted)
    }

    #[cfg(target_os = "linux")]
    fn remove(&self, key: &NotificationKey) -> Option<u32> {
        let mut state = self.state.lock().ok()?;
        let numeric = state.records.remove(key)?.numeric_id;
        state.order.retain(|candidate| candidate != key);
        Some(numeric)
    }
}

/// Emit portal `ActionInvoked` signals from compositor toast clicks. The
/// generic freedesktop notification server has its own independent consumer;
/// `NotificationBridge::add_action_tap` prevents either side from racing to
/// drain the shared action mailbox.
#[cfg(target_os = "linux")]
pub fn spawn_action_emitter(
    connection: zbus::Connection,
    portal: NotificationPortalHandle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(context) = SignalEmitter::new(&connection, super::OBJECT_PATH) else {
            return;
        };
        loop {
            let actions = portal.drain_actions();
            if actions.is_empty() {
                portal.wake.wait().await;
                continue;
            }
            for action in actions {
                let _ = NotificationBackend::action_invoked(
                    &context,
                    &action.app_id,
                    &action.id,
                    &action.action,
                    action.parameter,
                )
                .await;
            }
        }
    })
}

/// Backend instance registered alongside the other portal interfaces.
pub struct NotificationBackend {
    #[cfg(target_os = "linux")]
    ctx: PortalContext,
    portal: NotificationPortalHandle,
}

impl NotificationBackend {
    #[cfg(target_os = "linux")]
    pub fn new(ctx: PortalContext) -> Self {
        Self {
            ctx,
            portal: NotificationPortalHandle::default(),
        }
    }

    pub fn portal_handle(&self) -> NotificationPortalHandle {
        self.portal.clone()
    }

    /// The portal's `sound` is `"default"`, `"silent"` or a sound the app
    /// supplies; the shell plays its own theme cue for anything but silence,
    /// chosen by the notification's `category`.
    fn sound(
        notification: &HashMap<String, OwnedValue>,
    ) -> Option<settings_store::system_sounds::SystemSoundEvent> {
        let silent =
            crate::portal::option::<String>(&notification, "sound").as_deref() == Some("silent");
        let category = crate::portal::option::<String>(&notification, "category");
        (!silent).then(|| {
            settings_store::system_sounds::SystemSoundEvent::for_notification_category(
                category.as_deref(),
            )
        })
    }

    fn urgency(notification: &HashMap<String, OwnedValue>) -> u8 {
        match crate::portal::option::<String>(&notification, "priority").as_deref() {
            Some("low") => 0,
            Some("high" | "urgent") => 2,
            _ => 1,
        }
    }

    #[cfg(target_os = "linux")]
    fn display_timeout(notification: &HashMap<String, OwnedValue>) -> i32 {
        let hints =
            crate::portal::option::<Vec<String>>(notification, "display-hint").unwrap_or_default();
        if hints.iter().any(|hint| hint == "persistent") {
            0
        } else {
            -1
        }
    }

    #[cfg(target_os = "linux")]
    fn body(notification: &HashMap<String, OwnedValue>) -> String {
        let plain = crate::portal::option::<String>(&notification, "body").unwrap_or_default();
        let markup = crate::portal::option::<String>(&notification, "markup-body");
        truncate_chars(
            &markup.map_or(plain, |markup| markup_to_plain_text(&markup)),
            MAX_BODY_CHARS,
        )
    }

    #[cfg(target_os = "linux")]
    fn actions(
        notification: &HashMap<String, OwnedValue>,
    ) -> (Vec<(String, String)>, HashMap<String, ActionSpec>) {
        let mut rendered = Vec::new();
        let mut specs = HashMap::new();

        if let Some(action) = crate::portal::option::<String>(&notification, "default-action")
            .filter(|action| valid_action(action))
        {
            let target = clone_field(notification, "default-action-target");
            specs.insert(action.clone(), ActionSpec { target });
            rendered.push((action, crate::shell::i18n::t("portal-notification-open")));
        }

        let buttons =
            crate::portal::option::<Vec<HashMap<String, OwnedValue>>>(notification, "buttons")
                .unwrap_or_default();
        for button in buttons.into_iter().take(MAX_BUTTONS) {
            let Some(action) =
                crate::portal::option::<String>(&button, "action").filter(|v| valid_action(v))
            else {
                continue;
            };
            let Some(label) = crate::portal::option::<String>(&button, "label")
                .filter(|label| !label.is_empty())
                .map(|label| truncate_chars(&label, 80))
            else {
                continue;
            };
            let target = clone_field(&button, "target");
            specs.insert(action.clone(), ActionSpec { target });
            if !rendered.iter().any(|(existing, _)| existing == &action) {
                rendered.push((action, label));
            }
        }
        (rendered, specs)
    }
}

#[cfg(target_os = "linux")]
#[interface(name = "org.freedesktop.impl.portal.Notification")]
impl NotificationBackend {
    /// Add or replace a notification. Unsupported icon/sound/category fields
    /// are not advertised and are ignored rather than interpreted unsafely.
    async fn add_notification(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_id: String,
        id: String,
        notification: HashMap<String, OwnedValue>,
    ) {
        let Ok(auth) = authenticate_call(&self.ctx, &hdr, &app_id, None).await else {
            return;
        };
        if id.is_empty() || id.len() > 255 || id.chars().any(char::is_control) {
            return;
        }

        let (actions, action_specs) = Self::actions(&notification);
        let key = (app_id.clone(), id);
        let (dbus_id, evicted) =
            self.portal
                .upsert(key, || self.ctx.notifications.alloc_id(), action_specs);
        if let Some(evicted) = evicted {
            self.ctx.notifications.request_close(evicted);
        }
        let app_name = crate::shell::launcher::display_name_for_app_id(&app_id)
            .unwrap_or_else(|| app_id.clone());
        self.ctx.notifications.push_incoming(IncomingNotification {
            dbus_id,
            app_name,
            app_icon: None,
            desktop_entry: None,
            image_data: None,
            summary: truncate_chars(
                &crate::portal::option::<String>(&notification, "title").unwrap_or_default(),
                MAX_TITLE_CHARS,
            ),
            body: Self::body(&notification),
            actions,
            expire_timeout: Self::display_timeout(&notification),
            urgency: Self::urgency(&notification),
            caller: Some(auth.caller),
            badge_count: None,
            contact_endpoint: None,
            sender_name: None,
            sound: Self::sound(&notification),
            incoming_call: false,
            machine_wide: false,
        });
    }

    /// Withdraw only the authenticated application's matching local id.
    async fn remove_notification(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_id: String,
        id: String,
    ) {
        if authenticate_call(&self.ctx, &hdr, &app_id, None)
            .await
            .is_err()
        {
            return;
        }
        if let Some(numeric) = self.portal.remove(&(app_id, id)) {
            self.ctx.notifications.request_close(numeric);
        }
    }

    #[zbus(property)]
    fn supported_options(&self) -> HashMap<String, OwnedValue> {
        HashMap::new()
    }

    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    /// Emitted by compositor integration after resolving the numeric toast id
    /// through [`NotificationPortalHandle::action_for`].
    #[zbus(signal)]
    pub async fn action_invoked(
        ctxt: &SignalEmitter<'_>,
        app_id: &str,
        id: &str,
        action: &str,
        parameter: Vec<OwnedValue>,
    ) -> zbus::Result<()>;
}

#[cfg(target_os = "linux")]
fn valid_action(action: &str) -> bool {
    !action.is_empty() && action.len() <= 255 && !action.chars().any(char::is_control)
}

#[cfg(target_os = "linux")]
fn clone_field(values: &HashMap<String, OwnedValue>, key: &str) -> Option<OwnedValue> {
    values.get(key).and_then(|value| value.try_clone().ok())
}

#[cfg(target_os = "linux")]
fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Render the deliberately tiny portal markup language as safe plain text.
fn markup_to_plain_text(markup: &str) -> String {
    let mut plain = String::with_capacity(markup.len());
    let mut chars = markup.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            for next in chars.by_ref() {
                if next == '>' {
                    break;
                }
            }
            continue;
        }
        if ch == '&' {
            let mut entity = String::new();
            while entity.len() <= 6 {
                let Some(next) = chars.peek().copied() else {
                    break;
                };
                chars.next();
                if next == ';' {
                    break;
                }
                entity.push(next);
            }
            match entity.as_str() {
                "amp" => plain.push('&'),
                "lt" => plain.push('<'),
                "gt" => plain.push('>'),
                "quot" => plain.push('"'),
                "apos" => plain.push('\''),
                _ => plain.push('\u{fffd}'),
            }
            continue;
        }
        plain.push(ch);
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_map_priority_reduce_markup_and_bound_the_reverse_map() {
        // Phase `priority_maps_to_shell_urgency_and_bad_values_fail_safe`.
        for (priority, expected) in [("low", 0), ("normal", 1), ("high", 2), ("urgent", 2)] {
            let mut values = HashMap::new();
            values.insert(
                "priority".into(),
                OwnedValue::from(zbus::zvariant::Str::from(priority)),
            );
            assert_eq!(NotificationBackend::urgency(&values), expected);
        }
        assert_eq!(NotificationBackend::urgency(&HashMap::new()), 1);
        assert_eq!(
            markup_to_plain_text("Hello <b>world</b> &amp; <a href='x'>friends</a>"),
            "Hello world & friends"
        );
        {
            let handle = NotificationPortalHandle::default();
            for index in 0..=MAX_ACTIVE_PORTAL_NOTIFICATIONS {
                handle.upsert(
                    ("org.example.App".into(), index.to_string()),
                    || u32::try_from(index + 1).unwrap(),
                    HashMap::new(),
                );
            }
            assert_eq!(handle.active_keys().len(), MAX_ACTIVE_PORTAL_NOTIFICATIONS);
            assert!(!handle.active_keys().iter().any(|(_, id)| id == "0"));
            handle.close_numeric(2);
            assert_eq!(
                handle.active_keys().len(),
                MAX_ACTIVE_PORTAL_NOTIFICATIONS - 1
            );
        }
    }
}
