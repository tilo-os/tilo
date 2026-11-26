//! Provides a stable api to rust crates for interfacing with the Android platform. It is
//! initialized by the runtime, usually [__ndk-glue__](https://crates.io/crates/ndk-glue),
//! but could also be initialized by Java or Kotlin code when embedding in an existing Android
//! project.
//!
//! ```no_run
//! let ctx = ndk_context::android_context();
//! let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
//! let env = vm.attach_current_thread();
//! let class_ctx = env.find_class("android/content/Context")?;
//! let audio_service = env.get_static_field(class_ctx, "AUDIO_SERVICE", "Ljava/lang/String;")?;
//! let audio_manager = env
//!     .call_method(
//!         ctx.context() as jni::sys::jobject,
//!         "getSystemService",
//!         "(Ljava/lang/String;)Ljava/lang/Object;",
//!         &[audio_service],
//!     )?
//!     .l()?;
//! ```
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The published contexts, the one [`android_context`] returns last: a context published over
/// another is returned until it is released, and the one under it again after.
struct Published(Vec<AndroidContext>);

// SAFETY: the pointers are the process's `JavaVM` and global references, valid on every thread.
unsafe impl Send for Published {}

static ANDROID_CONTEXT: Mutex<Published> = Mutex::new(Published(Vec::new()));

fn published() -> MutexGuard<'static, Published> {
    ANDROID_CONTEXT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// [`AndroidContext`] provides the pointers required to interface with the jni on Android
/// platforms.
#[derive(Clone, Copy, Debug)]
pub struct AndroidContext {
    java_vm: *mut c_void,
    context_jobject: *mut c_void,
}

impl AndroidContext {
    /// A handle to the `JavaVM` object.
    ///
    /// Usage with [__jni__](https://crates.io/crates/jni) crate:
    /// ```no_run
    /// let ctx = ndk_context::android_context();
    /// let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
    /// let env = vm.attach_current_thread();
    /// ```
    pub fn vm(self) -> *mut c_void {
        self.java_vm
    }

    /// A handle to an [android.content.Context](https://developer.android.com/reference/android/content/Context).
    /// In most cases this will be a ptr to an `Activity`, but this isn't guaranteed.
    ///
    /// Usage with [__jni__](https://crates.io/crates/jni) crate:
    /// ```no_run
    /// let ctx = ndk_context::android_context();
    /// let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
    /// let env = vm.attach_current_thread();
    /// let class_ctx = env.find_class("android/content/Context")?;
    /// let audio_service = env.get_static_field(class_ctx, "AUDIO_SERVICE", "Ljava/lang/String;")?;
    /// let audio_manager = env
    ///     .call_method(
    ///         ctx.context() as jni::sys::jobject,
    ///         "getSystemService",
    ///         "(Ljava/lang/String;)Ljava/lang/Object;",
    ///         &[audio_service],
    ///     )?
    ///     .l()?;
    /// ```
    pub fn context(self) -> *mut c_void {
        self.context_jobject
    }
}

/// Main entry point to this crate. Returns an [`AndroidContext`]: the one published last.
pub fn android_context() -> AndroidContext {
    *published()
        .0
        .last()
        .expect("android context was not initialized")
}

/// Initializes the [`AndroidContext`]. [`AndroidContext`] is initialized by [__ndk-glue__](https://crates.io/crates/ndk-glue)
/// before `main` is called.
///
/// A context published already, such as the application's that Java or Kotlin code published
/// before the activity started, stays under this one: [`android_context`] returns this one until
/// it is released, and that one again after, so there is no moment without a context.
///
/// # Safety
///
/// The pointers must be valid until this context is released, or for the process when it never
/// is.
pub unsafe fn initialize_android_context(java_vm: *mut c_void, context_jobject: *mut c_void) {
    published().0.push(AndroidContext {
        java_vm,
        context_jobject,
    });
}

/// Removes the [`AndroidContext`] published last. It is released by [__ndk-glue__](https://crates.io/crates/ndk-glue)
/// when the activity is finished and destroyed; the context published before it, if any, is
/// returned again.
///
/// # Safety
///
/// This function must only be called after [`initialize_android_context()`],
/// when the activity is subsequently destroyed according to Android.
pub unsafe fn release_android_context() {
    let previous = published().0.pop();
    assert!(previous.is_some());
}
