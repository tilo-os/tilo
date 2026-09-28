//! `org.freedesktop.impl.portal.DynamicLauncher` backend.

#[cfg(target_os = "linux")]
use super::desktop_services::{LauncherInstallRequest, PortalResult};
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::{OwnedValue, Structure, Value};

#[cfg(target_os = "linux")]
use crate::shell::portal_dialog::{
    LauncherReviewRequest, PortalDialogQueue, ReviewDecision, ReviewRequest,
};

#[cfg(target_os = "linux")]
pub struct DynamicLauncherBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
    dialogs: PortalDialogQueue,
    connection: zbus::Connection,
}
#[cfg(target_os = "linux")]
impl DynamicLauncherBackend {
    /// `connection` is the bus this backend is served on; its request objects
    /// are registered there.
    pub fn new(ctx: PortalContext, connection: zbus::Connection) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new(), connection)
    }

    pub fn with_lifecycle(
        ctx: PortalContext,
        lifecycle: PortalLifecycle,
        connection: zbus::Connection,
    ) -> Self {
        Self {
            ctx,
            lifecycle,
            dialogs: PortalDialogQueue::global(),
            connection,
        }
    }
}
#[cfg(target_os = "linux")]
fn bool_opt(options: &HashMap<String, OwnedValue>, key: &str, default: bool) -> bool {
    options
        .get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(default)
}
#[cfg(target_os = "linux")]
fn str_opt(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options
        .get(key)?
        .try_clone()
        .ok()
        .and_then(|v| String::try_from(v).ok())
        .filter(|v| v.len() <= 4096 && !v.chars().any(char::is_control))
}
#[cfg(target_os = "linux")]
fn launcher_type(options: &HashMap<String, OwnedValue>) -> u32 {
    options
        .get("launcher_type")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(1)
}
fn safe_web_target(target: &str) -> bool {
    target.starts_with("https://")
        && target
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && target.trim() == target
        && !target.chars().any(char::is_control)
        && target.len() <= 4096
}

/// Decodes only the `GBytesIcon` representation produced by
/// `g_icon_serialize`: `('bytes', <ay>)`.  Paths, themed names and Unix FDs
/// are deliberately not accepted: Tilo neither reads app paths nor passes an
/// inherited FD to the compositor.
fn icon_bytes(icon: &OwnedValue) -> Option<Vec<u8>> {
    if icon.value_signature().to_string().contains('h') {
        return None;
    }
    let structure = Structure::try_from(icon.try_clone().ok()?).ok()?;
    let mut fields = structure.into_fields();
    if fields.len() != 2 {
        return None;
    }
    let kind = String::try_from(fields.remove(0)).ok()?;
    if kind != "bytes" {
        return None;
    }
    fields.remove(0).downcast::<Vec<u8>>().ok()
}

fn valid_icon(icon: &OwnedValue) -> bool {
    icon_bytes(icon).is_some_and(|bytes| valid_icon_payload(&bytes))
}

const TILO_MONOGRAM_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><rect width="128" height="128" rx="28" fill="#2d6cdf"/><path fill="white" d="M27 38h74v16H43v20h45v16H43v16h58v16H27z"/></svg>"##;
const TILO_WEB_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" viewBox="0 0 128 128"><rect width="128" height="128" rx="28" fill="#087f5b"/><circle cx="64" cy="64" r="36" fill="none" stroke="white" stroke-width="8"/><path fill="none" stroke="white" stroke-width="8" d="M28 64h72M64 28c14 16 14 56 0 72M64 28c-14 16-14 56 0 72"/></svg>"##;

fn tilo_icon(choice: usize) -> OwnedValue {
    let bytes = match choice {
        1 => TILO_MONOGRAM_ICON.as_bytes(),
        _ => TILO_WEB_ICON.as_bytes(),
    }
    .to_vec();
    // GBytesIcon's serialized wire form is (`bytes`, <ay>), exactly the
    // format produced by GLib.  It contains no path or file descriptor.
    OwnedValue::try_from(Value::new(("bytes", Value::new(bytes))))
        .expect("a static Tilo SVG is a valid zvariant value")
}
#[interface(name = "org.freedesktop.impl.portal.DynamicLauncher")]
#[cfg(target_os = "linux")]
impl DynamicLauncherBackend {
    #[zbus(property, name = "SupportedLauncherTypes")]
    fn supported_launcher_types(&self) -> u32 {
        3
    }
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }
    async fn prepare_install(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        name: String,
        icon_v: OwnedValue,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        if name.trim().is_empty()
            || name.trim() != name
            || name.len() > 255
            || name.chars().any(char::is_control)
            || !valid_icon(&icon_v)
        {
            return (2, HashMap::new());
        }
        let kind = launcher_type(&options);
        if kind != 1 && kind != 2 {
            return (2, HashMap::new());
        }
        let target = str_opt(&options, "target");
        if kind == 2 && !target.as_deref().is_some_and(safe_web_target) {
            return (2, HashMap::new());
        }
        if kind == 1 && target.is_some() {
            return (2, HashMap::new());
        }
        let editable_name = bool_opt(&options, "editable_name", true);
        let editable_icon = bool_opt(&options, "editable_icon", false);
        let resource = match target.as_deref() {
            Some(target) => format!("{name}\n{target}"),
            None => name.clone(),
        };
        let path = handle.to_string();
        let dialogs = self.dialogs.clone();
        let cancel_path = path.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id.clone(),
            resource: LifecycleResource::Prompt,
        };
        if !matches!(
            self.lifecycle
                .register_request(&self.connection, &path, entry, move |_| {
                    let _ = dialogs.cancel(&cancel_path);
                })
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        let Ok(review) = self.dialogs.enqueue_launcher_review(LauncherReviewRequest {
            review: ReviewRequest {
                handle: path.clone(),
                app_id: auth.caller.app_id.clone(),
                title: crate::shell::i18n::t(if kind == 2 {
                    "portal-launcher-web-app-title"
                } else {
                    "portal-launcher-title"
                }),
                reason: crate::shell::i18n::t("portal-launcher-reason"),
                resource,
                confirm_label: crate::shell::i18n::t("portal-launcher-confirm"),
            },
            name: name.clone(),
            editable_name,
            editable_icon,
            icon_choices: if editable_icon {
                vec![
                    crate::shell::i18n::t("portal-launcher-icon-supplied"),
                    crate::shell::i18n::t("portal-launcher-icon-monogram"),
                    crate::shell::i18n::t("portal-launcher-icon-compass"),
                ]
            } else {
                vec![crate::shell::i18n::t("portal-launcher-icon-supplied")]
            },
        }) else {
            self.lifecycle
                .complete_request_object(&self.connection, &path)
                .await;
            return (2, HashMap::new());
        };
        let Ok(decision) = review.await else {
            self.lifecycle
                .complete_request_object(&self.connection, &path)
                .await;
            return (1, HashMap::new());
        };
        if decision.decision != ReviewDecision::Confirmed
            || decision.name.trim().is_empty()
            || decision.name.len() > 255
            || decision.name.trim() != decision.name
            || decision.name.chars().any(char::is_control)
        {
            self.lifecycle
                .complete_request_object(&self.connection, &path)
                .await;
            return (1, HashMap::new());
        }
        let selected_icon = if editable_icon && decision.icon_choice > 0 {
            tilo_icon(decision.icon_choice)
        } else {
            icon_v
        };
        let selected_icon_bytes = if editable_icon && decision.icon_choice > 0 {
            None
        } else {
            // `valid_icon` above already proved this is a bounded GBytesIcon.
            // Persist its byte payload, never a caller-supplied path/FD.
            icon_bytes(&selected_icon)
        };
        let result =
            self.ctx
                .desktop_services
                .dynamic_launcher
                .prepare_install(&LauncherInstallRequest {
                    app_id: auth.caller.app_id,
                    name: decision.name.clone(),
                    launcher_type: kind,
                    target,
                    editable_name,
                    editable_icon,
                    icon_kind: match decision.icon_choice {
                        1 if editable_icon => "tilo-monogram".into(),
                        2 if editable_icon => "tilo-web".into(),
                        _ => "supplied".into(),
                    },
                    icon_bytes: selected_icon_bytes,
                });
        self.lifecycle
            .complete_request_object(&self.connection, &path)
            .await;
        let PortalResult::Success(choice) = result else {
            return (result.response_code(), HashMap::new());
        };
        let mut out = HashMap::new();
        out.insert(
            "name".into(),
            OwnedValue::from(zbus::zvariant::Str::from(choice.name)),
        );
        out.insert("icon".into(), selected_icon);
        (0, out)
    }
    async fn request_install_token(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> u32 {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), &app_id, None).await else {
            return 2;
        };
        self.ctx
            .desktop_services
            .dynamic_launcher
            .request_install_token(&auth.caller.app_id)
            .response_code()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dynamic_launcher_restricts_targets_and_icons() {
        // Phase `web_targets_are_restricted_to_http`.
        assert!(safe_web_target("https://example.test"));
        assert!(!safe_web_target("file:///tmp/a"));
        assert!(!safe_web_target("http://example.test"));
        // Phase `generated_icons_are_bounded_gbytes_icons`.
        {
            let icon = tilo_icon(1);
            assert!(valid_icon(&icon));
            assert!(icon_bytes(&icon).is_some_and(|bytes| bytes.starts_with(b"<svg")));
        }
        // Phase `non_bytes_icon_serializations_are_rejected_without_path_access`.
        {
            let path_icon = OwnedValue::try_from(Value::new((
                "file",
                Value::new("/home/person/.ssh/id_ed25519"),
            )))
            .expect("a variant value");
            assert!(!valid_icon(&path_icon));
        }
    }
}

pub(crate) use super::desktop_services::valid_icon_payload;
