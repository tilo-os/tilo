//! This process's Java VM on Android: every call into the framework (the clipboard, the
//! Keystore, a file picker) runs through [`with_activity`].
//!
//! `android-activity` publishes the process's `JavaVM*` and the activity in [`ndk_context`]
//! before `android_main` runs, and keeps them for the process.

use jni::JNIEnv;
use jni::objects::{JObject, JString};

/// Run `f` on this thread's JNI environment with the activity as context.
/// Local references `f` creates are released when it returns. A Java
/// exception is cleared and reported as the error, never left pending.
///
/// # Errors
/// The VM refused this thread, or `f` failed: a Java exception's message, or
/// what JNI said.
pub fn with_activity<T>(
    f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    let context = ndk_context::android_context();
    // SAFETY: android-activity stores the process's `JavaVM*` in
    // `ndk_context` before `android_main` and keeps it for the process.
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.map_err(|e| e.to_string())?;
    // Permanent: the toolkit calls in from the same few threads every frame.
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|e| e.to_string())?;
    // SAFETY: the context is the activity's global reference, valid while
    // the activity lives, which outlives every call the process makes.
    let activity = unsafe { JObject::from_raw(context.context().cast()) };
    env.with_local_frame(16, |env| {
        let result = f(env, &activity);
        let exception = pending_exception(env);
        Ok::<_, jni::errors::Error>(match result {
            Ok(value) => Ok(value),
            Err(jni::errors::Error::JavaException) => {
                Err(exception.unwrap_or_else(|| "a Java exception was thrown".to_string()))
            }
            Err(error) => Err(error.to_string()),
        })
    })
    .map_err(|e| e.to_string())?
}

/// Clear a pending Java exception and return its message.
pub fn pending_exception(env: &mut JNIEnv<'_>) -> Option<String> {
    if !env.exception_check().unwrap_or(false) {
        return None;
    }
    let throwable = env.exception_occurred().ok();
    let _ = env.exception_clear();
    let throwable = throwable?;
    let mut describe = |method: &str| -> Option<String> {
        let text = env
            .call_method(&throwable, method, "()Ljava/lang/String;", &[])
            .and_then(jni::objects::JValueGen::l);
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        let text = text.ok()?;
        if text.is_null() {
            return None;
        }
        env.get_string(&JString::from(text)).ok().map(Into::into)
    };
    describe("getMessage").or_else(|| describe("toString"))
}
