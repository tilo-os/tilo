//! `org.freedesktop.impl.portal.Settings` backend.
#[cfg(target_os = "linux")]
use super::PortalContext;
use super::desktop_services::PortalSetting;
use std::collections::{BTreeMap, HashMap};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
#[cfg(target_os = "linux")]
pub struct SettingsBackend {
    ctx: PortalContext,
}
#[cfg(target_os = "linux")]
impl SettingsBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }
}

#[cfg(target_os = "linux")]
pub fn spawn_signal_emitter(
    connection: zbus::Connection,
    mut changes: tokio::sync::broadcast::Receiver<(String, String, PortalSetting)>,
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
                    let Ok(context) = SignalEmitter::new(&task_connection, super::OBJECT_PATH)
                    else {
                        break;
                    };
                    if SettingsBackend::setting_changed(
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

/// Mirror Tilo's appearance into the portal namespaces and dconf, then keep
/// both current. The settings-store watch wakes this task on every save, and
/// [`super::desktop_services::SettingsServiceHandle::set`] broadcasts only
/// values that truly changed, so `SettingChanged` fires once per real edit.
#[cfg(target_os = "linux")]
pub fn spawn_appearance_monitor(settings: super::desktop_services::SettingsServiceHandle) {
    let (wake, mut woken) = tokio::sync::mpsc::channel::<()>(1);
    let watch = settings_store::watch::watch_settings(move || {
        let _ = wake.try_send(());
    });
    if let Err(error) = &watch {
        tracing::warn!(%error, "settings file is not watchable; appearance is published once");
    }
    tokio::spawn(async move {
        let _watch = watch;
        let mut dconf = super::appearance::DconfPublisher::new();
        loop {
            let appearance = super::appearance::LegacyAppearance::configured();
            for (namespace, key, value) in appearance.portal_settings() {
                settings.set(namespace, key, value);
            }
            dconf.publish(&appearance).await;
            if woken.recv().await.is_none() {
                break;
            }
        }
    });
}
fn matches_namespace(pattern: &str, namespace: &str) -> bool {
    pattern.is_empty()
        || pattern == namespace
        || pattern.strip_suffix(".*").is_some_and(|prefix| {
            namespace.starts_with(prefix) && namespace[prefix.len()..].starts_with('.')
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
#[interface(name = "org.freedesktop.impl.portal.Settings")]
#[cfg(target_os = "linux")]
impl SettingsBackend {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }
    async fn read_all(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        namespaces: Vec<String>,
    ) -> HashMap<String, HashMap<String, OwnedValue>> {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        if super::auth::authenticate_frontend_call(&self.ctx, sender.as_ref())
            .await
            .is_err()
        {
            return HashMap::new();
        }
        grouped(self.ctx.desktop_services.settings.all(), &namespaces)
    }
    async fn read(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        namespace: String,
        key: String,
    ) -> zbus::fdo::Result<OwnedValue> {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        super::auth::authenticate_frontend_call(&self.ctx, sender.as_ref())
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        self.ctx
            .desktop_services
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
    }
}
