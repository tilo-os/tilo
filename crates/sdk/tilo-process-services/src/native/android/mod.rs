//! The Android implementation of [`ProcessServices`].
//!
//! Every service calls the framework through the activity's JVM
//! ([`with_activity`]), and Tilo's Java classes in the APK ([`bridge`]):
//!
//! - clipboard: the system `ClipboardManager`; Android has no primary
//!   selection, so that target stays in this process;
//! - files: the Storage Access Framework ([`files`]);
//! - opener: an `ACTION_VIEW` intent;
//! - speech: the system `TextToSpeech` engine;
//! - notifications: `NotificationManager`, asking for the runtime permission;
//! - location: `LocationManager`, asking for the runtime permission.
//!
//! Media sessions and screen capture keep [`ProcessServices::null`]'s honest
//! refusals rather than pretending to work.

mod bridge;
mod files;
mod location;
mod notify;
mod speech;

use std::sync::{Arc, OnceLock};

use jni::JNIEnv;
use jni::objects::{JObject, JValue};
use tilo_process::android::with_activity;

use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
use crate::process::{ProcessServices, SpeechFactory, UriOpener};

/// The Android services for this process.
pub fn services(app_id: Option<&str>) -> ProcessServices {
    static CLIPBOARD: OnceLock<Arc<AndroidClipboard>> = OnceLock::new();
    static NOTIFIER: OnceLock<Arc<notify::SystemNotifier>> = OnceLock::new();
    let _ = app_id;
    let clipboard = CLIPBOARD
        .get_or_init(|| Arc::new(AndroidClipboard::default()))
        .clone();
    let notifications = NOTIFIER.get_or_init(Arc::default).clone();
    ProcessServices {
        clipboard,
        files: Arc::new(files::DocumentPicker),
        opener: Arc::new(IntentOpener),
        speech: Arc::new(SystemSpeech),
        notifications,
        location: Arc::new(location::SystemLocation),
        ..ProcessServices::null()
    }
}

fn system_service<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject<'_>,
    name: &str,
) -> jni::errors::Result<JObject<'local>> {
    let name = env.new_string(name)?;
    env.call_method(
        activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValue::Object(&name)],
    )?
    .l()
}

/// The system clipboard. Android has no primary selection: select-to-copy
/// stays in this process instead of overwriting what the user copied.
#[derive(Default)]
struct AndroidClipboard {
    primary: InProcessClipboard,
}

impl AndroidClipboard {
    fn set_system(text: &str, sensitive: bool) -> bool {
        let result = with_activity(|env, activity| {
            let manager = system_service(env, activity, "clipboard")?;
            let label = env.new_string("")?;
            let text = env.new_string(text)?;
            let clip = env
                .call_static_method(
                    "android/content/ClipData",
                    "newPlainText",
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                    &[JValue::Object(&label), JValue::Object(&text)],
                )?
                .l()?;
            if sensitive {
                // Keeps the system's copy confirmation and keyboard
                // suggestions from showing the content.
                let description = env
                    .call_method(
                        &clip,
                        "getDescription",
                        "()Landroid/content/ClipDescription;",
                        &[],
                    )?
                    .l()?;
                let extras = env.new_object("android/os/PersistableBundle", "()V", &[])?;
                let key = env.new_string("android.content.extra.IS_SENSITIVE")?;
                env.call_method(
                    &extras,
                    "putBoolean",
                    "(Ljava/lang/String;Z)V",
                    &[JValue::Object(&key), JValue::Bool(1)],
                )?;
                env.call_method(
                    &description,
                    "setExtras",
                    "(Landroid/os/PersistableBundle;)V",
                    &[JValue::Object(&extras)],
                )?;
            }
            env.call_method(
                &manager,
                "setPrimaryClip",
                "(Landroid/content/ClipData;)V",
                &[JValue::Object(&clip)],
            )?;
            Ok(())
        });
        if let Err(error) = &result {
            tracing::warn!(target: "tilo_ui::clipboard", %error, "android clipboard write failed");
        }
        result.is_ok()
    }

    /// Formatted text: `ClipData.newHtmlText`, `html` with `plain` as the
    /// text every app that reads no formatting pastes.
    fn set_system_html(html: &str, plain: &str) -> bool {
        let result = with_activity(|env, activity| {
            let manager = system_service(env, activity, "clipboard")?;
            let label = env.new_string("")?;
            let plain = env.new_string(plain)?;
            let html = env.new_string(html)?;
            let clip = env
                .call_static_method(
                    "android/content/ClipData",
                    "newHtmlText",
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;Ljava/lang/String;)Landroid/content/ClipData;",
                    &[JValue::Object(&label), JValue::Object(&plain), JValue::Object(&html)],
                )?
                .l()?;
            env.call_method(
                &manager,
                "setPrimaryClip",
                "(Landroid/content/ClipData;)V",
                &[JValue::Object(&clip)],
            )?;
            Ok(())
        });
        if let Err(error) = &result {
            tracing::warn!(target: "tilo_ui::clipboard", %error, "android clipboard write failed");
        }
        result.is_ok()
    }

    /// The HTML of the clip on the clipboard, when it holds formatted text.
    fn system_html() -> Option<String> {
        let result = with_activity(|env, activity| {
            let manager = system_service(env, activity, "clipboard")?;
            let clip = env
                .call_method(
                    &manager,
                    "getPrimaryClip",
                    "()Landroid/content/ClipData;",
                    &[],
                )?
                .l()?;
            if clip.is_null() {
                return Ok(None);
            }
            let count = env.call_method(&clip, "getItemCount", "()I", &[])?.i()?;
            if count < 1 {
                return Ok(None);
            }
            let item = env
                .call_method(
                    &clip,
                    "getItemAt",
                    "(I)Landroid/content/ClipData$Item;",
                    &[JValue::Int(0)],
                )?
                .l()?;
            let html = env
                .call_method(&item, "getHtmlText", "()Ljava/lang/String;", &[])?
                .l()?;
            if html.is_null() {
                return Ok(None);
            }
            let html: String = env.get_string(&html.into())?.into();
            Ok(Some(html))
        });
        match result {
            Ok(html) => html.filter(|html| !html.is_empty()),
            Err(error) => {
                tracing::warn!(target: "tilo_ui::clipboard", %error, "android clipboard read failed");
                None
            }
        }
    }

    fn system_text() -> Option<String> {
        let result = with_activity(|env, activity| {
            let manager = system_service(env, activity, "clipboard")?;
            let clip = env
                .call_method(
                    &manager,
                    "getPrimaryClip",
                    "()Landroid/content/ClipData;",
                    &[],
                )?
                .l()?;
            if clip.is_null() {
                return Ok(None);
            }
            let count = env.call_method(&clip, "getItemCount", "()I", &[])?.i()?;
            if count < 1 {
                return Ok(None);
            }
            let item = env
                .call_method(
                    &clip,
                    "getItemAt",
                    "(I)Landroid/content/ClipData$Item;",
                    &[JValue::Int(0)],
                )?
                .l()?;
            let text = env
                .call_method(
                    &item,
                    "coerceToText",
                    "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                    &[JValue::Object(activity)],
                )?
                .l()?;
            if text.is_null() {
                return Ok(None);
            }
            let text = env
                .call_method(&text, "toString", "()Ljava/lang/String;", &[])?
                .l()?;
            let text: String = env.get_string(&text.into())?.into();
            Ok(Some(text))
        });
        match result {
            Ok(text) => text.filter(|text| !text.is_empty()),
            Err(error) => {
                tracing::warn!(target: "tilo_ui::clipboard", %error, "android clipboard read failed");
                None
            }
        }
    }
}

impl ClipboardBackend for AndroidClipboard {
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        match target {
            ClipboardTarget::Clipboard => Self::set_system(text, false),
            ClipboardTarget::Primary => self.primary.set_text(target, text),
        }
    }

    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        match target {
            ClipboardTarget::Clipboard => Self::set_system(text, true),
            ClipboardTarget::Primary => false,
        }
    }

    fn set_secret_text(&self, text: &str) -> bool {
        Self::set_system(text, true)
    }

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        match target {
            ClipboardTarget::Clipboard => Self::system_text(),
            ClipboardTarget::Primary => self.primary.text(target),
        }
    }

    fn set_rich_text(&self, html: &str, plain: &str) -> bool {
        Self::set_system_html(html, plain)
    }

    fn rich_text(&self) -> Option<crate::clipboard::RichTextOffer> {
        let html = Self::system_html()?;
        Some(crate::clipboard::RichTextOffer {
            html,
            plain: Self::system_text(),
        })
        .filter(crate::clipboard::RichTextOffer::is_valid)
    }

    fn name(&self) -> &'static str {
        "android"
    }
}

/// Hands URIs to whichever app handles them, through `ACTION_VIEW`.
struct IntentOpener;

/// `Intent.FLAG_ACTIVITY_NEW_TASK`: the handler opens as its own task, so
/// Back returns to the app rather than stacking it inside this one.
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

impl UriOpener for IntentOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        // Android refuses to hand `file://` URIs to another app; a local
        // file has to be shared through a content provider instead.
        if uri.starts_with("file:") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "android cannot open file:// URIs in another app",
            ));
        }
        with_activity(|env, activity| {
            let uri = env.new_string(uri)?;
            let uri = env
                .call_static_method(
                    "android/net/Uri",
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[JValue::Object(&uri)],
                )?
                .l()?;
            let action = env.new_string("android.intent.action.VIEW")?;
            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[JValue::Object(&action), JValue::Object(&uri)],
            )?;
            env.call_method(
                &intent,
                "addFlags",
                "(I)Landroid/content/Intent;",
                &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)],
            )?;
            env.call_method(
                activity,
                "startActivity",
                "(Landroid/content/Intent;)V",
                &[JValue::Object(&intent)],
            )?;
            Ok(())
        })
        .map_err(std::io::Error::other)
    }
}

struct SystemSpeech;

impl SpeechFactory for SystemSpeech {
    fn make(&self) -> Box<dyn tts::Speaker> {
        speech::speaker()
    }
}
