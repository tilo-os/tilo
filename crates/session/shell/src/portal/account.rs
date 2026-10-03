//! `org.freedesktop.impl.portal.Account` backend.

use std::collections::HashMap;

#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedValue;

#[cfg(target_os = "linux")]
use crate::shell::portal_dialog::{PortalDialogQueue, ReviewDecision, ReviewRequest};

#[cfg(target_os = "linux")]
use super::desktop_services::PortalResult;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};

#[cfg(target_os = "linux")]
pub struct AccountBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
    dialogs: PortalDialogQueue,
}
#[cfg(target_os = "linux")]
impl AccountBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self {
            ctx,
            lifecycle,
            dialogs: PortalDialogQueue::global(),
        }
    }
}

fn option_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options
        .get(key)?
        .try_clone()
        .ok()
        .and_then(|v| String::try_from(v).ok())
        .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
}

#[interface(name = "org.freedesktop.impl.portal.Account")]
#[cfg(target_os = "linux")]
impl AccountBackend {
    async fn get_user_information(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        window: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) = authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&window)).await
        else {
            return (2, HashMap::new());
        };
        let reason = option_string(&options, "reason");
        let path = handle.to_string();
        let dialogs = self.dialogs.clone();
        let cancel_path = path.clone();
        if !matches!(
            self.lifecycle
                .register_request(
                    connection,
                    &path,
                    LifecycleEntry {
                        owner: auth.owner.to_string(),
                        app_id: auth.caller.app_id.clone(),
                        resource: LifecycleResource::Prompt,
                    },
                    move |_| {
                        let _ = dialogs.cancel(&cancel_path);
                    },
                )
                .await,
            Ok(true)
        ) {
            return (2, HashMap::new());
        }
        let review = match self.dialogs.enqueue_review(ReviewRequest {
            handle: path.clone(),
            app_id: auth.caller.app_id.clone(),
            title: crate::shell::i18n::t("portal-account-title"),
            reason: reason
                .clone()
                .unwrap_or_else(|| crate::shell::i18n::t("portal-account-reason")),
            resource: crate::shell::i18n::t("portal-account-resource"),
            confirm_label: crate::shell::i18n::t("portal-account-confirm"),
        }) {
            Ok(review) => review,
            Err(_) => {
                self.lifecycle
                    .complete_request_object(connection, &path)
                    .await;
                return (2, HashMap::new());
            }
        };
        if !matches!(review.await, Ok(ReviewDecision::Confirmed)) {
            self.lifecycle
                .complete_request_object(connection, &path)
                .await;
            return (1, HashMap::new());
        }
        let result = match self
            .ctx
            .desktop_services
            .account
            .get(&auth.caller.app_id, reason.as_deref())
        {
            PortalResult::Success(info) => {
                let mut result = HashMap::new();
                result.insert(
                    "id".into(),
                    OwnedValue::from(zbus::zvariant::Str::from(info.id)),
                );
                result.insert(
                    "name".into(),
                    OwnedValue::from(zbus::zvariant::Str::from(info.name)),
                );
                if let Some(image) = info.image_uri {
                    result.insert(
                        "image".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(image)),
                    );
                }
                (0, result)
            }
            outcome => {
                tracing::debug!(?outcome, "Account.GetUserInformation did not complete");
                (outcome.response_code(), HashMap::new())
            }
        };
        self.lifecycle
            .complete_request_object(connection, &path)
            .await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reason_rejects_control_characters() {
        let mut options = HashMap::new();
        options.insert(
            "reason".into(),
            OwnedValue::from(zbus::zvariant::Str::from("line\nbreak")),
        );
        assert_eq!(option_string(&options, "reason"), None);
    }
}
