//! `org.freedesktop.impl.portal.AppChooser` backend.

use super::reply::{Ended, OrFail, Reply, Results, answer, ensure};
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
#[cfg(target_os = "linux")]
use zbus::zvariant::OwnedValue;

#[cfg(target_os = "linux")]
use super::desktop_services::{ApplicationChoiceRequest, PortalResult};
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call, lockdown};

#[cfg(target_os = "linux")]
pub struct AppChooserBackend {
    ctx: PortalContext,
    pending: Arc<Mutex<HashMap<String, ApplicationChoiceRequest>>>,
    lifecycle: PortalLifecycle,
}
#[cfg(target_os = "linux")]
impl AppChooserBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self {
            ctx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            lifecycle,
        }
    }
}
fn valid_choices(choices: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    choices
        .into_iter()
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 255
                && !v.chars().any(char::is_control)
                && seen.insert(v.clone())
        })
        .collect()
}
#[interface(name = "org.freedesktop.impl.portal.AppChooser")]
#[cfg(target_os = "linux")]
impl AppChooserBackend {
    async fn choose_application(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        choices: Vec<String>,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            if lockdown::current_policy().disable_application_handlers {
                return Err(Ended::Cancelled);
            }
            let request = ApplicationChoiceRequest {
                handle: handle.to_string(),
                app_id: auth.caller.app_id.clone(),
                choices: valid_choices(choices),
                last_choice: crate::portal::text_option(&options, "last_choice", 4096),
                content_type: crate::portal::text_option(&options, "content_type", 4096),
                uri: crate::portal::text_option(&options, "uri", 4096),
                filename: crate::portal::text_option(&options, "filename", 4096),
                activation_token: crate::portal::text_option(&options, "activation_token", 4096),
            };
            if request.choices.is_empty() {
                return Err(Ended::Cancelled);
            }
            let entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id: auth.caller.app_id,
                resource: LifecycleResource::Prompt,
            };
            let pending = self.pending.clone();
            let chooser = self.ctx.desktop_services.app_chooser.clone();
            let cleanup_handle = request.handle.clone();
            let cancel = move |_: &LifecycleEntry| {
                if let Ok(mut pending) = pending.lock() {
                    pending.remove(&cleanup_handle);
                }
                chooser.cancel(&cleanup_handle);
            };
            self.lifecycle
                .with_request(connection, &request.handle, entry, cancel, async {
                    if let Ok(mut pending) = self.pending.lock() {
                        pending.insert(request.handle.clone(), request.clone());
                    }
                    let (reply, rx) = tokio::sync::oneshot::channel();
                    let queued = self
                        .ctx
                        .desktop_services
                        .app_chooser
                        .enqueue(request.clone(), reply);
                    let outcome = match queued {
                        PortalResult::Success(()) => rx.await.unwrap_or(PortalResult::Cancelled),
                        PortalResult::Cancelled => PortalResult::Cancelled,
                        PortalResult::Unavailable(error) => PortalResult::Unavailable(error),
                        PortalResult::Failed(error) => PortalResult::Failed(error),
                    };
                    let live_choices = self
                        .pending
                        .lock()
                        .ok()
                        .and_then(|mut pending| pending.remove(&request.handle))
                        .map(|request| request.choices)
                        .unwrap_or_default();
                    let choice = outcome.into_result()?;
                    ensure(live_choices.iter().any(|id| id == &choice.choice))?;
                    let mut out = Results::new();
                    out.insert(
                        "choice".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(choice.choice)),
                    );
                    if let Some(token) = choice.activation_token {
                        out.insert(
                            "activation_token".into(),
                            OwnedValue::from(zbus::zvariant::Str::from(token)),
                        );
                    }
                    Ok(out)
                })
                .await
        })
        .await
    }
    async fn update_choices(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        handle: ObjectPath<'_>,
        choices: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        let choices = valid_choices(choices);
        let key = handle.to_string();
        let app_id = self
            .pending
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("chooser state unavailable".into()))?
            .get(&key)
            .map(|request| request.app_id.clone())
            .ok_or_else(|| {
                zbus::fdo::Error::InvalidArgs("unknown or completed chooser request".into())
            })?;
        // UpdateChoices has no app-id argument.  Reauthenticate against the
        // id bound by ChooseApplication, then prove the request is owned by
        // that same frontend generation.
        let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal frontend".into()))?;
        if !self.lifecycle.has_request(&key, auth.owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "request is not owned by this portal frontend".into(),
            ));
        }
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("chooser state unavailable".into()))?;
        let request = pending.get_mut(&key).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs("unknown or completed chooser request".into())
        })?;
        request.choices = choices.clone();
        drop(pending);
        match self
            .ctx
            .desktop_services
            .app_chooser
            .update_choices(&key, &choices)
        {
            PortalResult::Success(()) => Ok(()),
            PortalResult::Cancelled => {
                Err(zbus::fdo::Error::Failed("chooser was cancelled".into()))
            }
            PortalResult::Unavailable(e) | PortalResult::Failed(e) => {
                Err(zbus::fdo::Error::Failed(e))
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choices_are_deduplicated_and_bounded() {
        assert_eq!(
            valid_choices(vec!["a".into(), "a".into(), "".into(), "b".into()]),
            vec!["a", "b"]
        );
    }
}
