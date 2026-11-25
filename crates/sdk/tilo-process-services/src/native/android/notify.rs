//! Notifications through `NotificationManager`, on one channel named after
//! the app. A tag is the notification's tag, so showing it again replaces it.
//! A notification's link, and the links of its buttons, reach the app as the
//! launch intent's extra, which the app reads as a tap.

use std::sync::atomic::{AtomicBool, Ordering};

use jni::JNIEnv;
use jni::objects::{JObject, JValue};

use super::bridge::{self, ALARMS, NOTIFICATIONS, optional_string};
use crate::process::{Notification, Notifier, NotifyError};
use tilo_notify::{Scheduled, SystemTime};

const POST_NOTIFICATIONS: &str = "android.permission.POST_NOTIFICATIONS";
/// Android 13, where posting became a runtime permission.
const RUNTIME_PERMISSION_SDK: i32 = 33;

#[derive(Default)]
pub(super) struct SystemNotifier {
    asked: AtomicBool,
}

impl SystemNotifier {
    /// On Android 13 and later the first notification asks for the runtime
    /// permission; it is not shown, and the answer decides later ones. The
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
    fn show_with_links(notification: &Notification) -> Result<(), NotifyError> {
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
        Self::show_with_links(notification)
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        Self::call("withdraw", &[tag])
    }

    /// An alarm: `AlarmManager.setAlarmClock` wakes the phone exactly at `at`, and the
    /// notification rings full screen whether or not the app runs then (`TiloAlarms`).
    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        self.permitted()?;
        let at_ms = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .ok_or(NotifyError::Invalid(
                "an alarm's moment is before the epoch",
            ))?;
        let refusal = super::with_activity(|env, activity| {
            let alarms = bridge::class(env, activity, ALARMS)?;
            let tag = env.new_string(&notification.tag)?;
            let title = env.new_string(&notification.title)?;
            let body = env.new_string(&notification.body)?;
            let labels: Vec<&str> = notification.buttons.iter().map(|b| b.label.as_str()).collect();
            let uris: Vec<&str> = notification
                .buttons
                .iter()
                .map(|b| b.activation.as_str())
                .collect();
            let labels = string_array(env, &labels)?;
            let uris = string_array(env, &uris)?;
            let refusal = env
                .call_static_method(
                    &alarms,
                    "schedule",
                    "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;J[Ljava/lang/String;[Ljava/lang/String;)Ljava/lang/String;",
                    &[
                        JValue::Object(activity),
                        JValue::Object(&tag),
                        JValue::Object(&title),
                        JValue::Object(&body),
                        JValue::Long(at_ms),
                        JValue::Object(&labels),
                        JValue::Object(&uris),
                    ],
                )?
                .l()?;
            optional_string(env, refusal)
        })
        .map_err(NotifyError::Failed)?;
        refusal.map_or(Ok(()), |reason| Err(NotifyError::Failed(reason)))
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        let refusal = super::with_activity(|env, activity| {
            let alarms = bridge::class(env, activity, ALARMS)?;
            let tag = env.new_string(tag)?;
            let refusal = env
                .call_static_method(
                    &alarms,
                    "cancel",
                    "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(activity), JValue::Object(&tag)],
                )?
                .l()?;
            optional_string(env, refusal)
        })
        .map_err(NotifyError::Failed)?;
        refusal.map_or(Ok(()), |reason| Err(NotifyError::Failed(reason)))
    }

    /// What `TiloAlarms` keeps armed: one it no longer lists rang.
    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        let listed = super::with_activity(|env, activity| {
            let alarms = bridge::class(env, activity, ALARMS)?;
            let listed = env
                .call_static_method(
                    &alarms,
                    "scheduled",
                    "(Landroid/content/Context;)Ljava/lang/String;",
                    &[JValue::Object(activity)],
                )?
                .l()?;
            optional_string(env, listed)
        })
        .map_err(NotifyError::Failed)?
        .unwrap_or_default();
        Ok(listed
            .lines()
            .filter_map(|line| {
                let (tag, at) = line.split_once('\t')?;
                let at = SystemTime::UNIX_EPOCH
                    .checked_add(std::time::Duration::from_millis(at.parse().ok()?))?;
                Some(Scheduled {
                    tag: tag.to_owned(),
                    at,
                })
            })
            .collect())
    }
}

/// `TiloActivity.nativeActivated`: the app was opened with the URI a notification's button
/// names, which its activation handler carries out.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloActivity_nativeActivated<'local>(
    mut env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    uri: jni::objects::JString<'local>,
) {
    let Ok(uri) = env.get_string(&uri).map(String::from) else {
        return;
    };
    tilo_notify::report_activation(&uri);
}

/// `items` as a Java `String[]`.
fn string_array<'local>(
    env: &mut jni::JNIEnv<'local>,
    items: &[&str],
) -> jni::errors::Result<jni::objects::JObjectArray<'local>> {
    let length = i32::try_from(items.len()).unwrap_or(i32::MAX);
    let array = env.new_object_array(length, "java/lang/String", JObject::null())?;
    for (index, item) in (0..length).zip(items) {
        let item = env.new_string(item)?;
        env.set_object_array_element(&array, index, item)?;
    }
    Ok(array)
}
