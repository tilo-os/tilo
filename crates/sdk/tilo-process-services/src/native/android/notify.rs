//! Notifications through `NotificationManager`, on one channel named after
//! the app. A tag is the notification's tag, so showing it again replaces it.
//! A notification's link, and the links of its buttons, reach the app as the
//! launch intent's extra, which the app reads as a tap.

use std::sync::atomic::{AtomicBool, Ordering};

use jni::JNIEnv;
use jni::objects::{JObject, JValue};

use super::bridge::{self, NOTIFICATIONS, optional_string};
use crate::process::{Notification, Notifier, NotifyError};

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

impl SystemNotifier {
    /// The Java `String[]` of `items`, for the call that takes them.
    fn strings<'local>(
        env: &mut JNIEnv<'local>,
        items: &[jni::objects::JString<'local>],
    ) -> jni::errors::Result<jni::objects::JObjectArray<'local>> {
        let array = env.new_object_array(
            jni::sys::jsize::try_from(items.len()).unwrap_or(i32::MAX),
            "java/lang/String",
            JObject::null(),
        )?;
        for (index, item) in items.iter().enumerate() {
            env.set_object_array_element(
                &array,
                jni::sys::jsize::try_from(index).unwrap_or_default(),
                item,
            )?;
        }
        Ok(array)
    }

    /// `show` with the notification's link and its buttons' labels and links.
    fn show_with_links(&self, notification: &Notification) -> Result<(), NotifyError> {
        let refusal = super::with_activity(|env, activity| {
            let notifications = bridge::class(env, activity, NOTIFICATIONS)?;
            let tag = env.new_string(&notification.tag)?;
            let title = env.new_string(&notification.title)?;
            let body = env.new_string(&notification.body)?;
            let activation =
                env.new_string(notification.activation.as_deref().unwrap_or_default())?;
            let buttons: Vec<_> = notification
                .buttons
                .iter()
                .take(crate::notify::MAX_BUTTONS)
                .collect();
            let mut labels = Vec::with_capacity(buttons.len());
            let mut links = Vec::with_capacity(buttons.len());
            for button in &buttons {
                labels.push(env.new_string(&button.label)?);
                links.push(env.new_string(&button.activation)?);
            }
            let labels = Self::strings(env, &labels)?;
            let links = Self::strings(env, &links)?;
            let refusal = env
                .call_static_method(
                    &notifications,
                    "show",
                    "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;\
                     Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;\
                     [Ljava/lang/String;)Ljava/lang/String;",
                    &[
                        JValue::Object(activity),
                        JValue::Object(&tag),
                        JValue::Object(&title),
                        JValue::Object(&body),
                        JValue::Object(&activation),
                        JValue::Object(&labels),
                        JValue::Object(&links),
                    ],
                )?
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
        self.show_with_links(notification)
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        Self::call("withdraw", &[tag])
    }
}
