//! `org.freedesktop.impl.portal.Wallpaper` backend.
#[cfg(target_os = "linux")]
use super::desktop_services::WallpaperRequest;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
#[cfg(target_os = "linux")]
use super::reply::{Ended, OrFail, Results, reply};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call};
#[cfg(target_os = "linux")]
use crate::shell::portal_dialog::{PortalDialogQueue, ReviewDecision, ReviewRequest};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::OwnedValue;
#[cfg(target_os = "linux")]
pub struct WallpaperBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
    dialogs: PortalDialogQueue,
}
#[cfg(target_os = "linux")]
impl WallpaperBackend {
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
fn set_on(options: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = crate::portal::option::<String>(options, "set-on").unwrap_or_else(|| "both".into());
    matches!(value.as_str(), "background" | "lockscreen" | "both").then_some(value)
}
/// xdg-document-portal exports use ordinary `file:` URIs. The service
/// canonicalises them below the document-portal mount before it reads a byte;
/// the input URI is never persisted or displayed after that import.
fn valid_uri(uri: &str) -> bool {
    uri.starts_with("file://")
        && uri.len() <= 8192
        && uri.trim() == uri
        && !uri.chars().any(char::is_control)
}
#[interface(name = "org.freedesktop.impl.portal.Wallpaper")]
#[cfg(target_os = "linux")]
impl WallpaperBackend {
    async fn set_wallpaper_uri(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        uri: String,
        options: HashMap<String, OwnedValue>,
    ) -> u32 {
        let Ok(auth) = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window)).await
        else {
            return 2;
        };
        let Some(set_on) = set_on(&options) else {
            return 2;
        };
        if !valid_uri(&uri) {
            return 2;
        }
        let request = WallpaperRequest {
            app_id: auth.caller.app_id.clone(),
            workspace_id: auth.caller.workspace_id.0.to_string(),
            uri,
            show_preview: crate::portal::option(&options, "show-preview").unwrap_or(false),
            set_on,
        };
        // Import and decode before asking. When `show-preview` is set the
        // shell's DesktopBackdrop switches to this private staged image under
        // the consent sheet; no source URI or host pathname is rendered.
        let staged = match self
            .ctx
            .desktop_services
            .wallpaper
            .stage_wallpaper(&request)
        {
            super::desktop_services::PortalResult::Success(staged) => staged,
            outcome => return outcome.response_code(),
        };
        let path = handle.to_string();
        let dialogs = self.dialogs.clone();
        let cancel_path = path.clone();
        let wallpaper = self.ctx.desktop_services.wallpaper.clone();
        let cleanup_request = request.clone();
        let cleanup_staged = staged.clone();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id,
            resource: LifecycleResource::Prompt,
        };
        let cancel = move |_: &LifecycleEntry| {
            let _ = dialogs.cancel(&cancel_path);
            wallpaper.discard_staged_wallpaper(&cleanup_request, &cleanup_staged);
        };
        let reviewed = self
            .lifecycle
            .with_request(connection, &path, entry, cancel, async {
                let preview = crate::shell::i18n::t(if request.show_preview {
                    "portal-wallpaper-preview"
                } else {
                    "portal-wallpaper-private"
                });
                let review = self
                    .dialogs
                    .enqueue_review(ReviewRequest {
                        handle: path.clone(),
                        app_id: request.app_id.clone(),
                        title: crate::shell::i18n::t("portal-wallpaper-title"),
                        reason: preview,
                        resource: crate::shell::i18n::t_args(
                            "portal-wallpaper-resource",
                            &[("target", request.set_on.as_str().into())],
                        ),
                        confirm_label: crate::shell::i18n::t("portal-wallpaper-confirm"),
                    })
                    .or_fail()?;
                match review.await {
                    Ok(ReviewDecision::Confirmed) => Ok(Results::new()),
                    _ => Err(Ended::Cancelled),
                }
            })
            .await;
        let wallpaper = &self.ctx.desktop_services.wallpaper;
        match reviewed {
            Ok(_) => wallpaper
                .apply_staged_wallpaper(&request, &staged)
                .response_code(),
            Err(ended) => {
                wallpaper.discard_staged_wallpaper(&request, &staged);
                reply(Err(ended)).0
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wallpaper_requests_accept_only_clean_uris_and_standard_targets() {
        assert!(valid_uri("file:///run/user/1000/doc/token/a.png"));
        assert!(!valid_uri("document://tilo/a.png"));
        assert!(!valid_uri("/tmp/a.png"));
        assert!(!valid_uri("file:///tmp/a\n.png"));
        {
            let mut map = HashMap::new();
            map.insert(
                "set-on".into(),
                OwnedValue::from(zbus::zvariant::Str::from("desktop")),
            );
            assert_eq!(set_on(&map), None);
        }
    }
}
