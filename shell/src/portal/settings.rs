//! `org.freedesktop.impl.portal.Settings` backend.
use super::PortalContext;
use super::desktop_services::PortalSetting;
use std::collections::{BTreeMap, HashMap};
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::OwnedValue;
pub struct SettingsBackend {
    ctx: PortalContext,
}
impl SettingsBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }
    /// Called by the compositor settings authority after it mutates the shared
    /// store. Keeping signal emission explicit prevents a background portal
    /// task from outliving the D-Bus connection.
    pub async fn emit_change(
        &self,
        ctxt: &SignalContext<'_>,
        namespace: &str,
        key: &str,
    ) -> zbus::Result<()> {
        if let Some(value) = self.ctx.desktop_services.settings.get(namespace, key) {
            Self::setting_changed(
                ctxt,
                namespace.to_string(),
                key.to_string(),
                value.to_owned_value(),
            )
            .await?;
        }
        Ok(())
    }
}

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
                    let Ok(context) = SignalContext::new(&task_connection, super::OBJECT_PATH)
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

/// Mirror qOS's live appearance store into the standard portal namespace.
/// The compositor watches the same file on the same cadence, so applications
/// and shell chrome switch coherently and `SettingChanged` is emitted only
/// when a value truly changes.
pub fn spawn_appearance_monitor(settings: super::desktop_services::SettingsServiceHandle) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(750));
        loop {
            interval.tick().await;
            let theme = ui::theme::Theme::configured();
            let background = theme.bg_app.to_rgba8();
            let luma = 0.2126 * f64::from(background.r)
                + 0.7152 * f64::from(background.g)
                + 0.0722 * f64::from(background.b);
            settings.set(
                "org.freedesktop.appearance",
                "color-scheme",
                PortalSetting::Unsigned(if luma < 127.5 { 1 } else { 2 }),
            );
            let high_contrast = settings_store::settings_path()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("appearance")?
                        .get("theme")?
                        .as_str()
                        .map(|name| name == "high-contrast" || name == "high_contrast")
                })
                .unwrap_or(false);
            settings.set(
                "org.freedesktop.appearance",
                "contrast",
                PortalSetting::Unsigned(u32::from(high_contrast)),
            );
            let accent =
                theme.tokens.colors.accent[ui::tokens::colors::ACCENT_PRIMARY_IDX].to_rgba8();
            settings.set(
                "org.freedesktop.appearance",
                "accent-color",
                PortalSetting::Color(
                    f64::from(accent.r) / 255.0,
                    f64::from(accent.g) / 255.0,
                    f64::from(accent.b) / 255.0,
                ),
            );
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
        ctxt: &SignalContext<'_>,
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
