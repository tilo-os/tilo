//! The Android implementation of [`HostServices`].
//!
//! Every service calls the framework through the activity's JVM, which
//! `android-activity` publishes in [`ndk_context`] before `android_main` runs:
//!
//! - clipboard: the system `ClipboardManager`; Android has no primary
//!   selection, so that target stays in this process;
//! - opener: an `ACTION_VIEW` intent;
//! - speech: [`tts::default_speaker`].
//!
//! Services without an Android backend yet keep [`HostServices::null`]'s
//! honest refusals rather than pretending to work.

use std::sync::{Arc, OnceLock};

use jni::JNIEnv;
use jni::objects::{JObject, JValue};

use crate::clipboard::{ClipboardBackend, ClipboardTarget, InProcessClipboard};
use crate::host::{HostServices, SpeechFactory, UriOpener};

/// The Android services for this process.
pub fn services(app_id: Option<&str>) -> HostServices {
    let _ = app_id;
    static CLIPBOARD: OnceLock<Arc<AndroidClipboard>> = OnceLock::new();
    let clipboard = CLIPBOARD
        .get_or_init(|| Arc::new(AndroidClipboard::default()))
        .clone();
    HostServices {
        clipboard,
        opener: Arc::new(IntentOpener),
        speech: Arc::new(SystemSpeech),
        ..HostServices::null()
    }
}

/// Run `f` on this thread's JNI environment with the activity as context.
/// A Java exception is cleared and reported as an error, never left pending.
fn with_activity<T>(
    f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    let context = ndk_context::android_context();
    // SAFETY: android-activity stores the process's `JavaVM*` in
    // `ndk_context` before `android_main` and keeps it for the process.
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    // SAFETY: the context is the activity's global reference, valid while
    // the activity lives, which outlives every call the toolkit makes.
    let activity = unsafe { JObject::from_raw(context.context().cast()) };
    let result = f(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result.map_err(|e| e.to_string())
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

    fn text(&self, target: ClipboardTarget) -> Option<String> {
        match target {
            ClipboardTarget::Clipboard => Self::system_text(),
            ClipboardTarget::Primary => self.primary.text(target),
        }
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
        tts::default_speaker()
    }
}
