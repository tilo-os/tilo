//! The browser backend: the Notifications API while the page is open.

use js_sys::{Promise, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::{Notification, Notifier, NotifyError};

#[wasm_bindgen(module = "/web/notify.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = notificationPermission)]
    fn notification_permission() -> String;
    #[wasm_bindgen(js_name = showNotification, catch)]
    fn show_notification(tag: &str, title: &str, body: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(js_name = requestNotification)]
    fn request_notification(tag: &str, title: &str, body: &str) -> Promise;
    #[wasm_bindgen(js_name = withdrawNotification)]
    fn withdraw_notification(tag: &str);
}

fn text(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            Reflect::get(error, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{error:?}"))
}

pub(crate) struct PageNotifier;

impl Notifier for PageNotifier {
    /// Shows at once when the site may notify. While the user has not
    /// decided, asks and shows if allowed; that later refusal is logged, not
    /// returned.
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        let Notification {
            tag, title, body, ..
        } = notification;
        match notification_permission().as_str() {
            "granted" => show_notification(tag, title, body)
                .map_err(|error| NotifyError::Failed(text(&error))),
            "default" => {
                let asked = request_notification(tag, title, body);
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(error) = JsFuture::from(asked).await {
                        web_sys::console::warn_1(&error);
                    }
                });
                Ok(())
            }
            "denied" => Err(NotifyError::Failed(
                "notifications are blocked for this site".to_owned(),
            )),
            _ => Err(NotifyError::Unavailable),
        }
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        if notification_permission() == "unavailable" {
            return Err(NotifyError::Unavailable);
        }
        withdraw_notification(tag);
        Ok(())
    }
}
