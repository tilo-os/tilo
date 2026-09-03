//! `org.freedesktop.impl.portal.Screenshot` backend.
use super::desktop_services::{PortalResult, ScreenshotRequest, ScreenshotTarget};
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::{PortalContext, auth::authenticate_call, lockdown};
use std::collections::HashMap;
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue};
pub struct ScreenshotBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
}
impl ScreenshotBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }
    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self { ctx, lifecycle }
    }
}
fn bool_opt(options: &HashMap<String, OwnedValue>, key: &str, default: bool) -> bool {
    options
        .get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(default)
}
fn target(
    options: &HashMap<String, OwnedValue>,
    available: u32,
) -> Result<Option<ScreenshotTarget>, ()> {
    let Some(raw) = options
        .get("target")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| u32::try_from(v).ok())
    else {
        return Ok(None);
    };
    let target = match raw {
        1 => ScreenshotTarget::Screen,
        2 => ScreenshotTarget::Window,
        4 => ScreenshotTarget::Area,
        8 => ScreenshotTarget::ActiveWindow,
        _ => return Err(()),
    };
    if available & target.bit() == 0 {
        Err(())
    } else {
        Ok(Some(target))
    }
}
fn color_value(color: (f64, f64, f64)) -> Option<OwnedValue> {
    OwnedValue::try_from(zbus::zvariant::Value::new(color)).ok()
}
#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl ScreenshotBackend {
    #[zbus(property, name = "AvailableTargets")]
    fn available_targets(&self) -> u32 {
        self.ctx.desktop_services.screenshot.available_targets()
    }
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        3
    }
    async fn screenshot(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        if lockdown::current_policy().disable_save_to_disk {
            return (1, HashMap::new());
        }
        let available = self.ctx.desktop_services.screenshot.available_targets();
        let Ok(target) = target(&options, available) else {
            return (2, HashMap::new());
        };
        let request_path = handle.to_string();
        let service = self.ctx.desktop_services.screenshot.clone();
        let cleanup_path = request_path.clone();
        let registered = self
            .lifecycle
            .register_request(
                connection,
                &request_path,
                LifecycleEntry {
                    owner: auth.owner.to_string(),
                    app_id: auth.caller.app_id.clone(),
                    resource: LifecycleResource::Prompt,
                },
                move |_| service.cancel_color(&cleanup_path),
            )
            .await;
        if !matches!(registered, Ok(true)) {
            return (2, HashMap::new());
        }
        let response = match self
            .ctx
            .desktop_services
            .screenshot
            .screenshot(&ScreenshotRequest {
                request_handle: request_path.clone(),
                caller: auth.caller.clone(),
                app_id: auth.caller.app_id,
                interactive: bool_opt(&options, "interactive", false),
                target,
            }) {
            PortalResult::Success(uri) => {
                let mut values = HashMap::new();
                values.insert(
                    "uri".into(),
                    OwnedValue::from(zbus::zvariant::Str::from(uri)),
                );
                (0, values)
            }
            other => (other.response_code(), HashMap::new()),
        };
        self.lifecycle
            .complete_request_object(connection, &request_path)
            .await;
        response
    }
    async fn pick_color(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        let request_path = handle.to_string();
        let service = self.ctx.desktop_services.screenshot.clone();
        let cleanup_path = request_path.clone();
        let registered = self
            .lifecycle
            .register_request(
                connection,
                &request_path,
                LifecycleEntry {
                    owner: auth.owner.to_string(),
                    app_id: auth.caller.app_id.clone(),
                    resource: LifecycleResource::Prompt,
                },
                move |_| service.cancel(&cleanup_path),
            )
            .await;
        if !matches!(registered, Ok(true)) {
            return (2, HashMap::new());
        }
        let response = match self
            .ctx
            .desktop_services
            .screenshot
            .pick_color(&request_path, &auth.caller.app_id)
        {
            PortalResult::Success((r, g, b))
                if (r.is_finite()
                    && g.is_finite()
                    && b.is_finite()
                    && (0.0..=1.0).contains(&r)
                    && (0.0..=1.0).contains(&g)
                    && (0.0..=1.0).contains(&b)) =>
            {
                let mut values = HashMap::new();
                let Some(color) = color_value((r, g, b)) else {
                    return (2, HashMap::new());
                };
                values.insert("color".into(), color);
                (0, values)
            }
            PortalResult::Success(_) => (2, HashMap::new()),
            other => (other.response_code(), HashMap::new()),
        };
        self.lifecycle
            .complete_request_object(connection, &request_path)
            .await;
        response
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_requires_advertised_bit() {
        let mut o = HashMap::new();
        o.insert("target".into(), OwnedValue::from(2u32));
        assert!(target(&o, 1).is_err());
        assert_eq!(target(&o, 2), Ok(Some(ScreenshotTarget::Window)));
    }
}
