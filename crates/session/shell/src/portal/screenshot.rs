//! `org.freedesktop.impl.portal.Screenshot` backend.
use super::desktop_services::ScreenshotTarget;
#[cfg(target_os = "linux")]
use super::desktop_services::{PortalResult, ScreenshotRequest};
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::reply::{Ended, OrFail, Reply, Results, answer};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call, lockdown};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedValue;
#[cfg(target_os = "linux")]
pub struct ScreenshotBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
}
#[cfg(target_os = "linux")]
impl ScreenshotBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }
    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self { ctx, lifecycle }
    }
}
fn target(
    options: &HashMap<String, OwnedValue>,
    available: u32,
) -> Result<Option<ScreenshotTarget>, ()> {
    let Some(raw) = crate::portal::option::<u32>(options, "target") else {
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
#[cfg(target_os = "linux")]
fn color_value(color: (f64, f64, f64)) -> Option<OwnedValue> {
    OwnedValue::try_from(zbus::zvariant::Value::new(color)).ok()
}
#[interface(name = "org.freedesktop.impl.portal.Screenshot")]
#[cfg(target_os = "linux")]
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
    ) -> Reply {
        answer(async move {
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            if lockdown::current_policy().disable_save_to_disk {
                return Err(Ended::Cancelled);
            }
            let available = self.ctx.desktop_services.screenshot.available_targets();
            let target = target(&options, available).or_fail()?;
            let request_path = handle.to_string();
            let service = self.ctx.desktop_services.screenshot.clone();
            let cleanup_path = request_path.clone();
            let cancel = move |_: &LifecycleEntry| service.cancel_color(&cleanup_path);
            self.lifecycle
                .with_request(
                    connection,
                    &request_path,
                    LifecycleEntry {
                        owner: auth.owner.to_string(),
                        app_id: auth.caller.app_id.clone(),
                        resource: LifecycleResource::Prompt,
                    },
                    cancel,
                    async {
                        match self
                            .ctx
                            .desktop_services
                            .screenshot
                            .screenshot(&ScreenshotRequest {
                                request_handle: request_path.clone(),
                                caller: auth.caller.clone(),
                                app_id: auth.caller.app_id,
                                interactive: crate::portal::option(&options, "interactive")
                                    .unwrap_or(false),
                                target,
                            }) {
                            PortalResult::Success(uri) => {
                                let mut values = Results::new();
                                values.insert(
                                    "uri".into(),
                                    OwnedValue::from(zbus::zvariant::Str::from(uri)),
                                );
                                Ok(values)
                            }
                            other => other.into_result().map(|_| Results::new()),
                        }
                    },
                )
                .await
        })
        .await
    }
    async fn pick_color(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            let request_path = handle.to_string();
            let service = self.ctx.desktop_services.screenshot.clone();
            let cleanup_path = request_path.clone();
            let cancel = move |_: &LifecycleEntry| service.cancel(&cleanup_path);
            self.lifecycle
                .with_request(
                    connection,
                    &request_path,
                    LifecycleEntry {
                        owner: auth.owner.to_string(),
                        app_id: auth.caller.app_id.clone(),
                        resource: LifecycleResource::Prompt,
                    },
                    cancel,
                    async {
                        match self
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
                                let mut values = Results::new();
                                let color = color_value((r, g, b)).or_fail()?;
                                values.insert("color".into(), color);
                                Ok(values)
                            }
                            PortalResult::Success(_) => Err(Ended::Failed),
                            other => other.into_result().map(|_| Results::new()),
                        }
                    },
                )
                .await
        })
        .await
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
