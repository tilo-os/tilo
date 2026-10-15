//! `org.freedesktop.impl.portal.Account` backend.

use super::reply::{Ended, OrFail, Reply, Results, answer};
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedValue;

#[cfg(target_os = "linux")]
use crate::shell::portal_dialog::{PortalDialogQueue, ReviewDecision, ReviewRequest};

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
    ) -> Reply {
        answer(async move {
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&window))
                .await
                .or_fail()?;
            let reason =
                crate::portal::text_option(&options, "reason", 1024).filter(|s| !s.is_empty());
            let path = handle.to_string();
            let dialogs = self.dialogs.clone();
            let cancel_path = path.clone();
            let cancel = move |_: &LifecycleEntry| {
                let _ = dialogs.cancel(&cancel_path);
            };
            self.lifecycle
                .with_request(
                    connection,
                    &path,
                    LifecycleEntry {
                        owner: auth.owner.to_string(),
                        app_id: auth.caller.app_id.clone(),
                        resource: LifecycleResource::Prompt,
                    },
                    cancel,
                    async {
                        let review = self
                            .dialogs
                            .enqueue_review(ReviewRequest {
                                handle: path.clone(),
                                app_id: auth.caller.app_id.clone(),
                                title: crate::shell::i18n::t("portal-account-title"),
                                reason: reason.clone().unwrap_or_else(|| {
                                    crate::shell::i18n::t("portal-account-reason")
                                }),
                                resource: crate::shell::i18n::t("portal-account-resource"),
                                confirm_label: crate::shell::i18n::t("portal-account-confirm"),
                            })
                            .or_fail()?;
                        if !matches!(review.await, Ok(ReviewDecision::Confirmed)) {
                            return Err(Ended::Cancelled);
                        }
                        match self
                            .ctx
                            .desktop_services
                            .account
                            .get(&auth.caller.app_id, reason.as_deref())
                            .into_result()
                        {
                            Ok(info) => {
                                let mut result = Results::new();
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
                                Ok(result)
                            }
                            Err(ended) => {
                                tracing::debug!(
                                    ?ended,
                                    "Account.GetUserInformation did not complete"
                                );
                                Err(ended)
                            }
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
    fn reason_rejects_control_characters() {
        let mut options = HashMap::new();
        options.insert(
            "reason".into(),
            OwnedValue::from(zbus::zvariant::Str::from("line\nbreak")),
        );
        assert_eq!(
            crate::portal::text_option(&options, "reason", 1024).filter(|s| !s.is_empty()),
            None
        );
    }
}
