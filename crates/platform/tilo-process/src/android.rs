//! This process's Java VM on Android: every call into the framework (the clipboard, the
//! Keystore, a file picker) runs through [`with_activity`], with the context [`ndk_context`]
//! holds.
//!
//! Two things publish that context. The app, as soon as its file provider or its activity is
//! created, whichever comes first: it publishes the application's `Context`
//! ([`provide_application_context`]) for the life of the process, so the Keystore holding the
//! device's key is reached before any window opens, and when another app only browses this one's
//! files and none ever does. And android-activity, which publishes the activity over it before
//! `android_main` runs and takes it back once `android_main` returns, leaving the application's
//! published again: `ndk_context` keeps the contexts as a stack (`vendor/ndk-context`), so there
//! is no moment without one. `android_main` says when the activity's is published
//! ([`activity_started`]) and when it is about to be taken back ([`activity_ended`]). This module
//! reads `ndk_context` only once one of the two is published.

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};

use jni::JNIEnv;
use jni::objects::{GlobalRef, JObject, JString};

/// Who published the context `ndk_context` holds now.
#[derive(Default)]
struct Published {
    /// android-activity's activity, while `android_main` runs, over the application's.
    activity: bool,
    /// The application's context, published for the life of the process.
    application: bool,
    /// The global reference the application's published pointer points to, kept for the process.
    kept: Option<GlobalRef>,
}

static PUBLISHED: Mutex<Published> = Mutex::new(Published {
    activity: false,
    application: false,
    kept: None,
});

fn published() -> std::sync::MutexGuard<'static, Published> {
    PUBLISHED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Publish `vm` and `context` in `ndk_context` as the application's, for the life of the process,
/// unless a context is published already. Answers whether this call published it.
///
/// # Safety
/// `vm` is the process's `JavaVM*` and `context` a global reference to a `Context` that lives as
/// long as the process.
unsafe fn publish_application(vm: *mut c_void, context: *mut c_void) -> bool {
    let mut published = published();
    // The activity's would sit under it, and android-activity's taking it back would take the
    // application's instead: the activity runs, so the activity's is what calls use.
    if published.activity || published.application {
        return false;
    }
    // SAFETY: the caller vouches for the pointers, which stay valid for the process.
    unsafe { ndk_context::initialize_android_context(vm, context) };
    published.application = true;
    true
}

/// android-activity published the activity in `ndk_context` and runs `android_main` now.
///
/// # Safety
/// Call it only from `android_main`, which android-activity calls once it published the context,
/// and call [`activity_ended`] before `android_main` returns.
pub unsafe fn activity_started() {
    published().activity = true;
}

/// `android_main` returns: android-activity takes its context back, and the application's, when
/// it was published, is the one calls use again.
pub fn activity_ended() {
    published().activity = false;
}

/// Publish the application's context of `context` (any `Context` of the app: its provider's or
/// its activity's) in `ndk_context` for the life of the process, unless one is published
/// already: what a call into the framework runs with while no activity does. Answers whether
/// this call published it.
///
/// # Errors
/// What JNI said when the application's context or the VM could not be reached.
pub fn provide_application_context(
    env: &mut JNIEnv<'_>,
    context: &JObject<'_>,
) -> Result<bool, String> {
    {
        let published = published();
        if published.activity || published.application {
            return Ok(false);
        }
    }
    let application = env
        .call_method(
            context,
            "getApplicationContext",
            "()Landroid/content/Context;",
            &[],
        )
        .and_then(jni::objects::JValueGen::l)
        .map_err(|error| pending_exception(env).unwrap_or_else(|| error.to_string()))?;
    // A provider created before the application finished starting has none yet: its own context
    // is the application package's all the same.
    let application = if application.is_null() {
        env.new_global_ref(context)
    } else {
        env.new_global_ref(&application)
    }
    .map_err(|error| error.to_string())?;
    let vm = env.get_java_vm().map_err(|error| error.to_string())?;
    let raw = application.as_obj().as_raw().cast::<c_void>();
    // SAFETY: the VM is the one this is called in, and `application` is a global reference kept
    // below for the process.
    let made = unsafe { publish_application(vm.get_java_vm_pointer().cast(), raw) };
    if made {
        published().kept = Some(application);
    }
    Ok(made)
}

/// Why no call into the framework can be made: the app published no context yet, nor did its
/// activity.
const NO_CONTEXT: &str = "this process has no Android context: the app has not published its \
                          application's, and no activity runs";

/// The context `ndk_context` holds, while one is published.
fn context() -> Result<ndk_context::AndroidContext, String> {
    let published = published();
    let context = (published.activity || published.application).then(ndk_context::android_context);
    drop(published);
    context.ok_or_else(|| NO_CONTEXT.to_owned())
}

/// Run `f` on this thread's JNI environment with the published context: the activity while it
/// runs, else the application's.
/// Local references `f` creates are released when it returns. A Java
/// exception is cleared and reported as the error, never left pending.
///
/// # Errors
/// No context is published, the VM refused this thread, or `f` failed: a Java exception's
/// message, or what JNI said.
pub fn with_activity<T>(
    f: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    let published = context()?;
    // SAFETY: the published `JavaVM*` is the process's, which lives as long as the process.
    let vm = unsafe { jni::JavaVM::from_raw(published.vm().cast()) }.map_err(|e| e.to_string())?;
    let context = published.context().cast::<jni::sys::_jobject>();
    // Permanent: the toolkit calls in from the same few threads every frame.
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|e| e.to_string())?;
    // SAFETY: the context is a global reference, the activity's valid while `android_main` runs
    // or the application's kept for the process.
    let context = unsafe { JObject::from_raw(context) };
    env.with_local_frame(16, |env| {
        let result = f(env, &context);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The process's one context, in the order Android publishes it: one test, since it is the
    /// process's.
    #[test]
    fn the_application_context_stays_under_the_activity() {
        // Nothing published: the Keystore, a picker or the clipboard are refused by name, never
        // a panic that ends the process.
        let called = std::cell::Cell::new(false);
        let refused = with_activity(|_, _| {
            called.set(true);
            Ok(())
        });
        assert_eq!(refused, Err(NO_CONTEXT.to_owned()));
        assert!(!called.get(), "nothing runs without a context");

        // The provider, or the activity's `onCreate`, publishes the application's context; the
        // other one's call publishes nothing more.
        let (vm, application) = (std::ptr::dangling_mut::<c_void>(), std::ptr::dangling_mut());
        // SAFETY: the pointers are only stored and compared, never dereferenced, in this test.
        assert!(unsafe { publish_application(vm, application) });
        let activity = std::ptr::without_provenance_mut::<c_void>(16);
        // SAFETY: as above.
        assert!(!unsafe { publish_application(activity, activity) });
        assert_eq!(
            context().map(ndk_context::AndroidContext::context),
            Ok(application)
        );

        // android-activity publishes the activity over it, as its thread does before
        // `android_main`: in one step, with the application's context published up to it.
        // SAFETY: as above.
        unsafe {
            ndk_context::initialize_android_context(activity, activity);
            activity_started();
        }
        assert_eq!(
            context().map(ndk_context::AndroidContext::context),
            Ok(activity)
        );

        // `android_main` returns and android-activity takes the activity's back: the
        // application's is published again, for whatever still runs in the process.
        activity_ended();
        // SAFETY: android-activity's taking back, once `android_main` returned.
        unsafe { ndk_context::release_android_context() };
        assert_eq!(
            context().map(ndk_context::AndroidContext::context),
            Ok(application)
        );
    }
}
