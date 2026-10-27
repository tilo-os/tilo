//! `org.freedesktop.impl.portal.Email` backend.
#[cfg(target_os = "linux")]
use super::desktop_services::EmailRequest;
use super::desktop_services::document_portal_path_from_uri;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::reply::{Ended, OrFail, Reply, Results, answer};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call, lockdown};
#[cfg(target_os = "linux")]
use crate::shell::portal_dialog::{PortalDialogQueue, ReviewDecision, ReviewRequest};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedValue;
#[cfg(target_os = "linux")]
pub struct EmailBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
    dialogs: PortalDialogQueue,
}
#[cfg(target_os = "linux")]
impl EmailBackend {
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
fn strings(
    options: &HashMap<String, OwnedValue>,
    key: &str,
    max_count: usize,
) -> Option<Vec<String>> {
    let Some(value) = options.get(key) else {
        return Some(Vec::new());
    };
    let values = value
        .try_clone()
        .ok()
        .and_then(|v| Vec::<String>::try_from(v).ok())?;
    (values.len() <= max_count
        && values
            .iter()
            .all(|v| !v.is_empty() && v.len() <= 4096 && !v.chars().any(char::is_control)))
    .then_some(values)
}

#[cfg(test)]
fn attachment_uris(values: &[String]) -> bool {
    values.iter().all(|value| {
        value.len() <= 8192
            && value.trim() == value
            && !value.chars().any(char::is_control)
            // Validate confinement now, while the original document grant is
            // still live. The service only forwards the original URI as an
            // encoded, opaque attachment field to the chosen mail handler.
            && document_portal_path_from_uri(value).is_ok()
    })
}

#[cfg(target_os = "linux")]
fn compose_detail(request: &EmailRequest) -> String {
    let mut recipients = request.addresses.clone();
    recipients.extend(request.cc.iter().map(|address| {
        crate::shell::i18n::t_args("portal-email-cc", &[("address", address.as_str().into())])
    }));
    recipients.extend(request.bcc.iter().map(|address| {
        crate::shell::i18n::t_args("portal-email-bcc", &[("address", address.as_str().into())])
    }));
    let mut detail = if recipients.is_empty() {
        crate::shell::i18n::t("portal-email-no-recipient")
    } else {
        recipients.join("\n")
    };
    if let Some(subject) = request.subject.as_deref() {
        detail.push_str("\n\n");
        detail.push_str(&crate::shell::i18n::t_args(
            "portal-email-subject",
            &[("subject", subject.into())],
        ));
    }
    if !request.attachments.is_empty() {
        detail.push_str("\n\n");
        detail.push_str(&crate::shell::i18n::t_args(
            "portal-email-attachments",
            &[("count", request.attachments.len().into())],
        ));
    }
    detail
}
#[interface(name = "org.freedesktop.impl.portal.Email")]
#[cfg(target_os = "linux")]
impl EmailBackend {
    async fn compose_email(
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
            if lockdown::current_policy().disable_application_handlers {
                return Err(Ended::Cancelled);
            }
            let (Some(mut addresses), Some(cc), Some(bcc), Some(attachments)) = (
                strings(&options, "addresses", 128),
                strings(&options, "cc", 128),
                strings(&options, "bcc", 128),
                strings(&options, "attachments", 128),
            ) else {
                return Err(Ended::Failed);
            };
            if let Some(address) = crate::portal::text_option(&options, "address", 4096) {
                addresses.push(address);
            }
            let attachment_paths = attachments
                .iter()
                .map(|uri| document_portal_path_from_uri(uri))
                .collect::<Result<Vec<_>, _>>();
            if addresses.len() > 128 || attachment_paths.is_err() {
                return Err(Ended::Failed);
            }
            let request = EmailRequest {
                app_id: auth.caller.app_id.clone(),
                workspace_id: auth.caller.workspace_id,
                addresses,
                cc,
                bcc,
                subject: crate::portal::text_option(&options, "subject", 998),
                body: crate::portal::text_option(&options, "body", 256 * 1024),
                attachments,
                attachment_paths: attachment_paths.unwrap_or_default(),
                activation_token: crate::portal::text_option(&options, "activation_token", 4096),
            };
            let path = handle.to_string();
            let dialogs = self.dialogs.clone();
            let cancel_path = path.clone();
            let entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id: auth.caller.app_id,
                resource: LifecycleResource::Prompt,
            };
            let cancel = move |_: &LifecycleEntry| {
                let _ = dialogs.cancel(&cancel_path);
            };
            self.lifecycle
                .with_request(connection, &path, entry, cancel, async {
                    let review = self
                        .dialogs
                        .enqueue_review(ReviewRequest {
                            handle: path.clone(),
                            app_id: request.app_id.clone(),
                            title: crate::shell::i18n::t("portal-email-title"),
                            reason: crate::shell::i18n::t("portal-email-reason"),
                            resource: compose_detail(&request),
                            confirm_label: crate::shell::i18n::t("portal-email-confirm"),
                        })
                        .or_fail()?;
                    if !matches!(review.await, Ok(ReviewDecision::Confirmed)) {
                        return Err(Ended::Cancelled);
                    }
                    let result = self.ctx.desktop_services.email.compose(&request);
                    result.into_result().map(|()| Results::new())
                })
                .await
        })
        .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn email_inputs_drop_controls_and_host_paths() {
        {
            let mut v = HashMap::new();
            v.insert(
                "cc".into(),
                OwnedValue::try_from(zbus::zvariant::Value::new(vec![
                    "a@example.test".to_string(),
                    "bad\n".to_string(),
                ]))
                .expect("string arrays serialize into a zvariant"),
            );
            assert_eq!(strings(&v, "cc", 8), None);
        }
        // Phase `host_paths_are_never_attachments`.
        // The helper additionally verifies that a file URI lies under the
        // live document portal mount; arbitrary host paths fail that check.
        assert!(!attachment_uris(&["file:///home/me/private.pdf".into()]));
        assert!(!attachment_uris(&["document://tilo/abc".into()]));
    }
}
