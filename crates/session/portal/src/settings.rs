//! `org.freedesktop.impl.portal.Settings`: the desktop's appearance as the
//! keys sandboxed toolkits read ([`crate::appearance`]), kept current as
//! the person changes it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

use crate::Portal;
use crate::appearance::{DconfPublisher, LegacyAppearance};

/// One setting's value, in the types the portal's keys use.
#[derive(Clone, Debug, PartialEq)]
pub enum PortalSetting {
    Unsigned(u32),
    Int(i32),
    Double(f64),
    Bool(bool),
    Color(f64, f64, f64),
    Text(String),
}

impl PortalSetting {
    #[must_use]
    pub fn to_owned_value(&self) -> OwnedValue {
        match self {
            Self::Unsigned(v) => OwnedValue::from(*v),
            Self::Int(v) => OwnedValue::from(*v),
            Self::Double(v) => OwnedValue::from(*v),
            Self::Bool(v) => OwnedValue::from(*v),
            Self::Color(r, g, b) => OwnedValue::try_from(zbus::zvariant::Value::new((*r, *g, *b)))
                .unwrap_or_else(|_| OwnedValue::from(0_u32)),
            Self::Text(v) => OwnedValue::from(zbus::zvariant::Str::from(v.as_str())),
        }
    }
}

/// A change to one setting: namespace, key and the new value.
pub type Change = (String, String, PortalSetting);

struct Shared {
    values: Mutex<BTreeMap<(String, String), PortalSetting>>,
    changes: tokio::sync::broadcast::Sender<Change>,
}

/// The settings the portal serves.
#[derive(Clone)]
pub struct Settings(Arc<Shared>);

impl Default for Settings {
    fn default() -> Self {
        // Every key exists from the first call: the default appearance,
        // until the person's settings are read.
        let settings = LegacyAppearance::from_settings_text("", false)
            .portal_settings()
            .into_iter()
            .map(|(namespace, key, value)| ((namespace.to_owned(), key.to_owned()), value))
            .collect();
        let (changes, _) = tokio::sync::broadcast::channel(64);
        Self(Arc::new(Shared {
            values: Mutex::new(settings),
            changes,
        }))
    }
}

impl Settings {
    #[must_use]
    pub fn get(&self, namespace: &str, key: &str) -> Option<PortalSetting> {
        self.0
            .values
            .lock_recover()
            .get(&(namespace.into(), key.into()))
            .cloned()
    }

    #[must_use]
    pub fn all(&self) -> BTreeMap<(String, String), PortalSetting> {
        self.0.values.lock_recover().clone()
    }

    /// Set one key; a value that truly changed is announced once.
    pub fn set(&self, namespace: impl Into<String>, key: impl Into<String>, value: PortalSetting) {
        let namespace = namespace.into();
        let key = key.into();
        let changed = self
            .0
            .values
            .lock_recover()
            .insert((namespace.clone(), key.clone()), value.clone())
            .as_ref()
            != Some(&value);
        if changed {
            let _ = self.0.changes.send((namespace, key, value));
        }
    }

    /// Every change from now on.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Change> {
        self.0.changes.subscribe()
    }

    /// Serve `appearance` from now on.
    pub fn publish(&self, appearance: &LegacyAppearance) {
        for (namespace, key, value) in appearance.portal_settings() {
            self.set(namespace, key, value);
        }
    }
}

/// Mirror the person's appearance into `settings` and dconf, and keep both
/// current: the settings watch wakes this task on every save, and
/// [`Settings::set`] announces only values that truly changed, so
/// `SettingChanged` fires once per real edit.
pub fn follow_appearance(settings: Settings) {
    let (wake, mut woken) = tokio::sync::mpsc::channel::<()>(1);
    let watch = settings_store::watch::watch_settings(move || {
        let _ = wake.try_send(());
    });
    if let Err(error) = &watch {
        tracing::warn!(%error, "settings file is not watchable; appearance is published once");
    }
    tokio::spawn(async move {
        let _watch = watch;
        let mut dconf = DconfPublisher::new();
        loop {
            let appearance = LegacyAppearance::configured();
            settings.publish(&appearance);
            dconf.publish(&appearance).await;
            if woken.recv().await.is_none() {
                break;
            }
        }
    });
}

/// Emit `SettingChanged` on `connection` for every change.
pub(crate) fn emit_changes(
    connection: &zbus::Connection,
    mut changes: tokio::sync::broadcast::Receiver<Change>,
) {
    let task_connection = connection.clone();
    connection
        .executor()
        .spawn(
            async move {
                loop {
                    let (namespace, key, value) = match changes.recv().await {
                        Ok(change) => change,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let Ok(context) = SignalEmitter::new(&task_connection, crate::OBJECT_PATH)
                    else {
                        break;
                    };
                    if SettingsPortal::setting_changed(
                        &context,
                        namespace,
                        key,
                        value.to_owned_value(),
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
            },
            "portal settings signal emitter",
        )
        .detach();
}

fn matches_namespace(pattern: &str, namespace: &str) -> bool {
    pattern.is_empty()
        || pattern == namespace
        || pattern.strip_suffix(".*").is_some_and(|prefix| {
            namespace
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
        })
}

fn grouped(
    settings: BTreeMap<(String, String), PortalSetting>,
    patterns: &[String],
) -> HashMap<String, HashMap<String, OwnedValue>> {
    let mut result = HashMap::new();
    for ((namespace, key), value) in settings {
        if patterns.iter().any(|p| matches_namespace(p, &namespace)) {
            result
                .entry(namespace)
                .or_insert_with(HashMap::new)
                .insert(key, value.to_owned_value());
        }
    }
    result
}

/// The `org.freedesktop.impl.portal.Settings` interface.
pub struct SettingsPortal {
    portal: Portal,
}

impl SettingsPortal {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    async fn trusted(&self, header: &zbus::message::Header<'_>) -> bool {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        self.portal
            .authenticator
            .authenticate_frontend(sender.as_ref())
            .await
            .is_ok()
    }
}

#[interface(name = "org.freedesktop.impl.portal.Settings")]
impl SettingsPortal {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn read_all(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        namespaces: Vec<String>,
    ) -> HashMap<String, HashMap<String, OwnedValue>> {
        if !self.trusted(&header).await {
            return HashMap::new();
        }
        grouped(self.portal.settings.all(), &namespaces)
    }

    async fn read(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        namespace: String,
        key: String,
    ) -> zbus::fdo::Result<OwnedValue> {
        if !self.trusted(&header).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "untrusted portal caller".into(),
            ));
        }
        self.portal
            .settings
            .get(&namespace, &key)
            .map(|v| v.to_owned_value())
            .ok_or_else(|| {
                zbus::fdo::Error::InvalidArgs(format!("unknown portal setting {namespace}/{key}"))
            })
    }

    #[zbus(signal)]
    async fn setting_changed(
        ctxt: &SignalEmitter<'_>,
        namespace: String,
        key: String,
        value: OwnedValue,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_all_honours_only_trailing_section_globs() {
        let mut settings = BTreeMap::new();
        settings.insert(
            ("org.freedesktop.appearance".into(), "color-scheme".into()),
            PortalSetting::Unsigned(1),
        );
        settings.insert(
            ("org.example".into(), "key".into()),
            PortalSetting::Unsigned(2),
        );
        assert_eq!(grouped(settings, &["org.freedesktop.*".into()]).len(), 1);
        assert!(matches_namespace("", "anything"));
        assert!(!matches_namespace("org.*.bad", "org.any.bad"));
        assert!(!matches_namespace("org.free.*", "org.freedesktop"));
    }

    #[test]
    fn a_value_that_did_not_change_is_not_announced() {
        let settings = Settings::default();
        let mut changes = settings.subscribe();
        settings.set("org.example", "key", PortalSetting::Bool(true));
        settings.set("org.example", "key", PortalSetting::Bool(true));
        assert!(changes.try_recv().is_ok());
        assert!(changes.try_recv().is_err(), "announced once");
    }
}
