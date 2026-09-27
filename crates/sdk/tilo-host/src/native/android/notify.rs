//! Notifications through `NotificationManager`, on one channel named after
//! the app. A tag is the notification's tag, so showing it again replaces it.

use std::sync::atomic::{AtomicBool, Ordering};

use jni::objects::JValue;

use super::bridge::{self, NOTIFICATIONS, optional_string};
use crate::host::{Notification, Notifier, NotifyError};

const POST_NOTIFICATIONS: &str = "android.permission.POST_NOTIFICATIONS";
/// Android 13, where posting became a runtime permission.
const RUNTIME_PERMISSION_SDK: i32 = 33;

#[derive(Default)]
pub(super) struct SystemNotifier {
    asked: AtomicBool,
}

impl SystemNotifier {
    /// On Android 13 and later the first notification asks for the runtime
    /// permission; it is not shown, and the answer governs later ones. The
    /// user is asked once per process.
    fn permitted(&self) -> Result<(), NotifyError> {
        if bridge::sdk_int() < RUNTIME_PERMISSION_SDK
            || bridge::is_granted(POST_NOTIFICATIONS).map_err(NotifyError::Failed)?
        {
            return Ok(());
        }
        if self.asked.swap(true, Ordering::Relaxed) {
            return Err(NotifyError::Failed(
                "the user has not allowed notifications".to_string(),
            ));
        }
        bridge::request_permissions(&[POST_NOTIFICATIONS], |answer| match answer {
            Ok(granted) => {
                tracing::info!(target: "tilo_ui::notifications", granted, "notification permission answered");
            }
            Err(error) => {
                tracing::warn!(target: "tilo_ui::notifications", %error, "notification permission prompt failed");
            }
        });
        Err(NotifyError::Failed(
            "asked the user to allow notifications; this one was not shown".to_string(),
        ))
    }

    fn call(method: &str, args: &[&str]) -> Result<(), NotifyError> {
        let refusal = super::with_activity(|env, activity| {
            let notifications = bridge::class(env, activity, NOTIFICATIONS)?;
            let mut strings = Vec::new();
            for arg in args {
                strings.push(env.new_string(arg)?);
            }
            let mut values = vec![JValue::Object(activity)];
            values.extend(strings.iter().map(|string| JValue::Object(string)));
            let signature = format!(
                "(Landroid/content/Context;{})Ljava/lang/String;",
                "Ljava/lang/String;".repeat(args.len())
            );
            let refusal = env
                .call_static_method(&notifications, method, signature, &values)?
                .l()?;
            optional_string(env, refusal)
        })
        .map_err(NotifyError::Failed)?;
        refusal.map_or(Ok(()), |reason| Err(NotifyError::Failed(reason)))
    }
}

impl Notifier for SystemNotifier {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.permitted()?;
        Self::call(
            "show",
            &[&notification.tag, &notification.title, &notification.body],
        )
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        Self::call("withdraw", &[tag])
    }
}
