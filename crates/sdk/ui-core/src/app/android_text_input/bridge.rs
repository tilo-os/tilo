//! The JNI seam to `so.tilo.android.TiloTextInput`.
//!
//! The natives are registered from here rather than exported by name, so they
//! resolve however NativeActivity loaded the library. They run on the Java
//! main thread and only queue: the event loop drains the queue on
//! [`UserEvent::PlatformTextInput`], so every edit reaches the widget tree on
//! the UI thread, in order.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint};
use jni::{JNIEnv, JavaVM, NativeMethod};
use winit::event_loop::EventLoopProxy;
use winit::window::ImePurpose;

use super::{TextInputOp, named_key};
use crate::app::{ImeWindowState, UserEvent};
use crate::ime::ImeSurroundingText;
use tilo_lock::MutexRecover as _;

const CLASS: &str = "so.tilo.android.TiloTextInput";
const MAX_DELETE: u32 = 4096;

struct Java {
    vm: JavaVM,
    class: GlobalRef,
}

static JAVA: OnceLock<Java> = OnceLock::new();
static PROXY: Mutex<Option<EventLoopProxy<UserEvent>>> = Mutex::new(None);
static PENDING: Mutex<VecDeque<TextInputOp>> = Mutex::new(VecDeque::new());

/// Connects the activity's `TiloTextInput` to this event loop. `false` when
/// the activity has none, and winit's own keyboard toggle is all there is.
pub(in crate::app) fn install(proxy: EventLoopProxy<UserEvent>) -> bool {
    *PROXY.lock_recover() = Some(proxy);
    if JAVA.get().is_none() {
        match connect() {
            Ok(Some(java)) => {
                let _ = JAVA.set(java);
            }
            Ok(None) => {
                tracing::info!(
                    target: "tilo_ui::ime",
                    "activity has no TiloTextInput; soft keyboards cannot type"
                );
            }
            Err(error) => {
                tracing::warn!(target: "tilo_ui::ime", %error, "TiloTextInput bridge failed");
            }
        }
    }
    installed()
}

pub(in crate::app) fn installed() -> bool {
    JAVA.get().is_some()
}

/// Every edit queued since the last drain, oldest first.
pub(in crate::app) fn drain() -> Vec<TextInputOp> {
    std::mem::take(&mut *PENDING.lock_recover()).into()
}

/// Points the keyboard at the focused field, or dismisses it for `None`.
/// `restart` discards what the input method knows of the previous field.
/// `surrounding` is what the next input connection starts from.
pub(in crate::app) fn update(
    field: Option<&ImeWindowState>,
    surrounding: Option<&ImeSurroundingText>,
    scale: f64,
    restart: bool,
) {
    let physical = |logical: u32| {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a caret rectangle in physical pixels fits the window, far inside i32"
        )]
        let pixels = (f64::from(logical) * scale).round() as i32;
        JValue::Int(pixels)
    };
    let (enabled, purpose, rect) = match field {
        Some(field) => (
            true,
            match field.purpose {
                ImePurpose::Password => 1,
                ImePurpose::Terminal => 2,
                _ => 0,
            },
            [field.x, field.y, field.width, field.height].map(physical),
        ),
        None => (false, 0, [JValue::Int(0); 4]),
    };
    let [x, y, width, height] = rect;
    let (text, [anchor, focus]) = surrounding.map_or((String::new(), [0; 2]), utf16_selection);
    with_env("update", |env, class| {
        let text = env.new_string(text)?;
        env.call_static_method(
            class,
            "update",
            "(ZIIIIIZLjava/lang/String;II)V",
            &[
                JValue::Bool(jboolean::from(enabled)),
                JValue::Int(purpose),
                x,
                y,
                width,
                height,
                JValue::Bool(jboolean::from(restart)),
                JValue::Object(&text),
                JValue::Int(anchor),
                JValue::Int(focus),
            ],
        )?;
        Ok(())
    });
}

/// The text with its selection in UTF-16 units, which Java strings count in.
fn utf16_selection(surrounding: &ImeSurroundingText) -> (String, [jint; 2]) {
    let units = |byte: usize| {
        let count = surrounding
            .text
            .get(..byte)
            .map_or(0, |prefix| prefix.encode_utf16().count());
        jint::try_from(count).unwrap_or(jint::MAX)
    };
    (
        surrounding.text.clone(),
        [units(surrounding.anchor), units(surrounding.focus)],
    )
}

/// The user touched the window while a field had the keyboard: bring the
/// keyboard back if it was dismissed and forget the old caret context.
pub(in crate::app) fn touched() {
    call("touched", "()V", &[]);
}

fn call(name: &str, sig: &str, args: &[JValue<'_, '_>]) {
    with_env(name, |env, class| {
        env.call_static_method(class, name, sig, args)?;
        Ok(())
    });
}

fn with_env(
    name: &str,
    body: impl FnOnce(&mut JNIEnv<'_>, &JClass<'_>) -> jni::errors::Result<()>,
) {
    let Some(java) = JAVA.get() else {
        return;
    };
    let Ok(mut env) = java.vm.attach_current_thread_permanently() else {
        return;
    };
    let class: &JClass<'_> = java.class.as_obj().into();
    if let Err(error) = env.with_local_frame(8, |env| body(env, class)) {
        clear_exception(&env);
        tracing::warn!(target: "tilo_ui::ime", %error, name, "TiloTextInput call failed");
    }
}

fn connect() -> jni::errors::Result<Option<Java>> {
    let context = ndk_context::android_context();
    // SAFETY: android-activity stores the process's `JavaVM*` in
    // `ndk_context` before `android_main` and keeps it for the process.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }?;
    let class = {
        let mut env = vm.attach_current_thread_permanently()?;
        // SAFETY: the context is the activity's global reference, valid
        // while the activity lives.
        let activity = unsafe { JObject::from_raw(context.context().cast()) };
        // `FindClass` on this thread would search the system class loader,
        // which does not have the app's classes.
        let loader = env
            .call_method(
                &activity,
                "getClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )?
            .l()?;
        let name = env.new_string(CLASS)?;
        let loaded = env.call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        );
        let class: JClass<'_> = match loaded {
            Ok(class) => class.l()?.into(),
            Err(jni::errors::Error::JavaException) => {
                clear_exception(&env);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        env.register_native_methods(&class, &natives())?;
        if !env.call_static_method(&class, "attach", "()Z", &[])?.z()? {
            return Ok(None);
        }
        env.new_global_ref(class)?
    };
    Ok(Some(Java { vm, class }))
}

fn natives() -> [NativeMethod; 4] {
    [
        NativeMethod {
            name: "nativeCommit".into(),
            sig: "(Ljava/lang/String;)V".into(),
            fn_ptr: native_commit as *mut c_void,
        },
        NativeMethod {
            name: "nativePreedit".into(),
            sig: "(Ljava/lang/String;)V".into(),
            fn_ptr: native_preedit as *mut c_void,
        },
        NativeMethod {
            name: "nativeKey".into(),
            sig: "(I)V".into(),
            fn_ptr: native_key as *mut c_void,
        },
        NativeMethod {
            name: "nativeDelete".into(),
            sig: "(II)V".into(),
            fn_ptr: native_delete as *mut c_void,
        },
    ]
}

fn clear_exception(env: &JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
}

fn java_string(env: &mut JNIEnv<'_>, text: &JString<'_>) -> Option<String> {
    let text = env.get_string(text).map(String::from);
    if text.is_err() {
        clear_exception(env);
    }
    text.ok()
}

fn push(op: TextInputOp) {
    let mut pending = PENDING.lock_recover();
    let wake = pending.is_empty();
    pending.push_back(op);
    drop(pending);
    if !wake {
        return;
    }
    let proxy = PROXY.lock_recover().clone();
    if let Some(proxy) = proxy {
        let _ = proxy.send_event(UserEvent::PlatformTextInput);
    }
}

extern "system" fn native_commit(mut env: JNIEnv<'_>, _class: JClass<'_>, text: JString<'_>) {
    if let Some(text) = java_string(&mut env, &text) {
        push(TextInputOp::Commit(text));
    }
}

extern "system" fn native_preedit(mut env: JNIEnv<'_>, _class: JClass<'_>, text: JString<'_>) {
    if let Some(text) = java_string(&mut env, &text) {
        push(TextInputOp::Preedit(text));
    }
}

extern "system" fn native_key(_env: JNIEnv<'_>, _class: JClass<'_>, key_code: jint) {
    if let Some(key) = named_key(key_code) {
        push(TextInputOp::Key(key));
    }
}

extern "system" fn native_delete(_env: JNIEnv<'_>, _class: JClass<'_>, before: jint, after: jint) {
    // Each grapheme becomes a key press; a keyboard asking for "everything"
    // must not stall the event loop.
    let count = |n: jint| u32::try_from(n).unwrap_or(0).min(MAX_DELETE);
    push(TextInputOp::Delete {
        before: count(before),
        after: count(after),
    });
}
